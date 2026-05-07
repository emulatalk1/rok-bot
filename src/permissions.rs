//! macOS TCC permission preflight.
//!
//! Without Screen Recording granted to the terminal app, `CGWindowListCopyWindowInfo`
//! still returns RoK's windows but with `kCGWindowName` stripped to nil. Our
//! owner+title filter would then silently miss the main window and return
//! `WindowNotFound`, leaving the user staring at a misleading "is the game
//! running?" error while the game is visibly running. The preflight catches
//! this up front and returns `PermissionsMissing { which: "Screen Recording" }`
//! instead.
//!
//! v0.1 checks **Screen Recording only.** Accessibility (required for
//! `CGEvent.post` synthetic input) lands when the hello-world click milestone
//! adds the first synthetic event — see TODOS.md.

use core_graphics::access::ScreenCaptureAccess;

use crate::error::{BotError, Result};

/// Canonical label for the Screen Recording permission. Single source of truth
/// so the error variant, log lines, and tests can reference one constant
/// instead of literal "Screen Recording" strings scattered across files.
pub const SCREEN_RECORDING: &str = "Screen Recording";

/// Pure: turn a granted/denied bool into the preflight result. Lets us
/// unit-test both branches without needing two Macs with different TCC state.
const fn check_inner(granted: bool) -> Result<()> {
    if granted {
        Ok(())
    } else {
        Err(BotError::PermissionsMissing {
            which: SCREEN_RECORDING,
        })
    }
}

/// Live wrapper with first-run fallback.
///
/// Flow:
///   1. `preflight()` — purely checks current grant state, no prompt.
///   2. If denied AND we're a fresh install (terminal app not yet in
///      System Settings → Privacy → Screen Recording), `request()` shows
///      the system prompt and registers the app there. Without this
///      step, a brand-new Mac hits a permanent `exit 13` loop with no UI
///      affordance to grant from. (Codex finding #1.)
///   3. `request()` returns the post-prompt state, so its boolean is
///      authoritative — we don't need a third preflight.
pub fn check_screen_recording() -> Result<()> {
    let access = ScreenCaptureAccess;
    if access.preflight() {
        return Ok(());
    }
    if access.request() {
        return Ok(());
    }
    check_inner(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn granted_returns_ok() {
        assert!(check_inner(true).is_ok());
    }

    #[test]
    fn denied_returns_permissions_missing_with_screen_recording_label() {
        let err = check_inner(false).unwrap_err();
        assert_eq!(
            err,
            BotError::PermissionsMissing {
                which: SCREEN_RECORDING
            }
        );
    }

    #[test]
    fn denied_message_points_at_system_settings() {
        let err = check_inner(false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Screen Recording"), "msg: {msg}");
        assert!(
            msg.contains("System Settings"),
            "user-facing msg should name the macOS settings panel: {msg}"
        );
    }

    #[test]
    fn denied_uses_permissions_missing_exit_code() {
        let err = check_inner(false).unwrap_err();
        assert_eq!(err.exit_code(), 13, "PermissionsMissing exit code is 13");
    }
}
