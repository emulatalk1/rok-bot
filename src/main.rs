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
//!                            log "Mode 1" and exit 0 (v0.1.1 stops here).
//!          `Mode::Virtual` → exit `BotError::RokNotOnPrimary` (Mode 2 lives in v0.2).
//!     6. Any error → log + exit with the variant's exit code.
//!
//! v0.1.1 (this milestone) adds the capture step. Next sub-milestones:
//! v0.1.2 template-match a target image; v0.1.3 click via CGEvent.post +
//! Accessibility preflight; v0.1.4 after-state verification.

mod capture;
mod display;
mod error;
mod permissions;
mod window;

use std::path::PathBuf;

use tracing_subscriber::EnvFilter;

use crate::capture::capture_window;
use crate::display::{Mode, detect_mode, mode_to_result};
use crate::error::{BotError, Result};
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
        "captured RoK window — v0.1.1 hello-world step 1 of 4 done. \
         Next: v0.1.2 template match against this PNG."
    );
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
