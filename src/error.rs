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

    /// Needle is strictly larger than haystack in at least one dimension.
    /// `imageproc::template_matching::match_template_parallel` panics when
    /// `template.{w,h} > image.{w,h}`; this guard converts that panic into a
    /// typed exit. Equal dims are accepted (verified empirically against
    /// imageproc 0.26.2's `CrossCorrelationNormalized` — they produce a
    /// degenerate 1-wide or 1-tall heatmap rather than panicking, contrary
    /// to imageproc's docstring claim of "strictly less than"). Tuple
    /// order: `(width, height)`.
    #[error(
        "target image is too large: needle {}x{} larger than haystack {}x{} \
         in at least one dimension (needle dims must be <= haystack dims)",
        needle.0, needle.1, haystack.0, haystack.1
    )]
    TargetTooLarge {
        needle: (u32, u32),
        haystack: (u32, u32),
    },

    /// The synthetic click did not reach the target window. v0.1.5 routes
    /// click delivery through the macOS Accessibility (AX) API
    /// (`AXUIElementPerformAction(kAXPressAction)`), which has a richer
    /// failure surface than v0.1.3-4's `CGEventPost`: AX calls return
    /// typed `AXError` codes, so unlike `CGEvent::post` (which returns
    /// `()`) we can distinguish app-resolve failure, element-resolve
    /// failure, press-action failure, and RPC timeout. Each maps to a
    /// distinct `reason` string declared in `src/ax.rs` (see
    /// `REASON_AX_APP_RESOLVE_FAILED`, `REASON_AX_ELEMENT_RESOLVE_FAILED`,
    /// `REASON_AX_PRESS_FAILED`, `REASON_AX_TIMEOUT`).
    ///
    /// Verifying that a successful AX press caused a visible state
    /// change is `ClickNotVerified`'s job — a successful AX press tells
    /// us the action was accepted by the target's accessibility tree,
    /// not that the underlying control actually responded.
    #[error(
        "synthetic click could not be delivered (reason: {reason}). \
         The click did not reach the target window. The macOS Accessibility \
         layer refused or failed the request before delivery — check the \
         preceding warn log for the underlying AXError code."
    )]
    ClickFailed { reason: &'static str },

    /// The RoK window's state changed between discovery (top of `run()`)
    /// and the click site, OR was already in an unreachable state at
    /// boot, in a way that would let a synthetic AX-privileged click
    /// land on the wrong window (or no window). v0.1.3 introduced the
    /// pre-click TOCTOU close; v0.1.5 extends the same check to boot-time
    /// discovery and post-click re-validation, and adds PID-anchored
    /// lookups + hidden-Space detection.
    ///
    /// `reason` distinguishes which check failed (all map to exit 19):
    /// - `"window_id_gone"` — the (WID, PID) pair from discovery is no
    ///   longer in `kCGWindowListOptionAll`. RoK closed/crashed, OR the
    ///   numeric WID was reused by an unrelated window (caught by the
    ///   v0.1.5 PID anchor; v0.1.3 would have false-passed).
    /// - `"not_visible"` — the (WID, PID) exists in `kCGWindowList
    ///   OptionAll` but is missing from `kCGWindowListOptionOnScreenOnly`.
    ///   RoK is alive but hidden: another app went macOS-native
    ///   fullscreen and pushed RoK to a separate Space, or RoK is
    ///   minimized to Dock, or the `WindowServer` is in a transient
    ///   hide state. Distinct exit from `window_id_gone` so the operator
    ///   knows to switch Spaces / unminimize, not restart RoK.
    /// - `"frame_moved"` — the (WID, PID) is on screen but its frame
    ///   origin or size shifted by more than the per-axis tolerance
    ///   since discovery.
    /// - `"point_outside_frame"` — pre-click only. The requested click
    ///   point is not inside the discovered frame. Catches operator-
    ///   side coord math bugs before the AX layer is engaged. Pre-v0.1.5
    ///   this was caught accidentally by the topmost walk (deleted in
    ///   v0.1.5 because `kAXPressAction` delivers through z-order overlap
    ///   on Catalyst Bridge apps — see learnings/ax-press-works-catalyst).
    #[error(
        "RoK window state changed or unreachable (reason: {reason}). \
         Aborted before sending privileged synthetic input to avoid \
         wrong-window delivery. For 'not_visible', switch to RoK's Space \
         or unminimize. For 'window_id_gone', re-launch RoK. For \
         'frame_moved', wait for RoK to stop moving and re-run. For \
         'point_outside_frame', check that the matched target lands \
         inside the discovered window frame."
    )]
    WindowChanged { reason: &'static str },

    /// After-state verification reported no visible change post-click.
    /// The synthetic click was delivered (v0.1.5's AX press returned
    /// success, meaning the target's accessibility tree accepted the
    /// action), but the pre-click and post-click captures of the RoK
    /// window are pixel-identical within
    /// `verify::PIXEL_DIFF_REJECT_THRESHOLD`. RoK did not visibly react.
    ///
    /// v0.1.4 ships two reason tags:
    ///
    /// - `"screen_unchanged"` — the common case. Pre and post captures
    ///   decoded successfully, dims matched, but pixel-diff fell below
    ///   `verify::PIXEL_DIFF_REJECT_THRESHOLD`. RoK did not visibly react.
    ///   Outside Voice F2 from /plan-eng-review forced dropping the
    ///   originally-planned third tag (`"match_stable"` — post re-match
    ///   shows target at same coords/score), because it mis-flagged
    ///   legitimate clicks on RoK buttons that stay visible after click
    ///   (dropdowns, tabs, selections). Pixel-diff is the gate; the
    ///   re-match runs inside `verify::after_state` but logs diagnostics
    ///   only.
    /// - `"dim_mismatch"` — added in response to adversarial review of
    ///   v0.1.4. Pre and post captures decoded to `GrayImage`s with
    ///   different dimensions, so pixel-diff has nothing meaningful to
    ///   compare. Fires when the capture pipeline state-changed between
    ///   pre and post (display DPI reconfig, RoK fullscreen-borderless
    ///   toggle, screencapture padded to a different size). The verify
    ///   gate fails closed here rather than passing on the `u64::MAX`
    ///   sentinel from `pixel_diff`.
    ///
    /// Common causes by reason tag (operator's diagnostic checklist):
    ///
    /// **`screen_unchanged`:**
    /// - Target image is stale (asset rot — RoK shipped a UI update that
    ///   shifted the matched element's pixel rendering by more than the
    ///   matcher's tolerance, so the click landed on empty space).
    /// - Accessibility permission was silently revoked between
    ///   `permissions::check_accessibility` and the AX press (rare;
    ///   `AXUIElementPerformAction` would return `kAXErrorFailure`
    ///   in that window before C4's `press_at` got a chance to surface
    ///   `ClickFailed`, but a race after a successful press could
    ///   produce this state).
    /// - The matched UI element is non-interactive (decorative button
    ///   art, disabled state, or chrome that doesn't respond to input).
    /// - RoK is frozen, stuttering, or paused (App Nap, system load,
    ///   game mid-loading-screen).
    /// - Click landed in the 500ms window between RoK's render frame
    ///   and the post-capture (rare — `VERIFY_DELAY_MS` gives 2× typical
    ///   transition margin).
    ///
    /// **`dim_mismatch`:**
    /// - Operator changed display DPI / Scaled-resolution between
    ///   pre-capture and post-capture.
    /// - RoK toggled fullscreen-borderless mid-flow (frame stays within
    ///   tolerance but internal content area changed).
    /// - Display arrangement reconfig (BetterDisplay reconnect, external
    ///   monitor hot-plug) altered the capture's backing pixel grid.
    /// - Capture-pipeline integrity drift (`screencapture` chose a
    ///   different output mode for the two calls).
    ///
    /// **Not** a cause covered by this variant: server-bound clicks
    /// (resource spend, troop dispatch, server sync) that show a 1-3s UI
    /// spinner before state changes render. Those fire `screen_unchanged`
    /// despite the click landing correctly — v0.1.4 scope is UI-local
    /// only. See TODOS.md P2 "v0.1.4+ — server-roundtrip click verify"
    /// for the retry-and-poll path that fixes them.
    #[error(
        "synthetic click delivered but post-state verify failed (reason: {reason}). \
         The AX press succeeded and the target window's accessibility tree \
         accepted the action. For reason='screen_unchanged': pre/post \
         pixel-space comparison shows no change above threshold (stale \
         target, non-interactive element accepting the press without \
         visible effect, RoK frozen, or server-bound click still loading \
         per TODOS P2). For reason='dim_mismatch': pre/post captures have \
         different dimensions, indicating capture-pipeline state changed \
         between calls (display reconfig, fullscreen toggle, etc.)."
    )]
    ClickNotVerified { reason: &'static str },
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
            Self::ClickFailed { .. } => 18,
            Self::WindowChanged { .. } => 19,
            Self::ClickNotVerified { .. } => 20,
        }
    }
}

pub type Result<T> = std::result::Result<T, BotError>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::click::{REASON_DOWN, REASON_SOURCE, REASON_UP};
    use crate::verify::{REASON_DIM_MISMATCH, REASON_SCREEN_UNCHANGED};

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
            BotError::ClickFailed {
                reason: REASON_SOURCE,
            }
            .exit_code(),
            BotError::WindowChanged {
                reason: "window_id_gone",
            }
            .exit_code(),
            BotError::ClickNotVerified {
                reason: REASON_SCREEN_UNCHANGED,
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
        // The `which: "needle"` tag is also a documented contract path
        // (matcher.rs's defensive arm for malformed embedded asset). Pin it
        // to 16 explicitly so a future PartialEq-on-which derive change
        // can't silently break the contract.
        assert_eq!(
            BotError::ImageLoadFailed { which: "needle" }.exit_code(),
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
        // Pin all three documented `reason` values to 18 — reason is a string
        // tag for the operator-facing log, not part of the exit-code contract,
        // but exercising each variant ensures a future PartialEq-on-reason
        // refactor can't accidentally fork the exit code per-reason. Using
        // the click.rs constants (vs literal strings) keeps the test in sync
        // with any future rename of the reason tags.
        assert_eq!(
            BotError::ClickFailed {
                reason: REASON_SOURCE
            }
            .exit_code(),
            18
        );
        assert_eq!(
            BotError::ClickFailed {
                reason: REASON_DOWN
            }
            .exit_code(),
            18
        );
        assert_eq!(BotError::ClickFailed { reason: REASON_UP }.exit_code(), 18);
        // WindowChanged variants share exit 19. Pin all 4 documented
        // v0.1.5 reasons even though reason isn't part of the exit-code
        // contract — same rationale as ClickFailed. The v0.1.3 reason
        // "not_topmost_at_click" was deleted in v0.1.5 (replaced by
        // "not_visible" for the hidden-Space case and dropped for the
        // click-time occluder case, since AX press delivers through
        // z-order overlap on Catalyst Bridge apps).
        for reason in [
            "window_id_gone",
            "frame_moved",
            "not_visible",
            "point_outside_frame",
        ] {
            assert_eq!(
                BotError::WindowChanged { reason }.exit_code(),
                19,
                "WindowChanged({reason}) must map to exit 19"
            );
        }
        // ClickNotVerified has TWO documented reason tags. The first
        // (screen_unchanged) is the common-case pixel-diff failure;
        // the second (dim_mismatch) closes a fail-open path adversarial
        // review caught during /review of v0.1.4 (pixel_diff's u64::MAX
        // sentinel was passing verdict on capture-pipeline integrity
        // drift). /plan-eng-review's D7 dropped match_stable; the
        // dim_mismatch addition is a fail-closed correction, not a
        // walk-back of that decision (match_stable mis-flagged valid
        // clicks; dim_mismatch surfaces a real capture-pipeline state
        // change). Pin via the verify-module constants so a future
        // rename is forced through both gates (here and verify.rs).
        for reason in [REASON_SCREEN_UNCHANGED, REASON_DIM_MISMATCH] {
            assert_eq!(
                BotError::ClickNotVerified { reason }.exit_code(),
                20,
                "ClickNotVerified({reason}) must map to exit 20"
            );
        }
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

    #[test]
    fn click_not_verified_message_includes_reason_and_post_state_phrasing() {
        // Operator's first instinct on ClickNotVerified is "which gate
        // fired?" The reason tag must surface in Display output so the
        // operator's log line lands on the right diagnostic. The
        // "post-state verify" phrasing must appear so the message is
        // semantically distinguishable from ClickFailed (creation-time
        // CGEvent failure, different exit code, different fix path).
        // Using the verify-module constants rather than literals pins
        // the test to the same string-of-truth the runtime emits.
        for reason in [REASON_SCREEN_UNCHANGED, REASON_DIM_MISMATCH] {
            let err = BotError::ClickNotVerified { reason };
            let msg = err.to_string();
            assert!(
                msg.contains(reason),
                "ClickNotVerified Display must include the reason tag '{reason}': {msg}"
            );
            let lower = msg.to_lowercase();
            assert!(
                lower.contains("post-state verify"),
                "ClickNotVerified Display must reference 'post-state verify' to \
                 distinguish from ClickFailed: {msg}"
            );
        }
    }

    #[test]
    fn click_failed_message_includes_reason_and_undelivered_promise() {
        // The `reason` tag must surface so the operator's first-look log
        // line points at the right call site. v0.1.5 rewrote the message
        // to drop HID vocabulary (the path is now AX, not CGEventPost);
        // the new contract is that `ClickFailed` means the click never
        // reached the target window — debugging doesn't need to consider
        // partial-delivery scenarios.
        let err = BotError::ClickFailed {
            reason: REASON_DOWN,
        };
        let msg = err.to_string();
        assert!(
            msg.contains(REASON_DOWN),
            "ClickFailed Display must include the reason tag: {msg}"
        );
        let lower = msg.to_lowercase();
        assert!(
            lower.contains("did not reach"),
            "ClickFailed Display must promise the click did not reach the \
             target window: {msg}"
        );
    }
}
