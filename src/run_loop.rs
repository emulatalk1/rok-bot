//! v0.2 continuous loop engine.
//!
//! v0.1.x was a one-shot pipeline: boot → capture → match → click →
//! verify → exit. v0.2 turns the `capture → match → click → verify`
//! core into a loop that runs until Ctrl-C (or a tick cap). The target
//! stays the state-neutral city↔world toggle — this is a deliberate
//! plumbing milestone (design D1), proving the loop machinery without
//! doing a real in-game task.
//!
//! ```text
//! BOOT (once, in main::run)            PER-TICK (this module, repeat)
//! ─────────────────────────            ──────────────────────────────
//! register WindowServer                ┌─► probe liveness → capture pre PNG
//! check Screen Recording                │   match NEEDLES, best-of:
//! check SCK grant                       │     last_match? → last-pos ROI
//! check Accessibility (HARD)            │     else        → castle ROI
//! find_rok_window → RokWindow            │     ROI miss → full-frame fallback
//! detect_mode                            │     still miss → TargetNotFound
//! parse ROK_BOT_MAX_TICKS                │   validate_match_dims → screen_point
//! install ctrlc → AtomicBool             │   validate_at_click_site
//!                                       │   click_at (+ ClickGuard RAII)
//!   loop state:                         │   sleep VERIFY_DELAY_MS
//!   last_match: Option<Match>            │   validate_window_present
//!   consecutive_failures: u32            │   capture post → needle-swap verify
//!   completed_ticks: u32                 │   update last_match; reset/incr fails
//!   stop: &AtomicBool ◄─ ctrlc           └── stop flag OR tick==max → exit 0
//! ```
//!
//! Locked decisions this module implements:
//!
//! - **D4** — run-until-Ctrl-C plus a `ROK_BOT_MAX_TICKS` hard cap with
//!   a finite default ([`DEFAULT_MAX_TICKS`]). `ROK_BOT_MAX_TICKS=1`
//!   reproduces the v0.1.x one-shot exactly.
//! - **D5 / D12 / v0.2.1 L3** — error policy. [`classify_error`] sorts
//!   every `BotError` into an [`ErrorClass`]: `Fatal` (abort now),
//!   `Transient` (count toward [`LOOP_FAILURE_BUDGET`]), and the v0.2.1
//!   window-lifecycle classes `WindowGone` / `WindowHidden` (enter
//!   window recovery via `recover_window`).
//! - **D9 / T1** — the decision core ([`classify_error`],
//!   [`next_failure_count`], [`should_stop`], [`parse_max_ticks`], plus
//!   `matcher::select_roi`) is pure free functions, exhaustively unit-
//!   tested with no live RoK. [`tick`] and [`run_loop`] are thin live
//!   wiring covered by `#[ignore]`'d integration tests.
//! - **D11** — verification is `verify::confirm_needle_swap` (a
//!   different needle matching the toggle ROI post-click), not pixel-diff.
//! - **v0.2.1 L5** — window recovery. A `WindowGone` / `WindowHidden`
//!   tick error enters [`recover_window`], a bounded poll-with-backoff
//!   sub-loop that waits RoK out and resumes the loop instead of
//!   aborting. [`recovery_enabled`] gates it to a genuine continuous
//!   loop — a `ROK_BOT_MAX_TICKS=1` one-shot still aborts immediately.
//!   [`RECOVERY_BUDGET`] consecutive recoveries with no successful tick
//!   between them abort the loop, so a crash-looping RoK can't spin it
//!   forever.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use core_graphics::display::CGPoint;

use crate::capture::capture_window;
use crate::click::click_at;
use crate::error::{BotError, Result};
use crate::matcher::{
    Match, NEEDLES, NeedleMatch, count_placeholder_needles, find_best_needle, screen_point,
    select_roi, validate_match_dims,
};
use crate::verify;
use crate::window::{self, RokWindow, validate_at_click_site, validate_window_present};

/// Consecutive transient failures the loop tolerates before aborting
/// with `BotError::LoopAborted` (exit 21). One transient failure is
/// noise — RoK mid-animation, a dropped frame, a mid-transition verify
/// frame. Three in a row, with no successful tick resetting the count,
/// means RoK is frozen, hidden for good, or the needle asset rotted:
/// the loop stops rather than spin. Per design D5.
pub const LOOP_FAILURE_BUDGET: u32 = 3;

/// Consecutive window recoveries the loop tolerates before aborting
/// with `BotError::LoopAborted` (exit 21). v0.2.1 L5 / `/review` D1.
///
/// One recovery rides through a RoK relaunch or a screensaver — the
/// v0.2.1 win. But a RoK that crash-loops, or a BetterDisplay display
/// that flaps, makes every tick fail into recovery: recovery succeeds,
/// the loop resumes, the next tick fails into recovery again. Without a
/// bound that spins forever — recovery is not a tick (the
/// `completed_ticks` cap never advances) and a successful recovery
/// resets `consecutive_failures` (the failure budget never trips).
/// `RECOVERY_BUDGET` consecutive recoveries with no successful tick
/// resetting the streak → the loop stops rather than recover endlessly,
/// mirroring [`LOOP_FAILURE_BUDGET`].
pub const RECOVERY_BUDGET: u32 = 3;

/// Default per-run tick cap when `ROK_BOT_MAX_TICKS` is unset. The v0.2
/// loop's primary mode is run-until-Ctrl-C; this finite default is the
/// backstop so an unattended run can't loop forever. ~100 ticks at the
/// loop's ~1s cadence is a 1-2 minute bounded demo run.
pub const DEFAULT_MAX_TICKS: u32 = 100;

/// Environment variable that overrides the per-run tick cap (design
/// D4). `ROK_BOT_MAX_TICKS=1` reproduces the v0.1.x one-shot pipeline.
pub const MAX_TICKS_ENV: &str = "ROK_BOT_MAX_TICKS";

/// `BotError::LoopAborted` reason tags. Pinned as `&'static str` so
/// operator-facing log lines and `error::tests` see one exact value.
///
/// `REASON_FAILURE_BUDGET_EXHAUSTED` — [`LOOP_FAILURE_BUDGET`]
/// consecutive transient failures with no success resetting the count.
///
/// `REASON_SIGNAL_INSTALL_FAILED` — `ctrlc::set_handler` failed at boot
/// (surfaced from `main::run`); the loop refuses to start without a
/// clean-shutdown path.
pub const REASON_FAILURE_BUDGET_EXHAUSTED: &str = "failure_budget_exhausted";
pub const REASON_SIGNAL_INSTALL_FAILED: &str = "signal_handler_install_failed";

/// `BotError::LoopAborted` reason tags for v0.2.1 window recovery (L8).
/// Pinned `&'static str`; `error::tests` checks the exact values.
///
/// `REASON_WINDOW_RECOVERY_EXHAUSTED` — a `WindowGone` (relaunch)
/// recovery polled for the full [`RELAUNCH_RECOVERY_DEADLINE`] without
/// RoK coming back.
///
/// `REASON_VISIBILITY_RECOVERY_EXHAUSTED` — a `WindowHidden` (visibility)
/// recovery polled for the full [`VISIBILITY_RECOVERY_DEADLINE`] without
/// RoK reappearing.
///
/// `REASON_RECOVERY_BUDGET_EXHAUSTED` — [`RECOVERY_BUDGET`] consecutive
/// recoveries with no successful tick between them (RoK is crash-looping
/// or the display is flapping).
pub const REASON_WINDOW_RECOVERY_EXHAUSTED: &str = "window_recovery_exhausted";
pub const REASON_VISIBILITY_RECOVERY_EXHAUSTED: &str = "visibility_recovery_exhausted";
pub const REASON_RECOVERY_BUDGET_EXHAUSTED: &str = "recovery_budget_exhausted";

/// v0.2.1 L5 — relaunch-recovery poll interval and total deadline
/// (entered on [`ErrorClass::WindowGone`]). RoK cold-start is ~10-30 s;
/// the 60 s deadline leaves headroom for one slow restart before the
/// loop gives up with `LoopAborted{window_recovery_exhausted}`.
const RELAUNCH_RECOVERY_INTERVAL: Duration = Duration::from_secs(2);
const RELAUNCH_RECOVERY_DEADLINE: Duration = Duration::from_secs(60);

/// v0.2.1 L5 — visibility-recovery poll interval and total deadline
/// (entered on [`ErrorClass::WindowHidden`]). A legitimate screensaver
/// or display sleep can be long, so the deadline is generous (30 min);
/// the finite bound still catches a permanently-gone virtual display.
const VISIBILITY_RECOVERY_INTERVAL: Duration = Duration::from_secs(5);
const VISIBILITY_RECOVERY_DEADLINE: Duration = Duration::from_secs(30 * 60);

/// Pre-click capture path, rewritten every tick. Relative to cwd;
/// `.gitignore` covers the `rok-capture-*.png` wildcard. `capture.rs`'s
/// `O_NOFOLLOW + O_EXCL` open unlinks the prior tick's file and
/// recreates it, so reusing one path across ticks is race-free.
const TICK_CAPTURE_PRE_PATH: &str = "rok-capture-pre.png";

/// Post-click capture path, rewritten every tick. Sibling of
/// [`TICK_CAPTURE_PRE_PATH`].
const TICK_CAPTURE_POST_PATH: &str = "rok-capture-post.png";

/// How [`classify_error`] sorts a per-tick `BotError` for the loop's
/// error policy (design D5/D12, extended to four-way by v0.2.1 L3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// Abort the loop immediately — the error will not self-heal
    /// (a permission is missing, the click pipeline broke, the window
    /// frame drifted out from under the loop).
    Fatal,
    /// Count toward [`LOOP_FAILURE_BUDGET`]. One is noise; a streak
    /// aborts. A successful tick resets the streak to 0.
    Transient,
    /// RoK's window is gone — closed, crashed, or its `CGWindowID` was
    /// reused by another process. v0.2.1 L3 reclassified
    /// `WindowChanged{window_id_gone}` here (it was `Fatal` in v0.2):
    /// the loop enters relaunch recovery rather than aborting, so a RoK
    /// crash-and-restart mid-loop is ridden through.
    WindowGone,
    /// RoK's window is alive but not visible — on another Space,
    /// minimized, or behind a fullscreen app / screensaver. v0.2.1 L3
    /// reclassified `WindowChanged{not_visible}` here (it was
    /// `Transient` in v0.2): the loop enters visibility recovery and
    /// waits the window out rather than burning the failure budget.
    WindowHidden,
}

/// Pure: sort a `BotError` into an [`ErrorClass`] for the loop's
/// per-tick error policy (design D5/D12; v0.2.1 L3 extended the split
/// from two classes to four).
///
/// `WindowChanged` splits by reason tag (see [`classify_window_changed`]):
/// `window_id_gone` → `WindowGone`, `not_visible` → `WindowHidden`,
/// everything else → `Fatal`.
///
/// Total over every `BotError` variant. The boot-only / structural
/// variants (`WindowNotFound`, `ImageLoadFailed`, …) cannot occur
/// per-tick given the boot sequence runs `find_rok_window` before the
/// loop — but `classify_error` must be exhaustive, so they default to
/// `Fatal`: an unanticipated error aborts cleanly rather than spinning.
pub fn classify_error(err: &BotError) -> ErrorClass {
    match err {
        // WindowChanged splits by reason tag (window_id_gone →
        // WindowGone, not_visible → WindowHidden, else → Fatal; v0.2.1
        // L3) — handle it before the blanket arms below.
        BotError::WindowChanged { reason } => classify_window_changed(reason),
        // TRANSIENT (D5): one failure is noise, a streak aborts the loop.
        BotError::TargetNotFound
        | BotError::ClickNotVerified { .. }
        | BotError::CaptureFailed { .. } => ErrorClass::Transient,
        // FATAL: D5's fatal set (PermissionsMissing, ClickFailed) plus
        // the boot-time / structural variants. The structural ones
        // cannot occur per-tick — the boot sequence resolved the window
        // + display before the loop — but classify_error must be total,
        // and an unanticipated error aborting cleanly beats spinning.
        BotError::PermissionsMissing { .. }
        | BotError::ClickFailed { .. }
        | BotError::WindowNotFound
        | BotError::WindowScreenUnresolved
        | BotError::ImageLoadFailed { .. }
        | BotError::TargetTooLarge { .. }
        | BotError::LoopAborted { .. } => ErrorClass::Fatal,
    }
}

/// Pure: classify a `WindowChanged` error by its reason tag (v0.2.1 L3).
fn classify_window_changed(reason: &str) -> ErrorClass {
    match reason {
        // RoK closed, crashed, or its CGWindowID was reused. v0.2.1 L3
        // reclassified this Fatal→WindowGone: the loop re-discovers the
        // window and rides through a relaunch instead of aborting.
        window::REASON_WID_GONE => ErrorClass::WindowGone,
        // A hide: Space switch, momentary idle, or a full screensaver.
        // D12 reclassified not_visible Fatal→Transient; v0.2.1 L3
        // reclassifies it again Transient→WindowHidden so the loop
        // waits the window out rather than burning the failure budget.
        window::REASON_NOT_VISIBLE => ErrorClass::WindowHidden,
        // frame_moved / point_outside_frame stay Fatal — on a healthy
        // loop the window neither drifts nor yields an out-of-frame
        // click point, so either signals a state the loop can't ride
        // through. Future reason tags default here too.
        _ => ErrorClass::Fatal,
    }
}

/// Pure: is window recovery active for this run (v0.2.1 L10 / D7)?
///
/// Recovery is a continuous-loop feature. `ROK_BOT_MAX_TICKS=1` is the
/// documented one-shot — entering a multi-second recovery wait on it
/// would break the "reproduces the v0.1.x one-shot exactly" contract
/// and would hang the `/qa` smoke runs that use `ROK_BOT_MAX_TICKS=1`.
/// So recovery is enabled only for a genuine loop (`max_ticks >= 2`); a
/// one-shot run treats `WindowGone` / `WindowHidden` as `Fatal` and
/// aborts immediately, exactly like v0.1.x.
pub const fn recovery_enabled(max_ticks: u32) -> bool {
    max_ticks >= 2
}

/// Which window state [`recover_window`] is polling for (v0.2.1 L5).
/// `recover_window` is *entered* in one of these and may *retarget*
/// between them per poll — a hidden RoK that then crashes flips
/// `Hidden` → `Gone`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryTarget {
    /// RoK's window is gone — poll `find_rok_window` to re-discover it.
    Gone,
    /// RoK's window is hidden — poll `validate_window_present` until it
    /// is visible again.
    Hidden,
}

/// Whether [`recover_window`]'s poll loop should continue, give up, or
/// stop cleanly — the output of [`recovery_decision`] (v0.2.1 L5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryDecision {
    /// Keep polling — the deadline has not passed and no stop is pending.
    Continue,
    /// The recovery deadline elapsed — abort the loop with `LoopAborted`.
    Exhausted,
    /// The operator pressed Ctrl-C — stop the loop cleanly (exit 0).
    /// `Stopped` wins over `Exhausted` when both hold at once.
    Stopped,
}

/// How [`classify_recovery_err`] routes a *failed* recovery probe
/// (v0.2.1 L5; decisions D4 / D5). A probe *success* is handled inline
/// by [`recover_window`] — a `Gone` probe yields a fresh window, a
/// `Hidden` probe re-validates the original — so there is no
/// `Recovered` route here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryRoute {
    /// Re-target to `Gone` and re-discover via `find_rok_window`. The
    /// window is gone, or alive-but-moved (`frame_moved`, D5) — either
    /// way it needs a fresh, correctly-framed `RokWindow`.
    RetargetGone,
    /// Re-target to `Hidden` and keep waiting on visibility.
    RetargetHidden,
    /// Transient unavailability — RoK not re-enumerated yet, or an SCK
    /// fetch hiccup — keep polling the current target (D4).
    KeepPolling,
    /// A genuinely unrecoverable error — abort recovery, propagate it.
    Fatal,
}

/// The non-error outcome of [`recover_window`]: RoK is back (the loop
/// resumes), or the operator pressed Ctrl-C (the loop exits cleanly).
/// An *error* outcome — deadline exhausted, or a fatal probe error — is
/// the `Err` arm of `recover_window`'s `Result`.
#[derive(Debug)]
pub enum RecoveryOutcome {
    /// RoK is back. The loop resumes with this (possibly fresh) window.
    Recovered(RokWindow),
    /// Ctrl-C during the recovery wait — the loop stops cleanly (exit 0).
    Stopped,
}

/// Pure: should [`recover_window`]'s poll loop continue, give up, or
/// stop (v0.2.1 L5)?
///
/// `Stopped` (Ctrl-C) wins over `Exhausted` (deadline) when both hold —
/// a Ctrl-C landing exactly at the deadline boundary exits 0, not 21.
/// `elapsed == deadline` counts as exhausted (the deadline is inclusive).
pub fn recovery_decision(
    elapsed: Duration,
    deadline: Duration,
    stop_requested: bool,
) -> RecoveryDecision {
    if stop_requested {
        RecoveryDecision::Stopped
    } else if elapsed >= deadline {
        RecoveryDecision::Exhausted
    } else {
        RecoveryDecision::Continue
    }
}

/// Pure: route a *failed* recovery probe to a [`RecoveryRoute`]
/// (v0.2.1 L5; decisions D4 / D5).
///
/// `WindowChanged{window_id_gone}` and `WindowChanged{frame_moved}` →
/// `RetargetGone` (gone, or alive-but-moved — re-discover for a fresh
/// frame, D5). `WindowChanged{not_visible}` → `RetargetHidden`.
/// `WindowNotFound` and `CaptureFailed` → `KeepPolling` (RoK not
/// re-enumerated yet, or a transient SCK fetch hiccup — both absorbed
/// by the deadline-bounded poll, D4). Everything else → `Fatal`.
///
/// `validate_window_present` never returns `point_outside_frame` (that
/// check is pre-click-only), so the `WindowChanged` arms are exhaustive
/// over a real recovery probe's outputs.
pub fn classify_recovery_err(err: &BotError) -> RecoveryRoute {
    match err {
        BotError::WindowChanged { reason } => match *reason {
            window::REASON_WID_GONE | window::REASON_FRAME_MOVED => RecoveryRoute::RetargetGone,
            window::REASON_NOT_VISIBLE => RecoveryRoute::RetargetHidden,
            _ => RecoveryRoute::Fatal,
        },
        BotError::WindowNotFound | BotError::CaptureFailed { .. } => RecoveryRoute::KeepPolling,
        _ => RecoveryRoute::Fatal,
    }
}

/// Pure: the consecutive-failure counter's next value. A successful
/// tick resets the streak to 0; a transient failure increments it
/// (saturating). Not called for a fatal failure — the loop aborts on
/// that directly. The result feeds the `>= LOOP_FAILURE_BUDGET` check.
pub const fn next_failure_count(current: u32, tick_succeeded: bool) -> u32 {
    if tick_succeeded {
        0
    } else {
        current.saturating_add(1)
    }
}

/// Pure: should the loop stop CLEANLY (exit 0) at this tick boundary?
/// True when the operator pressed Ctrl-C (`stop_requested`) or the tick
/// cap is reached. Distinct from the failure-budget abort, which is an
/// error (`LoopAborted`, exit 21), not a clean stop.
pub const fn should_stop(stop_requested: bool, completed_ticks: u32, max_ticks: u32) -> bool {
    stop_requested || completed_ticks >= max_ticks
}

/// Pure: parse the `ROK_BOT_MAX_TICKS` env value into a tick cap.
///
/// Returns `(cap, fell_back)`. `fell_back` is true when the raw value
/// was present but unparseable — the caller logs a warn and uses the
/// default. Unset → `(DEFAULT_MAX_TICKS, false)`, not flagged. A valid
/// `0` is accepted (the loop exits before tick 1, a clean no-op run).
pub fn parse_max_ticks(raw: Option<&str>) -> (u32, bool) {
    raw.map_or((DEFAULT_MAX_TICKS, false), |s| {
        s.trim()
            .parse::<u32>()
            .map_or((DEFAULT_MAX_TICKS, true), |n| (n, false))
    })
}

/// Live: run one loop tick — probe window liveness, capture, match
/// both needles, click, and confirm the city↔world view toggled.
/// Returns the pre-click `Match` on success (the loop records it as
/// `last_match` to seed the next tick's fast last-position ROI).
///
/// v0.2.1 L4: the tick opens with a `validate_window_present` probe,
/// *before* the capture. A RoK relaunch leaves a dead `SCWindow`
/// handle and SCK's behavior capturing through it is undocumented; the
/// probe surfaces a relaunch (`window_id_gone`) or a hidden window
/// (`not_visible`) up front so the loop routes to window recovery (L5)
/// without betting on the capture failing cleanly.
///
/// `last_match` is the previous successful tick's match position (or
/// `None` on tick 1 / after a reset): it drives `select_roi` —
/// `Some` → a tight box around last position, `None` → the broad
/// castle quadrant. On a ROI miss the search falls back to the full
/// frame; a full-frame miss is `TargetNotFound` (a transient failure).
fn tick(
    window: &RokWindow,
    needles: &[&[u8]],
    last_match: Option<Match>,
    tick_num: u32,
) -> Result<Match> {
    // v0.2.1 L4: pre-capture window liveness probe. A relaunch
    // (window_id_gone) or a hidden window (not_visible) is caught here,
    // before the ~140 ms capture, and routed to window recovery (L5) —
    // independent of how SCK behaves on a dead SCWindow handle. The
    // probe's wall cost is logged so the per-tick CGWindow-enumeration
    // overhead is observable (D9).
    let probe_start = Instant::now();
    validate_window_present(window)?;
    tracing::info!(
        target: "rok_bot",
        tick = tick_num,
        probe_ms = probe_start.elapsed().as_millis() as u64,
        "tick: pre-capture window liveness probe OK"
    );

    let pre_path = PathBuf::from(TICK_CAPTURE_PRE_PATH);
    capture_window(&window.scwindow, &pre_path)?;

    // Match both needles. ROI = last-position box (if we have a prior
    // match) or the castle quadrant. A ROI miss falls back to a full-
    // frame search; a full-frame miss is a transient TargetNotFound.
    let roi_match = find_best_needle(&pre_path, needles, |w, h| {
        Some(select_roi(last_match, w, h))
    })?;
    let pre_nm: NeedleMatch = if let Some(nm) = roi_match {
        nm
    } else {
        tracing::info!(
            target: "rok_bot",
            tick = tick_num,
            "ROI search found no needle — running full-frame fallback"
        );
        find_best_needle(&pre_path, needles, |_, _| None)?.ok_or(BotError::TargetNotFound)?
    };
    validate_match_dims(&pre_nm.m)?;

    let (sx, sy) = screen_point(&pre_nm.m, &window.frame);
    tracing::info!(
        target: "rok_bot",
        tick = tick_num,
        needle_idx = pre_nm.needle_idx,
        match_x = pre_nm.m.x,
        match_y = pre_nm.m.y,
        score = pre_nm.m.score,
        screen_x = sx,
        screen_y = sy,
        "tick: pre-click match located"
    );

    let click_point = CGPoint::new(sx, sy);
    validate_at_click_site(window, click_point)?;
    click_at(window, click_point)?;

    verify::sleep_verify_delay();
    validate_window_present(window)?;

    let post_path = PathBuf::from(TICK_CAPTURE_POST_PATH);
    capture_window(&window.scwindow, &post_path)?;
    verify::confirm_needle_swap(&post_path, needles, pre_nm)?;

    Ok(pre_nm.m)
}

/// Live: poll until RoK's window is usable again, or give up (v0.2.1
/// L5). Entered from `run_loop`'s `WindowGone` / `WindowHidden` arm.
///
/// One bounded poll-with-backoff loop. It is *entered* in a
/// [`RecoveryTarget`] but re-classifies the live window state each poll
/// ([`classify_recovery_err`]), so a window that transitions
/// mid-recovery — a hidden RoK that then crashes — is handled by the
/// same loop. The poll interval and total deadline are fixed by the
/// *entry* state and do not reset on a retarget (decision D2).
///
/// Returns:
/// - `Ok(RecoveryOutcome::Recovered(w))` — RoK is back; the loop
///   resumes with `w` (a freshly discovered window after a `Gone`
///   recovery, the re-validated original after a `Hidden` one — D6).
/// - `Ok(RecoveryOutcome::Stopped)` — Ctrl-C during the wait; exit 0.
/// - `Err(LoopAborted { .._recovery_exhausted })` — the deadline
///   elapsed without RoK becoming usable.
/// - `Err(other)` — a genuinely unrecoverable probe error (D4).
fn recover_window(
    entry: RecoveryTarget,
    window: &RokWindow,
    stop: &AtomicBool,
) -> Result<RecoveryOutcome> {
    let (interval, deadline, exhausted_reason) = match entry {
        RecoveryTarget::Gone => (
            RELAUNCH_RECOVERY_INTERVAL,
            RELAUNCH_RECOVERY_DEADLINE,
            REASON_WINDOW_RECOVERY_EXHAUSTED,
        ),
        RecoveryTarget::Hidden => (
            VISIBILITY_RECOVERY_INTERVAL,
            VISIBILITY_RECOVERY_DEADLINE,
            REASON_VISIBILITY_RECOVERY_EXHAUSTED,
        ),
    };

    tracing::warn!(
        target: "rok_bot",
        ?entry,
        interval_s = interval.as_secs(),
        deadline_s = deadline.as_secs(),
        "window recovery starting — polling until RoK is usable (Ctrl-C to stop)"
    );

    let started = Instant::now();
    let mut target = entry;

    loop {
        let stop_requested = stop.load(Ordering::SeqCst);
        match recovery_decision(started.elapsed(), deadline, stop_requested) {
            RecoveryDecision::Stopped => {
                tracing::info!(
                    target: "rok_bot",
                    "Ctrl-C during window recovery — exiting cleanly"
                );
                return Ok(RecoveryOutcome::Stopped);
            }
            RecoveryDecision::Exhausted => {
                tracing::error!(
                    target: "rok_bot",
                    ?entry,
                    deadline_s = deadline.as_secs(),
                    "window recovery deadline exhausted — aborting loop"
                );
                return Err(BotError::LoopAborted {
                    reason: exhausted_reason,
                });
            }
            RecoveryDecision::Continue => {}
        }

        // Probe the current target. A `Gone` probe re-discovers RoK and
        // yields a fresh window; a `Hidden` probe re-validates the
        // window we already hold (decision D6). A probe failure routes
        // through `classify_recovery_err`.
        let probe_err: BotError = match target {
            RecoveryTarget::Gone => {
                window::invalidate_shareable_content_cache();
                match window::find_rok_window() {
                    Ok(fresh) => {
                        tracing::info!(
                            target: "rok_bot",
                            id = fresh.id,
                            pid = fresh.pid,
                            "window recovery: RoK re-discovered — resuming loop"
                        );
                        return Ok(RecoveryOutcome::Recovered(fresh));
                    }
                    Err(err) => err,
                }
            }
            RecoveryTarget::Hidden => match validate_window_present(window) {
                Ok(()) => {
                    tracing::info!(
                        target: "rok_bot",
                        "window recovery: RoK is visible again — resuming loop"
                    );
                    return Ok(RecoveryOutcome::Recovered(window.clone()));
                }
                Err(err) => err,
            },
        };

        match classify_recovery_err(&probe_err) {
            RecoveryRoute::RetargetGone => target = RecoveryTarget::Gone,
            RecoveryRoute::RetargetHidden => target = RecoveryTarget::Hidden,
            RecoveryRoute::KeepPolling => {}
            RecoveryRoute::Fatal => {
                tracing::error!(
                    target: "rok_bot",
                    error = %probe_err,
                    "unrecoverable error during window recovery — aborting loop"
                );
                return Err(probe_err);
            }
        }

        std::thread::sleep(interval);
    }
}

/// Live: run the v0.2 continuous loop until Ctrl-C or the tick cap.
///
/// `stop` is the `AtomicBool` the boot-installed `ctrlc` handler flips;
/// the loop checks it at each tick boundary, so a Ctrl-C lets the
/// in-flight tick finish before a clean exit (design D4).
///
/// Returns:
/// - `Ok(())` — stopped cleanly (Ctrl-C, or `completed_ticks` reached
///   `max_ticks`). `main` maps this to exit 0.
/// - `Err(fatal)` — a tick hit a `Fatal` error; it propagates unchanged
///   with its v0.1.x exit code.
/// - `Err(LoopAborted { failure_budget_exhausted })` — exit 21;
///   [`LOOP_FAILURE_BUDGET`] transient failures in a row.
/// - `Err(LoopAborted { *_recovery_exhausted })` — exit 21; a
///   `WindowGone` / `WindowHidden` recovery polled past its deadline
///   without RoK becoming usable (v0.2.1 L5).
///
/// Takes `window` by value (v0.2.1 L2): the loop owns it and swaps in a
/// freshly discovered `RokWindow` after a relaunch recovery.
pub fn run_loop(window: RokWindow, max_ticks: u32, stop: &AtomicBool) -> Result<()> {
    let needles: &[&[u8]] = &NEEDLES;
    let mut window = window;
    let mut last_match: Option<Match> = None;
    let mut consecutive_failures: u32 = 0;
    let mut consecutive_recoveries: u32 = 0;
    let mut completed_ticks: u32 = 0;

    tracing::info!(
        target: "rok_bot",
        max_ticks,
        failure_budget = LOOP_FAILURE_BUDGET,
        "continuous loop starting (Ctrl-C to stop)"
    );

    // Placeholder-needle heads-up. A sentinel needle (the v0.2
    // `world-button.png` until an operator crops it) is dormant — the
    // matcher skips it — so the needle-swap verify can never confirm a
    // toggle and every tick fails `ClickNotVerified`. The loop still
    // runs (it exercises the full boot→capture→match→click→verify
    // pipeline as a plumbing smoke test) and aborts cleanly via the
    // failure budget. Surfacing it once here keeps the eventual
    // `LoopAborted` from looking like a mystery.
    let placeholder_needles = count_placeholder_needles(needles);
    if placeholder_needles > 0 {
        tracing::warn!(
            target: "rok_bot",
            placeholder_needles,
            total_needles = needles.len(),
            "one or more needles are placeholders (e.g. \
             assets/targets/world-button.png) — the needle-swap verify \
             cannot confirm a city↔world toggle until they are cropped \
             from real RoK art. Every tick will fail ClickNotVerified and \
             the loop will abort with LoopAborted once the failure budget \
             is spent; the run still exercises the full \
             boot→capture→match→click→verify pipeline as a plumbing smoke \
             test."
        );
    }

    loop {
        let stop_requested = stop.load(Ordering::SeqCst);
        if should_stop(stop_requested, completed_ticks, max_ticks) {
            tracing::info!(
                target: "rok_bot",
                completed_ticks,
                stop_requested,
                "loop stop condition met — exiting cleanly"
            );
            return Ok(());
        }

        let tick_num = completed_ticks.saturating_add(1);
        match tick(&window, needles, last_match, tick_num) {
            Ok(m) => {
                last_match = Some(m);
                consecutive_failures = next_failure_count(consecutive_failures, true);
                // A genuinely successful tick clears the recovery streak
                // (/review D1): occasional relaunches are fine — only a
                // sustained crash-loop with no good tick between aborts.
                consecutive_recoveries = 0;
                tracing::info!(target: "rok_bot", tick = tick_num, "tick OK — toggle confirmed");
            }
            Err(err) => match classify_error(&err) {
                ErrorClass::Fatal => {
                    tracing::error!(
                        target: "rok_bot",
                        tick = tick_num,
                        error = %err,
                        "fatal tick error — aborting loop"
                    );
                    return Err(err);
                }
                // v0.2.1 L3 / L5: RoK's window is gone or hidden. Enter
                // window recovery — poll until RoK is usable again and
                // resume the loop. A one-shot run (`recovery_enabled` is
                // false for ROK_BOT_MAX_TICKS=1, L10/D7) skips recovery
                // and aborts immediately like v0.1.x. A successful
                // recovery resets the loop state (L6): the failure
                // streak clears, `last_match` drops (position knowledge
                // is stale across a relaunch), and `continue` skips the
                // `completed_ticks` bump — recovery is not a tick.
                class @ (ErrorClass::WindowGone | ErrorClass::WindowHidden) => {
                    if !recovery_enabled(max_ticks) {
                        tracing::error!(
                            target: "rok_bot",
                            tick = tick_num,
                            error = %err,
                            "window-lifecycle error on a one-shot run \
                             (ROK_BOT_MAX_TICKS=1) — recovery disabled, aborting"
                        );
                        return Err(err);
                    }
                    let entry = if class == ErrorClass::WindowGone {
                        RecoveryTarget::Gone
                    } else {
                        RecoveryTarget::Hidden
                    };
                    match recover_window(entry, &window, stop)? {
                        RecoveryOutcome::Recovered(fresh) => {
                            window = fresh;
                            last_match = None;
                            consecutive_failures = 0;
                            // /review D1: bound the recoveries. A
                            // crash-looping RoK makes every tick recover —
                            // without this the loop spins forever
                            // (recovery is not a tick, and it just reset
                            // consecutive_failures). RECOVERY_BUDGET in a
                            // row with no successful tick → abort.
                            consecutive_recoveries = consecutive_recoveries.saturating_add(1);
                            if consecutive_recoveries >= RECOVERY_BUDGET {
                                tracing::error!(
                                    target: "rok_bot",
                                    consecutive_recoveries,
                                    recovery_budget = RECOVERY_BUDGET,
                                    "recovery budget exhausted — RoK is \
                                     crash-looping or the display is \
                                     flapping; aborting loop"
                                );
                                return Err(BotError::LoopAborted {
                                    reason: REASON_RECOVERY_BUDGET_EXHAUSTED,
                                });
                            }
                            tracing::info!(
                                target: "rok_bot",
                                consecutive_recoveries,
                                recovery_budget = RECOVERY_BUDGET,
                                "window recovered — loop resuming"
                            );
                        }
                        RecoveryOutcome::Stopped => {
                            tracing::info!(
                                target: "rok_bot",
                                completed_ticks,
                                "loop stopped during window recovery — exiting cleanly"
                            );
                            return Ok(());
                        }
                    }
                    continue;
                }
                ErrorClass::Transient => {
                    // A failed tick means our position knowledge is
                    // stale — reset last_match so the next tick searches
                    // the broad castle ROI, not a stale last-position box.
                    last_match = None;
                    consecutive_failures = next_failure_count(consecutive_failures, false);
                    tracing::warn!(
                        target: "rok_bot",
                        tick = tick_num,
                        error = %err,
                        consecutive_failures,
                        failure_budget = LOOP_FAILURE_BUDGET,
                        "transient tick failure"
                    );
                    if consecutive_failures >= LOOP_FAILURE_BUDGET {
                        tracing::error!(
                            target: "rok_bot",
                            consecutive_failures,
                            "consecutive-failure budget exhausted — aborting loop"
                        );
                        return Err(BotError::LoopAborted {
                            reason: REASON_FAILURE_BUDGET_EXHAUSTED,
                        });
                    }
                }
            },
        }
        completed_ticks = completed_ticks.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::STAGE_PNG_WRITE;
    use crate::click::REASON_DOWN;
    use crate::permissions::ACCESSIBILITY;
    use crate::verify::REASON_NO_SWAP;
    use crate::window::{
        REASON_FRAME_MOVED, REASON_NOT_VISIBLE, REASON_POINT_OUTSIDE_FRAME, REASON_WID_GONE,
    };

    // ---------- constants ----------

    #[test]
    fn loop_failure_budget_pinned() {
        // D5 locked the budget at 3. Changing it is a tuning decision —
        // pin so a casual change is intentional.
        assert_eq!(LOOP_FAILURE_BUDGET, 3);
    }

    #[test]
    fn default_max_ticks_is_finite_and_sane() {
        // D4: the default cap must be finite (the backstop for an
        // unattended run) and large enough to be a useful demo.
        assert!((1..=10_000).contains(&DEFAULT_MAX_TICKS));
    }

    #[test]
    fn reason_constants_pinned_to_documented_values() {
        // error::tests + shell log parsers pattern-match these strings.
        assert_eq!(REASON_FAILURE_BUDGET_EXHAUSTED, "failure_budget_exhausted");
        assert_eq!(
            REASON_SIGNAL_INSTALL_FAILED,
            "signal_handler_install_failed"
        );
        assert_eq!(MAX_TICKS_ENV, "ROK_BOT_MAX_TICKS");
    }

    // ---------- classify_error (exhaustive over BotError) ----------

    #[test]
    fn classify_error_fatal_variants() {
        // D5 FATAL set + the structural variants that default Fatal.
        // v0.2.1 L3: window_id_gone moved out of this set (→ WindowGone);
        // frame_moved / point_outside_frame stay Fatal.
        let fatal = [
            BotError::PermissionsMissing {
                which: ACCESSIBILITY,
            },
            BotError::ClickFailed {
                reason: REASON_DOWN,
            },
            BotError::WindowChanged {
                reason: REASON_FRAME_MOVED,
            },
            BotError::WindowChanged {
                reason: REASON_POINT_OUTSIDE_FRAME,
            },
            BotError::WindowNotFound,
            BotError::WindowScreenUnresolved,
            BotError::ImageLoadFailed { which: "haystack" },
            BotError::TargetTooLarge {
                needle: (200, 200),
                haystack: (100, 100),
            },
            BotError::LoopAborted {
                reason: REASON_FAILURE_BUDGET_EXHAUSTED,
            },
        ];
        for err in fatal {
            assert_eq!(
                classify_error(&err),
                ErrorClass::Fatal,
                "{err:?} must classify Fatal"
            );
        }
    }

    #[test]
    fn classify_error_transient_variants() {
        // D5 TRANSIENT set. v0.2.1 L3: not_visible moved out of this
        // set (→ WindowHidden); CaptureFailed stays Transient.
        let transient = [
            BotError::TargetNotFound,
            BotError::ClickNotVerified {
                reason: REASON_NO_SWAP,
            },
            BotError::CaptureFailed {
                stage: STAGE_PNG_WRITE,
                exit_code: None,
            },
        ];
        for err in transient {
            assert_eq!(
                classify_error(&err),
                ErrorClass::Transient,
                "{err:?} must classify Transient"
            );
        }
    }

    #[test]
    fn classify_error_window_changed_splits_by_reason() {
        // The split is the load-bearing part of D5/D12 + v0.2.1 L3:
        // one variant, four-way class depending on the reason tag.
        assert_eq!(
            classify_error(&BotError::WindowChanged {
                reason: REASON_WID_GONE
            }),
            ErrorClass::WindowGone,
            "window_id_gone → WindowGone (v0.2.1 L3: enter relaunch recovery)"
        );
        assert_eq!(
            classify_error(&BotError::WindowChanged {
                reason: REASON_NOT_VISIBLE
            }),
            ErrorClass::WindowHidden,
            "not_visible → WindowHidden (v0.2.1 L3: enter visibility recovery)"
        );
        assert_eq!(
            classify_error(&BotError::WindowChanged {
                reason: REASON_FRAME_MOVED
            }),
            ErrorClass::Fatal,
            "frame_moved → Fatal (loop can't ride through a frame drift)"
        );
        assert_eq!(
            classify_error(&BotError::WindowChanged {
                reason: REASON_POINT_OUTSIDE_FRAME
            }),
            ErrorClass::Fatal,
            "point_outside_frame → Fatal (operator-side coord-math bug)"
        );
    }

    // ---------- next_failure_count ----------

    #[test]
    fn next_failure_count_resets_on_success() {
        assert_eq!(next_failure_count(0, true), 0);
        assert_eq!(next_failure_count(2, true), 0);
        assert_eq!(next_failure_count(u32::MAX, true), 0);
    }

    #[test]
    fn next_failure_count_increments_on_failure() {
        assert_eq!(next_failure_count(0, false), 1);
        assert_eq!(next_failure_count(1, false), 2);
        assert_eq!(next_failure_count(2, false), 3);
    }

    #[test]
    fn next_failure_count_saturates_at_u32_max() {
        // Defense-in-depth: a pathologically long failure streak must
        // not wrap (which would falsely reset below the budget).
        assert_eq!(next_failure_count(u32::MAX, false), u32::MAX);
    }

    // ---------- should_stop ----------

    #[test]
    fn should_stop_false_mid_run() {
        assert!(!should_stop(false, 0, 100));
        assert!(!should_stop(false, 99, 100));
    }

    #[test]
    fn should_stop_true_on_ctrl_c() {
        // Ctrl-C stops even with ticks remaining.
        assert!(should_stop(true, 0, 100));
        assert!(should_stop(true, 50, 100));
    }

    #[test]
    fn should_stop_true_at_tick_cap() {
        assert!(should_stop(false, 100, 100));
        assert!(should_stop(false, 101, 100));
    }

    #[test]
    fn should_stop_true_when_max_ticks_zero() {
        // ROK_BOT_MAX_TICKS=0 → stop before tick 1, a clean no-op run.
        assert!(should_stop(false, 0, 0));
    }

    #[test]
    fn should_stop_max_ticks_one_reproduces_one_shot() {
        // D4: ROK_BOT_MAX_TICKS=1 must run exactly one tick. Tick 1
        // proceeds (completed=0 < 1); after it, completed=1 stops.
        assert!(!should_stop(false, 0, 1), "tick 1 must proceed");
        assert!(should_stop(false, 1, 1), "loop stops after exactly 1 tick");
    }

    // ---------- parse_max_ticks ----------

    #[test]
    fn parse_max_ticks_unset_uses_default() {
        assert_eq!(parse_max_ticks(None), (DEFAULT_MAX_TICKS, false));
    }

    #[test]
    fn parse_max_ticks_valid_values() {
        assert_eq!(parse_max_ticks(Some("1")), (1, false));
        assert_eq!(parse_max_ticks(Some("0")), (0, false));
        assert_eq!(parse_max_ticks(Some("250")), (250, false));
        // Surrounding whitespace is trimmed.
        assert_eq!(parse_max_ticks(Some("  7  ")), (7, false));
    }

    #[test]
    fn parse_max_ticks_invalid_values_fall_back_and_flag() {
        // Non-numeric, negative, overflowing, and empty all fall back
        // to the default with the fell_back flag set so the caller warns.
        for raw in ["abc", "-1", "99999999999999999999", "", "  ", "1.5", "3x"] {
            assert_eq!(
                parse_max_ticks(Some(raw)),
                (DEFAULT_MAX_TICKS, true),
                "{raw:?} must fall back to the default and flag invalid"
            );
        }
    }

    // ---------- recovery_enabled (L10 / D7) ----------

    #[test]
    fn recovery_enabled_disabled_for_one_shot() {
        // ROK_BOT_MAX_TICKS=1 (and the degenerate 0) is a one-shot — no
        // recovery, so it reproduces the v0.1.x one-shot exactly (D7).
        assert!(!recovery_enabled(0));
        assert!(!recovery_enabled(1));
    }

    #[test]
    fn recovery_enabled_on_for_a_real_loop() {
        assert!(recovery_enabled(2));
        assert!(recovery_enabled(100));
        assert!(recovery_enabled(u32::MAX));
    }

    // ---------- recovery_decision ----------

    #[test]
    fn recovery_decision_continues_mid_wait() {
        assert_eq!(
            recovery_decision(Duration::from_secs(5), Duration::from_secs(60), false),
            RecoveryDecision::Continue
        );
    }

    #[test]
    fn recovery_decision_exhausted_past_deadline() {
        assert_eq!(
            recovery_decision(Duration::from_secs(61), Duration::from_secs(60), false),
            RecoveryDecision::Exhausted
        );
    }

    #[test]
    fn recovery_decision_exhausted_at_deadline_boundary() {
        // elapsed == deadline is exhausted — the deadline is inclusive.
        assert_eq!(
            recovery_decision(Duration::from_secs(60), Duration::from_secs(60), false),
            RecoveryDecision::Exhausted
        );
    }

    #[test]
    fn recovery_decision_stop_requested_returns_stopped() {
        assert_eq!(
            recovery_decision(Duration::from_secs(5), Duration::from_secs(60), true),
            RecoveryDecision::Stopped
        );
    }

    #[test]
    fn recovery_decision_stopped_beats_exhausted() {
        // A Ctrl-C landing exactly when the deadline also elapsed must
        // exit 0 (Stopped), not 21 (Exhausted).
        assert_eq!(
            recovery_decision(Duration::from_secs(60), Duration::from_secs(60), true),
            RecoveryDecision::Stopped
        );
    }

    // ---------- classify_recovery_err ----------

    #[test]
    fn classify_recovery_err_wid_gone_and_frame_moved_retarget_gone() {
        // D5: gone, or alive-but-moved — both re-discover for a fresh frame.
        for reason in [REASON_WID_GONE, REASON_FRAME_MOVED] {
            assert_eq!(
                classify_recovery_err(&BotError::WindowChanged { reason }),
                RecoveryRoute::RetargetGone,
                "{reason} must route RetargetGone"
            );
        }
    }

    #[test]
    fn classify_recovery_err_not_visible_retargets_hidden() {
        assert_eq!(
            classify_recovery_err(&BotError::WindowChanged {
                reason: REASON_NOT_VISIBLE
            }),
            RecoveryRoute::RetargetHidden
        );
    }

    #[test]
    fn classify_recovery_err_window_not_found_and_capture_failed_keep_polling() {
        // D4: RoK not re-enumerated yet, or a transient SCK fetch
        // hiccup — the deadline-bounded poll absorbs both.
        assert_eq!(
            classify_recovery_err(&BotError::WindowNotFound),
            RecoveryRoute::KeepPolling
        );
        assert_eq!(
            classify_recovery_err(&BotError::CaptureFailed {
                stage: STAGE_PNG_WRITE,
                exit_code: None,
            }),
            RecoveryRoute::KeepPolling
        );
    }

    #[test]
    fn classify_recovery_err_fatal_for_unrecoverable() {
        // PermissionsMissing / ClickFailed and any other non-window
        // error short-circuit recovery.
        for err in [
            BotError::PermissionsMissing {
                which: ACCESSIBILITY,
            },
            BotError::ClickFailed {
                reason: REASON_DOWN,
            },
        ] {
            assert_eq!(
                classify_recovery_err(&err),
                RecoveryRoute::Fatal,
                "{err:?} must route Fatal"
            );
        }
    }

    #[test]
    fn classify_recovery_err_frame_moved_differs_from_classify_error() {
        // frame_moved is Fatal for a per-tick error (classify_error) but
        // RetargetGone inside recovery — a window that reappeared moved
        // is recoverable by re-discovery (D5), not a loop-ending fault.
        assert_eq!(
            classify_error(&BotError::WindowChanged {
                reason: REASON_FRAME_MOVED
            }),
            ErrorClass::Fatal
        );
        assert_eq!(
            classify_recovery_err(&BotError::WindowChanged {
                reason: REASON_FRAME_MOVED
            }),
            RecoveryRoute::RetargetGone
        );
    }

    #[test]
    fn classify_recovery_err_unknown_window_changed_reason_is_fatal() {
        // The inner WindowChanged `_` arm. validate_window_present never
        // emits point_outside_frame (it is pre-click-only), but
        // classify_recovery_err is total — an unrecognized WindowChanged
        // reason must route Fatal, not a retarget. Mirrors how
        // classify_error_window_changed_splits_by_reason pins the
        // classify_error catch-all.
        assert_eq!(
            classify_recovery_err(&BotError::WindowChanged {
                reason: REASON_POINT_OUTSIDE_FRAME
            }),
            RecoveryRoute::Fatal
        );
    }

    // ---------- recovery constants ----------

    #[test]
    fn recovery_intervals_and_deadlines_in_sane_range() {
        // Interval well under deadline so a recovery gets many polls;
        // deadlines finite so a permanently-gone RoK still aborts.
        assert!(RELAUNCH_RECOVERY_INTERVAL < RELAUNCH_RECOVERY_DEADLINE);
        assert!(VISIBILITY_RECOVERY_INTERVAL < VISIBILITY_RECOVERY_DEADLINE);
        assert_eq!(RELAUNCH_RECOVERY_INTERVAL, Duration::from_secs(2));
        assert_eq!(RELAUNCH_RECOVERY_DEADLINE, Duration::from_secs(60));
        assert_eq!(VISIBILITY_RECOVERY_INTERVAL, Duration::from_secs(5));
        assert_eq!(VISIBILITY_RECOVERY_DEADLINE, Duration::from_secs(1800));
    }

    #[test]
    fn recovery_reason_constants_pinned() {
        // error::tests + shell log parsers pattern-match these strings.
        assert_eq!(
            REASON_WINDOW_RECOVERY_EXHAUSTED,
            "window_recovery_exhausted"
        );
        assert_eq!(
            REASON_VISIBILITY_RECOVERY_EXHAUSTED,
            "visibility_recovery_exhausted"
        );
        assert_eq!(
            REASON_RECOVERY_BUDGET_EXHAUSTED,
            "recovery_budget_exhausted"
        );
    }

    #[test]
    fn recovery_budget_pinned() {
        // /review D1 locked the recovery budget at 3, mirroring
        // LOOP_FAILURE_BUDGET. Pin so a casual change is intentional.
        assert_eq!(RECOVERY_BUDGET, 3);
    }
}
