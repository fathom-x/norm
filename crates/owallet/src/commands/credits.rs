//! `owallet credits load` — mint a Lightning invoice for core credits and
//! display a terminal QR code the user scans from any Lightning wallet.

use std::time::Duration;

use owallet_db::default_db_path;
use owallet_overpay::Auth;
use qrcode::render::unicode;
use qrcode::{EcLevel, QrCode};

use super::overpay::{block_on, client as overpay_client, host_key};
use super::{open_unlock, CmdError, Result};

pub fn run(amount_cents: i64, wait: bool) -> Result<()> {
    let db = open_unlock(&default_db_path())?;
    let npub = db
        .read_default_npub()?
        .ok_or_else(|| CmdError::BadInput("no default wallet — run `owallet select`".into()))?;
    let token = db
        .read_token(&npub, &host_key())?
        .ok_or(CmdError::NotAuthorized)?;
    let overpay = overpay_client()?;

    let resp = block_on(async {
        overpay
            .load_core_credits(amount_cents, Auth::Bearer(&token))
            .await
    })?;

    let usd = resp.amount_cents as f64 / 100.0;
    println!("Lightning invoice — ${:.2} ({} sats)", usd, resp.sats);
    if let Some(ref exp) = resp.expires_at {
        println!("Expires: {exp}");
    }

    // All-uppercase so the QR encoder can use the compact alphanumeric mode.
    let uri = format!("LIGHTNING:{}", resp.bolt11.to_uppercase());
    match QrCode::with_error_correction_level(uri.as_bytes(), EcLevel::L) {
        Ok(code) => {
            let image = code
                .render::<unicode::Dense1x2>()
                .dark_color(unicode::Dense1x2::Dark)
                .light_color(unicode::Dense1x2::Light)
                .build();
            println!("\n{image}");
        }
        Err(e) => {
            eprintln!("warning: could not render QR code: {e}");
        }
    }

    println!("BOLT11:\n  {}", resp.bolt11);
    if let Some(ref url) = resp.order_url {
        let public_url = overpay.to_public_url(url);
        println!("Order: {public_url}");
    }

    if wait {
        println!("\nWaiting for payment…");
        block_on(async { poll_until_paid(&overpay, &resp.order_id, &token).await })?;
        println!("Payment confirmed — credits loaded.");
        print_credits(&overpay, &token);
    }

    Ok(())
}

fn print_credits(overpay: &owallet_overpay::OverpayClient, token: &str) {
    match block_on(async { overpay.list_merchant_credits(Auth::Bearer(token)).await }) {
        Ok(credits) if !credits.data.is_empty() => {
            println!("\nMerchant credit balances:");
            for c in &credits.data {
                let slug = c
                    .organization_slug
                    .as_deref()
                    .or(c.seller_slug.as_deref())
                    .unwrap_or("?");
                let balance = c.formatted_balance.as_deref().unwrap_or("?");
                println!("  {slug}: {balance}");
            }
        }
        _ => {}
    }
}

async fn poll_until_paid(
    overpay: &owallet_overpay::OverpayClient,
    order_id: &str,
    token: &str,
) -> Result<()> {
    loop {
        tokio::time::sleep(Duration::from_secs(3)).await;
        match overpay.get_order(order_id, Auth::Bearer(token)).await {
            Ok(order) => match order.payment_status.as_deref() {
                Some("paid") => return Ok(()),
                Some("expired") => {
                    return Err(CmdError::BadInput("invoice expired before payment".into()));
                }
                _ => {}
            },
            Err(e) => eprintln!("poll error (will retry): {e}"),
        }
    }
}
