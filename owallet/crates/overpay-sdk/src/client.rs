//! The client and its transport: auth headers, retries and idempotency keys.

use std::sync::Arc;
use std::time::Duration;

use rand::Rng;
use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER};
use reqwest::{Method, StatusCode};
use serde_json::Value;
use url::Url;

use crate::auth::{Auth, SIGNATURE_HEADER};
use crate::error::{ApiError, Error};
use crate::response::RawResponse;
use crate::ErrorCode;

/// Header carrying a request's idempotency key.
pub const IDEMPOTENCY_KEY_HEADER: &str = "Idempotency-Key";
/// Header the API sets on a replayed idempotent response.
pub const IDEMPOTENT_REPLAYED_HEADER: &str = "Idempotent-Replayed";

/// When to retry a request that failed in a way that may be transient.
///
/// Only requests that are safe to repeat are retried: GETs, and POSTs that
/// carry an idempotency key (the API replays the first response to a
/// repeat). GETs are retried on connection errors, timeouts, and 429, 502,
/// 503 and 504 responses. A POST is by default retried only when it
/// certainly wasn't acted on — a connection that never opened, 429, 503 —
/// because against a server that doesn't honor `Idempotency-Key` a repeat
/// after a timeout, 502 or 504 could act twice; see
/// [`ClientBuilder::retry_ambiguous_writes`]. A keyed POST told that its
/// first attempt is still running (409 `idempotency_request_in_progress`)
/// is asked again: the server replays that attempt's response. Delays grow
/// exponentially with full jitter, and a `Retry-After` header (in seconds)
/// is honored up to `max_delay`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total attempts, including the first. 1 disables retries.
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl RetryPolicy {
    /// `max_attempts` in all (1 disables retries), waiting from
    /// `base_delay`, doubling, up to `max_delay` between attempts.
    #[must_use]
    pub const fn new(max_attempts: u32, base_delay: Duration, max_delay: Duration) -> Self {
        Self {
            max_attempts,
            base_delay,
            max_delay,
        }
    }

    /// One attempt; never retry.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            max_attempts: 1,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
        }
    }

    fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        if let Some(wait) = retry_after {
            return wait.min(self.max_delay);
        }
        let exp = self
            .base_delay
            .saturating_mul(1u32 << (attempt - 1).min(16))
            .min(self.max_delay);
        let millis = u64::try_from(exp.as_millis()).unwrap_or(u64::MAX);
        Duration::from_millis(rand::thread_rng().gen_range(0..=millis))
    }
}

impl Default for RetryPolicy {
    /// Three attempts, 250 ms base delay, at most 5 s between attempts.
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(5),
        }
    }
}

/// Builds a [`Client`].
#[derive(Debug, Clone)]
pub struct ClientBuilder {
    base_url: String,
    public_url: Option<String>,
    user_agent: String,
    timeout: Duration,
    retry: RetryPolicy,
    idempotency_keys: bool,
    retry_ambiguous_writes: bool,
    http: Option<reqwest::Client>,
}

impl ClientBuilder {
    /// The URL browsers use to reach the marketplace, when it differs from
    /// the API base (Docker, reverse proxies). Browser-facing URLs — the
    /// OAuth authorize page, `to_public_url` — use it.
    #[must_use]
    pub fn public_url(mut self, url: impl Into<String>) -> Self {
        self.public_url = Some(url.into());
        self
    }

    /// `User-Agent` (default `overpay-sdk/<version>`).
    #[must_use]
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = user_agent.into();
        self
    }

    /// Per-attempt timeout (default 30 s). Ignored with [`Self::http_client`].
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Retry policy (default [`RetryPolicy::default`]).
    #[must_use]
    pub fn retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Whether requests that create something (POST /api/v1/orders) get an
    /// automatic `Idempotency-Key` when the caller didn't set one, which
    /// is what makes them safe to retry. Default on.
    #[must_use]
    pub fn idempotency_keys(mut self, on: bool) -> Self {
        self.idempotency_keys = on;
        self
    }

    /// Also retry a keyed POST after a timeout, 502 or 504 — failures where
    /// the server may have acted before the response was lost. Safe only
    /// against a marketplace that honors `Idempotency-Key` (it replays the
    /// first response instead of acting again); default off.
    #[must_use]
    pub fn retry_ambiguous_writes(mut self, on: bool) -> Self {
        self.retry_ambiguous_writes = on;
        self
    }

    /// Use this reqwest client (its timeout, proxy and TLS settings) as is.
    /// reqwest's types are part of this crate's API (here and in
    /// [`Error::Transport`]), so a reqwest major version is an SDK one.
    #[must_use]
    pub fn http_client(mut self, http: reqwest::Client) -> Self {
        self.http = Some(http);
        self
    }

    pub fn build(self) -> Result<Client, Error> {
        let base_url = Url::parse(&self.base_url)?;
        let public_url = match &self.public_url {
            Some(url) => Url::parse(url)?,
            None => base_url.clone(),
        };
        let http = match self.http {
            Some(http) => http,
            None => reqwest::Client::builder()
                .timeout(self.timeout)
                .user_agent(self.user_agent)
                .build()?,
        };
        Ok(Client {
            inner: Arc::new(Inner {
                base_url,
                public_url,
                http,
                retry: self.retry,
                idempotency_keys: self.idempotency_keys,
                retry_ambiguous_writes: self.retry_ambiguous_writes,
            }),
        })
    }
}

/// A client for one Overpay marketplace. Cheap to clone.
///
/// ```no_run
/// # async fn demo() -> Result<(), overpay_sdk::Error> {
/// use overpay_sdk::{Auth, Client, ListingQuery};
///
/// let client = Client::new("https://overpay.example")?;
/// let page = client.listings().list(&ListingQuery::default()).await?.parse()?;
/// for listing in &page.data {
///     println!("{} — {}", listing.title, listing.price_usd);
/// }
/// let account = client.account().get(Auth::Bearer("token")).await?.parse()?;
/// # let _ = account; Ok(()) }
/// ```
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    base_url: Url,
    public_url: Url,
    http: reqwest::Client,
    retry: RetryPolicy,
    idempotency_keys: bool,
    retry_ambiguous_writes: bool,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base_url", &self.inner.base_url.as_str())
            .finish_non_exhaustive()
    }
}

/// A request body.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum Body {
    None,
    Json(Value),
    Form(Vec<(String, String)>),
}

/// One API request, for [`Client::execute`] — the escape hatch for calls
/// the typed resources don't cover.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct Request {
    pub method: Method,
    /// Path joined onto the base URL (`/api/v1/...`), or an absolute URL.
    pub path: String,
    pub query: Vec<(String, String)>,
    pub body: Body,
    /// Sent as `Idempotency-Key`. A POST with one is retried.
    pub idempotency_key: Option<String>,
    /// Send `Accept: application/json` (default true).
    pub accept_json: bool,
}

impl Request {
    #[must_use]
    pub fn new(method: Method, path: impl Into<String>) -> Self {
        Self {
            method,
            path: path.into(),
            query: Vec::new(),
            body: Body::None,
            idempotency_key: None,
            accept_json: true,
        }
    }

    #[must_use]
    pub fn get(path: impl Into<String>) -> Self {
        Self::new(Method::GET, path)
    }

    #[must_use]
    pub fn post(path: impl Into<String>) -> Self {
        Self::new(Method::POST, path)
    }

    /// Add a query parameter (skipped when `value` is `None`).
    #[must_use]
    pub fn query(mut self, key: &str, value: Option<impl ToString>) -> Self {
        if let Some(value) = value {
            self.query.push((key.to_string(), value.to_string()));
        }
        self
    }

    #[must_use]
    pub fn json(mut self, body: Value) -> Self {
        self.body = Body::Json(body);
        self
    }

    #[must_use]
    pub fn form(mut self, fields: Vec<(String, String)>) -> Self {
        self.body = Body::Form(fields);
        self
    }

    #[must_use]
    pub fn idempotency_key(mut self, key: Option<String>) -> Self {
        self.idempotency_key = key;
        self
    }

    fn retryable(&self) -> bool {
        self.method == Method::GET || self.method == Method::HEAD || self.idempotency_key.is_some()
    }
}

impl Client {
    /// A client with default settings.
    pub fn new(base_url: &str) -> Result<Self, Error> {
        Self::builder(base_url).build()
    }

    #[must_use]
    pub fn builder(base_url: impl Into<String>) -> ClientBuilder {
        ClientBuilder {
            base_url: base_url.into(),
            public_url: None,
            user_agent: concat!("overpay-sdk/", env!("CARGO_PKG_VERSION")).to_string(),
            timeout: Duration::from_secs(30),
            retry: RetryPolicy::default(),
            idempotency_keys: true,
            retry_ambiguous_writes: false,
            http: None,
        }
    }

    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.inner.base_url
    }

    #[must_use]
    pub fn public_url(&self) -> &Url {
        &self.inner.public_url
    }

    #[must_use]
    pub fn retry_policy(&self) -> RetryPolicy {
        self.inner.retry
    }

    /// The API base URL without a trailing slash — a stable key to file
    /// credentials for this marketplace under. See [`host_key`].
    #[must_use]
    pub fn host_key(&self) -> String {
        host_key(self.inner.base_url.as_str())
    }

    /// Rewrite a URL the API returned on the API host to the public host
    /// (see [`ClientBuilder::public_url`]). Other URLs pass through.
    #[must_use]
    pub fn to_public_url(&self, raw: &str) -> String {
        let (base, public) = (&self.inner.base_url, &self.inner.public_url);
        match Url::parse(raw) {
            Ok(mut u) => {
                if public != base
                    && u.host_str() == base.host_str()
                    && u.port_or_known_default() == base.port_or_known_default()
                {
                    let _ = u.set_scheme(public.scheme());
                    let _ = u.set_host(public.host_str());
                    let _ = u.set_port(public.port());
                }
                u.to_string()
            }
            Err(_) => raw.to_string(),
        }
    }

    /// The underlying reqwest client.
    #[must_use]
    pub fn http(&self) -> &reqwest::Client {
        &self.inner.http
    }

    pub(crate) fn join(&self, path: &str) -> Result<Url, Error> {
        Ok(self.inner.base_url.join(path)?)
    }

    pub(crate) fn join_public(&self, path: &str) -> Result<Url, Error> {
        Ok(self.inner.public_url.join(path)?)
    }

    /// A fresh idempotency key when automatic keys are on.
    pub(crate) fn auto_idempotency_key(&self) -> Option<String> {
        self.inner.idempotency_keys.then(new_idempotency_key)
    }

    /// Send `request`, retrying per the client's [`RetryPolicy`] when it is
    /// safe to, and return the JSON body of a 2xx response. A non-2xx
    /// response is [`Error::Api`].
    pub async fn execute(&self, request: Request, auth: Auth<'_>) -> Result<RawResponse, Error> {
        let mut url = self.join(&request.path)?;
        if !request.query.is_empty() {
            url.query_pairs_mut().extend_pairs(&request.query);
        }
        let policy = if request.retryable() {
            self.inner.retry
        } else {
            RetryPolicy::none()
        };
        // A read may always be repeated; a write only where it certainly
        // wasn't acted on, unless the caller vouches for idempotency.
        let ambiguous_ok = request.method == Method::GET
            || request.method == Method::HEAD
            || self.inner.retry_ambiguous_writes;
        let mut attempt = 1;
        loop {
            match self.send_once(&request, &url, auth).await {
                Ok(resp) => {
                    let status = resp.status();
                    if attempt < policy.max_attempts && retryable_status(status, ambiguous_ok) {
                        let wait = policy.delay(attempt, retry_after(&resp));
                        attempt += 1;
                        tokio::time::sleep(wait).await;
                        continue;
                    }
                    // A keyed write whose first attempt is still running
                    // on the server (its response was lost on the way
                    // back): asking again is safe — the server replays
                    // the stored response once that attempt finishes.
                    if attempt < policy.max_attempts
                        && status == StatusCode::CONFLICT
                        && request.idempotency_key.is_some()
                    {
                        let wait = policy.delay(attempt, retry_after(&resp));
                        let err = ApiError::from_response(status.as_u16(), &resp.bytes().await?);
                        if err.code != Some(ErrorCode::IdempotencyRequestInProgress) {
                            return Err(err.into());
                        }
                        attempt += 1;
                        tokio::time::sleep(wait).await;
                        continue;
                    }
                    return read_response(resp).await;
                }
                Err(Error::Transport(e))
                    if attempt < policy.max_attempts
                        && (e.is_connect() || (ambiguous_ok && e.is_timeout())) =>
                {
                    let wait = policy.delay(attempt, None);
                    attempt += 1;
                    tokio::time::sleep(wait).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn send_once(
        &self,
        request: &Request,
        url: &Url,
        auth: Auth<'_>,
    ) -> Result<reqwest::Response, Error> {
        let mut builder = self.inner.http.request(request.method.clone(), url.clone());
        // The body is serialised once, here: a signature's `payload` tag is
        // the hash of these exact bytes, and these are the bytes sent.
        let body = encode_body(&request.body)?;
        // Signed per attempt: NIP-98 events expire and must name the exact
        // URL the server sees. (A retry in the same second keeps the event
        // id, which hashes key, time, kind and tags; its signature is new.)
        let headers = auth.headers(
            request.method.as_str(),
            url.as_str(),
            body.as_ref().map(|(bytes, _)| bytes.as_slice()),
        )?;
        let header = |v: &str| HeaderValue::from_str(v).map_err(|e| Error::Sign(e.to_string()));
        if let Some(value) = &headers.authorization {
            builder = builder.header(AUTHORIZATION, header(value)?);
        }
        if let Some(value) = &headers.signature {
            builder = builder.header(SIGNATURE_HEADER, header(value)?);
        }
        if request.accept_json {
            builder = builder.header(ACCEPT, HeaderValue::from_static("application/json"));
        }
        if let Some(key) = &request.idempotency_key {
            builder = builder.header(IDEMPOTENCY_KEY_HEADER, key.as_str());
        }
        if let Some((bytes, content_type)) = body {
            builder = builder
                .header(CONTENT_TYPE, HeaderValue::from_static(content_type))
                .body(bytes);
        }
        Ok(builder.send().await?)
    }
}

/// A request body as the bytes to send and their content type.
fn encode_body(body: &Body) -> Result<Option<(Vec<u8>, &'static str)>, Error> {
    Ok(match body {
        Body::None => None,
        Body::Json(value) => Some((serde_json::to_vec(value)?, "application/json")),
        Body::Form(fields) => {
            let encoded = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(fields)
                .finish();
            Some((encoded.into_bytes(), "application/x-www-form-urlencoded"))
        }
    })
}

/// 429 and 503 mean the server didn't act; after a 502 or 504 it may have.
fn retryable_status(status: StatusCode, ambiguous_ok: bool) -> bool {
    match status.as_u16() {
        429 | 503 => true,
        502 | 504 => ambiguous_ok,
        _ => false,
    }
}

fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    resp.headers()
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

async fn read_response(resp: reqwest::Response) -> Result<RawResponse, Error> {
    let status = resp.status();
    let replayed = resp
        .headers()
        .get(IDEMPOTENT_REPLAYED_HEADER)
        .is_some_and(|v| v.as_bytes() == b"true");
    let bytes = resp.bytes().await?;
    if !status.is_success() {
        return Err(ApiError::from_response(status.as_u16(), &bytes).into());
    }
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)?
    };
    Ok(RawResponse {
        status: status.as_u16(),
        replayed,
        body,
    })
}

/// A random 128-bit key, hex.
#[must_use]
pub fn new_idempotency_key() -> String {
    let bytes: [u8; 16] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The canonical key credentials for a marketplace are filed under: its API
/// base URL, normalized, without a trailing slash.
#[must_use]
pub fn host_key(base_url: &str) -> String {
    match Url::parse(base_url) {
        Ok(u) => u.as_str().trim_end_matches('/').to_string(),
        Err(_) => base_url.trim_end_matches('/').to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_key_trims_and_normalizes() {
        assert_eq!(host_key("http://localhost:3001/"), "http://localhost:3001");
        assert_eq!(
            host_key("https://gw.example/overpay/"),
            "https://gw.example/overpay"
        );
        assert_eq!(host_key("not a url/"), "not a url");
        assert_eq!(
            Client::new("http://localhost:3001/").unwrap().host_key(),
            "http://localhost:3001"
        );
    }

    #[test]
    fn public_url_rewrites_only_the_api_host() {
        let client = Client::builder("http://web:3000")
            .public_url("https://overpay.example")
            .build()
            .unwrap();
        assert_eq!(
            client.to_public_url("http://web:3000/orders/1"),
            "https://overpay.example/orders/1"
        );
        assert_eq!(client.to_public_url("http://other/x"), "http://other/x");
    }

    #[test]
    fn retry_delay_honors_retry_after_up_to_the_cap() {
        let policy = RetryPolicy::default();
        assert_eq!(
            policy.delay(1, Some(Duration::from_secs(2))),
            Duration::from_secs(2)
        );
        assert_eq!(
            policy.delay(1, Some(Duration::from_secs(60))),
            policy.max_delay
        );
        assert!(policy.delay(3, None) <= policy.max_delay);
    }

    #[test]
    fn idempotency_keys_are_unique_hex() {
        let (a, b) = (new_idempotency_key(), new_idempotency_key());
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }
}
