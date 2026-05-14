//! After-state verification: confirm `click::click_at` visibly changed RoK's UI.
//!
//! v0.1.4 closes the silent-failure gap v0.1.3 left open. `CGEvent::post`
//! returns `()` — once the synthetic mouse event is queued, the OS gives no
//! signal about whether the target window actually consumed it. v0.1.3 exits
//! 0 after the post regardless of whether RoK reacted. This module turns
//! "click landed visibly" from an unverified assumption into a checked
//! invariant.
//!
//! ## The gate is pixel-diff
//!
//! `after_state` captures the haystack-shaped picture twice — once before
//! the click (already done by `main.rs::run` for the `find_target` step) and
//! once after a [`VERIFY_DELAY_MS`] sleep. The two captures are decoded to
//! Luma8 (reusing `matcher::load_haystack`) and compared pixel-by-pixel.
//! If the count of differing pixels is below
//! [`PIXEL_DIFF_REJECT_THRESHOLD`], the verdict is
//! `BotError::ClickNotVerified { reason: REASON_SCREEN_UNCHANGED }` →
//! exit 20.
//!
//! ### Why pixel-diff, not byte-diff
//!
//! /plan-eng-review Outside Voice F3 caught that PNG file-byte diff is
//! unreliable in two directions:
//!
//! - PNG uses DEFLATE compression: a single-pixel change can shift the
//!   entire compressed byte stream. Two captures that differ by one
//!   visible pixel can differ by thousands of bytes.
//! - Conversely, two captures of pixel-identical screens can differ in
//!   their PNG bytes due to encoder timing, gAMA / metadata chunks, and
//!   non-deterministic deflate level choices.
//!
//! Pixel-space comparison (decode → Luma8 → byte-by-byte over `as_raw()`)
//! has neither problem. The threshold reads as "N decoded pixels changed",
//! which has real meaning the operator can reason about.
//!
//! ### Why no `match_stable` failure path
//!
//! An earlier draft of v0.1.4 included a second failure reason
//! (`match_stable`) that fired when the post-click re-match found the
//! target at the same coords with the same score. /plan-eng-review
//! Outside Voice F2 caught this: most RoK buttons stay on screen after
//! click (dropdowns, tabs, selections). Hard-failing on `same target at
//! same coords` mis-flags those legitimate clicks. The re-match still
//! runs inside `after_state` — but for diagnostic logging only, not for
//! the verdict.
//!
//! ## Scope: UI-local clicks only
//!
//! [`VERIFY_DELAY_MS`] = 500ms covers RoK's typical 100-300ms UI-local
//! transitions (toggle, dropdown, modal-open) with 2× margin. Server-bound
//! clicks (resource spend, troop dispatch) show a UI spinner for 1-3s; at
//! 500ms the post-capture lands mid-spinner and the verify gate
//! false-fails. That's a deliberate v0.1.4 scope decision — retry-and-poll
//! at multiple delay tiers is in TODOS.md (P2 — v0.1.4+ server-roundtrip
//! click verify).
//!
//! ## TOCTOU note
//!
//! `main.rs::run` re-validates the RoK window between click and
//! post-capture via `window::validate_window_present` (a 2-check sibling
//! of `validate_at_click_site` that drops the topmost-at-click invariant).
//! See window.rs docs for why the post-capture check is structurally
//! different from the pre-click check.

use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use image::GrayImage;

use crate::error::{BotError, Result};
use crate::matcher::{Match, find_target_in_castle_roi, load_haystack};

/// Wall-clock sleep between `click::click_at` returning and the post-click
/// capture. Long enough to let RoK render typical UI-local transitions
/// (button-press flash 50-100ms, panel slide-in 200-400ms, modal fade-in
/// ~400ms), short enough that operator one-shot runs feel responsive
/// (~1.5s total bot latency). p3-spike observed "within ~1s" for visible
/// RoK responses; 500ms gives ~2× margin under the perceptual threshold.
///
/// **Scope:** covers UI-local clicks. Server-bound clicks (1-3s response
/// for resource spend / troop dispatch) need retry-and-poll at multiple
/// delay tiers — deferred to TODOS P2 ("v0.1.4+ — server-roundtrip click
/// verify"). Don't tune this constant to cover server-bound: it'd double
/// v0.2 continuous-loop cycle time for the common UI-local case.
pub const VERIFY_DELAY_MS: u64 = 500;

/// Minimum pixel-diff (count of differing Luma8 pixels between pre and
/// post captures) for verify to PASS. Strictly less than this → fail with
/// `REASON_SCREEN_UNCHANGED` exit 20.
///
/// `1000` is a calibration starting point, not an empirical floor.
/// p2-spike + p3-spike data is in byte-space, not pixel-space — pixel-diff
/// values for the four scenarios (quiescent + no-click, quiescent +
/// click, animating + no-click, animating + click) need real-data
/// calibration once v0.1.4 deploys. The `tracing::info!` line in
/// `after_state` emits the diff value on every run so the operator can
/// collect samples.
///
/// See TODOS.md P3 ("v0.1.4+ — empirical calibration of
/// `PIXEL_DIFF_REJECT_THRESHOLD`") for the calibration approach.
pub const PIXEL_DIFF_REJECT_THRESHOLD: u64 = 1000;

/// Reason tags surfaced via `BotError::ClickNotVerified { reason }`. Pinned
/// as `&'static str` so operator-facing log lines and shell pattern-matchers
/// see one of these exact values.
///
/// `REASON_SCREEN_UNCHANGED` is the common case: pre/post captures decoded
/// successfully, dims matched, and pixel-diff fell below the threshold —
/// RoK did not visibly react. /plan-eng-review Outside Voice F2 forced
/// dropping the originally-planned `match_stable` tag (mis-flagged
/// stays-visible buttons).
///
/// `REASON_DIM_MISMATCH` fires when the pre and post captures decode to
/// `GrayImage`s with different dimensions. The verify gate has no
/// meaningful comparison to make in this state: pixel-diff over differently
/// shaped buffers is undefined, and the dim mismatch itself is evidence
/// the capture pipeline state-changed between pre and post (display DPI
/// reconfig mid-flow, RoK toggled fullscreen-borderless, screencapture
/// padded to a different size). Adversarial review caught a fail-open
/// path where the dim-mismatch sentinel passed verdict; this tag closes
/// it. Operator sees exit 20 with a reason that points at the capture
/// pipeline, not at the click.
pub const REASON_SCREEN_UNCHANGED: &str = "screen_unchanged";
pub const REASON_DIM_MISMATCH: &str = "dim_mismatch";

/// Pure: count Luma8 pixels where `a` and `b` differ.
///
/// Returns `u64::MAX` sentinel if `a.dimensions() != b.dimensions()` —
/// caller should treat as "definitely changed" (mismatched dims usually
/// signal a capture-pipeline issue like a window resize between captures,
/// which is exactly the kind of state-change verify should pass on).
///
/// Comparison is over `Luma8.as_raw()` byte slices: each pixel is one
/// byte (Luma8 = 1 channel × 1 byte/channel), so byte-by-byte equality
/// over the raw buffer is pixel-by-pixel equality.
pub fn pixel_diff(a: &GrayImage, b: &GrayImage) -> u64 {
    if a.dimensions() != b.dimensions() {
        return u64::MAX;
    }
    a.as_raw()
        .iter()
        .zip(b.as_raw().iter())
        .filter(|(x, y)| x != y)
        .count() as u64
}

/// Pure: turn a pixel-diff count into a verdict.
///
/// Strict `<` semantics: `diff == PIXEL_DIFF_REJECT_THRESHOLD` passes
/// (exactly threshold-many pixels changed is interpreted as "just enough
/// change to count as verified"). A `diff` value of `u64::MAX` (the
/// dim-mismatch sentinel from `pixel_diff`) passes trivially — see
/// `pixel_diff` docs for why dim mismatch is treated as "definitely
/// changed."
pub const fn verdict_from_pixel_diff(diff: u64) -> Result<()> {
    if diff < PIXEL_DIFF_REJECT_THRESHOLD {
        return Err(BotError::ClickNotVerified {
            reason: REASON_SCREEN_UNCHANGED,
        });
    }
    Ok(())
}

/// Live: load pre + post captures, pixel-diff them, run a diagnostic
/// re-match, return verdict.
///
/// Sequence:
///
/// 1. Decode pre + post PNGs via [`matcher::load_haystack`] (reuses the
///    same dimension-limited Luma8 path as `find_target`). Either decode
///    failing surfaces as `BotError::ImageLoadFailed { which: "haystack" }`
///    exit 16 — same contract as the pre-click match.
/// 2. Compute `pixel_diff(pre, post)`. Log at `info!` level with the
///    threshold value alongside for operator visibility.
/// 3. Run `find_target(post_path)` for **diagnostic logging only**. The
///    result is logged but does not drive the verdict:
///    - `Ok(Some(m))` → target still present; log pre/post coords + score
///      delta as a neutral observation.
///    - `Ok(None)` → target gone; log as a strong positive signal.
///    - `Err(_)` → diagnostic failed; log warn, continue. The pixel-diff
///      verdict stands.
/// 4. Return `verdict_from_pixel_diff(diff)`.
///
/// Diagnostic re-match is best-effort. Its errors do NOT propagate — a
/// failed re-match is just lost log info, not a verify failure. The
/// pixel-diff is the only source of truth for the verdict.
pub fn after_state(pre_path: &Path, post_path: &Path, pre_match: &Match) -> Result<()> {
    let pre = load_haystack(pre_path)?;
    let post = load_haystack(post_path)?;

    // Fail-closed on dim mismatch. pixel_diff has a u64::MAX sentinel for
    // this case, but routing it through verdict_from_pixel_diff produces
    // Ok (sentinel > threshold) — which silently passes verify on a
    // capture-pipeline integrity drift. Adversarial review caught this as
    // a fail-open defect; explicit check here closes it with a distinct
    // reason tag the operator can diagnose against.
    if pre.dimensions() != post.dimensions() {
        tracing::warn!(
            target: "rok_bot",
            pre_dims = ?pre.dimensions(),
            post_dims = ?post.dimensions(),
            "after-state dim mismatch — capture pipeline state changed between \
             pre and post (display reconfig, fullscreen toggle, screencapture \
             padding); verify cannot meaningfully compare"
        );
        return Err(BotError::ClickNotVerified {
            reason: REASON_DIM_MISMATCH,
        });
    }

    let diff = pixel_diff(&pre, &post);

    tracing::info!(
        target: "rok_bot",
        pixel_diff = diff,
        threshold = PIXEL_DIFF_REJECT_THRESHOLD,
        pre_dims = ?pre.dimensions(),
        post_dims = ?post.dimensions(),
        "after-state pixel diff computed"
    );

    log_post_match_diagnostic(post_path, pre_match);

    verdict_from_pixel_diff(diff)
}

/// Diagnostic-only: re-match on the post haystack and log the outcome
/// vs the pre-match. Best-effort; errors do not propagate.
fn log_post_match_diagnostic(post_path: &Path, pre_match: &Match) {
    match find_target_in_castle_roi(post_path) {
        Ok(Some(m)) => {
            // Use abs_diff to avoid u32 underflow when post.x < pre.x
            // (Outside Voice F7 / /plan-eng-review D9). Neither delta
            // drives the verdict — these are purely diagnostic numbers
            // for the operator's log — but `0u32 - 1u32` would still
            // panic in debug builds and surface as a misleading test
            // failure if we got it wrong.
            tracing::info!(
                target: "rok_bot",
                pre_x = pre_match.x,
                pre_y = pre_match.y,
                pre_score = pre_match.score,
                post_x = m.x,
                post_y = m.y,
                post_score = m.score,
                dx = u32::abs_diff(m.x, pre_match.x),
                dy = u32::abs_diff(m.y, pre_match.y),
                "post-match diagnostic: target still found"
            );
        }
        Ok(None) => {
            // Earlier draft logged this as "strong positive signal (target gone)"
            // but adversarial review noted the same line fires when the matched
            // button visually darkens/depresses post-click: the highlight
            // transition can drop the NCC score below MATCH_THRESHOLD, surfacing
            // as Ok(None) even though the target is still on screen. The
            // operator reading the log would mis-diagnose "view toggled" when
            // really "button-press highlight transition." Reword to surface the
            // ambiguity rather than over-promising.
            tracing::info!(
                target: "rok_bot",
                "post-match diagnostic: target absent or scored below MATCH_THRESHOLD \
                 (could be a real state change, OR a button-press / highlight \
                 transition that dropped the NCC score; verify via image inspection \
                 of rok-capture-post.png if uncertain)"
            );
        }
        Err(err) => {
            tracing::warn!(
                target: "rok_bot",
                error = %err,
                "post-match diagnostic failed; verdict relies on pixel-diff alone"
            );
        }
    }
}

/// Wall-clock sleep of [`VERIFY_DELAY_MS`]. Wrapper exists so `main.rs::run`
/// reads as `verify::sleep_verify_delay()` instead of inlining the duration
/// — keeps the verify timing constant colocated with the rest of the
/// verify primitive.
///
/// Uses `std::thread::sleep` consistent with `click::click_at`'s
/// `CLICK_GAP_MS` precedent. Same TODOS P3 caveat applies (scheduler
/// pressure can drift `thread::sleep` beyond requested duration; v0.2's
/// continuous loop is where `mach_wait_until` migration matters).
pub fn sleep_verify_delay() {
    sleep(Duration::from_millis(VERIFY_DELAY_MS));
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Luma, RgbaImage};

    fn flat_luma(w: u32, h: u32, value: u8) -> GrayImage {
        GrayImage::from_pixel(w, h, Luma([value]))
    }

    /// Helper to write a Luma8 image to disk as RGBA PNG. The round-trip
    /// through `image::ImageReader::decode().to_luma8()` (which is what
    /// `load_haystack` does) preserves the luma values exactly because
    /// BT.601 weights sum to 1 and R=G=B.
    fn write_luma_as_rgba_png(img: &GrayImage, path: &Path) {
        let mut rgba = RgbaImage::new(img.width(), img.height());
        for (x, y, p) in img.enumerate_pixels() {
            let l = p[0];
            rgba.put_pixel(x, y, image::Rgba([l, l, l, 255]));
        }
        rgba.save(path)
            .expect("write tempfile PNG must succeed in test");
    }

    // ---------- pixel_diff ----------

    #[test]
    fn pixel_diff_returns_zero_for_identical_grayimages() {
        // Two flat-128 images. Every pixel matches → diff = 0.
        let a = flat_luma(50, 40, 128);
        let b = flat_luma(50, 40, 128);
        assert_eq!(pixel_diff(&a, &b), 0);
    }

    #[test]
    fn pixel_diff_counts_every_differing_pixel_for_fully_distinct_images() {
        // Flat-0 vs flat-255 over 100×80 = 8000 pixels, all differ.
        let a = flat_luma(100, 80, 0);
        let b = flat_luma(100, 80, 255);
        assert_eq!(pixel_diff(&a, &b), 8000);
    }

    #[test]
    fn pixel_diff_counts_scattered_differences() {
        // Construct two images that differ at exactly 3 pixels.
        let a = flat_luma(10, 10, 0);
        let mut b = a.clone();
        b.put_pixel(0, 0, Luma([255]));
        b.put_pixel(5, 5, Luma([100]));
        b.put_pixel(9, 9, Luma([1]));
        assert_eq!(pixel_diff(&a, &b), 3);
    }

    #[test]
    fn pixel_diff_returns_u64_max_on_dim_mismatch() {
        // Per docs: dim mismatch → u64::MAX sentinel (treat as definitely
        // changed). Caller's verdict_from_pixel_diff then passes
        // trivially.
        let a = flat_luma(50, 40, 128);
        let b = flat_luma(50, 41, 128); // height differs by 1
        assert_eq!(pixel_diff(&a, &b), u64::MAX);
    }

    #[test]
    fn pixel_diff_returns_u64_max_on_swapped_dims() {
        // 50x40 vs 40x50 — same pixel count, different dimensions.
        // Naive flat-buffer comparison without the dim guard would
        // silently produce a meaningful-looking diff count; the dim
        // guard rules it out.
        let a = flat_luma(50, 40, 128);
        let b = flat_luma(40, 50, 128);
        assert_eq!(pixel_diff(&a, &b), u64::MAX);
    }

    // ---------- verdict_from_pixel_diff ----------

    #[test]
    fn verdict_from_pixel_diff_passes_when_at_or_above_threshold() {
        // Strict `<` semantics: diff == threshold passes.
        assert!(verdict_from_pixel_diff(PIXEL_DIFF_REJECT_THRESHOLD).is_ok());
        assert!(verdict_from_pixel_diff(PIXEL_DIFF_REJECT_THRESHOLD + 1).is_ok());
        assert!(verdict_from_pixel_diff(u64::MAX).is_ok());
    }

    #[test]
    fn verdict_from_pixel_diff_fails_when_below_threshold() {
        // diff == threshold - 1 must fail.
        match verdict_from_pixel_diff(PIXEL_DIFF_REJECT_THRESHOLD - 1) {
            Err(BotError::ClickNotVerified { reason }) => {
                assert_eq!(reason, REASON_SCREEN_UNCHANGED);
            }
            other => panic!("expected ClickNotVerified{{screen_unchanged}}, got {other:?}"),
        }
    }

    #[test]
    fn verdict_from_pixel_diff_fails_at_zero() {
        // Identical captures (pixel_diff = 0) must fail — RoK didn't
        // react at all.
        match verdict_from_pixel_diff(0) {
            Err(BotError::ClickNotVerified { reason }) => {
                assert_eq!(reason, REASON_SCREEN_UNCHANGED);
            }
            other => {
                panic!("expected ClickNotVerified{{screen_unchanged}} at diff=0, got {other:?}")
            }
        }
    }

    // ---------- Constants pinning ----------

    #[test]
    fn verify_delay_ms_in_sane_range() {
        // The const is operator-facing tuning. Pin a sane range — 200ms
        // floor (faster than typical RoK UI transition) and 2000ms
        // ceiling (would blow v0.2 loop budget). Tighten when real-data
        // calibration lands per TODOS P3.
        assert!(
            (200..=2000).contains(&VERIFY_DELAY_MS),
            "VERIFY_DELAY_MS = {VERIFY_DELAY_MS} drifted out of [200, 2000] ms range; \
             update the test if the change is intentional and matches a calibration update"
        );
    }

    #[test]
    fn pixel_diff_reject_threshold_in_sane_range() {
        // 100-100000 pixel range. Below 100 the gate is essentially
        // "any change at all"; above 100000 (3% of a 2102×1640 capture)
        // ambient animation alone would pass. Real-data calibration will
        // narrow this band — TODOS P3.
        assert!(
            (100..=100_000).contains(&PIXEL_DIFF_REJECT_THRESHOLD),
            "PIXEL_DIFF_REJECT_THRESHOLD = {PIXEL_DIFF_REJECT_THRESHOLD} drifted out of \
             [100, 100000] range; tighten/loosen with calibration data, not by drift"
        );
    }

    #[test]
    fn reason_screen_unchanged_pinned_to_documented_value() {
        // Shell users + log parsers pattern-match against this string.
        // Changing it is a log contract change — pin so the change is
        // explicit.
        assert_eq!(REASON_SCREEN_UNCHANGED, "screen_unchanged");
    }

    #[test]
    fn click_not_verified_maps_to_exit_20() {
        // Defense-in-depth: confirm the reason constant routes through
        // BotError::ClickNotVerified to the documented exit code. The
        // error::tests pin this for the literal string; this test pins
        // it for the constant we actually emit at runtime.
        let err = BotError::ClickNotVerified {
            reason: REASON_SCREEN_UNCHANGED,
        };
        assert_eq!(err.exit_code(), 20);
    }

    // ---------- Integration: after_state via tempfile fixtures ----------

    fn dummy_match(capture_dims: (u32, u32)) -> Match {
        // pre_match isn't required for the verdict (verdict relies only
        // on pixel_diff), so fixture values just need to be sane for the
        // diagnostic log line and survive `Match`'s zero-dim invariants.
        Match {
            x: 10,
            y: 10,
            score: 0.99,
            capture_dims,
            needle_dims: (16, 16),
        }
    }

    #[test]
    fn after_state_returns_click_not_verified_when_captures_are_identical() {
        // Pre-capture and post-capture are byte-identical synthetic PNGs.
        // pixel_diff = 0, below the 1000 threshold → exit 20
        // screen_unchanged.
        let dir = tempfile::tempdir().expect("tempdir");
        let pre_path = dir.path().join("pre.png");
        let post_path = dir.path().join("post.png");

        // Use a structured image (not flat) so the diagnostic re-match
        // doesn't trip the matcher's variance guard. The exact content
        // doesn't matter for pixel_diff since both captures share it.
        let mut img = flat_luma(120, 100, 0);
        for x in 30..50 {
            for y in 30..50 {
                img.put_pixel(x, y, Luma([255]));
            }
        }
        write_luma_as_rgba_png(&img, &pre_path);
        write_luma_as_rgba_png(&img, &post_path);

        match after_state(&pre_path, &post_path, &dummy_match((120, 100))) {
            Err(BotError::ClickNotVerified { reason }) => {
                assert_eq!(reason, REASON_SCREEN_UNCHANGED);
            }
            other => panic!("expected ClickNotVerified on identical captures, got {other:?}"),
        }
    }

    #[test]
    fn after_state_passes_when_captures_differ_above_threshold() {
        // Post-capture differs from pre by ~2000 changed pixels — well
        // above the 1000 threshold. Verdict passes.
        let dir = tempfile::tempdir().expect("tempdir");
        let pre_path = dir.path().join("pre.png");
        let post_path = dir.path().join("post.png");

        let pre = flat_luma(120, 100, 0);
        let mut post = pre.clone();
        // Plant a 50×40 = 2000-pixel rectangle of 255 in the post,
        // unambiguously above the 1000-pixel threshold.
        for x in 10..60 {
            for y in 10..50 {
                post.put_pixel(x, y, Luma([255]));
            }
        }
        write_luma_as_rgba_png(&pre, &pre_path);
        write_luma_as_rgba_png(&post, &post_path);

        let result = after_state(&pre_path, &post_path, &dummy_match((120, 100)));
        assert!(
            result.is_ok(),
            "captures differing by ~2000 pixels must pass verify: {result:?}"
        );
    }

    #[test]
    fn after_state_returns_dim_mismatch_when_pre_post_dimensions_differ() {
        // Adversarial review of v0.1.4 caught a fail-open path: pixel_diff
        // returns u64::MAX on dim mismatch, and verdict_from_pixel_diff
        // passes on u64::MAX (sentinel > threshold). Without the explicit
        // dim check in after_state, dimension-changing capture-pipeline
        // drift (display reconfig, fullscreen toggle, screencapture
        // padding) silently passes verify. This test pins the fail-closed
        // path: write two synthetic PNGs at different dimensions and
        // assert the dim_mismatch reason tag surfaces.
        let dir = tempfile::tempdir().expect("tempdir");
        let pre_path = dir.path().join("pre.png");
        let post_path = dir.path().join("post.png");

        write_luma_as_rgba_png(&flat_luma(120, 100, 0), &pre_path);
        write_luma_as_rgba_png(&flat_luma(120, 99, 0), &post_path); // height differs

        match after_state(&pre_path, &post_path, &dummy_match((120, 100))) {
            Err(BotError::ClickNotVerified { reason }) => {
                assert_eq!(
                    reason, REASON_DIM_MISMATCH,
                    "dim mismatch must surface as dim_mismatch reason, not screen_unchanged"
                );
            }
            other => {
                panic!("expected ClickNotVerified{{dim_mismatch}} on dim mismatch, got {other:?}")
            }
        }
    }

    #[test]
    fn after_state_propagates_image_load_failed_when_pre_path_missing() {
        // Surfaces matcher::load_haystack's exit-16 contract on a missing
        // pre path. The post path doesn't matter — pre fails first.
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("does-not-exist.png");
        let post = dir.path().join("post.png");
        write_luma_as_rgba_png(&flat_luma(120, 100, 0), &post);

        match after_state(&missing, &post, &dummy_match((120, 100))) {
            Err(BotError::ImageLoadFailed { which }) => {
                assert_eq!(which, "haystack");
            }
            other => panic!("expected ImageLoadFailed{{haystack}} on missing pre, got {other:?}"),
        }
    }
}
