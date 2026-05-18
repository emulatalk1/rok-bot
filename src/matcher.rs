//! Template matching against the captured RoK window.
//!
//! v0.2 surface: [`find_best_needle`] searches a `&[&[u8]]` slice of embedded
//! needles against a per-tick capture PNG (`rok-capture-pre.png` /
//! `-post.png`, written by [`crate::capture`]) and returns the single global
//! best [`NeedleMatch`] — the highest-NCC match across all needles, tagged
//! with the winning needle's index — or `Ok(None)` if no position cleared
//! `MATCH_THRESHOLD`. The two embedded needles ([`NEEDLES`]: city-view art =
//! index 0, world-view art = index 1) let the v0.2 continuous loop's
//! needle-swap verify tell the city↔world toggle apart.
//!
//! Algorithm: normalized cross-correlation (NCC) over a parallel sliding
//! window via [`imageproc::template_matching::match_template_parallel`],
//! restricted to a region of interest (see [`Roi`] / [`select_roi`]).
//! ROI-cropped NCC hit the v0.2 continuous-loop cadence target on its own;
//! FFT-based NCC is indefinitely deferred (see TODOS.md and CLAUDE.md).
//!
//! Coordinate space: returned `Match.{x, y}` are in the **capture's** pixel
//! space (Retina-doubled). [`screen_point`] maps these to CG screen coords
//! given the window frame from [`crate::window`].
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
//! Assets: the needles are `include_bytes!`-embedded at compile time from
//! `assets/targets/`. Both are 96×96 tight crops of the inner glyph of
//! RoK's bottom-left city↔world toggle button — `city-button.png` is the
//! castle-medallion glyph, `world-button.png` is the map glyph. The crops
//! deliberately exclude the gold-rim/blue-circle chrome the two toggle
//! states share: under the matcher's non-mean-centered NCC a full-button
//! crop scores ~0.96 against the wrong state (shared chrome dominates the
//! correlation), while the tight glyph crop drops that to ~0.88. A
//! `needle_has_placeholder_sentinel` safety brake inside [`find_best_needle`]
//! still skips any needle carrying the v0.1.4-era synthetic placeholder
//! pattern — defense-in-depth against a placeholder being committed again.

use std::path::Path;
use std::time::Instant;

use core_graphics::display::CGRect;
use image::{GenericImageView, GrayImage, ImageReader, Limits};
use imageproc::template_matching::{MatchTemplateMethod, match_template_parallel};

use crate::error::{BotError, Result};

/// Threshold for "axis scales agree" in [`screen_point`]. A divergence of
/// more than this fraction between x-axis and y-axis scale factors is
/// logged at warn level — typically signals mixed-DPI multi-monitor setups
/// or a non-square pixel ratio in the capture (rare but possible). The
/// click still proceeds with per-axis scaling (`x_scale` on x, `y_scale`
/// on y), so the warn is informational rather than corrective: the per-axis
/// math hits the right relative offset within the window even when scales
/// disagree, but the disagreement itself is worth surfacing in case it
/// signals a deeper capture-pipeline misconfiguration.
///
/// `0.01` (1%) is the starting empirical guess from /plan-eng-review CMT-5.
/// Tune from real captures: tighten if false positives bite, relax if
/// false negatives bite.
const SCALE_DIVERGENCE_WARN_THRESHOLD: f64 = 0.01;

/// Maximum haystack dimension (in pixels) accepted by `load_haystack` (and
/// therefore by [`find_best_needle`]). Decoder-enforced via `image::Limits`,
/// so a malformed PNG with an oversized IHDR header is rejected before the
/// matcher allocates anything.
///
/// `8192` covers reasonable Retina + 6K-external-monitor captures (RoK at
/// our standard configuration is `2102×1640`, the largest external displays
/// max out around `6016×3384`). Anything larger is either operator error
/// (wrong source path) or a decompression bomb.
const MAX_HAYSTACK_DIM: u32 = 8192;

/// Confidence floor for accepting a best-match. Empirical. See module docs
/// for why scores are in `[0, 1]` (not the textbook `[-1, 1]`) — imageproc's
/// `CrossCorrelationNormalized` is not mean-centered, so for non-negative
/// grayscale pixels the formula is bounded `[0, 1]` with `1.0` at exact
/// match. `0.85` is forgiving enough for minor render variation (anti-alias
/// jitter, sub-pixel layout drift) but tight enough to reject the typical
/// `0.7-0.9` correlation seen between unrelated structured needles and
/// random captures.
///
/// Tuning: revisit after v0.1.3+ produces a stream of real-world scores.
/// If false-negatives bite, lower; if false-positives slip in, raise. The
/// `match_threshold_const_is_zero_eight_five` test pins the current value
/// so a casual change forces the operator to update both sides intentionally.
pub const MATCH_THRESHOLD: f32 = 0.85;

/// Compile-time-embedded city-view needle — index 0 in [`NEEDLES`].
/// Lives under `assets/targets/` for PR visibility but doesn't get
/// read from disk at runtime — embedding side-steps the "asset missing
/// at runtime" failure mode. The bytes can still be a malformed PNG
/// (cargo build doesn't validate PNG structure), which the needle-
/// decode arm in `find_best_needle` catches as `ImageLoadFailed`. The
/// `embedded_needle_decodes` test pins decode-validity at `cargo test` time.
///
/// The committed bytes are a 96×96 tight crop of the castle-medallion
/// glyph of RoK's city↔world toggle button. The crop excludes the
/// gold-rim/blue-circle chrome shared with the map-glyph world needle so
/// the non-mean-centered NCC can discriminate the two toggle states. The
/// sentinel-placeholder safety brake is dormant defense-in-depth (a real
/// crop carries no sentinel; see /qa live-smoke 2026-05-11 for the
/// original placeholder false-match incident).
pub const TARGET_BYTES: &[u8] = include_bytes!("../assets/targets/city-button.png");

/// Compile-time-embedded world-view needle — index 1 in [`NEEDLES`].
///
/// v0.2's continuous loop targets the city↔world toggle. Confirming a
/// click actually toggled the view (design D11 needle-swap verify)
/// needs a needle for EACH state of the toggle button: `TARGET_BYTES`
/// is the castle-medallion glyph, `WORLD_TARGET_BYTES` is the map glyph.
///
/// The committed bytes are a 96×96 tight crop of the map glyph. v0.2
/// shipped this as a sentinel placeholder, which left the needle-swap
/// verify dormant — every tick failed `no_swap` because only one real
/// needle was matchable. The v0.2.x needle re-crop replaced it with the
/// real RoK art. The tight inner-glyph crop is deliberate: a full-button
/// crop NCC-matches the castle state at ~0.96, leaving `find_best_needle`'s
/// best-of-N a fragile ~0.04 margin; the glyph crop drops the cross-state
/// score to ~0.88 for a ~0.12 margin. The crop procedure is in
/// `docs/setup.md`.
pub const WORLD_TARGET_BYTES: &[u8] = include_bytes!("../assets/targets/world-button.png");

/// The two needles in canonical index order: 0 = castle-medallion
/// glyph, 1 = map glyph. The continuous loop's per-tick match and the
/// needle-swap verify both key off these indices — a confirmed toggle
/// is "the post-click best match has a different index than the
/// pre-click best match did."
pub const NEEDLES: [&[u8]; 2] = [TARGET_BYTES, WORLD_TARGET_BYTES];

/// Sentinel pattern in the placeholder needle's top-left 4 pixels (Luma8).
/// Alternating max/min: `[255, 0, 255, 0]`. Detectable by
/// [`needle_has_placeholder_sentinel`]; statistically improbable in real RoK
/// crops because natural images don't have single-pixel max/min/max/min
/// transitions in horizontally-adjacent positions.
///
/// Once `assets/targets/city-button.png` is replaced with a real RoK UI
/// crop, the sentinel is gone and the matcher refuses no needle.
pub const PLACEHOLDER_SENTINEL_LUMA: [u8; 4] = [255, 0, 255, 0];

/// Pure: does this needle's top-left 4 pixels match the placeholder sentinel?
///
/// The sentinel gate exists because the matcher's NCC is not mean-centered
/// (see module docs) — low-entropy needles can score 0.9+ against any
/// real image. A naïve placeholder fails the "won't match" safety claim.
/// This gate makes the safety brake **structural**: the placeholder is
/// detectable by content, not by NCC-score chance.
///
/// Returns `false` immediately for needles narrower than 4 pixels or 0
/// height — a real RoK crop wider than 4 pixels can still tigger if its
/// top-left happens to look like [255, 0, 255, 0], but a natural image
/// with pixel-perfect alternating extremes in adjacent positions is
/// vanishingly rare. `/qa` caught the placeholder false-match 2026-05-11;
/// this gate is the fix.
pub fn needle_has_placeholder_sentinel(needle: &GrayImage) -> bool {
    if needle.width() < 4 || needle.height() == 0 {
        return false;
    }
    for (x, expected) in PLACEHOLDER_SENTINEL_LUMA.iter().enumerate() {
        // x < 4 by loop bound + width >= 4 by guard above. Cast is safe.
        let px = needle.get_pixel(x as u32, 0)[0];
        if px != *expected {
            return false;
        }
    }
    true
}

/// Count how many needles in the slice carry the placeholder sentinel
/// pattern. Best-effort boot diagnostic for the v0.2 loop: a placeholder
/// needle is dormant (skipped by [`find_best_needle`]), so the loop runs
/// but the needle-swap verify can never confirm a toggle. A needle that
/// fails to decode is NOT counted here — `find_best_needle` surfaces that
/// as `ImageLoadFailed` on the first tick instead.
#[must_use]
pub fn count_placeholder_needles(needles: &[&[u8]]) -> usize {
    needles
        .iter()
        .copied()
        .filter_map(|bytes| image::load_from_memory(bytes).ok())
        .filter(|img| needle_has_placeholder_sentinel(&img.to_luma8()))
        .count()
}

/// Coordinates and confidence of the best template match in the capture.
///
/// Coordinates are in the **capture's** pixel space (Retina-doubled,
/// 2102×1640 for our current screencapture output). The capture-to-screen
/// coord conversion is `matcher::screen_point` (v0.1.3), which combines a
/// `Match` with the live window `CGRect` to produce CG global-screen
/// coordinates suitable for `CGEventPost`.
///
/// `score` is in `[0, 1]` for non-negative grayscale (see module docs for
/// why this isn't `[-1, 1]`). Only scores `>= MATCH_THRESHOLD` ever surface
/// in `Some(Match)` — below-threshold returns `Ok(None)` from the matcher.
///
/// `capture_dims` and `needle_dims` are carried alongside the position so
/// downstream coordinate math doesn't have to re-decode either image.
/// `find_best_needle` already decodes both during matching; throwing the
/// dimensions away here would force `screen_point` to either re-decode the
/// haystack (silent O(N) trap on every click) or accept dims as separate
/// arguments (silent contract bug if caller and source diverge). Storing
/// them on `Match` makes the contract self-describing. Both are
/// `(width, height)` in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Match {
    pub x: u32,
    pub y: u32,
    pub score: f32,
    pub capture_dims: (u32, u32),
    pub needle_dims: (u32, u32),
}

/// A [`Match`] tagged with which needle in the searched slice produced
/// it. [`find_best_needle`] searches a `&[&[u8]]` slice of needles and
/// returns the single global best match across all of them;
/// `needle_idx` is that winning needle's position in the slice the
/// caller passed. For [`NEEDLES`], index 0 is the city-view art and
/// index 1 is the world-view art.
///
/// The v0.2 needle-swap verify (design D11) is built on this index:
/// the loop records the pre-click match's `needle_idx`, then after the
/// click re-matches the toggle ROI — a `needle_idx` that flipped means
/// the city↔world view actually toggled, which is the only
/// animation-immune proof the click landed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NeedleMatch {
    pub needle_idx: usize,
    pub m: Match,
}

/// Open a haystack PNG with decoder-enforced dimension limits, decode, and
/// convert to grayscale. Returns `BotError::ImageLoadFailed { which: "haystack" }`
/// for any failure (open, format-detection, decode, or dimensions exceeding
/// `MAX_HAYSTACK_DIM`). Each failure step logs the underlying error at warn
/// level before mapping, mirroring `capture_with_bin`'s `io::Error` handling.
///
/// Why pre-decode limits matter: PNG IDAT decompression bombs (huge IHDR-
/// declared dimensions in a small compressed payload) would otherwise OOM
/// the process before any matcher logic runs. The `image::Limits` check
/// fires inside `.decode()` based on the reader's IHDR, so we never
/// allocate the pixel buffer for an oversized image.
///
/// `pub` so it is reachable as the single dimension-limited Luma8 decode for
/// every haystack the matcher touches. Both the per-tick ROI search and the
/// full-frame fallback route through [`find_best_needle`], which calls this —
/// so a future `MAX_HAYSTACK_DIM` change tightens every capture-decode path
/// at once.
pub fn load_haystack(path: &Path) -> Result<GrayImage> {
    let mut reader = ImageReader::open(path).map_err(|err| {
        tracing::warn!(
            target: "rok_bot",
            path = %path.display(),
            error = %err,
            "failed to open haystack image"
        );
        BotError::ImageLoadFailed { which: "haystack" }
    })?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_HAYSTACK_DIM);
    limits.max_image_height = Some(MAX_HAYSTACK_DIM);
    reader.limits(limits);
    let reader = reader.with_guessed_format().map_err(|err| {
        tracing::warn!(
            target: "rok_bot",
            path = %path.display(),
            error = %err,
            "failed to detect haystack image format"
        );
        BotError::ImageLoadFailed { which: "haystack" }
    })?;
    let dynamic = reader.decode().map_err(|err| {
        tracing::warn!(
            target: "rok_bot",
            path = %path.display(),
            error = %err,
            max_dim = MAX_HAYSTACK_DIM,
            "failed to decode haystack (PNG error or dimensions exceeded MAX_HAYSTACK_DIM)"
        );
        BotError::ImageLoadFailed { which: "haystack" }
    })?;
    Ok(dynamic.to_luma8())
}

/// Rectangular region-of-interest inside a capture, in capture-pixel space.
///
/// [`find_best_needle`] takes a `roi_fn` closure returning a `Roi` to
/// restrict NCC's sliding window to a sub-rectangle of the haystack
/// (the live loop builds that closure from [`select_roi`]). The bottleneck
/// the v0.2 milestone needed to kill is NCC's `O(heatmap_pixels ×
/// needle_pixels)` cost — for our 2102×1640 Retina capture vs 180×180 needle
/// that's ~91 billion ops per pass (~22s live). Shrinking the search region
/// is the cheapest path to a usable per-tick cadence; FFT-NCC is
/// indefinitely deferred — ROI alone hit the cadence target.
///
/// **Coords are inclusive-of-origin, exclusive-of-end:** a ROI at
/// `(x=0, y=1230, w=420, h=410)` covers capture columns `0..420` and rows
/// `1230..1640`. The ROI must lie wholly inside the haystack — out-of-bounds
/// ROIs surface as [`BotError::ImageLoadFailed`] `which="haystack"` from
/// `find_best_needle`, since the only way to produce one is a caller bug
/// against a malformed haystack.
///
/// **Match coords returned through ROI are still in full-capture space.**
/// `find_best_needle` adds `(roi.x, roi.y)` back to each local match before
/// returning, and preserves the full haystack dims in `Match.capture_dims`.
/// This keeps `screen_point` working unchanged — its scale math is against
/// the original `window.size` / `capture_dims` ratio, which doesn't care
/// that the matcher only searched a sub-region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Roi {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Castle medallion ROI as fractions of capture dimensions.
///
/// Pinned to the bottom-left quadrant, where the city↔world toggle lives
/// across both Mode 1 (built-in Retina, 2102×1640) and Mode 2 (BD virtual,
/// 2102×1640). The 2026-05-13 live run matched at capture-pixel `(11, 1443)`
/// with a 180×180 needle — `(0.0, 0.75, 0.20, 0.25)` covers `(0..420,
/// 1230..1640)` with ~200px of slack around the observed position.
///
/// Fraction-of-capture (not absolute pixels) so the same constants work
/// across DPI configurations: a non-Retina 1× capture at 1051×820 produces
/// the same proportional region `(0..210, 615..820)`, which still contains
/// the analog match position. If RoK ever rescales its UI such that the
/// medallion drifts outside this quadrant, the fix is to widen the fractions
/// here, not to add per-DPI special cases.
///
/// **Tuning bound:** if the bot misses matches on a display config that
/// places the medallion outside this quadrant, /qa surfaces it as
/// `TargetNotFound` (exit 15). Logs show the no-match path with diagnostic
/// `best_score` / `best_x` / `best_y` — operator can widen the fraction
/// from those numbers without re-cropping the needle asset.
pub const CASTLE_BUTTON_ROI_FRACTION_X: f64 = 0.0;
pub const CASTLE_BUTTON_ROI_FRACTION_Y: f64 = 0.75;
pub const CASTLE_BUTTON_ROI_FRACTION_W: f64 = 0.20;
pub const CASTLE_BUTTON_ROI_FRACTION_H: f64 = 0.25;

/// Compute the castle-button [`Roi`] for a given capture size.
///
/// The fractions are clamped to `[0, 1]` and rounded to nearest pixel.
/// Returns a ROI whose `x + w <= capture_w` and `y + h <= capture_h` —
/// `find_best_needle` re-validates this invariant defensively, but
/// constructing here means the live pipeline never produces an out-of-bounds
/// ROI from a sane capture.
///
/// `#[must_use]` because the only caller pattern is "compute ROI, pass to
/// matcher" — silently dropping the result is a bug.
#[must_use]
pub fn castle_button_roi(capture_w: u32, capture_h: u32) -> Roi {
    // Compute in f64 then round to nearest pixel. Saturate at capture
    // dims so a malformed (0×0) capture produces a (0, 0, 0, 0) ROI
    // rather than a wraparound u32 — the downstream bounds check in
    // `find_best_needle` rejects this with the usual ImageLoadFailed
    // error rather than silently matching on a zero-size buffer.
    let fx = f64::from(capture_w) * CASTLE_BUTTON_ROI_FRACTION_X;
    let fy = f64::from(capture_h) * CASTLE_BUTTON_ROI_FRACTION_Y;
    let fw = f64::from(capture_w) * CASTLE_BUTTON_ROI_FRACTION_W;
    let fh = f64::from(capture_h) * CASTLE_BUTTON_ROI_FRACTION_H;

    let mut x = fx.round() as u32;
    let mut y = fy.round() as u32;
    let mut w = fw.round() as u32;
    let mut h = fh.round() as u32;

    // Clip the right/bottom edges so the ROI never extends past the capture.
    // The first two `>=` branches only fire if a future fraction const is
    // raised to >= 1.0 (today's X=0.0 + Y=0.75 keep x and y inside bounds
    // by construction). The `> capture_*` branches catch the realistic case
    // where rounding nudges `x + w` or `y + h` one pixel past the edge —
    // e.g., on a 2102×1640 capture, `1230 + 410 = 1640` lands exactly on
    // the edge, but a slightly different capture size could push over.
    // Saturating arithmetic dodges clippy's arithmetic_side_effects rule
    // and fails-soft on degenerate (0×0) captures.
    if x >= capture_w {
        x = capture_w.saturating_sub(1);
    }
    if y >= capture_h {
        y = capture_h.saturating_sub(1);
    }
    if x.saturating_add(w) > capture_w {
        w = capture_w.saturating_sub(x);
    }
    if y.saturating_add(h) > capture_h {
        h = capture_h.saturating_sub(y);
    }

    Roi { x, y, w, h }
}

/// Padding (capture pixels) added around the last match's footprint to
/// form the v0.2 last-position ROI. The city↔world toggle is a fixed
/// on-screen button — its position does not move tick to tick — so a
/// tight box around where it matched last tick is enough. The margin
/// only absorbs sub-pixel anti-alias drift between the two view arts.
/// If the toggle ever drifts further (a popup covers it, RoK re-lays-
/// out its HUD), the last-position ROI misses and `run_loop::tick`'s
/// full-frame fallback recovers — so this can be tight without risk.
///
/// 20 px → a 220×220 ROI for the 180×180 needle: the NCC heatmap is
/// ≈41×41 ≈1.7K positions vs the castle ROI's ≈56K. That ~33× cut is
/// the "per-tick near-zero NCC" the v0.2 continuous loop wants on
/// tick 2+ (once a first match has seeded `last_match`).
pub const LAST_POSITION_ROI_MARGIN_PX: u32 = 20;

/// Compute the v0.2 last-position [`Roi`]: the last match's needle
/// footprint expanded by [`LAST_POSITION_ROI_MARGIN_PX`] on every
/// side, clamped to the capture bounds.
///
/// Clamping at the capture edges means a match near a corner produces
/// a smaller (but still valid, still ≥ the needle in both dims) ROI
/// rather than an out-of-bounds one — `find_best_needle`'s bounds
/// check would otherwise reject it. The result is always ≥ the needle
/// because a valid `last` Match satisfies `last.x + needle_w <=
/// capture_w` by construction, so the clamped span never cuts below
/// the needle footprint.
#[must_use]
pub fn last_position_roi(last: Match, capture_w: u32, capture_h: u32) -> Roi {
    let (needle_w, needle_h) = last.needle_dims;
    let x = last.x.saturating_sub(LAST_POSITION_ROI_MARGIN_PX);
    let y = last.y.saturating_sub(LAST_POSITION_ROI_MARGIN_PX);
    let x_end = last
        .x
        .saturating_add(needle_w)
        .saturating_add(LAST_POSITION_ROI_MARGIN_PX)
        .min(capture_w);
    let y_end = last
        .y
        .saturating_add(needle_h)
        .saturating_add(LAST_POSITION_ROI_MARGIN_PX)
        .min(capture_h);
    Roi {
        x,
        y,
        w: x_end.saturating_sub(x),
        h: y_end.saturating_sub(y),
    }
}

/// Pure: pick the per-tick search ROI for the v0.2 continuous loop.
/// `last_match` present → search a tight box around where the toggle
/// matched last tick ([`last_position_roi`]); absent (tick 1, or after
/// a failed tick reset `last_match` to `None`) → search the broad
/// castle-button quadrant ([`castle_button_roi`]).
#[must_use]
pub fn select_roi(last_match: Option<Match>, capture_w: u32, capture_h: u32) -> Roi {
    last_match.map_or_else(
        || castle_button_roi(capture_w, capture_h),
        |m| last_position_roi(m, capture_w, capture_h),
    )
}

/// Live entry: load the haystack PNG from disk and locate the best NCC
/// match of the embedded city needle across the **full** capture.
///
/// Single-needle convenience wrapper over [`find_best_needle`] —
/// preserved as the no-ROI entry point for the test suite. The live
/// v0.2 pipeline (`run_loop` + `verify`) calls `find_best_needle`
/// directly with the full [`NEEDLES`] slice.
///
/// Returns:
/// * `Ok(Some(m))` — best match cleared `MATCH_THRESHOLD`.
/// * `Ok(None)`    — best match below threshold, or the only needle
///   was sentinel-gated. Caller translates to `BotError::TargetNotFound`.
/// * `Err(...)`    — haystack open/decode/oversize, needle decode, or
///   oversized needle.
#[allow(dead_code)]
pub fn find_target(haystack_path: &Path) -> Result<Option<Match>> {
    Ok(find_best_needle(haystack_path, &[TARGET_BYTES], |_, _| None)?.map(|nm| nm.m))
}

/// Live entry: load the haystack PNG and locate the best NCC match of
/// the embedded city needle constrained to `roi`.
///
/// Single-needle convenience wrapper over [`find_best_needle`]. Match
/// coords are offset back to **full-capture pixel space** and
/// `Match.capture_dims` is the full haystack dims, not the ROI dims —
/// see `find_best_needle` for the contract. An ROI past the haystack
/// bounds surfaces as `BotError::ImageLoadFailed { which: "haystack" }`.
#[allow(dead_code)]
pub fn find_target_in_roi(haystack_path: &Path, roi: Roi) -> Result<Option<Match>> {
    Ok(find_best_needle(haystack_path, &[TARGET_BYTES], move |_, _| Some(roi))?.map(|nm| nm.m))
}

/// Live entry: load the haystack, compute the [`castle_button_roi`] from
/// its dims, and locate the best NCC match of the embedded city needle
/// in that sub-region.
///
/// Single-needle convenience wrapper over [`find_best_needle`]. Kept
/// for the test suite (the full vs castle-ROI cross-check); the v0.2
/// live pipeline calls `find_best_needle` with [`NEEDLES`] + a
/// [`select_roi`]-driven closure instead.
#[allow(dead_code)]
pub fn find_target_in_castle_roi(haystack_path: &Path) -> Result<Option<Match>> {
    Ok(find_best_needle(haystack_path, &[TARGET_BYTES], |w, h| {
        Some(castle_button_roi(w, h))
    })?
    .map(|nm| nm.m))
}

/// Locate the best NCC match across a slice of needles in one haystack
/// (D7/CQ1 from the v0.2 `/plan-eng-review`).
///
/// The haystack is decoded ONCE and cropped to the ROI ONCE; every
/// needle then runs `match_in` against that single cropped image. The
/// return is the single global best [`NeedleMatch`] — the highest NCC
/// score across all needles, tagged with the winning needle's index in
/// `needles`. Ties go to the lower index (first encountered), matching
/// `match_in`'s lexicographic tie-break philosophy.
///
/// `roi_fn` receives the full haystack dims (post-decode) and returns
/// the ROI to search, or `None` for a full-haystack search. Generic
/// over `FnOnce(u32, u32) -> Option<Roi>` so callers hand in a static
/// `None`, a fixed `Some(roi)`, or a [`select_roi`]-driven builder
/// without probing haystack dims themselves.
///
/// Per-needle handling:
/// * A needle carrying the `PLACEHOLDER_SENTINEL_LUMA` pattern is
///   skipped (logged at warn), not matched — so a placeholder needle
///   is dormant rather than poisoning the result with a false match.
/// * A needle that fails to decode surfaces as
///   `BotError::ImageLoadFailed { which: "needle" }`.
///
/// Returns `Ok(None)` when no needle cleared `MATCH_THRESHOLD` (or
/// every needle was sentinel-gated). Match coords are re-anchored to
/// full-capture pixel space and `Match.capture_dims` is the full
/// haystack dims, so downstream `screen_point` scale math is unchanged
/// regardless of which ROI was searched.
pub fn find_best_needle<F>(
    haystack_path: &Path,
    needles: &[&[u8]],
    roi_fn: F,
) -> Result<Option<NeedleMatch>>
where
    F: FnOnce(u32, u32) -> Option<Roi>,
{
    let started = Instant::now();

    let t_haystack = Instant::now();
    let haystack = load_haystack(haystack_path)?;
    let haystack_load_ms = u64::try_from(t_haystack.elapsed().as_millis()).unwrap_or(u64::MAX);
    let full_dims = (haystack.width(), haystack.height());

    let roi = roi_fn(full_dims.0, full_dims.1);

    // Bounds-check the ROI before cropping. We treat out-of-bounds ROIs the
    // same as a malformed haystack: the only way to produce one is a caller
    // bug against the live capture dims, which manifests as exit-16 (image
    // load failed) — same diagnostic surface as a corrupt PNG.
    if let Some(r) = roi {
        let x_end = r.x.saturating_add(r.w);
        let y_end = r.y.saturating_add(r.h);
        if r.w == 0 || r.h == 0 || x_end > full_dims.0 || y_end > full_dims.1 {
            tracing::warn!(
                target: "rok_bot",
                roi_x = r.x,
                roi_y = r.y,
                roi_w = r.w,
                roi_h = r.h,
                capture_w = full_dims.0,
                capture_h = full_dims.1,
                "ROI out of bounds or zero-sized — refusing to match"
            );
            return Err(BotError::ImageLoadFailed { which: "haystack" });
        }
    }

    // Crop to the ROI (or the full haystack when no ROI is given) ONCE,
    // before the needle loop — every needle searches the same cropped
    // image (D7: single haystack decode + crop, best-of-N). Materialize
    // via `to_image()` because `imageproc::match_template_parallel`
    // takes a concrete `&ImageBuffer`, not a `SubImage` view.
    let t_crop = Instant::now();
    let effective_roi = roi.unwrap_or(Roi {
        x: 0,
        y: 0,
        w: full_dims.0,
        h: full_dims.1,
    });
    let search_image = haystack
        .view(
            effective_roi.x,
            effective_roi.y,
            effective_roi.w,
            effective_roi.h,
        )
        .to_image();
    let roi_offset = (effective_roi.x, effective_roi.y);
    let crop_ms = u64::try_from(t_crop.elapsed().as_millis()).unwrap_or(u64::MAX);

    // Per-needle: decode, sentinel-gate, NCC. Track the single global
    // best across all needles (ties go to the lower index — `is_none_or`
    // + strict `>`). Needle decode is technically fallible (include_bytes!
    // embeds bytes but doesn't validate PNG structure); a malformed asset
    // surfaces as ImageLoadFailed{needle}.
    let t_needles = Instant::now();
    let mut best: Option<NeedleMatch> = None;
    for (needle_idx, bytes) in needles.iter().copied().enumerate() {
        let needle = image::load_from_memory(bytes)
            .map_err(|err| {
                tracing::warn!(
                    target: "rok_bot",
                    needle_idx,
                    error = %err,
                    "failed to decode embedded needle"
                );
                BotError::ImageLoadFailed { which: "needle" }
            })?
            .to_luma8();

        // Placeholder sentinel gate. A needle carrying the v0.1.4-era
        // synthetic placeholder pattern is SKIPPED, not matched: the
        // matcher's non-mean-centered NCC scores low-entropy needles
        // 0.9+ against arbitrary haystacks, so matching a placeholder
        // would post a false-positive click. Skipping (vs aborting the
        // whole call) leaves the other needles in the slice free to
        // match — /qa caught the original false-match in live smoke
        // 2026-05-11.
        if needle_has_placeholder_sentinel(&needle) {
            tracing::warn!(
                target: "rok_bot",
                needle_idx,
                "placeholder sentinel needle detected (top-left luma \
                 [255,0,255,0]); skipping — replace the asset with a real \
                 RoK crop to enable matching this needle"
            );
            continue;
        }

        // Re-anchor each match to full-capture coords + restore the full
        // haystack dims. screen_point's scale factor is `window.size /
        // capture_dims`; substituting ROI dims would inflate the scale
        // and post clicks at the wrong screen point.
        if let Some(m_local) = match_in(&search_image, &needle)? {
            let m = Match {
                x: m_local.x.saturating_add(roi_offset.0),
                y: m_local.y.saturating_add(roi_offset.1),
                score: m_local.score,
                capture_dims: full_dims,
                needle_dims: m_local.needle_dims,
            };
            if best.is_none_or(|b| m.score > b.m.score) {
                best = Some(NeedleMatch { needle_idx, m });
            }
        }
    }
    let needles_ms = u64::try_from(t_needles.elapsed().as_millis()).unwrap_or(u64::MAX);

    // Profiling breakdown — fires for every call (match or no-match).
    // `roi_offset_*` lets operators translate `match_in`'s search-image-
    // space below-threshold warn coords back to full-capture pixels.
    tracing::info!(
        target: "rok_bot",
        haystack_load_ms,
        crop_ms,
        needles_ms,
        total_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        haystack_w = full_dims.0,
        haystack_h = full_dims.1,
        search_w = search_image.width(),
        search_h = search_image.height(),
        roi_offset_x = roi_offset.0,
        roi_offset_y = roi_offset.1,
        roi_applied = roi.is_some(),
        needle_count = needles.len(),
        matched = best.is_some(),
        "find_best_needle timing"
    );

    if let Some(nm) = best {
        tracing::info!(
            target: "rok_bot",
            needle_idx = nm.needle_idx,
            x = nm.m.x,
            y = nm.m.y,
            score = nm.m.score,
            elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            "found target"
        );
    }
    Ok(best)
}

/// Pure: validate that a `Match` carries non-zero `capture_dims` and
/// `needle_dims` before downstream coordinate math runs against it.
///
/// Design A16 zero-dim hard-fail: `screen_point` is infallible, but a
/// `Match` with any zero dim would feed it a zero divisor and produce
/// NaN/Inf coordinates. Catching that at the `Match` boundary maps the
/// failure to a typed exit (`ImageLoadFailed { which: "haystack" | "needle" }`,
/// exit 16) and surfaces the failure close to its source — a malformed
/// haystack capture or a malformed embedded needle decoded with garbage
/// dimensions.
///
/// Lives here (alongside `Match` and `screen_point`) rather than in
/// `main.rs::run` so the four edge cases (capture w=0, capture h=0,
/// needle w=0, needle h=0) are unit-testable without a live pipeline.
pub fn validate_match_dims(m: &Match) -> Result<()> {
    if m.capture_dims.0 == 0 || m.capture_dims.1 == 0 {
        tracing::warn!(
            target: "rok_bot",
            capture_dims = ?m.capture_dims,
            "matcher returned Match with zero capture dims — capture file likely malformed"
        );
        return Err(BotError::ImageLoadFailed { which: "haystack" });
    }
    if m.needle_dims.0 == 0 || m.needle_dims.1 == 0 {
        tracing::warn!(
            target: "rok_bot",
            needle_dims = ?m.needle_dims,
            "matcher returned Match with zero needle dims — embedded asset likely malformed"
        );
        return Err(BotError::ImageLoadFailed { which: "needle" });
    }
    Ok(())
}

/// Pure: convert a capture-pixel-space match into a CG global-screen point
/// suitable for `CGEventPost`.
///
/// The mapping is:
///
/// 1. Compute `(needle_center_x, needle_center_y)` in capture pixels — that's
///    the click anchor (design A6: click at needle CENTER, not top-left).
/// 2. Derive scale factors `x_scale = window.width / capture_width` and
///    `y_scale = window.height / capture_height`. On a single-display
///    Retina capture these are typically `0.5` (capture is 2× window in
///    points). They should agree to within ~1% on a sane setup.
/// 3. Translate by the window origin: `screen = window.origin + center * scale`.
///
/// **Design A15 — scale consistency:** in debug builds, asserts the two
/// scales agree to within `1e-6` (a float-precision tolerance — on a sane
/// single-display screencapture both scales come from the same display's
/// backing factor, so they should be bit-equal). In release, logs a
/// `tracing::warn!` if they diverge by more than
/// [`SCALE_DIVERGENCE_WARN_THRESHOLD`] (1%). **Both axes are scaled
/// independently** — `screen_x` uses `x_scale`, `screen_y` uses `y_scale`.
/// On a sane capture they're equal so this is identical to single-scale;
/// on a divergent capture the per-axis math still maps within the window
/// (the warn surfaces the divergence in case it signals a deeper capture
/// misconfiguration, but does not change the math).
///
/// **Design A16 — zero-dim hard-fail:** this function is **infallible**
/// once given a `Match`. Validation that `capture_dims > 0` and
/// `needle_dims > 0` happens at the boundary in `main.rs::run` (mapping
/// to `BotError::ImageLoadFailed { which: "haystack" }` exit 16). Inside
/// `screen_point`, zero dims would only produce NaN/Inf coordinates;
/// catching that here would hide the real failure (a malformed capture
/// arrived at the matcher).
///
/// Returned tuple is `(x, y)` in CG global-screen coordinates (top-left
/// origin, points). Negative values are valid — RoK on a virtual display
/// at e.g. `(-1051, 103)` produces negative click x-coordinates, and
/// `CGEventPost` accepts them.
#[must_use]
pub fn screen_point(m: &Match, window: &CGRect) -> (f64, f64) {
    let (capture_w, capture_h) = m.capture_dims;
    let (needle_w, needle_h) = m.needle_dims;

    let needle_center_x = f64::from(m.x) + f64::from(needle_w) / 2.0;
    let needle_center_y = f64::from(m.y) + f64::from(needle_h) / 2.0;

    let x_scale = window.size.width / f64::from(capture_w);
    let y_scale = window.size.height / f64::from(capture_h);

    debug_assert!(
        (x_scale - y_scale).abs() < 1e-6,
        "screen_point: x-scale {x_scale} and y-scale {y_scale} should agree on a normal capture; \
         diverged by {} (capture {}x{}, window {}x{})",
        (x_scale - y_scale).abs(),
        capture_w,
        capture_h,
        window.size.width,
        window.size.height,
    );

    if !cfg!(debug_assertions) {
        // Use the larger scale as the divergence denominator so a near-zero
        // smaller scale doesn't inflate the relative-difference percentage
        // into an always-fires warning. Pick `max` over `min` for the same
        // reason.
        let denom = x_scale.abs().max(y_scale.abs());
        if denom > 0.0 {
            let divergence = (x_scale - y_scale).abs() / denom;
            if divergence > SCALE_DIVERGENCE_WARN_THRESHOLD {
                tracing::warn!(
                    target: "rok_bot",
                    x_scale,
                    y_scale,
                    divergence_fraction = divergence,
                    threshold = SCALE_DIVERGENCE_WARN_THRESHOLD,
                    capture_w,
                    capture_h,
                    window_w = window.size.width,
                    window_h = window.size.height,
                    "capture-to-window scale factors disagree beyond threshold; click \
                     may miss on mixed-DPI multi-monitor setups",
                );
            }
        }
    }

    let screen_x = window.origin.x + needle_center_x * x_scale;
    let screen_y = window.origin.y + needle_center_y * y_scale;
    (screen_x, screen_y)
}

/// Pure: compute the best-match NCC score and location over in-memory
/// `GrayImage` fixtures. Tests drive this directly without disk I/O so they
/// stay fast and deterministic regardless of which bytes the embedded
/// needle holds today.
///
/// Behavior:
/// * Errors `TargetTooLarge` if `needle.width() > haystack.width() ||
///   needle.height() > haystack.height()`. Guard fires **before**
///   `match_template_parallel` because imageproc panics when needle dims
///   are strictly greater than haystack dims. (Equal dims are accepted
///   and produce a 1-wide or 1-tall heatmap; that's degenerate but
///   well-defined — empirically verified against imageproc 0.26.2's
///   `CrossCorrelationNormalized`.)
/// * Walks the resulting heatmap once, skipping NaN scores (zero-variance
///   inputs can produce NaN in normalized cross-correlation). For tied
///   maxima, the lexicographically-smallest position wins (first encountered
///   by row-major iteration), matching `imageproc::find_extremes`'s tie-break.
/// * Returns `Ok(None)` when the best score is below `MATCH_THRESHOLD` (or
///   when every score is NaN). Logs `warn!` with the diagnostic numbers.
fn match_in(haystack: &GrayImage, needle: &GrayImage) -> Result<Option<Match>> {
    if needle.width() > haystack.width() || needle.height() > haystack.height() {
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

    let t_ncc = Instant::now();
    let heatmap = match_template_parallel(
        haystack,
        needle,
        MatchTemplateMethod::CrossCorrelationNormalized,
    );
    let ncc_ms = u64::try_from(t_ncc.elapsed().as_millis()).unwrap_or(u64::MAX);

    // Manual max-search instead of `imageproc::find_extremes`: find_extremes
    // doesn't filter NaN, so a uniform-variance input would leave its max
    // initialized to whatever (0,0) holds (potentially NaN itself). We walk
    // once, skip NaN, and use strict `>` so the first-encountered wins on
    // ties — matching find_extremes's lex-smallest behavior since
    // `enumerate_pixels` iterates row-major.
    let t_max_search = Instant::now();
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
    let max_search_ms = u64::try_from(t_max_search.elapsed().as_millis()).unwrap_or(u64::MAX);

    // Profiling breakdown for the inner NCC pass. Emitted at INFO so a single
    // `cargo run --release` against a real RoK frame surfaces the numbers
    // without RUST_LOG tweaking. heatmap dims = (H-h+1, W-w+1) — naive O(N²)
    // sliding-window NCC scales as `heatmap_pixels * needle_pixels`, so these
    // numbers tell us whether shrinking the haystack region or switching to
    // FFT-NCC is the right v0.2 unlock.
    tracing::info!(
        target: "rok_bot",
        ncc_ms,
        max_search_ms,
        haystack_pixels = u64::from(haystack.width()).saturating_mul(u64::from(haystack.height())),
        needle_pixels = u64::from(needle.width()).saturating_mul(u64::from(needle.height())),
        heatmap_w = heatmap.width(),
        heatmap_h = heatmap.height(),
        heatmap_pixels = u64::from(heatmap.width()).saturating_mul(u64::from(heatmap.height())),
        "match_in NCC timing"
    );

    let Some((x, y, score)) = best else {
        // Defense-in-depth: every heatmap pixel is NaN. With imageproc 0.26.2's
        // `CrossCorrelationNormalized` this is unreachable in practice — the
        // formula `sum(i*t)/sqrt(sum(i²)*sum(t²))` yields finite values for
        // all non-negative grayscale, and the upstream `score / norm` path
        // returns the unnormalized `score` when `norm == 0` rather than NaN.
        // The zero-variance-needle guard above further short-circuits the
        // most obvious NaN-producing input. Branch retained because (1) it
        // costs nothing at runtime and (2) we'd rather log "all NaN" than
        // surface garbage if a future imageproc release changes its
        // zero-norm semantics.
        tracing::warn!(
            target: "rok_bot",
            "match heatmap was entirely NaN — defense-in-depth path; check imageproc version semantics if this fires"
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

    Ok(Some(Match {
        x,
        y,
        score,
        capture_dims: (haystack.width(), haystack.height()),
        needle_dims: (needle.width(), needle.height()),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_graphics::display::{CGPoint, CGSize};
    use image::Luma;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(&CGPoint::new(x, y), &CGSize::new(w, h))
    }

    fn make_match(x: u32, y: u32, capture: (u32, u32), needle: (u32, u32)) -> Match {
        // Score is irrelevant for screen_point math — pin to a clearly-above-
        // threshold value so a future Match invariant ("score must be >=
        // MATCH_THRESHOLD") doesn't silently invalidate these fixtures.
        Match {
            x,
            y,
            score: 0.99,
            capture_dims: capture,
            needle_dims: needle,
        }
    }

    /// Deterministic pseudo-noise. We avoid pulling in `rand` for tests —
    /// a tiny xorshift gives us reproducible distinct-looking pixel values
    /// without a third-party dep. The output isn't statistically random,
    /// just non-uniform enough that NCC against an unrelated needle scores
    /// well below 0.85.
    ///
    /// xorshift32 has a degenerate state at `seed == 0` (stays 0 forever,
    /// producing a uniform-zero "noise" image which would surprisingly
    /// trigger the matcher's variance guard). The `debug_assert` catches
    /// accidental zero seeds in test code.
    fn xorshift_byte(seed: &mut u32) -> u8 {
        debug_assert!(*seed != 0, "xorshift32 has a degenerate state at seed=0");
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
    fn match_in_populates_capture_and_needle_dims() {
        // Pin the v0.1.3 contract: Match carries both image dimensions so
        // screen_point can compute the capture→screen scale factors without
        // re-decoding either image. A regression here would be silent —
        // screen_point would still compile but produce zero-scale clicks.
        let needle = noise_image(11, 7, 0xCAFE_BABE);
        let mut haystack = noise_image(80, 50, 0xBEEF_0001);
        plant_needle_at(&mut haystack, &needle, 5, 5);

        let m = match_in(&haystack, &needle)
            .expect("size guard")
            .expect("planted needle");
        assert_eq!(m.capture_dims, (80, 50), "capture dims must equal haystack");
        assert_eq!(m.needle_dims, (11, 7), "needle dims must equal needle");
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
    fn match_in_handles_equal_size_needle_haystack() {
        // Empirical finding from /review investigation: imageproc 0.26.2's
        // CrossCorrelationNormalized DOES NOT panic at equal dims (despite
        // its docstring). It returns a 1×1 heatmap. So our guard at
        // match_in is `>` not `>=`, and equal-size flows through to a
        // valid (degenerate) match.
        //
        // Two unrelated noise patterns at the same dims won't clear
        // MATCH_THRESHOLD (correlation lands well below 0.85 by chance).
        // Pin the no-error contract here; the identity-match case
        // (`match_in_handles_equal_size_self_match`) covers the
        // non-degenerate score path.
        let haystack = noise_image(20, 20, 0xAAAA_BBBB);
        let needle = noise_image(20, 20, 0xCCCC_DDDD);
        let outcome = match_in(&haystack, &needle)
            .expect("size guard must NOT fire on equal-size — imageproc handles it");
        assert!(
            outcome.is_none(),
            "two unrelated noise patterns shouldn't clear threshold; got {outcome:?}"
        );
    }

    #[test]
    fn match_in_handles_equal_size_self_match() {
        // Identity case at equal dims: same pattern on both sides. NCC
        // produces 1.0 at the only position (0, 0). This pins that the
        // `>=` → `>` guard change doesn't accidentally break the legitimate
        // (if degenerate) identity-match path.
        let img = noise_image(20, 20, 0xEEEE_FFFF);
        let outcome = match_in(&img, &img)
            .expect("size guard must not fire")
            .expect("identity match should clear threshold");
        assert_eq!((outcome.x, outcome.y), (0, 0));
        assert!(
            outcome.score > 0.99,
            "identity NCC should be ~1.0, got {}",
            outcome.score
        );
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

    /// A synthetic sentinel needle as RGBA PNG bytes — top row carries the
    /// `PLACEHOLDER_SENTINEL_LUMA` pattern, the rest is mid-gray. Both
    /// embedded needles are real RoK crops since the v0.2.x needle re-crop,
    /// so the sentinel-gate tests build their own placeholder rather than
    /// leaning on a shipped asset.
    fn sentinel_needle_png_bytes() -> Vec<u8> {
        let mut img = GrayImage::from_pixel(8, 8, Luma([128]));
        for (x, &l) in PLACEHOLDER_SENTINEL_LUMA.iter().enumerate() {
            img.put_pixel(
                u32::try_from(x).expect("sentinel index fits u32"),
                0,
                Luma([l]),
            );
        }
        let mut rgba = image::RgbaImage::new(img.width(), img.height());
        for (x, y, p) in img.enumerate_pixels() {
            let l = p[0];
            rgba.put_pixel(x, y, image::Rgba([l, l, l, 255]));
        }
        let mut cursor = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(rgba)
            .write_to(&mut cursor, image::ImageFormat::Png)
            .expect("encode sentinel test PNG");
        cursor.into_inner()
    }

    #[test]
    fn find_target_round_trip_with_real_haystack() {
        // Integration happy path: plant the real embedded needle into a noise
        // haystack at a known position, write the haystack as RGBA PNG, then
        // call find_target on the file. Asserts the full pipeline (file IO +
        // decode + Luma + NCC + threshold + Match construction) lands at the
        // planted coords with a near-1.0 score.
        //
        // Pre-v0.1.4 this test was the same shape but blocked by the
        // sentinel-placeholder safety brake (find_target refused to match
        // any needle carrying the placeholder pattern). With the v0.1.5
        // re-crop of `assets/targets/city-button.png` to a real RoK button,
        // the sentinel pattern is gone and find_target produces a real match.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("haystack.png");

        let needle = embedded_needle_luma();
        // Haystack must be larger than the needle in both dimensions plus
        // the plant offset. The v0.2.x needle is 96×96; 400x320 leaves
        // ample background context on the right/bottom for NCC's sliding
        // window to confirm uniqueness of the planted position.
        let mut haystack = noise_image(400, 320, 0xF00D);
        plant_needle_at(&mut haystack, &needle, 100, 60);
        write_luma_as_rgba_png(&haystack, &path);

        let outcome = find_target(&path)
            .expect("find_target should succeed: needle fits, decode is valid")
            .expect("planted needle at exact pixels must clear MATCH_THRESHOLD");
        assert_eq!(
            (outcome.x, outcome.y),
            (100, 60),
            "planted location must win NCC tie-break (highest score is exact-match)"
        );
        assert!(
            outcome.score >= MATCH_THRESHOLD,
            "planted needle should score >= MATCH_THRESHOLD = {MATCH_THRESHOLD}; got {}",
            outcome.score
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
        // Garbage bytes at a .png path. image::ImageReader inspects magic
        // bytes during with_guessed_format() and fails before allocating
        // pixel buffers, which keeps this test cheap. The wrapper variant
        // pin (`which == "haystack"`) is the only invariant the matcher
        // promises here — the underlying `image::ImageError` class is an
        // implementation detail of the upstream crate.
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

    #[test]
    fn find_target_returns_none_when_no_match() {
        // Operationally the most common path: capture is fine, target isn't
        // visible. find_target returns Ok(None); main.rs translates to exit
        // 15. This exercises the full file-IO + RGBA→Luma + NCC + threshold
        // pipeline for the no-match case (the unit `match_in` test below
        // covers the pure-fn path; this pins the wrapper).
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("no-match.png");
        // Plain noise haystack — nothing planted from the embedded needle.
        let haystack = noise_image(320, 200, 0xABCD_1234);
        write_luma_as_rgba_png(&haystack, &path);
        let outcome = find_target(&path).expect("haystack must decode");
        assert!(
            outcome.is_none(),
            "noise haystack should not match the structured embedded needle; got {outcome:?}"
        );
    }

    #[test]
    fn find_target_returns_target_too_large_for_tiny_haystack() {
        // Integration pin for the size-guard at find_target's file-IO entry
        // point. When the haystack is smaller than the embedded needle in
        // either dimension, match_in's size guard fires and propagates
        // BotError::TargetTooLarge through find_target (exit 17). The
        // pure-fn equivalent is `match_in_returns_target_too_large_when
        // _needle_oversized`; this test pins the same path through the
        // file-IO wrapper.
        //
        // Pre-v0.1.5 (sentinel placeholder era) this test couldn't fire
        // because the sentinel gate intercepted the placeholder before
        // match_in's size guard ran. With the real RoK crop, the sentinel
        // gate is a no-op and the size guard becomes the operative check.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tiny.png");
        // 40x20 << 180x180 needle. Any haystack smaller than the needle in
        // either dimension trips the size guard.
        let tiny = noise_image(40, 20, 0x4242_2424);
        write_luma_as_rgba_png(&tiny, &path);
        match find_target(&path) {
            Err(BotError::TargetTooLarge { needle, haystack }) => {
                assert_eq!(haystack, (40, 20));
                assert!(
                    needle.0 > haystack.0 || needle.1 > haystack.1,
                    "TargetTooLarge needle dims must exceed haystack in at least one axis; \
                     got needle={needle:?} haystack={haystack:?}"
                );
            }
            other => panic!(
                "expected TargetTooLarge on 40x20 haystack vs embedded needle, got {other:?}"
            ),
        }
    }

    // ---------- validate_match_dims (v0.1.3) ----------

    #[test]
    fn validate_match_dims_accepts_non_zero_dims() {
        let m = make_match(0, 0, (100, 80), (10, 10));
        assert!(validate_match_dims(&m).is_ok());
    }

    #[test]
    fn validate_match_dims_rejects_zero_capture_width() {
        let m = make_match(0, 0, (0, 80), (10, 10));
        match validate_match_dims(&m) {
            Err(BotError::ImageLoadFailed { which }) => {
                assert_eq!(which, "haystack", "zero capture width must tag haystack");
            }
            other => panic!("expected ImageLoadFailed{{haystack}}, got {other:?}"),
        }
    }

    #[test]
    fn validate_match_dims_rejects_zero_capture_height() {
        let m = make_match(0, 0, (100, 0), (10, 10));
        match validate_match_dims(&m) {
            Err(BotError::ImageLoadFailed { which }) => {
                assert_eq!(which, "haystack", "zero capture height must tag haystack");
            }
            other => panic!("expected ImageLoadFailed{{haystack}}, got {other:?}"),
        }
    }

    #[test]
    fn validate_match_dims_rejects_zero_needle_width() {
        let m = make_match(0, 0, (100, 80), (0, 10));
        match validate_match_dims(&m) {
            Err(BotError::ImageLoadFailed { which }) => {
                assert_eq!(which, "needle", "zero needle width must tag needle");
            }
            other => panic!("expected ImageLoadFailed{{needle}}, got {other:?}"),
        }
    }

    #[test]
    fn validate_match_dims_rejects_zero_needle_height() {
        let m = make_match(0, 0, (100, 80), (10, 0));
        match validate_match_dims(&m) {
            Err(BotError::ImageLoadFailed { which }) => {
                assert_eq!(which, "needle", "zero needle height must tag needle");
            }
            other => panic!("expected ImageLoadFailed{{needle}}, got {other:?}"),
        }
    }

    #[test]
    fn validate_match_dims_capture_zero_takes_precedence_over_needle_zero() {
        // Both capture and needle dims zero — capture check fires first
        // (it's the primary boundary failure). Pin the order so a refactor
        // can't silently flip which `which` tag operators see.
        let m = make_match(0, 0, (0, 0), (0, 0));
        match validate_match_dims(&m) {
            Err(BotError::ImageLoadFailed { which }) => {
                assert_eq!(
                    which, "haystack",
                    "capture-zero wins precedence over needle-zero"
                );
            }
            other => panic!("expected ImageLoadFailed{{haystack}}, got {other:?}"),
        }
    }

    // ---------- screen_point (v0.1.3) ----------

    #[test]
    fn screen_point_retina_2x_known_position() {
        // Retina case (the common one): capture is 2× the window in both
        // axes. Window at (100, 200) with size 1280×720 in points; capture
        // is 2560×1440 in pixels. A needle whose top-left lands at
        // capture-pixel (1000, 600) and whose dims are (40, 60) has its
        // center at capture-pixel (1020, 630). Scale factors are both 0.5.
        // Expected screen point: (100 + 1020*0.5, 200 + 630*0.5) =
        // (610, 515).
        let m = make_match(1000, 600, (2560, 1440), (40, 60));
        let window = rect(100.0, 200.0, 1280.0, 720.0);
        let (sx, sy) = screen_point(&m, &window);
        assert!((sx - 610.0).abs() < 1e-9, "screen_x: {sx}");
        assert!((sy - 515.0).abs() < 1e-9, "screen_y: {sy}");
    }

    #[test]
    fn screen_point_non_retina_1x_known_position() {
        // External 1× display: capture pixels equal window points 1:1.
        // Window at (0, 0) with size 1024×768; capture 1024×768. A needle
        // top-left at (200, 300) with dims (20, 10) centers at (210, 305);
        // scale 1.0 → screen (210, 305).
        let m = make_match(200, 300, (1024, 768), (20, 10));
        let window = rect(0.0, 0.0, 1024.0, 768.0);
        let (sx, sy) = screen_point(&m, &window);
        assert!((sx - 210.0).abs() < 1e-9, "screen_x: {sx}");
        assert!((sy - 305.0).abs() < 1e-9, "screen_y: {sy}");
    }

    #[test]
    fn screen_point_negative_origin_virtual_display() {
        // P3 spike's actual setup: RoK on BetterDisplay virtual screen at
        // CG origin (-1051, 103) size 1051×820. Capture from screencapture
        // is typically 2× → 2102×1640 (Retina BD). A needle landing at
        // capture-pixel (1000, 800) with dims (52, 24) centers at
        // (1026, 812). Scale 0.5 → screen (-1051 + 513, 103 + 406) =
        // (-538, 509). Negative screen-x is valid for virtual displays
        // and CGEventPost accepts it (verified by the spike).
        let m = make_match(1000, 800, (2102, 1640), (52, 24));
        let window = rect(-1051.0, 103.0, 1051.0, 820.0);
        let (sx, sy) = screen_point(&m, &window);
        assert!((sx - (-538.0)).abs() < 1e-9, "screen_x: {sx}");
        assert!((sy - 509.0).abs() < 1e-9, "screen_y: {sy}");
    }

    #[test]
    fn screen_point_clicks_at_needle_center_not_top_left() {
        // Design A6: clicks must land at the geometric center of the
        // matched needle, not the top-left anchor returned by the
        // imageproc heatmap. This pin protects against a refactor that
        // accidentally drops the half-needle offset.
        let m = make_match(100, 100, (1000, 1000), (40, 60));
        let window = rect(0.0, 0.0, 1000.0, 1000.0);
        let (sx, sy) = screen_point(&m, &window);
        // Top-left would be (100, 100); center is (100 + 20, 100 + 30)
        // = (120, 130). Pin the center value AND assert it differs from
        // the top-left to make the contract violation obvious in the
        // failure message.
        assert!((sx - 120.0).abs() < 1e-9, "screen_x at center: {sx}");
        assert!((sy - 130.0).abs() < 1e-9, "screen_y at center: {sy}");
        assert!(
            (sx - 100.0).abs() > 1.0 && (sy - 100.0).abs() > 1.0,
            "screen_point must NOT return needle top-left, got ({sx}, {sy})"
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "should agree on a normal capture")]
    fn screen_point_debug_asserts_on_axis_scale_divergence() {
        // Design A15: in debug builds, a divergence between x-scale and
        // y-scale beyond 1e-6 triggers a debug_assert. This pins the
        // assertion text so future refactors can't silently weaken the
        // check (e.g., switching to debug_assert_eq! and losing the
        // descriptive message).
        //
        // Capture 100×100, window 50×100 → x-scale 0.5, y-scale 1.0:
        // a 0.5 absolute divergence, far over 1e-6.
        let m = make_match(10, 10, (100, 100), (10, 10));
        let window = rect(0.0, 0.0, 50.0, 100.0);
        let _ = screen_point(&m, &window);
    }

    #[test]
    fn scale_divergence_warn_threshold_is_one_percent() {
        // Pin the SCALE_DIVERGENCE_WARN_THRESHOLD value to its v0.1.3
        // contract. The warn-trigger is operator-facing tuning; changing
        // the threshold is a deliberate decision, not an incidental
        // refactor. Bit-equality side-steps the `float_cmp_const` lint.
        let pinned = SCALE_DIVERGENCE_WARN_THRESHOLD.to_bits() == 0.01_f64.to_bits();
        assert!(
            pinned,
            "SCALE_DIVERGENCE_WARN_THRESHOLD drifted from 0.01: {SCALE_DIVERGENCE_WARN_THRESHOLD}"
        );
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn screen_point_zero_capture_dims_returns_non_finite_in_release() {
        // Documented A16 contract: screen_point is infallible — zero dims
        // would only produce NaN/Inf, which the boundary check in
        // main.rs::run catches. Pin that screen_point itself does NOT
        // silently return (0.0, 0.0) or some other plausible-looking
        // coord, which would let a zero-dim Match slip past the boundary
        // and post a click at the window origin.
        let m = make_match(0, 0, (0, 0), (0, 0));
        let window = rect(100.0, 200.0, 1280.0, 720.0);
        let (sx, sy) = screen_point(&m, &window);
        assert!(
            !sx.is_finite() || !sy.is_finite(),
            "screen_point with zero capture_dims should produce non-finite coords \
             so the boundary check fires; got ({sx}, {sy})"
        );
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn screen_point_release_mode_tolerates_axis_scale_divergence() {
        // Design A15 release-mode side: the same input that panics in
        // debug must NOT panic in release — it just logs a warn. The
        // returned coord uses the x-axis scale for both axes (intentional;
        // see screen_point doc). Pin both that no panic occurs and that
        // the x-axis scale is the one applied.
        let m = make_match(10, 10, (100, 100), (10, 10));
        let window = rect(0.0, 0.0, 50.0, 100.0);
        let (sx, sy) = screen_point(&m, &window);
        // x-scale = 0.5, y-scale = 1.0. Each axis uses its own scale per
        // the implementation, so center (15, 15) → (15*0.5, 15*1.0) =
        // (7.5, 15.0). Pin that to keep the contract observable in release.
        assert!((sx - 7.5).abs() < 1e-9, "release screen_x: {sx}");
        assert!((sy - 15.0).abs() < 1e-9, "release screen_y: {sy}");
    }

    // ---------- Placeholder sentinel gate (v0.1.4 /qa fix) ----------

    #[test]
    fn needle_has_placeholder_sentinel_detects_canonical_pattern() {
        // Synthesize an 80×40 needle whose top-left 4 pixels match the
        // sentinel; rest is whatever (here, zeros). The gate must fire.
        let mut needle = GrayImage::new(80, 40);
        for (x, luma) in PLACEHOLDER_SENTINEL_LUMA.iter().enumerate() {
            // x < 4 by loop bound, cast safe.
            needle.put_pixel(x as u32, 0, Luma([*luma]));
        }
        assert!(
            needle_has_placeholder_sentinel(&needle),
            "needle with canonical top-left sentinel must be detected"
        );
    }

    #[test]
    fn needle_has_placeholder_sentinel_rejects_one_pixel_off() {
        // Change pixel (1, 0) from 0 to 1. Should NOT match.
        let mut needle = GrayImage::new(80, 40);
        for (x, luma) in PLACEHOLDER_SENTINEL_LUMA.iter().enumerate() {
            needle.put_pixel(x as u32, 0, Luma([*luma]));
        }
        needle.put_pixel(1, 0, Luma([1]));
        assert!(
            !needle_has_placeholder_sentinel(&needle),
            "needle with one sentinel pixel off must NOT match"
        );
    }

    #[test]
    fn needle_has_placeholder_sentinel_rejects_narrow_needle() {
        // 3-wide needle can't carry the 4-byte sentinel.
        let needle = GrayImage::from_pixel(3, 10, Luma([0]));
        assert!(
            !needle_has_placeholder_sentinel(&needle),
            "needle narrower than 4 pixels must NOT match"
        );
    }

    #[test]
    fn needle_has_placeholder_sentinel_rejects_zero_height() {
        // 0×0 image — defensively guards against degenerate dim.
        let needle = GrayImage::new(0, 0);
        assert!(
            !needle_has_placeholder_sentinel(&needle),
            "zero-dim needle must NOT match"
        );
    }

    #[test]
    fn needle_has_placeholder_sentinel_rejects_all_zero_needle() {
        // Uniform-zero needle has top-left luma [0, 0, 0, 0] — not the
        // sentinel. The variance guard in match_in catches this case via
        // a different path, but this test pins the sentinel-gate-only
        // behavior independently.
        let needle = GrayImage::from_pixel(80, 40, Luma([0]));
        assert!(
            !needle_has_placeholder_sentinel(&needle),
            "uniform-zero needle must NOT match the sentinel"
        );
    }

    #[test]
    fn placeholder_sentinel_luma_const_pinned() {
        // /qa picked [255, 0, 255, 0] as the sentinel — alternating max/min
        // luma in horizontally-adjacent positions. Changing this is a
        // safety-brake contract change: the placeholder PNG would need to
        // be regenerated to match. Pin the value.
        assert_eq!(
            PLACEHOLDER_SENTINEL_LUMA,
            [255, 0, 255, 0],
            "placeholder sentinel drifted; regenerate assets/targets/city-button.png \
             to match new pattern OR revert this test"
        );
    }

    #[test]
    fn embedded_needle_carries_no_sentinel() {
        // Inverse of the deleted `embedded_placeholder_carries_sentinel`
        // test. The v0.1.5 re-crop replaced the placeholder with a real
        // RoK button crop; the sentinel pattern must NOT be present, or
        // the safety brake in find_target would block all matches.
        // Guards against accidental re-introduction of the sentinel
        // pattern (e.g., if someone regenerates the asset using the
        // placeholder script by mistake).
        let needle = image::load_from_memory(TARGET_BYTES)
            .expect("embedded needle must decode")
            .to_luma8();
        assert!(
            !needle_has_placeholder_sentinel(&needle),
            "committed needle must NOT carry the placeholder sentinel \
             pattern [255, 0, 255, 0]. If this fires, the asset was \
             accidentally regenerated with the placeholder script — \
             restore the real RoK crop."
        );
    }

    #[test]
    fn embedded_needle_decodes() {
        // Compile-time validation that `assets/targets/city-button.png` is a
        // valid PNG. `cargo build` only checks the file exists; this test
        // catches a malformed commit before it reaches a live run where the
        // needle-decode arm of find_target would fire (exit 16 with which:
        // "needle"). Cheap insurance against an asset PR landing broken.
        let needle = image::load_from_memory(TARGET_BYTES)
            .expect("embedded TARGET_BYTES must decode as a valid image");
        assert!(
            needle.width() > 0 && needle.height() > 0,
            "embedded needle has zero dimensions: {}x{}",
            needle.width(),
            needle.height()
        );
    }

    // ---------- ROI (v0.2 unlock for FFT-NCC-tier latency without FFT) ----------

    #[test]
    fn castle_button_roi_fractions_pinned() {
        // The four fraction constants together define the bottom-left
        // quadrant where the city↔world toggle lives. Pin them as a
        // bit-pattern check so a casual change forces an intentional update.
        // Changing the quadrant on accident would silently miss matches on
        // every live run; pinning surfaces the change at test time.
        assert_eq!(CASTLE_BUTTON_ROI_FRACTION_X.to_bits(), 0.0_f64.to_bits());
        assert_eq!(CASTLE_BUTTON_ROI_FRACTION_Y.to_bits(), 0.75_f64.to_bits());
        assert_eq!(CASTLE_BUTTON_ROI_FRACTION_W.to_bits(), 0.20_f64.to_bits());
        assert_eq!(CASTLE_BUTTON_ROI_FRACTION_H.to_bits(), 0.25_f64.to_bits());
    }

    #[test]
    fn castle_button_roi_for_retina_capture_covers_observed_match() {
        // 2026-05-13 live run matched the castle at capture-pixel (11, 1443)
        // with needle dims (180, 180). The ROI must contain both the
        // match anchor (top-left) AND the full needle footprint, otherwise
        // NCC's sliding window wouldn't have seen the needle in the first
        // place. Pin this so a future fraction change can't silently drop
        // the live observation outside the ROI.
        let roi = castle_button_roi(2102, 1640);

        let match_x = 11_u32;
        let match_y = 1443_u32;
        let needle_w = 180_u32;
        let needle_h = 180_u32;

        assert!(
            match_x >= roi.x && match_y >= roi.y,
            "ROI must contain observed match anchor ({match_x}, {match_y}); \
             roi origin = ({}, {})",
            roi.x,
            roi.y,
        );
        let roi_x_end = roi.x.saturating_add(roi.w);
        let roi_y_end = roi.y.saturating_add(roi.h);
        let needle_x_end = match_x.saturating_add(needle_w);
        let needle_y_end = match_y.saturating_add(needle_h);
        assert!(
            needle_x_end <= roi_x_end && needle_y_end <= roi_y_end,
            "ROI must contain full needle footprint at observed position; \
             needle ends at ({needle_x_end}, {needle_y_end}), \
             roi ends at ({roi_x_end}, {roi_y_end})",
        );
    }

    #[test]
    fn castle_button_roi_stays_within_capture_bounds() {
        // Defense-in-depth: across a range of plausible capture sizes
        // (Retina 2×, non-Retina 1×, BD half-scale), the rounded ROI must
        // never extend past the haystack. find_target_impl's bounds check
        // would catch this at runtime as ImageLoadFailed, but pinning here
        // means we catch it at `cargo test` instead of waiting for /qa.
        for (w, h) in [(2102, 1640), (1051, 820), (3024, 1964), (1920, 1080)] {
            let roi = castle_button_roi(w, h);
            assert!(
                roi.x.saturating_add(roi.w) <= w && roi.y.saturating_add(roi.h) <= h,
                "ROI {roi:?} extends past capture {w}x{h}",
            );
            assert!(
                roi.w > 0 && roi.h > 0,
                "ROI must be non-empty for sane capture {w}x{h}; got {roi:?}",
            );
        }
    }

    #[test]
    fn castle_button_roi_zero_capture_does_not_panic() {
        // Degenerate 0×0 capture would only arrive from a malformed PNG
        // that nonetheless decoded — vanishingly unlikely but the const fn
        // must not panic on it. find_target_impl's zero-ROI check then
        // rejects with ImageLoadFailed.
        let roi = castle_button_roi(0, 0);
        assert!(
            roi.w == 0 || roi.h == 0,
            "0×0 capture must produce a zero-sized ROI; got {roi:?}",
        );
    }

    #[test]
    fn find_target_in_roi_offsets_match_coords_to_full_capture_space() {
        // Plant the embedded needle deep inside a haystack at known global
        // coords. Run find_target_in_roi with an ROI that covers the plant
        // region. The returned match must report global coords (offset by
        // ROI origin), NOT local-to-crop coords. A regression here would
        // silently shift every click by `(roi.x, roi.y)` — exactly the
        // failure mode this refactor exists to prevent.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("haystack.png");

        let needle = embedded_needle_luma();
        // 800×600 noise haystack — large enough to leave headroom outside
        // the ROI so a match at local (0, 0) would be visually distinct
        // from a global match.
        let mut haystack = noise_image(800, 600, 0xC0FF_EE01);
        // Plant at global (250, 300). Pick a ROI that starts at (200, 250)
        // and covers a 400×300 region — the needle's full 180×180 footprint
        // fits with margin. Match local coords would be (50, 50);
        // global must be (250, 300).
        plant_needle_at(&mut haystack, &needle, 250, 300);
        write_luma_as_rgba_png(&haystack, &path);

        let roi = Roi {
            x: 200,
            y: 250,
            w: 400,
            h: 300,
        };
        let outcome = find_target_in_roi(&path, roi)
            .expect("ROI fits inside haystack")
            .expect("planted needle must clear MATCH_THRESHOLD");

        assert_eq!(
            (outcome.x, outcome.y),
            (250, 300),
            "match coords must be full-capture-global, not ROI-local",
        );
    }

    #[test]
    fn find_target_in_roi_preserves_full_capture_dims() {
        // Match.capture_dims drives screen_point's scale factor against the
        // live window frame. If we returned the ROI dims here, screen_point
        // would inflate the scale by `full_w / roi_w` and post clicks
        // miles outside the window. Pin: capture_dims must be the full
        // haystack dims regardless of which ROI was searched.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("haystack.png");

        let needle = embedded_needle_luma();
        let mut haystack = noise_image(800, 600, 0xC0FF_EE02);
        plant_needle_at(&mut haystack, &needle, 250, 300);
        write_luma_as_rgba_png(&haystack, &path);

        let roi = Roi {
            x: 200,
            y: 250,
            w: 400,
            h: 300,
        };
        let outcome = find_target_in_roi(&path, roi)
            .expect("ROI fits")
            .expect("planted needle matches");

        assert_eq!(
            outcome.capture_dims,
            (800, 600),
            "capture_dims must reflect full haystack, not ROI",
        );
    }

    #[test]
    fn find_target_in_roi_returns_none_when_needle_lies_outside_roi() {
        // Plant the needle outside the ROI bounds. NCC inside the ROI sees
        // only noise; best score lands below MATCH_THRESHOLD. Confirms the
        // ROI actually restricts the search space rather than silently
        // searching the full haystack.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("haystack.png");

        let needle = embedded_needle_luma();
        let mut haystack = noise_image(800, 600, 0xC0FF_EE03);
        // Plant at (500, 400) — well outside the ROI below.
        plant_needle_at(&mut haystack, &needle, 500, 400);
        write_luma_as_rgba_png(&haystack, &path);

        // ROI covers top-left quadrant only, far from the plant.
        let roi = Roi {
            x: 0,
            y: 0,
            w: 250,
            h: 200,
        };
        let outcome =
            find_target_in_roi(&path, roi).expect("ROI bounds-valid; just no match expected");
        assert!(
            outcome.is_none(),
            "ROI must exclude needle planted at (500, 400); got {outcome:?}",
        );
    }

    #[test]
    fn find_target_in_roi_rejects_out_of_bounds_roi() {
        // Caller bug: ROI extends past the haystack. find_target_impl
        // surfaces this as ImageLoadFailed exit-16 since the only way to
        // construct one in practice is a mismatch between caller's assumed
        // capture dims and the real ones.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("haystack.png");

        let haystack = noise_image(400, 300, 0xC0FF_EE04);
        write_luma_as_rgba_png(&haystack, &path);

        let bad_roi = Roi {
            x: 200,
            y: 200,
            w: 300,
            h: 200,
        };
        match find_target_in_roi(&path, bad_roi) {
            Err(BotError::ImageLoadFailed { which }) => {
                assert_eq!(which, "haystack", "out-of-bounds ROI must tag haystack");
            }
            other => panic!("expected ImageLoadFailed haystack-tagged on OOB ROI, got {other:?}"),
        }
    }

    #[test]
    fn find_target_in_roi_rejects_zero_sized_roi() {
        // A zero-width or zero-height ROI is a degenerate input the
        // bounds-check refuses up front (NCC against a 0-row buffer is
        // undefined). Pin the error so an accidental const-fraction change
        // that rounds to zero surfaces at test time.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("haystack.png");

        let haystack = noise_image(400, 300, 0xC0FF_EE05);
        write_luma_as_rgba_png(&haystack, &path);

        let zero_w = Roi {
            x: 50,
            y: 50,
            w: 0,
            h: 100,
        };
        match find_target_in_roi(&path, zero_w) {
            Err(BotError::ImageLoadFailed { which }) => assert_eq!(which, "haystack"),
            other => panic!("expected ImageLoadFailed on zero-w ROI, got {other:?}"),
        }

        let zero_h = Roi {
            x: 50,
            y: 50,
            w: 100,
            h: 0,
        };
        match find_target_in_roi(&path, zero_h) {
            Err(BotError::ImageLoadFailed { which }) => assert_eq!(which, "haystack"),
            other => panic!("expected ImageLoadFailed on zero-h ROI, got {other:?}"),
        }
    }

    #[test]
    fn find_target_and_castle_roi_return_identical_match_when_needle_in_quadrant() {
        // Boundary pin: when the planted needle lies inside the castle ROI,
        // both entries (full-haystack `find_target` and ROI'd
        // `find_target_in_castle_roi`) must report the SAME Match coords +
        // capture_dims. The two paths exist precisely so the live pipeline
        // can take the fast lane without coord drift; this test fails if a
        // future refactor of the offset math diverges them silently.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("haystack.png");

        let needle = embedded_needle_luma();
        let mut haystack = noise_image(2102, 1640, 0xC0FF_EE07);
        plant_needle_at(&mut haystack, &needle, 11, 1443);
        write_luma_as_rgba_png(&haystack, &path);

        let full = find_target(&path)
            .expect("full-haystack decode")
            .expect("planted needle matches via full path");
        let roi = find_target_in_castle_roi(&path)
            .expect("ROI decode")
            .expect("planted needle matches via ROI path");

        assert_eq!(
            (full.x, full.y),
            (roi.x, roi.y),
            "full and castle-ROI entries must agree on planted-needle coords",
        );
        assert_eq!(
            full.capture_dims, roi.capture_dims,
            "both entries must report full capture dims",
        );
        assert_eq!(
            full.needle_dims, roi.needle_dims,
            "both entries must report identical needle dims",
        );
        // Scores may differ by float-precision noise between full and
        // cropped NCC passes — both must clear MATCH_THRESHOLD, exact
        // bit-equality not required.
        assert!(
            full.score >= MATCH_THRESHOLD && roi.score >= MATCH_THRESHOLD,
            "both scores must clear threshold; full={}, roi={}",
            full.score,
            roi.score,
        );
    }

    #[test]
    fn find_target_in_castle_roi_matches_planted_needle_in_bottom_left() {
        // End-to-end test for the live-pipeline entry. Plant the embedded
        // needle in the bottom-left quadrant of a Retina-sized synthetic
        // capture (matching the 2026-05-13 live observation). Confirm
        // the entry point loads → computes castle ROI → matches → returns
        // a Match with full-capture coords.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("haystack.png");

        let needle = embedded_needle_luma();
        let mut haystack = noise_image(2102, 1640, 0xC0FF_EE06);
        // Plant near the observed live position (11, 1443).
        plant_needle_at(&mut haystack, &needle, 11, 1443);
        write_luma_as_rgba_png(&haystack, &path);

        let outcome = find_target_in_castle_roi(&path)
            .expect("haystack decodes and ROI fits")
            .expect("planted needle in castle quadrant must match");

        assert_eq!(
            (outcome.x, outcome.y),
            (11, 1443),
            "castle-ROI match must return full-capture coords",
        );
        assert_eq!(
            outcome.capture_dims,
            (2102, 1640),
            "capture_dims must reflect full haystack",
        );
    }

    // ---------- v0.2: NeedleMatch / find_best_needle ----------

    /// Encode a luma image as RGBA PNG bytes in memory. The round-trip
    /// through `find_best_needle`'s decode preserves luma exactly
    /// (BT.601 weights sum to 1, R=G=B). Used to feed synthetic needles
    /// into `find_best_needle`, which takes `&[&[u8]]` PNG bytes.
    fn encode_luma_as_png_bytes(img: &GrayImage) -> Vec<u8> {
        let mut rgba = image::RgbaImage::new(img.width(), img.height());
        for (x, y, p) in img.enumerate_pixels() {
            let l = p[0];
            rgba.put_pixel(x, y, image::Rgba([l, l, l, 255]));
        }
        let mut cursor = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(rgba)
            .write_to(&mut cursor, image::ImageFormat::Png)
            .expect("encode test needle PNG must succeed");
        cursor.into_inner()
    }

    #[test]
    fn find_best_needle_single_element_slice_matches_find_target() {
        // Regression-guard parity (the v0.2 plan's explicit ask): a
        // 1-element needle slice through find_best_needle must produce
        // the SAME Match the v0.1.7/v0.1.8 single-needle find_target
        // produced — needle_idx 0, byte-identical `.m`.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("haystack.png");
        let needle = embedded_needle_luma();
        let mut haystack = noise_image(800, 600, 0xBEEF_2202);
        plant_needle_at(&mut haystack, &needle, 220, 140);
        write_luma_as_rgba_png(&haystack, &path);

        let via_find_target = find_target(&path)
            .expect("find_target decode")
            .expect("find_target match");
        let via_slice = find_best_needle(&path, &[TARGET_BYTES], |_, _| None)
            .expect("find_best_needle decode")
            .expect("find_best_needle match");
        assert_eq!(
            via_slice.needle_idx, 0,
            "single-element slice → needle_idx 0"
        );
        assert_eq!(
            via_slice.m, via_find_target,
            "1-element needle slice must produce a Match identical to find_target",
        );
    }

    #[test]
    fn find_best_needle_returns_global_best_across_needles() {
        // Two distinct synthetic needles. Plant ONLY needle 1 exactly;
        // needle 0 is absent (haystack is otherwise noise). The global
        // best must be needle_idx 1 — find_best_needle returns the
        // highest-scoring needle, not the first in the slice.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("haystack.png");
        let needle0 = noise_image(40, 40, 0x1111_2222);
        let needle1 = noise_image(40, 40, 0x3333_4444);
        let mut haystack = noise_image(400, 300, 0x5555_6666);
        plant_needle_at(&mut haystack, &needle1, 100, 80);
        write_luma_as_rgba_png(&haystack, &path);

        let n0 = encode_luma_as_png_bytes(&needle0);
        let n1 = encode_luma_as_png_bytes(&needle1);
        let nm = find_best_needle(&path, &[n0.as_slice(), n1.as_slice()], |_, _| None)
            .expect("haystack decodes")
            .expect("needle 1 is planted exactly → clears threshold");
        assert_eq!(
            nm.needle_idx, 1,
            "the planted needle (idx 1) must win best-of-N"
        );
        assert_eq!(
            (nm.m.x, nm.m.y),
            (100, 80),
            "match coords at the plant site"
        );
    }

    #[test]
    fn find_best_needle_returns_city_match_when_only_city_planted() {
        // NEEDLES = [city, world], both real RoK crops since the v0.2.x
        // re-crop. Plant only the city (castle-medallion) needle into a
        // noise haystack: find_best_needle's best-of-N must return the
        // city match at needle_idx 0 — the world needle has nothing to
        // correlate with in the noise.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("haystack.png");
        let needle = embedded_needle_luma();
        let mut haystack = noise_image(800, 600, 0xC1A7_0001);
        plant_needle_at(&mut haystack, &needle, 200, 150);
        write_luma_as_rgba_png(&haystack, &path);

        let nm = find_best_needle(&path, &NEEDLES, |_, _| None)
            .expect("haystack decodes")
            .expect("city needle planted exactly → clears threshold");
        assert_eq!(
            nm.needle_idx, 0,
            "city needle (idx 0) planted exactly must win best-of-N",
        );
        assert_eq!((nm.m.x, nm.m.y), (200, 150));
    }

    #[test]
    fn find_best_needle_returns_world_match_when_only_world_planted() {
        // Regression for the v0.2.x needle re-crop. While world-button.png
        // was the magenta-X sentinel placeholder, find_best_needle skipped
        // the world needle entirely, so the needle-swap verify could never
        // observe a city→world toggle (every tick failed `no_swap`).
        // Planting the real world (map-glyph) needle must now yield a
        // needle_idx 1 match; with the placeholder this panics on the
        // `.expect` below (sentinel-skipped → no idx-1 match).
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("haystack.png");
        let world = image::load_from_memory(WORLD_TARGET_BYTES)
            .expect("world needle decodes")
            .to_luma8();
        let mut haystack = noise_image(800, 600, 0xD00D_0001);
        plant_needle_at(&mut haystack, &world, 200, 150);
        write_luma_as_rgba_png(&haystack, &path);

        let nm = find_best_needle(&path, &NEEDLES, |_, _| None)
            .expect("haystack decodes")
            .expect("world needle planted exactly → clears threshold");
        assert_eq!(
            nm.needle_idx, 1,
            "world needle (idx 1) planted exactly must win best-of-N — it was \
             sentinel-skipped while world-button.png was the placeholder",
        );
        assert_eq!((nm.m.x, nm.m.y), (200, 150));
    }

    #[test]
    fn embedded_world_needle_decodes() {
        // Compile-time validation that assets/targets/world-button.png
        // is a valid PNG. `cargo build` only checks the file exists;
        // this catches a malformed commit before a live run.
        let needle = image::load_from_memory(WORLD_TARGET_BYTES)
            .expect("embedded WORLD_TARGET_BYTES must decode as a valid image");
        assert!(
            needle.width() > 0 && needle.height() > 0,
            "embedded world needle has zero dimensions: {}x{}",
            needle.width(),
            needle.height()
        );
    }

    #[test]
    fn embedded_world_needle_carries_no_sentinel() {
        // The v0.2.x needle re-crop replaced the magenta-X placeholder
        // with a real RoK map-glyph crop. The sentinel pattern must NOT
        // be present, or find_best_needle would sentinel-skip the world
        // needle and the needle-swap verify would go dormant again
        // (every tick `no_swap`). Mirror of `embedded_needle_carries_no_sentinel`.
        let needle = image::load_from_memory(WORLD_TARGET_BYTES)
            .expect("embedded world needle must decode")
            .to_luma8();
        assert!(
            !needle_has_placeholder_sentinel(&needle),
            "committed world needle must NOT carry the placeholder sentinel \
             pattern [255, 0, 255, 0]. If this fires, \
             assets/targets/world-button.png was reverted to the placeholder \
             — restore the real RoK map-button crop."
        );
    }

    #[test]
    fn needles_const_is_city_then_world() {
        // The continuous loop + needle-swap verify key off these
        // indices: 0 = city art, 1 = world art. Pin the order.
        let [city, world] = NEEDLES;
        assert_eq!(city, TARGET_BYTES, "NEEDLES[0] must be the city needle");
        assert_eq!(
            world, WORLD_TARGET_BYTES,
            "NEEDLES[1] must be the world needle"
        );
    }

    // ---------- v0.2: last_position_roi / select_roi ----------

    #[test]
    fn last_position_roi_margin_const_pinned() {
        // The margin sizes the per-tick fast-path ROI. Changing it is a
        // latency tuning decision — pin so a casual change is intentional.
        assert_eq!(LAST_POSITION_ROI_MARGIN_PX, 20);
    }

    #[test]
    fn last_position_roi_boxes_needle_with_margin() {
        // Match at (500, 600), 180×180 needle, 2102×1640 capture. ROI
        // is the needle footprint padded 20px every side: x=480, y=580,
        // w=220, h=220.
        let m = make_match(500, 600, (2102, 1640), (180, 180));
        let roi = last_position_roi(m, 2102, 1640);
        assert_eq!(
            roi,
            Roi {
                x: 480,
                y: 580,
                w: 220,
                h: 220,
            },
        );
        // Sanity: the ROI fully contains the needle footprint.
        assert!(roi.x <= m.x && roi.y <= m.y);
        assert!(roi.x.saturating_add(roi.w) >= m.x.saturating_add(180));
        assert!(roi.y.saturating_add(roi.h) >= m.y.saturating_add(180));
    }

    #[test]
    fn last_position_roi_clamps_at_top_left_corner() {
        // Match at (5, 5): the 20px margin would underflow the origin.
        // saturating_sub clamps x and y to 0; the ROI stays ≥ needle.
        let m = make_match(5, 5, (2102, 1640), (180, 180));
        let roi = last_position_roi(m, 2102, 1640);
        assert_eq!(roi.x, 0, "left margin clamps to 0");
        assert_eq!(roi.y, 0, "top margin clamps to 0");
        assert!(
            roi.w >= 180 && roi.h >= 180,
            "clamped ROI must still cover the needle: {roi:?}",
        );
    }

    #[test]
    fn last_position_roi_clamps_at_bottom_right_corner() {
        // Needle flush against the bottom-right edge. The +margin would
        // overrun the capture; .min() clamps x_end/y_end to the bounds.
        let m = make_match(2102 - 180, 1640 - 180, (2102, 1640), (180, 180));
        let roi = last_position_roi(m, 2102, 1640);
        assert_eq!(
            roi.x.saturating_add(roi.w),
            2102,
            "right edge clamps to capture_w"
        );
        assert_eq!(
            roi.y.saturating_add(roi.h),
            1640,
            "bottom edge clamps to capture_h"
        );
        assert!(
            roi.w >= 180 && roi.h >= 180,
            "clamped ROI must still cover the needle: {roi:?}",
        );
    }

    #[test]
    fn last_position_roi_stays_within_capture_and_at_least_needle() {
        // Across plausible match positions the ROI must never extend
        // past the capture (find_best_needle's bounds check would
        // reject it) and never shrink below the needle (match_in's
        // size guard would fire TargetTooLarge).
        for (mx, my) in [(0, 0), (11, 1443), (500, 500), (1922, 1460)] {
            let m = make_match(mx, my, (2102, 1640), (180, 180));
            let roi = last_position_roi(m, 2102, 1640);
            assert!(
                roi.x.saturating_add(roi.w) <= 2102 && roi.y.saturating_add(roi.h) <= 1640,
                "ROI {roi:?} extends past capture for match ({mx}, {my})",
            );
            assert!(
                roi.w >= 180 && roi.h >= 180,
                "ROI {roi:?} smaller than needle for match ({mx}, {my})",
            );
        }
    }

    #[test]
    fn select_roi_none_returns_castle_roi() {
        // Tick 1 (or after a failed tick reset last_match): no last
        // match → broad castle-button quadrant.
        let got = select_roi(None, 2102, 1640);
        assert_eq!(got, castle_button_roi(2102, 1640));
    }

    #[test]
    fn select_roi_some_returns_last_position_roi() {
        // Tick 2+: a last match seeds the tight last-position ROI.
        let m = make_match(500, 600, (2102, 1640), (180, 180));
        let got = select_roi(Some(m), 2102, 1640);
        assert_eq!(got, last_position_roi(m, 2102, 1640));
    }

    // ---------- v0.2: find_best_needle edge cases (from /review) ----------

    #[test]
    fn find_best_needle_tie_breaks_to_lower_index() {
        // Two byte-identical needles → both score 1.0 at the plant site.
        // The documented contract is: a score tie resolves to the lower
        // needle index (strict `>` in the is_none_or predicate). A future
        // `>` → `>=` slip would silently flip this — this test catches it.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("haystack.png");
        let needle = noise_image(40, 40, 0x7777_8888);
        let mut haystack = noise_image(400, 300, 0x9999_AAAA);
        plant_needle_at(&mut haystack, &needle, 120, 90);
        write_luma_as_rgba_png(&haystack, &path);
        let n = encode_luma_as_png_bytes(&needle);
        let nm = find_best_needle(&path, &[n.as_slice(), n.as_slice()], |_, _| None)
            .expect("haystack decodes")
            .expect("identical needles both clear threshold");
        assert_eq!(
            nm.needle_idx, 0,
            "a score tie must resolve to the lower needle index",
        );
    }

    #[test]
    fn find_best_needle_empty_slice_returns_none() {
        // An empty needle slice: the per-needle loop never runs → Ok(None).
        // find_best_needle is pub; run_loop always passes the 2-element
        // NEEDLES, but the empty-slice path must be a clean no-match.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("haystack.png");
        write_luma_as_rgba_png(&noise_image(200, 200, 0x1357_2468), &path);
        let empty: [&[u8]; 0] = [];
        let got = find_best_needle(&path, &empty, |_, _| None)
            .expect("an empty needle slice must not error");
        assert!(got.is_none(), "an empty needle slice yields no match");
    }

    #[test]
    fn find_best_needle_malformed_needle_surfaces_image_load_failed() {
        // Garbage bytes as a needle: the per-needle decode arm must
        // surface BotError::ImageLoadFailed{which:"needle"} — propagated
        // as Err, not silently skipped like the sentinel gate.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("haystack.png");
        write_luma_as_rgba_png(&noise_image(200, 200, 0x2468_1357), &path);
        let garbage: &[u8] = b"not a png at all";
        match find_best_needle(&path, &[garbage], |_, _| None) {
            Err(BotError::ImageLoadFailed { which }) => assert_eq!(which, "needle"),
            other => {
                panic!("expected ImageLoadFailed{{needle}} for a malformed needle, got {other:?}")
            }
        }
    }

    #[test]
    fn find_best_needle_all_sentinel_needles_returns_none() {
        // Every needle carries the placeholder sentinel → all skipped →
        // Ok(None): no match, no error. Both shipped needles are real RoK
        // crops now, so the test builds synthetic sentinel needles.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("haystack.png");
        write_luma_as_rgba_png(&noise_image(300, 300, 0xABCD_1234), &path);
        let sentinel = sentinel_needle_png_bytes();
        let s: &[u8] = &sentinel;
        let got = find_best_needle(&path, &[s, s], |_, _| None).expect("haystack decodes");
        assert!(got.is_none(), "every needle sentinel-gated → Ok(None)");
    }

    #[test]
    fn count_placeholder_needles_counts_sentinel_needles() {
        // Both embedded needles are real RoK crops since the v0.2.x
        // re-crop → zero placeholders. Synthetic sentinel needles drive
        // the counting assertions. count_placeholder_needles(&NEEDLES)
        // returning 0 is what keeps the run_loop boot warning silent.
        assert_eq!(count_placeholder_needles(&NEEDLES), 0);
        assert_eq!(count_placeholder_needles(&[TARGET_BYTES]), 0);
        assert_eq!(count_placeholder_needles(&[WORLD_TARGET_BYTES]), 0);
        let sentinel = sentinel_needle_png_bytes();
        let s: &[u8] = &sentinel;
        assert_eq!(count_placeholder_needles(&[s]), 1);
        assert_eq!(count_placeholder_needles(&[s, s]), 2);
        assert_eq!(count_placeholder_needles(&[TARGET_BYTES, s]), 1);
    }
}
