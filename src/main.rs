//! rok-bot v0.1 — Mode 1 (visible) only.
//!
//! Boot sequence:
//!     1. Init tracing (RUST_LOG controls level; default INFO).
//!     2. Preflight: Screen Recording permission. Without it, CGWindowList
//!        strips window titles and our owner+title filter silently misses
//!        RoK — fail fast with `PermissionsMissing` instead.
//!     3. Peek Accessibility (warn-only at boot — design A11). The bot can
//!        produce useful capture+match output without click; the hard check
//!        fires later only if a click is actually about to happen.
//!     4. Find the RoK main window via Core Graphics.
//!     5. Detect which display it lives on.
//!     6. Branch:
//!          `Mode::Visible` → capture window → template-match → validate
//!                            capture/needle dims (A16 zero-dim hard-fail)
//!                            → hard Accessibility check (A11; exit 13 if
//!                            denied) → translate match to screen coords
//!                            (`matcher::screen_point`) → synthesize click
//!                            (`click::click_at`) → exit 0.
//!          `Mode::Virtual` → exit `BotError::RokNotOnPrimary` (Mode 2 lives in v0.2).
//!     7. Any error → log + exit with the variant's exit code.
//!
//! v0.1.3 (this milestone) adds the click step. Verified end-to-end via
//! the P3 spike (`spikes/p3-spike/`) before integration. Next:
//! v0.1.4 after-state verification (capture diff to confirm click landed).

mod capture;
mod click;
mod display;
mod error;
mod matcher;
mod permissions;
mod window;

use std::path::PathBuf;

use tracing_subscriber::EnvFilter;

use crate::capture::capture_window;
use crate::click::click_at;
use crate::display::{Mode, detect_mode, mode_to_result};
use crate::error::{BotError, Result};
use crate::matcher::{find_target, screen_point, validate_match_dims};
use crate::permissions::{
    ACCESSIBILITY, check_accessibility, check_screen_recording, peek_accessibility,
};
use core_graphics::display::CGPoint;

use crate::window::{find_rok_window, validate_at_click_site};

/// Where the window capture is written. Relative to the cwd —
/// `cargo run` from the repo root puts it at `./rok-capture.png`.
/// `.gitignore` excludes it. `capture_window` always overwrites, so
/// each run sees a fresh capture (no stale-state risk between runs).
/// v0.2+ may move this under a proper user cache dir once we capture
/// more than once per run.
const CAPTURE_OUTPUT_PATH: &str = "rok-capture.png";

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
        BotError::RokNotOnPrimary => "RokNotOnPrimary",
        BotError::PermissionsMissing { .. } => "PermissionsMissing",
        BotError::CaptureFailed { .. } => "CaptureFailed",
        BotError::TargetNotFound => "TargetNotFound",
        BotError::ImageLoadFailed { .. } => "ImageLoadFailed",
        BotError::TargetTooLarge { .. } => "TargetTooLarge",
        BotError::ClickFailed { .. } => "ClickFailed",
        BotError::WindowChanged { .. } => "WindowChanged",
    }
}

fn run() -> Result<()> {
    tracing::info!(target: "rok_bot", "rok-bot v{} starting", env!("CARGO_PKG_VERSION"));

    check_screen_recording()?;
    tracing::info!(target: "rok_bot", "Screen Recording permission OK");

    // Design A11 boot peek: warn-only at boot. The bot can produce useful
    // capture+match output without click; the hard check fires at the click
    // site only if a match is actually found. This avoids hitting the user
    // with an Accessibility prompt during runs that would have just printed
    // a no-match warning anyway.
    if peek_accessibility() {
        tracing::info!(target: "rok_bot", "Accessibility permission OK (peek)");
    } else {
        tracing::warn!(
            target: "rok_bot",
            permission = ACCESSIBILITY,
            "Accessibility not yet granted — capture + match will run but a successful \
             match will trigger a hard check that exits 13 if still denied. Grant in \
             System Settings → Privacy & Security → Accessibility before the next match."
        );
    }

    let window = find_rok_window()?;
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

    let mode = detect_mode(&window)?;
    mode_to_result(mode)?;
    debug_assert_eq!(
        mode,
        Mode::Visible,
        "mode_to_result returned Ok only for Visible"
    );
    tracing::info!(
        target: "rok_bot",
        "Mode 1 (visible) — RoK is on the built-in display."
    );

    let capture_path = PathBuf::from(CAPTURE_OUTPUT_PATH);
    capture_window(window.id, &capture_path)?;
    tracing::info!(
        target: "rok_bot",
        path = %capture_path.display(),
        "captured RoK window"
    );

    // Locate the embedded target needle inside the capture. None here means
    // "best score below MATCH_THRESHOLD"; the matcher already logged the
    // diagnostic numbers at warn before returning. We translate None to a
    // typed BotError so the exit-code contract stays uniform — shell users
    // distinguish "target absent" (15) from "image broken" (16) from
    // "needle too big" (17) without parsing log lines.
    let m = find_target(&capture_path)?.ok_or(BotError::TargetNotFound)?;

    // Design A16 zero-dim hard-fail: catch malformed Match at the boundary
    // before screen_point produces NaN/Inf coords. The check lives in
    // matcher::validate_match_dims so each branch is unit-testable.
    validate_match_dims(&m)?;

    // Design A11 hard check: a match exists, so we are about to click. NOW
    // demand Accessibility — the prompt (if denied) is async, so first-run
    // UX is "exit 13, grant in Settings, re-run." See permissions.rs docs
    // for why a blocking prompt is intentionally not implemented.
    check_accessibility()?;
    tracing::info!(target: "rok_bot", "Accessibility permission OK (hard check)");

    let (sx, sy) = screen_point(&m, &window.frame);
    tracing::info!(
        target: "rok_bot",
        match_x = m.x,
        match_y = m.y,
        score = m.score,
        screen_x = sx,
        screen_y = sy,
        "translated match to screen point"
    );

    // TOCTOU close: re-validate the RoK window's state immediately before
    // posting the click. Three checks, each mapping to a distinct
    // BotError::WindowChanged reason: WID still exists, frame within
    // tolerance, RoK is topmost at the click coords. Closes the window
    // between find_rok_window() (top of run) and click_at() — during
    // capture (~50-100ms), find_target (could be seconds), and the AX
    // check, RoK could move, resize, close, lose focus to a system
    // overlay, or be occluded by a notification banner. Without this
    // gate, a synthetic AX-privileged click could land on the wrong
    // window. Both Claude and Codex adversarial reviews flagged this
    // path as the top exploitable issue in v0.1.3.
    validate_at_click_site(&window, CGPoint::new(sx, sy))?;
    tracing::info!(target: "rok_bot", "click-site re-validation OK; clicking");

    click_at(sx, sy)?;
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}
