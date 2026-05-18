//! rok-bot v0.2 — continuous loop (Mode 1 visible + Mode 2 virtual display).
//!
//! v0.1.x was a one-shot pipeline: boot → find RoK → capture → match →
//! click → verify → exit. v0.2 turns the `capture → match → click →
//! verify` core into a loop that runs until Ctrl-C, targeting the
//! state-neutral city↔world toggle. This is a deliberate plumbing
//! milestone — it proves the loop machinery without doing a real
//! in-game task. The one-shot pipeline is retired; `ROK_BOT_MAX_TICKS=1`
//! reproduces it exactly.
//!
//! `main::run` is now boot + delegate. Boot:
//!
//!     1. Init tracing (RUST_LOG controls level; default INFO).
//!     2. Bootstrap WindowServer registration (NSApplicationLoad) so
//!        the first SCK call doesn't trip CGS_REQUIRE_INIT.
//!     3. Preflight Screen Recording (CG-level) + the SCK shareable-
//!        content grant (per-binary, v0.1.8+).
//!     4. HARD Accessibility check. v0.1.x peeked Accessibility at boot
//!        (warn-only) because a capture-only run needed no click. The
//!        v0.2 loop ALWAYS clicks — there is no capture-only mode — so
//!        the A11 boot peek is intentionally superseded by a hard check
//!        (exit 13 if denied). Fail fast instead of looping into a
//!        guaranteed click failure.
//!     5. Find the RoK main window via ScreenCaptureKit and classify
//!        which display it lives on (`Mode::Visible` / `Mode::Virtual`
//!        — both proceed; Mode 2 is recommended). The find-window +
//!        detect-mode pair is retried (v0.2.1 L1) through a transient
//!        BetterDisplay reconnect race.
//!     6. Resolve the tick cap from `ROK_BOT_MAX_TICKS` (finite default).
//!     7. Install a `ctrlc` SIGINT handler that flips an `AtomicBool`.
//!        The loop checks it at tick boundaries, so Ctrl-C finishes the
//!        in-flight tick and exits 0 — it never hard-kills the process
//!        mid-click and strands a `LeftMouseDown`.
//!     8. Delegate to `run_loop::run_loop`.
//!
//! Per-tick capture → match → click → verify lives in `run_loop`; see
//! that module's docs for the loop's error policy and decision core.
//!
//! Exit codes: 0 clean stop (Ctrl-C or tick cap); 10-11, 13-20 the
//! v0.1.x `BotError` codes; 21 `LoopAborted` (consecutive-failure
//! budget exhausted, or the SIGINT handler failed to install).

mod capture;
mod cg_bootstrap;
mod click;
mod display;
mod error;
mod matcher;
mod permissions;
mod run_loop;
mod verify;
mod window;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tracing_subscriber::EnvFilter;

use crate::cg_bootstrap::register_with_window_server;
use crate::display::{Mode, detect_mode};
use crate::error::{BotError, Result};
use crate::permissions::{check_accessibility, check_sck_grant, check_screen_recording};
use crate::window::{RokWindow, find_rok_window, invalidate_shareable_content_cache};

fn main() {
    init_tracing();
    let exit_code = match run() {
        Ok(()) => 0,
        Err(err) => {
            let kind = error_kind(&err);
            let code = err.exit_code();
            tracing::error!(target: "rok_bot", error_kind = kind, exit_code = code, "{err}");
            code
        }
    };
    std::process::exit(exit_code);
}

const fn error_kind(err: &BotError) -> &'static str {
    match err {
        BotError::WindowNotFound => "WindowNotFound",
        BotError::WindowScreenUnresolved => "WindowScreenUnresolved",
        BotError::PermissionsMissing { .. } => "PermissionsMissing",
        BotError::CaptureFailed { .. } => "CaptureFailed",
        BotError::TargetNotFound => "TargetNotFound",
        BotError::ImageLoadFailed { .. } => "ImageLoadFailed",
        BotError::TargetTooLarge { .. } => "TargetTooLarge",
        BotError::ClickFailed { .. } => "ClickFailed",
        BotError::WindowChanged { .. } => "WindowChanged",
        BotError::ClickNotVerified { .. } => "ClickNotVerified",
        BotError::LoopAborted { .. } => "LoopAborted",
    }
}

fn run() -> Result<()> {
    tracing::info!(
        target: "rok_bot",
        "rok-bot v{} starting — v0.2 continuous loop",
        env!("CARGO_PKG_VERSION")
    );

    // T2 (v0.1.8 design): bootstrap WindowServer registration before
    // any SCK call so SCStreamConfiguration::new doesn't trip
    // CGS_REQUIRE_INIT. NSApplicationLoad is idempotent.
    register_with_window_server();

    check_screen_recording()?;
    tracing::info!(target: "rok_bot", "Screen Recording permission OK");

    // SCK-specific TCC preflight: SCK requires Screen Recording granted
    // to the rok-bot binary itself, not just its parent terminal.
    check_sck_grant()?;
    tracing::info!(target: "rok_bot", "ScreenCaptureKit shareable-content fetch OK");

    // v0.2 A11-supersede: HARD Accessibility check at boot. v0.1.x did
    // a warn-only peek here because a capture-only run needed no click;
    // the v0.2 loop ALWAYS clicks, so a denied Accessibility grant is a
    // guaranteed per-tick failure. Demand it up front (exit 13 if
    // denied) instead of looping into a wall.
    check_accessibility()?;
    tracing::info!(
        target: "rok_bot",
        "Accessibility permission OK (hard check at boot)"
    );

    let (window, mode) = resolve_window_and_mode()?;
    tracing::info!(
        target: "rok_bot",
        "found RoK window id={} pid={} frame=({:.2},{:.2} {:.2}x{:.2})",
        window.id,
        window.pid,
        window.frame.origin.x,
        window.frame.origin.y,
        window.frame.size.width,
        window.frame.size.height,
    );

    match mode {
        Mode::Visible => tracing::info!(
            target: "rok_bot",
            "Mode 1 (visible) — RoK is on the built-in display. The loop's \
             activate-and-raise + cursor moves are visible; Mode 2 (virtual \
             display) is recommended for unattended runs."
        ),
        Mode::Virtual => tracing::info!(
            target: "rok_bot",
            "Mode 2 (virtual) — RoK is on a non-built-in display. The loop \
             runs invisibly; you can keep working on the built-in display."
        ),
    }

    // D4: resolve the tick cap. Primary mode is run-until-Ctrl-C; the
    // ROK_BOT_MAX_TICKS env var (finite default) is the hard backstop.
    let raw_max_ticks = std::env::var(run_loop::MAX_TICKS_ENV).ok();
    let (max_ticks, invalid) = run_loop::parse_max_ticks(raw_max_ticks.as_deref());
    if invalid {
        tracing::warn!(
            target: "rok_bot",
            env_var = run_loop::MAX_TICKS_ENV,
            default = run_loop::DEFAULT_MAX_TICKS,
            "{} was set but is not a valid u32 — using the default cap",
            run_loop::MAX_TICKS_ENV
        );
    }
    tracing::info!(target: "rok_bot", max_ticks, "tick cap resolved");

    // D4: install the SIGINT handler. It flips `stop`, which run_loop
    // checks at each tick boundary — Ctrl-C finishes the in-flight tick
    // and exits 0 rather than hard-killing mid-click. The loop refuses
    // to start without it: a hard-kill mid-click would strand a
    // LeftMouseDown in RoK's event queue.
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_handler = Arc::clone(&stop);
    ctrlc::set_handler(move || {
        stop_for_handler.store(true, Ordering::SeqCst);
    })
    .map_err(|err| {
        tracing::error!(
            target: "rok_bot",
            error = %err,
            "failed to install the SIGINT handler — refusing to start the loop"
        );
        BotError::LoopAborted {
            reason: run_loop::REASON_SIGNAL_INSTALL_FAILED,
        }
    })?;

    run_loop::run_loop(window, max_ticks, &stop)
}

/// Outcome of [`boot_retry_decision`]: retry the find-window +
/// detect-mode pair, or give up and surface the error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BootRetry {
    Retry,
    GiveUp,
}

/// Pure: should `main::run`'s boot retry the find-window + detect-mode
/// pair (v0.2.1 L1 / D3)?
///
/// `Retry` only when attempts remain (`attempt < max_attempts`) AND the
/// error is `WindowScreenUnresolved` — the documented BetterDisplay
/// reconnect race (the virtual display blinks out, the window center
/// resolves to no display). Every other error — including
/// `WindowNotFound`, which means RoK simply is not running — gives up
/// immediately so the common boot mistake exits fast (D3).
const fn boot_retry_decision(attempt: u32, max_attempts: u32, err: &BotError) -> BootRetry {
    if attempt < max_attempts && matches!(err, BotError::WindowScreenUnresolved) {
        BootRetry::Retry
    } else {
        BootRetry::GiveUp
    }
}

/// Pure: the backoff before the next boot attempt (v0.2.1 L1). 150 ms
/// before attempt 2, 400 ms before attempt 3 — short enough that a boot
/// landing inside a BetterDisplay reconnect storm rides through in well
/// under a second, long enough to let the display arrangement settle.
const fn boot_backoff(attempt: u32) -> Duration {
    if attempt <= 1 {
        Duration::from_millis(150)
    } else {
        Duration::from_millis(400)
    }
}

/// Resolve the RoK window and its display [`Mode`], retrying the pair
/// through a BetterDisplay reconnect race (v0.2.1 L1).
///
/// `find_rok_window` and `detect_mode` are two snapshots of different
/// subsystems (SCK window enumeration, CG display arrangement). During
/// a BD reconnect / sleep-wake the virtual display can blink out
/// between them, so `detect_mode` returns `WindowScreenUnresolved` even
/// though the steady-state setup is valid. This retries the pair (3
/// attempts, 150 ms / 400 ms backoff) on that error only; the SCK
/// content cache is invalidated between attempts so each retry
/// re-fetches RoK's current frame (D8 — RoK can auto-migrate across a
/// BD reconnect).
fn resolve_window_and_mode() -> Result<(RokWindow, Mode)> {
    const MAX_ATTEMPTS: u32 = 3;
    let mut attempt: u32 = 1;
    loop {
        let resolved = find_rok_window().and_then(|w| detect_mode(&w).map(|mode| (w, mode)));
        match resolved {
            Ok(pair) => return Ok(pair),
            Err(err) => match boot_retry_decision(attempt, MAX_ATTEMPTS, &err) {
                BootRetry::GiveUp => return Err(err),
                BootRetry::Retry => {
                    let backoff = boot_backoff(attempt);
                    tracing::warn!(
                        target: "rok_bot",
                        attempt,
                        max_attempts = MAX_ATTEMPTS,
                        error = %err,
                        backoff_ms = backoff.as_millis() as u64,
                        "boot: window/display unresolved (likely a BetterDisplay \
                         reconnect race) — invalidating the SCK cache and retrying"
                    );
                    invalidate_shareable_content_cache();
                    std::thread::sleep(backoff);
                    attempt = attempt.saturating_add(1);
                }
            },
        }
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_retry_decision_retries_window_screen_unresolved_while_attempts_remain() {
        // The BD-reconnect race surfaces as WindowScreenUnresolved;
        // retry it while attempts remain (D3).
        assert_eq!(
            boot_retry_decision(1, 3, &BotError::WindowScreenUnresolved),
            BootRetry::Retry
        );
        assert_eq!(
            boot_retry_decision(2, 3, &BotError::WindowScreenUnresolved),
            BootRetry::Retry
        );
    }

    #[test]
    fn boot_retry_decision_gives_up_on_last_attempt() {
        // attempt == max_attempts: no retries left, surface the error.
        assert_eq!(
            boot_retry_decision(3, 3, &BotError::WindowScreenUnresolved),
            BootRetry::GiveUp
        );
    }

    #[test]
    fn boot_retry_decision_gives_up_on_non_retryable_errors() {
        // D3: only WindowScreenUnresolved retries. WindowNotFound (RoK
        // not running) and every other error exit fast on attempt 1.
        for err in [
            BotError::WindowNotFound,
            BotError::CaptureFailed {
                stage: "no_shareable_content",
                exit_code: None,
            },
            BotError::PermissionsMissing {
                which: "Screen Recording",
            },
        ] {
            assert_eq!(
                boot_retry_decision(1, 3, &err),
                BootRetry::GiveUp,
                "{err:?} must not retry"
            );
        }
    }

    #[test]
    fn boot_backoff_schedule() {
        // 150 ms before attempt 2, 400 ms before attempt 3.
        assert_eq!(boot_backoff(1), Duration::from_millis(150));
        assert_eq!(boot_backoff(2), Duration::from_millis(400));
    }
}
