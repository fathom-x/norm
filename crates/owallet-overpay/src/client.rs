//! Async REST client for the Overpay Rails API.

use std::time::Duration;

use owallet_crypto::{nip98, PrivateKey};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Method, RequestBuilder, Response, Url};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::error::OverpayError;
use crate::models::{
    AccountInfo, LightningLoadResponse, ListingFilters, ListingsPage, LoadCoreCreditsRequest,
    MerchantCredits, MerchantCreditsList, OAuthRegisterRequest, OAuthRegisterResponse,
    OAuthTokenResponse, Order, OrderFilters, OrdersPage, PurchaseCreditsRequest,
    PurchaseCreditsResponse, RedeemCreditsRequest, RedeemCreditsResponse, WebSessionResponse,
};

/// Canonical key an Overpay bearer token is filed under in the wallet DB's
/// `tokens` table: the Overpay API base URL, normalized and without a
/// trailing slash.
///
/// Every entry point that stores or reads an Overpay bearer has to agree on
/// this — `owallet authorize`, the dashboard OAuth callback, and the MCP
/// tools all read the same row. Keying by anything local to one of those
/// entry points (the dashboard's own issuer URL, say) makes a wallet linked
/// through one path look unlinked to the others.
#[must_use]
pub fn host_key(overpay_base_url: &str) -> String {
    match Url::parse(overpay_base_url) {
        Ok(u) => u.as_str().trim_end_matches('/').to_string(),
        Err(_) => overpay_base_url.trim_end_matches('/').to_string(),
    }
}

/// Authentication strategy for a single request.
pub enum Auth<'a> {
    /// No `Authorization` header (e.g. public marketplace endpoints).
    None,
    /// Pre-stored OAuth bearer token.
    Bearer(&'a str),
    /// Wallet-key-signed NIP-98 envelope. The signing happens per-request
    /// because the canonical event includes the URL + method.
    Nip98(&'a PrivateKey),
    /// The bearer token, plus the wallet key's signature on every request in
    /// `X-Nostr-Signature` (NIP-98, with a `payload` tag over the exact body
    /// for POSTs). Overpay stores it as the buyer's signed authorization of
    /// a spend and refuses replays.
    BearerSigned(&'a str, &'a PrivateKey),
}

/// Header carrying the wallet's NIP-98 signature next to a bearer token.
pub const SIGNATURE_HEADER: &str = "x-nostr-signature";

#[derive(Clone)]
pub struct OverpayClient {
    base_url: Url,
    /// Public-facing URL used to rewrite any browser-targeted URLs the
    /// Rails app returns (matches `_to_public_url` in `server.py:123`).
    public_url: Url,
    http: reqwest::Client,
}

impl OverpayClient {
    pub fn new(base_url: &str) -> Result<Self, OverpayError> {
        let base = Url::parse(base_url)?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("owallet/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            base_url: base.clone(),
            public_url: base,
            http,
        })
    }

    /// Override the public-facing URL used for browser-targeted redirects
    /// (Docker / reverse-proxy scenarios — see `_OVERPAY_PUBLIC_URL` in
    /// `server.py:120`). Defaults to `base_url` if not set.
    pub fn with_public_url(mut self, public_url: &str) -> Result<Self, OverpayError> {
        self.public_url = Url::parse(public_url)?;
        Ok(self)
    }

    /// Rewrite a Rails-generated URL to use the public-facing host.
    #[must_use]
    pub fn to_public_url(&self, raw: &str) -> String {
        match Url::parse(raw) {
            Ok(mut u) => {
                if self.public_url != self.base_url
                    && u.host_str() == self.base_url.host_str()
                    && u.port_or_known_default() == self.base_url.port_or_known_default()
                {
                    let _ = u.set_scheme(self.public_url.scheme());
                    let _ = u.set_host(self.public_url.host_str());
                    let _ = u.set_port(self.public_url.port());
                }
                u.to_string()
            }
            Err(_) => raw.to_string(),
        }
    }

    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// Token-store key for the Overpay host this client talks to. Deriving
    /// it from the client means a caller can't file a bearer under one host
    /// and look it up under another. See [`host_key`].
    #[must_use]
    pub fn host_key(&self) -> String {
        host_key(self.base_url.as_str())
    }

    pub fn public_url(&self) -> &Url {
        &self.public_url
    }

    // ---- OAuth (PKCE) ----

    pub async fn register_oauth_client(
        &self,
        req: &OAuthRegisterRequest,
    ) -> Result<OAuthRegisterResponse, OverpayError> {
        let url = self.join("/oauth/clients")?;
        let resp = self.http.post(url).json(req).send().await?;
        decode_json(resp).await
    }

    /// Exchange a PKCE authorization code for an access token.
    pub async fn exchange_code(
        &self,
        client_id: &str,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<OAuthTokenResponse, OverpayError> {
        let url = self.join("/oauth/token")?;
        let body = [
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect_uri),
        ];
        let resp = self.http.post(url).form(&body).send().await?;
        decode_json(resp).await
    }

    /// Build the browser-facing authorization URL the user must visit. The
    /// `redirect_uri` must match what was passed to `register_oauth_client`.
    pub fn authorize_url(
        &self,
        client_id: &str,
        redirect_uri: &str,
        state: &str,
        code_challenge: &str,
        scope: &str,
    ) -> Result<Url, OverpayError> {
        let mut url = self.join_public("/oauth/authorize")?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", scope)
            .append_pair("state", state)
            .append_pair("code_challenge", code_challenge)
            .append_pair("code_challenge_method", crate::pkce::Pkce::method());
        Ok(url)
    }

    // ---- API endpoints ----

    pub async fn account(&self, auth: Auth<'_>) -> Result<AccountInfo, OverpayError> {
        self.get_json("/api/v1/account", auth).await
    }

    /// Raw-`Value` variant of [`account`]. Used by `get_account_info`'s
    /// MCP tool to forward the full Rails `{data: {...}}` envelope to
    /// the consumer verbatim (fathom-x/overpay#288 — matches Python's
    /// `result["account"] = resp.json()`).
    pub async fn account_value(&self, auth: Auth<'_>) -> Result<Value, OverpayError> {
        self.get_json_value("/api/v1/account", auth).await
    }

    pub async fn web_session(&self, auth: Auth<'_>) -> Result<WebSessionResponse, OverpayError> {
        self.post_json::<_, WebSessionResponse>(
            "/api/v1/buyer/web_session",
            auth,
            &serde_json::json!({}),
        )
        .await
    }

    pub async fn list_listings(
        &self,
        filters: &ListingFilters,
    ) -> Result<ListingsPage, OverpayError> {
        let url = self.listings_url(filters)?;
        let resp = self.http.get(url).send().await?;
        decode_json(resp).await
    }

    /// Raw-`Value` variant of [`list_listings`]. Returns the Rails
    /// response verbatim so MCP consumers see byte-identical wire to
    /// the Python tool (fathom-x/overpay#288).
    pub async fn list_listings_value(
        &self,
        filters: &ListingFilters,
    ) -> Result<Value, OverpayError> {
        let url = self.listings_url(filters)?;
        let resp = self.http.get(url).send().await?;
        decode_value(resp).await
    }

    pub async fn list_orders(
        &self,
        auth: Auth<'_>,
        filters: &OrderFilters,
    ) -> Result<OrdersPage, OverpayError> {
        let url = self.orders_url(filters)?;
        let req = self.build(Method::GET, url, auth)?;
        let resp = req.send().await?;
        decode_json(resp).await
    }

    /// Raw-`Value` variant of [`list_orders`]. See `list_listings_value`.
    pub async fn list_orders_value(
        &self,
        auth: Auth<'_>,
        filters: &OrderFilters,
    ) -> Result<Value, OverpayError> {
        let url = self.orders_url(filters)?;
        let req = self.build(Method::GET, url, auth)?;
        let resp = req.send().await?;
        decode_value(resp).await
    }

    pub async fn get_order(&self, id: &str, auth: Auth<'_>) -> Result<Order, OverpayError> {
        self.get_json(&format!("/api/v1/orders/{id}"), auth).await
    }

    /// Raw-`Value` variant of [`get_order`]. See `list_listings_value`.
    pub async fn get_order_value(&self, id: &str, auth: Auth<'_>) -> Result<Value, OverpayError> {
        self.get_json_value(&format!("/api/v1/orders/{id}"), auth)
            .await
    }

    /// [`Self::get_order_value`], conditional on the streaming buffer: with
    /// `since_seq` set to the `partial_seq` already seen, the marketplace
    /// leaves an unchanged `partial_content` out of the response (it still
    /// sends `partial_seq`), so polling a long reply doesn't re-download it.
    pub async fn get_order_value_since(
        &self,
        id: &str,
        since_seq: Option<u64>,
        auth: Auth<'_>,
    ) -> Result<Value, OverpayError> {
        match since_seq {
            Some(seq) => {
                self.get_json_value(&format!("/api/v1/orders/{id}?since_seq={seq}"), auth)
                    .await
            }
            None => self.get_order_value(id, auth).await,
        }
    }

    /// Largest delivered file [`Self::fetch_delivered_content`] reads.
    pub const MAX_DELIVERED_CONTENT_BYTES: usize = 16 * 1024 * 1024;

    /// Fetch an order's result when Rails delivered it as a file
    /// (`delivered_content_url`, an Active Storage link) instead of inline
    /// `delivered_content`. The link is signed, so no auth header is sent;
    /// it must point at this marketplace's own origin (base or public URL) —
    /// the redirect it answers with, to blob storage, is followed.
    ///
    /// Text comes back as [`DeliveredContent::Text`]. Anything else — an
    /// image, a PDF, bytes that are not UTF-8 — is a successful delivery
    /// too, returned as [`DeliveredContent::Binary`] (type and size) for the
    /// caller to hand on as a link: decoding a generated PNG as UTF-8 once
    /// failed the whole paid call with "file is not UTF-8 text". A file
    /// whose `Content-Type` already says binary is not downloaded at all.
    pub async fn fetch_delivered_content(
        &self,
        url: &str,
    ) -> Result<DeliveredContent, OverpayError> {
        let target = Url::parse(url)?;
        let same_origin = |known: &Url| {
            known.scheme() == target.scheme()
                && known.host_str() == target.host_str()
                && known.port_or_known_default() == target.port_or_known_default()
        };
        if !same_origin(&self.base_url) && !same_origin(&self.public_url) {
            return Err(OverpayError::Delivery(format!(
                "refusing to fetch from {}, which is not this marketplace",
                target.host_str().unwrap_or("an unknown host")
            )));
        }
        let resp = self.http.get(target).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(OverpayError::HttpStatus {
                status: status.as_u16(),
                body: resp.text().await.unwrap_or_default(),
            });
        }
        let content_type = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap_or(v).trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty());
        if content_type.as_deref().is_some_and(is_binary_media_type) {
            return Ok(DeliveredContent::Binary {
                content_type,
                bytes: resp.content_length(),
            });
        }
        let bytes = resp.bytes().await?;
        if bytes.len() > Self::MAX_DELIVERED_CONTENT_BYTES {
            return Err(OverpayError::Delivery(format!(
                "file is {} bytes, over the {}-byte limit",
                bytes.len(),
                Self::MAX_DELIVERED_CONTENT_BYTES
            )));
        }
        let len = bytes.len() as u64;
        Ok(match String::from_utf8(bytes.to_vec()) {
            Ok(text) => DeliveredContent::Text(text),
            Err(_) => DeliveredContent::Binary {
                content_type,
                bytes: Some(len),
            },
        })
    }

    pub async fn create_order(
        &self,
        listing_id: &str,
        buyer_note: Option<&str>,
        auth: Auth<'_>,
    ) -> Result<Order, OverpayError> {
        let body = serde_json::json!({
            "listing_id": listing_id,
            "buyer_note": buyer_note,
        });
        self.post_json("/api/v1/orders", auth, &body).await
    }

    /// Raw-`Value` variant of [`create_order`]. See `list_listings_value`.
    /// `buyer_note` is sent as a JSON string (matching Python's
    /// `Optional[str]` shape — the seller bot `JSON.parse`s it). Callers
    /// that want to pass a structured note should serialize it to a JSON
    /// string first.
    pub async fn create_order_value(
        &self,
        listing_id: &str,
        buyer_note: Option<&str>,
        auth: Auth<'_>,
    ) -> Result<Value, OverpayError> {
        let body = serde_json::json!({
            "listing_id": listing_id,
            "buyer_note": buyer_note,
        });
        self.post_json_value("/api/v1/orders", auth, &body).await
    }

    /// Create *and pay* an order on a metered listing with a buyer-set
    /// authorization — the most the seller may charge for it. Rails only
    /// accepts `authorization_cents` alongside `pay: "merchant_credits"`
    /// (a partly released hold can only go back as credits), and treats it
    /// as all-or-nothing: credits that don't cover it cancel the order and
    /// answer `402 insufficient_credits`. The response carries the order
    /// (`data`) and the redemption (`payment`).
    pub async fn create_paid_order_value(
        &self,
        listing_id: &str,
        buyer_note: Option<&str>,
        authorization_cents: i64,
        auth: Auth<'_>,
    ) -> Result<Value, OverpayError> {
        let body = serde_json::json!({
            "listing_id": listing_id,
            "buyer_note": buyer_note,
            "pay": "merchant_credits",
            "authorization_cents": authorization_cents,
        });
        self.post_json_value("/api/v1/orders", auth, &body).await
    }

    /// Raw-`Value` fetch of a single listing (`GET /api/v1/listings/{id}`).
    /// Returns the full Rails response including the `{data: {...}}`
    /// envelope and any `buyer_note_schema` / `checkout_schema` fields.
    /// Public endpoint — no auth required.
    pub async fn get_listing_value(&self, id: &str) -> Result<Value, OverpayError> {
        self.get_json_value(&format!("/api/v1/listings/{id}"), Auth::None)
            .await
    }

    // ---- Merchant credits ----

    pub async fn list_merchant_credits(
        &self,
        auth: Auth<'_>,
    ) -> Result<MerchantCreditsList, OverpayError> {
        self.get_json("/api/v1/merchant_credits", auth).await
    }

    /// Raw-`Value` variant of [`list_merchant_credits`]. See
    /// `list_listings_value`.
    pub async fn list_merchant_credits_value(&self, auth: Auth<'_>) -> Result<Value, OverpayError> {
        self.get_json_value("/api/v1/merchant_credits", auth).await
    }

    pub async fn get_merchant_credits(
        &self,
        seller_slug: &str,
        auth: Auth<'_>,
    ) -> Result<MerchantCredits, OverpayError> {
        self.get_json(&format!("/api/v1/merchant_credits/{seller_slug}"), auth)
            .await
    }

    /// Raw-`Value` variant of [`get_merchant_credits`]. See
    /// `list_listings_value`.
    pub async fn get_merchant_credits_value(
        &self,
        seller_slug: &str,
        auth: Auth<'_>,
    ) -> Result<Value, OverpayError> {
        self.get_json_value(&format!("/api/v1/merchant_credits/{seller_slug}"), auth)
            .await
    }

    pub async fn purchase_merchant_credits(
        &self,
        seller_slug: &str,
        amount_cents: i64,
        auth: Auth<'_>,
    ) -> Result<PurchaseCreditsResponse, OverpayError> {
        let body = PurchaseCreditsRequest { amount_cents };
        self.post_json(
            &format!("/api/v1/merchant_credits/{seller_slug}/purchase"),
            auth,
            &body,
        )
        .await
    }

    /// Load core marketplace credits via Lightning. Calls
    /// `POST /api/v1/merchant_credits/load` and returns a BOLT11 invoice
    /// plus order metadata for polling.
    pub async fn load_core_credits(
        &self,
        amount_cents: i64,
        auth: Auth<'_>,
    ) -> Result<LightningLoadResponse, OverpayError> {
        let body = LoadCoreCreditsRequest { amount_cents };
        self.post_json("/api/v1/merchant_credits/load", auth, &body)
            .await
    }

    pub async fn redeem_merchant_credits(
        &self,
        seller_slug: &str,
        order_id: &str,
        auth: Auth<'_>,
    ) -> Result<RedeemCreditsResponse, OverpayError> {
        let body = RedeemCreditsRequest {
            order_id: order_id.to_string(),
        };
        self.post_json(
            &format!("/api/v1/merchant_credits/{seller_slug}/redeem"),
            auth,
            &body,
        )
        .await
    }

    /// Raw-`Value` variant of [`redeem_merchant_credits`]. See
    /// `list_listings_value`.
    pub async fn redeem_merchant_credits_value(
        &self,
        seller_slug: &str,
        order_id: &str,
        auth: Auth<'_>,
    ) -> Result<Value, OverpayError> {
        let body = RedeemCreditsRequest {
            order_id: order_id.to_string(),
        };
        self.post_json_value(
            &format!("/api/v1/merchant_credits/{seller_slug}/redeem"),
            auth,
            &body,
        )
        .await
    }

    // ---- Internal helpers ----

    fn join(&self, path: &str) -> Result<Url, OverpayError> {
        Ok(self.base_url.join(path)?)
    }

    fn listings_url(&self, filters: &ListingFilters) -> Result<Url, OverpayError> {
        let mut url = self.join("/api/v1/listings")?;
        {
            let mut q = url.query_pairs_mut();
            if let Some(c) = &filters.category {
                q.append_pair("category", c);
            }
            if let Some(s) = &filters.seller_slug {
                // Rails reads `params[:seller]`, not `params[:seller_slug]`
                // (listings_controller.rb#index) — the field here is named
                // `seller_slug` for clarity on our side, but the wire param
                // must match Rails's name or this filters nothing.
                q.append_pair("seller", s);
            }
            if let Some(c) = &filters.cursor {
                q.append_pair("cursor", c);
            }
            if let Some(n) = filters.limit {
                q.append_pair("limit", &n.to_string());
            }
        }
        Ok(url)
    }

    fn orders_url(&self, filters: &OrderFilters) -> Result<Url, OverpayError> {
        let mut url = self.join("/api/v1/orders")?;
        {
            let mut q = url.query_pairs_mut();
            // Rails accepts `payment_status=...`, not `status=...`
            // (orders_controller.rb#index).
            if let Some(s) = &filters.payment_status {
                q.append_pair("payment_status", s);
            }
            if let Some(s) = &filters.fulfillment_status {
                q.append_pair("fulfillment_status", s);
            }
            if let Some(c) = &filters.cursor {
                q.append_pair("cursor", c);
            }
            if let Some(n) = filters.limit {
                q.append_pair("limit", &n.to_string());
            }
            if let Some(addr) = &filters.payer_address {
                q.append_pair("payer_address", addr);
            }
        }
        Ok(url)
    }

    fn join_public(&self, path: &str) -> Result<Url, OverpayError> {
        Ok(self.public_url.join(path)?)
    }

    fn build(
        &self,
        method: Method,
        url: Url,
        auth: Auth<'_>,
    ) -> Result<RequestBuilder, OverpayError> {
        self.build_with_body(method, url, auth, None)
    }

    /// [`Self::build`] for a request with a body: signatures cover these
    /// exact bytes (NIP-98 `payload` tag), so the caller must send them
    /// unchanged — never re-serialise.
    fn build_with_body(
        &self,
        method: Method,
        url: Url,
        auth: Auth<'_>,
        body: Option<&[u8]>,
    ) -> Result<RequestBuilder, OverpayError> {
        let mut req = self.http.request(method.clone(), url.clone());
        req = req.headers(auth_headers(method.as_str(), url.as_str(), auth, body)?);
        Ok(req)
    }

    /// A JSON POST whose signature covers the body: serialise once, sign
    /// those bytes, send those bytes.
    fn post_signed<B: Serialize>(
        &self,
        url: Url,
        auth: Auth<'_>,
        body: &B,
    ) -> Result<RequestBuilder, OverpayError> {
        let bytes = serde_json::to_vec(body)?;
        let req = self.build_with_body(Method::POST, url, auth, Some(&bytes))?;
        Ok(req
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .body(bytes))
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        auth: Auth<'_>,
    ) -> Result<T, OverpayError> {
        let url = self.join(path)?;
        let resp = self.build(Method::GET, url, auth)?.send().await?;
        decode_json(resp).await
    }

    async fn post_json<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        auth: Auth<'_>,
        body: &B,
    ) -> Result<T, OverpayError> {
        let url = self.join(path)?;
        let resp = self.post_signed(url, auth, body)?.send().await?;
        decode_json(resp).await
    }

    /// Raw-`Value` GET. Returns the Rails response body verbatim — no
    /// envelope unwrap, no struct flattening. The MCP tool layer uses
    /// this so wallets see byte-identical output across Python and
    /// Rust (fathom-x/overpay#288).
    async fn get_json_value(&self, path: &str, auth: Auth<'_>) -> Result<Value, OverpayError> {
        let url = self.join(path)?;
        let resp = self.build(Method::GET, url, auth)?.send().await?;
        decode_value(resp).await
    }

    /// Same passthrough story as [`get_json_value`] but for POST.
    async fn post_json_value<B: Serialize>(
        &self,
        path: &str,
        auth: Auth<'_>,
        body: &B,
    ) -> Result<Value, OverpayError> {
        let url = self.join(path)?;
        let resp = self.post_signed(url, auth, body)?.send().await?;
        decode_value(resp).await
    }
}

fn auth_headers(
    method: &str,
    url: &str,
    auth: Auth<'_>,
    body: Option<&[u8]>,
) -> Result<HeaderMap, OverpayError> {
    let sign = |sk: &PrivateKey| match body {
        Some(body) => nip98::sign_with_body(sk, url, method, body),
        None => nip98::sign(sk, url, method),
    };
    let value = |s: &str| HeaderValue::from_str(s).map_err(|e| OverpayError::Sign(e.to_string()));
    let mut h = HeaderMap::new();
    match auth {
        Auth::None => {}
        Auth::Bearer(token) => {
            h.insert(AUTHORIZATION, value(&format!("Bearer {token}"))?);
        }
        Auth::Nip98(sk) => {
            h.insert(AUTHORIZATION, value(&sign(sk))?);
        }
        Auth::BearerSigned(token, sk) => {
            h.insert(AUTHORIZATION, value(&format!("Bearer {token}"))?);
            h.insert(HeaderName::from_static(SIGNATURE_HEADER), value(&sign(sk))?);
        }
    }
    // Always announce JSON. Silently ignored if Content-Type is set later.
    h.insert(
        HeaderName::from_static("accept"),
        HeaderValue::from_static("application/json"),
    );
    Ok(h)
}

async fn decode_json<T: DeserializeOwned>(resp: Response) -> Result<T, OverpayError> {
    let status = resp.status();
    let bytes = resp.bytes().await?;
    if !status.is_success() {
        let body = String::from_utf8_lossy(&bytes).into_owned();
        return Err(OverpayError::HttpStatus {
            status: status.as_u16(),
            body,
        });
    }
    Ok(serde_json::from_slice(&bytes)?)
}

/// Same shape as [`decode_json`] but returns the raw `serde_json::Value`
/// for the verbatim-passthrough call sites.
async fn decode_value(resp: Response) -> Result<Value, OverpayError> {
    let status = resp.status();
    let bytes = resp.bytes().await?;
    if !status.is_success() {
        let body = String::from_utf8_lossy(&bytes).into_owned();
        return Err(OverpayError::HttpStatus {
            status: status.as_u16(),
            body,
        });
    }
    Ok(serde_json::from_slice(&bytes)?)
}

/// What [`OverpayClient::fetch_delivered_content`] found behind a
/// delivered file's link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveredContent {
    /// UTF-8 text, safe to inline as `delivered_content`.
    Text(String),
    /// Anything else (an image, audio, a PDF…): its media type and size,
    /// when known. The bytes are not kept.
    Binary {
        content_type: Option<String>,
        bytes: Option<u64>,
    },
}

/// Media types that are never text, so there is no point downloading
/// them to find out. Unknown and `application/octet-stream` files are
/// still read and tested for UTF-8 — seller bots upload text as either.
fn is_binary_media_type(media_type: &str) -> bool {
    ["image/", "audio/", "video/", "font/"]
        .iter()
        .any(|p| media_type.starts_with(p))
        || matches!(
            media_type,
            "application/pdf"
                | "application/zip"
                | "application/gzip"
                | "application/x-tar"
                | "application/wasm"
        )
}

#[cfg(test)]
mod host_key_tests {
    use super::*;

    #[test]
    fn trailing_slash_does_not_change_the_key() {
        assert_eq!(host_key("http://localhost:3001/"), "http://localhost:3001");
        assert_eq!(host_key("http://localhost:3001"), "http://localhost:3001");
    }

    #[test]
    fn client_and_free_fn_agree() {
        // The CLI has the raw configured URL; `serve` only has the built
        // client. Both have to land on the same row.
        let raw = "http://localhost:3001/";
        let client = OverpayClient::new(raw).unwrap();
        assert_eq!(client.host_key(), host_key(raw));
    }

    #[test]
    fn a_path_prefixed_host_keeps_its_path() {
        assert_eq!(
            host_key("https://gw.example/overpay/"),
            "https://gw.example/overpay"
        );
    }

    #[test]
    fn an_unparseable_url_falls_back_to_a_trim() {
        assert_eq!(host_key("not a url/"), "not a url");
    }
}
