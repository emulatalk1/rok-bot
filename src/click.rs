//! Synthetic click delivery — stealth HID tap + osascript activation.
//!
//! v0.1.6 reverts the v0.1.5 AX-press path. AX press on Mac Catalyst Bridge
//! is positionless: `AXUIElementCopyElementAtPosition` resolves the entire
//! game canvas to a single `AXGenericElement` whose `AXActivationPoint`
//! (read-only) sits at the canvas center, so every press fires at the same
//! coord regardless of (x, y). Confirmed via `/qa` 2026-05-11 (three live
//! runs opened the same center-popup instead of pressing the targeted
//! castle button) + `spikes/p5-spike/src/main.rs --ax-set-point` which
//! verified `AXUIElementIsAttributeSettable(AXActivationPoint)` returns
//! false. See `TODOS.md` P0 entry for the 6-path Mode 1 matrix.
//!
//! v0.1.6 click delivery (Mode 2 only — `src/main.rs` no longer gates
//! `Mode::Virtual`):
//!
//!   1. `osascript -e 'tell application "System Events" to set frontmost
//!      of (first process whose unix id is N) to true'` — forces RoK
//!      topmost on its display. Necessary because Catalyst Bridge apps
//!      ignore synthetic `UITouch` input until their event pipeline is woken
//!      up; activation wakes it. On a BetterDisplay virtual display the
//!      "raise" is invisible (no user observes the surface), so the Mode 1
//!      observability rejection doesn't apply.
//!   2. **Cursor stealth.** Probe current cursor position via
//!      `CGEvent::new(source).location()`, disassociate the visible cursor
//!      from the logical cursor (`CGAssociateMouseAndMouseCursorPosition
//!      (false)`), so the HID tap can route events at the click point
//!      without the user's visible cursor jumping. The user's cursor is
//!      typically on the built-in display while the bot operates on the
//!      virtual display, so even without stealth the jump would be
//!      off-screen-and-back; with stealth there is no jump at all.
//!   3. `CGEvent::post(HID)` `LeftMouseDown` at `(x, y)` (CG global-screen
//!      coords). Sleep `CLICK_GAP_MS = 80ms` (matches v0.1.3 timing).
//!      `LeftMouseUp` at the same point. Events are routed by coord, so RoK
//!      receives them as a real HID-level tap.
//!   4. `CGDisplay::warp_mouse_cursor_position(saved)` + reassociate.
//!      Visible cursor returns to where the user left it. RAII guard
//!      (`CursorStealth::Drop`) handles this on panic/early-return paths.
//!
//! All click failures map to `BotError::ClickFailed { reason }` (exit 18).
//! The `reason` strings are pinned by `error::tests`:
//!
//! - `REASON_ACTIVATION_FAILED` — `osascript` did not return 0. RoK pid
//!   may have changed since `find_rok_window`; operator's first move is to
//!   re-run.
//! - `REASON_PROBE` — `CGEvent::new(source)` failed to create the probe
//!   event we use to read the user's current cursor position. CG-level
//!   issue, very rare.
//! - `REASON_DISASSOCIATE` — `CGAssociateMouseAndMouseCursorPosition`
//!   refused. Reassociate is best-effort on Drop.
//! - `REASON_SOURCE` — `CGEventSource::new(HIDSystemState)` failed. CG-
//!   level issue, also very rare; usually means the process lost its
//!   event-source bind (Mac restarted, runaway leak in another app).
//! - `REASON_DOWN` / `REASON_UP` — `CGEvent::new_mouse_event` returned
//!   `Err` for the down or up half of the click pair. Accessibility
//!   permission may have been revoked between the boot peek and the click
//!   site; `permissions::check_accessibility` is called before this fn so
//!   a denial there should appear as exit 13, not 18 — but the race
//!   exists.

#![allow(
    unsafe_code,
    reason = "CGAssociateMouseAndMouseCursorPosition is unbound by core-graphics. \
              The unsafe surface is contained in this module."
)]

use std::ffi::c_int;
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

use core_graphics::display::{CGDisplay, CGPoint};
use core_graphics::event::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

use crate::error::{BotError, Result};
use crate::window::Window;

/// Sleep gap between `LeftMouseDown` and `LeftMouseUp` posts (milliseconds).
/// Matches v0.1.3's `CLICK_GAP_MS = 80` — long enough that RoK's event
/// loop registers a discrete click rather than collapsing the pair into
/// no-op, short enough that the bot doesn't visibly hesitate per click.
/// TODOS.md P3 carries the v0.2 `mach_wait_until` upgrade for sub-ms
/// precision under scheduler pressure.
const CLICK_GAP_MS: u64 = 80;

/// Sleep after `osascript` activation, before posting the HID tap
/// (milliseconds). Lets the AppKit→UIKit Catalyst Bridge translation
/// layer settle on the foreground change before the click arrives. 50ms
/// is a guess; lower bound is "too fast and RoK ignores the click,"
/// upper bound is "user perceives latency." Calibrate if the click
/// success rate drifts; TODOS P3 carries the empirical calibration entry.
const ACTIVATION_SETTLE_MS: u64 = 50;

// NOTE on osascript timeouts: `std::process::Command::output()` blocks
// indefinitely if the subprocess hangs. For v0.1.6, `osascript` against
// System Events typically returns in 1-2s; an indefinite hang would only
// happen if launchd is wedged (in which case the bot has bigger issues).
// If a wedged osascript becomes a real problem, port `wait-timeout` from
// the v0.2 lifecycle module plan and wrap activate_rok's Command call.

/// Reason tags surfaced via `BotError::ClickFailed { reason }`. Each
/// tag corresponds to a distinct failure mode in the click pipeline.
/// Operator-facing log lines and shell users key off these strings, so
/// a rename is a contract change (pin in `error::tests`).
pub const REASON_ACTIVATION_FAILED: &str = "activation_failed";
pub const REASON_PROBE: &str = "probe";
pub const REASON_DISASSOCIATE: &str = "disassociate";
pub const REASON_SOURCE: &str = "source";
pub const REASON_DOWN: &str = "down";
pub const REASON_UP: &str = "up";

/// Build the `AppleScript` text that activates RoK by pid. Pure for unit
/// testability — `activate_rok` shells out, but the script content is a
/// stable string we can pin.
///
/// The form `(first process whose unix id is N)` is preferred over
/// `tell application "RiseOfKingdoms"` because:
///
/// - It does not depend on knowing the bundle ID at compile time.
/// - It targets the specific pid found by `find_rok_window`, so it
///   cannot accidentally raise a stale spoofer.
/// - System Events is always installed; no separate "is RoK scriptable?"
///   probe is needed.
fn build_activation_script(pid: i32) -> String {
    format!(
        "tell application \"System Events\" to set frontmost of \
         (first process whose unix id is {pid}) to true"
    )
}

/// Live: shell out to `osascript` to set RoK frontmost. Returns
/// `Ok(())` on exit 0, `Err(ClickFailed { REASON_ACTIVATION_FAILED })`
/// otherwise.
fn activate_rok(pid: i32) -> Result<()> {
    let script = build_activation_script(pid);
    let output = Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map_err(|err| {
            tracing::warn!(
                target: "rok_bot",
                pid,
                io_error = %err,
                "osascript subprocess spawn failed"
            );
            BotError::ClickFailed {
                reason: REASON_ACTIVATION_FAILED,
            }
        })?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            target: "rok_bot",
            pid,
            exit_code = ?output.status.code(),
            stderr = %stderr,
            "osascript exited non-zero — RoK activation refused"
        );
        Err(BotError::ClickFailed {
            reason: REASON_ACTIVATION_FAILED,
        })
    }
}

/// RAII guard around `CGAssociateMouseAndMouseCursorPosition(false)` +
/// `warp_mouse_cursor_position(saved)`. On `Drop` (whether success path
/// or panic/early-return), warps the logical cursor back to its saved
/// position and reassociates. Without this, a panic between disassociate
/// and reassociate would leave the user's visible cursor stuck.
///
/// The guard is "armed" by `CursorStealth::arm`; if the probe or
/// disassociate FFI calls fail, the guard is never armed and Drop is a
/// no-op. Reassociation failures during Drop are logged at warn but not
/// propagated — Drop cannot return errors and reassociating is best-
/// effort.
struct CursorStealth {
    saved: CGPoint,
    armed: bool,
}

impl CursorStealth {
    /// Probe the current cursor location (via `CGEvent::new(source)`'s
    /// default-populated cursor field), then disassociate the visible
    /// cursor. Both steps must succeed for the guard to be armed; any
    /// failure returns `Err` and the caller does NOT have a stealth
    /// context to drop.
    fn arm(source: &CGEventSource) -> Result<Self> {
        let probe = CGEvent::new(source.clone()).map_err(|()| {
            tracing::warn!(
                target: "rok_bot",
                "CGEvent::new(source) probe for cursor location failed"
            );
            BotError::ClickFailed {
                reason: REASON_PROBE,
            }
        })?;
        let saved = probe.location();
        // SAFETY: CGAssociateMouseAndMouseCursorPosition is an exported C
        // symbol in CoreGraphics.framework. The connect bool is the only
        // argument. Returns `CGError` (i32; 0 = success).
        let err = unsafe { CGAssociateMouseAndMouseCursorPosition(0) };
        if err != 0 {
            tracing::warn!(
                target: "rok_bot",
                cg_error = err,
                "CGAssociateMouseAndMouseCursorPosition(false) refused"
            );
            return Err(BotError::ClickFailed {
                reason: REASON_DISASSOCIATE,
            });
        }
        Ok(Self { saved, armed: true })
    }
}

impl Drop for CursorStealth {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Err(err) = CGDisplay::warp_mouse_cursor_position(self.saved) {
            tracing::warn!(
                target: "rok_bot",
                cg_error = err,
                saved_x = self.saved.x,
                saved_y = self.saved.y,
                "CGWarpMouseCursorPosition during stealth cleanup failed; \
                 user cursor may not be restored to its pre-click position"
            );
        }
        // SAFETY: Same FFI as arm(); connect=1 reverses the disassociation.
        let err = unsafe { CGAssociateMouseAndMouseCursorPosition(1) };
        if err != 0 {
            tracing::warn!(
                target: "rok_bot",
                cg_error = err,
                "CGAssociateMouseAndMouseCursorPosition(true) during stealth \
                 cleanup failed; the user's cursor may be stuck disassociated. \
                 Restart the cursor with a trackpad gesture or run \
                 `pkill -SIGCONT WindowServer` as a last resort."
            );
        }
    }
}

/// Synthesize and post a single left-click at `point` (CG global-screen
/// coords) to the process owning `window.pid`. v0.1.6 path: activate +
/// stealth-disassociate + HID tap pair + cursor-restore.
///
/// Caller (`main.rs::run`) must have already:
/// - verified Accessibility permission via `permissions::check_accessibility()`
///   (required for `CGEvent::post(HID)` to deliver),
/// - re-validated the window state via `window::validate_at_click_site`.
pub fn click_at(window: &Window, point: CGPoint) -> Result<()> {
    activate_rok(window.pid)?;
    sleep(Duration::from_millis(ACTIVATION_SETTLE_MS));

    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState).map_err(|()| {
        tracing::warn!(
            target: "rok_bot",
            "CGEventSource::new(HIDSystemState) failed"
        );
        BotError::ClickFailed {
            reason: REASON_SOURCE,
        }
    })?;

    let _stealth = CursorStealth::arm(&source)?;

    let down = CGEvent::new_mouse_event(
        source.clone(),
        CGEventType::LeftMouseDown,
        point,
        CGMouseButton::Left,
    )
    .map_err(|()| {
        tracing::warn!(
            target: "rok_bot",
            x = point.x,
            y = point.y,
            "CGEvent::new_mouse_event(LeftMouseDown) failed"
        );
        BotError::ClickFailed {
            reason: REASON_DOWN,
        }
    })?;

    let up = CGEvent::new_mouse_event(source, CGEventType::LeftMouseUp, point, CGMouseButton::Left)
        .map_err(|()| {
            tracing::warn!(
                target: "rok_bot",
                x = point.x,
                y = point.y,
                "CGEvent::new_mouse_event(LeftMouseUp) failed"
            );
            BotError::ClickFailed { reason: REASON_UP }
        })?;

    tracing::info!(
        target: "rok_bot",
        pid = window.pid,
        window_id = window.id,
        x = point.x,
        y = point.y,
        "posting HID click pair to RoK (stealth-cursor)"
    );
    down.post(CGEventTapLocation::HID);
    sleep(Duration::from_millis(CLICK_GAP_MS));
    up.post(CGEventTapLocation::HID);

    // `_stealth` drops here, restoring the cursor + reassociating.
    Ok(())
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    /// `CGError CGAssociateMouseAndMouseCursorPosition(boolean_t connect)`
    /// — `boolean_t` is `c_int` (32-bit). 0 disassociates (cursor stops
    /// following logical mouse position); non-zero reassociates. Returns
    /// 0 on success; non-zero `CGError` codes on failure.
    fn CGAssociateMouseAndMouseCursorPosition(connect: c_int) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reason_constants_match_expected_strings() {
        // Operator-facing log strings + shell-user grep targets. A rename
        // is a contract change; pin via these tests so any drift fails
        // here before reaching a user.
        assert_eq!(REASON_ACTIVATION_FAILED, "activation_failed");
        assert_eq!(REASON_PROBE, "probe");
        assert_eq!(REASON_DISASSOCIATE, "disassociate");
        assert_eq!(REASON_SOURCE, "source");
        assert_eq!(REASON_DOWN, "down");
        assert_eq!(REASON_UP, "up");
    }

    #[test]
    fn click_reasons_all_map_to_click_failed_exit_18() {
        // Defense-in-depth pin: every click reason this module emits at
        // runtime must produce BotError::ClickFailed with exit 18. Mirrors
        // the v0.1.5 ax.rs pin which guarded against a future split of
        // ClickFailed into mechanism-specific variants.
        for reason in [
            REASON_ACTIVATION_FAILED,
            REASON_PROBE,
            REASON_DISASSOCIATE,
            REASON_SOURCE,
            REASON_DOWN,
            REASON_UP,
        ] {
            let err = BotError::ClickFailed { reason };
            assert_eq!(err.exit_code(), 18, "reason {reason} must map to exit 18");
        }
    }

    #[test]
    fn activation_script_targets_pid_via_system_events() {
        // The script form matters. (a) "first process whose unix id is N"
        // targets a specific pid — no spoof risk vs. the `tell application
        // "RiseOfKingdoms"` form which dispatches by name. (b) System
        // Events is always installed, so no per-bundle scriptability
        // probe is needed. (c) `set frontmost ... to true` is the
        // documented activation primitive.
        let script = build_activation_script(95833);
        assert!(
            script.contains("System Events"),
            "script must dispatch through System Events: {script}"
        );
        assert!(
            script.contains("unix id is 95833"),
            "script must include the specific pid (not bundle name): {script}"
        );
        assert!(
            script.contains("frontmost"),
            "script must call out frontmost as the activation primitive: {script}"
        );
        assert!(
            script.contains("true"),
            "script must set frontmost to true (activation, not deactivation): {script}"
        );
    }

    #[test]
    fn activation_script_handles_typical_pids() {
        // pids on macOS are i32 but in practice fit in u32. Make sure the
        // formatter doesn't choke on edge values (single-digit pid 1 for
        // launchd-style daemons; max-i32 for runaway-counter regressions).
        let small = build_activation_script(1);
        let large = build_activation_script(i32::MAX);
        assert!(small.contains("unix id is 1"), "small pid: {small}");
        assert!(
            large.contains(&format!("unix id is {}", i32::MAX)),
            "large pid: {large}"
        );
    }

    #[test]
    fn click_gap_ms_in_sane_range() {
        // CLICK_GAP_MS bounds the rhythm of the synthesized click pair.
        // Too fast (sub-10ms) and RoK's event loop collapses the pair
        // into a no-op; too slow (sub-second+) and the bot visibly
        // hesitates per click + scheduler drift becomes a TODO P3
        // concern. Pin a wide-but-finite range so a future bump leaves
        // a paper trail.
        assert!(
            (10..=500).contains(&CLICK_GAP_MS),
            "CLICK_GAP_MS = {CLICK_GAP_MS}ms drifted outside 10..=500; \
             if intentional, update both this pin and the constant doc."
        );
    }

    #[test]
    fn activation_settle_ms_in_sane_range() {
        // ACTIVATION_SETTLE_MS gives the AppKit→UIKit Catalyst Bridge
        // translation layer time to apply the foreground change before
        // the HID tap arrives. Sub-10ms risks RoK ignoring the click;
        // multi-second adds perceptible latency.
        assert!(
            (10..=1000).contains(&ACTIVATION_SETTLE_MS),
            "ACTIVATION_SETTLE_MS = {ACTIVATION_SETTLE_MS}ms drifted outside \
             10..=1000; if intentional, update both this pin and the constant \
             doc."
        );
    }
}
