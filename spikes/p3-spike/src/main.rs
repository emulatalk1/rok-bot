//! P3 spike — Rust CGEvent + CGEventPost path verification.
//!
//! Architecture-level click delivery is already verified end-to-end by
//! p2-spike (Swift CGEventCreate + CGEventPost on RoK produced 6.1M
//! differing bytes between before/after captures). This spike is
//! narrower: prove the Rust `core-graphics` binding for the same APIs
//! produces the same observable effect from a standalone Rust binary,
//! when the binary's Accessibility grant is bootstrapped via hand-rolled
//! FFI to `AXIsProcessTrustedWithOptions(prompt=true)`.
//!
//! Run procedure: see `README.md`.

use std::env;
use std::process::ExitCode;
use std::thread::sleep;
use std::time::Duration;

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::string::{CFString, CFStringRef};

use core_graphics::event::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;

// Hand-rolled FFI to ApplicationServices' Accessibility surface. The
// `core-graphics` crate does not bind it, and there is no maintained
// crate as of 2026-05. This mirrors the pattern src/permissions.rs uses
// for ScreenCapture against CoreGraphics. In v0.1.3 production this
// moves into permissions.rs.
//
// AXIsProcessTrustedWithOptions returns macOS `Boolean` (= unsigned char,
// u8: 0 or 1). The kAXTrustedCheckOptionPrompt constant is a CFStringRef
// exported by the framework.
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> u8;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

const EXIT_OK: u8 = 0;
const EXIT_AX_DENIED: u8 = 13;
const EXIT_BAD_USAGE: u8 = 64;
const EXIT_CLICK_BUILD_FAILED: u8 = 18;

const CLICK_GAP_MS: u64 = 80;

fn main() -> ExitCode {
    let (x, y) = match parse_args() {
        Ok(c) => c,
        Err(e) => {
            print_usage(&e);
            return ExitCode::from(EXIT_BAD_USAGE);
        }
    };

    eprintln!("[p3-spike] checking Accessibility (will prompt if not yet granted)...");
    if !check_accessibility_with_prompt() {
        eprintln!(
            "[p3-spike] Accessibility denied. macOS may have just shown a prompt; \
             grant the spike binary Accessibility in System Settings → Privacy & \
             Security → Accessibility, then re-run. The prompt is asynchronous, \
             so this single-run exit-then-grant-then-rerun cycle is expected on \
             first use."
        );
        return ExitCode::from(EXIT_AX_DENIED);
    }
    eprintln!("[p3-spike] Accessibility granted.");

    eprintln!(
        "[p3-spike] posting click pair at ({x}, {y}) via CGEventPost(HID, ...) \
         with {CLICK_GAP_MS}ms gap"
    );
    if post_click(x, y).is_err() {
        eprintln!(
            "[p3-spike] CGEvent construction failed (CGEventSource::new or \
             CGEvent::new_mouse_event returned Err)."
        );
        return ExitCode::from(EXIT_CLICK_BUILD_FAILED);
    }

    eprintln!("[p3-spike] click posted at ({x}, {y}) — observe RoK");
    ExitCode::from(EXIT_OK)
}

fn print_usage(err: &str) {
    eprintln!("usage: p3-spike --x <f64> --y <f64>");
    eprintln!("       (CG global-screen coordinates, top-left origin, points)");
    eprintln!("error: {err}");
}

fn parse_args() -> Result<(f64, f64), String> {
    let mut x: Option<f64> = None;
    let mut y: Option<f64> = None;
    let mut args = env::args().skip(1);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--x" => {
                let v = args.next().ok_or_else(|| "--x missing value".to_string())?;
                x = Some(v.parse().map_err(|e| format!("--x: {e}"))?);
            }
            "--y" => {
                let v = args.next().ok_or_else(|| "--y missing value".to_string())?;
                y = Some(v.parse().map_err(|e| format!("--y: {e}"))?);
            }
            "-h" | "--help" => return Err("help requested".to_string()),
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    let x = x.ok_or_else(|| "--x required".to_string())?;
    let y = y.ok_or_else(|| "--y required".to_string())?;
    Ok((x, y))
}

fn check_accessibility_with_prompt() -> bool {
    let prompt_key = unsafe { CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt) };
    let options = CFDictionary::from_CFType_pairs(&[(prompt_key, CFBoolean::true_value())]);

    // SAFETY: AXIsProcessTrustedWithOptions accepts a CFDictionaryRef and
    // returns a Boolean (u8). The CFDictionary outlives the call, so the
    // ref is valid; the framework does not retain past return.
    let trusted = unsafe { AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef()) };
    trusted != 0
}

fn post_click(x: f64, y: f64) -> Result<(), ()> {
    // Two distinct CGEventSource instances rather than relying on Clone;
    // CGEventSource creation is cheap and avoids assuming Clone semantics
    // on a CFType wrapper.
    let down_src = CGEventSource::new(CGEventSourceStateID::HIDSystemState)?;
    let up_src = CGEventSource::new(CGEventSourceStateID::HIDSystemState)?;

    let pt = CGPoint::new(x, y);
    let down = CGEvent::new_mouse_event(
        down_src,
        CGEventType::LeftMouseDown,
        pt,
        CGMouseButton::Left,
    )?;
    let up = CGEvent::new_mouse_event(up_src, CGEventType::LeftMouseUp, pt, CGMouseButton::Left)?;

    down.post(CGEventTapLocation::HID);
    sleep(Duration::from_millis(CLICK_GAP_MS));
    up.post(CGEventTapLocation::HID);
    Ok(())
}
