use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BotError {
    #[error("RoK window not found — is the game running?")]
    WindowNotFound,

    #[error("RoK window center is not on any online display")]
    WindowScreenUnresolved,

    #[error(
        "RoK is not on the primary display. v0.1 supports Mode 1 (visible) only — \
         drag RoK to your built-in display, then re-run. \
         Mode 2 (background on a virtual display) is planned for v0.2."
    )]
    RokNotOnPrimary,

    /// Returned by `permissions::check_screen_recording` when
    /// `CGPreflightScreenCaptureAccess` reports the running app lacks the
    /// permission. v0.1 only checks Screen Recording; Accessibility is added
    /// when the synthetic-input milestone lands `CGEvent.post`.
    #[error(
        "Missing macOS permission: {which}. Grant it to your terminal app in \
         System Settings → Privacy & Security → {which}, then re-run."
    )]
    PermissionsMissing { which: &'static str },

    /// `screencapture` subprocess returned non-zero or failed to spawn.
    /// `exit_code` is `None` if the process couldn't be spawned (e.g.,
    /// `/usr/sbin/screencapture` missing) or was killed by signal.
    #[error(
        "screencapture failed (exit code: {exit_code:?}). Ensure Screen Recording is \
         granted to your terminal app and the RoK window ID is still valid."
    )]
    CaptureFailed { exit_code: Option<i32> },
}

impl BotError {
    /// Stable, per-variant exit code for `main`. Lets shell users branch on outcome.
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::WindowNotFound => 10,
            Self::WindowScreenUnresolved => 11,
            Self::RokNotOnPrimary => 12,
            Self::PermissionsMissing { .. } => 13,
            Self::CaptureFailed { .. } => 14,
        }
    }
}

pub type Result<T> = std::result::Result<T, BotError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_stable_and_unique() {
        let codes = [
            BotError::WindowNotFound.exit_code(),
            BotError::WindowScreenUnresolved.exit_code(),
            BotError::RokNotOnPrimary.exit_code(),
            BotError::PermissionsMissing {
                which: "Screen Recording",
            }
            .exit_code(),
            BotError::CaptureFailed { exit_code: Some(1) }.exit_code(),
        ];
        let mut sorted: Vec<i32> = codes.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), codes.len(), "exit codes must be unique");
    }

    /// Pin the exact numeric exit codes — shell users branch on these values,
    /// so renumbering a variant is a contract break and must fail this test.
    #[test]
    fn exit_codes_have_specific_stable_values() {
        assert_eq!(BotError::WindowNotFound.exit_code(), 10);
        assert_eq!(BotError::WindowScreenUnresolved.exit_code(), 11);
        assert_eq!(BotError::RokNotOnPrimary.exit_code(), 12);
        assert_eq!(
            BotError::PermissionsMissing {
                which: "Screen Recording"
            }
            .exit_code(),
            13
        );
        assert_eq!(
            BotError::CaptureFailed { exit_code: Some(1) }.exit_code(),
            14
        );
        assert_eq!(BotError::CaptureFailed { exit_code: None }.exit_code(), 14);
    }

    #[test]
    fn capture_failed_message_mentions_screen_recording() {
        let msg = BotError::CaptureFailed { exit_code: Some(1) }.to_string();
        assert!(
            msg.contains("Screen Recording"),
            "capture failure should hint at the most likely root cause: {msg}"
        );
    }

    #[test]
    fn window_not_found_message_hints_at_running_game() {
        let msg = BotError::WindowNotFound.to_string();
        assert!(msg.contains("game running"), "msg: {msg}");
    }

    #[test]
    fn window_screen_unresolved_message_mentions_display() {
        let msg = BotError::WindowScreenUnresolved.to_string();
        assert!(msg.to_lowercase().contains("display"), "msg: {msg}");
    }

    #[test]
    fn permissions_missing_message_includes_which() {
        let err = BotError::PermissionsMissing {
            which: "Screen Recording",
        };
        let msg = err.to_string();
        assert!(msg.contains("Screen Recording"), "msg: {msg}");
    }

    #[test]
    fn rok_not_on_primary_mentions_v0_2() {
        let msg = BotError::RokNotOnPrimary.to_string();
        assert!(msg.contains("v0.2"), "msg should reference v0.2: {msg}");
    }
}
