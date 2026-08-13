//! In-memory web session store.
//!
//! Matches `_web_sessions` and `_WEB_SESSION_TTL` from `wallet_mcp/server.py:194-195`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use rand::RngCore;

/// 1-hour session TTL (matches `_WEB_SESSION_TTL` in server.py).
pub const SESSION_TTL: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone)]
pub enum SessionRole {
    /// Full access — authenticated with the master DB password.
    Admin,
    /// Scoped to a single wallet's npub.
    Wallet { npub: String },
}

#[derive(Debug, Clone)]
pub struct WebSession {
    pub role: SessionRole,
    pub expires_at: Instant,
}

impl WebSession {
    pub fn is_alive(&self) -> bool {
        Instant::now() < self.expires_at
    }

    pub fn npub(&self) -> Option<&str> {
        match &self.role {
            SessionRole::Wallet { npub } => Some(npub),
            SessionRole::Admin => None,
        }
    }

    pub fn is_admin(&self) -> bool {
        matches!(self.role, SessionRole::Admin)
    }
}

/// Thread-safe in-memory session map. Clone is cheap (Arc'd).
#[derive(Debug, Clone, Default)]
pub struct SessionStore {
    inner: Arc<DashMap<String, WebSession>>,
}

impl SessionStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a fresh session, returning the random token to store in the
    /// `owallet_session` cookie.
    pub fn insert(&self, role: SessionRole) -> String {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        let token = hex::encode(bytes);
        self.inner.insert(
            token.clone(),
            WebSession {
                role,
                expires_at: Instant::now() + SESSION_TTL,
            },
        );
        token
    }

    /// Look up a session by cookie value. Returns `None` if missing or
    /// expired (and lazily evicts expired entries).
    pub fn get(&self, token: &str) -> Option<WebSession> {
        let s = self.inner.get(token)?.clone();
        if s.is_alive() {
            Some(s)
        } else {
            self.inner.remove(token);
            None
        }
    }

    pub fn remove(&self, token: &str) {
        self.inner.remove(token);
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}
