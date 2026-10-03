//! Portability shims for the browser (`wasm32-unknown-unknown`) build.
//!
//! On wasm32 the browser's `fetch` futures hold JS handles and are
//! `!Send`, but axum handlers — and the `Sse` streams owallet builds on
//! them — must be `Send`. wasm32-unknown-unknown without the `atomics`
//! target feature is single-threaded: nothing can move a value to another
//! thread, so asserting `Send` there is sound. [`sendable`] does exactly
//! that on wasm32 and is the identity everywhere else, so native builds
//! keep the compiler's real `Send` checking.

use std::future::Future;

/// True when compiled for the browser (`wasm32-unknown-unknown`).
pub const IS_BROWSER: bool = cfg!(all(target_family = "wasm", target_os = "unknown"));

/// Identity natively: the future must already be `Send` where it matters.
#[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
#[inline]
pub fn sendable<F: Future>(f: F) -> F {
    f
}

/// Mark a single-threaded browser future `Send` (see the module docs).
#[cfg(all(target_family = "wasm", target_os = "unknown"))]
#[inline]
pub fn sendable<F: Future>(f: F) -> SendFuture<F> {
    SendFuture(f)
}

#[cfg(all(
    target_family = "wasm",
    target_os = "unknown",
    target_feature = "atomics"
))]
compile_error!("owallet's browser build assumes single-threaded wasm (no `atomics`)");

/// A future asserted `Send` because the target has a single thread.
#[cfg(all(target_family = "wasm", target_os = "unknown"))]
pub struct SendFuture<F>(F);

// SAFETY: wasm32-unknown-unknown without `atomics` (enforced above) has
// exactly one thread, so the wrapped value can never be sent or shared
// across threads.
#[cfg(all(target_family = "wasm", target_os = "unknown"))]
unsafe impl<F> Send for SendFuture<F> {}

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
impl<F: Future> Future for SendFuture<F> {
    type Output = F::Output;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<F::Output> {
        // SAFETY: structural pinning — `0` is never moved out of the pin.
        unsafe { self.map_unchecked_mut(|s| &mut s.0) }.poll(cx)
    }
}
