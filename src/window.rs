//! Window enumeration via Core Graphics.
//!
//! Finds the single Rise of Kingdoms main window and returns its frame in
//! CG coordinates. The matcher requires both `kCGWindowOwnerName` AND
//! `kCGWindowName` to equal `"RiseOfKingdoms"` — RoK spawns auxiliary
//! windows (splash, header overlays, popups) that share the owner but
//! have a different title or no title at all. Per the verified P7 spike,
//! the main window keeps a stable window ID and PID across BD
//! disconnect/reconnect, so identifying it by owner+title is sufficient.

#![allow(
    unsafe_code,
    reason = "Core Graphics FFI is required to wrap CFString constants and to call \
              CGRectMakeWithDictionaryRepresentation; the unsafe surface is contained \
              in this module."
)]

use std::ffi::c_void;

use core_foundation::ConcreteCFType;
use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_graphics::display::{CGPoint, CGRect, CGSize};
use core_graphics::window::{
    copy_window_info, kCGNullWindowID, kCGWindowBounds, kCGWindowListOptionOnScreenOnly,
    kCGWindowName, kCGWindowNumber, kCGWindowOwnerName, kCGWindowOwnerPID,
};

use crate::error::{BotError, Result};

pub const ROK_OWNER: &str = "RiseOfKingdoms";
pub const ROK_TITLE: &str = "RiseOfKingdoms";

/// Bundle-ID prefix that legitimate RoK installs share. Verified on
/// `/Applications/RiseOfKingdoms.app` (Vietnam region: `com.rok.ios.vn`).
/// The `.vn` / `.kr` / `.us` suffix varies by region but the prefix is
/// stable across all official releases. Used as a spoof check: any
/// process can set `kCGWindowOwnerName == "RiseOfKingdoms"` and
/// `kCGWindowName == "RiseOfKingdoms"`, but only the real game has a
/// bundle ID under `com.rok.ios.`. Without this gate, once v0.1+ adds
/// synthetic input or screen capture, a spoof could redirect the bot
/// onto an attacker-controlled window.
pub const ROK_BUNDLE_PREFIX: &str = "com.rok.ios.";

#[derive(Debug, Clone, Copy)]
pub struct Window {
    pub id: u32,
    pub pid: i32,
    pub frame: CGRect,
}

impl Window {
    pub fn center(&self) -> CGPoint {
        CGPoint {
            x: self.frame.origin.x + self.frame.size.width / 2.0,
            y: self.frame.origin.y + self.frame.size.height / 2.0,
        }
    }
}

#[derive(Debug, Default, Clone)]
struct WindowRecord {
    owner: Option<String>,
    title: Option<String>,
    id: Option<u32>,
    pid: Option<i32>,
    bounds: Option<CGRect>,
}

/// Pure: turn a parsed record into a `Window` iff it matches the RoK main
/// window AND every required field is present. Auxiliary RoK windows
/// (splash, popups) share `owner` but have no title or a different title
/// and return `None`.
fn select_rok_window(record: &WindowRecord) -> Option<Window> {
    let owner = record.owner.as_deref()?;
    let title = record.title.as_deref()?;
    if owner != ROK_OWNER || title != ROK_TITLE {
        return None;
    }
    Some(Window {
        id: record.id?,
        pid: record.pid?,
        frame: record.bounds?,
    })
}

/// Walk the live `CGWindowListCopyWindowInfo` array and return the first
/// window matching the RoK main-window predicate AND backed by a process
/// whose bundle ID starts with `ROK_BUNDLE_PREFIX` (anti-spoof check).
/// Spoofs are skipped with a `tracing::warn!` so they're visible in logs
/// without halting the search — the real RoK window may be later in the
/// list.
pub fn find_rok_window() -> Result<Window> {
    let Some(info_list) = copy_window_info(kCGWindowListOptionOnScreenOnly, kCGNullWindowID) else {
        return Err(BotError::WindowNotFound);
    };

    for entry in info_list.iter() {
        // Each entry is a *const c_void pointing at a CFDictionaryRef.
        let raw_ptr: *const c_void = *entry;
        if raw_ptr.is_null() {
            continue;
        }
        // SAFETY: CGWindowList vends each array slot as an unretained
        // CFDictionaryRef ("Get rule"). `wrap_under_get_rule` is the
        // canonical lift: it CFRetains internally so the resulting `cf`
        // owns its retain. The null guard above prevents the upstream
        // assertion in `wrap_under_get_rule(reference: CFTypeRef)`.
        let cf = unsafe { CFType::wrap_under_get_rule(raw_ptr.cast()) };
        let Some(dict) = cf.downcast::<CFDictionary>() else {
            continue;
        };
        let record = parse_record(&dict);
        if let Some(window) = select_rok_window(&record) {
            if bundle_id_for_pid(window.pid)
                .as_deref()
                .is_some_and(matches_rok_bundle_id)
            {
                return Ok(window);
            }
            tracing::warn!(
                target: "rok_bot",
                pid = window.pid,
                window_id = window.id,
                expected_prefix = ROK_BUNDLE_PREFIX,
                actual = bundle_id_for_pid(window.pid).as_deref().unwrap_or("<unknown>"),
                "skipping window with owner=title=\"{ROK_OWNER}\" — bundle ID mismatch (possible spoof or stale window)",
            );
        }
    }
    Err(BotError::WindowNotFound)
}

/// Pure: does this bundle ID belong to a legitimate RoK install?
fn matches_rok_bundle_id(bundle_id: &str) -> bool {
    bundle_id.starts_with(ROK_BUNDLE_PREFIX)
}

/// Live wrapper: ask AppKit for the bundle ID of the process owning `pid`.
/// Returns `None` if the process has no bundle (rare for GUI apps),
/// no longer exists, or AppKit can't enumerate it. Both objc2 calls
/// here are declared safe by the binding (no `unsafe` block needed).
fn bundle_id_for_pid(pid: i32) -> Option<String> {
    let app = objc2_app_kit::NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
    let ns_id = app.bundleIdentifier()?;
    Some(ns_id.to_string())
}

fn parse_record(dict: &CFDictionary) -> WindowRecord {
    // SAFETY: reading static CFStringRef constants vended by Core Graphics
    // (Rust 2024 requires `unsafe` for any extern static read).
    let (k_owner, k_title, k_id, k_pid, k_bounds) = unsafe {
        (
            kCGWindowOwnerName.cast::<c_void>(),
            kCGWindowName.cast::<c_void>(),
            kCGWindowNumber.cast::<c_void>(),
            kCGWindowOwnerPID.cast::<c_void>(),
            kCGWindowBounds.cast::<c_void>(),
        )
    };
    WindowRecord {
        owner: dict_get::<CFString>(dict, k_owner).map(|s| s.to_string()),
        title: dict_get::<CFString>(dict, k_title).map(|s| s.to_string()),
        id: dict_get::<CFNumber>(dict, k_id)
            .and_then(|n| n.to_i64())
            .and_then(|v| u32::try_from(v).ok()),
        pid: dict_get::<CFNumber>(dict, k_pid)
            .and_then(|n| n.to_i64())
            .and_then(|v| i32::try_from(v).ok()),
        bounds: dict_get::<CFDictionary>(dict, k_bounds).and_then(|d| rect_from_dict(&d)),
    }
}

/// Generic typed lookup against a default-typed CFDictionary.
fn dict_get<T: ConcreteCFType>(dict: &CFDictionary, key: *const c_void) -> Option<T> {
    let value_ref = dict.find(key)?;
    let value_ptr: *const c_void = *value_ref;
    if value_ptr.is_null() {
        return None;
    }
    // SAFETY: dict.find returns a borrowed slot pointer ("Get rule" —
    // unretained). `wrap_under_get_rule` CFRetains internally so `cf`
    // holds its own ownership. The null guard above prevents the
    // upstream null-assertion in `wrap_under_get_rule(reference: CFTypeRef)`.
    let cf = unsafe { CFType::wrap_under_get_rule(value_ptr.cast()) };
    cf.downcast::<T>()
}

/// Convert the `kCGWindowBounds` dict (with X/Y/Width/Height keys) into a
/// `CGRect` using Core Graphics's official converter. Rejects rects whose
/// fields aren't finite or whose size is non-positive — `CGRectMake...`
/// itself will accept NaN/Inf and zero-size dicts; downstream code (window
/// center arithmetic + `rect_contains`) silently produces wrong results
/// when fed those, so guard at the boundary.
fn rect_from_dict(dict: &CFDictionary) -> Option<CGRect> {
    let mut rect = CGRect {
        origin: CGPoint::new(0.0, 0.0),
        size: CGSize::new(0.0, 0.0),
    };
    // SAFETY: `dict` is a live CFDictionary obtained from CGWindowList. We
    // pass a writable CGRect by pointer and check the boolean return per
    // Apple's API contract.
    let ok = unsafe {
        CGRectMakeWithDictionaryRepresentation(dict.as_concrete_TypeRef().cast(), &raw mut rect)
    };
    if !ok {
        return None;
    }
    if !rect.origin.x.is_finite()
        || !rect.origin.y.is_finite()
        || !rect.size.width.is_finite()
        || !rect.size.height.is_finite()
        || rect.size.width <= 0.0
        || rect.size.height <= 0.0
    {
        return None;
    }
    Some(rect)
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGRectMakeWithDictionaryRepresentation(dict: *const c_void, rect: *mut CGRect) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(&CGPoint::new(x, y), &CGSize::new(w, h))
    }

    fn assert_rect_eq(a: CGRect, b: CGRect) {
        assert!((a.origin.x - b.origin.x).abs() < f64::EPSILON, "origin.x");
        assert!((a.origin.y - b.origin.y).abs() < f64::EPSILON, "origin.y");
        assert!(
            (a.size.width - b.size.width).abs() < f64::EPSILON,
            "size.width"
        );
        assert!(
            (a.size.height - b.size.height).abs() < f64::EPSILON,
            "size.height"
        );
    }

    #[test]
    fn select_main_window_full_match() {
        let record = WindowRecord {
            owner: Some(ROK_OWNER.to_owned()),
            title: Some(ROK_TITLE.to_owned()),
            id: Some(64793),
            pid: Some(21916),
            bounds: Some(rect(-525.0, 502.0, 1280.0, 720.0)),
        };
        let window = select_rok_window(&record).expect("should match");
        assert_eq!(window.id, 64793);
        assert_eq!(window.pid, 21916);
        assert_rect_eq(window.frame, rect(-525.0, 502.0, 1280.0, 720.0));
    }

    #[test]
    fn select_skips_aux_window_with_no_title() {
        let record = WindowRecord {
            owner: Some(ROK_OWNER.to_owned()),
            title: None,
            id: Some(42),
            pid: Some(21916),
            bounds: Some(rect(0.0, 0.0, 100.0, 100.0)),
        };
        assert!(select_rok_window(&record).is_none());
    }

    #[test]
    fn select_skips_aux_window_with_different_title() {
        let record = WindowRecord {
            owner: Some(ROK_OWNER.to_owned()),
            title: Some("Splash".to_owned()),
            id: Some(42),
            pid: Some(21916),
            bounds: Some(rect(0.0, 0.0, 100.0, 100.0)),
        };
        assert!(select_rok_window(&record).is_none());
    }

    #[test]
    fn select_skips_record_with_missing_owner() {
        let record = WindowRecord {
            owner: None,
            title: Some(ROK_TITLE.to_owned()),
            id: Some(1),
            pid: Some(2),
            bounds: Some(rect(0.0, 0.0, 1.0, 1.0)),
        };
        assert!(select_rok_window(&record).is_none());
    }

    #[test]
    fn select_skips_other_apps() {
        let record = WindowRecord {
            owner: Some("Finder".to_owned()),
            title: Some(ROK_TITLE.to_owned()),
            id: Some(42),
            pid: Some(100),
            bounds: Some(rect(0.0, 0.0, 100.0, 100.0)),
        };
        assert!(select_rok_window(&record).is_none());
    }

    #[test]
    fn select_requires_all_required_fields() {
        let base = WindowRecord {
            owner: Some(ROK_OWNER.to_owned()),
            title: Some(ROK_TITLE.to_owned()),
            id: Some(1),
            pid: Some(2),
            bounds: Some(rect(0.0, 0.0, 1.0, 1.0)),
        };
        let mut r = base.clone();
        r.id = None;
        assert!(select_rok_window(&r).is_none(), "missing id should reject");
        let mut r = base.clone();
        r.pid = None;
        assert!(select_rok_window(&r).is_none(), "missing pid should reject");
        let mut r = base;
        r.bounds = None;
        assert!(
            select_rok_window(&r).is_none(),
            "missing bounds should reject"
        );
    }

    #[test]
    fn window_center_is_geometric_midpoint() {
        let w = Window {
            id: 1,
            pid: 2,
            frame: rect(100.0, 200.0, 1280.0, 720.0),
        };
        let c = w.center();
        assert!((c.x - 740.0).abs() < f64::EPSILON);
        assert!((c.y - 560.0).abs() < f64::EPSILON);
    }

    #[test]
    fn window_center_zero_size_frame_returns_origin() {
        // Degenerate but real during window-resize transitions.
        let w = Window {
            id: 1,
            pid: 2,
            frame: rect(50.0, 60.0, 0.0, 0.0),
        };
        let c = w.center();
        assert!((c.x - 50.0).abs() < f64::EPSILON);
        assert!((c.y - 60.0).abs() < f64::EPSILON);
    }

    // ---- Bundle ID validation (Codex #2 + Claude A4) ----

    #[test]
    fn matches_rok_bundle_id_accepts_vn() {
        assert!(matches_rok_bundle_id("com.rok.ios.vn"));
    }

    #[test]
    fn matches_rok_bundle_id_accepts_other_regions() {
        assert!(matches_rok_bundle_id("com.rok.ios.kr"));
        assert!(matches_rok_bundle_id("com.rok.ios.us"));
        assert!(matches_rok_bundle_id("com.rok.ios.eu"));
    }

    #[test]
    fn matches_rok_bundle_id_rejects_spoofs() {
        // Pretend-RoK from a launcher or impersonator.
        assert!(!matches_rok_bundle_id("com.example.fake-rok"));
        assert!(!matches_rok_bundle_id("RiseOfKingdoms"));
        assert!(!matches_rok_bundle_id("org.rok.ios.vn")); // wrong TLD
        assert!(!matches_rok_bundle_id(""));
        assert!(!matches_rok_bundle_id("com.finder.app"));
    }

    #[test]
    fn matches_rok_bundle_id_rejects_prefix_only() {
        // Exact prefix without a region suffix is suspicious — but allowed
        // as a forward-compat call, since a future "com.rok.ios.global"
        // would also be valid. Document that any com.rok.ios.* is trusted.
        assert!(matches_rok_bundle_id("com.rok.ios."));
    }

    #[test]
    fn window_center_handles_negative_origin() {
        // Per P7 spike: RoK on virtual display had origin (-525, 502).
        let w = Window {
            id: 64793,
            pid: 21916,
            frame: rect(-525.0, 502.0, 1280.0, 720.0),
        };
        let c = w.center();
        assert!((c.x - 115.0).abs() < f64::EPSILON);
        assert!((c.y - 862.0).abs() < f64::EPSILON);
    }
}
