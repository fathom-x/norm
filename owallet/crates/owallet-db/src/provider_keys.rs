//! Hashed API keys for the localhost OpenAI-compatible provider.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::Result;

/// The scope every provider key has: chat completions against `/v1`.
pub const PROVIDER_SCOPE_CHAT: &str = "chat";
/// Opt-in scope allowing the model-callable wallet spending tools
/// (`buy` / `pay_order` / …) on `/v1`. Never granted by default.
pub const PROVIDER_SCOPE_SPEND: &str = "spend";

/// The budget window key: the julian day number of the calendar date *in
/// the wallet's timezone* (an IANA name like "America/New_York"; UTC when
/// unset or unrecognized). Spend accounting is scoped to the current day
/// and resets at the wallet's local midnight — lazily, inside the same
/// guarded UPDATE that reserves, so no sweeper job is involved and
/// concurrent requests still can't double-spend. Public so tests and
/// callers interpreting stored `spent_day` values can name "today".
pub fn budget_day(tz_name: Option<&str>) -> i64 {
    use time_tz::OffsetDateTimeExt;
    let tz = tz_name
        .and_then(time_tz::timezones::get_by_name)
        .unwrap_or(time_tz::timezones::db::UTC);
    let now = time::OffsetDateTime::from_unix_timestamp(crate::now_secs())
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    i64::from(now.to_timezone(tz).date().to_julian_day())
}

/// Whether `name` is a known IANA timezone ("Europe/Berlin"). The one
/// validator both the dashboard form and [`crate::Database::write_timezone`]
/// use.
pub fn timezone_is_valid(name: &str) -> bool {
    time_tz::timezones::get_by_name(name).is_some()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderKeyRow {
    pub id: String,
    pub npub: String,
    pub created_at: i64,
    /// Who minted it — "dashboard" or "browser login". `None` on rows from
    /// before the column existed.
    pub label: Option<String>,
    /// The first few characters of the raw key (`owk_` + 8 hex), stored so
    /// the dashboard list is matchable against a key the user holds. Short
    /// enough to reveal nothing useful of the 256-bit token.
    pub token_prefix: Option<String>,
    /// Space-separated capability scopes ("chat", "chat spend"). `None` on
    /// rows from before the column existed — treated as chat-only.
    pub scopes: Option<String>,
    /// Per-day USD spending allowance for this key, in cents (the day
    /// boundary follows the wallet's timezone setting; UTC by default).
    /// `None` — rows from before the column, and keys minted without a
    /// limit — means no daily bound (the per-request cap still applies).
    pub daily_budget_usd_cents: Option<i64>,
    /// Cents the key's spending tools have moved **today** (the wallet's
    /// timezone). Normalized at read time: when the stored window is a
    /// past day, this reads as 0 without waiting for a write to roll the
    /// row over.
    pub spent_usd_cents: i64,
    /// Raw stored window (julian day, wallet timezone) — `None` until the
    /// key first spends. Diagnostic; `spent_usd_cents` is already
    /// normalized against it.
    pub spent_day: Option<i64>,
}

impl ProviderKeyRow {
    /// Whether this key may call the wallet spending tools on `/v1`.
    /// Absent scopes (pre-column rows) are chat-only.
    pub fn can_spend(&self) -> bool {
        scopes_allow_spend(self.scopes.as_deref())
    }

    /// Cents spent so far today (wallet timezone). Reads the normalized
    /// field — kept as a method so call sites say what they mean.
    pub fn spent_today_usd_cents(&self) -> i64 {
        self.spent_usd_cents
    }

    /// Cents left of today's budget (wallet timezone) — `None` when the
    /// key has no daily budget.
    pub fn remaining_today_usd_cents(&self) -> Option<i64> {
        self.daily_budget_usd_cents
            .map(|budget| (budget - self.spent_today_usd_cents()).max(0))
    }
}

/// Whether a scopes string grants the `spend` capability. `None` (rows
/// predating the column) is chat-only.
pub fn scopes_allow_spend(scopes: Option<&str>) -> bool {
    scopes
        .map(|s| s.split_whitespace().any(|w| w == PROVIDER_SCOPE_SPEND))
        .unwrap_or(false)
}

/// Outcome of trying to reserve part of a key's daily budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetReservation {
    /// The amount fit today's window — `spent_usd_cents` now includes it.
    Reserved,
    /// The amount would exceed the key's daily budget; nothing was
    /// recorded.
    OverBudget {
        daily_budget_usd_cents: i64,
        remaining_today_usd_cents: i64,
    },
    /// No such key (revoked mid-request); nothing was recorded.
    KeyMissing,
}

/// `?1` in every query below is the current budget day: `spent_usd_cents`
/// is normalized to 0 in SQL when the stored window is not today, so every
/// row handed out is already "as of now".
const SELECT_COLUMNS: &str =
    "id, npub, created_at, label, token_prefix, scopes, daily_budget_usd_cents, \
     CASE WHEN spent_day IS ?1 THEN COALESCE(spent_usd_cents, 0) ELSE 0 END, spent_day";

fn row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProviderKeyRow> {
    Ok(ProviderKeyRow {
        id: row.get(0)?,
        npub: row.get(1)?,
        created_at: row.get(2)?,
        label: row.get(3)?,
        token_prefix: row.get(4)?,
        scopes: row.get(5)?,
        daily_budget_usd_cents: row.get(6)?,
        spent_usd_cents: row.get(7)?,
        spent_day: row.get(8)?,
    })
}

pub(crate) fn insert(conn: &Connection, row: &ProviderKeyRow, token_hash: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO provider_keys(id, token_hash, npub, created_at, label, token_prefix, \
         scopes, daily_budget_usd_cents, spent_usd_cents, spent_day) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            row.id,
            token_hash,
            row.npub,
            row.created_at,
            row.label,
            row.token_prefix,
            row.scopes,
            row.daily_budget_usd_cents,
            row.spent_usd_cents,
            row.spent_day,
        ],
    )?;
    Ok(())
}

pub(crate) fn read_auth(
    conn: &Connection,
    token_hash: &str,
    today: i64,
) -> Result<Option<ProviderKeyRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {SELECT_COLUMNS} FROM provider_keys WHERE token_hash = ?2"),
            params![today, token_hash],
            row_from,
        )
        .optional()?)
}

pub(crate) fn read(conn: &Connection, id: &str, today: i64) -> Result<Option<ProviderKeyRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {SELECT_COLUMNS} FROM provider_keys WHERE id = ?2"),
            params![today, id],
            row_from,
        )
        .optional()?)
}

pub(crate) fn list(conn: &Connection, npub: &str, today: i64) -> Result<Vec<ProviderKeyRow>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {SELECT_COLUMNS} FROM provider_keys \
         WHERE npub = ?2 ORDER BY created_at DESC"
    ))?;
    let rows = statement.query_map(params![today, npub], row_from)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(crate) fn delete(conn: &Connection, id: &str, npub: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM provider_keys WHERE id = ?1 AND npub = ?2",
        params![id, npub],
    )?;
    Ok(())
}

/// Set (or clear, with `None`) a key's daily budget. Today's spend is
/// deliberately untouched — raising the budget takes effect immediately,
/// and lowering it below what's already spent today just stops further
/// spending until the wallet-local midnight. Scoped by `npub` like
/// `delete`, so a
/// session can only edit its own keys.
pub(crate) fn update_budget(
    conn: &Connection,
    id: &str,
    npub: &str,
    daily_budget_usd_cents: Option<i64>,
) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE provider_keys SET daily_budget_usd_cents = ?3 WHERE id = ?1 AND npub = ?2",
        params![id, npub, daily_budget_usd_cents],
    )?;
    Ok(changed > 0)
}

/// Atomically reserve `amount_cents` of the key's daily budget. The one
/// guarded UPDATE both rolls a stale window over to today (`spent_day IS
/// NOT ?today` ⇒ today's spend counts as 0) and refuses when the new total
/// wouldn't fit — so two concurrent requests on the same key cannot both
/// squeeze through the last dollar, and the wallet-local midnight needs
/// no sweeper job.
pub(crate) fn try_reserve_spend(
    conn: &Connection,
    id: &str,
    amount_cents: i64,
    today: i64,
) -> Result<BudgetReservation> {
    debug_assert!(amount_cents > 0);
    let changed = conn.execute(
        "UPDATE provider_keys \
         SET spent_usd_cents = CASE WHEN spent_day IS ?3 \
                                    THEN COALESCE(spent_usd_cents, 0) + ?2 \
                                    ELSE ?2 END, \
             spent_day = ?3 \
         WHERE id = ?1 \
           AND (daily_budget_usd_cents IS NULL \
                OR (CASE WHEN spent_day IS ?3 \
                         THEN COALESCE(spent_usd_cents, 0) \
                         ELSE 0 END) + ?2 <= daily_budget_usd_cents)",
        params![id, amount_cents, today],
    )?;
    if changed > 0 {
        return Ok(BudgetReservation::Reserved);
    }
    match read(conn, id, today)? {
        Some(row) => Ok(BudgetReservation::OverBudget {
            // The guard only refuses when a daily budget exists.
            daily_budget_usd_cents: row.daily_budget_usd_cents.unwrap_or(0),
            remaining_today_usd_cents: row.remaining_today_usd_cents().unwrap_or(0),
        }),
        None => Ok(BudgetReservation::KeyMissing),
    }
}

/// Hand back a reservation that turned out not to spend (payment refused
/// before anything moved). Floored at 0, and a no-op if the wallet-local
/// midnight already rolled the window (the reservation it would refund no
/// longer counts against anything).
pub(crate) fn release_spend(
    conn: &Connection,
    id: &str,
    amount_cents: i64,
    today: i64,
) -> Result<()> {
    debug_assert!(amount_cents > 0);
    conn.execute(
        "UPDATE provider_keys \
         SET spent_usd_cents = MAX(COALESCE(spent_usd_cents, 0) - ?2, 0) \
         WHERE id = ?1 AND spent_day IS ?3",
        params![id, amount_cents, today],
    )?;
    Ok(())
}

/// Record spend discovered after the fact (credit redemption amounts) —
/// unconditional, so it may overshoot the daily budget by the final
/// amount; the key just refuses everything else until the wallet-local
/// midnight.
pub(crate) fn record_spend(
    conn: &Connection,
    id: &str,
    amount_cents: i64,
    today: i64,
) -> Result<()> {
    debug_assert!(amount_cents > 0);
    conn.execute(
        "UPDATE provider_keys \
         SET spent_usd_cents = CASE WHEN spent_day IS ?3 \
                                    THEN COALESCE(spent_usd_cents, 0) + ?2 \
                                    ELSE ?2 END, \
             spent_day = ?3 \
         WHERE id = ?1",
        params![id, amount_cents, today],
    )?;
    Ok(())
}
