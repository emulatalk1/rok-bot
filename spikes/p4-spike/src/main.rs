//! P4 spike — verify `CGEventPostToPid` delivers clicks to RoK
//! (iOS-on-Mac Catalyst Bridge) regardless of window stacking.
//!
//! See `README.md` for the contract, prerequisites, and decision branches.
//!
//! Two modes, selected by an optional flag:
//!
//!   p4-spike <pid> <x> <y>         (experiment) post_to_pid path
//!   p4-spike <pid> <x> <y> --hid   (control)    HID-tap path
//!
//! Coordinates are in CG global-screen points (same space main.rs uses
//! after `screen_point` translation). No window discovery is done here —
//! caller passes pid and coords explicitly so the spike stays minimal.
//!
//! Accessibility is bootstrapped via hand-rolled FFI to
//! `AXIsProcessTrustedWithOptions(prompt=true)`, identical to p3-spike.
//! TCC is per-binary, so first run will fire the system prompt and exit 13;
//! grant the spike binary in System Settings > Privacy & Security >
//! Accessibility, then re-run.

use std::env;
use std::process::ExitCode;
use std::thread::sleep;
use std::time::Duration;

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::string::{CFString, CFStringRef};

use core_graphics::display::CGPoint;
use core_graphics::event::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

// Hand-rolled FFI to ApplicationServices, same pattern as p3-spike and
// src/permissions.rs in the main crate.
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> u8;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

const CLICK_GAP_MS: u64 = 80;

fn usage(progname: &str) -> ExitCode {
    eprintln!("usage: {progname} <pid> <screen_x> <screen_y> [--hid]");
    eprintln!();
    eprintln!("  default:  CGEvent::post_to_pid(pid)         (experiment)");
    eprintln!("  --hid:    CGEvent::post(HID tap location)   (control = current v0.1.3 path)");
    ExitCode::from(2)
}

fn check_accessibility_with_prompt() -> bool {
    let prompt_key = unsafe { CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt) };
    let options = CFDictionary::from_CFType_pairs(&[(prompt_key, CFBoolean::true_value())]);
    // SAFETY: AXIsProcessTrustedWithOptions takes CFDictionaryRef, returns
    // Boolean (u8). The CFDictionary outlives the call.
    let trusted = unsafe { AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef()) };
    trusted != 0
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let progname = args.first().map_or("p4-spike", String::as_str);
    if args.len() < 4 {
        return usage(progname);
    }
    let Ok(pid) = args[1].parse::<i32>() else {
        eprintln!("[ERROR] pid must be a positive integer");
        return usage(progname);
    };
    let Ok(x) = args[2].parse::<f64>() else {
        eprintln!("[ERROR] screen_x must be a float (e.g., 461.0)");
        return usage(progname);
    };
    let Ok(y) = args[3].parse::<f64>() else {
        eprintln!("[ERROR] screen_y must be a float (e.g., 833.5)");
        return usage(progname);
    };
    let use_hid = args.iter().any(|a| a == "--hid");

    eprintln!("[p4-spike] checking Accessibility (will prompt if not yet granted)...");
    if !check_accessibility_with_prompt() {
        eprintln!(
            "[p4-spike] Accessibility denied. macOS may have just shown a \
             prompt; grant the spike binary Accessibility in System Settings \
             > Privacy & Security > Accessibility, then re-run. The prompt is \
             asynchronous, so first-run exit-then-grant-then-rerun is expected."
        );
        return ExitCode::from(13);
    }
    eprintln!("[p4-spike] Accessibility granted.");

    let Ok(source) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        eprintln!("[ERROR] CGEventSource::new(HIDSystemState) returned Err");
        return ExitCode::from(13);
    };
    let point = CGPoint::new(x, y);
    let Ok(down) = CGEvent::new_mouse_event(
        source.clone(),
        CGEventType::LeftMouseDown,
        point,
        CGMouseButton::Left,
    ) else {
        eprintln!("[ERROR] CGEvent::new_mouse_event(LeftMouseDown) returned Err");
        return ExitCode::from(18);
    };
    let Ok(up) = CGEvent::new_mouse_event(
        source,
        CGEventType::LeftMouseUp,
        point,
        CGMouseButton::Left,
    ) else {
        eprintln!("[ERROR] CGEvent::new_mouse_event(LeftMouseUp) returned Err");
        return ExitCode::from(18);
    };

    if use_hid {
        eprintln!(
            "[p4-spike] CONTROL — posting to HID tap (v0.1.3 path). \
             pid={pid} @ ({x}, {y})"
        );
        down.post(CGEventTapLocation::HID);
        sleep(Duration::from_millis(CLICK_GAP_MS));
        up.post(CGEventTapLocation::HID);
    } else {
        eprintln!(
            "[p4-spike] EXPERIMENT — posting to pid {pid} directly @ ({x}, {y})"
        );
        down.post_to_pid(pid);
        sleep(Duration::from_millis(CLICK_GAP_MS));
        up.post_to_pid(pid);
    }
    eprintln!("[p4-spike] click pair posted. Eyeball RoK for visible response.");
    ExitCode::SUCCESS
}
