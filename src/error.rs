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

    /// Best-match score below `matcher::MATCH_THRESHOLD`. The matcher already
    /// logged the diagnostic numbers (`best_score`, `threshold`) at warn level
    /// before this variant fires; this is the structured exit-code carrier.
    #[error("target not found in capture (best match below confidence threshold)")]
    TargetNotFound,

    /// PNG decode or open failed for the haystack. The underlying
    /// `image::ImageError` is logged at warn before mapping to this, mirroring
    /// `capture_with_bin`'s `io::Error` handling. `which` distinguishes
    /// haystack vs. needle in the log line; v0.1.x only fires for "haystack"
    /// because the needle is `include_bytes!`-embedded.
    #[error("failed to load {which} image — see prior warn log for the underlying error")]
    ImageLoadFailed { which: &'static str },

    /// Needle dimensions are not strictly less than haystack dimensions.
    /// `imageproc::template_matching::match_template_parallel` panics when
    /// `template.{w,h} >= image.{w,h}`; this guard converts that panic into
    /// a typed exit. `>=`, not `>` — equal-size also panics per imageproc's
    /// docstring. Tuple order: `(width, height)`.
    #[error(
        "target image is too large: needle {}x{} >= haystack {}x{} \
         (needle dims must be strictly smaller)",
        needle.0, needle.1, haystack.0, haystack.1
    )]
    TargetTooLarge {
        needle: (u32, u32),
        haystack: (u32, u32),
    },
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
            Self::TargetNotFound => 15,
            Self::ImageLoadFailed { .. } => 16,
            Self::TargetTooLarge { .. } => 17,
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
            BotError::TargetNotFound.exit_code(),
            BotError::ImageLoadFailed { which: "haystack" }.exit_code(),
            BotError::TargetTooLarge {
                needle: (50, 50),
                haystack: (40, 40),
            }
            .exit_code(),
        ];
        // HashSet invariant: dedup() only collapses adjacent equals, which
        // would silently pass for a non-adjacent collision if the sort step
        // were ever removed. HashSet captures the real intent: every code is
        // distinct from every other.
        let unique: std::collections::HashSet<i32> = codes.iter().copied().collect();
        assert_eq!(
            unique.len(),
            codes.len(),
            "exit codes must be unique: {codes:?}"
        );
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
        assert_eq!(BotError::TargetNotFound.exit_code(), 15);
        assert_eq!(
            BotError::ImageLoadFailed { which: "haystack" }.exit_code(),
            16
        );
        assert_eq!(
            BotError::TargetTooLarge {
                needle: (100, 100),
                haystack: (80, 40),
            }
            .exit_code(),
            17
        );
    }

    #[test]
    fn target_not_found_message_mentions_threshold() {
        // Operator's first instinct on TargetNotFound is "did the matcher
        // think there's a near-miss?" The pre-error warn log carries the
        // numeric score; this Display string just needs to acknowledge the
        // threshold concept so the operator knows where to look.
        let msg = BotError::TargetNotFound.to_string();
        assert!(
            msg.to_lowercase().contains("threshold"),
            "TargetNotFound msg should reference the threshold: {msg}"
        );
    }

    #[test]
    fn image_load_failed_message_includes_which() {
        // The `which` field tags whether haystack vs. needle failed to decode.
        // Display must surface it so operators don't have to cross-ref the
        // pre-error warn log to know which file is broken.
        let err = BotError::ImageLoadFailed { which: "haystack" };
        let msg = err.to_string();
        assert!(
            msg.contains("haystack"),
            "ImageLoadFailed Display must include the `which` tag: {msg}"
        );
    }

    #[test]
    fn target_too_large_message_includes_dims() {
        // Operators debugging an oversized-needle rebuild need both pairs of
        // dimensions in the failure message — knowing only the needle size
        // doesn't tell them whether the haystack shrank or the needle grew.
        let err = BotError::TargetTooLarge {
            needle: (200, 100),
            haystack: (150, 80),
        };
        let msg = err.to_string();
        assert!(msg.contains("200"), "needle width missing: {msg}");
        assert!(msg.contains("100"), "needle height missing: {msg}");
        assert!(msg.contains("150"), "haystack width missing: {msg}");
        assert!(msg.contains("80"), "haystack height missing: {msg}");
    }

    #[test]
    fn capture_failed_message_mentions_screen_recording() {
        // Both Some(_) (non-zero exit) and None (spawn failed) variants should
        // surface the same root-cause hint; the user's first debugging step is
        // the same regardless of which path failed.
        for err in [
            BotError::CaptureFailed { exit_code: Some(1) },
            BotError::CaptureFailed { exit_code: None },
        ] {
            let msg = err.to_string();
            assert!(
                msg.contains("Screen Recording"),
                "capture failure should hint at the most likely root cause: {msg}"
            );
        }
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
