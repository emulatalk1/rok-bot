//! Post-click verification: confirm `click::click_at` toggled RoK's view.
//!
//! `CGEvent::post` returns `()` — once the synthetic mouse event is queued
//! the OS gives no signal about whether RoK consumed it. This module turns
//! "the click toggled the city↔world view" from an unverified assumption
//! into a checked invariant for the v0.2 continuous loop.
//!
//! ## The gate is a needle swap (design D11)
//!
//! v0.1.4 verified clicks with a pixel-diff between a pre-click and a
//! post-click capture. `/plan-eng-review` for v0.2 caught two fatal flaws
//! in that approach for a continuous loop:
//!
//! - Pixel-diff trivially passes on ANY view change. It cannot tell "the
//!   toggle fired" from "the screen happened to move."
//! - Worse, it FALSE-PASSES a missed click during RoK's ambient animation
//!   (water shimmer, marching troops, weather) — the screen changed, so
//!   the gate says "verified" even though nothing was clicked.
//!
//! v0.2 replaces it with **needle-swap verification**. The bot carries two
//! needles (city-view art = index 0, world-view art = index 1). A
//! confirmed toggle is: the post-click capture, re-matched in the toggle
//! ROI, matches a DIFFERENT needle than the pre-click capture did. Ambient
//! animation cannot fake that — only an actual city↔world switch changes
//! which needle wins. The pixel-diff path (`after_state`, `pixel_diff`,
//! `verdict_from_pixel_diff`, `PIXEL_DIFF_REJECT_THRESHOLD`) was deleted
//! outright, not left as a shim.
//!
//! ## Failure tags
//!
//! [`confirm_needle_swap`] returns `BotError::ClickNotVerified` with:
//!
//! - [`REASON_NO_SWAP`] — the post-click re-match found the SAME needle.
//!   The view did not toggle: the click missed, landed on a dead pixel,
//!   or RoK was mid-loading-screen.
//! - [`REASON_NEITHER_NEEDLE`] — the post-click re-match found NEITHER
//!   needle above `matcher::MATCH_THRESHOLD`. Usually a mid-transition
//!   frame (the verify delay landed inside the city↔world cross-fade).
//!   Treated as an unconfirmed swap — a failed tick the loop retries.
//!
//! Both are transient in the loop's error policy (design D5): one is
//! noise, `run_loop::LOOP_FAILURE_BUDGET` in a row aborts the loop.

use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use crate::error::{BotError, Result};
use crate::matcher::{NeedleMatch, find_best_needle, last_position_roi};

/// Wall-clock sleep between `click::click_at` returning and the post-click
/// capture. Long enough to let RoK render the toggled view (button-press
/// flash 50-100ms, view cross-fade 200-400ms), short enough that the
/// continuous loop stays responsive.
///
/// Needle-swap verify needs the post-capture to land AFTER the view has
/// settled — too short and the post-capture catches a mid-transition
/// frame that matches neither needle (surfaces as [`REASON_NEITHER_NEEDLE`],
/// a transient failure the loop retries). 500ms gives ~2× margin over the
/// typical transition. Empirical calibration is a TODOS P3 item.
pub const VERIFY_DELAY_MS: u64 = 500;

/// `confirm_needle_swap` reason tags, surfaced via
/// `BotError::ClickNotVerified { reason }`. Pinned as `&'static str` so
/// operator-facing log lines and shell pattern-matchers see one of these
/// exact values.
///
/// `REASON_NO_SWAP` — the post-click capture re-matched the SAME needle
/// that matched pre-click; the view did not toggle.
///
/// `REASON_NEITHER_NEEDLE` — the post-click capture matched neither
/// needle; likely a mid-transition frame. Per design D11 an unconfirmed
/// swap is a failed tick, so this is a failure, not a pass.
pub const REASON_NO_SWAP: &str = "no_swap";
pub const REASON_NEITHER_NEEDLE: &str = "neither_needle";

/// Confirm a click toggled the city↔world view by re-matching the post-
/// click capture against both needles in the toggle ROI (design D11).
///
/// `pre` is the pre-click [`NeedleMatch`] — its `needle_idx` is the view
/// the bot saw before clicking, and its `.m` position seeds the search
/// ROI (the toggle is a fixed on-screen button, so a tight
/// [`last_position_roi`] around the pre-click position covers it).
///
/// Outcomes:
/// - post-click best match has a DIFFERENT `needle_idx` → `Ok(())`, the
///   view toggled, the click is confirmed.
/// - post-click best match has the SAME `needle_idx` →
///   `Err(ClickNotVerified { REASON_NO_SWAP })`.
/// - no needle matched the post-click capture →
///   `Err(ClickNotVerified { REASON_NEITHER_NEEDLE })`.
/// - the matcher itself failed (post capture missing/corrupt, oversized
///   needle) → that `BotError` propagates unchanged.
pub fn confirm_needle_swap(post_path: &Path, needles: &[&[u8]], pre: NeedleMatch) -> Result<()> {
    let pre_idx = pre.needle_idx;
    let best = find_best_needle(post_path, needles, move |w, h| {
        Some(last_position_roi(pre.m, w, h))
    })?;

    match best {
        Some(nm) if nm.needle_idx != pre_idx => {
            tracing::info!(
                target: "rok_bot",
                pre_needle_idx = pre_idx,
                post_needle_idx = nm.needle_idx,
                post_score = nm.m.score,
                "needle-swap verify passed: view toggled"
            );
            Ok(())
        }
        Some(nm) => {
            tracing::warn!(
                target: "rok_bot",
                pre_needle_idx = pre_idx,
                post_needle_idx = nm.needle_idx,
                post_score = nm.m.score,
                "needle-swap verify failed: post-click capture matched the same \
                 needle — the click did not toggle the view"
            );
            Err(BotError::ClickNotVerified {
                reason: REASON_NO_SWAP,
            })
        }
        None => {
            tracing::warn!(
                target: "rok_bot",
                pre_needle_idx = pre_idx,
                "needle-swap verify failed: post-click capture matched neither \
                 needle — likely a mid-transition frame; the loop retries"
            );
            Err(BotError::ClickNotVerified {
                reason: REASON_NEITHER_NEEDLE,
            })
        }
    }
}

/// Wall-clock sleep of [`VERIFY_DELAY_MS`]. Wrapper exists so `run_loop`
/// reads as `verify::sleep_verify_delay()` instead of inlining the
/// duration — keeps the verify timing constant colocated with the rest
/// of the verify primitive.
pub fn sleep_verify_delay() {
    sleep(Duration::from_millis(VERIFY_DELAY_MS));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matcher::Match;
    use image::{GrayImage, Luma};

    /// 40×40 needle with a bright square at `(x0..x1, y0..y1)`, dark
    /// elsewhere. Strong low-frequency structure that does NOT correlate
    /// with noise — so a no-match haystack stays a no-match. Top-left
    /// pixel is dark, so the placeholder-sentinel gate never fires.
    fn square_needle(x0: u32, x1: u32, y0: u32, y1: u32) -> GrayImage {
        GrayImage::from_fn(40, 40, |x, y| {
            if (x0..x1).contains(&x) && (y0..y1).contains(&y) {
                Luma([255])
            } else {
                Luma([0])
            }
        })
    }

    /// Deterministic xorshift noise — a haystack that matches no needle.
    fn noise(w: u32, h: u32, seed: u32) -> GrayImage {
        let mut s = seed;
        GrayImage::from_fn(w, h, |_, _| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            Luma([u8::try_from(s & 0xFF).unwrap_or(0)])
        })
    }

    /// Overwrite a `needle`-sized rectangle of `haystack` at `(px, py)`
    /// with the needle's exact pixels — NCC there is then 1.0.
    fn plant(haystack: &mut GrayImage, needle: &GrayImage, px: u32, py: u32) {
        for ny in 0..needle.height() {
            for nx in 0..needle.width() {
                let dst_x = px.saturating_add(nx);
                let dst_y = py.saturating_add(ny);
                haystack.put_pixel(dst_x, dst_y, *needle.get_pixel(nx, ny));
            }
        }
    }

    /// Encode a luma image as RGBA PNG bytes (R=G=B=L, A=255). The
    /// round-trip through `to_luma8()` preserves the luma values.
    fn encode_png(img: &GrayImage) -> Vec<u8> {
        let mut rgba = image::RgbaImage::new(img.width(), img.height());
        for (x, y, p) in img.enumerate_pixels() {
            let l = p[0];
            rgba.put_pixel(x, y, image::Rgba([l, l, l, 255]));
        }
        let mut cursor = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(rgba)
            .write_to(&mut cursor, image::ImageFormat::Png)
            .expect("encode test PNG must succeed");
        cursor.into_inner()
    }

    /// Write a luma image to `path` as an RGBA PNG.
    fn write_png(img: &GrayImage, path: &Path) {
        let mut rgba = image::RgbaImage::new(img.width(), img.height());
        for (x, y, p) in img.enumerate_pixels() {
            let l = p[0];
            rgba.put_pixel(x, y, image::Rgba([l, l, l, 255]));
        }
        rgba.save(path).expect("write tempfile PNG must succeed");
    }

    /// A pre-click `NeedleMatch`: needle `idx` matched at `(200, 200)`
    /// with 40×40 dims in a 400×400 capture. `confirm_needle_swap`
    /// builds the verify ROI from `.m` via `last_position_roi`, so the
    /// post-capture's planted needle must sit near `(200, 200)`.
    fn pre_match(idx: usize) -> NeedleMatch {
        NeedleMatch {
            needle_idx: idx,
            m: Match {
                x: 200,
                y: 200,
                score: 0.99,
                capture_dims: (400, 400),
                needle_dims: (40, 40),
            },
        }
    }

    // ---------- VERIFY_DELAY_MS constant ----------

    #[test]
    fn verify_delay_ms_in_sane_range() {
        // 200ms floor (faster than a typical RoK view transition) and
        // 2000ms ceiling (would blow the v0.2 loop cadence budget).
        assert!(
            (200..=2000).contains(&VERIFY_DELAY_MS),
            "VERIFY_DELAY_MS = {VERIFY_DELAY_MS} drifted out of [200, 2000]ms"
        );
    }

    #[test]
    fn reason_constants_pinned_to_documented_values() {
        // Shell users + log parsers pattern-match against these strings;
        // error::tests also pins them. A rename is a contract change.
        assert_eq!(REASON_NO_SWAP, "no_swap");
        assert_eq!(REASON_NEITHER_NEEDLE, "neither_needle");
    }

    // ---------- confirm_needle_swap ----------

    #[test]
    fn confirm_needle_swap_passes_when_post_matches_a_different_needle() {
        // Pre-click view = needle 0. Post-click capture contains needle
        // 1 (planted exactly at the toggle position). A different needle
        // matched → the view toggled → Ok.
        let dir = tempfile::tempdir().expect("tempdir");
        let post_path = dir.path().join("post.png");

        let needle0 = square_needle(8, 32, 8, 32);
        let needle1 = square_needle(4, 20, 20, 36);
        let mut post = noise(400, 400, 0x0BAD_F00D);
        plant(&mut post, &needle1, 200, 200);
        write_png(&post, &post_path);

        let n0 = encode_png(&needle0);
        let n1 = encode_png(&needle1);
        let slice: [&[u8]; 2] = [&n0, &n1];
        let result = confirm_needle_swap(&post_path, &slice, pre_match(0));
        assert!(
            result.is_ok(),
            "post-click capture matched needle 1 (pre was 0) — swap confirmed: {result:?}"
        );
    }

    #[test]
    fn confirm_needle_swap_fails_no_swap_when_post_matches_same_needle() {
        // Pre-click view = needle 0. Post-click capture STILL contains
        // needle 0 — the click did not toggle the view → no_swap.
        let dir = tempfile::tempdir().expect("tempdir");
        let post_path = dir.path().join("post.png");

        let needle0 = square_needle(8, 32, 8, 32);
        let needle1 = square_needle(4, 20, 20, 36);
        let mut post = noise(400, 400, 0x1234_5678);
        plant(&mut post, &needle0, 200, 200);
        write_png(&post, &post_path);

        let n0 = encode_png(&needle0);
        let n1 = encode_png(&needle1);
        let slice: [&[u8]; 2] = [&n0, &n1];
        match confirm_needle_swap(&post_path, &slice, pre_match(0)) {
            Err(BotError::ClickNotVerified { reason }) => {
                assert_eq!(reason, REASON_NO_SWAP);
            }
            other => panic!("expected ClickNotVerified{{no_swap}}, got {other:?}"),
        }
    }

    #[test]
    fn confirm_needle_swap_fails_neither_needle_when_post_matches_nothing() {
        // Post-click capture is pure noise — neither needle clears the
        // threshold. Per D11 an unconfirmed swap is a failed tick.
        let dir = tempfile::tempdir().expect("tempdir");
        let post_path = dir.path().join("post.png");

        let needle0 = square_needle(8, 32, 8, 32);
        let needle1 = square_needle(4, 20, 20, 36);
        let post = noise(400, 400, 0xDEAD_BEEF);
        write_png(&post, &post_path);

        let n0 = encode_png(&needle0);
        let n1 = encode_png(&needle1);
        let slice: [&[u8]; 2] = [&n0, &n1];
        match confirm_needle_swap(&post_path, &slice, pre_match(0)) {
            Err(BotError::ClickNotVerified { reason }) => {
                assert_eq!(reason, REASON_NEITHER_NEEDLE);
            }
            other => panic!("expected ClickNotVerified{{neither_needle}}, got {other:?}"),
        }
    }

    #[test]
    fn confirm_needle_swap_propagates_image_load_failed_on_missing_post() {
        // The post capture file doesn't exist — the matcher's exit-16
        // ImageLoadFailed must propagate unchanged, not be swallowed
        // into a ClickNotVerified.
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("does-not-exist.png");

        let needle0 = square_needle(8, 32, 8, 32);
        let needle1 = square_needle(4, 20, 20, 36);
        let n0 = encode_png(&needle0);
        let n1 = encode_png(&needle1);
        let slice: [&[u8]; 2] = [&n0, &n1];
        match confirm_needle_swap(&missing, &slice, pre_match(0)) {
            Err(BotError::ImageLoadFailed { which }) => {
                assert_eq!(which, "haystack");
            }
            other => panic!("expected ImageLoadFailed{{haystack}}, got {other:?}"),
        }
    }

    #[test]
    fn confirm_needle_swap_maps_failures_to_exit_20() {
        // Defense-in-depth: both reason tags route through
        // BotError::ClickNotVerified to the documented exit code 20.
        for reason in [REASON_NO_SWAP, REASON_NEITHER_NEEDLE] {
            let err = BotError::ClickNotVerified { reason };
            assert_eq!(err.exit_code(), 20);
        }
    }

    #[test]
    fn confirm_needle_swap_handles_pre_match_near_corner() {
        // Pre-click match flush against the top-left corner — the toggle
        // button lives in the bottom-left castle quadrant, so a clamped
        // verify ROI is the realistic case. last_position_roi clamps the
        // ROI origin to (0,0); the swap must still confirm.
        let dir = tempfile::tempdir().expect("tempdir");
        let post_path = dir.path().join("post.png");
        let needle0 = square_needle(8, 32, 8, 32);
        let needle1 = square_needle(4, 20, 20, 36);
        let mut post = noise(400, 400, 0xFACE_0001);
        plant(&mut post, &needle1, 0, 0);
        write_png(&post, &post_path);
        let n0 = encode_png(&needle0);
        let n1 = encode_png(&needle1);
        let slice: [&[u8]; 2] = [&n0, &n1];
        let pre = NeedleMatch {
            needle_idx: 0,
            m: Match {
                x: 0,
                y: 0,
                score: 0.99,
                capture_dims: (400, 400),
                needle_dims: (40, 40),
            },
        };
        assert!(
            confirm_needle_swap(&post_path, &slice, pre).is_ok(),
            "a corner pre-match (clamped verify ROI) must still confirm a swap",
        );
    }
}
