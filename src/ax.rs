//! macOS Accessibility API click delivery.
//!
//! v0.1.5 routes synthetic clicks through `AXUIElementPerformAction
//! (kAXPressAction)` instead of v0.1.3-4's `CGEventPost(kCGHIDEventTap, ...)`.
//! The switch is forced by p4-spike (2026-05-11) which proved
//! `CGEventPostToPid` is dropped silently by the AppKit→UIKit translation
//! layer for Catalyst Bridge apps like RoK, and validated by p5-spike
//! which proved AX press DOES deliver to those same apps regardless of
//! z-order overlap at the click point (see
//! `learnings/ax-press-works-catalyst`).
//!
//! Flow (all errors map to `BotError::ClickFailed`, exit 18):
//!
//!   1. `AXUIElementCreateApplication(window.pid)` — resolves a process-
//!      scoped AX root. Returns null when AX permission is denied or PID
//!      is stale. → `REASON_AX_APP_RESOLVE_FAILED`.
//!   2. `AXUIElementSetMessagingTimeout(app, AX_MESSAGING_TIMEOUT_SECONDS)`
//!      — caps AX RPC blocking on a hung target. Non-fatal if it fails
//!      (default timeout applies); logged at warn but not surfaced as
//!      `ClickFailed`.
//!   3. `AXUIElementCopyElementAtPosition(app, x, y, &out_element)` —
//!      walks the AX hierarchy to find the leaf at `(x, y)` in CG global-
//!      screen coords. Returns `AXError`; `kAXErrorCannotComplete (-25204)`
//!      surfaces as `REASON_AX_TIMEOUT`, anything else as
//!      `REASON_AX_ELEMENT_RESOLVE_FAILED`.
//!   4. `AXUIElementPerformAction(element, kAXPressAction)` — dispatches
//!      the press to the resolved element. Same error mapping:
//!      `kAXErrorCannotComplete` → `REASON_AX_TIMEOUT`, else
//!      `REASON_AX_PRESS_FAILED`. `kAXErrorFailure (-25200)` is the
//!      common operator-facing failure on hidden Spaces — the v0.1.5
//!      pre-click `validate_at_click_site` catches that earlier (exit
//!      19 `REASON_NOT_VISIBLE`), so seeing it here means the Space
//!      flipped after validation, which is rare.
//!
//! ## FFI argument types
//!
//! `AXUIElementCopyElementAtPosition` and `AXUIElementSetMessagingTimeout`
//! take `float` (C 32-bit float = `f32` in Rust), NOT `CGFloat` (which is
//! `double` = `f64` on 64-bit macOS). Verified against Apple SDK header
//! `AXUIElement.h` on 2026-05-11:
//!
//! ```c
//! extern AXError AXUIElementCopyElementAtPosition(AXUIElementRef application,
//!                                                 float x, float y,
//!                                                 AXUIElementRef *element);
//! extern AXError AXUIElementSetMessagingTimeout(AXUIElementRef element,
//!                                               float timeoutInSeconds);
//! ```
//!
//! The locked plan from `/plan-eng-review` (codex outside-voice review,
//! 2026-05-11) claimed both should be `f64`/`CGFloat`. That was a codex
//! confusion between AX (uses `float`) and CGEvent / `CGPoint` (uses
//! `CGFloat`). p5-spike's `f32` was correct empirically; this module
//! matches Apple's actual signature. Learning logged as
//! `ax-ffi-uses-float-not-cgfloat` (supersedes `ax-ffi-cgfloat-is-f64`).
//!
//! Sub-pixel precision loss from `f64 → f32` is theoretical: AX
//! coordinates are integer-pixel by convention, and even sub-pixel
//! drift well under 0.001pt won't reach the AX layer's behavioral
//! boundary.
//!
//! ## RAII
//!
//! `AXUIElementRef` is a Core Foundation opaque type. Both
//! `AXUIElementCreateApplication` and `AXUIElementCopyElementAtPosition`
//! return "Create"-rule references — caller owns the retain and must
//! `CFRelease` when done. We wrap each in `AxRef`, whose `Drop` impl
//! calls `CFRelease` on a non-null inner pointer. This eliminates the
//! manual `CFRelease` pairs that p5-spike carried (where the press call
//! site had to remember to release both `app` and `element` on every
//! exit path).

#![allow(
    unsafe_code,
    reason = "macOS Accessibility API is C FFI; the unsafe surface is contained \
              in this module and gated by RAII (AxRef) for CFRelease."
)]

use std::ffi::c_void;
use std::ptr;

use core_foundation::base::TCFType;
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::display::CGPoint;

use crate::error::{BotError, Result};
use crate::window::Window;

/// AX RPC timeout, in seconds. Caps how long a single AX call (element
/// resolution, action dispatch) can block on a hung target before the
/// AX framework synthesizes `kAXErrorCannotComplete (-25204)`. 2.0s is
/// the codex-outside-voice recommendation from /plan-eng-review: long
/// enough that a busy-but-not-hung RoK (mid-loading-screen, server-bound
/// click) doesn't false-positive, short enough that a truly hung target
/// surfaces a clean error instead of leaving the bot wedged. Calibrate
/// if the noise floor shifts — TODOS.md P3 carries the v0.1.5+ entry.
///
/// `f32` matches the Apple header signature (see module docs).
pub const AX_MESSAGING_TIMEOUT_SECONDS: f32 = 2.0;

/// `AXError` value for success. Apple's framework uses `0` as the
/// uniform success code across all `AXUIElement*` functions; non-zero
/// values are negative codes from the `kAXError*` enum.
const AX_ERROR_SUCCESS: i32 = 0;

/// `kAXErrorCannotComplete` from `AXError.h`. Returned by AX framework
/// when the target process couldn't respond within the messaging
/// timeout window. We treat this as a distinct reason (`REASON_AX_TIMEOUT`)
/// because the operator's diagnostic path is different from a generic
/// element/press failure: timeouts usually mean RoK is busy or pinned
/// by another process, not that the AX permission is wrong or the
/// element is non-pressable.
const AX_ERROR_CANNOT_COMPLETE: i32 = -25204;

/// Reason tags surfaced via `BotError::ClickFailed { reason }`. Each
/// tag corresponds to a distinct AX failure mode in [`press_at`]; the
/// strings appear in operator-facing log lines, so renaming one is a
/// log-contract change (pin both here and in `error.rs`'s exit-code
/// test).
pub const REASON_AX_APP_RESOLVE_FAILED: &str = "ax_app_resolve_failed";
pub const REASON_AX_ELEMENT_RESOLVE_FAILED: &str = "ax_element_resolve_failed";
pub const REASON_AX_PRESS_FAILED: &str = "ax_press_failed";
pub const REASON_AX_TIMEOUT: &str = "ax_timeout";

/// RAII wrapper around an `AXUIElementRef` (or any "Create"-rule
/// `CFTypeRef` from the AX framework). Calls `CFRelease` on a non-null
/// inner pointer when dropped. A separate `null_taken` flag isn't
/// needed because the AX functions return null on failure and we never
/// transfer ownership out of an `AxRef` — `Drop` is always the release
/// path.
struct AxRef(*mut c_void);

impl Drop for AxRef {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: AX FFI returns "Create"-rule references that the
            // caller owns. `AxRef` is constructed only from such
            // references (via `AXUIElementCreateApplication` and the
            // out-param of `AXUIElementCopyElementAtPosition`), and is
            // not Clone/Copy, so this is the unique owner.
            unsafe {
                CFRelease(self.0);
            }
        }
    }
}

/// Deliver a single press to the AX element at `point`, scoped to the
/// running process that owns `window.pid`.
///
/// `&Window` is taken (not just `(pid, point)`) so future refactors can
/// re-verify the window state inside this function with no signature
/// change — the type carries the (WID, PID, frame) facts that the
/// caller's pre-click validation already proved.
///
/// Errors are described in the module docs; all map to `BotError::
/// ClickFailed`, exit 18. AX permission must already be granted (the
/// preflight is `permissions::check_accessibility`'s job, run by
/// `main.rs` before calling here).
pub fn press_at(window: &Window, point: CGPoint) -> Result<()> {
    // Step 1: app-level AX element. Apple's docs say
    // `AXUIElementCreateApplication` does not validate that the PID
    // refers to a running, AX-trusted process — it just returns an
    // opaque ref. A stale PID is detected later when
    // `CopyElementAtPosition` returns `kAXErrorInvalidUIElement`.
    // We only fail here if the ref itself is null (AX framework refused
    // to allocate, or pid is structurally invalid).
    // SAFETY: pid_t is i32 on macOS. The returned ref is "Create"-rule;
    // wrapping in AxRef makes Drop responsible for CFRelease.
    let app_ptr = unsafe { AXUIElementCreateApplication(window.pid) };
    if app_ptr.is_null() {
        tracing::warn!(
            target: "rok_bot",
            pid = window.pid,
            "AXUIElementCreateApplication returned null"
        );
        return Err(BotError::ClickFailed {
            reason: REASON_AX_APP_RESOLVE_FAILED,
        });
    }
    let app = AxRef(app_ptr);

    // Step 2: cap AX RPC blocking. Non-fatal — if SetMessagingTimeout
    // fails, the framework default (~6s) applies. Log the AXError but
    // don't bail; the rest of the flow still has its own per-call
    // failure paths.
    // SAFETY: app.0 is non-null (just checked). Timeout is positive.
    let timeout_err =
        unsafe { AXUIElementSetMessagingTimeout(app.0, AX_MESSAGING_TIMEOUT_SECONDS) };
    if timeout_err != AX_ERROR_SUCCESS {
        tracing::warn!(
            target: "rok_bot",
            ax_error = timeout_err,
            ax_error_label = ax_error_label(timeout_err),
            timeout_seconds = AX_MESSAGING_TIMEOUT_SECONDS,
            "AXUIElementSetMessagingTimeout failed; default timeout applies"
        );
    }

    // Step 3: resolve element at point. Coordinates are AX-convention
    // top-left-origin CG global-screen coords, which is what
    // `matcher::screen_point` already produces.
    let mut element_ptr: *mut c_void = ptr::null_mut();
    // SAFETY: app.0 is non-null. Element out-param starts null; AX
    // framework writes a "Create"-rule ref on success. x/y are f32 to
    // match Apple's `float` signature (see module docs).
    let resolve_err = unsafe {
        AXUIElementCopyElementAtPosition(
            app.0,
            point.x as f32,
            point.y as f32,
            &raw mut element_ptr,
        )
    };
    if resolve_err != AX_ERROR_SUCCESS {
        let reason = map_ax_error(resolve_err, REASON_AX_ELEMENT_RESOLVE_FAILED);
        tracing::warn!(
            target: "rok_bot",
            ax_error = resolve_err,
            ax_error_label = ax_error_label(resolve_err),
            x = point.x,
            y = point.y,
            "AXUIElementCopyElementAtPosition failed"
        );
        return Err(BotError::ClickFailed { reason });
    }
    if element_ptr.is_null() {
        // AX framework returned Success but null element — defensive
        // guard. Apple's docs don't explicitly allow this, but a
        // null-success would be silently UB-adjacent on press.
        tracing::warn!(
            target: "rok_bot",
            x = point.x,
            y = point.y,
            "AXUIElementCopyElementAtPosition returned Success but null element"
        );
        return Err(BotError::ClickFailed {
            reason: REASON_AX_ELEMENT_RESOLVE_FAILED,
        });
    }
    let element = AxRef(element_ptr);

    // Step 4: press. The `kAXPressAction` C constant's value is the
    // string `"AXPress"` (verified against AXActionConstants.h).
    // CFString from a static lifetime literal is the canonical safe
    // construction; no manual release needed (Rust drop covers it).
    let press_action = CFString::from_static_string("AXPress");
    // SAFETY: element.0 is non-null. The CFString outlives the call
    // (it's bound to `press_action` for the rest of the scope).
    let press_err =
        unsafe { AXUIElementPerformAction(element.0, press_action.as_concrete_TypeRef()) };
    if press_err != AX_ERROR_SUCCESS {
        let reason = map_ax_error(press_err, REASON_AX_PRESS_FAILED);
        tracing::warn!(
            target: "rok_bot",
            ax_error = press_err,
            ax_error_label = ax_error_label(press_err),
            x = point.x,
            y = point.y,
            "AXUIElementPerformAction(AXPress) failed"
        );
        return Err(BotError::ClickFailed { reason });
    }

    tracing::info!(
        target: "rok_bot",
        pid = window.pid,
        window_id = window.id,
        x = point.x,
        y = point.y,
        "AX press dispatched to RoK"
    );
    Ok(())
}

/// Pure: pick the right `REASON_*` for an `AXError` code. Timeout
/// codes get the dedicated `REASON_AX_TIMEOUT`; everything else falls
/// through to the call-site-specific `fallback` (e.g.,
/// `REASON_AX_ELEMENT_RESOLVE_FAILED` or `REASON_AX_PRESS_FAILED`).
/// Split out for unit testability — exercising `press_at` end-to-end
/// requires AX permission and a live target, which the test runner
/// doesn't have.
const fn map_ax_error(err: i32, fallback: &'static str) -> &'static str {
    if err == AX_ERROR_CANNOT_COMPLETE {
        REASON_AX_TIMEOUT
    } else {
        fallback
    }
}

/// Pure: human-readable label for an `AXError` code. Mirrors p5-spike's
/// `ax_error_label` (lifted from AX framework headers) so the operator
/// sees `"InvalidUIElement"` rather than just `-25202` in log lines.
/// Exhaustive coverage of the public `AXError` enum values lets a
/// future grep on the log find the symbolic name.
pub const fn ax_error_label(code: i32) -> &'static str {
    match code {
        0 => "Success",
        -25200 => "Failure",
        -25201 => "IllegalArgument",
        -25202 => "InvalidUIElement",
        -25203 => "InvalidUIElementObserver",
        -25204 => "CannotComplete",
        -25205 => "AttributeUnsupported",
        -25206 => "ActionUnsupported",
        -25207 => "NotificationUnsupported",
        -25208 => "NotImplemented",
        -25209 => "NotificationAlreadyRegistered",
        -25210 => "NotificationNotRegistered",
        -25211 => "APIDisabled",
        -25212 => "NoValue",
        -25213 => "ParameterizedAttributeUnsupported",
        -25214 => "NotEnoughPrecision",
        _ => "unknown",
    }
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> *mut c_void;
    fn AXUIElementSetMessagingTimeout(element: *mut c_void, timeout: f32) -> i32;
    fn AXUIElementCopyElementAtPosition(
        application: *mut c_void,
        x: f32,
        y: f32,
        element: *mut *mut c_void,
    ) -> i32;
    fn AXUIElementPerformAction(element: *mut c_void, action: CFStringRef) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *mut c_void);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ax_error_label_covers_documented_codes() {
        // Pin the AXError code → label map. Operator-facing log lines
        // reference these strings; a future drift between code and label
        // would mislead the operator at the worst time (diagnosing a
        // failed live run). Pin only the codes documented in
        // AXError.h (Apple's stable public API surface).
        assert_eq!(ax_error_label(0), "Success");
        assert_eq!(ax_error_label(-25200), "Failure");
        assert_eq!(ax_error_label(-25201), "IllegalArgument");
        assert_eq!(ax_error_label(-25202), "InvalidUIElement");
        assert_eq!(ax_error_label(-25203), "InvalidUIElementObserver");
        assert_eq!(ax_error_label(-25204), "CannotComplete");
        assert_eq!(ax_error_label(-25205), "AttributeUnsupported");
        assert_eq!(ax_error_label(-25206), "ActionUnsupported");
        assert_eq!(ax_error_label(-25207), "NotificationUnsupported");
        assert_eq!(ax_error_label(-25208), "NotImplemented");
        assert_eq!(ax_error_label(-25209), "NotificationAlreadyRegistered");
        assert_eq!(ax_error_label(-25210), "NotificationNotRegistered");
        assert_eq!(ax_error_label(-25211), "APIDisabled");
        assert_eq!(ax_error_label(-25212), "NoValue");
        assert_eq!(ax_error_label(-25213), "ParameterizedAttributeUnsupported");
        assert_eq!(ax_error_label(-25214), "NotEnoughPrecision");
    }

    #[test]
    fn ax_error_label_unknown_for_undocumented_codes() {
        // A future macOS release adding a new AXError code shouldn't
        // crash or panic; "unknown" surfaces the numeric code in the
        // adjacent log field while still letting the operator know
        // the framework returned something outside our pin set.
        assert_eq!(ax_error_label(1), "unknown");
        assert_eq!(ax_error_label(-99999), "unknown");
        assert_eq!(ax_error_label(i32::MAX), "unknown");
        assert_eq!(ax_error_label(i32::MIN), "unknown");
    }

    #[test]
    fn map_ax_error_timeout_overrides_fallback() {
        // kAXErrorCannotComplete (-25204) is the canonical macOS AX
        // timeout signal. press_at's element-resolve and press-action
        // paths both call map_ax_error to route this code to the
        // operator-actionable REASON_AX_TIMEOUT, not the generic
        // call-site reason. Pin the override so a future reorder of
        // the match arms can't accidentally drop the timeout case.
        assert_eq!(
            map_ax_error(AX_ERROR_CANNOT_COMPLETE, REASON_AX_ELEMENT_RESOLVE_FAILED),
            REASON_AX_TIMEOUT
        );
        assert_eq!(
            map_ax_error(AX_ERROR_CANNOT_COMPLETE, REASON_AX_PRESS_FAILED),
            REASON_AX_TIMEOUT
        );
    }

    #[test]
    fn map_ax_error_falls_through_for_other_codes() {
        // Anything that isn't the timeout code falls through to the
        // call-site fallback. The fallback is the caller's
        // responsibility to set correctly (element-resolve site uses
        // RESOLVE_FAILED, press site uses PRESS_FAILED); map_ax_error
        // just passes it through unchanged.
        assert_eq!(
            map_ax_error(-25200, REASON_AX_PRESS_FAILED),
            REASON_AX_PRESS_FAILED,
            "kAXErrorFailure must fall through to caller's fallback"
        );
        assert_eq!(
            map_ax_error(-25206, REASON_AX_PRESS_FAILED),
            REASON_AX_PRESS_FAILED,
            "kAXErrorActionUnsupported must fall through"
        );
        assert_eq!(
            map_ax_error(-25202, REASON_AX_ELEMENT_RESOLVE_FAILED),
            REASON_AX_ELEMENT_RESOLVE_FAILED,
            "kAXErrorInvalidUIElement must fall through"
        );
        assert_eq!(
            map_ax_error(0, REASON_AX_PRESS_FAILED),
            REASON_AX_PRESS_FAILED,
            "Success code through map_ax_error returns fallback (caller \
             should not have called map_ax_error on success — pin defensive \
             behavior)"
        );
    }

    #[test]
    fn ax_reason_constants_match_expected_strings() {
        // Pin the operator-facing log strings. Each constant maps to a
        // distinct AX failure mode in press_at; the strings surface in
        // ClickFailed reason fields and in any external log-grep'ing
        // an operator might do. Changing one breaks shell users
        // pattern-matching on the error message.
        assert_eq!(REASON_AX_APP_RESOLVE_FAILED, "ax_app_resolve_failed");
        assert_eq!(
            REASON_AX_ELEMENT_RESOLVE_FAILED,
            "ax_element_resolve_failed"
        );
        assert_eq!(REASON_AX_PRESS_FAILED, "ax_press_failed");
        assert_eq!(REASON_AX_TIMEOUT, "ax_timeout");
    }

    #[test]
    fn ax_reasons_map_to_click_failed_exit_18() {
        // Defense-in-depth pin: every AX reason this module emits at
        // runtime must produce BotError::ClickFailed with exit 18.
        // The error::tests file pins the exit code via literal
        // strings; this pin closes the gap by exercising the actual
        // constants we'll use, in case a future refactor splits AX
        // into its own BotError variant.
        for reason in [
            REASON_AX_APP_RESOLVE_FAILED,
            REASON_AX_ELEMENT_RESOLVE_FAILED,
            REASON_AX_PRESS_FAILED,
            REASON_AX_TIMEOUT,
        ] {
            let err = BotError::ClickFailed { reason };
            assert_eq!(err.exit_code(), 18, "reason {reason} must map to exit 18");
        }
    }

    #[test]
    fn ax_messaging_timeout_is_in_practical_range() {
        // The module commits to ~2s as the AX RPC timeout — long enough
        // to absorb RoK's render hiccups, short enough to surface a
        // truly hung target. A regression dropping to 0.0 would make
        // every AX call false-fail as CannotComplete; a regression
        // raising to 60.0 would leave the bot wedged on a real hang.
        // Pin the range so any out-of-band edit forces a conscious
        // decision (TODOS P3 carries the calibration ticket).
        assert!(
            (0.5..=10.0).contains(&AX_MESSAGING_TIMEOUT_SECONDS),
            "AX_MESSAGING_TIMEOUT_SECONDS = {AX_MESSAGING_TIMEOUT_SECONDS}s drifted \
             out of the 0.5-10s practical range; if intentional, update both \
             this pin and the constant doc."
        );
    }
}
