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

use core_graphics::display::{CGDisplay, CGPoint};
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
    fn AXUIElementCopyAttributeNames(element: *mut c_void, names: *mut CFArrayRef) -> i32;
    fn AXUIElementCopyParameterizedAttributeNames(
        element: *mut c_void,
        names: *mut CFArrayRef,
    ) -> i32;
    fn AXUIElementCopyActionNames(element: *mut c_void, names: *mut CFArrayRef) -> i32;
    fn AXUIElementIsAttributeSettable(
        element: *mut c_void,
        attribute: CFStringRef,
        settable: *mut u8,
    ) -> i32;
    fn AXUIElementSetAttributeValue(
        element: *mut c_void,
        attribute: CFStringRef,
        value: *const c_void,
    ) -> i32;
    fn AXValueCreate(value_type: u32, value_ptr: *const c_void) -> *const c_void;
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
    eprintln!(
        "usage: {progname} <pid> <screen_x> <screen_y> [--hid|--stealth|--noop|--inspect]"
    );
    eprintln!();
    eprintln!(
        "  default:   AXUIElementPerformAction(kAXPressAction) at (x, y) within app  [experiment]"
    );
    eprintln!(
        "  --hid:     CGEvent::post(HID tap location)                                  [control = v0.1.4 path]"
    );
    eprintln!(
        "  --stealth: disassociate cursor + HID tap + reassociate                      [v0.1.6 candidate]"
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

/// Resolve `SLEventPostToPid` from SkyLight.framework via dlopen + dlsym.
/// Returns None if the symbol can't be resolved on this OS.
///
/// Signature: `void SLEventPostToPid(pid_t, CGEventRef)` — same shape as
/// public `CGEventPostToPid` but routes through SkyLight's per-pid event-post
/// path which DOES bypass the HID stream entirely (no cursor warp) and DOES
/// deliver to backgrounded targets reliably.
fn resolve_sl_event_post_to_pid() -> Option<extern "C" fn(i32, *mut c_void)> {
    type Symbol = extern "C" fn(i32, *mut c_void);
    unsafe extern "C" {
        fn dlopen(filename: *const i8, flag: i32) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const i8) -> *mut c_void;
    }
    const RTLD_LAZY: i32 = 1;
    let path = b"/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight\0";
    let sym = b"SLEventPostToPid\0";
    unsafe {
        let _ = dlopen(path.as_ptr() as *const i8, RTLD_LAZY);
        // RTLD_DEFAULT == (void *)-2 on macOS.
        let handle = (-2isize) as *mut c_void;
        let ptr = dlsym(handle, sym.as_ptr() as *const i8);
        if ptr.is_null() {
            None
        } else {
            Some(std::mem::transmute::<*mut c_void, Symbol>(ptr))
        }
    }
}

/// Minimal v0.1.6 candidate: SkyLight `SLEventPostToPid` only, no focus-without-raise
/// dance. RoK is a Catalyst game — it processes input events through Catalyst's
/// Bridge translation layer, not through a user-activation gate like Chromium.
/// If this works, it's the cleanest possible background-click path: no cursor
/// warp, no focus flip, no window raise.
fn post_skylight_click(pid: i32, x: f64, y: f64) -> ExitCode {
    let Some(sl_post) = resolve_sl_event_post_to_pid() else {
        eprintln!("[ERROR] SLEventPostToPid not resolvable (SkyLight.framework symbol missing)");
        return ExitCode::from(18);
    };
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
    eprintln!(
        "[p5-spike] SKYLIGHT — SLEventPostToPid only, no focus-without-raise, pid={pid} ({x}, {y})"
    );
    // The CGEvent crate's into_raw or as_ptr surface is gated; the events
    // are TCFType wrappers. To call SLEventPostToPid we need the raw
    // CGEventRef. core-graphics 0.25 doesn't expose as_concrete_TypeRef
    // for CGEvent directly, but Drop+leak via Box / ManuallyDrop works:
    // we use the TCFType impl's as_CFTypeRef which returns the underlying
    // pointer without releasing ownership.
    use foreign_types_shared::ForeignType;
    let down_ptr = down.as_ptr() as *mut c_void;
    let up_ptr = up.as_ptr() as *mut c_void;
    sl_post(pid, down_ptr);
    sleep(Duration::from_millis(CLICK_GAP_MS));
    sl_post(pid, up_ptr);
    eprintln!("[p5-spike] SKYLIGHT — click pair posted via SLEventPostToPid");
    ExitCode::SUCCESS
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

/// Disassociate the visible cursor from the logical cursor position, post
/// a click pair at (x, y), then warp the logical cursor back to the user's
/// original position and reassociate. The user's visible cursor never
/// moves. RoK receives a real synthetic HID click at the target coords.
///
/// This is the candidate v0.1.6 click delivery path: HID-level taps work
/// on Catalyst Bridge (verified) and explicit activation puts RoK topmost
/// (verified), but the bare HID-tap variant warps the user's visible
/// cursor as a side effect. Disassociation suppresses the visible warp.
fn post_hid_click_stealth(x: f64, y: f64) -> ExitCode {
    let Ok(source) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        eprintln!("[ERROR] CGEventSource::new(HIDSystemState) returned Err");
        return ExitCode::from(13);
    };
    let point = CGPoint::new(x, y);

    // Step 1: snapshot the current cursor location so we can warp the
    // logical cursor back to it before reassociating. CGEventCreate(NULL)
    // returns a freshly-built event populated with the current cursor pos.
    let saved_cursor = {
        let Ok(probe) = CGEvent::new(source.clone()) else {
            eprintln!("[ERROR] CGEvent::new(source) probe for cursor location failed");
            return ExitCode::from(18);
        };
        probe.location()
    };
    eprintln!(
        "[p5-spike] STEALTH — saved cursor at ({:.1}, {:.1})",
        saved_cursor.x, saved_cursor.y
    );

    // Step 2: disassociate the visible cursor from the logical mouse.
    // After this call, HID events still route by their coords, but the
    // visible cursor stops following the logical position. Any failure
    // here aborts before posting events.
    if let Err(e) = CGDisplay::associate_mouse_and_mouse_cursor_position(false) {
        eprintln!("[ERROR] CGAssociateMouseAndMouseCursorPosition(false) returned CGError {e}");
        return ExitCode::from(18);
    }
    eprintln!("[p5-spike] STEALTH — cursor disassociated");

    let down = match CGEvent::new_mouse_event(
        source.clone(),
        CGEventType::LeftMouseDown,
        point,
        CGMouseButton::Left,
    ) {
        Ok(e) => e,
        Err(_) => {
            let _ = CGDisplay::associate_mouse_and_mouse_cursor_position(true);
            eprintln!("[ERROR] CGEvent::new_mouse_event(LeftMouseDown) returned Err");
            return ExitCode::from(18);
        }
    };
    let up = match CGEvent::new_mouse_event(
        source,
        CGEventType::LeftMouseUp,
        point,
        CGMouseButton::Left,
    ) {
        Ok(e) => e,
        Err(_) => {
            let _ = CGDisplay::associate_mouse_and_mouse_cursor_position(true);
            eprintln!("[ERROR] CGEvent::new_mouse_event(LeftMouseUp) returned Err");
            return ExitCode::from(18);
        }
    };

    eprintln!(
        "[p5-spike] STEALTH — posting LeftMouseDown/Up at ({x}, {y}) [cursor disassociated]"
    );
    down.post(CGEventTapLocation::HID);
    sleep(Duration::from_millis(CLICK_GAP_MS));
    up.post(CGEventTapLocation::HID);

    // Step 3: warp the logical cursor back to the user's original position.
    // Without this, reassociating snaps the visible cursor to wherever the
    // logical cursor landed after the HID event (i.e., (x, y) — the click
    // point) and the user sees a cursor jump.
    if let Err(e) = CGDisplay::warp_mouse_cursor_position(saved_cursor) {
        eprintln!("[WARN] CGWarpMouseCursorPosition(saved) returned CGError {e}");
        // continue to reassociate anyway so we don't leave the user stuck
    }

    // Step 4: reassociate. Visible cursor now follows the logical cursor
    // again, which is back at the user's original position. No visible jump.
    if let Err(e) = CGDisplay::associate_mouse_and_mouse_cursor_position(true) {
        eprintln!(
            "[WARN] CGAssociateMouseAndMouseCursorPosition(true) returned CGError {e} — user cursor may be stuck disassociated"
        );
        return ExitCode::from(18);
    }
    eprintln!("[p5-spike] STEALTH — cursor reassociated; click pair complete");
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

/// Shared CFArrayRef-of-CFStringRef → Vec<String> reader. Used for the
/// three Copy*Names enumerators (Attribute, Parameterized, Action).
/// Releases the array. Returns None and prints the AXError on failure.
fn collect_string_array_names(
    element: *mut c_void,
    label: &str,
    fetch: unsafe extern "C" fn(*mut c_void, *mut CFArrayRef) -> i32,
) -> Option<Vec<String>> {
    let mut names: CFArrayRef = ptr::null();
    let err = unsafe { fetch(element, &mut names) };
    if err != AX_ERROR_SUCCESS || names.is_null() {
        eprintln!("  [{label}] AXError {err} ({})", ax_error_label(err));
        return None;
    }
    let count = unsafe { CFArrayGetCount(names) };
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        let item: *const c_void = unsafe { CFArrayGetValueAtIndex(names, i) };
        if item.is_null() {
            continue;
        }
        let cfstr = unsafe { CFString::wrap_under_get_rule(item as CFStringRef) };
        out.push(cfstr.to_string());
    }
    unsafe {
        CFRelease(names as *mut c_void);
    }
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
        "AXFrame",
        "AXValue",
        "AXSelected",
        "AXActivationPoint",
        "AXTopLevelUIElement",
        "AXWindow",
        "AXParent",
        "AXChildren",
        "AXChildrenInNavigationOrder",
        "AXLinkedUIElements",
        "AXCustomActions",
        "AXCustomContent",
        "AXCustomRotors",
        "AXUserInputLabels",
        "AXLanguage",
    ] {
        if let Some(value) = read_attribute(element, attr) {
            eprintln!("  [{attr}] = {value}");
        }
    }
    eprintln!("[p5-spike] Enumerating ALL AX attribute names:");
    if let Some(attrs) =
        collect_string_array_names(element, "AXAttributeNames", AXUIElementCopyAttributeNames)
    {
        if attrs.is_empty() {
            eprintln!("  (none)");
        } else {
            for name in &attrs {
                eprintln!("  - {name}");
            }
        }
    }
    eprintln!("[p5-spike] Enumerating ALL AX parameterized attribute names:");
    if let Some(pattrs) = collect_string_array_names(
        element,
        "AXParameterizedAttributeNames",
        AXUIElementCopyParameterizedAttributeNames,
    ) {
        if pattrs.is_empty() {
            eprintln!("  (none — no parameterized attributes; rules out hit-test-by-point)");
        } else {
            for name in &pattrs {
                let marker = if name.contains("ForPoint")
                    || name.contains("AtPoint")
                    || name.contains("HitTest")
                {
                    "  <-- candidate coord-aware attribute"
                } else {
                    ""
                };
                eprintln!("  - {name}{marker}");
            }
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

/// Dump RoK's app-level AX hierarchy. Different starting point from
/// `--inspect`: this asks AXUIElementCreateApplication(pid) for the app
/// element, reads AXWindows, then recursively dumps each window's
/// children. If RoK exposes ANY granularity (per-window subtrees, button
/// children, panel groups), this surfaces it. The position-resolved
/// `--inspect` only sees one element because point-lookup goes to the
/// deepest leaf at that coord; the app-level walk goes top-down.
fn inspect_app_tree(pid: i32) -> ExitCode {
    eprintln!("[p5-spike] APP-TREE — dumping AX hierarchy of pid {pid}");
    let app: *mut c_void = unsafe { AXUIElementCreateApplication(pid) };
    if app.is_null() {
        eprintln!("[ERROR] AXUIElementCreateApplication returned null for pid {pid}");
        return ExitCode::from(18);
    }

    eprintln!("[app] AXUIElementCreateApplication(pid={pid}) — attributes:");
    for attr in [
        "AXRole",
        "AXTitle",
        "AXIdentifier",
        "AXFocused",
        "AXFocusedWindow",
        "AXMainWindow",
        "AXWindows",
        "AXMenuBar",
    ] {
        if let Some(value) = read_attribute(app, attr) {
            eprintln!("  [{attr}] = {value}");
        }
    }

    // Pull out the AXWindows array and recurse into each.
    let windows_key = CFString::new("AXWindows");
    let mut windows_ref: *const c_void = ptr::null();
    let err = unsafe {
        AXUIElementCopyAttributeValue(app, windows_key.as_concrete_TypeRef(), &mut windows_ref)
    };
    if err != AX_ERROR_SUCCESS || windows_ref.is_null() {
        eprintln!(
            "[ERROR] AXUIElementCopyAttributeValue(AXWindows) returned AXError {err} ({})",
            ax_error_label(err)
        );
        unsafe {
            CFRelease(app);
        }
        return ExitCode::from(18);
    }
    let windows = windows_ref as CFArrayRef;
    let window_count = unsafe { CFArrayGetCount(windows) };
    eprintln!("[app] AXWindows count: {window_count}");

    for w_idx in 0..window_count {
        let window: *mut c_void =
            unsafe { CFArrayGetValueAtIndex(windows, w_idx) as *mut c_void };
        if window.is_null() {
            continue;
        }
        eprintln!("\n[window {w_idx}]");
        for attr in [
            "AXRole",
            "AXSubrole",
            "AXTitle",
            "AXIdentifier",
            "AXMain",
            "AXFocused",
            "AXModal",
            "AXFrame",
            "AXChildren",
        ] {
            if let Some(value) = read_attribute(window, attr) {
                eprintln!("  [{attr}] = {value}");
            }
        }

        // Walk children of this window, one level deep.
        let children_key = CFString::new("AXChildren");
        let mut children_ref: *const c_void = ptr::null();
        let err = unsafe {
            AXUIElementCopyAttributeValue(
                window,
                children_key.as_concrete_TypeRef(),
                &mut children_ref,
            )
        };
        if err != AX_ERROR_SUCCESS || children_ref.is_null() {
            continue;
        }
        let children = children_ref as CFArrayRef;
        let child_count = unsafe { CFArrayGetCount(children) };
        eprintln!("  [window {w_idx} children count] = {child_count}");
        for c_idx in 0..child_count.min(20) {
            let child: *mut c_void =
                unsafe { CFArrayGetValueAtIndex(children, c_idx) as *mut c_void };
            if child.is_null() {
                continue;
            }
            eprintln!("  --- window {w_idx} child {c_idx} ---");
            for attr in [
                "AXRole",
                "AXSubrole",
                "AXTitle",
                "AXIdentifier",
                "AXDescription",
                "AXFrame",
                "AXPosition",
                "AXSize",
            ] {
                if let Some(value) = read_attribute(child, attr) {
                    eprintln!("    [{attr}] = {value}");
                }
            }
            if let Some(actions) =
                collect_string_array_names(child, "AXActionNames", AXUIElementCopyActionNames)
            {
                eprintln!("    [actions] = {actions:?}");
            }

            // Recurse one more level — most interesting for iOSContentGroup.
            // Catalyst Bridge wraps RoK's UIKit hierarchy here; if there are
            // any per-control AX nodes, they'll surface as grandchildren.
            let children_key2 = CFString::new("AXChildren");
            let mut gc_ref: *const c_void = ptr::null();
            let err2 = unsafe {
                AXUIElementCopyAttributeValue(
                    child,
                    children_key2.as_concrete_TypeRef(),
                    &mut gc_ref,
                )
            };
            if err2 == AX_ERROR_SUCCESS && !gc_ref.is_null() {
                let grandchildren = gc_ref as CFArrayRef;
                let gc_count = unsafe { CFArrayGetCount(grandchildren) };
                eprintln!("    [grandchild count] = {gc_count}");
                for gc_idx in 0..gc_count.min(30) {
                    let gc: *mut c_void =
                        unsafe { CFArrayGetValueAtIndex(grandchildren, gc_idx) as *mut c_void };
                    if gc.is_null() {
                        continue;
                    }
                    eprintln!("      --- grandchild {gc_idx} ---");
                    for attr in [
                        "AXRole",
                        "AXSubrole",
                        "AXTitle",
                        "AXIdentifier",
                        "AXDescription",
                        "AXFrame",
                    ] {
                        if let Some(value) = read_attribute(gc, attr) {
                            eprintln!("        [{attr}] = {value}");
                        }
                    }
                    if let Some(actions) = collect_string_array_names(
                        gc,
                        "AXActionNames",
                        AXUIElementCopyActionNames,
                    ) {
                        if !actions.is_empty() {
                            eprintln!("        [actions] = {actions:?}");
                        }
                    }
                }
                unsafe {
                    CFRelease(gc_ref as *mut c_void);
                }
            }
        }
        unsafe {
            CFRelease(children_ref as *mut c_void);
        }
    }

    unsafe {
        CFRelease(windows_ref as *mut c_void);
        CFRelease(app);
    }
    ExitCode::SUCCESS
}

/// Experimental: try to set AXActivationPoint on the root element, then
/// AXPress. If this works, RoK fires the press at the coord we wrote
/// instead of at the element's default activation point (which is the
/// window center, confirming the v0.1.5 wrong-target bug).
///
/// Requires AXActivationPoint to be settable on the root element. If
/// the attribute is read-only, this path is dead and we fall back to
/// HID stealth + activation.
const K_AX_VALUE_CG_POINT_TYPE: u32 = 1; // AXValueType.CGPoint

fn post_ax_press_at_set_point(pid: i32, x: f64, y: f64) -> ExitCode {
    eprintln!("[p5-spike] AX-SET-POINT — try to write AXActivationPoint, then AXPress");
    let (app, element) = match resolve_element_at(pid, x, y) {
        Ok(pair) => pair,
        Err(code) => return ExitCode::from(code),
    };

    let attr_key = CFString::new("AXActivationPoint");

    // Settability probe — bail with a clear message if not settable.
    let mut settable: u8 = 0;
    let err = unsafe {
        AXUIElementIsAttributeSettable(element, attr_key.as_concrete_TypeRef(), &mut settable)
    };
    if err != AX_ERROR_SUCCESS {
        eprintln!(
            "[ERROR] AXUIElementIsAttributeSettable(AXActivationPoint) returned AXError {err} ({})",
            ax_error_label(err)
        );
        unsafe {
            CFRelease(element);
            CFRelease(app);
        }
        return ExitCode::from(18);
    }
    if settable == 0 {
        eprintln!(
            "[p5-spike] AXActivationPoint is NOT settable on RoK's root element. This path is dead."
        );
        unsafe {
            CFRelease(element);
            CFRelease(app);
        }
        return ExitCode::from(18);
    }
    eprintln!("[p5-spike] AXActivationPoint IS settable. Building CGPoint AXValue…");

    // Build an AXValue wrapping our target CGPoint.
    let target = CGPoint::new(x, y);
    let target_ptr: *const c_void = &target as *const CGPoint as *const c_void;
    let value_ref = unsafe { AXValueCreate(K_AX_VALUE_CG_POINT_TYPE, target_ptr) };
    if value_ref.is_null() {
        eprintln!("[ERROR] AXValueCreate(CGPoint) returned null");
        unsafe {
            CFRelease(element);
            CFRelease(app);
        }
        return ExitCode::from(18);
    }

    let err = unsafe {
        AXUIElementSetAttributeValue(element, attr_key.as_concrete_TypeRef(), value_ref)
    };
    unsafe {
        CFRelease(value_ref as *mut c_void);
    }
    if err != AX_ERROR_SUCCESS {
        eprintln!(
            "[ERROR] AXUIElementSetAttributeValue(AXActivationPoint) returned AXError {err} ({})",
            ax_error_label(err)
        );
        unsafe {
            CFRelease(element);
            CFRelease(app);
        }
        return ExitCode::from(18);
    }
    eprintln!("[p5-spike] AXActivationPoint set to ({x}, {y}). Reading back to verify…");

    if let Some(readback) = read_attribute(element, "AXActivationPoint") {
        eprintln!("  [AXActivationPoint readback] = {readback}");
    }

    eprintln!("[p5-spike] Performing AXPress on element with overridden activation point…");
    let press = CFString::from_static_string("AXPress");
    let err =
        unsafe { AXUIElementPerformAction(element, press.as_concrete_TypeRef()) };

    unsafe {
        CFRelease(element);
        CFRelease(app);
    }

    if err != AX_ERROR_SUCCESS {
        eprintln!(
            "[ERROR] AXUIElementPerformAction(AXPress) returned AXError {err} ({})",
            ax_error_label(err)
        );
        return ExitCode::from(18);
    }
    eprintln!("[p5-spike] AXPress dispatched after set-point. Observe RoK.");
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
    let use_stealth = args.iter().any(|a| a == "--stealth");
    let use_ax_set_point = args.iter().any(|a| a == "--ax-set-point");
    let use_app_tree = args.iter().any(|a| a == "--app-tree");
    let use_skylight = args.iter().any(|a| a == "--skylight");

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
    if use_app_tree {
        return inspect_app_tree(pid);
    }
    if use_skylight {
        return post_skylight_click(pid, x, y);
    }
    if use_ax_set_point {
        return post_ax_press_at_set_point(pid, x, y);
    }
    if use_stealth {
        return post_hid_click_stealth(x, y);
    }
    if use_hid {
        return post_hid_click(x, y);
    }
    post_ax_press(pid, x, y)
}
