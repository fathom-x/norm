//! Time for the native and browser builds alike.
//!
//! `std::time::Instant::now()`/`SystemTime::now()` panic on
//! wasm32-unknown-unknown and tokio's timer needs its runtime, which the
//! browser build doesn't have. Everything in this crate reads time through
//! here: natively these are `std::time` and `tokio::time` exactly as
//! before; in the browser `web-time` (`performance.now()` / `Date.now()`)
//! and `setTimeout` via gloo-timers.

use std::future::Future;
use std::time::Duration;

pub use web_time::{Instant, SystemTime, UNIX_EPOCH};

/// Wait `d`.
#[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
pub async fn sleep(d: Duration) {
    tokio::time::sleep(d).await;
}

/// Wait `d` (browser: a `setTimeout`; marked `Send` like every browser
/// future, see [`owallet_overpay::compat`]).
#[cfg(all(target_family = "wasm", target_os = "unknown"))]
pub async fn sleep(d: Duration) {
    let ms = u32::try_from(d.as_millis()).unwrap_or(u32::MAX);
    owallet_overpay::sendable(gloo_timers::future::sleep(Duration::from_millis(
        u64::from(ms),
    )))
    .await;
}

/// `fut` ran past its deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Elapsed;

impl std::fmt::Display for Elapsed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("deadline elapsed")
    }
}

impl std::error::Error for Elapsed {}

/// Run `fut`, giving up after `d` (drops it, like `tokio::time::timeout`).
pub async fn timeout<F: Future>(d: Duration, fut: F) -> Result<F::Output, Elapsed> {
    tokio::select! {
        biased;
        out = fut => Ok(out),
        () = sleep(d) => Err(Elapsed),
    }
}

/// Seconds since the Unix epoch (0 if the clock is before it).
#[must_use]
pub fn unix_now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn timeout_passes_through_a_fast_future() {
        assert_eq!(timeout(Duration::from_secs(5), async { 7 }).await, Ok(7));
    }

    #[tokio::test]
    async fn timeout_gives_up_on_a_slow_one() {
        let slow = sleep(Duration::from_secs(30));
        assert_eq!(timeout(Duration::from_millis(10), slow).await, Err(Elapsed));
    }

    #[tokio::test]
    async fn sleep_and_instant_agree() {
        let start = Instant::now();
        sleep(Duration::from_millis(20)).await;
        assert!(start.elapsed() >= Duration::from_millis(20));
    }

    #[test]
    fn unix_now_is_after_2024() {
        assert!(unix_now_secs() > 1_700_000_000);
    }
}
