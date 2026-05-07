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
//!          `Mode::Visible` → log "Mode 1" and exit 0 (v0.1 stops here).
//!          `Mode::Virtual` → exit `BotError::RokNotOnPrimary` (Mode 2 lives in v0.2).
//!     6. Any error → log + exit with the variant's exit code.
//!
//! The actual gameplay automation (capture, match, click, verify) is the
//! next milestone after this binary proves it can find RoK and detect mode.

mod display;
mod error;
mod permissions;
mod window;

use tracing_subscriber::EnvFilter;

use crate::display::{Mode, detect_mode};
use crate::error::{BotError, Result};
use crate::permissions::check_screen_recording;
use crate::window::find_rok_window;

fn main() {
    init_tracing();
    let exit_code = match run() {
        Ok(()) => 0,
        Err(err) => {
            tracing::error!(target: "rok_bot", "{err}");
            err.exit_code()
        }
    };
    std::process::exit(exit_code);
}

fn run() -> Result<()> {
    tracing::info!(target: "rok_bot", "rok-bot v{} starting", env!("CARGO_PKG_VERSION"));

    check_screen_recording()?;
    tracing::info!(target: "rok_bot", "Screen Recording permission OK");

    let window = find_rok_window()?;
    tracing::info!(
        target: "rok_bot",
        "found RoK window id={} pid={} frame=({:.0},{:.0} {:.0}x{:.0})",
        window.id,
        window.pid,
        window.frame.origin.x,
        window.frame.origin.y,
        window.frame.size.width,
        window.frame.size.height,
    );

    let mode = detect_mode(&window)?;
    match mode {
        Mode::Visible => {
            tracing::info!(
                target: "rok_bot",
                "Mode 1 (visible) — RoK is on the primary display. \
                 v0.1 hello-world will go here in the next milestone."
            );
            Ok(())
        }
        Mode::Virtual => Err(BotError::RokNotOnPrimary),
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
