//! Askama template structs. Kept in one file so the templates can share
//! types and the dashboard handlers stay focused on request handling.

use askama::Template;
use owallet_db::WalletRow;

#[derive(Template)]
#[template(path = "login.html")]
pub struct LoginTemplate {
    pub role: &'static str,
    pub identifier: String,
    pub error: Option<String>,
}

#[derive(Template)]
#[template(path = "dashboard.html")]
pub struct DashboardTemplate {
    pub is_admin: bool,
    pub active: Option<WalletRow>,
    pub wallets: Vec<WalletRow>,
    pub default_npub: Option<String>,
    pub notice: Option<String>,
    /// True when the active wallet has a stored Overpay bearer token under
    /// the dashboard's host_key. Drives which Overpay button is shown
    /// (Open Overpay vs Link Overpay account).
    pub has_overpay_token: bool,
    /// Live Overpay username for the active wallet. Falls back to the
    /// cached `wallets.overpay_username` if the API fetch fails. Often
    /// `None` — anonymous Mullvad-style accounts have no display name.
    pub overpay_username: Option<String>,
    /// Live Overpay account number for the active wallet. This is the
    /// 16-digit anonymous identifier — it's what we surface for accounts
    /// without a username set.
    pub overpay_account_number: Option<String>,
    /// Display name of the configured EVM chain (e.g. "Base") for the
    /// on-chain balance rows. `None` when no active wallet / no address.
    pub chain_name: Option<String>,
    /// Pre-formatted ETH balance string (e.g. "0.0123 ETH" or a
    /// "(could not fetch …)" notice). `None` when no active wallet.
    pub eth_balance: Option<String>,
    /// Pre-formatted USDC balance string. Same semantics as `eth_balance`.
    pub usdc_balance: Option<String>,
}

#[derive(Template)]
#[template(path = "generate.html")]
pub struct GenerateTemplate {
    pub error: Option<String>,
    /// Selected mnemonic length (defaults to 24) — preserved across an
    /// error re-render so the radio/select keeps the user's choice.
    pub words: u8,
}

#[derive(Template)]
#[template(path = "generate_seed.html")]
pub struct GenerateSeedTemplate {
    pub npub: String,
    pub address: String,
    pub phrase: String,
}

#[derive(Template)]
#[template(path = "import.html")]
pub struct ImportTemplate {
    pub error: Option<String>,
}

#[derive(Template)]
#[template(path = "password.html")]
pub struct PasswordTemplate {
    pub npub: String,
    pub error: Option<String>,
}

#[derive(Template)]
#[template(path = "send_result.html")]
pub struct SendResultTemplate {
    pub ok: bool,
    pub npub: String,
    pub chain_name: String,
    pub to: String,
    pub amount: String,
    pub tx_hash: String,
    pub block_number: Option<u64>,
    pub explorer_url: Option<String>,
    pub error: Option<String>,
}

/// One row in the `/wallet/purchases` list.
pub struct PurchaseListRow {
    pub order_id: String,
    pub title: String,
    pub seller: String,
    pub badge_class: String,
    pub status_label: String,
    pub amount: String,
    pub when: String,
}

#[derive(Template)]
#[template(path = "purchases_list.html")]
pub struct PurchasesListTemplate {
    pub npub_short: String,
    pub count: i64,
    pub notice: Option<String>,
    pub notice_is_error: bool,
    pub rows: Vec<PurchaseListRow>,
}

#[derive(Template)]
#[template(path = "purchase_detail.html")]
pub struct PurchaseDetailTemplate {
    pub title: String,
    pub badge_class: String,
    pub status_label: String,
    pub meta: Vec<(String, String)>,
    /// Pre-rendered, escaped delivered-content HTML — injected with `|safe`.
    pub content_html: String,
}
