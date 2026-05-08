//! Synthetic mouse-click delivery via Quartz Event Services.
//!
//! v0.1.3 ships a single shape: a left-button down/up pair posted at HID
//! event-tap level via `CGEventPost(kCGHIDEventTap, ...)`. That tap level
//! is what the OS itself uses for real mouse input, so RoK (and any other
//! foreground process on the target display) processes the events as if a
//! human had clicked. Verified empirically by the P3 spike — see
//! `spikes/p3-spike/README.md` for the verification procedure and the
//! recorded outcome.
//!
//! Three deliberate non-features:
//!
//! - **No cursor warp.** We do not call `CGWarpMouseCursorPosition` before
//!   posting the click. The synthesized event carries its own
//!   `mouseCursorPosition` field, which is what the OS dispatches against;
//!   warping the visible cursor is purely cosmetic and would make the bot's
//!   activity visible to the user (design A4 — bot stays invisible).
//!
//! - **No drag, no double-click, no right-click.** A single left-click pair
//!   covers the v0.1 surface. Other shapes are deferred to TODOS P3
//!   (additional click types).
//!
//! - **No post-time error detection.** `CGEvent::post(...)` returns `()`,
//!   not `Result`. There is no Quartz-level signal that a posted event was
//!   dropped, filtered, or ignored. `BotError::ClickFailed` therefore only
//!   covers *creation-time* failures (`CGEventSource::new` or
//!   `CGEvent::new_mouse_event` returning `Err`); verifying that the click
//!   landed is v0.1.4's job (after-state capture diff).

use std::thread::sleep;
use std::time::Duration;

use core_graphics::display::CGPoint;
use core_graphics::event::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

use crate::error::{BotError, Result};

/// Delay between mouseDown and mouseUp. Real human clicks last ~50–150 ms.
/// 80 ms is comfortably inside that range and matches what the P3 spike
/// posted during verification. Some games filter "too fast" synthetic
/// clicks as bot signal; staying in human range is cheap defence.
const CLICK_GAP_MS: u64 = 80;

/// Reason tags surfaced via `BotError::ClickFailed { reason }`. Pinned as
/// constants so the strings appearing in operator-facing logs (and asserted
/// against in tests) live in one place. Changing one of these is a
/// user-facing log contract change — flip the constant and update the
/// `error::tests::exit_codes_have_specific_stable_values` test together.
pub const REASON_SOURCE: &str = "source_creation_failed";
pub const REASON_DOWN: &str = "down_event_creation_failed";
pub const REASON_UP: &str = "up_event_creation_failed";

/// Build a (mouseDown, mouseUp) pair at `point` in CG global-screen coords.
///
/// Caller supplies the `CGEventSource` so this function stays pure-ish:
/// no Quartz state is created here and no events are posted; the source
/// itself is the only Quartz handle, which the caller controls. This
/// keeps the unit-testable surface (the construction failure → typed
/// `BotError::ClickFailed` mapping) exercisable without forcing a live
/// event tap into tests.
///
/// Returns `Err(ClickFailed { reason: REASON_DOWN | REASON_UP })` if the
/// underlying `CGEvent::new_mouse_event` constructor returns `Err`. That
/// constructor is documented to fail when the system is unable to allocate
/// the event (e.g., extreme memory pressure) — rare in practice but a
/// real failure path worth surfacing as a typed exit instead of a panic.
pub fn build_click_events(source: &CGEventSource, point: CGPoint) -> Result<(CGEvent, CGEvent)> {
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
            "CGEvent::new_mouse_event(LeftMouseDown) returned Err"
        );
        BotError::ClickFailed {
            reason: REASON_DOWN,
        }
    })?;
    let up = CGEvent::new_mouse_event(
        source.clone(),
        CGEventType::LeftMouseUp,
        point,
        CGMouseButton::Left,
    )
    .map_err(|()| {
        tracing::warn!(
            target: "rok_bot",
            x = point.x,
            y = point.y,
            "CGEvent::new_mouse_event(LeftMouseUp) returned Err"
        );
        BotError::ClickFailed { reason: REASON_UP }
    })?;
    Ok((down, up))
}

/// Live: synthesize and post a single left-click at `(x, y)` in CG
/// global-screen coordinates.
///
/// Flow:
///
///   1. Create `CGEventSource(HIDSystemState)`. Failure → `ClickFailed { reason: REASON_SOURCE }`.
///   2. Build the down/up pair via [`build_click_events`]. Failure → `ClickFailed { REASON_DOWN | REASON_UP }`.
///   3. Post `down` to the HID tap.
///   4. Sleep `CLICK_GAP_MS` (80 ms — inside human-click range).
///   5. Post `up` to the HID tap.
///
/// Once `CGEvent::post` is invoked, there is no error signal — see the
/// module docs for why.
///
/// Caller must have already verified Accessibility via
/// `permissions::check_accessibility()`; without it, the down/up events
/// are silently dropped by the system input pipeline (no panic, no error,
/// just no observable effect on the target window). That preflight is
/// `main.rs::run`'s responsibility, not ours.
pub fn click_at(x: f64, y: f64) -> Result<()> {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState).map_err(|()| {
        tracing::warn!(
            target: "rok_bot",
            "CGEventSource::new(HIDSystemState) returned Err"
        );
        BotError::ClickFailed {
            reason: REASON_SOURCE,
        }
    })?;
    let point = CGPoint::new(x, y);
    let (down, up) = build_click_events(&source, point)?;
    down.post(CGEventTapLocation::HID);
    sleep(Duration::from_millis(CLICK_GAP_MS));
    up.post(CGEventTapLocation::HID);
    tracing::info!(
        target: "rok_bot",
        x,
        y,
        gap_ms = CLICK_GAP_MS,
        "posted left-click pair to HID event tap"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_click_events_succeeds_with_live_source() {
        // Happy path: a live CGEventSource on the test runner host should
        // accept the constructor calls. This is the only end-to-end path
        // we can unit-test — the post side has no error signal — so we
        // pin both that the construction succeeds AND that the two events
        // have the correct distinct types. A refactor returning
        // `Ok((down.clone(), down.clone()))` would type-check but post
        // two LeftMouseDown events and never an Up; CGEvent::get_type
        // catches that.
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .expect("CGEventSource::new must succeed on the test host");
        let point = CGPoint::new(100.0, 100.0);
        match build_click_events(&source, point) {
            Ok((down, up)) => {
                // CGEventType in core-graphics 0.25 is `#[repr(u32)]` but
                // does NOT derive PartialEq, so cast to u32 to compare.
                // LeftMouseDown = 1, LeftMouseUp = 2 in CG's IOLLEvent
                // mapping (pinned via cast to make a future enum-renumber
                // a compiler error here).
                assert_eq!(
                    down.get_type() as u32,
                    CGEventType::LeftMouseDown as u32,
                    "first event must be LeftMouseDown"
                );
                assert_eq!(
                    up.get_type() as u32,
                    CGEventType::LeftMouseUp as u32,
                    "second event must be LeftMouseUp"
                );
            }
            Err(err) => panic!("build_click_events on plausible inputs must not fail: {err}"),
        }
    }

    #[test]
    fn build_click_events_succeeds_with_negative_coords() {
        // Per P3 spike: virtual displays produce negative CG screen
        // coordinates (e.g., RoK on BetterDisplay at x=-525). The
        // CGEvent constructor accepts them — pin so a future "validate
        // coords are non-negative" mistake gets caught at test time.
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .expect("CGEventSource::new must succeed on the test host");
        let point = CGPoint::new(-525.5, 513.0);
        if let Err(err) = build_click_events(&source, point) {
            panic!("build_click_events must accept negative virtual-display coords: {err}");
        }
    }

    #[test]
    fn click_failed_reason_constants_have_expected_values() {
        // The reason tags surface in operator-facing log lines AND in the
        // exit-code pin tests over in error::tests. Changing one of these
        // is a log contract change — pin the values here so a future
        // refactor is forced through both gates.
        assert_eq!(REASON_SOURCE, "source_creation_failed");
        assert_eq!(REASON_DOWN, "down_event_creation_failed");
        assert_eq!(REASON_UP, "up_event_creation_failed");
    }

    #[test]
    fn click_gap_ms_is_in_human_click_range() {
        // The module doc commits to staying inside the 50-150 ms human-click
        // range as cheap defence against bot-detection heuristics. A
        // regression dropping the gap to 0 or 5 ms would silently make the
        // pair look unmistakably synthetic. Pin the range so any out-of-band
        // edit forces a conscious decision.
        assert!(
            (50..=150).contains(&CLICK_GAP_MS),
            "CLICK_GAP_MS = {CLICK_GAP_MS} drifted out of the documented 50-150ms \
             human-click range; if the change is intentional update both this \
             pin and the module doc."
        );
    }

    #[test]
    fn click_failed_reasons_map_to_exit_18() {
        // Defense-in-depth: confirm each reason value our module emits is
        // accepted by BotError::ClickFailed and maps to the documented
        // exit code 18. The error::tests already pin this for fixed
        // string literals; this test pins it for the constants we'll
        // actually emit at runtime.
        for reason in [REASON_SOURCE, REASON_DOWN, REASON_UP] {
            let err = BotError::ClickFailed { reason };
            assert_eq!(err.exit_code(), 18, "reason {reason} must map to exit 18");
        }
    }
}
