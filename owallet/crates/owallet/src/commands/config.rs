//! `owallet config` — show resolved URL config, or print the `.mcp.json` blob.

use owallet_config::defaults;
use serde_json::json;

use super::Result;

pub fn run(mcp: bool) -> Result<()> {
    let rails =
        std::env::var("OVERPAY_RAILS_URL").unwrap_or_else(|_| defaults::OVERPAY_RAILS_URL.into());
    let port: u16 = std::env::var("OWALLET_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(defaults::OWALLET_PORT);

    if mcp {
        // owallet no longer registers the hosted Overpay MCP — only the
        // local server entry (matches `mcp_config` in wallet_mcp/cli.py).
        let blob = json!({
            "mcpServers": {
                "owallet": {
                    "type": "http",
                    "url": format!("http://127.0.0.1:{port}/mcp"),
                },
            },
        });
        println!("{}", serde_json::to_string_pretty(&blob)?);
    } else {
        println!("OVERPAY_RAILS_URL = {rails}");
        println!("OWALLET_PORT      = {port}");
    }
    Ok(())
}
