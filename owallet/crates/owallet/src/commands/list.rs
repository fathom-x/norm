//! `owallet list <what>` — currently supports only `marketplace`.

use owallet_overpay::models::ListingFilters;

use super::overpay::{block_on, client as overpay_client};
use super::Result;
use crate::cli::ListWhat;

pub fn run(what: ListWhat) -> Result<()> {
    match what {
        ListWhat::Marketplace {
            category,
            seller,
            cursor,
            limit,
        } => marketplace(category, seller, cursor, limit),
    }
}

fn marketplace(
    category: Option<String>,
    seller: Option<String>,
    cursor: Option<String>,
    limit: u32,
) -> Result<()> {
    let overpay = overpay_client()?;
    let filters = ListingFilters {
        category,
        seller_slug: seller,
        cursor,
        limit: Some(limit),
    };
    // Use the raw-`Value` API so the CLI renders against the Rails wire
    // directly. Keeps Python ↔ Rust parity (fathom-x/overpay#288): if
    // Rails adds/renames a field, the CLI's `[...].as_str()` lookups
    // surface the change instead of being silently dropped by a typed
    // struct.
    let page = block_on(async { overpay.list_listings_value(&filters).await })?;

    let empty = Vec::new();
    let listings = page
        .get("data")
        .and_then(|d| d.as_array())
        .unwrap_or(&empty);
    if listings.is_empty() {
        println!("No listings.");
        return Ok(());
    }

    println!("{:<24}  {:>10}  {:<16}  TITLE", "ID", "PRICE USD", "SELLER");
    for l in listings {
        let id = l.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let price = l.get("price_usd").and_then(|v| v.as_str()).unwrap_or("");
        // Prefer the nested seller (name, then slug); fall back to a flat
        // `seller_slug`. Mirrors `seller_name()` in wallet_mcp/cli.py.
        let seller = l
            .get("seller")
            .and_then(|s| s.get("name").or_else(|| s.get("slug")))
            .and_then(|v| v.as_str())
            .or_else(|| l.get("seller_slug").and_then(|v| v.as_str()))
            .unwrap_or("");
        let title = l.get("title").and_then(|v| v.as_str()).unwrap_or("");
        println!("{id:<24}  {price:>10}  {seller:<16}  {title}");
    }
    if let Some(c) = page.get("next_cursor").and_then(|v| v.as_str()) {
        println!("\nNext page: --cursor {c}");
    }
    Ok(())
}
