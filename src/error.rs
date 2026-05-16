use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BotError {
    #[error("RoK window not found — is the game running?")]
    WindowNotFound,

    #[error("RoK window center is not on any online display")]
    WindowScreenUnresolved,

    /// A required macOS permission is denied. The `which` tag is one of
    /// two values: `"Screen Recording"` (from
    /// `permissions::check_screen_recording`, needed for `ScreenCaptureKit`)
    /// or `"Accessibility"` (from `permissions::check_accessibility`, needed
    /// for `CGEvent::post` to deliver synthetic clicks — checked as a hard
    /// boot gate in v0.2 since the continuous loop always clicks).
    #[error(
        "Missing macOS permission: {which}. Grant it to your terminal app in \
         System Settings → Privacy & Security → {which}, then re-run."
    )]
    PermissionsMissing { which: &'static str },

    /// RoK window capture failed at one of the structured pipeline
    /// stages. v0.1.8 split the original opaque `CaptureFailed` into
    /// stage-tagged failures so operators (and shell scripts grepping
    /// the error message) can branch on which step failed without
    /// scanning prior log lines.
    ///
    /// `stage` values are the `STAGE_*` constants in `capture.rs` and
    /// `permissions.rs` — pinned as `&'static str` so the operator-
    /// facing log line is always one of the documented values:
    ///
    /// - `"symlink_refused"` — output path was a pre-existing symlink.
    ///   Refused before invoking SCK to close the local-attacker hazard
    ///   surfaced by /review F2 (a pre-placed symlink at
    ///   `rok-capture-pre.png` would let the capture write through to
    ///   any file the user can write).
    /// - `"no_shareable_content"` — `SCShareableContent.getShareable
    ///   ContentWithCompletionHandler` returned nil or errored. Most
    ///   common cause: Screen Recording TCC denied for the rok-bot
    ///   binary (v0.1.8 needs SR granted to the binary itself, not
    ///   just its parent terminal — UX regression vs v0.1.x's
    ///   screencapture-CLI shellout). Less common: macOS pre-14.0
    ///   (SCK requires 14+).
    /// - `"window_not_found"` — `SCShareableContent.windows` did not
    ///   contain a window with the requested `CGWindowID`. Either
    ///   RoK closed between discovery and capture, or the cached
    ///   shareable-content list is stale (cache invalidates on this
    ///   stage; the next call re-fetches).
    /// - `"capture_returned_nil"` — `SCScreenshotManager.captureImage
    ///   WithFilter:configuration:completionHandler:` reported success
    ///   but the `CGImage` pointer was nil, OR the completion handler
    ///   never fired before the 5s timeout (T1 deadlock fail-safe). A
    ///   nil image with no error indicates SCK gave up; the timeout
    ///   indicates SCK never delivered (rare in production; see
    ///   `tests/sck_integration.rs::live_capture_times_out_when_
    ///   disconnected`).
    /// - `"cgimage_decode"` — pixel-byte read from the returned
    ///   `CGImage` failed. Either `CFData` length was negative, the
    ///   buffer was undersized, or the BGRA→RGBA conversion bailed.
    ///   The ×2 scale canary (codex #10) also surfaces here when the
    ///   captured dims don't match `frame.width * 2 × frame.height *
    ///   2` (operator on a non-Retina or scaled display; see TODOS
    ///   D10 for the proper fix).
    /// - `"png_write"` — `image::save_buffer` failed to write the
    ///   captured PNG to disk. Disk full, permission issue on the
    ///   output dir, or the matcher's PNG round-trip would fail
    ///   downstream. Write happens AFTER the SCK capture succeeded,
    ///   so this stage only fires on filesystem-level problems.
    ///
    /// `exit_code` is `Option<i32>` for shell-script compat: v0.1.x's
    /// `screencapture` subprocess sometimes returned a non-zero exit
    /// that operators branched on. v0.1.8 has no subprocess — the
    /// field stays `Some(0)` for the v0.1.x 0-byte equivalent
    /// (`capture_returned_nil` after a successful SCK call) and `None`
    /// for everything else, so existing shell branches keep working.
    /// Stage tag is the future-proof discriminator.
    #[error(
        "RoK window capture failed (stage: {stage}, exit code: {exit_code:?}). \
         Ensure Screen Recording is granted to the rok-bot binary in \
         System Settings → Privacy & Security → Screen Recording and \
         that the RoK window is on a captured display."
    )]
    CaptureFailed {
        stage: &'static str,
        exit_code: Option<i32>,
    },

    /// Best-match score below `matcher::MATCH_THRESHOLD`. The matcher already
    /// logged the diagnostic numbers (`best_score`, `threshold`) at warn level
    /// before this variant fires; this is the structured exit-code carrier.
    #[error("target not found in capture (best match below confidence threshold)")]
    TargetNotFound,

    /// PNG decode or open failed. The underlying `image::ImageError` is
    /// logged at warn before mapping to this. `which` is two-valued:
    /// `"haystack"` — the capture PNG failed to open/decode (or exceeded
    /// `MAX_HAYSTACK_DIM`); `"needle"` — an `include_bytes!`-embedded
    /// needle asset failed to decode. `find_best_needle` decodes each
    /// needle per call, so a malformed committed asset surfaces here.
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

    /// The synthetic click did not reach the target window. v0.1.6
    /// routes click delivery through `CGEvent::post(HID)` with explicit
    /// `osascript`-driven activation and cursor stealth (see
    /// `src/click.rs` for the full flow). v0.1.5's AX-press path was
    /// reverted because `AXUIElementPerformAction(kAXPressAction)` is
    /// positionless on Mac Catalyst Bridge apps (every press fires at
    /// the canvas-center `AXActivationPoint`, regardless of the (x, y)
    /// passed to `CopyElementAtPosition`).
    ///
    /// `reason` distinguishes which step in the click pipeline failed:
    ///
    /// - `"activation_failed"` — `osascript` returned non-zero when
    ///   trying to set RoK frontmost. RoK's pid changed between
    ///   `find_rok_window` and the click site, OR System Events refused
    ///   the request (rare).
    /// - `"probe"` — `CGEvent::new(source)` failed when probing the
    ///   user's current cursor position. CG-level issue.
    /// - `"disassociate"` — `CGAssociateMouseAndMouseCursorPosition
    ///   (false)` refused. The visible cursor cannot be detached from
    ///   the logical cursor; aborting prevents a visible cursor jump
    ///   during the HID tap.
    /// - `"source"` — `CGEventSource::new(HIDSystemState)` failed. CG-
    ///   level issue, typically only seen after a Mac restart or a
    ///   runaway-leak in another app.
    /// - `"down"` / `"up"` — `CGEvent::new_mouse_event` returned `Err`
    ///   for the down or up half of the click pair. Accessibility
    ///   permission may have been revoked between
    ///   `permissions::check_accessibility` and this site; a denial
    ///   there normally surfaces as `PermissionsMissing` (exit 13), but
    ///   the race exists.
    ///
    /// Verifying that a successful HID tap caused a visible state change
    /// is `ClickNotVerified`'s job — `CGEvent::post` returns `()` and
    /// provides no delivery confirmation, so success here means the
    /// event was posted, not that RoK reacted.
    #[error(
        "synthetic click could not be delivered (reason: {reason}). \
         The click did not reach the target window. A step in the \
         activate→stealth→HID-tap pipeline failed before the event \
         pair was posted — check the preceding warn log for the \
         underlying CG/osascript error."
    )]
    ClickFailed { reason: &'static str },

    /// The RoK window's state changed between discovery and a click
    /// site, OR was already in an unreachable state, in a way that would
    /// let a synthetic HID click land on the wrong window (or no window).
    /// v0.1.3 introduced the pre-click TOCTOU close; v0.1.5 extended the
    /// check to boot-time discovery and post-click re-validation with
    /// PID-anchored lookups + hidden-Space detection; v0.2 runs this
    /// check every loop tick.
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
    ///   side coord math bugs before any synthetic input is engaged.
    ///   Pre-v0.1.5 this was caught accidentally by the topmost walk;
    ///   v0.1.5 added the explicit bounds check, and v0.1.6 preserves
    ///   it (click-mechanism-independent).
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

    /// Post-click needle-swap verification failed (v0.2, design D11).
    /// The click was delivered, but a re-capture of the toggle ROI did
    /// not confirm the city-view to world-view (or back) toggle.
    ///
    /// v0.2's continuous loop replaced v0.1.4's pixel-diff verify.
    /// Pixel-diff trivially passed on ANY view change AND false-passed
    /// a missed click during RoK's ambient animation (water, troops,
    /// weather) — it could not tell "the toggle fired" from "the screen
    /// happened to move." Needle-swap is animation-immune: the bot
    /// matches against two needles (city-view art = needle index 0,
    /// world-view art = needle index 1); a confirmed toggle means the
    /// post-click capture matches a DIFFERENT needle than the pre-click
    /// capture matched.
    ///
    /// v0.2 reason tags (both pinned in `verify.rs`):
    ///
    /// - `"no_swap"` — the post-click re-match found the SAME needle
    ///   that matched pre-click. The view did not toggle: the click
    ///   landed on a non-interactive pixel, RoK was mid-loading-screen,
    ///   or the click missed the button.
    /// - `"neither_needle"` — the post-click re-match found NEITHER
    ///   needle above `matcher::MATCH_THRESHOLD`. Usually a mid-
    ///   transition frame (the verify delay landed inside the
    ///   city↔world cross-fade and neither art scored). Treated as an
    ///   unconfirmed swap, i.e. a failed tick — the loop retries and a
    ///   real toggle confirms on the next tick.
    ///
    /// Both tags are TRANSIENT in the loop's error policy (design D5):
    /// one failure is noise, `run_loop::LOOP_FAILURE_BUDGET` in a row
    /// aborts the loop with `LoopAborted`.
    #[error(
        "post-click needle-swap verify failed (reason: {reason}). \
         The click was delivered but the city↔world toggle was not \
         confirmed. For 'no_swap', the post-click capture still matched \
         the same view's needle — the click did not toggle the view. \
         For 'neither_needle', neither needle matched the post-click \
         capture, likely a mid-transition frame; the loop retries."
    )]
    ClickNotVerified { reason: &'static str },

    /// The v0.2 continuous loop aborted. Two paths reach here, both at
    /// a tick boundary (never mid-click — the loop only checks for
    /// abort between ticks):
    ///
    /// - `"failure_budget_exhausted"` — `run_loop::LOOP_FAILURE_BUDGET`
    ///   transient failures occurred in a row with no successful tick
    ///   resetting the counter. A transient failure is `TargetNotFound`,
    ///   `ClickNotVerified`, `CaptureFailed`, or
    ///   `WindowChanged{not_visible}` (see `run_loop::classify_error`).
    ///   One failure is noise (RoK mid-animation, a dropped frame); N
    ///   in a row means RoK is frozen, the needle asset rotted, or RoK
    ///   got hidden for good — the loop stops rather than spin forever.
    ///   The per-tick warn log carries the specific error of each
    ///   failed tick; this variant carries only the abort trigger.
    /// - `"signal_handler_install_failed"` — `ctrlc::set_handler`
    ///   failed at boot. The loop refuses to start: without the SIGINT
    ///   handler a Ctrl-C would hard-kill the process mid-click and
    ///   strand a `LeftMouseDown` in RoK's event queue. Aborting before
    ///   tick 1 is safer than running a loop that cannot shut down
    ///   cleanly.
    ///
    /// Exit code 21 — the first free slot after v0.1.x's 10-20 range
    /// (12 is the retired `RokNotOnPrimary` gap).
    #[error(
        "continuous loop aborted (reason: {reason}). For \
         'failure_budget_exhausted', check the preceding per-tick warn \
         logs for the repeated failure — RoK may be frozen, hidden, or \
         the target needle may have rotted. For \
         'signal_handler_install_failed', the SIGINT handler could not \
         be installed; re-run, and if it persists check for another \
         process holding the handler."
    )]
    LoopAborted { reason: &'static str },
}

impl BotError {
    /// Stable, per-variant exit code for `main`. Lets shell users branch on outcome.
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::WindowNotFound => 10,
            Self::WindowScreenUnresolved => 11,
            // 12 was BotError::RokNotOnPrimary (Mode 1 only gate). Deleted
            // in v0.1.6 when src/main.rs dropped the Mode::Virtual gate
            // to enable Mode 2. The numeric slot is left unused (not
            // reassigned) so shell users with stale `case "$?" in 12)` arms
            // see a clean "exit 12 never fires" rather than a meaning swap.
            Self::PermissionsMissing { .. } => 13,
            Self::CaptureFailed { .. } => 14,
            Self::TargetNotFound => 15,
            Self::ImageLoadFailed { .. } => 16,
            Self::TargetTooLarge { .. } => 17,
            Self::ClickFailed { .. } => 18,
            Self::WindowChanged { .. } => 19,
            Self::ClickNotVerified { .. } => 20,
            Self::LoopAborted { .. } => 21,
        }
    }
}

pub type Result<T> = std::result::Result<T, BotError>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{
        STAGE_CAPTURE_RETURNED_NIL, STAGE_CGIMAGE_DECODE, STAGE_PNG_WRITE, STAGE_SYMLINK_REFUSED,
        STAGE_WINDOW_NOT_FOUND,
    };
    use crate::click::{
        REASON_ACTIVATION_FAILED, REASON_DISASSOCIATE, REASON_DOWN, REASON_PROBE, REASON_SOURCE,
        REASON_UP,
    };
    use crate::permissions::STAGE_NO_SHAREABLE_CONTENT;
    use crate::run_loop::{REASON_FAILURE_BUDGET_EXHAUSTED, REASON_SIGNAL_INSTALL_FAILED};
    use crate::verify::{REASON_NEITHER_NEEDLE, REASON_NO_SWAP};

    #[test]
    fn exit_codes_are_stable_and_unique() {
        let codes = [
            BotError::WindowNotFound.exit_code(),
            BotError::WindowScreenUnresolved.exit_code(),
            BotError::PermissionsMissing {
                which: "Screen Recording",
            }
            .exit_code(),
            BotError::CaptureFailed {
                stage: STAGE_CAPTURE_RETURNED_NIL,
                exit_code: Some(1),
            }
            .exit_code(),
            BotError::TargetNotFound.exit_code(),
            BotError::ImageLoadFailed { which: "haystack" }.exit_code(),
            BotError::TargetTooLarge {
                needle: (50, 50),
                haystack: (40, 40),
            }
            .exit_code(),
            BotError::ClickFailed {
                reason: REASON_DOWN,
            }
            .exit_code(),
            BotError::WindowChanged {
                reason: "window_id_gone",
            }
            .exit_code(),
            BotError::ClickNotVerified {
                reason: REASON_NO_SWAP,
            }
            .exit_code(),
            BotError::LoopAborted {
                reason: REASON_FAILURE_BUDGET_EXHAUSTED,
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
        // Exit 12 was BotError::RokNotOnPrimary, deleted in v0.1.6 when
        // the Mode::Virtual gate was dropped. No producer means no test;
        // a future re-introduction must pick a different slot to avoid
        // colliding with shell users' stale `case "$?" in 12)` arms.
        assert_eq!(
            BotError::PermissionsMissing {
                which: "Screen Recording"
            }
            .exit_code(),
            13
        );
        // CaptureFailed exit code is stage-independent (always 14) — pin
        // it across every documented stage so a future per-stage routing
        // change is forced through the test, AND so codex #14's "match-
        // arm test pins every stage string" requirement is satisfied at
        // the exit-code seam. Using the STAGE_* constants keeps the test
        // in sync with any future rename of a stage tag.
        for stage in [
            STAGE_SYMLINK_REFUSED,
            STAGE_NO_SHAREABLE_CONTENT,
            STAGE_WINDOW_NOT_FOUND,
            STAGE_CAPTURE_RETURNED_NIL,
            STAGE_CGIMAGE_DECODE,
            STAGE_PNG_WRITE,
        ] {
            assert_eq!(
                BotError::CaptureFailed {
                    stage,
                    exit_code: Some(1),
                }
                .exit_code(),
                14,
                "CaptureFailed({stage}, Some(1)) must map to exit 14"
            );
            assert_eq!(
                BotError::CaptureFailed {
                    stage,
                    exit_code: None,
                }
                .exit_code(),
                14,
                "CaptureFailed({stage}, None) must map to exit 14"
            );
        }
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
        // Pin all six v0.1.6 click `reason` values to 18 — reason is a
        // string tag for the operator-facing log, not part of the exit-
        // code contract, but exercising each variant ensures a future
        // PartialEq-on-reason refactor can't accidentally fork the exit
        // code per-reason. Using the click.rs constants (vs literal
        // strings) keeps the test in sync with any future rename of the
        // reason tags. The v0.1.5 AX reasons
        // (REASON_AX_APP_RESOLVE_FAILED / .._ELEMENT_RESOLVE_FAILED /
        // ..PRESS_FAILED / ..TIMEOUT) were deleted in v0.1.6 when
        // click.rs reverted from AX press to stealth HID + activation.
        for reason in [
            REASON_ACTIVATION_FAILED,
            REASON_PROBE,
            REASON_DISASSOCIATE,
            REASON_SOURCE,
            REASON_DOWN,
            REASON_UP,
        ] {
            assert_eq!(
                BotError::ClickFailed { reason }.exit_code(),
                18,
                "ClickFailed({reason}) must map to exit 18"
            );
        }
        // WindowChanged variants share exit 19. Pin all 4 documented
        // v0.1.5 reasons even though reason isn't part of the exit-code
        // contract — same rationale as ClickFailed. v0.1.6 preserves the
        // v0.1.5 set: window_id_gone, frame_moved, not_visible,
        // point_outside_frame. The pre-v0.1.5 "not_topmost_at_click" was
        // deleted in v0.1.5 when AX press took over click delivery; in
        // v0.1.6 the explicit osascript activation + the HID tap making
        // RoK topmost-on-its-display tautologically removed the need to
        // re-introduce a topmost-at-click check.
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
        // ClickNotVerified has TWO documented reason tags in v0.2's
        // needle-swap verify (design D11, which retired the v0.1.4
        // pixel-diff path and its screen_unchanged/dim_mismatch tags):
        // no_swap (post-click capture matched the same needle — view
        // didn't toggle) and neither_needle (post-click capture matched
        // neither needle — mid-transition frame). Pin via the verify-
        // module constants so a future rename is forced through both
        // gates (here and verify.rs).
        for reason in [REASON_NO_SWAP, REASON_NEITHER_NEEDLE] {
            assert_eq!(
                BotError::ClickNotVerified { reason }.exit_code(),
                20,
                "ClickNotVerified({reason}) must map to exit 20"
            );
        }
        // LoopAborted (v0.2) shares no exit code with v0.1.x — slot 21
        // is the first free code after the 10-20 range. Pin both
        // documented reasons via the run_loop-module constants.
        for reason in [
            REASON_FAILURE_BUDGET_EXHAUSTED,
            REASON_SIGNAL_INSTALL_FAILED,
        ] {
            assert_eq!(
                BotError::LoopAborted { reason }.exit_code(),
                21,
                "LoopAborted({reason}) must map to exit 21"
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
            BotError::CaptureFailed {
                stage: STAGE_CAPTURE_RETURNED_NIL,
                exit_code: Some(1),
            },
            BotError::CaptureFailed {
                stage: STAGE_NO_SHAREABLE_CONTENT,
                exit_code: None,
            },
        ] {
            let msg = err.to_string();
            assert!(
                msg.contains("Screen Recording"),
                "capture failure should hint at the most likely root cause: {msg}"
            );
        }
    }

    /// Pin every documented `stage` value into the Display string so a
    /// future rename of a stage constant is forced through this test
    /// (codex #14: "match-arm test pins every stage string"). The
    /// `which`-style tag scheme uses the value as part of the operator-
    /// facing log line — silent renames break shell scripts pattern-
    /// matching against the message.
    #[test]
    fn capture_failed_message_includes_every_documented_stage() {
        for stage in [
            STAGE_SYMLINK_REFUSED,
            STAGE_NO_SHAREABLE_CONTENT,
            STAGE_WINDOW_NOT_FOUND,
            STAGE_CAPTURE_RETURNED_NIL,
            STAGE_CGIMAGE_DECODE,
            STAGE_PNG_WRITE,
        ] {
            let err = BotError::CaptureFailed {
                stage,
                exit_code: None,
            };
            let msg = err.to_string();
            assert!(
                msg.contains(stage),
                "CaptureFailed Display must include the stage tag '{stage}': {msg}"
            );
        }
    }

    /// The six documented stage constants must all be distinct strings.
    /// A duplicate would silently merge two failure modes in operator
    /// log parsing — caught here at compile-test time rather than in
    /// the field.
    #[test]
    fn capture_failed_stage_constants_are_unique() {
        let stages = [
            STAGE_SYMLINK_REFUSED,
            STAGE_NO_SHAREABLE_CONTENT,
            STAGE_WINDOW_NOT_FOUND,
            STAGE_CAPTURE_RETURNED_NIL,
            STAGE_CGIMAGE_DECODE,
            STAGE_PNG_WRITE,
        ];
        let unique: std::collections::HashSet<&'static str> = stages.iter().copied().collect();
        assert_eq!(
            unique.len(),
            stages.len(),
            "STAGE_* constants must be unique: {stages:?}"
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
    fn click_not_verified_message_includes_reason_and_needle_swap_phrasing() {
        // Operator's first instinct on ClickNotVerified is "which gate
        // fired?" The reason tag must surface in Display output so the
        // operator's log line lands on the right diagnostic. The
        // "needle-swap verify" phrasing must appear so the message is
        // semantically distinguishable from ClickFailed (creation-time
        // CGEvent failure, different exit code, different fix path).
        // Using the verify-module constants rather than literals pins
        // the test to the same string-of-truth the runtime emits.
        for reason in [REASON_NO_SWAP, REASON_NEITHER_NEEDLE] {
            let err = BotError::ClickNotVerified { reason };
            let msg = err.to_string();
            assert!(
                msg.contains(reason),
                "ClickNotVerified Display must include the reason tag '{reason}': {msg}"
            );
            let lower = msg.to_lowercase();
            assert!(
                lower.contains("needle-swap verify"),
                "ClickNotVerified Display must reference 'needle-swap verify' to \
                 distinguish from ClickFailed: {msg}"
            );
        }
    }

    #[test]
    fn loop_aborted_message_includes_reason_and_loop_phrasing() {
        // v0.2 LoopAborted (exit 21). The reason tag must surface so the
        // operator's log line points at the abort trigger; the "loop
        // aborted" phrasing must appear so the message is unmistakably
        // a loop-lifecycle failure, not a single-tick one. Pin via the
        // run_loop-module constants.
        for reason in [
            REASON_FAILURE_BUDGET_EXHAUSTED,
            REASON_SIGNAL_INSTALL_FAILED,
        ] {
            let err = BotError::LoopAborted { reason };
            let msg = err.to_string();
            assert!(
                msg.contains(reason),
                "LoopAborted Display must include the reason tag '{reason}': {msg}"
            );
            assert!(
                msg.to_lowercase().contains("loop aborted"),
                "LoopAborted Display must reference 'loop aborted': {msg}"
            );
        }
    }

    #[test]
    fn click_failed_message_includes_reason_and_undelivered_promise() {
        // The `reason` tag must surface so the operator's first-look log
        // line points at the right call site. v0.1.6 rewrote the message
        // back to HID + activation vocabulary (the path reverted from
        // AX press because Catalyst Bridge AX press is positionless).
        // Contract: `ClickFailed` means the click never reached the
        // target window — debugging doesn't need to consider
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
