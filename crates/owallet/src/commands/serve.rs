//! `owallet serve` — run one or more HTTP servers (dashboard + OAuth AS + /mcp).
//!
//! Replaces the Python `multiprocessing.spawn` dance (`cli.py:240-274`)
//! with a single tokio runtime that hosts one axum server per `.owallet`
//! config, sharing the encrypted DB.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use owallet_config::{defaults, parse_ports, read_all_vars, resolve, BuiltinEnv, ResolvedConfig};
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
    zcash_lightwalletd: String,
    zcash_network: String,
}

pub fn run_with_cli(
    cli: &Cli,
    port_override: Option<String>,
    host_override: Option<String>,
) -> Result<()> {
    let configs = collect_configs(cli, port_override.as_deref(), host_override.as_deref())?;

    let path = default_db_path();
    let mut db = Database::open(&path)?;

    // rpassword opens /dev/tty and clears ISIG (so Ctrl-C can appear in
    // passwords). It restores on Drop, but on macOS the Drop's tcsetattr
    // silently fails (EINTR / mismatched fd), leaving ISIG cleared: the
    // terminal then echoes ^C as raw 0x03 and never generates SIGINT.
    // We save /dev/tty state ourselves and force ISIG back on after the
    // prompt regardless of what rpassword left behind.
    #[cfg(unix)]
    let tty_save: Option<(libc::c_int, libc::termios)> = {
        let fd = unsafe { libc::open(c"/dev/tty".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd >= 0 {
            let mut t = unsafe { std::mem::zeroed::<libc::termios>() };
            if unsafe { libc::tcgetattr(fd, &mut t) } == 0 {
                Some((fd, t))
            } else {
                unsafe { libc::close(fd) };
                None
            }
        } else {
            None
        }
    };

    let pw = password::read("Database password")?;
    if !db.unlock(pw.as_str())? {
        return Err(CmdError::WrongPassword);
    }
    drop(pw);

    // Restore /dev/tty with ISIG unconditionally set. Even if rpassword
    // restored it correctly, an explicit force-set is a safe no-op.
    #[cfg(unix)]
    if let Some((fd, mut t)) = tty_save {
        unsafe {
            t.c_lflag |= libc::ISIG;
            libc::tcsetattr(fd, libc::TCSANOW, &t);
            libc::close(fd);
        }
    }

    // POSIX-recommended pattern for Ctrl-C in a multi-threaded process:
    // block SIGINT on this thread (worker threads inherit the mask), then
    // park a dedicated thread on sigwait. When Ctrl-C arrives the kernel
    // queues it as a pending signal; sigwait dequeues it and exits cleanly.
    // This avoids the macOS pitfall where libc::signal() handlers installed
    // before a tokio multi-thread runtime may never fire.
    #[cfg(unix)]
    {
        let mut sigint_set = unsafe { std::mem::zeroed::<libc::sigset_t>() };
        unsafe {
            libc::sigemptyset(&mut sigint_set);
            libc::sigaddset(&mut sigint_set, libc::SIGINT);
            libc::pthread_sigmask(libc::SIG_BLOCK, &sigint_set, std::ptr::null_mut());
        }
        std::thread::spawn(move || {
            let mut sig = 0i32;
            unsafe { libc::sigwait(&sigint_set, &mut sig) };
            std::process::exit(0);
        });
    }

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
        let zcash = owallet_http::ZcashConfig {
            lightwalletd: cfg.zcash_lightwalletd.clone(),
            network: cfg.zcash_network.clone(),
        };
        let host_key = cfg.issuer_url.trim_end_matches('/').to_string();
        let state = AppState {
            db: db.clone(),
            sessions: owallet_http::SessionStore::new(),
            overpay,
            evm,
            zcash,
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
        // Give connections up to 3 s to drain, then exit. Without the
        // timeout, keep-alive connections (e.g. Claude Code's MCP client)
        // would hold the process open indefinitely.
        let drain = async {
            for t in tasks {
                let _ = t.await;
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(3), drain)
            .await
            .ok();
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

    let configs = resolve(&config_selector(cli)).map_err(CmdError::Config)?;

    if !port_overrides.is_empty() && port_overrides.len() != configs.len() {
        return Err(CmdError::BadInput(format!(
            "--port has {} value(s); expected {} (one per active config)",
            port_overrides.len(),
            configs.len()
        )));
    }

    let mut out = Vec::new();
    for (i, config) in configs.iter().enumerate() {
        let port_override = port_overrides.get(i).copied();
        match config {
            ResolvedConfig::Builtin(env) => out.push(server_from_builtin(ip, port_override, *env)?),
            ResolvedConfig::File(path) => out.push(server_from_dotenv(ip, port_override, path)?),
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

fn server_from_builtin(
    ip: IpAddr,
    port_override: Option<u16>,
    env: BuiltinEnv,
) -> Result<ServerConfig> {
    let cfg = env.config();
    let postfix = env.postfix();

    let port = port_override
        .or_else(|| {
            std::env::var(format!("OWALLET_PORT_{postfix}"))
                .ok()
                .and_then(|s| s.parse().ok())
        })
        .or_else(env_port_from_env)
        .unwrap_or(cfg.port);
    let bind = SocketAddr::new(ip, port);

    let issuer_url = std::env::var("OWALLET_MCP_BASE_URL").unwrap_or_else(|_| match ip {
        IpAddr::V4(v4) if v4.is_unspecified() => format!("http://127.0.0.1:{port}"),
        _ => format!("http://{bind}"),
    });

    // Priority: suffixed env var > unsuffixed env var > built-in default.
    // (Single-config commands run apply_env_overrides first so the unsuffixed
    // form is already set; multi-config serve skips that and each server reads
    // its own suffixed var here.)
    let rails_url = std::env::var(format!("OVERPAY_RAILS_URL_{postfix}"))
        .ok()
        .or_else(|| std::env::var("OVERPAY_RAILS_URL").ok())
        .or_else(|| cfg.rails_url.map(str::to_string))
        .unwrap_or_else(|| defaults::OVERPAY_RAILS_URL.to_string());
    let public_url = std::env::var(format!("OVERPAY_PUBLIC_URL_{postfix}"))
        .ok()
        .or_else(|| std::env::var("OVERPAY_PUBLIC_URL").ok());

    let evm_rpc_url =
        std::env::var("EVM_RPC_URL").unwrap_or_else(|_| "https://mainnet.base.org".to_string());
    let evm_network = std::env::var("EVM_NETWORK").unwrap_or_else(|_| "eip155:8453".to_string());
    let zcash_lightwalletd =
        std::env::var("ZEC_LIGHTWALLETD_URL").unwrap_or_else(|_| "zecrocks".to_string());
    let zcash_network = std::env::var("ZEC_NETWORK").unwrap_or_else(|_| "mainnet".to_string());
    Ok(ServerConfig {
        label: cfg.label.to_string(),
        bind,
        issuer_url,
        rails_url,
        public_url,
        evm_rpc_url,
        evm_network,
        zcash_lightwalletd,
        zcash_network,
    })
}

fn env_port_from_env() -> Option<u16> {
    std::env::var("OWALLET_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
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
    let zcash_lightwalletd = std::env::var(format!("ZEC_LIGHTWALLETD_URL_{postfix}"))
        .ok()
        .or_else(|| vars.get("ZEC_LIGHTWALLETD_URL").cloned())
        .unwrap_or_else(|| "zecrocks".to_string());
    let zcash_network = std::env::var(format!("ZEC_NETWORK_{postfix}"))
        .ok()
        .or_else(|| vars.get("ZEC_NETWORK").cloned())
        .unwrap_or_else(|| "mainnet".to_string());

    Ok(ServerConfig {
        label,
        bind,
        issuer_url,
        rails_url,
        public_url,
        evm_rpc_url,
        evm_network,
        zcash_lightwalletd,
        zcash_network,
    })
}

fn label_from_path(path: &std::path::Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "config".to_string())
}

async fn wait_for_shutdown() {
    // SIGTERM → graceful drain (systemd / container orchestrators).
    // ctrl_c (SIGINT via tokio kqueue) → fallback if the sigwait thread
    // races or isn't scheduled yet.  The dedicated sigwait thread (spawned
    // in run_with_cli) is the primary Ctrl-C handler on macOS.
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = sigterm.recv() => {}
            result = tokio::signal::ctrl_c() => { let _ = result; }
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.ok();
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
        assert_eq!(confs[0].label, "prod");
    }

    #[cfg(feature = "dev-envs")]
    #[test]
    fn collect_configs_multi_uses_builtin_defaults() {
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
        assert!(ports.contains(&8766));
        assert!(ports.contains(&8767));
        let rails: Vec<&str> = confs.iter().map(|c| c.rails_url.as_str()).collect();
        assert!(rails.contains(&"http://localhost:3001"));
    }

    #[test]
    fn collect_configs_explicit_file_still_works() {
        let tmp = TempDir::new().unwrap();
        write_envfile(
            tmp.path(),
            "custom.owallet",
            "OWALLET_PORT=19999\nOVERPAY_RAILS_URL=http://custom.test\n",
        );
        let cli = Cli {
            config: Some(tmp.path().join("custom.owallet")),
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
        assert_eq!(confs[0].bind.port(), 19999);
        assert_eq!(confs[0].rails_url, "http://custom.test");
    }

    #[cfg(feature = "dev-envs")]
    #[test]
    fn collect_configs_port_override_must_match_active_count() {
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

    #[cfg(feature = "dev-envs")]
    #[test]
    fn collect_configs_port_override_maps_positionally() {
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
}
