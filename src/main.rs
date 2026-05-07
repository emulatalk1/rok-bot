//! rok-bot v0.1 — Mode 1 (visible) only.
//!
//! Boot sequence:
//!     1. Init tracing (RUST_LOG controls level; default INFO).
//!     2. Preflight: Screen Recording permission. Without it, CGWindowList
//!        strips window titles and our owner+title filter silently misses
//!        RoK — fail fast with `PermissionsMissing` instead.
//!     3. Find the RoK main window via Core Graphics.
//!     4. Detect which display it lives on.
//!     5. Branch:
//!          `Mode::Visible` → capture window screenshot to `rok-capture.png`,
//!                            template-match the embedded target inside the
//!                            capture, log coords + score, exit 0.
//!          `Mode::Virtual` → exit `BotError::RokNotOnPrimary` (Mode 2 lives in v0.2).
//!     6. Any error → log + exit with the variant's exit code.
//!
//! v0.1.2 (this milestone) adds template matching after capture. Next
//! sub-milestones: v0.1.3 click via CGEvent.post + Accessibility preflight;
//! v0.1.4 after-state verification.

mod capture;
mod display;
mod error;
mod matcher;
mod permissions;
mod window;

use std::path::PathBuf;

use tracing_subscriber::EnvFilter;

use crate::capture::capture_window;
use crate::display::{Mode, detect_mode, mode_to_result};
use crate::error::{BotError, Result};
use crate::matcher::find_target;
use crate::permissions::check_screen_recording;
use crate::window::find_rok_window;

/// Where v0.1.1 writes the window capture. Relative to the cwd —
/// `cargo run` from the repo root puts it at `./rok-capture.png`.
/// `.gitignore` excludes it. v0.2+ may move this under a proper user
/// cache dir once we capture more than once per run.
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
    }
}

fn run() -> Result<()> {
    tracing::info!(target: "rok_bot", "rok-bot v{} starting", env!("CARGO_PKG_VERSION"));

    check_screen_recording()?;
    tracing::info!(target: "rok_bot", "Screen Recording permission OK");

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

    // v0.1.2: locate the embedded target needle inside the capture. None
    // here means "best score below MATCH_THRESHOLD"; the matcher already
    // logged the diagnostic numbers at warn before returning. We translate
    // None to a typed BotError so the exit-code contract stays uniform —
    // shell users distinguish "target absent" (15) from "image broken" (16)
    // from "needle too big" (17) without parsing log lines.
    find_target(&capture_path)?.ok_or(BotError::TargetNotFound)?;
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
