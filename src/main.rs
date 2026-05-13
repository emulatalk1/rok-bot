//! rok-bot v0.1.6 — Mode 1 (visible) + Mode 2 (virtual display) pipeline.
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
//!     5. Detect which display it lives on. Both `Mode::Visible` (built-in)
//!        and `Mode::Virtual` (BetterDisplay / external) proceed; the
//!        v0.1.5 exit-12 gate (`mode_to_result`) was deleted in v0.1.6
//!        once stealth HID + activation made Mode 2 the structural fix for
//!        Catalyst Bridge's auto-raise behavior.
//!     6. Capture → template-match → validate capture/needle dims (A16
//!        zero-dim hard-fail) → click-site TOCTOU close (4-check) → hard
//!        Accessibility check (A11; exit 13 if denied) → translate match
//!        to screen coords (`matcher::screen_point`) → synthesize click
//!        (`click::click_at` — activate + stealth + HID tap).
//!     7. Post-click verify: sleep, re-validate window present (3-check),
//!        re-capture, pixel-diff vs pre. Exit 20 if pre/post identical
//!        within threshold (RoK did not visibly react).
//!     8. Any error → log + exit with the variant's exit code.
//!
//! Why both modes proceed in v0.1.6: Catalyst Bridge auto-raises RoK on
//! any synthetic `UITouch` that wakes its event pipeline (verified across
//! 6 click-delivery paths during the 2026-05-11 ship-block research; see
//! TODOS.md P0 entry). On Mode 1 that defeats "user does other things
//! while bot runs"; on Mode 2 the raise is invisible (RoK lives on a
//! display nothing observes), so the constraint evaporates. The v0.1.5
//! Mode 1 lead is preserved at tag `v0.1.5-rc, ship-blocked` for project
//! record; v0.1.6 builds Mode 2 on top.

mod capture;
mod click;
mod display;
mod error;
mod matcher;
mod permissions;
mod verify;
mod window;

use std::path::PathBuf;

use tracing_subscriber::EnvFilter;

use crate::capture::capture_window;
use crate::click::click_at;
use crate::display::{Mode, detect_mode};
use crate::error::{BotError, Result};
use crate::matcher::{find_target, screen_point, validate_match_dims};
use crate::permissions::{
    ACCESSIBILITY, check_accessibility, check_screen_recording, peek_accessibility,
};
use core_graphics::display::CGPoint;

use crate::window::{find_rok_window, validate_at_click_site, validate_window_present};

/// Pre-click capture path. Written before `find_target` + `click_at`; the
/// matcher reads it back to locate the target needle. Relative to cwd —
/// `cargo run` from the repo root puts it at `./rok-capture-pre.png`.
/// `.gitignore` excludes the `rok-capture-*.png` wildcard.
///
/// `capture_window` always overwrites, so each run sees a fresh capture
/// (no stale-state risk between runs).
///
/// v0.1.3 used a single `rok-capture.png`; v0.1.4 splits into pre/post
/// because the verify gate needs both captures simultaneously for the
/// pixel-diff.
const CAPTURE_PRE_PATH: &str = "rok-capture-pre.png";

/// Post-click capture path. Written after the verify-delay sleep and
/// post-click TOCTOU re-check, then compared against the pre-click
/// capture by `verify::after_state`. Sibling of [`CAPTURE_PRE_PATH`];
/// both excluded by the `rok-capture-*.png` `.gitignore` wildcard.
const CAPTURE_POST_PATH: &str = "rok-capture-post.png";

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
    // v0.1.6: both modes proceed. Mode 1 = built-in display, Mode 2 =
    // virtual / external display. The mechanical click-delivery path is
    // identical (stealth HID + activation); the user-observable
    // difference is whether RoK's surface is on a display the user can
    // see. Mode 2 is the structural fix for Catalyst Bridge's auto-raise
    // (see crate docs).
    match mode {
        Mode::Visible => tracing::info!(
            target: "rok_bot",
            "Mode 1 (visible) — RoK is on the built-in display."
        ),
        Mode::Virtual => tracing::info!(
            target: "rok_bot",
            "Mode 2 (virtual) — RoK is on a non-built-in display \
             (BetterDisplay virtual, external, or Sidecar). \
             Activate-and-raise is invisible to the user; \
             pipeline proceeds."
        ),
    }

    let pre_capture_path = PathBuf::from(CAPTURE_PRE_PATH);
    capture_window(window.id, &pre_capture_path)?;
    tracing::info!(
        target: "rok_bot",
        path = %pre_capture_path.display(),
        "captured RoK window (pre-click)"
    );

    // Locate the embedded target needle inside the capture. None here means
    // "best score below MATCH_THRESHOLD"; the matcher already logged the
    // diagnostic numbers at warn before returning. We translate None to a
    // typed BotError so the exit-code contract stays uniform — shell users
    // distinguish "target absent" (15) from "image broken" (16) from
    // "needle too big" (17) without parsing log lines.
    let m = find_target(&pre_capture_path)?.ok_or(BotError::TargetNotFound)?;

    // Design A16 zero-dim hard-fail: catch malformed Match at the boundary
    // before screen_point produces NaN/Inf coords. The check lives in
    // matcher::validate_match_dims so each branch is unit-testable.
    validate_match_dims(&m)?;

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
    // committing to a click. v0.1.5 runs FOUR checks, each mapping to a
    // distinct BotError::WindowChanged reason (exit 19):
    //   - REASON_WID_GONE        — (WID, PID) no longer in `all` list.
    //   - REASON_NOT_VISIBLE     — RoK is hidden (Space switched, Dock,
    //                              fullscreen-from-another-app). AX press
    //                              would return kAXErrorFailure here.
    //   - REASON_FRAME_MOVED     — window dragged/resized since discovery.
    //   - REASON_POINT_OUTSIDE_FRAME — click point outside window bounds.
    //
    // Runs BEFORE check_accessibility (codex /plan-eng-review #8) so a
    // hidden-Space exit-19 doesn't trigger the AX TCC prompt for a click
    // that would have been refused anyway. The operator sees a clean
    // error message instead of being asked to grant permission they
    // don't need to spend.
    validate_at_click_site(&window, CGPoint::new(sx, sy))?;
    tracing::info!(target: "rok_bot", "click-site re-validation OK");

    // Design A11 hard check: a match exists AND the window is reachable,
    // so we are about to click. NOW demand Accessibility — the prompt
    // (if denied) is async, so first-run UX is "exit 13, grant in
    // Settings, re-run." See permissions.rs docs for why a blocking
    // prompt is intentionally not implemented.
    check_accessibility()?;
    tracing::info!(target: "rok_bot", "Accessibility permission OK (hard check); clicking");

    click_at(&window, CGPoint::new(sx, sy))?;

    // v0.1.4 after-state verify. Sleep long enough to let RoK render the
    // UI response, re-validate the window is still present (subset of
    // the pre-click TOCTOU check — drops the topmost invariant because
    // a successful click may legitimately spawn a modal/popup that's
    // now topmost; see window.rs::validate_present_inner for the
    // rationale), capture again, and diff against the pre capture.
    //
    // Exit codes added by this block:
    //   19 WindowChanged{window_id_gone|frame_moved} — RoK closed or
    //      moved between click and post-capture.
    //   14 CaptureFailed — screencapture itself failed (re-uses the
    //      existing capture-pipeline gates: 0-byte output, nonzero
    //      exit, symlink refuse).
    //   20 ClickNotVerified{screen_unchanged} — pre/post pixel-diff
    //      below threshold; the click landed but RoK did not visibly
    //      react in pixel space. See error.rs::ClickNotVerified docs
    //      for the operator's diagnostic checklist.
    verify::sleep_verify_delay();
    validate_window_present(&window)?;
    let post_capture_path = PathBuf::from(CAPTURE_POST_PATH);
    capture_window(window.id, &post_capture_path)?;
    tracing::info!(
        target: "rok_bot",
        path = %post_capture_path.display(),
        "captured RoK window (post-click)"
    );
    verify::after_state(&pre_capture_path, &post_capture_path, &m)?;
    tracing::info!(target: "rok_bot", "after-state verify passed; click confirmed");
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
