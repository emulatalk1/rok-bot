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
//! register WindowServer                ┌─► capture pre PNG
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
//! - **D5 / D12** — error policy. [`classify_error`] splits every
//!   `BotError` into [`ErrorClass::Fatal`] (abort the loop immediately)
//!   and [`ErrorClass::Transient`] (count toward [`LOOP_FAILURE_BUDGET`]
//!   consecutive failures; a successful tick resets the count).
//! - **D9 / T1** — the decision core ([`classify_error`],
//!   [`next_failure_count`], [`should_stop`], [`parse_max_ticks`], plus
//!   `matcher::select_roi`) is pure free functions, exhaustively unit-
//!   tested with no live RoK. [`tick`] and [`run_loop`] are thin live
//!   wiring covered by `#[ignore]`'d integration tests.
//! - **D11** — verification is `verify::confirm_needle_swap` (a
//!   different needle matching the toggle ROI post-click), not pixel-diff.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

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

/// Pre-click capture path, rewritten every tick. Relative to cwd;
/// `.gitignore` covers the `rok-capture-*.png` wildcard. `capture.rs`'s
/// `O_NOFOLLOW + O_EXCL` open unlinks the prior tick's file and
/// recreates it, so reusing one path across ticks is race-free.
const TICK_CAPTURE_PRE_PATH: &str = "rok-capture-pre.png";

/// Post-click capture path, rewritten every tick. Sibling of
/// [`TICK_CAPTURE_PRE_PATH`].
const TICK_CAPTURE_POST_PATH: &str = "rok-capture-post.png";

/// How [`classify_error`] sorts a per-tick `BotError` for the loop's
/// error policy (design D5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// Abort the loop immediately — the error will not self-heal
    /// (RoK closed, a permission is missing, the click pipeline broke).
    Fatal,
    /// Count toward [`LOOP_FAILURE_BUDGET`]. One is noise; a streak
    /// aborts. A successful tick resets the streak to 0.
    Transient,
}

/// Pure: sort a `BotError` into [`ErrorClass::Fatal`] or
/// [`ErrorClass::Transient`] for the loop's per-tick error policy
/// (design D5, with D12's `not_visible` reclassification).
///
/// Total over every `BotError` variant. The boot-only / structural
/// variants (`WindowNotFound`, `ImageLoadFailed`, …) cannot occur
/// per-tick given the boot sequence runs `find_rok_window` before the
/// loop — but `classify_error` must be exhaustive, so they default to
/// `Fatal`: an unanticipated error aborts cleanly rather than spinning.
pub fn classify_error(err: &BotError) -> ErrorClass {
    match err {
        // WindowChanged splits by reason (window_id_gone fatal,
        // not_visible transient per D12) — handle it before the blanket
        // arms below.
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

/// Pure: classify a `WindowChanged` error by its reason tag.
fn classify_window_changed(reason: &str) -> ErrorClass {
    match reason {
        // A brief hide: Space switch, momentary idle. D12 reclassified
        // not_visible FATAL→TRANSIENT — survive a blip, abort on a
        // sustained hide once the failure budget is spent.
        window::REASON_NOT_VISIBLE => ErrorClass::Transient,
        // Everything else is fatal. window_id_gone = RoK closed/crashed,
        // not coming back this run. frame_moved / point_outside_frame
        // are not in D5's transient list — on a healthy loop the window
        // neither moves nor yields an out-of-frame click point, so
        // either signals a state the loop can't ride through. Abort
        // cleanly rather than burn the budget; future reason tags
        // default here too.
        _ => ErrorClass::Fatal,
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

/// Live: run one loop tick — capture, match both needles, click, and
/// confirm the city↔world view toggled. Returns the pre-click
/// `Match` on success (the loop records it as `last_match` to seed the
/// next tick's fast last-position ROI).
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
pub fn run_loop(window: &RokWindow, max_ticks: u32, stop: &AtomicBool) -> Result<()> {
    let needles: &[&[u8]] = &NEEDLES;
    let mut last_match: Option<Match> = None;
    let mut consecutive_failures: u32 = 0;
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
        match tick(window, needles, last_match, tick_num) {
            Ok(m) => {
                last_match = Some(m);
                consecutive_failures = next_failure_count(consecutive_failures, true);
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
        let fatal = [
            BotError::PermissionsMissing {
                which: ACCESSIBILITY,
            },
            BotError::ClickFailed {
                reason: REASON_DOWN,
            },
            BotError::WindowChanged {
                reason: REASON_WID_GONE,
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
        // D5 TRANSIENT set, including D12's not_visible reclassification.
        let transient = [
            BotError::TargetNotFound,
            BotError::ClickNotVerified {
                reason: REASON_NO_SWAP,
            },
            BotError::CaptureFailed {
                stage: STAGE_PNG_WRITE,
                exit_code: None,
            },
            BotError::WindowChanged {
                reason: REASON_NOT_VISIBLE,
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
        // The split is the load-bearing part of D5/D12: same variant,
        // different class depending on the reason tag.
        assert_eq!(
            classify_error(&BotError::WindowChanged {
                reason: REASON_WID_GONE
            }),
            ErrorClass::Fatal,
            "window_id_gone → Fatal (RoK closed)"
        );
        assert_eq!(
            classify_error(&BotError::WindowChanged {
                reason: REASON_NOT_VISIBLE
            }),
            ErrorClass::Transient,
            "not_visible → Transient (D12: survive a brief hide)"
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
}
