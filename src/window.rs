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

/// Reason tags surfaced via `BotError::WindowChanged { reason }`. Each
/// check in the v0.1.5 validation pipeline maps to one of these constants;
/// pinned as `&'static str` so the operator-facing log line is always
/// one of these four values and tests assert against them directly.
///
/// **v0.1.5 changes vs v0.1.3:**
/// - `REASON_NOT_TOPMOST` was deleted. The 3-site hidden-Space check
///   subsumes most of its operator value (the click-time occluder case
///   is now caught earlier as `REASON_NOT_VISIBLE`), and `kAXPressAction`
///   on a Catalyst Bridge app like RoK delivers through z-order overlap
///   anyway — see `learnings/ax-press-works-catalyst`.
/// - `REASON_NOT_VISIBLE` is new: the WID+PID pair exists in
///   `kCGWindowListOptionAll` but is missing from
///   `kCGWindowListOptionOnScreenOnly`. Covers hidden Space (another
///   app went fullscreen and pushed RoK to a separate Space), minimized
///   to Dock, and transient `WindowServer` states that hide a window
///   without destroying it. Distinct from `REASON_WID_GONE` so the
///   operator knows whether to switch Spaces vs restart RoK.
/// - `REASON_POINT_OUTSIDE_FRAME` is new: the click point computed
///   from `screen_point(&match, &window.frame)` is not inside the
///   discovered frame. Catches bad coord math (negative origin sign
///   flip, off-by-one) and pure validation: under v0.1.3 the topmost
///   walk accidentally enforced this; without explicit checking, the
///   AX press could deliver at unintended desktop coords.
pub const REASON_WID_GONE: &str = "window_id_gone";
pub const REASON_FRAME_MOVED: &str = "frame_moved";
pub const REASON_NOT_VISIBLE: &str = "not_visible";
pub const REASON_POINT_OUTSIDE_FRAME: &str = "point_outside_frame";

/// `kCGWindowListOptionAll` = 0. The `core-graphics` 0.x crate exposes only
/// `kCGWindowListOptionOnScreenOnly`; the underlying CG enum uses 0 as the
/// "no on-screen filter" value (every window the calling process is allowed
/// to see, including those on hidden Spaces and minimized to the Dock).
/// Declared locally to avoid waiting on a crate update. Verified against
/// Apple's CGWindow.h: `enum { kCGWindowListOptionAll = 0, ... }`.
const K_CG_WINDOW_LIST_OPTION_ALL: u32 = 0;

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
///
/// **v0.1.5 hidden-Space distinction.** Three outcomes:
///
/// 1. **`Ok(window)`** — RoK is on a visible Space. Happy path; matches
///    against `kCGWindowListOptionOnScreenOnly`.
/// 2. **`Err(WindowChanged { REASON_NOT_VISIBLE })`** — RoK process is
///    running and has a window, but the window isn't on a currently-
///    displayed Space (typical when another app went macOS-native
///    fullscreen and pushed RoK behind it, or RoK was minimized to
///    Dock). Detected by falling back to `kCGWindowListOptionAll`.
///    Operator's fix is "switch to RoK's Space" or "unminimize",
///    which is meaningfully different from "start RoK." See
///    `learnings/ax-press-fails-hidden-space` for why Mode 1 cannot
///    click through to a hidden-Space window even with AX, and the
///    broader rationale for distinguishing this state.
/// 3. **`Err(WindowNotFound)`** — RoK isn't running at all. Operator
///    needs to launch it.
pub fn find_rok_window() -> Result<Window> {
    let onscreen = collect_window_snapshots(kCGWindowListOptionOnScreenOnly)
        .ok_or(BotError::WindowNotFound)?;
    if let Some(window) = first_rok_window(&onscreen) {
        return Ok(window);
    }
    // Not on a currently-displayed Space. Falling back to `Option=All`
    // includes hidden Spaces, minimized, and transient WindowServer
    // states. If RoK is found there, the process is alive but the
    // window isn't reachable for capture/click — surface that
    // distinctly from "not running."
    let all =
        collect_window_snapshots(K_CG_WINDOW_LIST_OPTION_ALL).ok_or(BotError::WindowNotFound)?;
    if first_rok_window(&all).is_some() {
        return Err(BotError::WindowChanged {
            reason: REASON_NOT_VISIBLE,
        });
    }
    Err(BotError::WindowNotFound)
}

/// Pure-ish helper: return the first snapshot in `snapshots` that matches
/// the RoK main-window predicate AND the bundle-ID anti-spoof gate. Spoof
/// candidates are skipped with `tracing::warn!` so a stale or impersonator
/// window earlier in the list doesn't block a legitimate match later.
/// `bundle_id_for_pid` is a live AppKit call, hence "pure-ish"; mockable
/// pure logic stays inside `select_rok_window` + `matches_rok_bundle_id`.
fn first_rok_window(snapshots: &[WindowSnapshot]) -> Option<Window> {
    for snap in snapshots {
        if let Some(window) = select_rok_window(snap) {
            if bundle_id_for_pid(window.pid)
                .as_deref()
                .is_some_and(matches_rok_bundle_id)
            {
                return Some(window);
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
    None
}

/// Pure: does this bundle ID belong to a legitimate RoK install?
fn matches_rok_bundle_id(bundle_id: &str) -> bool {
    bundle_id.starts_with(ROK_BUNDLE_PREFIX)
}

/// Pure: enforce the v0.1.5 TOCTOU invariants between window discovery
/// and click delivery, given paired snapshots of currently-displayed
/// (`onscreen`) and all-known (`all`) windows.
///
/// Four checks, in order, with first-failure-wins semantics so the
/// operator sees the most diagnostic reason:
///
/// 1. **`REASON_WID_GONE`** — the expected (WID, PID) pair is not in
///    `all`. Either RoK closed/crashed, or the numeric WID was reused
///    by an unrelated window after RoK's window was destroyed. PID-
///    anchored lookup catches both. Without the PID anchor, a WID-reuse
///    by another process would have masqueraded as "still alive,
///    different state" and let downstream code AX-press into the wrong
///    window.
///
/// 2. **`REASON_NOT_VISIBLE`** — the (WID, PID) is in `all` but missing
///    from `onscreen`. The window exists (process is alive, WID is
///    valid) but isn't on a currently-displayed Space — typical when
///    another app went macOS-native fullscreen and pushed RoK to a
///    hidden Space, or RoK was minimized to Dock, or a transient
///    `WindowServer` state hid the window. Distinct exit from `WID_GONE`
///    because the operator's fix is different: switch Spaces / un-
///    minimize, not "restart RoK." Empirically required: AX press
///    returns `kAXErrorFailure` (-25200) against a hidden-Space window
///    even though the AX element query succeeds (`learnings/
///    ax-press-fails-hidden-space`), so failing fast here gives a
///    clean error instead of an opaque `AXError` code at click time.
///
/// 3. **`REASON_FRAME_MOVED`** — the (WID, PID) is on screen but its
///    frame origin/size drifted beyond [`FRAME_TOLERANCE_POINTS`] in
///    any of the four components. The user moved or resized RoK
///    between discovery and click. The screen point computed from the
///    stale frame doesn't correspond to the same UI element anymore.
///
/// 4. **`REASON_POINT_OUTSIDE_FRAME`** — the requested click point is
///    not inside the discovered frame. Pre-v0.1.5 this was an
///    accidental side-effect of the topmost walk (no window contained
///    the point → not-topmost); the v0.1.5 pipeline drops the topmost
///    walk (replaced by hidden-Space + AX delivery's z-order
///    independence) so the bounds check becomes explicit. Catches
///    operator-side coord math bugs (negative-origin sign flip,
///    off-by-one in `screen_point`) before they reach the AX layer.
///
/// Returns `Ok(())` when all four pass.
fn validate_inner(
    expected_wid: u32,
    expected_pid: i32,
    expected_frame: CGRect,
    onscreen: &[WindowSnapshot],
    all: &[WindowSnapshot],
    click_point: CGPoint,
    tolerance: f64,
) -> Result<()> {
    // Check #1: (WID, PID) present in `all`. PID anchor catches WID reuse.
    if !all
        .iter()
        .any(|w| w.id == expected_wid && w.pid == expected_pid)
    {
        return Err(BotError::WindowChanged {
            reason: REASON_WID_GONE,
        });
    }
    // Check #2: same (WID, PID) reachable on a displayed Space.
    let Some(found) = onscreen
        .iter()
        .find(|w| w.id == expected_wid && w.pid == expected_pid)
    else {
        return Err(BotError::WindowChanged {
            reason: REASON_NOT_VISIBLE,
        });
    };
    // Check #3: frame within tolerance vs. discovery snapshot.
    if !frames_within_tolerance(found.frame, expected_frame, tolerance) {
        return Err(BotError::WindowChanged {
            reason: REASON_FRAME_MOVED,
        });
    }
    // Check #4: click point inside the discovered frame. Pre-v0.1.5 this
    // was implicit in the topmost walk; v0.1.5 enforces it explicitly so
    // the bounds invariant survives the topmost deletion.
    if !rect_contains_point(expected_frame, click_point) {
        return Err(BotError::WindowChanged {
            reason: REASON_POINT_OUTSIDE_FRAME,
        });
    }
    Ok(())
}

/// Pure: enforce the v0.1.5 post-capture TOCTOU subset — the (WID, PID)
/// is still alive, still on a visible Space, and the frame is within
/// tolerance. The click-point bounds check is dropped because we've
/// already clicked (the only thing being validated is whether the
/// screen state is still capture-able and pixel-comparable to pre).
///
/// /plan-eng-review Outside Voice F4 (v0.1.4) caught that the
/// pre-click topmost invariant doesn't hold post-click: a successful
/// click may legitimately spawn a modal that becomes topmost. v0.1.5
/// drops the topmost walk from the pre-click pipeline too (replaced
/// by hidden-Space + AX delivery's z-order independence), so the two
/// pipelines now differ only by the click-point bounds check. That
/// asymmetry is preserved: pre needs to know "is the point I'm
/// clicking on the discovered window," post just needs "is the window
/// still capturable in the same place."
///
/// Three checks, in order, first-failure-wins:
///
/// 1. **`REASON_WID_GONE`** — the expected (WID, PID) pair is not in
///    `all`. RoK closed, crashed, or its window was rebuilt with a
///    new WID between click and post-capture. Posting `screencapture
///    -l <stale_wid>` would either fail (`capture_with_bin`'s 0-byte
///    gate catches it) or capture a different window (bad — would
///    corrupt the verify pixel-diff). PID anchor catches WID-reuse
///    by another process.
///
/// 2. **`REASON_NOT_VISIBLE`** — the (WID, PID) is in `all` but
///    missing from `onscreen`. Same conditions as `validate_inner`
///    Check #2: another app went fullscreen, RoK was minimized, or
///    a Space switch hid the window. Post-click capture would
///    `screencapture -l` against a window not on a displayed Space;
///    the captured pixels typically come back blank or stale, and
///    pixel-diff would either false-positive or false-negative.
///
/// 3. **`REASON_FRAME_MOVED`** — same as `validate_inner`. RoK got
///    dragged/resized between click and post-capture; post-capture
///    would be misaligned relative to pre and pixel-diff would
///    false-positive across most pixels.
///
/// Returns `Ok(())` when all three pass.
fn validate_present_inner(
    expected_wid: u32,
    expected_pid: i32,
    expected_frame: CGRect,
    onscreen: &[WindowSnapshot],
    all: &[WindowSnapshot],
    tolerance: f64,
) -> Result<()> {
    if !all
        .iter()
        .any(|w| w.id == expected_wid && w.pid == expected_pid)
    {
        return Err(BotError::WindowChanged {
            reason: REASON_WID_GONE,
        });
    }
    let Some(found) = onscreen
        .iter()
        .find(|w| w.id == expected_wid && w.pid == expected_pid)
    else {
        return Err(BotError::WindowChanged {
            reason: REASON_NOT_VISIBLE,
        });
    };
    if !frames_within_tolerance(found.frame, expected_frame, tolerance) {
        return Err(BotError::WindowChanged {
            reason: REASON_FRAME_MOVED,
        });
    }
    Ok(())
}

/// Live wrapper: re-call `CGWindowListCopyWindowInfo` twice (onscreen +
/// all) and pass the results to [`validate_present_inner`]. Called from
/// `main.rs::run` between `click::click_at` returning and the post-click
/// `capture_window` to close the TOCTOU between click delivery and
/// post-capture.
///
/// Structurally similar to `validate_at_click_site` but skips the
/// click-point bounds check (no click is about to be sent). See
/// `validate_present_inner` docs for why the topmost invariant was
/// dropped from both pipelines.
pub fn validate_window_present(expected: &Window) -> Result<()> {
    let onscreen = collect_window_snapshots(kCGWindowListOptionOnScreenOnly).ok_or(
        BotError::WindowChanged {
            reason: REASON_WID_GONE,
        },
    )?;
    let all =
        collect_window_snapshots(K_CG_WINDOW_LIST_OPTION_ALL).ok_or(BotError::WindowChanged {
            reason: REASON_WID_GONE,
        })?;

    validate_present_inner(
        expected.id,
        expected.pid,
        expected.frame,
        &onscreen,
        &all,
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

/// Live wrapper: re-call `CGWindowListCopyWindowInfo` twice (onscreen +
/// all) and pass the results to [`validate_inner`]. Called from
/// `main.rs::run` between `matcher::screen_point` and `click::click_at`
/// (now `ax::press_at` via `click_at`) to close the TOCTOU between
/// window discovery and click delivery.
///
/// We need BOTH lists, not just `OnScreenOnly`. The on-screen check
/// rules out hidden-Space windows (where AX press would return
/// `kAXErrorFailure -25200`); the all-list check distinguishes that
/// from "RoK truly gone" so the operator gets `REASON_NOT_VISIBLE`
/// rather than the more alarming `REASON_WID_GONE`.
pub fn validate_at_click_site(expected: &Window, click_point: CGPoint) -> Result<()> {
    // CGWindowList unavailable mid-run is itself a TOCTOU signal:
    // something changed about the window-server's state. Treat as
    // window-gone rather than a generic permission failure — Screen
    // Recording has already been preflight-checked at boot.
    let onscreen = collect_window_snapshots(kCGWindowListOptionOnScreenOnly).ok_or(
        BotError::WindowChanged {
            reason: REASON_WID_GONE,
        },
    )?;
    let all =
        collect_window_snapshots(K_CG_WINDOW_LIST_OPTION_ALL).ok_or(BotError::WindowChanged {
            reason: REASON_WID_GONE,
        })?;

    validate_inner(
        expected.id,
        expected.pid,
        expected.frame,
        &onscreen,
        &all,
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

    // ---------- validate_inner / validate_present_inner (v0.1.5 4-/3-check) ----------

    /// PID used for the canonical "RoK process" in all validator tests.
    /// Matches the live RoK pid observed in p7-spike runs (21916) — the
    /// number is arbitrary but kept consistent so a future reader can
    /// cross-ref with spike logs.
    const ROK_PID: i32 = 21916;

    /// PID used for the WID-reuse adversarial test. Any pid ≠ `ROK_PID`
    /// works; this one is chosen far from `ROK_PID` to avoid the
    /// suspicion that an off-by-one mistake could mask the test.
    const OTHER_PID: i32 = 99999;

    /// Build a `WindowSnapshot` with the canonical RoK pid. v0.1.5
    /// validators anchor lookups on (WID, PID) rather than WID alone, so
    /// every test snapshot needs an explicit pid. Tests vary `id` and
    /// `frame` to drive each check; helper keeps the boilerplate down.
    fn snap(id: u32, x: f64, y: f64, w: f64, h: f64) -> WindowSnapshot {
        snap_pid(id, ROK_PID, x, y, w, h)
    }

    /// Build a `WindowSnapshot` with an explicit pid. Used by the
    /// WID-reuse tests where we deliberately put a "right WID, wrong
    /// PID" entry into the live snapshot list to prove the validators
    /// don't false-pass on numeric WID reuse.
    fn snap_pid(id: u32, pid: i32, x: f64, y: f64, w: f64, h: f64) -> WindowSnapshot {
        WindowSnapshot {
            id,
            pid,
            frame: rect(x, y, w, h),
            owner_name: None,
            title: None,
        }
    }

    const TOL: f64 = FRAME_TOLERANCE_POINTS;

    #[test]
    fn validate_inner_happy_path() {
        // RoK present at expected frame in both lists, click point
        // inside frame. All four checks pass.
        let onscreen = [snap(42, 100.0, 200.0, 1280.0, 720.0)];
        let all = onscreen.clone();
        let result = validate_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            &onscreen,
            &all,
            CGPoint::new(740.0, 560.0),
            TOL,
        );
        assert!(result.is_ok(), "happy path must succeed: {result:?}");
    }

    #[test]
    fn validate_inner_wid_gone() {
        // Expected RoK (WID 42, ROK_PID) not in `all` (only WID 99
        // present). RoK process closed/crashed between discovery and
        // re-check. PID anchor makes this check exact.
        let all = [snap(99, 0.0, 0.0, 1920.0, 1080.0)];
        let onscreen = all.clone();
        match validate_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            &onscreen,
            &all,
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
    fn validate_inner_wid_reused_wrong_pid() {
        // Adversarial: numeric WID 42 exists in `all` but under
        // OTHER_PID (not the RoK pid we discovered). Window-server
        // reuses WIDs after a window is destroyed. PID anchoring
        // catches this — without it, the bot would treat the reused
        // WID as "still RoK" and AX-press into an unrelated app.
        let all = [snap_pid(42, OTHER_PID, 100.0, 200.0, 1280.0, 720.0)];
        let onscreen = all.clone();
        match validate_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            &onscreen,
            &all,
            CGPoint::new(740.0, 560.0),
            TOL,
        ) {
            Err(BotError::WindowChanged { reason }) => {
                assert_eq!(reason, REASON_WID_GONE, "WID reuse must surface as gone");
            }
            other => panic!("expected WindowChanged{{wid_gone}}, got {other:?}"),
        }
    }

    #[test]
    fn validate_inner_on_hidden_space() {
        // (WID, PID) present in `all` but missing from `onscreen` —
        // RoK is running but on a hidden Space, minimized to Dock, or
        // in a transient WindowServer state. AX press would return
        // kAXErrorFailure (-25200) against this window, so we must
        // fail fast with a distinct reason that tells the operator
        // to switch Spaces rather than restart RoK.
        let all = [snap(42, 100.0, 200.0, 1280.0, 720.0)];
        let onscreen: [WindowSnapshot; 0] = [];
        match validate_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            &onscreen,
            &all,
            CGPoint::new(740.0, 560.0),
            TOL,
        ) {
            Err(BotError::WindowChanged { reason }) => {
                assert_eq!(reason, REASON_NOT_VISIBLE);
            }
            other => panic!("expected WindowChanged{{not_visible}}, got {other:?}"),
        }
    }

    #[test]
    fn validate_inner_frame_moved_origin() {
        // RoK still (WID 42, ROK_PID) but moved 100 points right
        // between discovery and re-check.
        let onscreen = [snap(42, 200.0, 200.0, 1280.0, 720.0)];
        let all = onscreen.clone();
        match validate_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            &onscreen,
            &all,
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
        // RoK still (WID 42, ROK_PID), origin unchanged, but window
        // resized by 50pt in width. Caught as frame_moved.
        let onscreen = [snap(42, 100.0, 200.0, 1330.0, 720.0)];
        let all = onscreen.clone();
        match validate_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            &onscreen,
            &all,
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
        let onscreen = [snap(42, 100.5, 200.0, 1280.0, 720.5)];
        let all = onscreen.clone();
        let result = validate_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            &onscreen,
            &all,
            CGPoint::new(740.0, 560.0),
            TOL,
        );
        assert!(
            result.is_ok(),
            "0.5pt jitter must pass under 1.0pt tolerance: {result:?}"
        );
    }

    #[test]
    fn validate_inner_point_outside_frame() {
        // (WID, PID) match, frame match, but the requested click point
        // falls outside the discovered frame. Pre-v0.1.5 this was
        // caught accidentally by the topmost walk (no window contains
        // the point → not-topmost); the v0.1.5 pipeline drops topmost
        // and makes the bounds check explicit. Catches operator-side
        // coord math bugs before they reach the AX layer.
        let onscreen = [snap(42, 100.0, 200.0, 1280.0, 720.0)];
        let all = onscreen.clone();
        match validate_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            &onscreen,
            &all,
            CGPoint::new(5000.0, 5000.0), // far outside RoK's frame
            TOL,
        ) {
            Err(BotError::WindowChanged { reason }) => {
                assert_eq!(reason, REASON_POINT_OUTSIDE_FRAME);
            }
            other => panic!("expected WindowChanged{{point_outside_frame}}, got {other:?}"),
        }
    }

    #[test]
    fn validate_inner_overlay_on_top_now_passes() {
        // Pre-v0.1.5 this fired REASON_NOT_TOPMOST. v0.1.5 drops the
        // topmost walk: kAXPressAction delivers to Catalyst Bridge
        // apps even when another window is z-order topmost at the
        // click point (verified empirically in p5-spike — see
        // `learnings/ax-press-works-catalyst`). The overlay scenario
        // is now a happy path.
        let onscreen = [
            snap(99, 700.0, 500.0, 200.0, 200.0),  // overlay covers click
            snap(42, 100.0, 200.0, 1280.0, 720.0), // RoK below
        ];
        let all = onscreen.clone();
        let result = validate_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            &onscreen,
            &all,
            CGPoint::new(740.0, 560.0),
            TOL,
        );
        assert!(
            result.is_ok(),
            "v0.1.5 AX-on-Catalyst makes z-order overlap a non-issue: {result:?}"
        );
    }

    #[test]
    fn validate_inner_negative_origin_virtual_display() {
        // Mirrors P3 spike geometry: RoK on BetterDisplay virtual screen
        // at (-1051, 103) size 1051x820. Click at center (-525.5, 513).
        // Negative-origin coords are valid CG global coords; the
        // contains-point math must handle them without sign confusion.
        let onscreen = [snap(73313, -1051.0, 103.0, 1051.0, 820.0)];
        let all = onscreen.clone();
        let result = validate_inner(
            73313,
            ROK_PID,
            rect(-1051.0, 103.0, 1051.0, 820.0),
            &onscreen,
            &all,
            CGPoint::new(-525.5, 513.0),
            TOL,
        );
        assert!(
            result.is_ok(),
            "virtual-display negative-origin happy path must pass: {result:?}"
        );
    }

    #[test]
    fn validate_inner_check_order_wid_then_visible_then_frame_then_point() {
        // Pin first-failure-wins precedence by exercising the WID-gone
        // leg with conditions that would also have tripped checks
        // 2-4. WID is not in `all` AND not in `onscreen` AND frame
        // would have moved AND point would have been outside — the
        // most diagnostic reason (WID gone) wins.
        let all: [WindowSnapshot; 0] = [];
        let onscreen: [WindowSnapshot; 0] = [];
        match validate_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            &onscreen,
            &all,
            CGPoint::new(99999.0, 99999.0),
            TOL,
        ) {
            Err(BotError::WindowChanged { reason }) => {
                assert_eq!(reason, REASON_WID_GONE, "WID_GONE must fire first");
            }
            other => panic!("expected WindowChanged{{wid_gone}}, got {other:?}"),
        }
    }

    #[test]
    fn validate_inner_not_visible_beats_frame_moved() {
        // Second-tier precedence: NOT_VISIBLE fires before FRAME_MOVED.
        // (WID, PID) present in `all` but the `all` entry has a
        // drifted frame; not present in `onscreen`. The hidden-Space
        // reason is the more actionable diagnostic ("switch Spaces"
        // vs "RoK moved") so it wins.
        let all = [snap(42, 999.0, 999.0, 1280.0, 720.0)]; // far from expected
        let onscreen: [WindowSnapshot; 0] = [];
        match validate_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            &onscreen,
            &all,
            CGPoint::new(740.0, 560.0),
            TOL,
        ) {
            Err(BotError::WindowChanged { reason }) => {
                assert_eq!(
                    reason, REASON_NOT_VISIBLE,
                    "NOT_VISIBLE must beat FRAME_MOVED"
                );
            }
            other => panic!("expected WindowChanged{{not_visible}}, got {other:?}"),
        }
    }

    // ---------- validate_present_inner (v0.1.5 post-capture, 3-check) ----------

    #[test]
    fn validate_present_inner_passes_when_wid_pid_present_and_frame_within_tolerance() {
        // Happy path: (WID, PID) in both lists, frame jitter under
        // tolerance. No click point because the post-capture path is
        // capture-only — the function signature makes that explicit.
        let onscreen = [snap(42, 100.0, 200.0, 1280.0, 720.0)];
        let all = onscreen.clone();
        let result = validate_present_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            &onscreen,
            &all,
            TOL,
        );
        assert!(result.is_ok(), "happy path must succeed: {result:?}");
    }

    #[test]
    fn validate_present_inner_wid_gone() {
        // RoK closed/crashed between click and post-capture.
        let all = [snap(99, 0.0, 0.0, 1920.0, 1080.0)];
        let onscreen = all.clone();
        match validate_present_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            &onscreen,
            &all,
            TOL,
        ) {
            Err(BotError::WindowChanged { reason }) => {
                assert_eq!(reason, REASON_WID_GONE);
            }
            other => panic!("expected WindowChanged{{wid_gone}}, got {other:?}"),
        }
    }

    #[test]
    fn validate_present_inner_wid_reused_wrong_pid() {
        // WID 42 was reused by an unrelated process between click and
        // post-capture. Without the PID anchor, the post-capture would
        // `screencapture -l 42` against the wrong window — pixel-diff
        // would compare RoK's pre-capture against an unrelated window
        // and false-positive on every pixel.
        let all = [snap_pid(42, OTHER_PID, 100.0, 200.0, 1280.0, 720.0)];
        let onscreen = all.clone();
        match validate_present_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            &onscreen,
            &all,
            TOL,
        ) {
            Err(BotError::WindowChanged { reason }) => {
                assert_eq!(reason, REASON_WID_GONE);
            }
            other => panic!("expected WindowChanged{{wid_gone}}, got {other:?}"),
        }
    }

    #[test]
    fn validate_present_inner_on_hidden_space() {
        // RoK got hidden between click and post-capture (Space switch,
        // minimize, or fullscreen-from-another-app). Post-capture
        // `screencapture -l` against a hidden-Space window typically
        // returns blank or stale pixels; failing fast here keeps
        // pixel-diff from drawing a wrong conclusion.
        let all = [snap(42, 100.0, 200.0, 1280.0, 720.0)];
        let onscreen: [WindowSnapshot; 0] = [];
        match validate_present_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            &onscreen,
            &all,
            TOL,
        ) {
            Err(BotError::WindowChanged { reason }) => {
                assert_eq!(reason, REASON_NOT_VISIBLE);
            }
            other => panic!("expected WindowChanged{{not_visible}}, got {other:?}"),
        }
    }

    #[test]
    fn validate_present_inner_frame_moved_origin() {
        // RoK still WID 42 but moved 100pt between click and post-capture.
        let onscreen = [snap(42, 200.0, 200.0, 1280.0, 720.0)];
        let all = onscreen.clone();
        match validate_present_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            &onscreen,
            &all,
            TOL,
        ) {
            Err(BotError::WindowChanged { reason }) => {
                assert_eq!(reason, REASON_FRAME_MOVED);
            }
            other => panic!("expected WindowChanged{{frame_moved}}, got {other:?}"),
        }
    }

    #[test]
    fn validate_present_inner_frame_moved_size() {
        // Window resized by 50pt in width mid-flow.
        let onscreen = [snap(42, 100.0, 200.0, 1330.0, 720.0)];
        let all = onscreen.clone();
        match validate_present_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            &onscreen,
            &all,
            TOL,
        ) {
            Err(BotError::WindowChanged { reason }) => {
                assert_eq!(reason, REASON_FRAME_MOVED);
            }
            other => panic!("expected WindowChanged{{frame_moved}}, got {other:?}"),
        }
    }

    #[test]
    fn validate_present_inner_passes_with_overlay_on_top() {
        // Click spawned a modal/overlay that's now z-order topmost.
        // Both the overlay and RoK are in `onscreen`; RoK's (WID, PID)
        // is found. v0.1.5 doesn't check topmost anywhere, so this is
        // unambiguously a happy path — preserved from v0.1.4 where it
        // was the post-capture pipeline's distinguishing test.
        let onscreen = [
            snap(99, 700.0, 500.0, 200.0, 200.0), // overlay (e.g. modal spawned by click)
            snap(42, 100.0, 200.0, 1280.0, 720.0), // RoK below
        ];
        let all = onscreen.clone();
        let result = validate_present_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            &onscreen,
            &all,
            TOL,
        );
        assert!(
            result.is_ok(),
            "overlay above RoK must NOT fire WindowChanged: {result:?}"
        );
    }

    #[test]
    fn validate_present_inner_frame_within_tolerance_passes() {
        // Sub-pixel jitter under the 1.0 tolerance must NOT fire frame_moved.
        let onscreen = [snap(42, 100.5, 200.0, 1280.0, 720.5)];
        let all = onscreen.clone();
        let result = validate_present_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            &onscreen,
            &all,
            TOL,
        );
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
        assert_eq!(REASON_NOT_VISIBLE, "not_visible");
        assert_eq!(REASON_POINT_OUTSIDE_FRAME, "point_outside_frame");
    }

    #[test]
    fn window_change_maps_to_exit_19() {
        for reason in [
            REASON_WID_GONE,
            REASON_FRAME_MOVED,
            REASON_NOT_VISIBLE,
            REASON_POINT_OUTSIDE_FRAME,
        ] {
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
