//! Wallet-local timestamp rendering for the dashboard. The wallet's
//! timezone setting used to govern only the daily-budget window while
//! every displayed timestamp stayed UTC; these helpers close that gap.
//! Each formatted time carries its zone abbreviation — the dashboard
//! mixes money and times, so the reader should never have to guess the
//! reference frame — and a human "how long ago" string rides along for
//! hover titles.

use time::macros::format_description;
use time::OffsetDateTime;
use time_tz::{timezones, Offset, OffsetDateTimeExt, TimeZone, Tz};

/// Resolve the wallet's stored timezone name; UTC when unset or unknown
/// (matching how the budget window resolves it in owallet-db).
pub fn wallet_tz(name: Option<&str>) -> &'static Tz {
    name.and_then(timezones::get_by_name)
        .unwrap_or(timezones::db::UTC)
}

/// "2026-08-19 10:15 EDT" in the wallet's zone, or "—" for a missing or
/// unrepresentable timestamp.
pub fn format_in_tz(ts: Option<i64>, tz: &Tz) -> String {
    let Some(dt) = ts.and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok()) else {
        return "—".to_string();
    };
    let local = dt.to_timezone(tz);
    let fmt = format_description!("[year]-[month]-[day] [hour]:[minute]");
    match local.format(&fmt) {
        Ok(s) => format!("{s} {}", tz.get_offset_utc(&dt).name()),
        Err(_) => "—".to_string(),
    }
}

/// "3 minutes ago", computed against `now`, for hover titles. Empty for a
/// missing timestamp so templates can skip the attribute entirely; a
/// timestamp (slightly) in the future reads "just now" rather than
/// inventing negative ages out of clock skew.
pub fn relative_age(ts: Option<i64>, now: OffsetDateTime) -> String {
    let Some(dt) = ts.and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok()) else {
        return String::new();
    };
    let secs = (now - dt).whole_seconds();
    match secs {
        i64::MIN..=59 => "just now".to_string(),
        60..=3_599 => plural(secs / 60, "minute"),
        3_600..=86_399 => plural(secs / 3_600, "hour"),
        _ => plural(secs / 86_400, "day"),
    }
}

/// The current moment rendered for an `as_of` stamp on volatile tool
/// results — same shape as [`format_in_tz`].
pub fn as_of_now(tz: &Tz) -> String {
    format_in_tz(Some(OffsetDateTime::now_utc().unix_timestamp()), tz)
}

fn plural(n: i64, unit: &str) -> String {
    if n == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{n} {unit}s ago")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn formats_in_the_wallet_zone_with_its_abbreviation() {
        // 2026-08-19 14:15 UTC is 10:15 EDT (summer) in New York.
        let ts = datetime!(2026-08-19 14:15 UTC).unix_timestamp();
        let tz = wallet_tz(Some("America/New_York"));
        assert_eq!(format_in_tz(Some(ts), tz), "2026-08-19 10:15 EDT");

        // And 09:15 EST once daylight saving ends.
        let winter = datetime!(2026-12-19 14:15 UTC).unix_timestamp();
        assert_eq!(format_in_tz(Some(winter), tz), "2026-12-19 09:15 EST");
    }

    #[test]
    fn unknown_or_unset_zone_falls_back_to_utc() {
        let ts = datetime!(2026-08-19 14:15 UTC).unix_timestamp();
        assert_eq!(
            format_in_tz(Some(ts), wallet_tz(None)),
            "2026-08-19 14:15 UTC"
        );
        assert_eq!(
            format_in_tz(Some(ts), wallet_tz(Some("Not/AZone"))),
            "2026-08-19 14:15 UTC"
        );
        assert_eq!(format_in_tz(None, wallet_tz(None)), "—");
    }

    #[test]
    fn relative_ages_read_naturally() {
        let now = datetime!(2026-08-19 14:15 UTC);
        let at = |secs_ago: i64| Some(now.unix_timestamp() - secs_ago);
        assert_eq!(relative_age(at(5), now), "just now");
        assert_eq!(relative_age(at(60), now), "1 minute ago");
        assert_eq!(relative_age(at(240), now), "4 minutes ago");
        assert_eq!(relative_age(at(3 * 3600), now), "3 hours ago");
        assert_eq!(relative_age(at(3 * 86_400), now), "3 days ago");
        assert_eq!(
            relative_age(at(-30), now),
            "just now",
            "future = clock skew"
        );
        assert_eq!(relative_age(None, now), "");
    }
}
