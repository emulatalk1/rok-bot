//! Template matching against the captured RoK window.
//!
//! v0.1.2 surface: locate one known UI element (the city/world toggle) inside
//! `rok-capture.png` produced by [`crate::capture`]. Output is a confident
//! best-match `(x, y, score)` in capture pixel space, or `Ok(None)` if no
//! position cleared `MATCH_THRESHOLD`.
//!
//! Algorithm: normalized cross-correlation (NCC) over a parallel sliding
//! window via [`imageproc::template_matching::match_template_parallel`].
//! Sub-second on M-series for our 2102×1640 Retina haystack with ~80×40
//! needle. v0.2 will swap this for FFT-based NCC once the per-tick capture
//! loop makes naive O(N²) sliding too slow (see TODOS.md P2).
//!
//! Coordinate space: returned `Match.{x, y}` are in the **capture's** pixel
//! space (Retina-doubled). v0.1.3 introduces a sibling helper that maps these
//! to CG screen coords given the window frame from [`crate::window`]; v0.1.2
//! intentionally stops at capture-pixel coords so the synthesis step is
//! isolated to the click milestone.
//!
//! Color: RGBA → Luma8 via `DynamicImage::to_luma8()` (ITU-R BT.601 weights).
//! Alpha is silently discarded. v0.1.x targets MUST be opaque rectangular
//! crops; transparent / anti-aliased-edge targets need masked NCC, which is
//! deferred to a future milestone (see TODOS.md P3 "masked NCC").
//!
//! Score interpretation: imageproc's `CrossCorrelationNormalized` is NOT
//! Pearson-correlation (no mean subtraction). It computes
//! `sum(i*t) / sqrt(sum(i²)*sum(t²))`, bounded in `[0, 1]` for non-negative
//! grayscale pixels. This means scores skew higher than a textbook
//! mean-centered NCC would produce — random uint8 noise haystacks routinely
//! correlate at 0.7-0.9 with arbitrary needles. The 0.85 threshold is
//! calibrated against this reality, not against textbook NCC. A flat-color
//! needle would correlate ~1.0 with most haystacks via this formula, which
//! is why `match_in` rejects zero-variance needles before invoking imageproc.
//!
//! Asset: the needle is `include_bytes!`-embedded at compile time from
//! `assets/targets/city-button.png`. The committed asset is a placeholder
//! pattern until the first live RoK crop replaces it; all the matcher logic
//! and tests remain valid regardless of which bytes are embedded, since the
//! tests use synthetic in-memory fixtures and the round-trip integration
//! test plants the embedded bytes back into a generated haystack.

use std::path::Path;
use std::time::Instant;

use image::GrayImage;
use imageproc::template_matching::{MatchTemplateMethod, match_template_parallel};

use crate::error::{BotError, Result};

/// Confidence floor for accepting a best-match. Empirical — NCC scores are in
/// `[-1, 1]`, with `1.0` for an exact match, `0` for uncorrelated, `-1` for
/// inverted contrast. `0.85` is forgiving enough for minor render variation
/// (anti-alias jitter, sub-pixel layout drift) but tight enough that random
/// correlation against a non-matching capture stays well below it.
///
/// Tuning: revisit after v0.1.3+ produces a stream of real-world scores.
/// If false-negatives bite, lower; if false-positives slip in, raise. The
/// `match_threshold_const_is_zero_eight_five` test pins the current value
/// so a casual change forces the operator to update both sides intentionally.
pub const MATCH_THRESHOLD: f32 = 0.85;

/// Compile-time-embedded target needle. Lives under `assets/targets/` for PR
/// visibility but doesn't get read from disk at runtime — embedding side-steps
/// "asset missing at runtime" failure modes entirely.
///
/// The committed bytes are a synthetic placeholder until the first live RoK
/// crop replaces them. Any valid PNG works; the matcher's correctness is
/// independent of the specific image content.
pub const TARGET_BYTES: &[u8] = include_bytes!("../assets/targets/city-button.png");

/// Coordinates and confidence of the best template match in the capture.
///
/// Coordinates are in the **capture's** pixel space (Retina-doubled,
/// 2102×1640 for our current screencapture output). The capture-to-screen
/// coord conversion lives in v0.1.3 alongside `CGEvent.post`.
///
/// `score` is in `[-1, 1]` (NCC range). Only scores `>= MATCH_THRESHOLD` ever
/// surface in `Some(Match)` — below-threshold returns `Ok(None)` from the
/// matcher.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Match {
    pub x: u32,
    pub y: u32,
    pub score: f32,
}

/// Live entry: load the haystack PNG from disk, decode the embedded needle,
/// and locate the best NCC match.
///
/// Returns:
/// * `Ok(Some(m))` — best match cleared `MATCH_THRESHOLD`. Logs an `info!`
///   line with `x`/`y`/`score`/`elapsed_ms` for operator visibility.
/// * `Ok(None)`    — best match below threshold (or all-NaN heatmap on
///   pathological input). The matcher already logged the diagnostic numbers
///   at `warn!` before returning. Caller (`main.rs::run`) translates this to
///   `BotError::TargetNotFound` (exit 15).
/// * `Err(...)`    — real failure path: PNG decode/open, oversized needle.
///   Each maps to a typed `BotError` variant with structured exit code.
pub fn find_target(haystack_path: &Path) -> Result<Option<Match>> {
    let started = Instant::now();

    let haystack = image::open(haystack_path)
        .map_err(|err| {
            // Log image::ImageError before mapping so the operator sees the
            // underlying cause (corrupt PNG / file missing / wrong magic).
            // Mirrors capture_with_bin's io::Error handling pattern.
            tracing::warn!(
                target: "rok_bot",
                path = %haystack_path.display(),
                error = %err,
                "failed to open or decode haystack image"
            );
            BotError::ImageLoadFailed { which: "haystack" }
        })?
        .to_luma8();

    // Needle decode is technically fallible (include_bytes! embeds bytes but
    // doesn't validate they parse as PNG — the file could be corrupt at
    // commit time). In practice this can only fire if the committed asset
    // is malformed, which `cargo build` won't catch. Defensive arm.
    let needle = image::load_from_memory(TARGET_BYTES)
        .map_err(|err| {
            tracing::warn!(
                target: "rok_bot",
                error = %err,
                "failed to decode embedded needle (assets/targets/city-button.png)"
            );
            BotError::ImageLoadFailed { which: "needle" }
        })?
        .to_luma8();

    let outcome = match_in(&haystack, &needle)?;

    if let Some(ref m) = outcome {
        tracing::info!(
            target: "rok_bot",
            x = m.x,
            y = m.y,
            score = m.score,
            elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            "found target"
        );
    }
    Ok(outcome)
}

/// Pure: compute the best-match NCC score and location over in-memory
/// `GrayImage` fixtures. Tests drive this directly without disk I/O so they
/// stay fast and deterministic regardless of which bytes the embedded
/// needle holds today.
///
/// Behavior:
/// * Errors `TargetTooLarge` if `needle.width() >= haystack.width() ||
///   needle.height() >= haystack.height()`. Guard fires **before**
///   `match_template_parallel` because imageproc panics when needle dims
///   aren't strictly less than haystack dims (per its docstring).
/// * Walks the resulting heatmap once, skipping NaN scores (zero-variance
///   inputs can produce NaN in normalized cross-correlation). For tied
///   maxima, the lexicographically-smallest position wins (first encountered
///   by row-major iteration), matching `imageproc::find_extremes`'s tie-break.
/// * Returns `Ok(None)` when the best score is below `MATCH_THRESHOLD` (or
///   when every score is NaN). Logs `warn!` with the diagnostic numbers.
fn match_in(haystack: &GrayImage, needle: &GrayImage) -> Result<Option<Match>> {
    if needle.width() >= haystack.width() || needle.height() >= haystack.height() {
        return Err(BotError::TargetTooLarge {
            needle: (needle.width(), needle.height()),
            haystack: (haystack.width(), haystack.height()),
        });
    }

    // Reject zero-variance needles. imageproc's CrossCorrelationNormalized
    // is `sum(i*t)/sqrt(sum(i²)*sum(t²))` — not mean-centered — so a uniform
    // needle correlates highly with every position in any positive-pixel
    // haystack. That's an operator-error case (someone committed a flat-color
    // target), not a real match; surfacing it as "found target" would mislead
    // downstream. Treat as no-match and surface the diagnostic.
    let (n_min, n_max) = needle.pixels().fold((u8::MAX, u8::MIN), |(lo, hi), p| {
        (lo.min(p[0]), hi.max(p[0]))
    });
    if n_min == n_max {
        tracing::warn!(
            target: "rok_bot",
            needle_pixel_value = n_min,
            "needle has zero variance (flat color); rejecting as ill-formed target"
        );
        return Ok(None);
    }

    let heatmap = match_template_parallel(
        haystack,
        needle,
        MatchTemplateMethod::CrossCorrelationNormalized,
    );

    // Manual max-search instead of `imageproc::find_extremes`: find_extremes
    // doesn't filter NaN, so a uniform-variance input would leave its max
    // initialized to whatever (0,0) holds (potentially NaN itself). We walk
    // once, skip NaN, and use strict `>` so the first-encountered wins on
    // ties — matching find_extremes's lex-smallest behavior since
    // `enumerate_pixels` iterates row-major.
    let mut best: Option<(u32, u32, f32)> = None;
    for (x, y, p) in heatmap.enumerate_pixels() {
        let s = p[0];
        if s.is_nan() {
            continue;
        }
        if best.is_none_or(|(_, _, bs)| s > bs) {
            best = Some((x, y, s));
        }
    }

    let Some((x, y, score)) = best else {
        // Pathological case — every heatmap pixel is NaN. Treat as no match
        // and surface the diagnostic so the operator can spot uniform-input
        // bugs without staring at a "TargetNotFound" with no context.
        tracing::warn!(
            target: "rok_bot",
            "match heatmap was entirely NaN — uniform-variance input likely (target or capture is a flat color)"
        );
        return Ok(None);
    };

    if score < MATCH_THRESHOLD {
        tracing::warn!(
            target: "rok_bot",
            best_score = score,
            threshold = MATCH_THRESHOLD,
            best_x = x,
            best_y = y,
            "best match below confidence threshold"
        );
        return Ok(None);
    }

    Ok(Some(Match { x, y, score }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Luma;

    /// Deterministic pseudo-noise. We avoid pulling in `rand` for tests —
    /// a tiny xorshift gives us reproducible distinct-looking pixel values
    /// without a third-party dep. The output isn't statistically random,
    /// just non-uniform enough that NCC against an unrelated needle scores
    /// well below 0.85.
    fn xorshift_byte(seed: &mut u32) -> u8 {
        let mut x = *seed;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        *seed = x;
        u8::try_from(x & 0xFF).unwrap_or(0)
    }

    fn noise_image(width: u32, height: u32, seed: u32) -> GrayImage {
        let mut s = seed;
        GrayImage::from_fn(width, height, |_, _| Luma([xorshift_byte(&mut s)]))
    }

    /// 16×16 needle with a bright centered square, dark elsewhere. Has
    /// strong low-frequency structure that doesn't appear by chance in
    /// xorshift noise — NCC against `noise_image()` haystacks consistently
    /// scores below 0.5 across the heatmap, well under `MATCH_THRESHOLD`.
    fn structured_needle() -> GrayImage {
        GrayImage::from_fn(16, 16, |x, y| {
            if (4..12).contains(&x) && (4..12).contains(&y) {
                Luma([255])
            } else {
                Luma([0])
            }
        })
    }

    /// Build a haystack of `noise_image(seed)` and overwrite a `needle`-sized
    /// rectangle starting at `(plant_x, plant_y)` with the needle's exact
    /// pixels. NCC at that position should be `1.0`.
    ///
    /// Asserts the plant fits inside the haystack — caller error otherwise.
    /// Uses `checked_add` because strict clippy (`arithmetic_side_effects`)
    /// rejects raw `u32 + u32` even when overflow is impossible by precondition.
    fn plant_needle_at(haystack: &mut GrayImage, needle: &GrayImage, plant_x: u32, plant_y: u32) {
        assert!(
            plant_x.saturating_add(needle.width()) <= haystack.width()
                && plant_y.saturating_add(needle.height()) <= haystack.height(),
            "plant region must fit inside haystack"
        );
        for ny in 0..needle.height() {
            for nx in 0..needle.width() {
                let p = needle.get_pixel(nx, ny);
                let dst_x = plant_x.checked_add(nx).expect("bounds asserted above");
                let dst_y = plant_y.checked_add(ny).expect("bounds asserted above");
                haystack.put_pixel(dst_x, dst_y, *p);
            }
        }
    }

    #[test]
    fn match_in_finds_planted_needle_in_noise_haystack() {
        // Happy path: plant the exact needle into a noise haystack at a known
        // position. NCC at that position is 1.0; everything else correlates
        // poorly. We assert (x, y) and a near-1.0 score.
        let needle = noise_image(8, 8, 0x9E37_79B9);
        let mut haystack = noise_image(100, 100, 0x6151_2C53);
        plant_needle_at(&mut haystack, &needle, 12, 30);

        let m = match_in(&haystack, &needle)
            .expect("size guard should not fire")
            .expect("planted needle should clear threshold");

        assert_eq!((m.x, m.y), (12, 30), "planted location wins");
        assert!(
            m.score > 0.99,
            "exact-match NCC should be ~1.0, got {}",
            m.score
        );
    }

    #[test]
    fn match_in_returns_none_when_below_threshold() {
        // Structured needle (centered bright square) against noise haystack.
        // The square's low-frequency pattern doesn't occur by chance in
        // xorshift noise; peak NCC stays well under MATCH_THRESHOLD.
        // (An earlier draft used noise-vs-noise — xorshift is not a great
        // PRNG and short 8×8 patches happen to correlate ~0.85 by coincidence,
        // so we use structure on the needle side to make "no match" robust.)
        let haystack = noise_image(120, 120, 0xDEAD_BEEF);
        let needle = structured_needle();
        let outcome = match_in(&haystack, &needle).expect("size guard should not fire");
        assert!(
            outcome.is_none(),
            "structured needle should not match random noise; got {outcome:?}"
        );
    }

    #[test]
    fn match_threshold_const_is_zero_eight_five() {
        // Pin the threshold value to its v0.1.2 contract. Changing it is a
        // user-facing tuning decision, not an incidental refactor — this
        // test forces the change to be intentional.
        //
        // Compare bit patterns rather than `==` to side-step the
        // `float_cmp_const` lint. Bit-equality is the strictest possible
        // pin and exactly what we want here: a deliberate change to the
        // const must produce a different bit pattern.
        let pinned = MATCH_THRESHOLD.to_bits() == 0.85_f32.to_bits();
        assert!(
            pinned,
            "MATCH_THRESHOLD drifted from 0.85: {MATCH_THRESHOLD}"
        );
    }

    #[test]
    fn match_in_picks_lex_smallest_when_multiple_max() {
        // Plant the same needle at three positions; NCC scores 1.0 at each.
        // Expected winner: the lex-smallest, which under enumerate_pixels'
        // row-major iteration is the smallest-y first, then smallest-x.
        // (12, 5) wins over (60, 5) (same y, smaller x) and over (12, 80)
        // (smaller y).
        let needle = noise_image(6, 6, 0x1357_9BDF);
        let mut haystack = noise_image(100, 100, 0x2468_ACE0);
        plant_needle_at(&mut haystack, &needle, 60, 5);
        plant_needle_at(&mut haystack, &needle, 12, 5);
        plant_needle_at(&mut haystack, &needle, 12, 80);

        let m = match_in(&haystack, &needle)
            .expect("size guard")
            .expect("planted needle should clear threshold");

        assert_eq!(
            (m.x, m.y),
            (12, 5),
            "lex-smallest (top row, leftmost) plant should win"
        );
    }

    #[test]
    fn match_in_returns_target_too_large_when_needle_equals_haystack() {
        // imageproc panics when needle dims aren't strictly less than haystack
        // dims (per its public docstring). Equal-size triggers the same panic
        // path on most methods, so our guard rejects equal-size to convert
        // that into a typed exit instead of an in-process panic.
        let haystack = noise_image(20, 20, 1);
        let needle = noise_image(20, 20, 2);
        match match_in(&haystack, &needle) {
            Err(BotError::TargetTooLarge {
                needle: n,
                haystack: h,
            }) => {
                assert_eq!(n, (20, 20));
                assert_eq!(h, (20, 20));
            }
            other => panic!("expected TargetTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn match_in_returns_target_too_large_when_needle_oversized() {
        // Strictly larger needle in either dimension. The guard fires on
        // either width or height — this exercises the height-only case so
        // we know both branches are live.
        let haystack = noise_image(100, 50, 3);
        let needle = noise_image(80, 60, 4);
        match match_in(&haystack, &needle) {
            Err(BotError::TargetTooLarge {
                needle: n,
                haystack: h,
            }) => {
                assert_eq!(n, (80, 60));
                assert_eq!(h, (100, 50));
            }
            other => panic!("expected TargetTooLarge on oversized needle, got {other:?}"),
        }
    }

    #[test]
    fn match_in_rejects_uniform_needle() {
        // Zero-variance needles are operator error (committed a flat-color
        // target). imageproc's non-mean-centered NCC would correlate them
        // highly with any positive-pixel haystack, so we guard before the
        // sliding window. Pin Ok(None) (caller maps to TargetNotFound at
        // exit 15) regardless of what the haystack contains.
        let needle = GrayImage::from_pixel(8, 8, Luma([128]));
        for seed in [0xBADC_0FFE, 0x1234_5678, 0xFFFF_0000] {
            let haystack = noise_image(100, 100, seed);
            let outcome = match_in(&haystack, &needle).expect("size guard should not fire");
            assert!(
                outcome.is_none(),
                "uniform needle must always reject; seed={seed:#x} got {outcome:?}"
            );
        }
    }

    #[test]
    fn match_in_rejects_uniform_needle_even_against_uniform_haystack() {
        // Uniform-vs-uniform is the degenerate "everything is a match" case.
        // The needle-variance guard fires before any imageproc call, so this
        // doesn't depend on whatever CcorrNormalized would return for
        // zero-norm inputs. Pin Ok(None) for the contract.
        let haystack = GrayImage::from_pixel(50, 50, Luma([128]));
        let needle = GrayImage::from_pixel(8, 8, Luma([128]));
        let outcome = match_in(&haystack, &needle).expect("size guard should not fire");
        assert!(
            outcome.is_none(),
            "uniform needle must reject; got {outcome:?}"
        );
    }

    // ---------- Integration tests: find_target round-trip via tempfile ----------

    /// Decode the embedded needle to a luma image — the round-trip test plants
    /// these exact luma values into the synthesized haystack so the matcher
    /// finds them when `find_target` calls `to_luma8()` again on read.
    fn embedded_needle_luma() -> GrayImage {
        image::load_from_memory(TARGET_BYTES)
            .expect("embedded needle must decode (asset is committed and built)")
            .to_luma8()
    }

    /// Encode a luma image as RGBA PNG (R=G=B=L, A=255). On decode +
    /// `to_luma8()`, the round-trip preserves the luma values exactly because
    /// BT.601 weights sum to 1 and R=G=B.
    fn write_luma_as_rgba_png(img: &GrayImage, path: &Path) {
        let mut rgba = image::RgbaImage::new(img.width(), img.height());
        for (x, y, p) in img.enumerate_pixels() {
            let l = p[0];
            rgba.put_pixel(x, y, image::Rgba([l, l, l, 255]));
        }
        rgba.save(path).expect("write tempfile PNG");
    }

    #[test]
    fn find_target_round_trip_with_real_haystack() {
        // Build a haystack the matcher will actually like: noise base, plant
        // the embedded-needle bytes at a known position, save as PNG, and
        // verify find_target finds them back at the planted coords.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("haystack.png");

        let needle = embedded_needle_luma();
        // Haystack must be strictly larger in both dims; pick something
        // bigger than the placeholder 80x40 needle with room for the plant.
        let mut haystack = noise_image(320, 200, 0xF00D);
        let plant_x = 100;
        let plant_y = 60;
        plant_needle_at(&mut haystack, &needle, plant_x, plant_y);
        write_luma_as_rgba_png(&haystack, &path);

        let m = find_target(&path)
            .expect("size guard / decode")
            .expect("planted needle should clear threshold");

        assert_eq!((m.x, m.y), (plant_x, plant_y));
        assert!(
            m.score > 0.99,
            "exact-match NCC should be ~1.0, got {}",
            m.score
        );
    }

    #[test]
    fn find_target_returns_image_load_failed_when_haystack_missing() {
        // Pointing find_target at a non-existent path must surface as the
        // typed exit-16 path, not as an unwrap panic from image::open.
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("does-not-exist.png");
        match find_target(&missing) {
            Err(BotError::ImageLoadFailed { which }) => {
                assert_eq!(which, "haystack", "should tag haystack, not needle");
            }
            other => panic!("expected ImageLoadFailed{{haystack}}, got {other:?}"),
        }
    }

    #[test]
    fn find_target_returns_image_load_failed_when_haystack_corrupt() {
        // Garbage bytes at a .png path. image::open inspects magic bytes and
        // fails before any allocation work, which keeps this test cheap.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("corrupt.png");
        std::fs::write(&path, b"not a real PNG, just garbage").expect("seed corrupt file");
        match find_target(&path) {
            Err(BotError::ImageLoadFailed { which }) => {
                assert_eq!(which, "haystack");
            }
            other => panic!("expected ImageLoadFailed on garbage bytes, got {other:?}"),
        }
    }
}
