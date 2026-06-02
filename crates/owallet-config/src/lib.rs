//! Config resolution for owallet.
//!
//! Three environments (`--prod`, `--dev`, `--staging`) each have hardcoded
//! built-in defaults applied with `setdefault` semantics so any env var already
//! in the shell always wins.  `--config PATH` still loads an explicit dotenv
//! file for truly custom configurations.

use std::path::{Path, PathBuf};

use thiserror::Error;

/// Default values shared across the codebase.
pub mod defaults {
    pub const OVERPAY_RAILS_URL: &str = "https://overpay.com";
    pub const OWALLET_PORT: u16 = 8765;
    pub const OWALLET_HOST: &str = "127.0.0.1";
}

/// Env var names that carry a per-environment `_<POSTFIX>` suffix as *inputs*
/// (matches `_SUFFIXED_ENV_KEYS` in `wallet_mcp/cli.py`).  The unsuffixed forms
/// are what the rest of the code reads.
pub const SUFFIXED_ENV_KEYS: [&str; 2] = ["OVERPAY_RAILS_URL", "OVERPAY_PUBLIC_URL"];

// ---------------------------------------------------------------------------
// Built-in per-environment defaults
// ---------------------------------------------------------------------------

/// A named environment with hardcoded defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinEnv {
    Prod,
    Dev,
    Staging,
}

/// Resolved values for a built-in environment.
pub struct BuiltinDefaults {
    pub label: &'static str,
    /// `None` for staging — the URL must be supplied via
    /// `OVERPAY_RAILS_URL_STAGING` or `OVERPAY_RAILS_URL`.
    pub rails_url: Option<&'static str>,
    pub port: u16,
}

impl BuiltinEnv {
    /// The uppercase suffix used in `OVERPAY_*_<POSTFIX>` env vars.
    pub fn postfix(self) -> &'static str {
        match self {
            BuiltinEnv::Prod => "PROD",
            BuiltinEnv::Dev => "DEV",
            BuiltinEnv::Staging => "STAGING",
        }
    }

    pub fn config(self) -> BuiltinDefaults {
        match self {
            BuiltinEnv::Prod => BuiltinDefaults {
                label: "prod",
                rails_url: Some(defaults::OVERPAY_RAILS_URL),
                port: defaults::OWALLET_PORT,
            },
            BuiltinEnv::Dev => BuiltinDefaults {
                label: "dev",
                rails_url: Some("http://localhost:3001"),
                port: 8766,
            },
            BuiltinEnv::Staging => BuiltinDefaults {
                label: "staging",
                rails_url: None,
                port: 8767,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Resolved config
// ---------------------------------------------------------------------------

/// What a resolved CLI selector maps to.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedConfig {
    /// Use a built-in environment's defaults (overridable by env vars).
    Builtin(BuiltinEnv),
    /// Parse an explicit dotenv file.
    File(PathBuf),
}

impl ResolvedConfig {
    /// The env-var postfix for this config (`"PROD"`, `"DEV"`, …).
    pub fn postfix(&self) -> String {
        match self {
            ResolvedConfig::Builtin(env) => env.postfix().to_string(),
            ResolvedConfig::File(path) => env_postfix(path),
        }
    }
}

// ---------------------------------------------------------------------------
// Env-var helpers
// ---------------------------------------------------------------------------

/// The uppercase suffix for a `.owallet` file path (`prod.owallet` → `"PROD"`).
#[must_use]
pub fn env_postfix(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_uppercase())
        .unwrap_or_default()
}

/// Copy `OVERPAY_*_<POSTFIX>` env vars into their unsuffixed forms, clearing
/// any stale unsuffixed value first.  Mirrors `_apply_env_overrides` in
/// `wallet_mcp/cli.py`.
pub fn apply_env_overrides(postfix: &str) {
    for key in SUFFIXED_ENV_KEYS {
        std::env::remove_var(key);
        if !postfix.is_empty() {
            let env_key = format!("{key}_{postfix}");
            if let Ok(v) = std::env::var(&env_key) {
                std::env::set_var(key, v);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Config selector + resolution
// ---------------------------------------------------------------------------

/// What the user asked for on the command line.
#[derive(Debug, Default, Clone)]
pub struct ConfigSelector {
    /// Explicit `--config PATH` (required to exist if set).
    pub explicit: Option<PathBuf>,
    pub prod: bool,
    pub dev: bool,
    pub staging: bool,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config file not found: {0}")]
    NotFound(PathBuf),
    #[error("io error reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Resolve CLI flags to a list of configs.
///
/// - `--config PATH` — explicit dotenv file; errors if missing.
/// - `--prod`/`--dev`/`--staging` — built-in defaults for those environments.
/// - No flags — `Builtin(Prod)` (production defaults).
pub fn resolve(selector: &ConfigSelector) -> Result<Vec<ResolvedConfig>, ConfigError> {
    if let Some(p) = &selector.explicit {
        let path = expand_tilde(p);
        if path.exists() {
            return Ok(vec![ResolvedConfig::File(path)]);
        }
        return Err(ConfigError::NotFound(path));
    }

    if selector.prod || selector.dev || selector.staging {
        let mut out = Vec::new();
        if selector.prod {
            out.push(ResolvedConfig::Builtin(BuiltinEnv::Prod));
        }
        if selector.dev {
            out.push(ResolvedConfig::Builtin(BuiltinEnv::Dev));
        }
        if selector.staging {
            out.push(ResolvedConfig::Builtin(BuiltinEnv::Staging));
        }
        return Ok(out);
    }

    Ok(vec![ResolvedConfig::Builtin(BuiltinEnv::Prod)])
}

/// Ordered list of directories that would be searched for `.owallet` files
/// (useful for debugging / `owallet config` output).
pub fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(d) = std::env::var_os("OWALLET_CONFIG_DIR") {
        if !d.is_empty() {
            dirs.push(PathBuf::from(d));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd);
    }
    if let Some(parent) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        dirs.push(parent);
    }
    dirs
}

// ---------------------------------------------------------------------------
// Environment loading
// ---------------------------------------------------------------------------

/// Apply resolved configs to the process environment (`setdefault` semantics —
/// values already in the env are never overwritten).  Also loads `.env` from
/// the CWD if it exists.
pub fn load_resolved_into_env(configs: &[ResolvedConfig]) -> Result<(), ConfigError> {
    for config in configs {
        match config {
            ResolvedConfig::Builtin(env) => {
                let cfg = env.config();
                if std::env::var_os("OWALLET_PORT").is_none() {
                    std::env::set_var("OWALLET_PORT", cfg.port.to_string());
                }
                if let Some(url) = cfg.rails_url {
                    if std::env::var_os("OVERPAY_RAILS_URL").is_none() {
                        std::env::set_var("OVERPAY_RAILS_URL", url);
                    }
                }
            }
            ResolvedConfig::File(path) => {
                load_file_into_env(path)?;
            }
        }
    }

    let dot_env = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".env");
    if dot_env.exists() {
        load_file_into_env(&dot_env)?;
    }

    Ok(())
}

fn load_file_into_env(path: &PathBuf) -> Result<(), ConfigError> {
    let items = dotenvy::from_filename_iter(path).map_err(|e| ConfigError::Io {
        path: path.clone(),
        source: std::io::Error::other(e.to_string()),
    })?;
    for item in items {
        let (k, v) = item.map_err(|e| ConfigError::Io {
            path: path.clone(),
            source: std::io::Error::other(e.to_string()),
        })?;
        if std::env::var_os(&k).is_none() {
            std::env::set_var(k, v);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// File reading helpers (for --config PATH / serve multi-config)
// ---------------------------------------------------------------------------

/// Read every key from a dotenv file without mutating the process env.
pub fn read_all_vars(
    path: &Path,
) -> Result<std::collections::HashMap<String, String>, ConfigError> {
    let mut out = std::collections::HashMap::new();
    let items = dotenvy::from_filename_iter(path).map_err(|e| ConfigError::Io {
        path: path.to_path_buf(),
        source: std::io::Error::other(e.to_string()),
    })?;
    for item in items {
        let (k, v) = item.map_err(|e| ConfigError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::other(e.to_string()),
        })?;
        out.insert(k, v);
    }
    Ok(out)
}

/// Read a single key from a dotenv file without mutating the process env.
pub fn read_var(path: &Path, key: &str) -> Result<Option<String>, ConfigError> {
    let items = dotenvy::from_filename_iter(path).map_err(|e| ConfigError::Io {
        path: path.to_path_buf(),
        source: std::io::Error::other(e.to_string()),
    })?;
    for item in items {
        let (k, v) = item.map_err(|e| ConfigError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::other(e.to_string()),
        })?;
        if k == key {
            return Ok(Some(v));
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// Port parsing
// ---------------------------------------------------------------------------

/// Parse `--port 9001,9002` style overrides.
pub fn parse_ports(raw: &str) -> Result<Vec<u16>, std::num::ParseIntError> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    raw.split(',').map(|t| t.trim().parse::<u16>()).collect()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn expand_tilde(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    p.to_path_buf()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_path_wins() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("custom.owallet");
        std::fs::write(&p, "OWALLET_PORT=9999\n").unwrap();
        let resolved = resolve(&ConfigSelector {
            explicit: Some(p.clone()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(resolved, vec![ResolvedConfig::File(p)]);
    }

    #[test]
    fn explicit_path_must_exist() {
        let tmp = tempfile::TempDir::new().unwrap();
        let err = resolve(&ConfigSelector {
            explicit: Some(tmp.path().join("nope.owallet")),
            ..Default::default()
        });
        assert!(matches!(err, Err(ConfigError::NotFound(_))));
    }

    #[test]
    fn no_flags_returns_prod_builtin() {
        let resolved = resolve(&ConfigSelector::default()).unwrap();
        assert_eq!(resolved, vec![ResolvedConfig::Builtin(BuiltinEnv::Prod)]);
    }

    #[test]
    fn prod_flag_returns_prod_builtin() {
        let resolved = resolve(&ConfigSelector {
            prod: true,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(resolved, vec![ResolvedConfig::Builtin(BuiltinEnv::Prod)]);
    }

    #[test]
    fn dev_flag_returns_dev_builtin() {
        let resolved = resolve(&ConfigSelector {
            dev: true,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(resolved, vec![ResolvedConfig::Builtin(BuiltinEnv::Dev)]);
    }

    #[test]
    fn multi_flag_returns_all_builtins() {
        let resolved = resolve(&ConfigSelector {
            prod: true,
            dev: true,
            staging: true,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            resolved,
            vec![
                ResolvedConfig::Builtin(BuiltinEnv::Prod),
                ResolvedConfig::Builtin(BuiltinEnv::Dev),
                ResolvedConfig::Builtin(BuiltinEnv::Staging),
            ]
        );
    }

    #[test]
    fn builtin_postfixes() {
        assert_eq!(BuiltinEnv::Prod.postfix(), "PROD");
        assert_eq!(BuiltinEnv::Dev.postfix(), "DEV");
        assert_eq!(BuiltinEnv::Staging.postfix(), "STAGING");
    }

    #[test]
    fn builtin_defaults_values() {
        assert_eq!(
            BuiltinEnv::Prod.config().rails_url,
            Some("https://overpay.com")
        );
        assert_eq!(BuiltinEnv::Prod.config().port, 8765);
        assert_eq!(
            BuiltinEnv::Dev.config().rails_url,
            Some("http://localhost:3001")
        );
        assert_eq!(BuiltinEnv::Dev.config().port, 8766);
        assert!(BuiltinEnv::Staging.config().rails_url.is_none());
        assert_eq!(BuiltinEnv::Staging.config().port, 8767);
    }

    #[test]
    fn resolved_config_postfix() {
        assert_eq!(
            ResolvedConfig::Builtin(BuiltinEnv::Dev).postfix(),
            "DEV"
        );
        assert_eq!(
            ResolvedConfig::File(PathBuf::from("staging.owallet")).postfix(),
            "STAGING"
        );
    }

    #[test]
    fn read_var_works() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("dev.owallet");
        std::fs::write(&p, "OVERPAY_RAILS_URL=http://localhost:3001\nOWALLET_PORT=8766\n").unwrap();
        let v = read_var(&p, "OWALLET_PORT").unwrap().unwrap();
        assert_eq!(v, "8766");
    }

    #[test]
    fn read_all_vars_returns_everything() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("dev.owallet");
        std::fs::write(&p, "OVERPAY_RAILS_URL=http://localhost:3001\nOWALLET_PORT=8766\n").unwrap();
        let vars = read_all_vars(&p).unwrap();
        assert_eq!(vars.get("OWALLET_PORT").map(String::as_str), Some("8766"));
        assert_eq!(
            vars.get("OVERPAY_RAILS_URL").map(String::as_str),
            Some("http://localhost:3001")
        );
    }

    #[test]
    fn parse_ports_single() {
        assert_eq!(parse_ports("9001").unwrap(), vec![9001]);
    }

    #[test]
    fn parse_ports_multiple() {
        assert_eq!(
            parse_ports("9001,9002,9003").unwrap(),
            vec![9001, 9002, 9003]
        );
    }

    #[test]
    fn parse_ports_empty() {
        assert!(parse_ports("").unwrap().is_empty());
        assert!(parse_ports("   ").unwrap().is_empty());
    }

    #[test]
    fn parse_ports_with_whitespace() {
        assert_eq!(parse_ports("9001 , 9002").unwrap(), vec![9001, 9002]);
    }

    #[test]
    fn parse_ports_rejects_garbage() {
        assert!(parse_ports("9001,abc").is_err());
    }
}
