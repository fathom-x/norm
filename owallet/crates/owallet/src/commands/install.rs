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

/// One OpenAI-compatible `provider` entry for `opencode.json` — the model
/// catalog is fetched live from the OpenRouter listing's own
/// `buyer_note_schema` on Overpay rather than duplicated here, for the
/// same reason `resolve_models` in `openai_compat.rs` reads it live off
/// the listing: it's curated on the Ruby side (`MODEL_OPTIONS`) and can
/// change without a Rust rebuild. Fetching the listing directly (not the
/// running server's `/v1/models`) means `owallet serve` doesn't have to be
/// up for `install` to populate the block.
#[derive(Debug, Clone)]
pub(crate) struct ProviderEntry {
    pub name: String,
    pub base_url: String,
    pub models: Vec<String>,
}

/// Matches `owallet_mcp::openai_compat::DEFAULT_MODEL` — kept as a plain
/// literal rather than a cross-crate import since `install` has no other
/// reason to depend on `owallet-mcp`. Used as the sole model in a
/// provider entry when the live catalog can't be fetched (see
/// `build_provider_entries`); a request against it works without a live
/// catalog either, so the entry `install` writes is never a dead end.
const DEFAULT_MODEL: &str = "default";

/// The OpenRouter Inference listing — matches the `OPENROUTER_SELLER_SLUG`
/// / `OPENROUTER_LISTING_TITLE` constants in `openai_compat.rs` (same
/// reason as `DEFAULT_MODEL`: `install` deliberately doesn't depend on
/// `owallet-mcp`). Keep the two in sync by hand if these ever change.
const OPENROUTER_SELLER_SLUG: &str = "openrouter-bot";
const OPENROUTER_LISTING_TITLE: &str = "OpenRouter Inference";

pub fn run(args: InstallArgs<'_>) -> Result<()> {
    let entries = build_entries(args.cli, args.port)?;
    let targets = pick_targets(&args)?;

    if targets.is_empty() {
        return Err(CmdError::BadInput(
            "no target specified — pass one of --claude-local --claude-global --opencode-local --opencode-global --codex-local --codex-global".into(),
        ));
    }

    // Only fetched when an OpenCode target is actually requested — Claude
    // and Codex have no equivalent "model provider" concept.
    let needs_provider = targets
        .iter()
        .any(|t| matches!(t, Target::OpencodeLocal | Target::OpencodeGlobal));
    let providers = if needs_provider {
        build_provider_entries(args.cli, args.port)?
    } else {
        Vec::new()
    };

    for t in targets {
        let path = target_path(t)?;
        match t {
            Target::ClaudeLocal | Target::ClaudeGlobal => write_claude_json(&path, &entries)?,
            Target::OpencodeLocal | Target::OpencodeGlobal => {
                write_opencode_json(&path, &entries, &providers)?;
                if !providers.is_empty() {
                    let plugin_path = opencode_plugin_path(t)?;
                    write_opencode_plugin(&plugin_path, &providers)?;
                    println!("Installed auth plugin → {}", plugin_path.display());
                }
            }
            Target::CodexLocal | Target::CodexGlobal => write_codex_toml(&path, &entries)?,
        }
        println!("Installed {} entries → {}", entries.len(), path.display());
    }

    // The one manual step left: authenticating the provider. The plugin
    // written above gives `opencode auth login` two methods; either way the
    // credential lands in OpenCode's own auth store
    // (`~/.local/share/opencode/auth.json`), never in the config file.
    if !providers.is_empty() {
        let names: Vec<&str> = providers.iter().map(|p| p.name.as_str()).collect();
        println!(
            "\nAuthenticate with `opencode auth login` → {} — \"Browser login\" \
             approves in the owallet dashboard and mints a revocable provider \
             key (`owallet serve` must be running), or paste an owk_ key \
             created under \"OpenCode provider\" at /wallet.",
            names.join(" / ")
        );
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
        Target::OpencodeGlobal => opencode_config_dir()?.join("opencode.json"),
        Target::CodexLocal => PathBuf::from(".codex/config.toml"),
        Target::CodexGlobal => home()?.join(".codex/config.toml"),
    })
}

/// OpenCode's global config directory: `$XDG_CONFIG_HOME/opencode`, or
/// `~/.config/opencode`.
///
/// Deliberately **not** `dirs::config_dir()` — that resolves to
/// `~/Library/Application Support` on macOS, but OpenCode follows the XDG
/// layout on every platform, so writing there produced a file OpenCode
/// never read (and left the real `~/.config/opencode/opencode.json`
/// untouched). `dirs::config_dir()` happens to agree on Linux; macOS is
/// where the two diverge.
fn opencode_config_dir() -> Result<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let xdg = PathBuf::from(xdg);
        // An empty or relative XDG_CONFIG_HOME is invalid per the spec —
        // fall through to the default rather than writing somewhere
        // surprising relative to the cwd.
        if xdg.is_absolute() {
            return Ok(xdg.join("opencode"));
        }
    }
    let home = dirs::home_dir().ok_or_else(|| CmdError::BadInput("no $HOME".into()))?;
    Ok(home.join(".config/opencode"))
}

// ---------------------------------------------------------------------------
// Build the MCP + provider entries from the active `.owallet` configs.
// ---------------------------------------------------------------------------

/// One active server: just enough to build either an `McpEntry` or a
/// `ProviderEntry` — shared so both entry kinds agree on label/port
/// resolution instead of duplicating it.
struct ActiveConfig {
    label: String,
    port: u16,
    rails_url: Option<String>,
}

fn active_configs(cli: &Cli, port_override: Option<u16>) -> Result<Vec<ActiveConfig>> {
    let configs = resolve(&config_selector(cli)).map_err(CmdError::Config)?;
    let mut out = Vec::new();
    for config in &configs {
        let (label, port, rails_url) = match config {
            ResolvedConfig::Builtin(env) => {
                let p = port_override
                    .or_else(|| {
                        std::env::var("OWALLET_PORT")
                            .ok()
                            .and_then(|s| s.parse().ok())
                    })
                    .unwrap_or_else(|| env.config().port);
                (
                    env.config().label.to_string(),
                    p,
                    env.config().rails_url.map(str::to_string),
                )
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
                (label, p, vars.get("OVERPAY_RAILS_URL").cloned())
            }
        };
        out.push(ActiveConfig {
            label,
            port,
            rails_url,
        });
    }
    Ok(out)
}

/// `-dev` / `-staging` on the name (prod = no suffix), matching the
/// `-{label}` suffix `owallet serve` gives its own log lines.
fn name_suffix(label: &str) -> String {
    if label == "prod" {
        String::new()
    } else {
        format!("-{label}")
    }
}

fn build_entries(cli: &Cli, port_override: Option<u16>) -> Result<Vec<McpEntry>> {
    Ok(active_configs(cli, port_override)?
        .into_iter()
        // Only the local owallet server — the hosted Overpay MCP entry was
        // dropped upstream (owallet no longer calls it).
        .map(|c| McpEntry {
            name: format!("owallet{}", name_suffix(&c.label)),
            url: format!("http://127.0.0.1:{}/mcp", c.port),
        })
        .collect())
}

/// Fetches the curated model catalog straight from the OpenRouter
/// listing's own `buyer_note_schema` on Overpay — the exact source
/// `/v1/models` in `openai_compat.rs` proxies (`resolve_models`), so
/// fetching the listing directly instead of through a running `owallet
/// serve` can't drift and needs no local server. Public endpoints, no
/// auth. A listing that can't be resolved (Overpay down, bot not
/// registered, ...) is reported as a warning and skipped, not a hard
/// failure — the rest of `install` (MCP entries, other targets) shouldn't
/// fail just because OpenCode's provider block couldn't be populated this
/// time.
fn fetch_models(rails_url: &str) -> std::result::Result<Vec<String>, String> {
    super::overpay::block_on(async {
        let client = owallet_overpay::OverpayClient::new(rails_url).map_err(|e| e.to_string())?;

        let listing_id = resolve_openrouter_listing_id(&client).await?;
        let listing = client
            .get_listing_value(&listing_id)
            .await
            .map_err(|e| e.to_string())?;
        let inner = listing.get("data").unwrap_or(&listing);
        let models: Vec<String> = inner
            .pointer("/buyer_note_schema/properties/model/enum")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if models.is_empty() {
            return Err(format!(
                "the '{OPENROUTER_LISTING_TITLE}' listing has no model enum in its buyer_note_schema"
            ));
        }
        Ok(models)
    })
}

/// Resolve the OpenRouter Inference listing id by seller + title, mirroring
/// `resolve_listing_id_cached` in `openai_compat.rs`.
async fn resolve_openrouter_listing_id(
    client: &owallet_overpay::OverpayClient,
) -> std::result::Result<String, String> {
    let page = client
        .list_listings_value(&owallet_overpay::models::ListingFilters {
            seller_slug: Some(OPENROUTER_SELLER_SLUG.to_string()),
            limit: Some(20),
            ..Default::default()
        })
        .await
        .map_err(|e| e.to_string())?;

    page.get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|l| l.get("title").and_then(Value::as_str) == Some(OPENROUTER_LISTING_TITLE))
        .and_then(|l| l.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            format!(
                "could not find a '{OPENROUTER_LISTING_TITLE}' listing from seller '{OPENROUTER_SELLER_SLUG}' — is its bot registered?"
            )
        })
}

/// Collapse a fetch error to something a terminal won't drown in. Overpay
/// error bodies can be full Rails 404/500 HTML pages (hundreds of lines);
/// the `HTTP {status}` prefix is the actionable bit, so if the body looks
/// like markup, trim everything after the status.
fn compact_error(e: &str) -> String {
    const MAX: usize = 200;
    let trimmed = e
        .lines()
        .next()
        .map(str::trim)
        .unwrap_or(e)
        .trim_end_matches(|c: char| c == '<' || c == '>' || c.is_whitespace() || c == '-');
    let trimmed = if trimmed.contains("<") || trimmed.contains("<!doctype") {
        // `HTTP 404: <!doctype html>...` — keep only the status, not the page.
        e.split(':')
            .next()
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "HTTP error".to_string())
    } else {
        trimmed.to_string()
    };
    if trimmed.chars().count() > MAX {
        let cut: String = trimmed.chars().take(MAX).collect();
        format!("{cut}…")
    } else {
        trimmed
    }
}

fn build_provider_entries(cli: &Cli, port_override: Option<u16>) -> Result<Vec<ProviderEntry>> {
    let mut out = Vec::new();
    for c in active_configs(cli, port_override)? {
        let base_url = format!("http://127.0.0.1:{}", c.port);
        // A config that can't reach Overpay (no `OVERPAY_RAILS_URL`, Overpay
        // down, ...) still gets a working provider entry, just with only the
        // `DEFAULT_MODEL` sentinel instead of the live curated list — see
        // that constant's doc comment in `openai_compat.rs` for why a request
        // against it works without owallet needing to know a real model id
        // up front.
        let provider_name = format!("overpay{}", name_suffix(&c.label));
        let models = match &c.rails_url {
            Some(rails_url) => match fetch_models(rails_url) {
                Ok(models) => models,
                Err(e) => {
                    eprintln!(
                        "warning: could not fetch the model catalog from Overpay ({}) — \
                         writing '{provider_name}' with only the '{DEFAULT_MODEL}' model for \
                         '{}'",
                        compact_error(&e),
                        c.label
                    );
                    vec![DEFAULT_MODEL.to_string()]
                }
            },
            None => {
                eprintln!(
                    "warning: no OVERPAY_RAILS_URL configured for '{}' — writing '{provider_name}' \
                     with only the '{DEFAULT_MODEL}' model",
                    c.label
                );
                vec![DEFAULT_MODEL.to_string()]
            }
        };
        out.push(ProviderEntry {
            name: provider_name,
            base_url,
            models,
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

/// Edited through jsonc-parser's CST rather than serde_json, for the same
/// reason the Codex writer uses `toml_edit`: this is a file the user hand-
/// edits. OpenCode's config is **JSONC** — comments and trailing commas are
/// legal, and commenting a block out is the normal way to disable a server
/// — so a strict `serde_json` read fails outright on a config that OpenCode
/// itself accepts (`key must be a string at line N`), and a
/// `to_string_pretty` rewrite would silently delete every comment in the
/// file. The CST preserves both, touching only the properties we set.
fn write_opencode_json(
    path: &Path,
    entries: &[McpEntry],
    providers: &[ProviderEntry],
) -> Result<()> {
    use jsonc_parser::cst::CstRootNode;
    use jsonc_parser::ParseOptions;

    let text = if path.exists() {
        let raw = std::fs::read_to_string(path)?;
        if raw.trim().is_empty() {
            "{}".to_string()
        } else {
            raw
        }
    } else {
        "{}".to_string()
    };

    let root = CstRootNode::parse(&text, &ParseOptions::default()).map_err(|e| {
        CmdError::BadInput(format!("{} is not valid JSON/JSONC: {e}", path.display()))
    })?;
    // Replaces a non-object root (a bare array/string) with `{}` rather
    // than erroring — matches the old serde path's "start fresh if it
    // isn't usable" behavior for an empty/missing file.
    let obj = root.object_value_or_set();

    if obj.get("$schema").is_none() {
        obj.append(
            "$schema",
            cst_input(&json!("https://opencode.ai/config.json")),
        );
    }

    let mcp = obj.object_value_or_set("mcp");
    for e in entries {
        set_cst_prop(
            &mcp,
            &e.name,
            &json!({ "type": "remote", "url": e.url, "enabled": true }),
        );
    }

    if !providers.is_empty() {
        let provider = obj.object_value_or_set("provider");
        for p in providers {
            let models: serde_json::Map<String, Value> =
                p.models.iter().map(|id| (id.clone(), json!({}))).collect();
            let mut options = serde_json::Map::new();
            options.insert("baseURL".into(), json!(format!("{}/v1", p.base_url)));
            // No `apiKey`: OpenCode prompts for the key on first use and
            // stores it in its own auth store
            // (`~/.local/share/opencode/auth.json`), deliberately keeping
            // secrets out of a config file people commit and share. Writing
            // a placeholder here is worse than writing nothing — depending
            // on which source wins, it either sits in the config misleading
            // the reader, or gets sent to the server as a literal bogus key.
            // A real one the user set by hand is carried through, though.
            if let Some(existing) = existing_api_key(&provider, &p.name) {
                options.insert("apiKey".into(), json!(existing));
            }
            set_cst_prop(
                &provider,
                &p.name,
                &json!({
                    "npm": "@ai-sdk/openai-compatible",
                    "options": options,
                    "models": models,
                }),
            );
        }
    }

    ensure_parent(path)?;
    std::fs::write(path, root.to_string()).map_err(CmdError::Io)
}

/// The generated OpenCode auth plugin's shared runtime: `makeAuth(id, url)`
/// builds one `auth` hook offering "Browser login" (PKCE against owallet's
/// local OAuth AS with `scope=provider`, which mints a wallet-scoped `owk_`
/// provider key — see `oauth_as.rs`) and plain "API key" paste. The
/// generated file appends one exported plugin function per provider entry;
/// OpenCode loads every function export of a plugin file as its own plugin,
/// and non-function exports are load errors, so the file exports nothing
/// else. Kept dependency-free (node:http / node:crypto only) so it runs
/// under OpenCode's Bun runtime with no install step.
const OPENCODE_AUTH_PLUGIN_RUNTIME: &str = r#"// Generated by `owallet install` — edits are overwritten on the next install.
//
// Browser-based auth for the owallet OpenAI-compatible provider(s):
// `opencode auth login` → pick the provider → "Browser login" opens the
// owallet consent page, which mints a wallet-scoped provider API key
// (revocable under "OpenCode provider" on the /wallet dashboard).
// "API key" pastes an owk_ key created on that dashboard instead.

function makeAuth(providerId, owalletUrl) {
  return {
    provider: providerId,
    async loader(getAuth) {
      const info = await getAuth()
      // Pasted api-type keys are applied by OpenCode itself; only a
      // browser-minted oauth credential needs mapping onto apiKey.
      if (!info || info.type !== "oauth") return {}
      return { apiKey: info.access }
    },
    methods: [
      {
        type: "oauth",
        label: "Browser login (mints a revocable provider key)",
        async authorize() {
          const { createServer } = await import("node:http")
          const { createHash, randomBytes } = await import("node:crypto")
          const b64url = (buf) =>
            buf.toString("base64").replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "")

          const verifier = b64url(randomBytes(32))
          const challenge = b64url(createHash("sha256").update(verifier).digest())
          const state = b64url(randomBytes(16))

          // Ephemeral localhost listener for the consent redirect. Both ends
          // are loopback: owallet serves the consent page, the browser lands
          // back here with the code.
          let settle
          const code = new Promise((resolve, reject) => {
            settle = { resolve, reject }
          })
          const server = createServer((req, res) => {
            const u = new URL(req.url, "http://127.0.0.1")
            if (u.pathname !== "/callback") {
              res.writeHead(404)
              res.end()
              return
            }
            const err = u.searchParams.get("error")
            const ok = !err && u.searchParams.get("state") === state && u.searchParams.get("code")
            res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" })
            res.end(
              ok
                ? "<h2>Approved — you can close this tab.</h2>"
                : "<h2>Login failed — you can close this tab.</h2>",
            )
            if (ok) settle.resolve(u.searchParams.get("code"))
            else settle.reject(new Error(err || "state mismatch"))
          })
          await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve))
          // Never hold the host process open: `opencode auth login` is a
          // one-shot CLI, and an abandoned login would otherwise pin it.
          server.unref()
          const redirectUri = `http://127.0.0.1:${server.address().port}/callback`

          const reg = await fetch(`${owalletUrl}/oauth/register`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({
              client_name: "opencode",
              redirect_uris: [redirectUri],
              scope: "provider",
            }),
          }).catch(() => undefined)
          if (!reg || !reg.ok) {
            server.close()
            throw new Error(
              `owallet is not reachable at ${owalletUrl} — start it with \`owallet serve\``,
            )
          }
          const { client_id } = await reg.json()

          const query = new URLSearchParams({
            response_type: "code",
            client_id,
            redirect_uri: redirectUri,
            scope: "provider",
            state,
            code_challenge: challenge,
            code_challenge_method: "S256",
          })

          return {
            url: `${owalletUrl}/oauth/authorize?${query}`,
            instructions:
              "Approve in the browser: pick a wallet and enter its password. " +
              "The key can be revoked any time from the owallet dashboard (/wallet).",
            method: "auto",
            async callback() {
              let timer
              try {
                // Bounded by owallet's own consent-session TTL (5 minutes).
                const value = await Promise.race([
                  code,
                  new Promise((_, reject) => {
                    timer = setTimeout(() => reject(new Error("timed out")), 300000)
                  }),
                ])
                const res = await fetch(`${owalletUrl}/oauth/token`, {
                  method: "POST",
                  headers: { "Content-Type": "application/x-www-form-urlencoded" },
                  body: new URLSearchParams({
                    grant_type: "authorization_code",
                    code: value,
                    redirect_uri: redirectUri,
                    client_id,
                    code_verifier: verifier,
                  }),
                })
                if (!res.ok) return { type: "failed" }
                const data = await res.json()
                if (!data.access_token) return { type: "failed" }
                // Provider keys neither expire nor refresh; expires: 0 is the
                // static-credential convention (cf. the github-copilot plugin).
                return { type: "success", access: data.access_token, refresh: "", expires: 0 }
              } catch {
                return { type: "failed" }
              } finally {
                // The armed race timer and any keep-alive browser connection
                // would each pin a one-shot `opencode auth login` process.
                clearTimeout(timer)
                server.close()
                server.closeAllConnections?.()
              }
            },
          }
        },
      },
      {
        type: "api",
        label: "API key (owk_... from the /wallet dashboard)",
      },
    ],
  }
}
"#;

/// Where the generated auth plugin lives for an OpenCode target. OpenCode
/// auto-discovers `{plugin,plugins}/*.{ts,js}` under each config directory
/// (`ConfigPlugin.load` in opencode) — no `opencode.json` reference needed.
fn opencode_plugin_path(t: Target) -> Result<PathBuf> {
    Ok(match t {
        Target::OpencodeLocal => PathBuf::from(".opencode/plugin/owallet.js"),
        Target::OpencodeGlobal => opencode_config_dir()?.join("plugin/owallet.js"),
        _ => unreachable!("plugin path only exists for OpenCode targets"),
    })
}

/// `overpay-dev` → `OverpayDevAuth`: a valid JS identifier per provider for
/// the plugin file's named exports.
fn js_export_name(provider_name: &str) -> String {
    let mut out = String::new();
    for part in provider_name.split(|c: char| !c.is_ascii_alphanumeric()) {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.extend(chars);
        }
    }
    out.push_str("Auth");
    out
}

/// Write the generated OpenCode auth plugin: the shared runtime plus one
/// exported plugin function per provider entry (a file's every export must
/// be a plugin function, and each carries exactly one `auth` hook, so
/// multi-env installs need one export per environment). The file is fully
/// generated and overwritten wholesale — unlike `opencode.json` there is no
/// user content to preserve.
fn write_opencode_plugin(path: &Path, providers: &[ProviderEntry]) -> Result<()> {
    let mut text = String::from(OPENCODE_AUTH_PLUGIN_RUNTIME);
    for p in providers {
        // serde_json string encoding == a valid JS string literal.
        text.push_str(&format!(
            "\nexport const {} = async () => ({{ auth: makeAuth({}, {}) }})\n",
            js_export_name(&p.name),
            serde_json::to_string(&p.name)?,
            serde_json::to_string(&p.base_url)?,
        ));
    }
    ensure_parent(path)?;
    std::fs::write(path, text).map_err(CmdError::Io)
}

/// A `provider.<name>.options.apiKey` the user set by hand, so rewriting
/// the block doesn't silently delete it. Returns `None` for the historical
/// `REPLACE_WITH_...` placeholder earlier versions wrote — that one is
/// meant to disappear, since the real key belongs in OpenCode's auth store.
fn existing_api_key(provider: &jsonc_parser::cst::CstObject, name: &str) -> Option<String> {
    const PLACEHOLDER: &str = "REPLACE_WITH_OWALLET_PROVIDER_KEY";
    let key = provider
        .object_value(name)?
        .object_value("options")?
        .get("apiKey")?
        .value()?
        .as_string_lit()?
        .decoded_value()
        .ok()?;
    (key != PLACEHOLDER).then_some(key)
}

/// Replace `name`'s value if the property already exists (keeping its
/// position and any comment attached to it), otherwise append it.
fn set_cst_prop(obj: &jsonc_parser::cst::CstObject, name: &str, value: &Value) {
    match obj.get(name) {
        Some(prop) => prop.set_value(cst_input(value)),
        None => {
            obj.append(name, cst_input(value));
        }
    }
}

/// `serde_json::Value` -> the CST's own input type. The values we write are
/// all built by `json!` right here, so the number case can't lose precision
/// in practice — `to_string` is what the CST wants anyway.
fn cst_input(value: &Value) -> jsonc_parser::cst::CstInputValue {
    use jsonc_parser::cst::CstInputValue;
    match value {
        Value::Null => CstInputValue::Null,
        Value::Bool(b) => CstInputValue::Bool(*b),
        Value::Number(n) => CstInputValue::Number(n.to_string()),
        Value::String(s) => CstInputValue::String(s.clone()),
        Value::Array(items) => CstInputValue::Array(items.iter().map(cst_input).collect()),
        Value::Object(map) => {
            CstInputValue::Object(map.iter().map(|(k, v)| (k.clone(), cst_input(v))).collect())
        }
    }
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

    /// `XDG_CONFIG_HOME` is process-wide, so these are `#[serial]` —
    /// restores the prior value on drop so an unrelated parallel test
    /// never sees a half-set env.
    struct XdgGuard(Option<std::ffi::OsString>);
    impl XdgGuard {
        fn set(value: Option<&str>) -> Self {
            let prev = std::env::var_os("XDG_CONFIG_HOME");
            match value {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
            Self(prev)
        }
    }
    impl Drop for XdgGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }

    #[test]
    #[serial_test::serial]
    fn opencode_global_defaults_to_dot_config_not_the_platform_config_dir() {
        // The bug this guards: `dirs::config_dir()` resolves to
        // ~/Library/Application Support on macOS, so install wrote a file
        // OpenCode never reads. OpenCode is XDG on every platform.
        let _guard = XdgGuard::set(None);
        let path = target_path(Target::OpencodeGlobal).unwrap();
        let home = dirs::home_dir().unwrap();
        assert_eq!(path, home.join(".config/opencode/opencode.json"));
    }

    #[test]
    #[serial_test::serial]
    fn opencode_global_honors_an_absolute_xdg_config_home() {
        let tmp = TempDir::new().unwrap();
        let _guard = XdgGuard::set(Some(tmp.path().to_str().unwrap()));
        let path = target_path(Target::OpencodeGlobal).unwrap();
        assert_eq!(path, tmp.path().join("opencode/opencode.json"));
    }

    #[test]
    #[serial_test::serial]
    fn opencode_global_ignores_a_relative_xdg_config_home() {
        // Invalid per the XDG spec — falling back beats writing somewhere
        // surprising relative to whatever cwd install happened to run in.
        let _guard = XdgGuard::set(Some("relative/path"));
        let path = target_path(Target::OpencodeGlobal).unwrap();
        let home = dirs::home_dir().unwrap();
        assert_eq!(path, home.join(".config/opencode/opencode.json"));
    }

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
            &[],
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["$schema"], "https://opencode.ai/config.json");
        assert_eq!(parsed["mcp"]["owallet"]["type"], "remote");
        assert_eq!(parsed["mcp"]["owallet"]["enabled"], true);
        assert_eq!(parsed["mcp"]["owallet"]["url"], "http://127.0.0.1:8765/mcp");
    }

    #[test]
    fn js_export_name_pascal_cases_provider_names() {
        assert_eq!(js_export_name("overpay"), "OverpayAuth");
        assert_eq!(js_export_name("overpay-dev"), "OverpayDevAuth");
        assert_eq!(js_export_name("overpay-staging"), "OverpayStagingAuth");
    }

    #[test]
    fn opencode_plugin_exports_one_auth_hook_per_provider() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".opencode/plugin/owallet.js");
        write_opencode_plugin(
            &path,
            &[
                ProviderEntry {
                    name: "overpay".into(),
                    base_url: "http://127.0.0.1:8765".into(),
                    models: vec!["default".into()],
                },
                ProviderEntry {
                    name: "overpay-dev".into(),
                    base_url: "http://127.0.0.1:8766".into(),
                    models: vec!["default".into()],
                },
            ],
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        // OpenCode treats every export of a plugin file as a plugin function;
        // the runtime itself must stay un-exported.
        assert!(!text.contains("export function makeAuth"));
        assert!(text.contains(
            r#"export const OverpayAuth = async () => ({ auth: makeAuth("overpay", "http://127.0.0.1:8765") })"#
        ));
        assert!(text.contains(
            r#"export const OverpayDevAuth = async () => ({ auth: makeAuth("overpay-dev", "http://127.0.0.1:8766") })"#
        ));
        // The browser flow requests the provider scope (mints an owk_ key).
        assert!(text.contains(r#"scope: "provider""#));
    }

    /// Drop whole-line `//` comments so a JSONC fixture can be asserted on
    /// with `serde_json`. Deliberately only matches a comment that *starts*
    /// a line — cutting at the first `//` anywhere would truncate every
    /// `http://` URL in the fixtures mid-string. Test-only; the real read
    /// path uses jsonc-parser, which handles the general case.
    fn strip_line_comments(text: &str) -> String {
        text.lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn opencode_json_reads_a_jsonc_file_with_comments() {
        // The real-world failure this fixes: OpenCode's config is JSONC and
        // users comment servers out by hand. A strict serde_json read blew
        // up with "key must be a string at line 4 column 5" and wrote
        // nothing at all.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("opencode.json");
        std::fs::write(
            &path,
            r#"{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    // "overpay": {
    //   "type": "remote",
    //   "url": "https://mcp.overpay.com/mcp"
    // },
    "other": { "type": "remote", "url": "http://other", "enabled": true }
  }
}
"#,
        )
        .unwrap();

        write_opencode_json(
            &path,
            &[McpEntry {
                name: "owallet".into(),
                url: "http://127.0.0.1:8765/mcp".into(),
            }],
            &[],
        )
        .unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        // The user's commented-out block survives verbatim -- a
        // to_string_pretty rewrite would have silently deleted it.
        assert!(
            text.contains(r#"// "overpay": {"#),
            "commented-out block must survive: {text}"
        );
        assert!(
            text.contains(r#"//   "url": "https://mcp.overpay.com/mcp""#),
            "every comment line must survive: {text}"
        );

        let parsed: Value = serde_json::from_str(&strip_line_comments(&text)).unwrap();
        assert_eq!(parsed["mcp"]["owallet"]["url"], "http://127.0.0.1:8765/mcp");
        assert_eq!(parsed["mcp"]["other"]["url"], "http://other");
    }

    #[test]
    fn opencode_json_replaces_an_existing_entry_in_place_keeping_comments() {
        // Rewriting an entry that already exists must update it where it
        // sits rather than dropping and re-appending it, so a comment the
        // user attached above it doesn't end up orphaned above something
        // else.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("opencode.json");
        std::fs::write(
            &path,
            r#"{
  "mcp": {
    // my local wallet
    "owallet": { "type": "remote", "url": "http://127.0.0.1:9999/mcp", "enabled": true },
    "keep": { "type": "remote", "url": "http://keep" }
  }
}
"#,
        )
        .unwrap();

        write_opencode_json(
            &path,
            &[McpEntry {
                name: "owallet".into(),
                url: "http://127.0.0.1:8765/mcp".into(),
            }],
            &[],
        )
        .unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("// my local wallet"), "comment lost: {text}");
        let parsed: Value = serde_json::from_str(&strip_line_comments(&text)).unwrap();
        assert_eq!(
            parsed["mcp"]["owallet"]["url"], "http://127.0.0.1:8765/mcp",
            "the port must be updated in place: {text}"
        );
        assert_eq!(parsed["mcp"]["keep"]["url"], "http://keep");
    }

    #[test]
    fn opencode_json_reports_genuinely_broken_input_instead_of_clobbering_it() {
        // JSONC is permissive, but not infinitely so. A file that can't be
        // parsed at all must be an error, not silently replaced with a
        // fresh config -- that would destroy whatever the user had.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("opencode.json");
        std::fs::write(&path, "{ this is not json at all ][").unwrap();
        let err = write_opencode_json(&path, &[], &[]).unwrap_err();
        assert!(
            matches!(&err, CmdError::BadInput(m) if m.contains("not valid JSON/JSONC")),
            "got {err:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{ this is not json at all ][",
            "the original file must be left untouched"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fetch_models_reads_the_model_enum_from_the_listing() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/listings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    {"id": "l1", "title": "OpenRouter Inference", "seller_slug": "openrouter-bot"}
                ]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/listings/l1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "l1",
                    "title": "OpenRouter Inference",
                    "buyer_note_schema": {
                        "type": "object",
                        "properties": {
                            "model": {
                                "enum": ["openai/gpt-5-mini", "anthropic/claude-haiku-4.5"]
                            }
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        let uri = server.uri();
        let models = tokio::task::spawn_blocking(move || fetch_models(&uri).unwrap())
            .await
            .unwrap();
        assert_eq!(
            models,
            vec![
                "openai/gpt-5-mini".to_string(),
                "anthropic/claude-haiku-4.5".to_string()
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fetch_models_surfaces_the_servers_own_error_message_not_just_a_status() {
        // Overpay is reachable but can't serve the listing (listing
        // missing, bot not registered ...). A bare "HTTP 502 Bad Gateway"
        // gives the user nothing to act on -- the server's own error body
        // names the real cause.
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/listings"))
            .respond_with(ResponseTemplate::new(502).set_body_json(json!({
                "error": {
                    "message": "could not find an 'OpenRouter Inference' listing",
                    "type": "api_error", "param": null, "code": null
                }
            })))
            .mount(&server)
            .await;

        let uri = server.uri();
        let err = tokio::task::spawn_blocking(move || fetch_models(&uri).unwrap_err())
            .await
            .unwrap();
        assert!(
            err.contains("could not find an 'OpenRouter Inference' listing"),
            "the server's own reason must reach the user: {err}"
        );
        assert!(err.contains("502"), "status is still useful context: {err}");
    }

    #[test]
    fn fetch_models_reports_a_clear_error_when_the_server_is_unreachable() {
        // A free port nothing is listening on -- the OS refuses the
        // connection immediately rather than hanging, so this doesn't need
        // a timeout to resolve.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let err = fetch_models(&format!("http://127.0.0.1:{port}")).unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn build_provider_entries_falls_back_to_the_default_sentinel_when_unreachable() {
        // Same "nothing listening" setup as the fetch_models test above,
        // but through the full build_provider_entries path -- this is what
        // actually runs during `owallet install --opencode-*`, and it must
        // still produce a usable (if minimal) provider entry rather than
        // silently dropping it. Uses a config file pointing OVERPAY_RAILS_URL
        // at the dead port, so the fallback is exercised without touching
        // the real prod endpoint.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().join("dev.owallet");
        std::fs::write(
            &cfg,
            format!("OVERPAY_RAILS_URL=http://127.0.0.1:{port}\nOWALLET_PORT={port}\n"),
        )
        .unwrap();

        let cli = Cli {
            config: Some(cfg),
            prod: false,
            dev: false,
            staging: false,
            command: crate::cli::Command::Install {
                claude_local: false,
                claude_global: false,
                opencode_local: true,
                opencode_global: false,
                codex_local: false,
                codex_global: false,
                port: Some(port),
            },
        };
        let providers = build_provider_entries(&cli, Some(port)).unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].name, "overpay-dev");
        assert_eq!(providers[0].models, vec![DEFAULT_MODEL.to_string()]);
    }

    #[test]
    fn opencode_json_writes_provider_with_live_model_catalog() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("opencode.json");
        write_opencode_json(
            &path,
            &[],
            &[ProviderEntry {
                name: "overpay".into(),
                base_url: "http://127.0.0.1:8765".into(),
                models: vec![
                    "openai/gpt-5-mini".into(),
                    "anthropic/claude-haiku-4.5".into(),
                ],
            }],
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let overpay = &parsed["provider"]["overpay"];
        assert_eq!(overpay["npm"], "@ai-sdk/openai-compatible");
        assert_eq!(overpay["options"]["baseURL"], "http://127.0.0.1:8765/v1");
        // No apiKey: OpenCode prompts and stores it in its own auth store.
        assert!(
            overpay["options"]["apiKey"].is_null(),
            "must not write an apiKey: {}",
            overpay["options"]
        );
        assert!(overpay["models"]["openai/gpt-5-mini"].is_object());
        assert!(overpay["models"]["anthropic/claude-haiku-4.5"].is_object());
    }

    #[test]
    fn opencode_json_drops_the_legacy_apikey_placeholder_but_keeps_a_real_one() {
        // Earlier versions wrote REPLACE_WITH_OWALLET_PROVIDER_KEY into the
        // config while OpenCode kept the real key in auth.json -- so the
        // placeholder was at best noise and at worst a bogus key sent to
        // the server. Re-running install clears it. A key the user actually
        // set by hand is not theirs to delete, though.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("opencode.json");
        std::fs::write(
            &path,
            r#"{
  "provider": {
    "stale": { "options": { "apiKey": "REPLACE_WITH_OWALLET_PROVIDER_KEY", "baseURL": "http://old/v1" } },
    "mine": { "options": { "apiKey": "sk-a-real-key", "baseURL": "http://old/v1" } }
  }
}
"#,
        )
        .unwrap();

        let provider = |name: &str| ProviderEntry {
            name: name.into(),
            base_url: "http://127.0.0.1:8765".into(),
            models: vec!["default".into()],
        };
        write_opencode_json(&path, &[], &[provider("stale"), provider("mine")]).unwrap();

        let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            parsed["provider"]["stale"]["options"]["apiKey"].is_null(),
            "the placeholder must be dropped: {}",
            parsed["provider"]["stale"]["options"]
        );
        assert_eq!(
            parsed["provider"]["mine"]["options"]["apiKey"], "sk-a-real-key",
            "a hand-set key must survive a rewrite"
        );
        // Both still get the fresh baseURL + models.
        assert_eq!(
            parsed["provider"]["stale"]["options"]["baseURL"],
            "http://127.0.0.1:8765/v1"
        );
        assert!(parsed["provider"]["mine"]["models"]["default"].is_object());
    }

    #[test]
    fn opencode_json_omits_provider_block_when_no_providers_fetched() {
        // With nothing to write, the `provider` key isn't added rather than
        // writing an empty `"provider": {}` block that would shadow whatever
        // the user already had there.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("opencode.json");
        std::fs::write(
            &path,
            r#"{"provider": {"anthropic": {"npm": "@ai-sdk/anthropic"}}}"#,
        )
        .unwrap();
        write_opencode_json(
            &path,
            &[McpEntry {
                name: "owallet".into(),
                url: "http://127.0.0.1:8765/mcp".into(),
            }],
            &[],
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["provider"]["anthropic"]["npm"], "@ai-sdk/anthropic");
        assert!(parsed["provider"]["overpay"].is_null());
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
