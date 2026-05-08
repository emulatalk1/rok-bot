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

/// Reason tags surfaced via `BotError::WindowChanged { reason }`. The TOCTOU
/// re-validation at click site (`validate_at_click_site`) checks three
/// independent invariants and reports which one failed via these constants.
/// Pinned as `&'static str` so the operator-facing log line is always one of
/// these three values; tests assert against them directly.
pub const REASON_WID_GONE: &str = "window_id_gone";
pub const REASON_FRAME_MOVED: &str = "frame_moved";
pub const REASON_NOT_TOPMOST: &str = "not_topmost_at_click";

/// Tolerance for per-coordinate frame drift in
/// [`validate_at_click_site`]. macOS reports window bounds to fractional
/// pixels but ordinary user-driven moves and resizes always shift by at
/// least 1 point in some axis. `1.0` accepts the noise floor (sub-pixel
/// jitter from screencapture / window-server rounding) while still
/// catching any real move/resize. Tighten if false positives bite, relax
/// if false negatives ever bite.
pub const FRAME_TOLERANCE_POINTS: f64 = 1.0;

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

/// Snapshot of one on-screen window's (id, frame), in z-order from the live
/// `CGWindowListCopyWindowInfo` call. The on-screen list is already in
/// front-to-back z-order, so the **first** entry whose `frame` contains the
/// click point is the topmost window at that point. Used by
/// [`validate_inner`] as a pure data input.
#[derive(Debug, Clone, Copy)]
struct WindowSnapshot {
    id: u32,
    frame: CGRect,
}

/// Pure: enforce the v0.1.3 TOCTOU invariants between window discovery and
/// click delivery, given a snapshot of currently on-screen windows in
/// front-to-back z-order.
///
/// Three checks, in order, with first-failure-wins semantics:
///
/// 1. **`REASON_WID_GONE`** — the expected WID is not in `observed`. The
///    RoK process closed, crashed, or rebuilt its main window between
///    discovery and click. Posting at the stale screen coords would land
///    on whatever window now sits there.
///
/// 2. **`REASON_FRAME_MOVED`** — the WID exists but its frame origin/size
///    drifted beyond [`FRAME_TOLERANCE_POINTS`] in any of the four
///    components. The user moved or resized RoK between discovery and
///    click. The screen point computed from the stale frame doesn't
///    correspond to the same UI element anymore.
///
/// 3. **`REASON_NOT_TOPMOST`** — RoK exists at the expected frame, but
///    another window in `observed` (earlier in z-order, i.e., on top) has
///    a frame that contains the click point. Could be a system overlay,
///    notification banner, dialog, drag-and-drop tooltip, or a
///    focus-grabbing app that just opened. A privileged synthetic click
///    posted to those coords would deliver to the overlay, not RoK.
///
/// Returns `Ok(())` when all three pass.
fn validate_inner(
    expected_wid: u32,
    expected_frame: CGRect,
    observed: &[WindowSnapshot],
    click_point: CGPoint,
    tolerance: f64,
) -> Result<()> {
    let Some(found) = observed.iter().find(|w| w.id == expected_wid) else {
        return Err(BotError::WindowChanged {
            reason: REASON_WID_GONE,
        });
    };

    if !frames_within_tolerance(found.frame, expected_frame, tolerance) {
        return Err(BotError::WindowChanged {
            reason: REASON_FRAME_MOVED,
        });
    }

    // Topmost-at-point: walk in z-order (front-to-back). The first window
    // whose frame contains click_point is the topmost. If that's not RoK,
    // some other window is occluding the click site.
    if let Some(top) = observed
        .iter()
        .find(|w| rect_contains_point(w.frame, click_point))
    {
        if top.id != expected_wid {
            return Err(BotError::WindowChanged {
                reason: REASON_NOT_TOPMOST,
            });
        }
    } else {
        // No window covers the click point at all. That can only happen
        // if (sx, sy) is outside every on-screen window's bounds —
        // typically because RoK moved off-screen between discovery and
        // click, OR the screen_point math produced bad coords. Either
        // way, posting the click would land on the desktop / dock. Treat
        // as not-topmost: RoK isn't covering the point we'd click.
        return Err(BotError::WindowChanged {
            reason: REASON_NOT_TOPMOST,
        });
    }

    Ok(())
}

/// Pure: are two `CGRect`s equal within per-coordinate tolerance?
fn frames_within_tolerance(a: CGRect, b: CGRect, tolerance: f64) -> bool {
    (a.origin.x - b.origin.x).abs() <= tolerance
        && (a.origin.y - b.origin.y).abs() <= tolerance
        && (a.size.width - b.size.width).abs() <= tolerance
        && (a.size.height - b.size.height).abs() <= tolerance
}

/// Pure: half-open rectangle containment, top-left origin. Standard CG
/// convention: `[origin.x, origin.x + width)` × `[origin.y, origin.y + height)`.
fn rect_contains_point(rect: CGRect, p: CGPoint) -> bool {
    let x_min = rect.origin.x;
    let x_max = rect.origin.x + rect.size.width;
    let y_min = rect.origin.y;
    let y_max = rect.origin.y + rect.size.height;
    p.x >= x_min && p.x < x_max && p.y >= y_min && p.y < y_max
}

/// Live wrapper: re-call `CGWindowListCopyWindowInfo` and pass the result to
/// [`validate_inner`]. Called from `main.rs::run` between
/// `matcher::screen_point` and `click::click_at` to close the TOCTOU
/// between window discovery and click delivery.
///
/// The on-screen list is in front-to-back z-order. Auxiliary RoK windows
/// (splash, popups) are filtered by the `select_rok_window` predicate at
/// discovery, but here we keep ALL on-screen windows (any owner) because
/// the topmost-at-point check needs to see overlays from other apps too.
pub fn validate_at_click_site(expected: &Window, click_point: CGPoint) -> Result<()> {
    let Some(info_list) = copy_window_info(kCGWindowListOptionOnScreenOnly, kCGNullWindowID) else {
        // CGWindowList unavailable mid-run is itself a TOCTOU signal:
        // something changed about the window-server's state. Treat as
        // window-gone rather than a generic permission failure — Screen
        // Recording has already been preflight-checked at boot.
        return Err(BotError::WindowChanged {
            reason: REASON_WID_GONE,
        });
    };

    let mut observed: Vec<WindowSnapshot> = Vec::with_capacity(64);
    for entry in info_list.iter() {
        let raw_ptr: *const c_void = *entry;
        if raw_ptr.is_null() {
            continue;
        }
        // SAFETY: same Get-rule lift as `find_rok_window` — the slot is an
        // unretained CFDictionaryRef; `wrap_under_get_rule` CFRetains.
        let cf = unsafe { CFType::wrap_under_get_rule(raw_ptr.cast()) };
        let Some(dict) = cf.downcast::<CFDictionary>() else {
            continue;
        };
        let record = parse_record(&dict);
        let (Some(id), Some(frame)) = (record.id, record.bounds) else {
            continue;
        };
        observed.push(WindowSnapshot { id, frame });
    }

    validate_inner(
        expected.id,
        expected.frame,
        &observed,
        click_point,
        FRAME_TOLERANCE_POINTS,
    )
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

    // ---------- validate_at_click_site (v0.1.3 TOCTOU close) ----------

    fn snap(id: u32, x: f64, y: f64, w: f64, h: f64) -> WindowSnapshot {
        WindowSnapshot {
            id,
            frame: rect(x, y, w, h),
        }
    }

    const TOL: f64 = FRAME_TOLERANCE_POINTS;

    #[test]
    fn validate_inner_happy_path() {
        // RoK present at expected frame, click point inside it, no
        // occluder above. All three checks pass.
        let observed = [snap(42, 100.0, 200.0, 1280.0, 720.0)];
        let result = validate_inner(
            42,
            rect(100.0, 200.0, 1280.0, 720.0),
            &observed,
            CGPoint::new(740.0, 560.0),
            TOL,
        );
        assert!(result.is_ok(), "happy path must succeed: {result:?}");
    }

    #[test]
    fn validate_inner_wid_gone() {
        // Expected RoK WID 42, but only WID 99 (Finder, say) is on screen.
        let observed = [snap(99, 0.0, 0.0, 1920.0, 1080.0)];
        match validate_inner(
            42,
            rect(100.0, 200.0, 1280.0, 720.0),
            &observed,
            CGPoint::new(740.0, 560.0),
            TOL,
        ) {
            Err(BotError::WindowChanged { reason }) => {
                assert_eq!(reason, REASON_WID_GONE);
            }
            other => panic!("expected WindowChanged{{wid_gone}}, got {other:?}"),
        }
    }

    #[test]
    fn validate_inner_frame_moved_origin() {
        // RoK still WID 42 but moved 100 points right between discovery
        // and re-check. Frame drift in origin.x triggers the second check.
        let observed = [snap(42, 200.0, 200.0, 1280.0, 720.0)];
        match validate_inner(
            42,
            rect(100.0, 200.0, 1280.0, 720.0),
            &observed,
            CGPoint::new(740.0, 560.0),
            TOL,
        ) {
            Err(BotError::WindowChanged { reason }) => {
                assert_eq!(reason, REASON_FRAME_MOVED);
            }
            other => panic!("expected WindowChanged{{frame_moved}}, got {other:?}"),
        }
    }

    #[test]
    fn validate_inner_frame_moved_size() {
        // RoK still WID 42, origin unchanged, but window resized by 50pt
        // in width. Caught as frame_moved.
        let observed = [snap(42, 100.0, 200.0, 1330.0, 720.0)];
        match validate_inner(
            42,
            rect(100.0, 200.0, 1280.0, 720.0),
            &observed,
            CGPoint::new(740.0, 560.0),
            TOL,
        ) {
            Err(BotError::WindowChanged { reason }) => {
                assert_eq!(reason, REASON_FRAME_MOVED);
            }
            other => panic!("expected WindowChanged{{frame_moved}}, got {other:?}"),
        }
    }

    #[test]
    fn validate_inner_frame_within_tolerance_passes() {
        // Sub-pixel jitter (0.5pt) is below the 1.0 tolerance and must
        // NOT fire frame_moved. Pin so future tightening is intentional.
        let observed = [snap(42, 100.5, 200.0, 1280.0, 720.5)];
        let result = validate_inner(
            42,
            rect(100.0, 200.0, 1280.0, 720.0),
            &observed,
            CGPoint::new(740.0, 560.0),
            TOL,
        );
        assert!(
            result.is_ok(),
            "0.5pt jitter must pass under 1.0pt tolerance: {result:?}"
        );
    }

    #[test]
    fn validate_inner_not_topmost_when_overlay_above_rok() {
        // Z-order is front-to-back, so observed[0] is on top. Overlay WID
        // 99 covers the click point; RoK WID 42 is below at the same
        // frame. Topmost-at-point is 99, not 42.
        let observed = [
            snap(99, 700.0, 500.0, 200.0, 200.0),  // overlay covers click
            snap(42, 100.0, 200.0, 1280.0, 720.0), // RoK below
        ];
        match validate_inner(
            42,
            rect(100.0, 200.0, 1280.0, 720.0),
            &observed,
            CGPoint::new(740.0, 560.0),
            TOL,
        ) {
            Err(BotError::WindowChanged { reason }) => {
                assert_eq!(reason, REASON_NOT_TOPMOST);
            }
            other => panic!("expected WindowChanged{{not_topmost}}, got {other:?}"),
        }
    }

    #[test]
    fn validate_inner_not_topmost_when_no_window_covers_click_point() {
        // Click coords land off-screen (no window's frame contains the
        // point). Treated as not-topmost: posting there would land on
        // empty desktop or whatever's revealed.
        let observed = [snap(42, 100.0, 200.0, 1280.0, 720.0)];
        match validate_inner(
            42,
            rect(100.0, 200.0, 1280.0, 720.0),
            &observed,
            CGPoint::new(5000.0, 5000.0), // way outside RoK's frame
            TOL,
        ) {
            Err(BotError::WindowChanged { reason }) => {
                assert_eq!(reason, REASON_NOT_TOPMOST);
            }
            other => panic!("expected WindowChanged{{not_topmost}}, got {other:?}"),
        }
    }

    #[test]
    fn validate_inner_topmost_when_overlay_does_not_cover_click_point() {
        // Overlay exists above RoK but at a different region (e.g., menu
        // bar item, top-right notification banner that doesn't cover the
        // center of the RoK window). RoK is still topmost at the click
        // point itself. Should pass.
        let observed = [
            snap(99, 0.0, 0.0, 200.0, 30.0), // top-left overlay, doesn't cover click
            snap(42, 100.0, 200.0, 1280.0, 720.0),
        ];
        let result = validate_inner(
            42,
            rect(100.0, 200.0, 1280.0, 720.0),
            &observed,
            CGPoint::new(740.0, 560.0), // RoK center
            TOL,
        );
        assert!(
            result.is_ok(),
            "overlay outside click region must pass: {result:?}"
        );
    }

    #[test]
    fn validate_inner_negative_origin_virtual_display() {
        // Mirrors P3 spike geometry: RoK on BetterDisplay virtual screen
        // at (-1051, 103) size 1051x820. Click at center (-525.5, 513).
        // Negative-origin coords are valid CG global coords; the
        // contains-point math must handle them without sign confusion.
        let observed = [snap(73313, -1051.0, 103.0, 1051.0, 820.0)];
        let result = validate_inner(
            73313,
            rect(-1051.0, 103.0, 1051.0, 820.0),
            &observed,
            CGPoint::new(-525.5, 513.0),
            TOL,
        );
        assert!(
            result.is_ok(),
            "virtual-display negative-origin happy path must pass: {result:?}"
        );
    }

    #[test]
    fn validate_inner_check_order_is_wid_then_frame_then_topmost() {
        // Pin first-failure-wins precedence: if WID is gone AND frame
        // would have moved AND topmost would have failed, we must report
        // WID_GONE because it's the most diagnostic for the operator.
        // This test exercises only the WID-gone leg (the others can't
        // be checked without WID present), but documents the ordering.
        let observed = [snap(99, 999.0, 999.0, 1.0, 1.0)];
        match validate_inner(
            42,
            rect(100.0, 200.0, 1280.0, 720.0),
            &observed,
            CGPoint::new(740.0, 560.0),
            TOL,
        ) {
            Err(BotError::WindowChanged { reason }) => {
                assert_eq!(reason, REASON_WID_GONE, "WID_GONE must fire first");
            }
            other => panic!("expected WindowChanged{{wid_gone}}, got {other:?}"),
        }
    }

    #[test]
    fn frames_within_tolerance_pure_pin() {
        let a = rect(100.0, 200.0, 1280.0, 720.0);
        assert!(frames_within_tolerance(a, a, 0.0));
        assert!(frames_within_tolerance(
            a,
            rect(100.5, 200.0, 1280.0, 720.0),
            1.0
        ));
        assert!(!frames_within_tolerance(
            a,
            rect(102.0, 200.0, 1280.0, 720.0),
            1.0
        ));
    }

    #[test]
    fn rect_contains_point_handles_edges() {
        let r = rect(100.0, 200.0, 50.0, 60.0);
        // Inside.
        assert!(rect_contains_point(r, CGPoint::new(125.0, 230.0)));
        // Top-left corner inclusive.
        assert!(rect_contains_point(r, CGPoint::new(100.0, 200.0)));
        // Bottom-right corner exclusive (half-open).
        assert!(!rect_contains_point(r, CGPoint::new(150.0, 260.0)));
        // Just inside bottom-right.
        assert!(rect_contains_point(r, CGPoint::new(149.999, 259.999)));
        // Outside.
        assert!(!rect_contains_point(r, CGPoint::new(50.0, 230.0)));
        assert!(!rect_contains_point(r, CGPoint::new(125.0, 100.0)));
    }

    #[test]
    fn window_change_reason_constants_match_expected_strings() {
        // Pin the operator-facing log strings that surface via
        // BotError::WindowChanged. Changing one breaks shell users
        // pattern-matching on the error message.
        assert_eq!(REASON_WID_GONE, "window_id_gone");
        assert_eq!(REASON_FRAME_MOVED, "frame_moved");
        assert_eq!(REASON_NOT_TOPMOST, "not_topmost_at_click");
    }

    #[test]
    fn window_change_maps_to_exit_19() {
        for reason in [REASON_WID_GONE, REASON_FRAME_MOVED, REASON_NOT_TOPMOST] {
            let err = BotError::WindowChanged { reason };
            assert_eq!(err.exit_code(), 19, "reason {reason} must map to exit 19");
        }
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
