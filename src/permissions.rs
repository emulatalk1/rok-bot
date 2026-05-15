//! macOS TCC permission preflight.
//!
//! Two surfaces:
//!
//! - **Screen Recording** — without it, `CGWindowListCopyWindowInfo` still
//!   returns RoK's windows but with `kCGWindowName` stripped to nil. Our
//!   owner+title filter would then silently miss the main window and return
//!   `WindowNotFound`, leaving the user staring at a misleading "is the game
//!   running?" error while the game is visibly running. The preflight catches
//!   this up front. Backed by `core-graphics`' `ScreenCaptureAccess` wrapper.
//!
//! - **Accessibility** — required for `CGEventPost` to deliver synthetic
//!   input. Without it, the post call silently no-ops (`CGEventPost` returns
//!   `()` and provides no error signal — the `BotError::ClickFailed`
//!   creation-time variant only catches `CGEvent::new_mouse_event` /
//!   `CGEventSource::new` failures, not delivery failures). The Accessibility
//!   FFI is hand-rolled here because `core-graphics` does not bind it and no
//!   maintained Rust crate exists as of 2026-05.
//!
//! Two-stage AX preflight pattern (design A11):
//!
//! - At boot, `peek_accessibility()` reports the current trusted state
//!   without prompting. If denied, `main.rs::run` logs a warn and proceeds —
//!   the bot may still produce useful capture+match output even without click.
//! - At click site, `check_accessibility()` *prompts* if denied (returns a
//!   typed `PermissionsMissing` error → exit 13). The prompt is asynchronous,
//!   so first-run UX is "prompt fires, exit 13, user grants in System
//!   Settings, user re-runs and it works" — NOT analogous to
//!   `ScreenCaptureAccess::request()` which blocks. This was Codex review
//!   CMT-2 correction during /plan-eng-review.

#![allow(
    unsafe_code,
    reason = "Accessibility FFI to ApplicationServices is unbound by core-graphics \
              and no maintained Rust crate covers it; the unsafe surface is contained \
              in this module."
)]

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::access::ScreenCaptureAccess;

use crate::cg_bootstrap::register_with_window_server;
use crate::error::{BotError, Result};
use crate::window::fetch_shareable_content;

/// Canonical label for the Screen Recording permission. Single source of truth
/// so the error variant, log lines, and tests can reference one constant
/// instead of literal "Screen Recording" strings scattered across files.
pub const SCREEN_RECORDING: &str = "Screen Recording";

/// Canonical label for the Accessibility permission. Same role as
/// `SCREEN_RECORDING` but for the AX surface. Used as the `which` tag in
/// `BotError::PermissionsMissing` (exit 13) when the click-site hard check
/// finds AX denied.
pub const ACCESSIBILITY: &str = "Accessibility";

/// `SCShareableContent.getShareableContentWithCompletionHandler`
/// returned nil or errored at boot. Mirrors `capture::STAGE_*` so the
/// `CaptureFailed` error surface stays uniform across the boot
/// preflight and the per-tick capture path. v0.1.8 introduced this
/// stage tag because SCK requires Screen Recording grant on the
/// rok-bot binary itself (not just its parent terminal); a TCC denial
/// surfaces here as a clear `no_shareable_content` error rather than
/// the v0.1.x's opaque "screencapture failed."
pub const STAGE_NO_SHAREABLE_CONTENT: &str = "no_shareable_content";

// Hand-rolled FFI to the Accessibility check.
//
// `AXIsProcessTrustedWithOptions` is a process-scoped check: macOS evaluates
// the calling process's code signature + bundle ID against the
// `com.apple.accessibility` TCC table. The `prompt` option (true/false)
// controls whether macOS shows a system dialog when not yet trusted; the
// dialog is asynchronous — the function returns immediately with the
// current trusted state regardless of user action.
//
// Returns macOS `Boolean` (= `unsigned char` = `u8`: 0 = denied, 1 = trusted).
// `kAXTrustedCheckOptionPrompt` is a `CFStringRef` constant exported by
// ApplicationServices; pairing it with `CFBoolean::true_value()` in the
// options dict toggles the prompt on.
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> u8;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

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

/// Pure: turn a trusted/denied bool into the AX hard-check result.
/// Exists for the same testability reason as `check_inner`: the live FFI is
/// not parameterizable, but the boolean → typed-error mapping is.
const fn check_accessibility_inner(trusted: bool) -> Result<()> {
    if trusted {
        Ok(())
    } else {
        Err(BotError::PermissionsMissing {
            which: ACCESSIBILITY,
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

/// Build the options dictionary `{ kAXTrustedCheckOptionPrompt: <prompt> }`.
/// Factored out so `peek_accessibility` (prompt=false) and
/// `check_accessibility` (prompt=true) share construction without diverging.
fn ax_options(prompt: bool) -> CFDictionary<CFString, CFBoolean> {
    // SAFETY: `kAXTrustedCheckOptionPrompt` is a CFStringRef constant exported
    // statically by ApplicationServices. `wrap_under_get_rule` follows the
    // CoreFoundation "Get rule" — it CFRetains internally so the resulting
    // CFString owns its retain. The constant pointer is non-null and stable
    // for the process lifetime.
    let prompt_key = unsafe { CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt) };
    let prompt_val = if prompt {
        CFBoolean::true_value()
    } else {
        CFBoolean::false_value()
    };
    CFDictionary::from_CFType_pairs(&[(prompt_key, prompt_val)])
}

/// Live AX trust check, no prompt. Returns `true` if the calling process
/// is currently trusted for Accessibility, `false` otherwise. Never side-
/// effects the system (no dialog, no TCC mutation).
///
/// Intended for the boot-time peek per design A11: log a warn if denied,
/// continue execution — the bot can still produce capture + match output
/// without click, and the click-site hard check fires when needed.
pub fn peek_accessibility() -> bool {
    let opts = ax_options(false);
    // SAFETY: `AXIsProcessTrustedWithOptions` accepts a `CFDictionaryRef` and
    // returns a `Boolean`. The dictionary outlives this call; the framework
    // does not retain past return. Returning `u8` (0 or 1) is per Apple's
    // documented signature.
    let trusted = unsafe { AXIsProcessTrustedWithOptions(opts.as_concrete_TypeRef()) };
    trusted != 0
}

/// SCK preflight: confirm the rok-bot binary can enumerate shareable
/// content via `SCShareableContent.getShareableContentWithCompletion
/// Handler`. v0.1.8 introduces this as a separate boot-time check
/// because the v0.1.x `check_screen_recording` only guarantees CG-
/// level Screen Recording trust — SCK needs the rok-bot binary
/// itself to be granted SR (the v0.1.x screencapture CLI inherited
/// TCC from its parent terminal; SCK doesn't).
///
/// Sequence:
/// 1. Bootstrap `WindowServer` registration via
///    [`register_with_window_server`] (T2 — every SCK entrypoint
///    must call this idempotently).
/// 2. Call [`fetch_shareable_content`] which parks on a dispatch
///    semaphore for up to 10 s. Slower than the per-tick capture
///    timeout because TCC prompts can stall the call until user
///    interaction.
/// 3. On nil/error/timeout, return `BotError::CaptureFailed { stage:
///    STAGE_NO_SHAREABLE_CONTENT }` (exit 14) with an actionable log
///    line that points at System Settings → Privacy & Security →
///    Screen Recording AND mentions the macOS 14+ requirement (the
///    other plausible cause when the call returns nil cleanly).
///
/// Called from `main.rs::run` immediately after `check_screen_recording`
/// — the CG-level check fails fast on missing TCC but doesn't
/// prove SCK can enumerate. A pass here means `find_rok_window`'s
/// first `SCShareableContent` fetch (which the cache amortizes for
/// subsequent calls) will succeed under nominal conditions.
pub fn check_sck_grant() -> Result<()> {
    register_with_window_server();
    if fetch_shareable_content().is_ok() {
        return Ok(());
    }
    tracing::error!(
        target: "rok_bot",
        "ScreenCaptureKit could not enumerate shareable content. Most likely \
         cause: Screen Recording not granted to the rok-bot binary itself. \
         Grant in System Settings → Privacy & Security → Screen Recording \
         (look for 'rok-bot' in the list; v0.1.8's per-binary grant is a UX \
         regression vs v0.1.x's terminal-inherited grant). Less common: \
         macOS pre-14.0 (SCK requires 14+)."
    );
    Err(BotError::CaptureFailed {
        stage: STAGE_NO_SHAREABLE_CONTENT,
        exit_code: None,
    })
}

/// Live AX trust check, **prompts** if denied. Returns `Ok(())` if trusted,
/// `Err(PermissionsMissing { which: "Accessibility" })` (exit 13) if denied.
///
/// Critical UX detail: the prompt is **asynchronous** — macOS shows the
/// dialog and `AXIsProcessTrustedWithOptions` returns immediately with the
/// current (still denied) trusted state. There is no analogue to
/// `ScreenCaptureAccess::request()`'s blocking behavior. First-run flow is:
///
///   1. Bot reaches click site, calls this fn.
///   2. macOS shows AX prompt (async); fn returns false.
///   3. Bot exits with code 13 + user-facing log instructing "grant in
///      System Settings, then re-run."
///   4. User grants Accessibility for the binary.
///   5. User re-runs; this fn returns Ok on the second invocation.
///
/// This is intentional: a blocking prompt would require pumping the AppKit
/// event loop, which would balloon the dependency surface for marginal UX.
pub fn check_accessibility() -> Result<()> {
    let opts = ax_options(true);
    // SAFETY: same contract as `peek_accessibility` — see that comment.
    let trusted = unsafe { AXIsProcessTrustedWithOptions(opts.as_concrete_TypeRef()) };
    check_accessibility_inner(trusted != 0)
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

    // ---------- Accessibility (v0.1.3) ----------

    #[test]
    fn ax_granted_returns_ok() {
        assert!(check_accessibility_inner(true).is_ok());
    }

    #[test]
    fn ax_denied_returns_permissions_missing_with_accessibility_label() {
        let err = check_accessibility_inner(false).unwrap_err();
        assert_eq!(
            err,
            BotError::PermissionsMissing {
                which: ACCESSIBILITY,
            }
        );
    }

    #[test]
    fn ax_denied_uses_permissions_missing_exit_code() {
        // Same exit code as Screen Recording denial — both are
        // PermissionsMissing variants. Operator branches on the `which`
        // tag in the log line, not on a per-permission exit code.
        let err = check_accessibility_inner(false).unwrap_err();
        assert_eq!(err.exit_code(), 13);
    }

    #[test]
    fn ax_denied_message_points_at_system_settings() {
        let err = check_accessibility_inner(false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Accessibility"), "msg: {msg}");
        assert!(
            msg.contains("System Settings"),
            "user-facing msg should name the macOS settings panel: {msg}"
        );
    }

    #[test]
    fn stage_no_shareable_content_pinned_to_documented_value() {
        // Shell users + log parsers pattern-match against this string
        // when SR is denied for the rok-bot binary at boot. Changing
        // it is a log contract change — pin so the change is
        // explicit.
        assert_eq!(STAGE_NO_SHAREABLE_CONTENT, "no_shareable_content");
    }

    #[test]
    fn permission_label_constants_match_expected_strings() {
        // Pin the exact label strings — they appear in the operator-facing
        // log lines, the `which` tag of `PermissionsMissing`, and (for
        // `Accessibility`) the System Settings panel name. A typo here
        // would silently break the operator's pattern-matching against
        // their muscle memory.
        assert_eq!(SCREEN_RECORDING, "Screen Recording");
        assert_eq!(ACCESSIBILITY, "Accessibility");
    }
}
