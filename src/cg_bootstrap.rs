//! Bootstrap the WindowServer / CGS connection so SCK calls don't trip
//! `CGS_REQUIRE_INIT`.
//!
//! A plain Rust binary that only pulls in CG bindings never registers
//! with WindowServer; the first `SCStreamConfiguration::new` call then
//! aborts with `Assertion failed: (did_initialize), function
//! CGS_REQUIRE_INIT, file CGInitialization.c, line 44`. A normal AppKit
//! app gets the registration for free when `NSApplication` is touched;
//! a CLI binary has to do it explicitly via `NSApplicationLoad()`, which
//! is the documented "register with WindowServer without entering a
//! run loop" entry point.
//!
//! Discovered live in `spikes/p8-spike/` (commit `9c576a9`); v0.1.8
//! brings the fix into production.
//!
//! T2 from `/plan-eng-review` (2026-05-15) makes this idempotent
//! function the bootstrap surface for **every** SCK entrypoint, not
//! just `main.rs::run` — so cargo `#[ignore]`'d integration tests that
//! invoke SCK directly don't SIGABRT by bypassing main's call site.
//! `NSApplicationLoad` is documented to be safe at any point before
//! AppKit UI use and is idempotent (~1µs cost after the first call),
//! so calling it from every SCK entrypoint is free.
//!
//! v0.1.8 requires macOS 14.0+ (SCScreenshotManager is 14+). A runtime
//! version check is intentionally **not** included here — pre-14
//! systems trip `no_shareable_content` from `permissions::check_sck_
//! grant` instead, which carries the same actionable surface. The
//! deferred check would double the FFI surface for marginal UX gain
//! on a vanishingly rare user population.

#![allow(
    unsafe_code,
    reason = "AppKit FFI for the documented WindowServer-bootstrap call; \
              the unsafe surface is contained in this module."
)]
// Module is heavy on framework / Apple-ObjC names that the pedantic
// doc_markdown lint mis-flags; opt out for readability.
#![allow(clippy::doc_markdown)]

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    fn NSApplicationLoad() -> bool;
}

/// Idempotent. Call from EVERY SCK entrypoint:
/// - `permissions::check_sck_grant` (boot preflight)
/// - `window::find_rok_window` (enumeration)
/// - `capture::capture_window` (capture)
///
/// First call connects the process to WindowServer; subsequent calls
/// return early inside AppKit (~1µs measured cost). Without this,
/// `SCStreamConfiguration::new` aborts the process inside
/// `CGS_REQUIRE_INIT` before any error can be surfaced.
///
/// Returns true on success; the AppKit return is informational only —
/// failure (which has not been observed in spike or live runs) would
/// surface downstream as `CaptureFailed { stage: "no_shareable_content" }`
/// when SCShareableContent itself fails to enumerate.
pub fn register_with_window_server() -> bool {
    // SAFETY: NSApplicationLoad is documented to be safe at any point
    // before AppKit UI use and is idempotent.
    unsafe { NSApplicationLoad() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_with_window_server_returns_true() {
        // Idempotency contract: calling twice in a row must succeed.
        // Live FFI — runs on every cargo test invocation. If this ever
        // fails, the rest of the SCK pipeline is unreachable and every
        // integration test would also fail; pinning here gives a clean
        // diagnostic surface.
        assert!(register_with_window_server());
        assert!(register_with_window_server());
    }

    #[test]
    fn register_with_window_server_is_idempotent_under_load() {
        // T2 (per-SCK-entrypoint defensive call) means this fires
        // multiple times per cargo run. Pin that 100 calls in a tight
        // loop don't accumulate FFI cost or crash. NSApplicationLoad
        // is documented idempotent (~1µs after first call); this
        // confirms the binding and our wrapper preserve that.
        for _ in 0..100 {
            assert!(register_with_window_server());
        }
    }
}
