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

/// Pure: turn a granted/denied bool into the preflight result. Lets us
/// unit-test both branches without needing two Macs with different TCC state.
const fn check_inner(granted: bool) -> Result<()> {
    if granted {
        Ok(())
    } else {
        Err(BotError::PermissionsMissing {
            which: "Screen Recording",
        })
    }
}

/// Live wrapper. Calls Apple's `CGPreflightScreenCaptureAccess` via the safe
/// `core-graphics` wrapper (no `unsafe` in our code) and returns the result.
/// Does NOT trigger the system permission prompt — purely a check.
pub fn check_screen_recording() -> Result<()> {
    check_inner(ScreenCaptureAccess.preflight())
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
                which: "Screen Recording"
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
