//! `owallet serve` — run one or more HTTP servers (dashboard + OAuth AS + /mcp).
//!
//! Replaces the Python `multiprocessing.spawn` dance (`cli.py:240-274`)
//! with a single tokio runtime that hosts one axum server per `.owallet`
//! config, sharing the encrypted DB.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use owallet_config::{defaults, parse_ports, read_all_vars, resolve};
use owallet_db::{default_db_path, Database};
use owallet_http::{build_full_router, AppState, EvmConfig};
use owallet_overpay::OverpayClient;

use super::{CmdError, Result};
use crate::cli::{config_selector, Cli};
use crate::password;

/// One server instance to launch.
#[derive(Debug, Clone)]
struct ServerConfig {
    label: String,
    bind: SocketAddr,
    issuer_url: String,
    rails_url: String,
    public_url: Option<String>,
    evm_rpc_url: String,
    evm_network: String,
}

pub fn run_with_cli(
    cli: &Cli,
    port_override: Option<String>,
    host_override: Option<String>,
) -> Result<()> {
    let configs = collect_configs(cli, port_override.as_deref(), host_override.as_deref())?;

    let path = default_db_path();
    let mut db = Database::open(&path)?;
    let pw = password::read("Database password")?;
    if !db.unlock(pw.as_str())? {
        return Err(CmdError::WrongPassword);
    }
    drop(pw);

    // Build one AppState per server up front — each carries its own
    // Overpay client + EVM config + host_key, plus a fresh SessionStore
    // so a sign-in on the dev server doesn't bleed into prod.
    let db = Arc::new(std::sync::Mutex::new(db));
    let mut per_server: Vec<(ServerConfig, AppState)> = Vec::with_capacity(configs.len());
    for cfg in &configs {
        let mut overpay = OverpayClient::new(&cfg.rails_url)?;
        if let Some(p) = cfg.public_url.as_deref() {
            if p != cfg.rails_url {
                overpay = overpay.with_public_url(p)?;
            }
        }
        let overpay = Arc::new(overpay);

        let evm = EvmConfig {
            rpc_url: cfg.evm_rpc_url.clone(),
            network: cfg.evm_network.clone(),
        };
        let host_key = cfg.issuer_url.trim_end_matches('/').to_string();
        let state = AppState {
            db: db.clone(),
            sessions: owallet_http::SessionStore::new(),
            overpay,
            evm,
            host_key,
            pending_auth: owallet_http::PendingDashboardAuthMap::default(),
        };
        per_server.push((cfg.clone(), state));
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| CmdError::BadInput(format!("tokio runtime: {e}")))?;

    rt.block_on(async move {
        let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
        let mut tasks = Vec::with_capacity(per_server.len());

        for (cfg, state) in per_server {
            println!(
                "[{}] http://{} (dashboard /wallet · MCP /mcp · OAuth /oauth/*)",
                cfg.label, cfg.bind
            );
            println!(
                "[{}]   Overpay = {} · EVM = {} ({})",
                cfg.label, cfg.rails_url, cfg.evm_rpc_url, cfg.evm_network
            );

            let app = build_full_router(state, cfg.issuer_url.clone());
            let listener = tokio::net::TcpListener::bind(cfg.bind)
                .await
                .map_err(|e| CmdError::BadInput(format!("bind {}: {e}", cfg.bind)))?;

            let label = cfg.label.clone();
            let mut rx = shutdown_tx.subscribe();
            tasks.push(tokio::spawn(async move {
                let shutdown = async move {
                    let _ = rx.recv().await;
                };
                if let Err(e) = axum::serve(listener, app)
                    .with_graceful_shutdown(shutdown)
                    .await
                {
                    eprintln!("[{label}] serve error: {e}");
                }
            }));
        }

        // Wait for SIGTERM / Ctrl+C, then signal every server.
        wait_for_shutdown().await;
        println!("\nshutting down…");
        let _ = shutdown_tx.send(());
        for t in tasks {
            let _ = t.await;
        }
        Ok::<(), CmdError>(())
    })?;

    Ok(())
}

/// Build the list of servers to run.
fn collect_configs(
    cli: &Cli,
    port_override: Option<&str>,
    host_override: Option<&str>,
) -> Result<Vec<ServerConfig>> {
    let host_default = host_override
        .map(str::to_string)
        .or_else(|| std::env::var("OWALLET_HOST").ok())
        .unwrap_or_else(|| defaults::OWALLET_HOST.to_string());
    let ip: IpAddr = host_default
        .parse()
        .map_err(|e| CmdError::BadInput(format!("invalid bind host {host_default:?}: {e}")))?;

    let port_overrides = match port_override {
        Some(s) => parse_ports(s)
            .map_err(|e| CmdError::BadInput(format!("--port must be u16 list: {e}")))?,
        None => Vec::new(),
    };

    let paths = resolve(&config_selector(cli)).map_err(CmdError::Config)?;

    let mut out = Vec::new();
    if paths.is_empty() {
        // No `.owallet` selection — run a single server using the process
        // env (which already has any explicit `--config` loaded by
        // load_env_from_flags).
        let port = port_overrides.first().copied().unwrap_or_else(env_port);
        out.push(server_from_env(ip, port, "default".to_string())?);
    } else {
        if !port_overrides.is_empty() && port_overrides.len() != paths.len() {
            return Err(CmdError::BadInput(format!(
                "--port has {} value(s); expected {} (one per active config)",
                port_overrides.len(),
                paths.len()
            )));
        }
        for (i, p) in paths.iter().enumerate() {
            let port_override = port_overrides.get(i).copied();
            out.push(server_from_dotenv(ip, port_override, p)?);
        }
    }
    Ok(out)
}

fn env_port() -> u16 {
    std::env::var("OWALLET_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(defaults::OWALLET_PORT)
}

fn server_from_env(ip: IpAddr, port: u16, label: String) -> Result<ServerConfig> {
    let bind = SocketAddr::new(ip, port);
    let issuer_url = std::env::var("OWALLET_MCP_BASE_URL").unwrap_or_else(|_| match ip {
        IpAddr::V4(v4) if v4.is_unspecified() => format!("http://127.0.0.1:{port}"),
        _ => format!("http://{bind}"),
    });
    let rails_url = std::env::var("OVERPAY_RAILS_URL")
        .unwrap_or_else(|_| defaults::OVERPAY_RAILS_URL.to_string());
    let public_url = std::env::var("OVERPAY_PUBLIC_URL").ok();
    let evm_rpc_url =
        std::env::var("EVM_RPC_URL").unwrap_or_else(|_| "https://mainnet.base.org".to_string());
    let evm_network = std::env::var("EVM_NETWORK").unwrap_or_else(|_| "eip155:8453".to_string());
    Ok(ServerConfig {
        label,
        bind,
        issuer_url,
        rails_url,
        public_url,
        evm_rpc_url,
        evm_network,
    })
}

fn server_from_dotenv(
    ip: IpAddr,
    port_override: Option<u16>,
    path: &std::path::Path,
) -> Result<ServerConfig> {
    let label = label_from_path(path);
    let vars = read_all_vars(path).map_err(CmdError::Config)?;

    let port = port_override
        .or_else(|| vars.get("OWALLET_PORT").and_then(|s| s.parse().ok()))
        .unwrap_or_else(env_port);
    let bind = SocketAddr::new(ip, port);

    let issuer_url = vars
        .get("OWALLET_MCP_BASE_URL")
        .cloned()
        .unwrap_or_else(|| match ip {
            IpAddr::V4(v4) if v4.is_unspecified() => format!("http://127.0.0.1:{port}"),
            _ => format!("http://{bind}"),
        });

    // Per-environment env vars (`OVERPAY_RAILS_URL_<POSTFIX>`) win over the
    // config-file value, which wins over the built-in default. This is the
    // in-process equivalent of Python's per-subprocess `_apply_env_overrides`
    // (`wallet_mcp/cli.py`): in multi-config serve the parent env is left
    // un-polluted, so each server resolves its own suffixed var directly.
    let postfix = owallet_config::env_postfix(path);
    let rails_url = std::env::var(format!("OVERPAY_RAILS_URL_{postfix}"))
        .ok()
        .or_else(|| vars.get("OVERPAY_RAILS_URL").cloned())
        .unwrap_or_else(|| defaults::OVERPAY_RAILS_URL.to_string());
    let public_url = std::env::var(format!("OVERPAY_PUBLIC_URL_{postfix}"))
        .ok()
        .or_else(|| vars.get("OVERPAY_PUBLIC_URL").cloned());
    let evm_rpc_url = vars
        .get("EVM_RPC_URL")
        .cloned()
        .unwrap_or_else(|| "https://mainnet.base.org".to_string());
    let evm_network = vars
        .get("EVM_NETWORK")
        .cloned()
        .unwrap_or_else(|| "eip155:8453".to_string());

    Ok(ServerConfig {
        label,
        bind,
        issuer_url,
        rails_url,
        public_url,
        evm_rpc_url,
        evm_network,
    })
}

fn label_from_path(path: &std::path::Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "config".to_string())
}

async fn wait_for_shutdown() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install Ctrl+C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        signal(SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_envfile(dir: &std::path::Path, name: &str, contents: &str) {
        fs::write(dir.join(name), contents).unwrap();
    }

    #[test]
    fn collect_configs_default_when_no_flags() {
        let cli = Cli {
            config: None,
            prod: false,
            dev: false,
            staging: false,
            command: crate::cli::Command::Serve {
                port: None,
                host: None,
            },
        };
        let confs = collect_configs(&cli, None, Some("127.0.0.1")).unwrap();
        assert_eq!(confs.len(), 1);
        assert_eq!(confs[0].label, "default");
    }

    #[test]
    #[serial_test::serial]
    fn collect_configs_multi_uses_each_files_port() {
        let tmp = TempDir::new().unwrap();
        write_envfile(
            tmp.path(),
            "dev.owallet",
            "OWALLET_PORT=18888\nOVERPAY_RAILS_URL=http://dev.test\nEVM_RPC_URL=http://dev.rpc\n",
        );
        write_envfile(
            tmp.path(),
            "staging.owallet",
            "OWALLET_PORT=18889\nOVERPAY_RAILS_URL=http://staging.test\n",
        );
        // Run from the temp dir so `resolve` finds the files at repo_root=cwd.
        let _g = ChdirGuard::new(tmp.path());

        let cli = Cli {
            config: None,
            prod: false,
            dev: true,
            staging: true,
            command: crate::cli::Command::Serve {
                port: None,
                host: None,
            },
        };
        let confs = collect_configs(&cli, None, Some("127.0.0.1")).unwrap();
        assert_eq!(confs.len(), 2);
        let labels: Vec<&str> = confs.iter().map(|c| c.label.as_str()).collect();
        assert!(labels.contains(&"dev"));
        assert!(labels.contains(&"staging"));
        let ports: Vec<u16> = confs.iter().map(|c| c.bind.port()).collect();
        assert!(ports.contains(&18888));
        assert!(ports.contains(&18889));
        // Each config gets its own Rails URL.
        let rails: Vec<&str> = confs.iter().map(|c| c.rails_url.as_str()).collect();
        assert!(rails.contains(&"http://dev.test"));
        assert!(rails.contains(&"http://staging.test"));
    }

    #[test]
    #[serial_test::serial]
    fn collect_configs_port_override_must_match_active_count() {
        let tmp = TempDir::new().unwrap();
        write_envfile(tmp.path(), "dev.owallet", "OWALLET_PORT=18888\n");
        write_envfile(tmp.path(), "staging.owallet", "OWALLET_PORT=18889\n");
        let _g = ChdirGuard::new(tmp.path());

        let cli = Cli {
            config: None,
            prod: false,
            dev: true,
            staging: true,
            command: crate::cli::Command::Serve {
                port: Some("19001".into()),
                host: None,
            },
        };
        let err = collect_configs(&cli, Some("19001"), Some("127.0.0.1")).unwrap_err();
        assert!(
            matches!(err, CmdError::BadInput(ref msg) if msg.contains("expected 2")),
            "got {err:?}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn collect_configs_port_override_maps_positionally() {
        let tmp = TempDir::new().unwrap();
        write_envfile(tmp.path(), "dev.owallet", "OWALLET_PORT=1\n");
        write_envfile(tmp.path(), "staging.owallet", "OWALLET_PORT=2\n");
        let _g = ChdirGuard::new(tmp.path());

        let cli = Cli {
            config: None,
            prod: false,
            dev: true,
            staging: true,
            command: crate::cli::Command::Serve {
                port: Some("19001,19002".into()),
                host: None,
            },
        };
        let confs = collect_configs(&cli, Some("19001,19002"), Some("127.0.0.1")).unwrap();
        let ports: Vec<u16> = confs.iter().map(|c| c.bind.port()).collect();
        assert_eq!(ports, vec![19001, 19002]);
    }

    /// Tests in this module share the global cwd; serialize chdir via this
    /// guard. Not perfectly thread-safe under `cargo test` parallelism, but
    /// good enough for a handful of cases.
    struct ChdirGuard {
        prev: std::path::PathBuf,
    }

    impl ChdirGuard {
        fn new(p: &std::path::Path) -> Self {
            let prev = std::env::current_dir().unwrap();
            std::env::set_current_dir(p).unwrap();
            Self { prev }
        }
    }

    impl Drop for ChdirGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.prev);
        }
    }
}
