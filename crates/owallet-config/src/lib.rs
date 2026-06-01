//! `.owallet` config-file resolution and dotenv loading.
//!
//! Mirrors the resolution logic in `wallet_mcp/cli.py:74-111`, adapted for a
//! compiled binary (which has no Python `__file__` to anchor to):
//!
//! - `--config PATH` selects a single file that *must* exist.
//! - `--prod`, `--dev`, `--staging` load `{prod,dev,staging}.owallet`, searched
//!   in `$OWALLET_CONFIG_DIR`, then the current working directory, then the
//!   executable's own directory (first match wins). An explicitly requested
//!   file that is found in none of these is an **error** — we never silently
//!   fall back to the built-in production defaults. (The Python CLI marks
//!   these flag files `required=True`; only the no-flag default is optional.)
//! - With no flags, `prod.owallet` is loaded silently if it exists.
//! - Flags can be combined to load multiple configs (one server per config
//!   when `serve` runs; each gets its own `OWALLET_PORT`).

use std::path::{Path, PathBuf};

use thiserror::Error;

/// Default URLs (mirror `wallet_mcp/_defaults.py`). `OVERPAY_MCP_URL` was
/// removed upstream — owallet no longer calls the Overpay-hosted MCP, so
/// neither `install` nor `config` emit an `overpay` server entry anymore.
pub mod defaults {
    pub const OVERPAY_RAILS_URL: &str = "https://overpay.com";
    pub const OWALLET_PORT: u16 = 8765;
    pub const OWALLET_HOST: &str = "127.0.0.1";
}

/// Env var names that must carry a per-environment `_<POSTFIX>` suffix as
/// *inputs* (matches `_SUFFIXED_ENV_KEYS` in `wallet_mcp/cli.py`). The
/// unsuffixed forms are what the rest of the code reads — but those values
/// now only come from the `.owallet` config file or from the suffixed env
/// vars resolved by [`apply_env_overrides`].
pub const SUFFIXED_ENV_KEYS: [&str; 2] = ["OVERPAY_RAILS_URL", "OVERPAY_PUBLIC_URL"];

/// The uppercase suffix used in `OVERPAY_*_<POSTFIX>` env vars for a given
/// config file — the filename stem uppercased (`prod.owallet` → `PROD`).
/// Mirrors `_env_postfix` in `wallet_mcp/cli.py`.
#[must_use]
pub fn env_postfix(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_uppercase())
        .unwrap_or_default()
}

/// Resolve `OVERPAY_*_<POSTFIX>` env vars into their unsuffixed forms.
///
/// Always clears the unsuffixed value first so a generic `OVERPAY_RAILS_URL`
/// (shell export, stale env) can't bypass the suffix convention. Then, if the
/// matching suffixed variable is set, copies it back into the unsuffixed name
/// that the rest of the code reads. Mirrors `_apply_env_overrides` in
/// `wallet_mcp/cli.py`. A no-op for the keys when `postfix` is empty (other
/// than the clearing).
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

/// What the user asked for on the command line.
#[derive(Debug, Default, Clone)]
pub struct ConfigSelector {
    /// Explicit `--config PATH` (required if non-empty and missing).
    pub explicit: Option<PathBuf>,
    pub prod: bool,
    pub dev: bool,
    pub staging: bool,
    /// Directory where `prod.owallet` / `dev.owallet` / `staging.owallet`
    /// live. Defaults to the current working directory.
    pub repo_root: Option<PathBuf>,
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

/// Resolve which `.owallet` files to load and in what order.
///
/// Returns an empty vec if no flags were given and `prod.owallet` does not
/// exist (silent fallback to built-in defaults).
pub fn resolve(selector: &ConfigSelector) -> Result<Vec<PathBuf>, ConfigError> {
    // --config PATH wins — required to exist.
    if let Some(p) = &selector.explicit {
        let path = expand_tilde(p);
        if path.exists() {
            return Ok(vec![path]);
        }
        return Err(ConfigError::NotFound(path));
    }

    let dirs = base_dirs(selector);

    // Explicit env flags. Unlike the bare default load below, an explicitly
    // requested `--prod/--dev/--staging` file is REQUIRED: if it is found in
    // none of the candidate directories we error rather than silently falling
    // back to the built-in production defaults. (Matches the `required=True`
    // flag files in `wallet_mcp/cli.py:85,97-98`.)
    if selector.prod || selector.dev || selector.staging {
        let mut out = Vec::new();
        for (flag, name) in [
            (selector.prod, "prod.owallet"),
            (selector.dev, "dev.owallet"),
            (selector.staging, "staging.owallet"),
        ] {
            if !flag {
                continue;
            }
            match find_in_dirs(&dirs, name) {
                Some(p) => out.push(p),
                None => {
                    let where_ = dirs
                        .first()
                        .map_or_else(|| PathBuf::from(name), |d| d.join(name));
                    return Err(ConfigError::NotFound(where_));
                }
            }
        }
        return Ok(out);
    }

    // Default: prod.owallet, silent if missing (matches Python).
    Ok(match find_in_dirs(&dirs, "prod.owallet") {
        Some(p) => vec![p],
        None => vec![],
    })
}

/// Ordered list of directories searched for `.owallet` config files at
/// runtime (mirrors the logic used by [`resolve`], without a selector).
///
/// Checks, in priority order:
///   1. `$OWALLET_CONFIG_DIR`, if set and non-empty;
///   2. the current working directory;
///   3. the directory containing the running executable.
pub fn search_dirs() -> Vec<PathBuf> {
    base_dirs(&ConfigSelector::default())
}

/// Ordered list of directories to search for a `<name>.owallet` file.
///
/// When `selector.repo_root` is set (tests, or an explicit anchor) only that
/// directory is used. Otherwise — a compiled binary has no Python `__file__`
/// to anchor to — we search, in priority order:
///   1. `$OWALLET_CONFIG_DIR`, if set and non-empty;
///   2. the current working directory;
///   3. the directory containing the running executable.
///
/// The cwd entry preserves the prior behaviour; `$OWALLET_CONFIG_DIR` and the
/// exe-dir make `--prod/--dev/--staging` resolvable regardless of cwd (the
/// Python original always read them from a fixed dir next to its package).
fn base_dirs(selector: &ConfigSelector) -> Vec<PathBuf> {
    if let Some(root) = &selector.repo_root {
        return vec![root.clone()];
    }
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

/// First `dir/name` that exists, searched in order.
fn find_in_dirs(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    dirs.iter().map(|d| d.join(name)).find(|p| p.exists())
}

/// Load every selected `.owallet` file into the process environment (and
/// also `.env` if present in CWD). Earlier sources win — matches the
/// Python code's `os.environ.setdefault(...)` semantics in `cli.py:103-111`.
pub fn load_into_env(paths: &[PathBuf]) -> Result<(), ConfigError> {
    let mut sources = Vec::with_capacity(paths.len() + 1);
    sources.extend(paths.iter().cloned());
    let dot_env = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".env");
    if dot_env.exists() {
        sources.push(dot_env);
    }

    for path in sources {
        let items = dotenvy::from_filename_iter(&path).map_err(|e| ConfigError::Io {
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
    }
    Ok(())
}

/// Read every key from a `.owallet` file without mutating the process env.
/// Later entries with the same key overwrite earlier ones, matching how
/// shells parse dotenv files.
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

/// Read a single key from a `.owallet` file without mutating the process env.
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

/// Parse `--port 9001,9002` style overrides. Returns the parsed list or an
/// error if any token doesn't parse as a u16. Empty string returns `Ok(vec![])`.
pub fn parse_ports(raw: &str) -> Result<Vec<u16>, std::num::ParseIntError> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    raw.split(',').map(|t| t.trim().parse::<u16>()).collect()
}

fn expand_tilde(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    p.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_root() -> tempfile::TempDir {
        let tmp = tempfile::TempDir::new().unwrap();
        fs::write(
            tmp.path().join("prod.owallet"),
            "OVERPAY_RAILS_URL=https://overpay.com\nOWALLET_PORT=8765\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("dev.owallet"),
            "OVERPAY_RAILS_URL=http://localhost:3001\nOWALLET_PORT=8766\n",
        )
        .unwrap();
        fs::write(tmp.path().join("staging.owallet"), "OWALLET_PORT=8767\n").unwrap();
        tmp
    }

    #[test]
    fn explicit_path_wins() {
        let tmp = make_root();
        let resolved = resolve(&ConfigSelector {
            explicit: Some(tmp.path().join("dev.owallet")),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(resolved, vec![tmp.path().join("dev.owallet")]);
    }

    #[test]
    fn explicit_path_must_exist() {
        let tmp = make_root();
        let err = resolve(&ConfigSelector {
            explicit: Some(tmp.path().join("nope.owallet")),
            ..Default::default()
        });
        assert!(matches!(err, Err(ConfigError::NotFound(_))));
    }

    #[test]
    fn no_flags_silent_default_when_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let resolved = resolve(&ConfigSelector {
            repo_root: Some(tmp.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap();
        assert!(resolved.is_empty());
    }

    #[test]
    fn no_flags_loads_prod_when_present() {
        let tmp = make_root();
        let resolved = resolve(&ConfigSelector {
            repo_root: Some(tmp.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(resolved, vec![tmp.path().join("prod.owallet")]);
    }

    #[test]
    fn multi_flag_loads_all_present() {
        let tmp = make_root();
        let resolved = resolve(&ConfigSelector {
            prod: true,
            dev: true,
            staging: true,
            repo_root: Some(tmp.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            resolved,
            vec![
                tmp.path().join("prod.owallet"),
                tmp.path().join("dev.owallet"),
                tmp.path().join("staging.owallet"),
            ]
        );
    }

    #[test]
    fn requested_flag_file_is_required() {
        let tmp = tempfile::TempDir::new().unwrap();
        // No files written; --prod flag set → error, never a silent fallback.
        let err = resolve(&ConfigSelector {
            prod: true,
            repo_root: Some(tmp.path().to_path_buf()),
            ..Default::default()
        });
        assert!(matches!(err, Err(ConfigError::NotFound(_))));
    }

    #[test]
    fn base_dirs_uses_only_repo_root_when_set() {
        let dirs = base_dirs(&ConfigSelector {
            repo_root: Some(PathBuf::from("/anchor")),
            ..Default::default()
        });
        assert_eq!(dirs, vec![PathBuf::from("/anchor")]);
    }

    #[test]
    fn find_in_dirs_returns_first_existing() {
        let a = tempfile::TempDir::new().unwrap();
        let b = tempfile::TempDir::new().unwrap();
        fs::write(b.path().join("staging.owallet"), "X=1\n").unwrap();
        // `a` has no file, `b` does → the first existing match (b) wins.
        let dirs = vec![a.path().to_path_buf(), b.path().to_path_buf()];
        assert_eq!(
            find_in_dirs(&dirs, "staging.owallet"),
            Some(b.path().join("staging.owallet"))
        );
        assert_eq!(find_in_dirs(&dirs, "nope.owallet"), None);
    }

    #[test]
    fn read_var_works() {
        let tmp = make_root();
        let v = read_var(&tmp.path().join("dev.owallet"), "OWALLET_PORT")
            .unwrap()
            .unwrap();
        assert_eq!(v, "8766");
    }

    #[test]
    fn read_all_vars_returns_everything() {
        let tmp = make_root();
        let vars = read_all_vars(&tmp.path().join("dev.owallet")).unwrap();
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
