//! `owallet demo-credits [--claim]` — a demo deployment's one-time credits.
//!
//! Some Overpay deployments (staging) grant each new account a small
//! core-credit balance once (`GET`/`POST /api/v1/demo_credits`). Without
//! `--claim` this reports whether they are offered and claimed, plus the
//! wallet's core-credit balance — what a front end needs to decide whether to
//! offer them. Authenticates with the stored bearer, else NIP-98.

use owallet_crypto::derive_from_stored_seed;
use owallet_db::default_db_path;
use owallet_overpay::{Auth, OverpayError};
use serde_json::{json, Value};

use super::overpay::{block_on, client as overpay_client, host_key};
use super::{open_unlock, CmdError, Result};

pub fn run(claim: bool, json: bool) -> Result<()> {
    let db = open_unlock(&default_db_path())?;
    let npub = db
        .read_default_npub()?
        .ok_or_else(|| CmdError::BadInput("no default wallet — run `owallet select`".into()))?;
    let token = db.read_token(&npub, &host_key())?;
    let sk = match token {
        Some(_) => None,
        None => {
            let seed = db
                .read_seed(&npub)?
                .ok_or_else(|| CmdError::NotFound(npub.clone()))?;
            Some(derive_from_stored_seed(&seed)?)
        }
    };
    let auth = || match (&token, &sk) {
        (Some(t), _) => Auth::Bearer(t),
        (None, Some(k)) => Auth::Nip98(k),
        (None, None) => unreachable!("either a token or a key"),
    };
    let overpay = overpay_client()?;

    block_on(async {
        if claim {
            match overpay.claim_demo_credits_value(auth()).await {
                Ok(value) => {
                    let data = &value["data"];
                    if json {
                        println!("{data}");
                    } else {
                        println!(
                            "Added {} of demo credits — balance {}",
                            usd(data["granted_cents"].as_i64().unwrap_or(0)),
                            usd(data["balance_cents"].as_i64().unwrap_or(0)),
                        );
                    }
                    Ok(())
                }
                Err(OverpayError::HttpStatus { status, body }) => {
                    let (code, message) = refusal(status, &body);
                    if json {
                        println!("{}", json!({"error": {"code": code, "message": message}}));
                    }
                    Err(CmdError::BadInput(format!(
                        "demo credits refused ({code}): {message}"
                    )))
                }
                Err(e) => Err(e.into()),
            }
        } else {
            // An Overpay that predates the endpoint (404) offers none.
            let offer = match overpay.demo_credits_value(auth()).await {
                Ok(v) => v["data"].clone(),
                Err(OverpayError::HttpStatus { status: 404, .. }) => {
                    json!({"enabled": false, "amount_cents": 0, "granted": false})
                }
                Err(e) => return Err(e.into()),
            };
            let credits = overpay.list_merchant_credits_value(auth()).await?;
            let core = core_balance_cents(&credits);
            if json {
                let mut out = offer.clone();
                out["core_balance_cents"] = json!(core);
                println!("{out}");
            } else {
                println!("Core credits: {}", usd(core));
                match (offer["enabled"].as_bool(), offer["granted"].as_bool()) {
                    (Some(true), Some(true)) => println!("Demo credits: already claimed"),
                    (Some(true), _) => println!(
                        "Demo credits: {} offered — `owallet demo-credits --claim`",
                        usd(offer["amount_cents"].as_i64().unwrap_or(0))
                    ),
                    _ => println!("Demo credits: not offered by this Overpay"),
                }
            }
            Ok(())
        }
    })
}

/// The credits norm can spend anywhere: the core organisation's, summed.
pub(crate) fn core_balance_cents(body: &Value) -> i64 {
    body["data"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|r| r["core"].as_bool() == Some(true))
                .filter_map(|r| r["balance_cents"].as_i64())
                .sum()
        })
        .unwrap_or(0)
}

/// Overpay's refusal `{error, code}` → (code, message).
fn refusal(status: u16, body: &str) -> (String, String) {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let code = v["code"].as_str().unwrap_or("overpay_error").to_string();
    let message = v["error"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| format!("Overpay answered HTTP {status}"));
    (code, message)
}

fn usd(cents: i64) -> String {
    format!("${}.{:02}", cents / 100, cents % 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_core_credits_count() {
        let body = json!({"data": [
            {"core": true, "balance_cents": 250},
            {"core": false, "seller_slug": "x", "balance_cents": 900},
            {"core": true, "balance_cents": 50},
        ]});
        assert_eq!(core_balance_cents(&body), 300);
        assert_eq!(core_balance_cents(&json!({})), 0);
    }

    #[test]
    fn refusals_read_overpay_code() {
        let (code, msg) = refusal(409, r#"{"error":"already","code":"already_granted"}"#);
        assert_eq!(
            (code.as_str(), msg.as_str()),
            ("already_granted", "already")
        );
        assert_eq!(refusal(500, "<html>").0, "overpay_error");
    }
}
