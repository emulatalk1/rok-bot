//! P5 spike — verify `AXUIElementPerformAction(kAXPressAction)` delivers
//! clicks to RoK (iOS-on-Mac Catalyst Bridge) regardless of window
//! z-order at the click point.
//!
//! See `README.md` for the contract, prerequisites, and decision branches.
//!
//! Four modes:
//!
//!   p5-spike <pid> <x> <y>            (experiment)    AX press at (x, y) within app
//!   p5-spike <pid> <x> <y> --hid      (control)       HID-tap (same as p4-spike --hid)
//!   p5-spike <pid> <x> <y> --noop     (baseline)      no click; accessibility check + exit 0
//!   p5-spike <pid> <x> <y> --inspect  (introspection) read-only: AXRole/Subrole/Title/Identifier
//!                                                     and AXActionNames for the element at (x, y)
//!
//! Coordinates are CG global-screen points (top-left origin). Matches the
//! `screen_point` translation main.rs already produces and the convention
//! AX docs use for `AXUIElementCopyElementAtPosition`.
//!
//! Why this spike: p4-spike confirmed `CGEventPostToPid` is dead for
//! Catalyst Bridge apps (silently dropped in the AppKit→UIKit translation
//! layer). The macOS Accessibility API is the next candidate for
//! "screen-position-independent click delivery" because Apple's
//! [Accessibility design for Mac Catalyst][1] explicitly states UIKit
//! accessibility auto-bridges to macOS Accessibility. Hammerspoon,
//! AXorcist, and DFAXUIElement all use this path in production.
//!
//! [1]: https://developer.apple.com/documentation/accessibility/accessibility_design_for_mac_catalyst

use std::env;
use std::ffi::c_void;
use std::process::ExitCode;
use std::ptr;
use std::thread::sleep;
use std::time::Duration;

use core_foundation::array::CFArrayRef;
use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::string::{CFString, CFStringRef};

use core_graphics::display::CGPoint;
use core_graphics::event::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

// Hand-rolled FFI to ApplicationServices. AXUIElementRef is an opaque
// CFType (typedef of `__AXUIElement *`). AXError is `int32_t`.
//
// We treat AXUIElementRef as `*mut c_void` and release via CFRelease
// because the official return semantics are "Create"-rule: caller owns
// the reference and must release. Same pattern as p3/p4 spikes for
// AXIsProcessTrustedWithOptions.
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> u8;
    static kAXTrustedCheckOptionPrompt: CFStringRef;

    fn AXUIElementCreateApplication(pid: i32) -> *mut c_void;
    fn AXUIElementCopyElementAtPosition(
        application: *mut c_void,
        x: f32,
        y: f32,
        element: *mut *mut c_void,
    ) -> i32;
    fn AXUIElementPerformAction(element: *mut c_void, action: CFStringRef) -> i32;

    // Introspection: attribute reader + action enumerator.
    // Both return AXError. `value` and `names` are "Create"-rule out-params,
    // caller releases via CFRelease.
    fn AXUIElementCopyAttributeValue(
        element: *mut c_void,
        attribute: CFStringRef,
        value: *mut *const c_void,
    ) -> i32;
    fn AXUIElementCopyActionNames(element: *mut c_void, names: *mut CFArrayRef) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *mut c_void);
    fn CFArrayGetCount(array: CFArrayRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CFArrayRef, index: isize) -> *const c_void;
    fn CFGetTypeID(cf: *const c_void) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFCopyDescription(cf: *const c_void) -> CFStringRef;
}

const CLICK_GAP_MS: u64 = 80;
const AX_ERROR_SUCCESS: i32 = 0;

fn usage(progname: &str) -> ExitCode {
    eprintln!("usage: {progname} <pid> <screen_x> <screen_y> [--hid|--noop|--inspect]");
    eprintln!();
    eprintln!(
        "  default:   AXUIElementPerformAction(kAXPressAction) at (x, y) within app  [experiment]"
    );
    eprintln!(
        "  --hid:     CGEvent::post(HID tap location)                                  [control = v0.1.4 path]"
    );
    eprintln!(
        "  --noop:    no click posted; AX preflight only                               [baseline]"
    );
    eprintln!(
        "  --inspect: read-only AX: AXRole/Subrole/Title/Identifier + AXActionNames    [diagnostic]"
    );
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

fn post_hid_click(x: f64, y: f64) -> ExitCode {
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
    let Ok(up) =
        CGEvent::new_mouse_event(source, CGEventType::LeftMouseUp, point, CGMouseButton::Left)
    else {
        eprintln!("[ERROR] CGEvent::new_mouse_event(LeftMouseUp) returned Err");
        return ExitCode::from(18);
    };
    eprintln!("[p5-spike] CONTROL — posting to HID tap (v0.1.4 path) @ ({x}, {y})");
    down.post(CGEventTapLocation::HID);
    sleep(Duration::from_millis(CLICK_GAP_MS));
    up.post(CGEventTapLocation::HID);
    eprintln!("[p5-spike] HID click pair posted.");
    ExitCode::SUCCESS
}

fn ax_error_label(code: i32) -> &'static str {
    // AXError codes from the macOS AX framework (AXError.h). Spike includes
    // the names inline so the operator doesn't need to look them up while
    // diagnosing a failed run.
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

/// Resolve the AX element at (x, y) within `pid`'s hierarchy. Returns the
/// (app_ref, element_ref) pair on success. Caller releases both via CFRelease.
///
/// Shared by `post_ax_press` and `inspect_element`. Pulled into its own
/// function so the introspection path doesn't duplicate the resolution
/// boilerplate.
fn resolve_element_at(pid: i32, x: f64, y: f64) -> Result<(*mut c_void, *mut c_void), u8> {
    // SAFETY: AXUIElementCreateApplication returns a "Create"-rule CFTypeRef.
    let app: *mut c_void = unsafe { AXUIElementCreateApplication(pid) };
    if app.is_null() {
        eprintln!("[ERROR] AXUIElementCreateApplication returned null for pid {pid}");
        return Err(18);
    }
    let mut element: *mut c_void = ptr::null_mut();
    let err = unsafe { AXUIElementCopyElementAtPosition(app, x as f32, y as f32, &mut element) };
    if err != AX_ERROR_SUCCESS {
        eprintln!(
            "[ERROR] AXUIElementCopyElementAtPosition returned AXError {err} ({})",
            ax_error_label(err)
        );
        unsafe {
            CFRelease(app);
        }
        return Err(18);
    }
    if element.is_null() {
        eprintln!("[ERROR] AXUIElementCopyElementAtPosition returned Success but null element");
        unsafe {
            CFRelease(app);
        }
        return Err(18);
    }
    Ok((app, element))
}

/// Read an AX attribute by name and return a human-readable string for any
/// CFType. CFString values are read directly; non-string CFTypes (CFBoolean,
/// CFNumber, AXValue, ...) are described via CFCopyDescription.
///
/// Returns `Some(repr)` if the attribute exists, `None` with the AXError
/// printed otherwise. Best-effort diagnostic helper.
fn read_attribute(element: *mut c_void, attribute: &str) -> Option<String> {
    let attr_key = CFString::new(attribute);
    let mut value: *const c_void = ptr::null();
    let err = unsafe {
        AXUIElementCopyAttributeValue(element, attr_key.as_concrete_TypeRef(), &mut value)
    };
    if err != AX_ERROR_SUCCESS || value.is_null() {
        eprintln!("  [{attribute}] AXError {err} ({})", ax_error_label(err));
        return None;
    }
    // Type-check before wrapping. CFString::wrap_under_create_rule on a
    // non-string CFType produces an object whose methods (e.g. `to_string`)
    // invoke ObjC bridges expecting a CFString backing store and abort with
    // "Rust cannot catch foreign exceptions". CFCopyDescription is the
    // universal escape hatch — works on any CFType.
    let string_type_id = unsafe { CFStringGetTypeID() };
    let actual_type_id = unsafe { CFGetTypeID(value) };
    let out = if actual_type_id == string_type_id {
        // SAFETY: type ID confirms this is a CFString. wrap_under_create_rule
        // takes ownership and CFReleases on drop.
        let cfstr = unsafe { CFString::wrap_under_create_rule(value as CFStringRef) };
        cfstr.to_string()
    } else {
        // SAFETY: CFCopyDescription returns a "Create"-rule CFStringRef
        // describing any CFType. The original `value` is still owned by us
        // and must be CFReleased after.
        let desc_ref = unsafe { CFCopyDescription(value) };
        let desc = if desc_ref.is_null() {
            "<non-string, CFCopyDescription returned null>".to_string()
        } else {
            // SAFETY: CFCopyDescription is "Create"-rule.
            let cfstr = unsafe { CFString::wrap_under_create_rule(desc_ref) };
            cfstr.to_string()
        };
        // SAFETY: value is a "Create"-rule CFTypeRef from AXUIElementCopyAttributeValue.
        unsafe {
            CFRelease(value as *mut c_void);
        }
        desc
    };
    Some(out)
}

/// Enumerate the AX action names exposed by an element. Returns the list
/// (possibly empty) on success, `None` if the call itself failed.
fn read_action_names(element: *mut c_void) -> Option<Vec<String>> {
    let mut names: CFArrayRef = ptr::null();
    let err = unsafe { AXUIElementCopyActionNames(element, &mut names) };
    if err != AX_ERROR_SUCCESS || names.is_null() {
        eprintln!("  [AXActionNames] AXError {err} ({})", ax_error_label(err));
        return None;
    }
    // SAFETY: AXUIElementCopyActionNames returns a "Create"-rule CFArrayRef
    // of CFStringRef. Iterate manually via the raw CFArrayGetCount /
    // CFArrayGetValueAtIndex calls — the core-foundation crate's typed
    // CFArray<T> wrapper would require T: TCFType, and CFStringRef is a
    // bare typedef, not a wrapper.
    let count = unsafe { CFArrayGetCount(names) };
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        let item: *const c_void = unsafe { CFArrayGetValueAtIndex(names, i) };
        if item.is_null() {
            continue;
        }
        // wrap_under_get_rule because the CFArray owns the reference; the
        // single Release on `names` below covers all elements.
        let cfstr = unsafe { CFString::wrap_under_get_rule(item as CFStringRef) };
        out.push(cfstr.to_string());
    }
    unsafe {
        CFRelease(names as *mut c_void);
    }
    Some(out)
}

fn inspect_element(pid: i32, x: f64, y: f64) -> ExitCode {
    eprintln!("[p5-spike] INSPECT — read-only AX query at ({x}, {y}) within pid {pid}");
    let (app, element) = match resolve_element_at(pid, x, y) {
        Ok(pair) => pair,
        Err(code) => return ExitCode::from(code),
    };
    eprintln!("[p5-spike] AX element resolved. Reading attributes:");
    for attr in [
        "AXRole",
        "AXSubrole",
        "AXRoleDescription",
        "AXTitle",
        "AXDescription",
        "AXIdentifier",
        "AXHelp",
        "AXEnabled",
        "AXFocused",
        "AXPosition",
        "AXSize",
        "AXParent",
        "AXChildren",
    ] {
        if let Some(value) = read_attribute(element, attr) {
            eprintln!("  [{attr}] = {value}");
        }
    }
    eprintln!("[p5-spike] Reading AXActionNames:");
    if let Some(actions) = read_action_names(element) {
        if actions.is_empty() {
            eprintln!("  (no actions exposed by this element)");
        } else {
            for action in &actions {
                let marker = if action == "AXPress" {
                    "  <-- AXPress"
                } else {
                    ""
                };
                eprintln!("  - {action}{marker}");
            }
            let has_press = actions.iter().any(|a| a == "AXPress");
            eprintln!(
                "[p5-spike] AXPress in action list: {}",
                if has_press { "YES" } else { "NO" }
            );
        }
    }
    unsafe {
        CFRelease(element);
        CFRelease(app);
    }
    ExitCode::SUCCESS
}

fn post_ax_press(pid: i32, x: f64, y: f64) -> ExitCode {
    eprintln!("[p5-spike] EXPERIMENT — AX press at ({x}, {y}) within pid {pid}");
    let (app, element) = match resolve_element_at(pid, x, y) {
        Ok(pair) => pair,
        Err(code) => return ExitCode::from(code),
    };
    eprintln!(
        "[p5-spike] AX element resolved at ({x}, {y}). Calling AXUIElementPerformAction(kAXPressAction)."
    );

    // kAXPressAction C constant value is the string "AXPress".
    let press = CFString::from_static_string("AXPress");
    let err = unsafe { AXUIElementPerformAction(element, press.as_concrete_TypeRef()) };

    unsafe {
        CFRelease(element);
        CFRelease(app);
    }

    if err != AX_ERROR_SUCCESS {
        eprintln!(
            "[ERROR] AXUIElementPerformAction(AXPress) returned AXError {err} ({})",
            ax_error_label(err)
        );
        eprintln!("        If Failure (-25200): action couldn't be performed — common when the");
        eprintln!("        target window is on a hidden Space (another app is in full-screen);");
        eprintln!("        AX query layer succeeds across Spaces but the action layer refuses.");
        eprintln!("        If ActionUnsupported (-25206): the AX element does NOT expose AXPress.");
        eprintln!("        Run `--inspect` at the same coords to see which actions ARE supported.");
        return ExitCode::from(18);
    }
    eprintln!("[p5-spike] AXPress dispatched. Eyeball RoK for visible response.");
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let progname = args.first().map_or("p5-spike", String::as_str);
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
    let use_noop = args.iter().any(|a| a == "--noop");
    let use_inspect = args.iter().any(|a| a == "--inspect");

    eprintln!("[p5-spike] checking Accessibility (will prompt if not yet granted)...");
    if !check_accessibility_with_prompt() {
        eprintln!(
            "[p5-spike] Accessibility denied. macOS may have just shown a \
             prompt; grant the spike binary Accessibility in System Settings \
             > Privacy & Security > Accessibility, then re-run. The prompt is \
             asynchronous, so first-run exit-then-grant-then-rerun is expected."
        );
        return ExitCode::from(13);
    }
    eprintln!("[p5-spike] Accessibility granted.");

    if use_noop {
        eprintln!("[p5-spike] BASELINE — no click posted. Exiting cleanly.");
        return ExitCode::SUCCESS;
    }
    if use_inspect {
        return inspect_element(pid, x, y);
    }
    if use_hid {
        return post_hid_click(x, y);
    }
    post_ax_press(pid, x, y)
}
