# P8 spike — `objc2-screen-capture-kit` one-shot CGWindow capture

Phase 0 spike for v0.2 (`TODOS.md` P1). Verifies that
`objc2-screen-capture-kit` 0.3.x can capture a single PNG of a known
`CGWindowID` from a CLT-only Rust binary, replacing the v0.1.x
`screencapture -l <wid>` CLI subprocess.

## Why this exists (separate from p2-spike)

p2-spike (`spikes/p2-spike/run.sh`) proved at the architecture level
that Apple's `screencapture` CLI captures the RoK window on a virtual
display. v0.1.x has been shipping on top of that subprocess.

v0.2's continuous capture+match+click+verify loop needs per-tick capture
cadence — the subprocess fixed-cost (`fork`/`exec`/PNG-encode-on-disk,
≈ 300–1400 ms per call depending on system load) blocks ≥1 Hz operation.
The fix is to move capture in-process via Apple's ScreenCaptureKit
framework. The catch: the original `screencapturekit` Rust crate (1.5.x)
wraps an internal Swift package via `swift-bridge`, and its `build.rs`
invokes `xcrun --sdk macosx --show-sdk-platform-path` — which fails on
machines with only Xcode Command Line Tools (full Xcode required,
~14 GB).

`objc2-screen-capture-kit` 0.3.x is the canonical CLT-friendly
alternative — direct ObjC2 message-passing bindings, no Swift bridge,
no Xcode requirement. p8-spike answers two narrower questions before
v0.2 commits to the full pipeline migration:

1. **Does `objc2-screen-capture-kit` 0.3.x actually build and run on
   this machine?** The whole reason we deferred SCK adoption in v0.1
   was the build-system hazard. The spike's `Cargo.toml` is a smaller
   verification surface than a full `src/capture.rs` rewrite.
2. **Does `SCScreenshotManager.captureImageWithFilter:configuration:`
   produce the same observable capture as `screencapture -l <wid>` for
   a RoK window on a BetterDisplay virtual display?** Specifically:
   - Captures by `SCWindow` reference (not `CGWindowID`), so the v0.1.5
     window-ID-reuse TOCTOU class disappears by construction.
   - Captures even when RoK is on a Space that isn't currently
     displayed (v0.1.6's `kCGWindowListOptionAll` fallback territory).
   - Returns BGRA pixel data we can convert to a PNG that decodes
     identically to the matcher's existing input.

If both succeed, v0.2 can proceed with the production `src/capture.rs`
migration. If either fails, the v0.2 design has revisit branches:
fall back to `screencapturekit` 1.5.x and require contributors to
install full Xcode, or keep the CLI subprocess and live with the
cadence ceiling.

## Prerequisites

- macOS 14.0+ (`SCScreenshotManager` is 14+; user is on macOS 15+ per
  `uname -r`).
- Xcode Command Line Tools installed (`xcode-select --install`). Full
  Xcode is NOT required — proving that is part of the spike's value.
- RoK is running (and ideally parked on a BetterDisplay virtual display
  per the project's Mode 2 setup) so there's a non-built-in window
  candidate to capture.
- Screen Recording permission has been granted to the terminal you'll
  run the spike from. On first run macOS may also prompt for the spike
  binary specifically — grant it, then re-run.

## Run procedure

### 1. Look up the RoK CGWindowID

The parent `rok-bot` already prints the window id on every run. Easiest
path:

```bash
cd ../..    # back to rok-bot/
cargo run --release 2>&1 | head -20
```

Grab the `window: id=N` value from the log. Or query directly:

```bash
osascript -e 'tell application "System Events" to get id of (every window whose name contains "Rise of Kingdoms")' 2>/dev/null
```

Note: `CGWindowID` is per-launch; if RoK has been re-launched since the
v0.1.7 session it will be different.

### 2. Build + run

```bash
cd spikes/p8-spike
cargo build --release --locked
./target/release/p8-spike --wid <WID> --out /tmp/p8-rok.png
```

Expected stderr:

```text
[p8-spike] requesting SCShareableContent…
[p8-spike] got shareable content in NNN ms
[p8-spike] found SCWindow wid=<WID> frame=(X,Y) WxH (points)
[p8-spike] requesting SCScreenshotManager capture W×2 x H×2…
[p8-spike] image: PWxPH bytes_per_row=BPR
[p8-spike] captured PWxPH → /tmp/p8-rok.png (content N ms, capture N ms, total N ms)
```

Exit 0 on success. Exit 10 if `SCShareableContent` returned nothing
(usually TCC denial). Exit 11 if the `CGWindowID` wasn't in the
shareable set. Exit 12 if capture itself failed. Exit 13/14 for image
decode / PNG write failures.

### 3. Compare against `screencapture -l`

```bash
/usr/sbin/screencapture -l <WID> -x -o /tmp/cli-rok.png
ls -la /tmp/p8-rok.png /tmp/cli-rok.png
identify /tmp/p8-rok.png /tmp/cli-rok.png 2>/dev/null  # or `file` / `sips -g pixelWidth -g pixelHeight ...`
```

Dimensions should match (within ±2 px for off-by-one rounding). Open
both in Preview and eyeball — they should look identical (same content,
same colors, no obvious tearing).

### 4. Time it

The spike already prints `content`, `capture`, and `total` ms. Record
those numbers and compare to the v0.1.7 `screencapture` baseline
(~300 ms typical, ~1400 ms under load on the same machine).

### 5. Record outcome

Append a row to the table below.

## Outcome record

| Date       | Target                          | Frame (points)            | Capture (PNG dims) | content ms (avg of 5) | capture ms (avg of 5) | total ms (avg of 5) | screencapture baseline (avg of 5) | Match? | Notes |
| ---------- | ------------------------------- | ------------------------- | ------------------ | --------------------- | --------------------- | ------------------- | --------------------------------- | ------ | ----- |
| 2026-05-15 | RoK on BD (pid 40933, wid 82449) | (-1125, 92) 1051×820       | 2102×1640 RGBA      | 95 (92–100)            | 141 (132–148)          | 280 (262–288)        | 381 ms (367–394)                   | Y      | First run aborted with `CGS_REQUIRE_INIT` assertion; fix: call `NSApplicationLoad()` at startup to bootstrap the WindowServer connection. After fix: dims/format/sample-count match CLI exactly. Decoded-pixel SHA differs (expected — captures are seconds apart and RoK is animating; 1.98% of luma values differ by >3, mean diff 0.86). bytes_per_row=8448 (= 2102×4 + 40 alignment padding) — RGBA converter strips padding correctly. Cold-process p8 (paying full SCShareableContent fetch every run) beats CLI by ~26%. Production loop with cached SCShareableContent will run capture alone (~141 ms), ~63% faster than CLI. |

## Outcome gate (decides v0.2 capture strategy)

**Outcome: Match.** 2026-05-15 live run on BD virtual display confirms
SCK captures parity-matching PNGs ~26% faster than the CLI cold, and
~63% faster once `SCShareableContent` is cached. v0.2 proceeds with
the production `src/capture.rs` rewrite. Drop the
`/usr/sbin/screencapture` shellout.

Remaining production-port concerns surfaced by the spike (must be
addressed in the v0.2 design doc, not deferred):

1. **`NSApplicationLoad()` startup call required.** A plain Rust binary
   that only pulls in CG bindings does not register with WindowServer,
   so `SCStreamConfiguration::new` trips `CGS_REQUIRE_INIT`. Production
   `src/main.rs` needs the same one-line bootstrap before any SCK call.
   Add a `link_appkit` FFI to `permissions.rs` (or a new
   `cg_bootstrap.rs`) so the dependency is documented at the boot site.
2. **Cache `SCShareableContent` across the continuous loop.** ~95 ms
   per fetch is the dominant cost; re-resolving the SCWindow by
   owner+title+bundle-ID against a cached content list keeps every
   capture in the ~141 ms range. Refresh the cache on `WindowVanished`
   or on a periodic timer (~1 Hz) — same trigger surface as the v0.1.5
   TOCTOU validator.
3. **BGRA → image pixel-format alignment.** The matcher decodes via
   `image::ImageReader`, so the PNG path is fine. But the production
   port may want to skip PNG encoding entirely and feed
   `image::ImageBuffer<Bgra<u8>, _>` directly to the matcher to cut
   the PNG round-trip. Bench in the production PR.

The previous failure-mode branches (build fails on CLT-only, capture
returns no image, slower than baseline) all turned out negative — kept
above as historical record in case a future macOS update regresses.

## Known unknowns this spike does NOT answer

- **`SCStream` continuous mode** vs. `SCScreenshotManager` one-shot.
  v0.2's continuous loop may eventually want the streaming API for
  near-zero-overhead per-frame capture. p8 only validates one-shot;
  benchmarking sustained capture is a v0.2 follow-on.
- **Window-ID-vs-SCWindow re-binding** in `src/window.rs`. TODOS line
  188–202 captures the design; p8 takes a `CGWindowID` as input for
  parity with v0.1.x but the production port will re-resolve by
  owner+title+bundle-ID against the shareable content list at use-time.
- **Permission prompt UX**. SCK respects the same Screen Recording TCC
  bit that `screencapture` does, but the prompt-trigger semantics
  (when exactly it fires) may differ. Document during the live run.

## Disposable

Once v0.2's production `src/capture.rs` lands, this spike directory can
be deleted with no loss. The decisions it produced should land in the
v0.2 design doc.
