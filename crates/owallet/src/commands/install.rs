//! `owallet install` — register the local owallet MCP server with one or
//! more client config files. (owallet no longer registers the hosted
//! Overpay MCP — matches `mcp_install` in `wallet_mcp/cli.py`.)
//!
//! Targets:
//! - Claude Code: `./.mcp.json` (local) / `~/.claude.json` (global)
//! - OpenCode:    `./opencode.json` (local) / `~/.config/opencode/opencode.json` (global)
//! - Codex:       `./.codex/config.toml` (local) / `~/.codex/config.toml` (global)
//!
//! With the internal `dev-envs`-only `--dev`/`--staging` flags, one entry is
//! added per active env with a `-dev` / `-staging` suffix on the server name
//! (prod = no suffix). Ports come from each config's `OWALLET_PORT`.

use std::path::{Path, PathBuf};

use owallet_config::{defaults, read_all_vars, resolve, ResolvedConfig};
use serde_json::{json, Value};

use super::{CmdError, Result};
use crate::cli::{config_selector, Cli};

pub struct InstallArgs<'a> {
    pub claude_local: bool,
    pub claude_global: bool,
    pub opencode_local: bool,
    pub opencode_global: bool,
    pub codex_local: bool,
    pub codex_global: bool,
    pub port: Option<u16>,
    pub cli: &'a Cli,
}

/// One MCP entry to write: a name + a URL.
#[derive(Debug, Clone)]
pub(crate) struct McpEntry {
    pub name: String,
    pub url: String,
}

pub fn run(args: InstallArgs<'_>) -> Result<()> {
    let entries = build_entries(args.cli, args.port)?;
    let targets = pick_targets(&args)?;

    if targets.is_empty() {
        return Err(CmdError::BadInput(
            "no target specified — pass one of --claude-local --claude-global --opencode-local --opencode-global --codex-local --codex-global".into(),
        ));
    }

    for t in targets {
        let path = target_path(t)?;
        match t {
            Target::ClaudeLocal | Target::ClaudeGlobal => write_claude_json(&path, &entries)?,
            Target::OpencodeLocal | Target::OpencodeGlobal => write_opencode_json(&path, &entries)?,
            Target::CodexLocal | Target::CodexGlobal => write_codex_toml(&path, &entries)?,
        }
        println!("Installed {} entries → {}", entries.len(), path.display());
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Target selection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum Target {
    ClaudeLocal,
    ClaudeGlobal,
    OpencodeLocal,
    OpencodeGlobal,
    CodexLocal,
    CodexGlobal,
}

fn pick_targets(args: &InstallArgs<'_>) -> Result<Vec<Target>> {
    let mut out = Vec::new();
    if args.claude_local {
        out.push(Target::ClaudeLocal);
    }
    if args.claude_global {
        out.push(Target::ClaudeGlobal);
    }
    if args.opencode_local {
        out.push(Target::OpencodeLocal);
    }
    if args.opencode_global {
        out.push(Target::OpencodeGlobal);
    }
    if args.codex_local {
        out.push(Target::CodexLocal);
    }
    if args.codex_global {
        out.push(Target::CodexGlobal);
    }
    Ok(out)
}

fn target_path(t: Target) -> Result<PathBuf> {
    let home = || dirs::home_dir().ok_or_else(|| CmdError::BadInput("no $HOME".into()));
    Ok(match t {
        Target::ClaudeLocal => PathBuf::from(".mcp.json"),
        Target::ClaudeGlobal => home()?.join(".claude.json"),
        Target::OpencodeLocal => PathBuf::from("opencode.json"),
        Target::OpencodeGlobal => {
            let base =
                dirs::config_dir().ok_or_else(|| CmdError::BadInput("no XDG config dir".into()))?;
            base.join("opencode/opencode.json")
        }
        Target::CodexLocal => PathBuf::from(".codex/config.toml"),
        Target::CodexGlobal => home()?.join(".codex/config.toml"),
    })
}

// ---------------------------------------------------------------------------
// Build the MCP entries from the active `.owallet` configs.
// ---------------------------------------------------------------------------

fn build_entries(cli: &Cli, port_override: Option<u16>) -> Result<Vec<McpEntry>> {
    let configs = resolve(&config_selector(cli)).map_err(CmdError::Config)?;
    let mut out = Vec::new();
    for config in &configs {
        let (label, port) = match config {
            ResolvedConfig::Builtin(env) => {
                let p = port_override
                    .or_else(|| {
                        std::env::var("OWALLET_PORT")
                            .ok()
                            .and_then(|s| s.parse().ok())
                    })
                    .unwrap_or_else(|| env.config().port);
                (env.config().label.to_string(), p)
            }
            ResolvedConfig::File(path) => {
                let vars = read_all_vars(path).map_err(CmdError::Config)?;
                let p = port_override
                    .or_else(|| vars.get("OWALLET_PORT").and_then(|s| s.parse().ok()))
                    .unwrap_or(defaults::OWALLET_PORT);
                let label = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "default".to_string());
                (label, p)
            }
        };
        let suffix = if label == "prod" {
            String::new()
        } else {
            format!("-{label}")
        };
        // Only the local owallet server — the hosted Overpay MCP entry was
        // dropped upstream (owallet no longer calls it).
        out.push(McpEntry {
            name: format!("owallet{suffix}"),
            url: format!("http://127.0.0.1:{port}/mcp"),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Claude / OpenCode JSON writers
// ---------------------------------------------------------------------------

fn write_claude_json(path: &Path, entries: &[McpEntry]) -> Result<()> {
    let mut root = read_json_or_empty(path)?;
    let map = root
        .as_object_mut()
        .ok_or_else(|| CmdError::BadInput(format!("{} is not a JSON object", path.display())))?;
    let servers = map
        .entry("mcpServers".to_string())
        .or_insert_with(|| Value::Object(Default::default()));
    let servers = servers
        .as_object_mut()
        .ok_or_else(|| CmdError::BadInput("mcpServers is not an object".into()))?;

    for e in entries {
        servers.insert(
            e.name.clone(),
            json!({
                "type": "http",
                "url": e.url,
            }),
        );
    }
    write_json_atomic(path, &root)
}

fn write_opencode_json(path: &Path, entries: &[McpEntry]) -> Result<()> {
    let mut root = read_json_or_empty(path)?;
    let map = root
        .as_object_mut()
        .ok_or_else(|| CmdError::BadInput(format!("{} is not a JSON object", path.display())))?;
    map.entry("$schema".to_string())
        .or_insert_with(|| json!("https://opencode.ai/config.json"));

    let mcp = map
        .entry("mcp".to_string())
        .or_insert_with(|| Value::Object(Default::default()));
    let mcp = mcp
        .as_object_mut()
        .ok_or_else(|| CmdError::BadInput("mcp is not an object".into()))?;

    for e in entries {
        mcp.insert(
            e.name.clone(),
            json!({
                "type": "remote",
                "url": e.url,
                "enabled": true,
            }),
        );
    }
    write_json_atomic(path, &root)
}

fn read_json_or_empty(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(Default::default()));
    }
    let text = std::fs::read_to_string(path)?;
    if text.trim().is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    Ok(serde_json::from_str(&text)?)
}

fn write_json_atomic(path: &Path, value: &Value) -> Result<()> {
    ensure_parent(path)?;
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    std::fs::write(path, text).map_err(CmdError::Io)
}

// ---------------------------------------------------------------------------
// Codex TOML writer (preserves comments + ordering via toml_edit)
// ---------------------------------------------------------------------------

fn write_codex_toml(path: &Path, entries: &[McpEntry]) -> Result<()> {
    use toml_edit::{value, DocumentMut, Item, Table};

    let mut doc: DocumentMut = if path.exists() {
        let text = std::fs::read_to_string(path)?;
        text.parse::<DocumentMut>()
            .map_err(|e| CmdError::BadInput(format!("{} is not valid TOML: {e}", path.display())))?
    } else {
        DocumentMut::new()
    };

    // Ensure [mcp_servers] is a table (not an inline value).
    let mcp_servers = doc
        .entry("mcp_servers")
        .or_insert(Item::Table(Table::new()));
    let mcp_servers = mcp_servers
        .as_table_mut()
        .ok_or_else(|| CmdError::BadInput("mcp_servers is not a table".into()))?;
    // `mcp_servers` should be a regular table so [mcp_servers.<name>] headers
    // render correctly on disk.
    mcp_servers.set_implicit(true);

    for e in entries {
        let sub = mcp_servers
            .entry(&e.name)
            .or_insert(Item::Table(Table::new()))
            .as_table_mut()
            .ok_or_else(|| CmdError::BadInput(format!("mcp_servers.{} is not a table", e.name)))?;
        sub["url"] = value(e.url.clone());
    }

    ensure_parent(path)?;
    std::fs::write(path, doc.to_string()).map_err(CmdError::Io)
}

fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn claude_json_writes_new_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".mcp.json");
        let entries = vec![
            McpEntry {
                name: "owallet".into(),
                url: "http://127.0.0.1:8765/mcp".into(),
            },
            McpEntry {
                name: "overpay".into(),
                url: "https://mcp.overpay.com/mcp".into(),
            },
        ];
        write_claude_json(&path, &entries).unwrap();
        let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            parsed["mcpServers"]["owallet"]["url"],
            "http://127.0.0.1:8765/mcp"
        );
        assert_eq!(parsed["mcpServers"]["owallet"]["type"], "http");
        assert_eq!(
            parsed["mcpServers"]["overpay"]["url"],
            "https://mcp.overpay.com/mcp"
        );
    }

    #[test]
    fn claude_json_preserves_existing_unrelated_keys() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".mcp.json");
        std::fs::write(
            &path,
            r#"{"theme": "dark", "mcpServers": {"other": {"type": "http", "url": "http://other"}}}"#,
        )
        .unwrap();
        write_claude_json(
            &path,
            &[McpEntry {
                name: "owallet".into(),
                url: "http://127.0.0.1:8765/mcp".into(),
            }],
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["theme"], "dark");
        assert_eq!(parsed["mcpServers"]["other"]["url"], "http://other");
        assert_eq!(
            parsed["mcpServers"]["owallet"]["url"],
            "http://127.0.0.1:8765/mcp"
        );
    }

    #[test]
    fn opencode_json_uses_remote_type_and_enabled_true() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("opencode.json");
        write_opencode_json(
            &path,
            &[McpEntry {
                name: "owallet".into(),
                url: "http://127.0.0.1:8765/mcp".into(),
            }],
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["$schema"], "https://opencode.ai/config.json");
        assert_eq!(parsed["mcp"]["owallet"]["type"], "remote");
        assert_eq!(parsed["mcp"]["owallet"]["enabled"], true);
        assert_eq!(parsed["mcp"]["owallet"]["url"], "http://127.0.0.1:8765/mcp");
    }

    #[test]
    fn codex_toml_writes_table_sections() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        write_codex_toml(
            &path,
            &[
                McpEntry {
                    name: "owallet".into(),
                    url: "http://127.0.0.1:8765/mcp".into(),
                },
                McpEntry {
                    name: "overpay".into(),
                    url: "https://mcp.overpay.com/mcp".into(),
                },
            ],
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[mcp_servers.owallet]"));
        assert!(text.contains("url = \"http://127.0.0.1:8765/mcp\""));
        assert!(text.contains("[mcp_servers.overpay]"));
    }

    #[test]
    fn codex_toml_preserves_comments_on_round_trip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            "# user comment\n[other]\nkey = \"value\"\n\n[mcp_servers.old]\nurl = \"http://old\"\n",
        )
        .unwrap();
        write_codex_toml(
            &path,
            &[McpEntry {
                name: "owallet".into(),
                url: "http://127.0.0.1:8765/mcp".into(),
            }],
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# user comment"));
        assert!(text.contains("[other]"));
        assert!(text.contains("key = \"value\""));
        assert!(text.contains("[mcp_servers.old]"));
        assert!(text.contains("[mcp_servers.owallet]"));
    }
}
