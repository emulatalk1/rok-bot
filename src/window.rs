//! Window enumeration via ScreenCaptureKit (v0.1.8) backed by Core
//! Graphics for the visibility-on-current-Space check.
//!
//! v0.1.8 migrates the discovery surface from `CGWindowListCopyWindowInfo`
//! (the v0.1.x path) to `SCShareableContent.getShareableContentWith
//! CompletionHandler`. The CGWindow enumeration stays — it's the only
//! cheap way to distinguish "RoK on a hidden Space" from "RoK gone"
//! (SCK's `windows` list returns both without distinction). The two
//! sources together preserve v0.1.6's exit-19 (`REASON_NOT_VISIBLE`)
//! vs exit-10 (`WindowNotFound`) operator-actionable split.
//!
//! Per the v0.1.8 design (D2/T3), `SCShareableContent` is cached at
//! module level and invalidated only on `CaptureFailed { stage =
//! window_not_found }`. Without the cache, every validator + capture
//! call would re-pay the ~95 ms enumeration cost; with it, only the
//! first call per session (or first call after invalidation) pays.
//!
//! ## Ownership pass on `RokWindow` (codex #6)
//!
//! `RokWindow` is owned by `main.rs::run` and borrowed by validators
//! (`&RokWindow`). It's `Clone` (one `Retained` retain ≈ ~10 ns), so
//! tests and helpers can take it by value cheaply. The
//! `Retained<SCWindow>` inside is reference-counted by SCK's autorelease
//! machinery; cloning bumps the retain count, dropping decrements.
//! The clone is NOT a deep copy of the underlying Cocoa window — both
//! the original and the clone reference the same `SCWindow` object.

#![allow(
    unsafe_code,
    reason = "Core Graphics + ScreenCaptureKit FFI are required for the \
              CGWindow visibility check + the SCK enumeration; the unsafe \
              surface is contained in this module."
)]
// Apple framework names dominate the docstrings; allow CamelCase
// terms without backticks for readability. The project-wide pedantic
// lint is `warn`; this module opts out.
#![allow(clippy::doc_markdown)]
// validate_inner + validate_present_inner need 7 + 8 args (the SCK
// frame parameter pushed validate_inner over the 7 ceiling); a
// struct-bundling refactor would hurt the per-test readability of
// the existing v0.1.5 test suite and buy nothing.
#![allow(clippy::too_many_arguments)]
// ContentCache wraps Mutex<Option<Retained<SCShareableContent>>>;
// the Retained inner is not auto-Send because objc2 doesn't
// declare Send/Sync on SCShareableContent's binding. We add the
// unsafe impl with a documented justification (Apple's SCK is
// thread-safe per the framework docs); this clippy lint then
// fires on the impl itself, but the safety reasoning is captured
// in the SAFETY comments on the unsafe impls.
#![allow(clippy::non_send_fields_in_send_ty)]

use std::ffi::c_void;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use block2::RcBlock;
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
use dispatch2::{DispatchSemaphore, DispatchTime};
use objc2::rc::Retained;
use objc2_foundation::NSError;
use objc2_screen_capture_kit::{SCShareableContent, SCWindow};

use crate::capture::STAGE_WINDOW_NOT_FOUND;
use crate::cg_bootstrap::register_with_window_server;
use crate::error::{BotError, Result};

/// RoK's CGWindow owner-name string. v0.1.x's CGWindowList-driven
/// discovery filtered by this directly; v0.1.8 routes through
/// SCK's `SCWindow.owningApplication.bundleIdentifier` instead, so
/// the constant is kept only for documentation + future diagnostic
/// use (operator may grep CG snapshots manually).
#[allow(
    dead_code,
    reason = "v0.1.x discovery path retired in v0.1.8; constant retained \
              for operator-side reference + future fallback"
)]
pub const ROK_OWNER: &str = "RiseOfKingdoms";

/// RoK's CGWindow title string AND its SCWindow.title() value.
/// Used by SCK enumeration to filter the main window from RoK's
/// auxiliary windows (splash, popups) which share owningApplication
/// but have a different title.
pub const ROK_TITLE: &str = "RiseOfKingdoms";

/// Reason tags surfaced via `BotError::WindowChanged { reason }`.
/// Preserved verbatim from v0.1.5/v0.1.6 — shell users + log parsers
/// pattern-match against these strings.
pub const REASON_WID_GONE: &str = "window_id_gone";
pub const REASON_FRAME_MOVED: &str = "frame_moved";
pub const REASON_NOT_VISIBLE: &str = "not_visible";
pub const REASON_POINT_OUTSIDE_FRAME: &str = "point_outside_frame";

/// `kCGWindowListOptionAll` = 0. The `core-graphics` 0.x crate exposes only
/// `kCGWindowListOptionOnScreenOnly`; the underlying CG enum uses 0 as the
/// "no on-screen filter" value.
const K_CG_WINDOW_LIST_OPTION_ALL: u32 = 0;

/// Tolerance for per-coordinate frame drift in [`validate_at_click_site`].
/// 1.0 point accepts sub-pixel jitter while still catching real moves.
pub const FRAME_TOLERANCE_POINTS: f64 = 1.0;

/// Bundle-ID prefix that legitimate RoK installs share. Verified on
/// `/Applications/RiseOfKingdoms.app` (Vietnam region: `com.rok.ios.vn`).
/// Spoof gate: any process can set `kCGWindowOwnerName == "RiseOfKingdoms"`,
/// but only the real game has a bundle ID under `com.rok.ios.`.
pub const ROK_BUNDLE_PREFIX: &str = "com.rok.ios.";

/// Maximum wait for `SCShareableContent.getShareableContentWith
/// CompletionHandler` to deliver. Larger than the per-tick capture
/// timeout because TCC prompts can stall this call until user
/// interaction (only on the first launch where SR isn't yet granted
/// to the rok-bot binary).
const SHAREABLE_CONTENT_TIMEOUT: Duration = Duration::from_secs(10);

/// Global cache of the most recent `SCShareableContent` snapshot
/// (D2/T3 from `/plan-eng-review`). The first call to
/// [`get_or_fetch_shareable_content`] populates the cache; subsequent
/// calls return the cached `Retained<SCShareableContent>` (~10 ns
/// vs ~95 ms cold). [`invalidate_shareable_content_cache`] clears
/// the slot on `CaptureFailed { stage = "window_not_found" }`,
/// triggering a re-fetch on the next call.
///
/// `Mutex<Option<Retained<SCShareableContent>>>` is the shape: the
/// outer `Mutex` provides interior mutability for cache update; the
/// `Option` lets us distinguish "not yet fetched" from "fetched but
/// SCK gave us nothing" (the latter never happens in practice — SCK
/// either returns content or errors).
///
/// `SCShareableContent` is an `NSObject` subclass with no main-thread
/// requirements (the `objc2-screen-capture-kit` 0.3.2 binding doesn't
/// declare `MainThreadMarker`), but `objc2`'s `Retained<T>` doesn't
/// auto-derive `Send + Sync` because the underlying `*const UnsafeCell`
/// makes the auto-derive bail. The wrapper newtype below carries
/// explicit `unsafe impl Send + Sync` so the cache can live in a
/// `static` (Apple's SCK is documented thread-safe; the rok-bot
/// process is single-threaded today, but the unsafe impls are
/// future-proof against the v0.2 continuous loop).
struct ContentCache(Mutex<Option<Retained<SCShareableContent>>>);

// SAFETY: `SCShareableContent` is an NSObject without main-thread-only
// constraints. Apple documents SCK as safe to call from any queue;
// the cached pointer can be cloned and dereferenced from any thread.
// The outer `Mutex` enforces interior-mutability serialization for
// the Option slot itself.
unsafe impl Send for ContentCache {}
// SAFETY: see Send impl above. `&ContentCache` exposes only `Mutex`
// methods, which serialize access to the inner `Option`.
unsafe impl Sync for ContentCache {}

static SCK_CONTENT_CACHE: OnceLock<ContentCache> = OnceLock::new();

/// Per-fetch slot for `fetch_shareable_content`'s completion-handler
/// result. Same wrapper pattern as `ContentCache` (and `capture::
/// ImageSlot`): the contained `Retained<SCShareableContent>` does
/// not auto-derive Send + Sync via objc2, but Apple documents
/// SCShareableContent as immutable + thread-safe and `objc_retain`/
/// `objc_release` are atomic, so the cross-thread move into the
/// SCK dispatch queue is sound.
struct ContentSlot(Mutex<Option<Retained<SCShareableContent>>>);

// SAFETY: see ContentSlot doc comment + matching SAFETY on
// `ContentCache` above.
unsafe impl Send for ContentSlot {}
// SAFETY: Mutex serializes interior mutability.
unsafe impl Sync for ContentSlot {}

/// Owned handle to the RoK main window discovered via SCK. Carries
/// the live `Retained<SCWindow>` (the capture-and-validation surface),
/// the matching `CGWindowID` (kept for diagnostic logging and the
/// CGWindow-side visibility check), the owning PID (for log lines and
/// to anchor the WID-reuse defense), the frame at discovery time (the
/// validators' baseline), and the bundle ID extracted from
/// `SCWindow.owningApplication` (recorded so the spoof check that
/// gated discovery is documented at the use site too).
///
/// Cloneable per the codex #6 ownership pass: validators take
/// `&RokWindow`, `main.rs::run` owns the original, and Clone bumps
/// SCK's retain count without deep-copying the Cocoa window.
#[derive(Debug, Clone)]
pub struct RokWindow {
    pub scwindow: Retained<SCWindow>,
    pub id: u32,
    pub pid: i32,
    pub frame: CGRect,
    /// Bundle ID extracted from `SCWindow.owningApplication
    /// .bundleIdentifier` at discovery time. Used as documentation
    /// of the spoof-check that gated discovery; future diagnostic
    /// log lines and tests can read it.
    #[allow(
        dead_code,
        reason = "field is part of the discovery contract; surfaced to log lines + future tests"
    )]
    pub bundle_id: String,
}

impl RokWindow {
    /// Geometric center of the discovery-time frame. Preserved from
    /// the v0.1.x `Window` struct's API for callsite parity.
    pub fn center(&self) -> CGPoint {
        center_of_rect(self.frame)
    }
}

/// Pure: geometric center of a `CGRect`. Extracted so the v0.1.x
/// `window_center_*` test cases survive without needing to construct
/// a `RokWindow` (which holds a `Retained<SCWindow>` we can't
/// fabricate in a unit test).
fn center_of_rect(frame: CGRect) -> CGPoint {
    CGPoint {
        x: frame.origin.x + frame.size.width / 2.0,
        y: frame.origin.y + frame.size.height / 2.0,
    }
}

/// One CGWindow's (id, pid) plus its frame and optional owner+title
/// strings, parsed from a single `CGWindowListCopyWindowInfo`
/// dictionary entry.
///
/// v0.1.8 only consumes (id, pid) from snapshots — the validators
/// use the live `SCWindow.frame()` for drift checks and SCK
/// enumeration for owner/title filtering. The other fields are
/// kept on the struct so a future validator that needs CG-side
/// frame or owner data doesn't have to retro-parse the dict; mark
/// them dead-code-allowed so clippy doesn't warn while they sit
/// idle.
#[derive(Debug, Clone)]
struct WindowSnapshot {
    id: u32,
    pid: i32,
    #[allow(
        dead_code,
        reason = "v0.1.5/v0.1.7 validators read this; v0.1.8 uses live SCK frame instead"
    )]
    frame: CGRect,
    #[allow(
        dead_code,
        reason = "v0.1.5 discovery pre-filtered on this; v0.1.8 uses SCK-side owningApplication"
    )]
    owner_name: Option<String>,
    #[allow(dead_code, reason = "same rationale as owner_name")]
    title: Option<String>,
}

/// Live wrapper: synchronously fetch a fresh `SCShareableContent`
/// without consulting the cache. Pure for testability is impractical
/// (the FFI dominates), so this is the lowest-level seam.
///
/// Used by:
/// - [`permissions::check_sck_grant`] at boot (one-shot preflight,
///   bypasses the cache so we know the live system is healthy).
/// - [`get_or_fetch_shareable_content`] on cache miss / invalidation.
///
/// Returns `Err(BotError::CaptureFailed { stage:
/// STAGE_WINDOW_NOT_FOUND })` on nil/error/timeout. The
/// stage-tag-on-fetch-error is a slight semantic stretch (the window
/// hasn't been searched for yet at this point), but it routes through
/// the cache-invalidation path correctly: any caller seeing this
/// error will treat the cache as stale, which is the right behavior
/// regardless of which fetch attempt failed.
pub fn fetch_shareable_content() -> Result<Retained<SCShareableContent>> {
    register_with_window_server();

    let sem = DispatchSemaphore::new(0);
    // Arc<ContentSlot> instead of stack-local Mutex: SCK retains its
    // own copy of the RcBlock on its background queue. If the timeout
    // fires and this function returns, SCK may still invoke the
    // completion handler later (TCC prompt was answered after 10s,
    // system was paged out). A clone of the Arc lives inside the
    // closure, so the late-firing handler dereferences a still-valid
    // Mutex rather than a freed stack-local. The wasted late-write is
    // harmless; the UAF it would otherwise cause is not. See
    // identical reasoning on `capture::capture_image_sync`.
    let slot: Arc<ContentSlot> = Arc::new(ContentSlot(Mutex::new(None)));
    let block = RcBlock::new({
        let sem = sem.clone();
        let slot = Arc::clone(&slot);
        move |content: *mut SCShareableContent, err: *mut NSError| {
            if !content.is_null() {
                // SAFETY: SCK passes an autoreleased pointer; retain
                // to keep it past the block return.
                if let Some(retained) = unsafe { Retained::retain(content) } {
                    if let Ok(mut guard) = slot.0.lock() {
                        *guard = Some(retained);
                    }
                }
            } else if !err.is_null() {
                // SAFETY: SCK passes an autoreleased NSError pointer.
                if let Some(err) = unsafe { Retained::retain(err) } {
                    tracing::warn!(
                        target: "rok_bot",
                        nserror = %err,
                        "SCShareableContent fetch reported error (likely Screen Recording denied for rok-bot binary)"
                    );
                }
            }
            sem.signal();
        }
    });
    // SAFETY: SCK class method; SCK retains the block on its dispatch
    // queue, so it survives our early return on timeout.
    unsafe {
        SCShareableContent::getShareableContentWithCompletionHandler(&block);
    }

    let dt = DispatchTime::try_from(SHAREABLE_CONTENT_TIMEOUT).unwrap_or(DispatchTime::FOREVER);
    if sem.wait(dt) != 0 {
        tracing::warn!(
            target: "rok_bot",
            timeout_ms = SHAREABLE_CONTENT_TIMEOUT.as_millis() as u64,
            "SCShareableContent fetch timed out (T1 deadlock fail-safe)"
        );
        return Err(BotError::CaptureFailed {
            stage: STAGE_WINDOW_NOT_FOUND,
            exit_code: None,
        });
    }

    let result = slot.0.lock().ok().and_then(|mut g| g.take());
    result.ok_or(BotError::CaptureFailed {
        stage: STAGE_WINDOW_NOT_FOUND,
        exit_code: None,
    })
}

/// Cache-aware fetch. First call populates [`SCK_CONTENT_CACHE`];
/// subsequent calls clone the cached `Retained<SCShareableContent>`
/// (~10 ns per call vs ~95 ms cold). Cache invalidation is explicit
/// via [`invalidate_shareable_content_cache`] — there's no time-based
/// TTL because v0.1.8's single-shot semantics keep cache lifetime
/// bounded by one `cargo run`.
///
/// The boolean return indicates whether the content came from the
/// cache (`true`) or a fresh fetch (`false`). Callers like
/// `find_rok_window` use this to decide whether a "not found" result
/// is worth retrying with a cache invalidation: a stale cache might
/// be missing a relaunched RoK's new SCWindow, but a fresh fetch
/// already saw the live state and re-fetching would just waste
/// another ~95 ms returning the same content.
fn get_or_fetch_shareable_content() -> Result<(Retained<SCShareableContent>, bool)> {
    let cell = SCK_CONTENT_CACHE.get_or_init(|| ContentCache(Mutex::new(None)));
    {
        let guard = cell.0.lock().map_err(|_| BotError::CaptureFailed {
            stage: STAGE_WINDOW_NOT_FOUND,
            exit_code: None,
        })?;
        if let Some(content) = guard.as_ref() {
            return Ok((content.clone(), true));
        }
    }
    let content = fetch_shareable_content()?;
    if let Ok(mut guard) = cell.0.lock() {
        *guard = Some(content.clone());
    }
    Ok((content, false))
}

/// Drop the cached `SCShareableContent` so the next fetch re-queries
/// SCK. Called by callers that observe a stale-cache symptom (e.g.,
/// `find_rok_window` returning `WindowNotFound` after a prior success
/// — RoK was relaunched and the cached list no longer contains the
/// new SCWindow). The v0.1.8 design (D2) ties this to the
/// `STAGE_WINDOW_NOT_FOUND` stage tag.
pub fn invalidate_shareable_content_cache() {
    if let Some(cell) = SCK_CONTENT_CACHE.get() {
        if let Ok(mut guard) = cell.0.lock() {
            *guard = None;
        }
    }
}

/// Walk `CGWindowListCopyWindowInfo` with the given option flag and
/// return a vec of snapshots for every dict that has id+pid+frame.
/// CGWindow enumeration stays in v0.1.8 — it's the only API that
/// exposes the OnScreenOnly vs All distinction we need to surface
/// `REASON_NOT_VISIBLE` vs `REASON_WID_GONE`.
fn collect_window_snapshots(option: u32) -> Option<Vec<WindowSnapshot>> {
    let info_list = copy_window_info(option, kCGNullWindowID)?;
    let mut snapshots: Vec<WindowSnapshot> = Vec::with_capacity(64);
    for entry in info_list.iter() {
        let raw_ptr: *const c_void = *entry;
        if raw_ptr.is_null() {
            continue;
        }
        // SAFETY: CGWindowList vends each array slot as an unretained
        // CFDictionaryRef ("Get rule"). `wrap_under_get_rule` is the
        // canonical lift.
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

/// Find the RoK main window via SCK enumeration + bundle-ID prefix
/// filter.
///
/// Three outcomes (matches v0.1.5 contract):
/// 1. `Ok(rok_window)` — RoK is enumerable. Note: SCK enumeration
///    includes hidden-Space windows, so this Ok does NOT guarantee RoK
///    is on the currently-displayed Space — that's the validator's
///    job to check at click time. v0.1.8 deliberately lets boot-time
///    discovery succeed for hidden-Space windows so the operator
///    sees a clean exit-19 NOT_VISIBLE later instead of an exit-10
///    WindowNotFound that mis-implies "RoK isn't running."
/// 2. `Err(WindowNotFound)` — SCK didn't enumerate any matching
///    window. RoK isn't running.
/// 3. `Err(CaptureFailed { stage = window_not_found })` — SCK fetch
///    itself failed (timeout, TCC denial mid-flow). Cache is
///    invalidated so the next call re-fetches.
pub fn find_rok_window() -> Result<RokWindow> {
    let (content, was_cached) = match get_or_fetch_shareable_content() {
        Ok(pair) => pair,
        Err(err) => {
            invalidate_shareable_content_cache();
            return Err(err);
        }
    };
    if let Some(window) = find_rok_window_in_content(&content) {
        return Ok(window);
    }
    // First search came from a fresh fetch — the live system saw no
    // matching window. Re-fetching wouldn't help; SCK already
    // enumerated current state. ~95 ms saved on the cold-miss path.
    if !was_cached {
        return Err(BotError::WindowNotFound);
    }
    // First search came from a cached SCShareableContent that may
    // pre-date a RoK relaunch (new SCWindow, new WID). Invalidate and
    // re-fetch to get the current live state. Propagate the original
    // SCK fetch error rather than swallowing it as WindowNotFound —
    // a TCC denial mid-run or an SCK timeout deserves its own
    // CaptureFailed { stage: window_not_found } exit so the operator
    // sees the real diagnostic instead of a misleading "RoK isn't
    // running" message.
    invalidate_shareable_content_cache();
    let (content, _) = get_or_fetch_shareable_content()?;
    find_rok_window_in_content(&content).ok_or(BotError::WindowNotFound)
}

/// Pure-ish: scan `content.windows` for the RoK main window.
/// Pure-ish because each iteration calls into ObjC for property
/// accessors; the search predicate itself is straightforward.
fn find_rok_window_in_content(content: &SCShareableContent) -> Option<RokWindow> {
    // SAFETY: SCShareableContent.windows returns a retained NSArray.
    let windows = unsafe { content.windows() };
    for i in 0..windows.count() {
        let scwindow = windows.objectAtIndex(i);
        // SAFETY: SCK property accessors on a live SCWindow.
        let title = unsafe { scwindow.title() };
        let title_str = title.map(|t| t.to_string()).unwrap_or_default();
        if title_str != ROK_TITLE {
            continue;
        }
        let app = unsafe { scwindow.owningApplication() };
        let Some(app) = app else { continue };
        // SAFETY: SCRunningApplication property accessor.
        let bundle_id = unsafe { app.bundleIdentifier() }.to_string();
        if !matches_rok_bundle_id(&bundle_id) {
            tracing::warn!(
                target: "rok_bot",
                expected_prefix = ROK_BUNDLE_PREFIX,
                actual = %bundle_id,
                "skipping window with title=\"{ROK_TITLE}\" — bundle ID mismatch (possible spoof)"
            );
            continue;
        }
        // SAFETY: SCRunningApplication property accessor (with libc feature).
        let pid = unsafe { app.processID() };
        // SAFETY: SCWindow property accessors (objc2-core-graphics + -foundation features).
        let id = unsafe { scwindow.windowID() };
        let frame = sck_frame_to_cg(unsafe { scwindow.frame() });
        return Some(RokWindow {
            scwindow,
            id,
            pid,
            frame,
            bundle_id,
        });
    }
    None
}

/// Pure: does this bundle ID belong to a legitimate RoK install?
fn matches_rok_bundle_id(bundle_id: &str) -> bool {
    bundle_id.starts_with(ROK_BUNDLE_PREFIX)
}

/// Pure: enforce the v0.1.8 click-site TOCTOU invariants.
///
/// Three logical checks per design D1, but four operationally:
///
/// 1. **`REASON_WID_GONE`** — (WID, PID) pair not in `all`. PID
///    anchor catches WID reuse.
/// 2. **`REASON_NOT_VISIBLE`** — (WID, PID) in `all` but missing
///    from `onscreen`. Hidden Space, minimized, or fullscreen-from-
///    another-app pushed RoK aside. Distinct exit so operator
///    knows to switch Spaces / unminimize, not "restart RoK."
/// 3. **`REASON_FRAME_MOVED`** — `actual_frame` (live SCK frame at
///    validation time) drifted beyond [`FRAME_TOLERANCE_POINTS`]
///    from `expected_frame` (discovery-time frame). v0.1.5 sourced
///    `actual_frame` from the CGWindow snapshot; v0.1.8 sources it
///    from `SCWindow.frame()` so the check is grounded in the same
///    coordinate space SCK uses for the capture. Same semantics,
///    different (slightly more authoritative) source.
/// 4. **`REASON_POINT_OUTSIDE_FRAME`** — click point not inside
///    `expected_frame`. Catches operator-side coord math bugs.
fn validate_inner(
    expected_wid: u32,
    expected_pid: i32,
    expected_frame: CGRect,
    actual_frame: CGRect,
    onscreen: &[WindowSnapshot],
    all: &[WindowSnapshot],
    click_point: CGPoint,
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
    if !onscreen
        .iter()
        .any(|w| w.id == expected_wid && w.pid == expected_pid)
    {
        return Err(BotError::WindowChanged {
            reason: REASON_NOT_VISIBLE,
        });
    }
    if !frames_within_tolerance(actual_frame, expected_frame, tolerance) {
        return Err(BotError::WindowChanged {
            reason: REASON_FRAME_MOVED,
        });
    }
    if !rect_contains_point(expected_frame, click_point) {
        return Err(BotError::WindowChanged {
            reason: REASON_POINT_OUTSIDE_FRAME,
        });
    }
    Ok(())
}

/// Pure: enforce the v0.1.8 post-capture TOCTOU subset — drops the
/// click-point bounds check (no click is being sent), keeps the
/// other three.
fn validate_present_inner(
    expected_wid: u32,
    expected_pid: i32,
    expected_frame: CGRect,
    actual_frame: CGRect,
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
    if !onscreen
        .iter()
        .any(|w| w.id == expected_wid && w.pid == expected_pid)
    {
        return Err(BotError::WindowChanged {
            reason: REASON_NOT_VISIBLE,
        });
    }
    if !frames_within_tolerance(actual_frame, expected_frame, tolerance) {
        return Err(BotError::WindowChanged {
            reason: REASON_FRAME_MOVED,
        });
    }
    Ok(())
}

/// Live wrapper: re-fetch SCK content + CGWindow snapshots, read the
/// current SCK frame, run [`validate_present_inner`].
pub fn validate_window_present(expected: &RokWindow) -> Result<()> {
    let onscreen = collect_window_snapshots(kCGWindowListOptionOnScreenOnly).ok_or(
        BotError::WindowChanged {
            reason: REASON_WID_GONE,
        },
    )?;
    let all =
        collect_window_snapshots(K_CG_WINDOW_LIST_OPTION_ALL).ok_or(BotError::WindowChanged {
            reason: REASON_WID_GONE,
        })?;
    // SAFETY: SCK property accessor on a live SCWindow we still hold.
    let actual_frame = sck_frame_to_cg(unsafe { expected.scwindow.frame() });
    validate_present_inner(
        expected.id,
        expected.pid,
        expected.frame,
        actual_frame,
        &onscreen,
        &all,
        FRAME_TOLERANCE_POINTS,
    )
}

/// Convert SCK's `objc2_core_foundation::CGRect` (returned by
/// `SCWindow.frame()`) into `core_graphics::display::CGRect` (the
/// type the v0.1.x validators and matcher already use). Both are
/// `#[repr(C)] { origin: CGPoint, size: CGSize }` of f64 fields with
/// identical layout, but they're nominally distinct types so we
/// convert field-by-field.
fn sck_frame_to_cg(f: objc2_core_foundation::CGRect) -> CGRect {
    CGRect::new(
        &CGPoint::new(f.origin.x, f.origin.y),
        &CGSize::new(f.size.width, f.size.height),
    )
}

/// Pure: are two `CGRect`s equal within per-coordinate tolerance?
fn frames_within_tolerance(a: CGRect, b: CGRect, tolerance: f64) -> bool {
    (a.origin.x - b.origin.x).abs() <= tolerance
        && (a.origin.y - b.origin.y).abs() <= tolerance
        && (a.size.width - b.size.width).abs() <= tolerance
        && (a.size.height - b.size.height).abs() <= tolerance
}

/// Pure: half-open rectangle containment, top-left origin.
fn rect_contains_point(rect: CGRect, p: CGPoint) -> bool {
    let x_min = rect.origin.x;
    let x_max = rect.origin.x + rect.size.width;
    let y_min = rect.origin.y;
    let y_max = rect.origin.y + rect.size.height;
    p.x >= x_min && p.x < x_max && p.y >= y_min && p.y < y_max
}

/// Live wrapper for the click-site validator. v0.1.8 reads the live
/// SCK frame at this site so a frame drift between discovery and
/// click is caught against the same coordinate space the capture
/// will use.
pub fn validate_at_click_site(expected: &RokWindow, click_point: CGPoint) -> Result<()> {
    let onscreen = collect_window_snapshots(kCGWindowListOptionOnScreenOnly).ok_or(
        BotError::WindowChanged {
            reason: REASON_WID_GONE,
        },
    )?;
    let all =
        collect_window_snapshots(K_CG_WINDOW_LIST_OPTION_ALL).ok_or(BotError::WindowChanged {
            reason: REASON_WID_GONE,
        })?;
    // SAFETY: SCK property accessor on a live SCWindow we still hold.
    let actual_frame = sck_frame_to_cg(unsafe { expected.scwindow.frame() });
    validate_inner(
        expected.id,
        expected.pid,
        expected.frame,
        actual_frame,
        &onscreen,
        &all,
        click_point,
        FRAME_TOLERANCE_POINTS,
    )
}

/// Parse one CGWindowList dictionary entry into a snapshot.
fn parse_snapshot(dict: &CFDictionary) -> Option<WindowSnapshot> {
    // SAFETY: reading static CFStringRef constants vended by Core Graphics.
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

fn dict_get<T: ConcreteCFType>(dict: &CFDictionary, key: *const c_void) -> Option<T> {
    let value_ref = dict.find(key)?;
    let value_ptr: *const c_void = *value_ref;
    if value_ptr.is_null() {
        return None;
    }
    // SAFETY: dict.find returns a borrowed slot pointer ("Get rule").
    let cf = unsafe { CFType::wrap_under_get_rule(value_ptr.cast()) };
    cf.downcast::<T>()
}

fn rect_from_dict(dict: &CFDictionary) -> Option<CGRect> {
    let mut rect = CGRect {
        origin: CGPoint::new(0.0, 0.0),
        size: CGSize::new(0.0, 0.0),
    };
    // SAFETY: live CFDictionary; pass writable CGRect by pointer per Apple's API.
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

    // ---------- Bundle ID validation ----------

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
        assert!(!matches_rok_bundle_id("com.example.fake-rok"));
        assert!(!matches_rok_bundle_id("RiseOfKingdoms"));
        assert!(!matches_rok_bundle_id("org.rok.ios.vn"));
        assert!(!matches_rok_bundle_id(""));
        assert!(!matches_rok_bundle_id("com.finder.app"));
    }

    #[test]
    fn matches_rok_bundle_id_accepts_bare_prefix() {
        // Forward-compat: future "com.rok.ios.global" or unsuffixed
        // installs are trusted.
        assert!(matches_rok_bundle_id("com.rok.ios."));
    }

    // ---------- validate_inner / validate_present_inner ----------

    const ROK_PID: i32 = 21916;
    const OTHER_PID: i32 = 99999;

    fn snap(id: u32, x: f64, y: f64, w: f64, h: f64) -> WindowSnapshot {
        snap_pid(id, ROK_PID, x, y, w, h)
    }

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
        let onscreen = [snap(42, 100.0, 200.0, 1280.0, 720.0)];
        let all = onscreen.clone();
        let result = validate_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
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
        let all = [snap(99, 0.0, 0.0, 1920.0, 1080.0)];
        let onscreen = all.clone();
        match validate_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
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
        // Adversarial: WID 42 exists in `all` but under OTHER_PID.
        // PID anchor must catch this (without it, AX would press into
        // an unrelated app whose window inherited the WID after RoK
        // closed).
        let all = [snap_pid(42, OTHER_PID, 100.0, 200.0, 1280.0, 720.0)];
        let onscreen = all.clone();
        match validate_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
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
        let all = [snap(42, 100.0, 200.0, 1280.0, 720.0)];
        let onscreen: [WindowSnapshot; 0] = [];
        match validate_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
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
        // Live SCK frame drifted 100pt right since discovery.
        let onscreen = [snap(42, 100.0, 200.0, 1280.0, 720.0)];
        let all = onscreen.clone();
        match validate_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            rect(200.0, 200.0, 1280.0, 720.0),
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
        let onscreen = [snap(42, 100.0, 200.0, 1280.0, 720.0)];
        let all = onscreen.clone();
        match validate_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            rect(100.0, 200.0, 1330.0, 720.0),
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
        // Sub-pixel jitter (0.5pt) below 1.0 tolerance must pass.
        let onscreen = [snap(42, 100.0, 200.0, 1280.0, 720.0)];
        let all = onscreen.clone();
        let result = validate_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            rect(100.5, 200.0, 1280.0, 720.5),
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
        let onscreen = [snap(42, 100.0, 200.0, 1280.0, 720.0)];
        let all = onscreen.clone();
        match validate_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            rect(100.0, 200.0, 1280.0, 720.0),
            &onscreen,
            &all,
            CGPoint::new(5000.0, 5000.0),
            TOL,
        ) {
            Err(BotError::WindowChanged { reason }) => {
                assert_eq!(reason, REASON_POINT_OUTSIDE_FRAME);
            }
            other => panic!("expected WindowChanged{{point_outside_frame}}, got {other:?}"),
        }
    }

    #[test]
    fn validate_inner_overlay_above_rok_passes() {
        // v0.1.5+ behavior: overlay above RoK is not a validator
        // failure — z-order is no longer checked. SCK enumeration
        // surfaces both windows; the validator just confirms RoK
        // itself is reachable.
        let onscreen = [
            snap(99, 700.0, 500.0, 200.0, 200.0),
            snap(42, 100.0, 200.0, 1280.0, 720.0),
        ];
        let all = onscreen.clone();
        let result = validate_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            rect(100.0, 200.0, 1280.0, 720.0),
            &onscreen,
            &all,
            CGPoint::new(740.0, 560.0),
            TOL,
        );
        assert!(
            result.is_ok(),
            "overlay z-order must not fire WindowChanged: {result:?}"
        );
    }

    #[test]
    fn validate_inner_negative_origin_virtual_display() {
        let onscreen = [snap(73313, -1051.0, 103.0, 1051.0, 820.0)];
        let all = onscreen.clone();
        let result = validate_inner(
            73313,
            ROK_PID,
            rect(-1051.0, 103.0, 1051.0, 820.0),
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
        let all: [WindowSnapshot; 0] = [];
        let onscreen: [WindowSnapshot; 0] = [];
        match validate_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            rect(99999.0, 99999.0, 0.1, 0.1),
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
        let all = [snap(42, 999.0, 999.0, 1280.0, 720.0)];
        let onscreen: [WindowSnapshot; 0] = [];
        match validate_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            rect(999.0, 999.0, 1280.0, 720.0),
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

    #[test]
    fn validate_present_inner_happy_path() {
        let onscreen = [snap(42, 100.0, 200.0, 1280.0, 720.0)];
        let all = onscreen.clone();
        let result = validate_present_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            rect(100.0, 200.0, 1280.0, 720.0),
            &onscreen,
            &all,
            TOL,
        );
        assert!(result.is_ok(), "happy path must succeed: {result:?}");
    }

    #[test]
    fn validate_present_inner_wid_gone() {
        let all = [snap(99, 0.0, 0.0, 1920.0, 1080.0)];
        let onscreen = all.clone();
        match validate_present_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
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
        let all = [snap_pid(42, OTHER_PID, 100.0, 200.0, 1280.0, 720.0)];
        let onscreen = all.clone();
        match validate_present_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
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
        let all = [snap(42, 100.0, 200.0, 1280.0, 720.0)];
        let onscreen: [WindowSnapshot; 0] = [];
        match validate_present_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
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
        let onscreen = [snap(42, 100.0, 200.0, 1280.0, 720.0)];
        let all = onscreen.clone();
        match validate_present_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            rect(200.0, 200.0, 1280.0, 720.0),
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
    fn validate_present_inner_passes_with_overlay() {
        let onscreen = [
            snap(99, 700.0, 500.0, 200.0, 200.0),
            snap(42, 100.0, 200.0, 1280.0, 720.0),
        ];
        let all = onscreen.clone();
        let result = validate_present_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            rect(100.0, 200.0, 1280.0, 720.0),
            &onscreen,
            &all,
            TOL,
        );
        assert!(
            result.is_ok(),
            "overlay z-order must not fire WindowChanged: {result:?}"
        );
    }

    #[test]
    fn validate_present_inner_frame_within_tolerance_passes() {
        let onscreen = [snap(42, 100.0, 200.0, 1280.0, 720.0)];
        let all = onscreen.clone();
        let result = validate_present_inner(
            42,
            ROK_PID,
            rect(100.0, 200.0, 1280.0, 720.0),
            rect(100.5, 200.0, 1280.0, 720.5),
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
        assert!(rect_contains_point(r, CGPoint::new(125.0, 230.0)));
        assert!(rect_contains_point(r, CGPoint::new(100.0, 200.0)));
        assert!(!rect_contains_point(r, CGPoint::new(150.0, 260.0)));
        assert!(rect_contains_point(r, CGPoint::new(149.999, 259.999)));
        assert!(!rect_contains_point(r, CGPoint::new(50.0, 230.0)));
        assert!(!rect_contains_point(r, CGPoint::new(125.0, 100.0)));
    }

    #[test]
    fn window_change_reason_constants_match_expected_strings() {
        assert_eq!(REASON_WID_GONE, "window_id_gone");
        assert_eq!(REASON_FRAME_MOVED, "frame_moved");
        assert_eq!(REASON_NOT_VISIBLE, "not_visible");
        assert_eq!(REASON_POINT_OUTSIDE_FRAME, "point_outside_frame");
    }

    // ---------- center_of_rect (RokWindow.center delegate) ----------

    #[test]
    fn center_of_rect_is_geometric_midpoint() {
        let r = rect(100.0, 200.0, 1280.0, 720.0);
        let c = center_of_rect(r);
        assert!((c.x - 740.0).abs() < f64::EPSILON);
        assert!((c.y - 560.0).abs() < f64::EPSILON);
    }

    #[test]
    fn center_of_rect_zero_size_returns_origin() {
        // Degenerate but real during window-resize transitions.
        let r = rect(50.0, 60.0, 0.0, 0.0);
        let c = center_of_rect(r);
        assert!((c.x - 50.0).abs() < f64::EPSILON);
        assert!((c.y - 60.0).abs() < f64::EPSILON);
    }

    #[test]
    fn center_of_rect_handles_negative_origin() {
        // Per P7 spike: RoK on virtual display had origin (-525, 502).
        let r = rect(-525.0, 502.0, 1280.0, 720.0);
        let c = center_of_rect(r);
        assert!((c.x - 115.0).abs() < f64::EPSILON);
        assert!((c.y - 862.0).abs() < f64::EPSILON);
    }

    // ---------- ContentCache lifecycle ----------

    #[test]
    fn invalidate_shareable_content_cache_is_safe_when_uninitialized() {
        // The cache OnceLock may not have been initialized yet (no
        // prior fetch). invalidate must not panic in that state.
        // Idempotent if called twice.
        invalidate_shareable_content_cache();
        invalidate_shareable_content_cache();
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
}
