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

/// One window's id/pid/frame plus optional owner+title strings, parsed
/// from a single `CGWindowListCopyWindowInfo` dictionary entry. Used as
/// pure data input by [`select_rok_window`], [`validate_inner`], and
/// [`validate_present_inner`].
///
/// `id`, `pid`, and `frame` are required — `parse_snapshot` returns
/// `None` if any are missing or invalid in the source dictionary. This
/// matches the v0.1.4 invariant that downstream validators can rely on
/// these three fields without re-checking, encoded in the type system.
///
/// `owner_name` and `title` are `Option<String>` because RoK auxiliary
/// windows (splashes, popups) legitimately omit the title, and a few
/// system windows have no owner-name. `select_rok_window` checks both
/// before promoting a snapshot to a `Window`. v0.1.5 PID-anchored hidden-
/// Space checks (C2) also consult `owner_name` for operator-readable
/// diagnostics.
#[derive(Debug, Clone)]
struct WindowSnapshot {
    id: u32,
    pid: i32,
    frame: CGRect,
    owner_name: Option<String>,
    title: Option<String>,
}

/// Pure: turn a parsed snapshot into a `Window` iff it matches the RoK
/// main window. Auxiliary RoK windows (splash, popups) share `owner_name`
/// but have no title or a different title and return `None`.
fn select_rok_window(snap: &WindowSnapshot) -> Option<Window> {
    let owner = snap.owner_name.as_deref()?;
    let title = snap.title.as_deref()?;
    if owner != ROK_OWNER || title != ROK_TITLE {
        return None;
    }
    Some(Window {
        id: snap.id,
        pid: snap.pid,
        frame: snap.frame,
    })
}

/// Walk `CGWindowListCopyWindowInfo` with the given option flag and return
/// a vec of snapshots for every dict that has id+pid+frame. Dicts missing
/// any of those three fields (rare — invalid/dead windows) are skipped.
/// Returns `None` if `copy_window_info` itself fails (Screen Recording
/// TCC revoked mid-session, `WindowServer` crash, etc.); each call site
/// maps that to its own `BotError` (typically `WindowNotFound` for
/// discovery or `WindowChanged { REASON_WID_GONE }` for mid-flow re-checks).
///
/// Pre-v0.1.5 the dict-walking loop was duplicated across `find_rok_window`,
/// `validate_at_click_site`, and `validate_window_present`. Centralizing
/// it here gives the v0.1.5 hidden-Space check a single seam to swap
/// `kCGWindowListOptionOnScreenOnly` for `kCGWindowListOptionAll` (C2),
/// and keeps `parse_snapshot` as the only place that touches CG private
/// CFString constants.
fn collect_window_snapshots(option: u32) -> Option<Vec<WindowSnapshot>> {
    let info_list = copy_window_info(option, kCGNullWindowID)?;
    let mut snapshots: Vec<WindowSnapshot> = Vec::with_capacity(64);
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
        if let Some(snap) = parse_snapshot(&dict) {
            snapshots.push(snap);
        }
    }
    Some(snapshots)
}

/// Walk the live `CGWindowListCopyWindowInfo` array and return the first
/// window matching the RoK main-window predicate AND backed by a process
/// whose bundle ID starts with `ROK_BUNDLE_PREFIX` (anti-spoof check).
/// Spoofs are skipped with a `tracing::warn!` so they're visible in logs
/// without halting the search — the real RoK window may be later in the
/// list.
pub fn find_rok_window() -> Result<Window> {
    let snapshots = collect_window_snapshots(kCGWindowListOptionOnScreenOnly)
        .ok_or(BotError::WindowNotFound)?;

    for snap in &snapshots {
        if let Some(window) = select_rok_window(snap) {
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

/// Pure: enforce the v0.1.4 post-capture TOCTOU subset — the WID still
/// exists and its frame is within tolerance, but **does not** check
/// topmost-at-click.
///
/// /plan-eng-review Outside Voice F4 caught that re-running the full
/// 3-check at post-capture time false-fails on legitimate state changes:
/// after a successful click, RoK may have spawned a modal/popup (same
/// pid, different WID) that's now topmost. The expected RoK WID is no
/// longer the front-most window at the click point, but the click DID
/// land correctly and we still want to capture the resulting state. The
/// 2-check sibling preserves the diagnostic value of `WindowChanged`
/// (WID gone / frame moved both still produce precise exit-19 messages)
/// while dropping the invariant that doesn't hold post-click.
///
/// Two checks, in order, first-failure-wins:
///
/// 1. **`REASON_WID_GONE`** — same as `validate_inner`. RoK closed,
///    crashed, or rebuilt its main window between click and post-capture.
///    Posting `screencapture -l <stale_wid>` would either fail (good —
///    `capture_with_bin`'s 0-byte gate catches it) or capture a different
///    window (bad — would corrupt the verify pixel-diff).
///
/// 2. **`REASON_FRAME_MOVED`** — same as `validate_inner`. RoK got
///    dragged/resized between click and post-capture; the post-capture
///    would be misaligned relative to the pre-capture, and pixel-diff
///    would false-positive everywhere.
///
/// Returns `Ok(())` when both pass. The dropped third check
/// (`REASON_NOT_TOPMOST`) is **not** an invariant post-click and is
/// not enforced here.
fn validate_present_inner(
    expected_wid: u32,
    expected_frame: CGRect,
    observed: &[WindowSnapshot],
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

    Ok(())
}

/// Live wrapper: re-call `CGWindowListCopyWindowInfo` and pass the result
/// to [`validate_present_inner`]. Called from `main.rs::run` between
/// `click::click_at` returning and the post-click `capture_window` to
/// close the TOCTOU between click delivery and post-capture.
///
/// Structurally similar to `validate_at_click_site` but uses the 2-check
/// variant (no topmost). See `validate_present_inner` docs for why the
/// topmost invariant doesn't hold post-click.
pub fn validate_window_present(expected: &Window) -> Result<()> {
    let observed = collect_window_snapshots(kCGWindowListOptionOnScreenOnly).ok_or(
        BotError::WindowChanged {
            reason: REASON_WID_GONE,
        },
    )?;

    validate_present_inner(
        expected.id,
        expected.frame,
        &observed,
        FRAME_TOLERANCE_POINTS,
    )
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
    // CGWindowList unavailable mid-run is itself a TOCTOU signal:
    // something changed about the window-server's state. Treat as
    // window-gone rather than a generic permission failure — Screen
    // Recording has already been preflight-checked at boot.
    let observed = collect_window_snapshots(kCGWindowListOptionOnScreenOnly).ok_or(
        BotError::WindowChanged {
            reason: REASON_WID_GONE,
        },
    )?;

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

/// Parse one `CGWindowListCopyWindowInfo` dictionary entry into a
/// `WindowSnapshot`. Returns `None` if any of id/pid/frame is missing or
/// fails its type/range check — those three fields are required so the
/// downstream validators can rely on them. `owner_name` and `title` are
/// optional (some auxiliary RoK windows omit title; rare system windows
/// omit owner-name).
fn parse_snapshot(dict: &CFDictionary) -> Option<WindowSnapshot> {
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
    let id = dict_get::<CFNumber>(dict, k_id)
        .and_then(|n| n.to_i64())
        .and_then(|v| u32::try_from(v).ok())?;
    let pid = dict_get::<CFNumber>(dict, k_pid)
        .and_then(|n| n.to_i64())
        .and_then(|v| i32::try_from(v).ok())?;
    let frame = dict_get::<CFDictionary>(dict, k_bounds).and_then(|d| rect_from_dict(&d))?;
    let owner_name = dict_get::<CFString>(dict, k_owner).map(|s| s.to_string());
    let title = dict_get::<CFString>(dict, k_title).map(|s| s.to_string());
    Some(WindowSnapshot {
        id,
        pid,
        frame,
        owner_name,
        title,
    })
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

    /// Build a `WindowSnapshot` for `select_rok_window` unit tests. Carries
    /// optional owner+title (most `select_*` tests vary these); id/pid/frame
    /// are pinned to harmless defaults.
    fn rok_snap(owner: Option<&str>, title: Option<&str>) -> WindowSnapshot {
        WindowSnapshot {
            id: 64793,
            pid: 21916,
            frame: rect(-525.0, 502.0, 1280.0, 720.0),
            owner_name: owner.map(str::to_owned),
            title: title.map(str::to_owned),
        }
    }

    #[test]
    fn select_main_window_full_match() {
        let snap = rok_snap(Some(ROK_OWNER), Some(ROK_TITLE));
        let window = select_rok_window(&snap).expect("should match");
        assert_eq!(window.id, 64793);
        assert_eq!(window.pid, 21916);
        assert_rect_eq(window.frame, rect(-525.0, 502.0, 1280.0, 720.0));
    }

    #[test]
    fn select_skips_aux_window_with_no_title() {
        let snap = rok_snap(Some(ROK_OWNER), None);
        assert!(select_rok_window(&snap).is_none());
    }

    #[test]
    fn select_skips_aux_window_with_different_title() {
        let snap = rok_snap(Some(ROK_OWNER), Some("Splash"));
        assert!(select_rok_window(&snap).is_none());
    }

    #[test]
    fn select_skips_record_with_missing_owner() {
        let snap = rok_snap(None, Some(ROK_TITLE));
        assert!(select_rok_window(&snap).is_none());
    }

    #[test]
    fn select_skips_other_apps() {
        let snap = rok_snap(Some("Finder"), Some(ROK_TITLE));
        assert!(select_rok_window(&snap).is_none());
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

    /// Build a `WindowSnapshot` for the `validate_inner` / `validate_present_inner`
    /// tests. PID isn't checked by these v0.1.3 validators (added in v0.1.5
    /// C2), but the field is set to a non-zero stub so future readers don't
    /// mistake the default for "PID = 0 means unset".
    fn snap(id: u32, x: f64, y: f64, w: f64, h: f64) -> WindowSnapshot {
        WindowSnapshot {
            id,
            pid: 21916,
            frame: rect(x, y, w, h),
            owner_name: None,
            title: None,
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

    // ---------- validate_present_inner (v0.1.4 post-capture TOCTOU subset) ----------

    #[test]
    fn validate_present_inner_passes_when_wid_present_and_frame_within_tolerance() {
        // Happy path: same WID, frame jitter under tolerance. The
        // 2-check sibling deliberately does NOT consider topmost-at-
        // click, so we don't pass a click point — the function signature
        // makes the dropped invariant unrepresentable.
        let observed = [snap(42, 100.0, 200.0, 1280.0, 720.0)];
        let result = validate_present_inner(42, rect(100.0, 200.0, 1280.0, 720.0), &observed, TOL);
        assert!(result.is_ok(), "happy path must succeed: {result:?}");
    }

    #[test]
    fn validate_present_inner_wid_gone() {
        // RoK closed/crashed between click and post-capture.
        let observed = [snap(99, 0.0, 0.0, 1920.0, 1080.0)];
        match validate_present_inner(42, rect(100.0, 200.0, 1280.0, 720.0), &observed, TOL) {
            Err(BotError::WindowChanged { reason }) => {
                assert_eq!(reason, REASON_WID_GONE);
            }
            other => panic!("expected WindowChanged{{wid_gone}}, got {other:?}"),
        }
    }

    #[test]
    fn validate_present_inner_frame_moved_origin() {
        // RoK still WID 42 but moved 100pt between click and post-capture.
        // Post-capture would be misaligned vs pre — pixel-diff would
        // false-positive on virtually every pixel.
        let observed = [snap(42, 200.0, 200.0, 1280.0, 720.0)];
        match validate_present_inner(42, rect(100.0, 200.0, 1280.0, 720.0), &observed, TOL) {
            Err(BotError::WindowChanged { reason }) => {
                assert_eq!(reason, REASON_FRAME_MOVED);
            }
            other => panic!("expected WindowChanged{{frame_moved}}, got {other:?}"),
        }
    }

    #[test]
    fn validate_present_inner_frame_moved_size() {
        // Window resized by 50pt in width mid-flow; same WID, different
        // frame.
        let observed = [snap(42, 100.0, 200.0, 1330.0, 720.0)];
        match validate_present_inner(42, rect(100.0, 200.0, 1280.0, 720.0), &observed, TOL) {
            Err(BotError::WindowChanged { reason }) => {
                assert_eq!(reason, REASON_FRAME_MOVED);
            }
            other => panic!("expected WindowChanged{{frame_moved}}, got {other:?}"),
        }
    }

    #[test]
    fn validate_present_inner_passes_with_overlay_on_top() {
        // The structural difference from validate_inner: an overlay
        // (WID 99, on top in z-order) covering the click point would
        // fire REASON_NOT_TOPMOST in the 3-check variant. The 2-check
        // sibling MUST pass this — it's the whole reason for the split
        // (post-click overlays/modals are legitimate state changes).
        let observed = [
            snap(99, 700.0, 500.0, 200.0, 200.0), // overlay (e.g. modal spawned by click)
            snap(42, 100.0, 200.0, 1280.0, 720.0), // RoK below
        ];
        let result = validate_present_inner(42, rect(100.0, 200.0, 1280.0, 720.0), &observed, TOL);
        assert!(
            result.is_ok(),
            "overlay above RoK must NOT fire WindowChanged in the 2-check sibling: {result:?}"
        );
    }

    #[test]
    fn validate_present_inner_frame_within_tolerance_passes() {
        // Sub-pixel jitter under the 1.0 tolerance must NOT fire frame_moved.
        let observed = [snap(42, 100.5, 200.0, 1280.0, 720.5)];
        let result = validate_present_inner(42, rect(100.0, 200.0, 1280.0, 720.0), &observed, TOL);
        assert!(
            result.is_ok(),
            "0.5pt jitter must pass under 1.0pt tolerance: {result:?}"
        );
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
