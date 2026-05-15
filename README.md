# rok-bot

Rust-based macOS automation experiment for **Rise of Kingdoms** on Apple Silicon. Personal/learning project. Public so others can read the design choices, not because it's polished or supported.

## Status: v0.1.8 — In-process ScreenCaptureKit drops capture from ~280ms-1.4s to ~140ms (cache-hit) and retires the `screencapture` CLI subprocess

What works today (v0.1.8):
- **In-process capture via `objc2-screen-capture-kit` 0.3.x** — replaces v0.1.x's `/usr/sbin/screencapture -l <wid>` CLI shellout with `SCScreenshotManager.captureImageWithFilter` for ~140ms steady-state per capture (vs ~280-1400ms via subprocess). Live-confirmed v0.1.8 on Mode 2 BD virtual display: 191ms cold + 126ms cache-hit = 317ms across both pre/post captures, well under the v0.1.7 baseline.
- **`SCShareableContent` cache** — `OnceLock<ContentCache>` amortizes the ~95ms enumeration cost across the discovery + capture path within a single `cargo run`. Invalidates on `CaptureFailed { stage: window_not_found }`.
- **`NSApplicationLoad` WindowServer bootstrap** — call from every SCK entrypoint (`permissions::check_sck_grant`, `window::find_rok_window`, `capture::capture_window`) so a CLI Rust binary doesn't trip `CGS_REQUIRE_INIT` when `SCStreamConfiguration::new` fires. New `src/cg_bootstrap.rs`.
- **`DispatchSemaphore` with explicit timeouts** — 5s for capture, 10s for shareable-content fetch (covers TCC prompt latency on first launch). Maps to `CaptureFailed { stage: capture_returned_nil | no_shareable_content }`. Spike's `DispatchTime::FOREVER` was retired per T1 from `/plan-eng-review` to ensure a hung SCK call exits cleanly instead of freezing the binary.
- **Find the RoK main window via SCK enumeration** — `SCShareableContent.windows` filtered by `SCWindow.title == "RiseOfKingdoms"` AND `SCWindow.owningApplication.bundleIdentifier` starting with `com.rok.ios.` (anti-spoof gate). Fallback to CGWindow enumeration is gone (SCK enumerates hidden-Space windows too).
- **Hidden-Space distinction at boot.** Same exit-19 `WindowChanged { not_visible }` vs exit-10 `WindowNotFound` split as v0.1.7, now via the v0.1.8 validator's CGWindow `OnScreenOnly` membership check (D1 from `/plan-eng-review`).
- Classify which display RoK is on via `CGDisplayIsBuiltin`. Both built-in (`Mode::Visible`) and non-built-in (`Mode::Virtual` — BetterDisplay virtual, external monitor, Sidecar) proceed through the same pipeline. v0.1.5's exit-12 Mode 2 gate was dropped in v0.1.6.
- **SCK-specific TCC preflight in `permissions::check_sck_grant`** — actionable error if Screen Recording isn't granted to the rok-bot binary itself (v0.1.x inherited TCC from the parent terminal; v0.1.8 needs per-binary grant — a UX regression documented in the boot log line and below in setup.md).
- **Symlink-safe PNG write via `O_NOFOLLOW + O_EXCL`** — replaces v0.1.x's `symlink_metadata` pre-check (which had a ~141ms-5s TOCTOU window). `open_capture_output_safely` is race-free against a local attacker planting symlinks.
- **`Arc<*Slot>` cross-thread completion-handler slots** — fixes a use-after-free that the v0.1.8 `/review` caught: the spike's stack-local `Mutex<Option<Retained<T>>>` captured by raw-pointer-as-usize would dangle if the SCK 5s timeout fired while SCK still held the retained block. Wrapped in `Arc<ImageSlot>` / `Arc<ContentSlot>` with newtype + `unsafe Send + Sync` (CG/SCK objects are Apple-documented thread-safe).
- **Template-match a known UI element** via `imageproc::match_template_parallel` (rayon-parallel NCC sliding window), restricted to a castle-button ROI. `find_target_in_castle_roi` crops the haystack to the bottom-left quadrant (20% × 25% of the capture) before NCC, dropping the heatmap from 2.81M to 56K positions. Live-confirmed v0.1.7: ~442ms per match on the 2102×1640 Retina haystack, down from ~22s in v0.1.6 (50× speedup). Match coords are restored to full-capture space inside `find_target_impl`, so `screen_point` math is unchanged. Configurable `MATCH_THRESHOLD` (default `0.85`); ROI dims pinned via `CASTLE_BUTTON_ROI_FRACTION_*` constants. FFT-NCC was the original v0.2 plan; ROI alone hit the target, so FFT is now indefinitely deferred.
- **Placeholder-sentinel safety brake.** The shipped needle has a structural sentinel (`[255, 0, 255, 0]` top-left luma + xorshift32 noise body); `matcher::needle_has_placeholder_sentinel` fail-closes BEFORE NCC runs so the bot can never synthetically click against a falsely-matched placeholder. /qa caught a real placeholder false-match at 0.93 NCC; this brake prevents the class.
- **4-check pre-click TOCTOU validation** (v0.1.5, anchored on WID+PID, runs BEFORE the AX TCC prompt so a hidden-Space exit doesn't waste an Accessibility grant). Maps to four `WindowChanged` reasons: `window_id_gone`, `not_visible`, `frame_moved`, `point_outside_frame`.
- **Click delivery via stealth HID tap + osascript activation** (v0.1.6 — reverts v0.1.5 AX press, which was structurally broken on Mac Catalyst Bridge; see TODOS for the full 6-path investigation). Sequence: `osascript` activate RoK by pid via System Events → 50ms settle → probe + disassociate cursor → `CGEvent::post(HID)` `LeftMouseDown` → 80ms gap → `LeftMouseUp` → warp logical cursor back → reassociate. RAII `CursorStealth` guard restores the cursor on panic/early-return.
- **Mode 2 (virtual display) is the recommended operating mode.** On a BetterDisplay virtual display the cursor is invisible to the user, RoK's auto-raise is invisible (nothing observes its surface), and the user can keep working on the built-in display. See [docs/setup.md](docs/setup.md) for the walkthrough.
- **Verify the click landed visibly.** 3-check post-click re-validation (WID+PID gone / hidden-Space / frame), sleep `VERIFY_DELAY_MS = 500ms`, re-capture to `rok-capture-post.png`, and pixel-diff the two haystacks over decoded Luma8. Below `PIXEL_DIFF_REJECT_THRESHOLD = 1000` differing pixels → exit 20 (`ClickNotVerified`). Pixel-diff (not file-bytes) because PNG DEFLATE is non-deterministic.
- Decompression-bomb guard on haystack decode (`image::ImageReader` with `Limits { max_image_width: 8192, max_image_height: 8192 }`).
- Structured exit codes (10–11, 13–20; slot 12 left unused after v0.1.5's `RokNotOnPrimary` was deleted) for shell consumers; tracing logs to stderr with `error_kind` + `exit_code` fields.

Not yet built (see [TODOS.md](TODOS.md)):
- **Continuous capture+match+click+verify loop** — v0.1.8 is single-shot. v0.2 wraps the v0.1.4 verify primitive per-click. v0.1.8's in-process SCK capture (~140ms steady-state) already cleared the per-tick latency budget for v0.2; remaining work is **last-position ROI** (search a ±50px window around the last known match) for near-zero per-tick NCC cost, and a **full-frame fallback** for the edge case where the castle ROI misses (e.g., RoK UI layout drift). v0.3 deferred work: SCStream continuous-frame delegate (TODOS D8), cut PNG round-trip (TODOS D9), discover display backing scale (TODOS D10).
- **Server-roundtrip click verify** — 500ms covers UI-local transitions (toggle, dropdown, modal). Server-bound clicks (resource spend, troop dispatch) show a 1-3s spinner; retry-and-poll at multiple delay tiers is queued.
- **Mode 2 lifecycle automation** — v0.1.6 expects the operator to set up the BetterDisplay virtual display manually and drag RoK to it. v0.2 adds RAII lifecycle, panic-safe disconnect, drop-detection, and a state file so a clean exit restores the user's display arrangement.
- **Multi-instance / "farm" support.**

## Quick start

Requires macOS Sequoia 15+ on Apple Silicon, Rust toolchain (auto-pinned via `rust-toolchain.toml`), and Rise of Kingdoms installed from the Mac App Store.

```sh
# First run: macOS will prompt for Screen Recording permission
cargo run --release
```

Expected outputs:
- RoK on built-in display (Mode 1), target visible, click verified → `[INFO] Mode 1 (visible) … captured RoK window … found target … posting HID click pair … after-state verify passed` exit 0, `rok-capture-pre.png` + `rok-capture-post.png` written to cwd. NOTE: clicking on Mode 1 visibly raises RoK to foreground (Catalyst Bridge auto-raise behavior); Mode 2 is recommended for unattended use.
- RoK on virtual display (Mode 2) → same flow as above with `[INFO] Mode 2 (virtual) — RoK is on a non-built-in display …`. Cursor stays put (stealth disassociation); RoK raise is invisible.
- RoK not running → `[ERROR] RoK window not found — is the game running?` exit 10
- RoK window center on no online display → `[ERROR] RoK window center is not on any online display` exit 11
- (exit 12 unused — was `RokNotOnPrimary` in v0.1.5, removed in v0.1.6 when Mode 2 gate opened)
- Screen Recording or Accessibility denied → `[ERROR] Missing macOS permission: …` exit 13
- SCK capture failed → `[ERROR] RoK window capture failed (stage: …, exit code: …)` exit 14. Stage tags from `src/capture.rs` + `src/permissions.rs`: `symlink_refused`, `no_shareable_content`, `window_not_found`, `capture_returned_nil`, `cgimage_decode`, `png_write`
- Target absent / below `MATCH_THRESHOLD` / placeholder-sentinel detected → `[ERROR] target not found in capture (best match below confidence threshold)` exit 15. Sentinel detection logs `WARN placeholder sentinel needle detected (top-left luma [255,0,255,0]); refusing to match`.
- Haystack PNG missing or oversized → `[ERROR] failed to load … image …` exit 16
- Needle strictly larger than haystack in either dim → `[ERROR] target image is too large …` exit 17
- Click pipeline step failed → `[ERROR] synthetic click could not be delivered (reason: …)` exit 18. Reason tags (from `src/click.rs`): `activation_failed`, `probe`, `disassociate`, `source`, `down`, `up`.
- Window vanished, hidden, moved, or click point outside frame → `[ERROR] RoK window state changed or unreachable (reason: …)` exit 19. Reason tags (from `src/window.rs`): `window_id_gone`, `not_visible`, `frame_moved`, `point_outside_frame`. `not_visible` is the actionable case where RoK is running but on a hidden Space or minimized — switch Spaces / unminimize rather than restart.
- Click delivered but screen didn't change → `[ERROR] synthetic click delivered but post-state verify failed (reason: …)` exit 20 (reason tags: `screen_unchanged`, `dim_mismatch`).

Full setup walkthrough: [docs/setup.md](docs/setup.md). Pre-commit hook install instructions are in there too.

## Repo layout

```
src/                     v0.1.7 Rust source (9 modules, 148 unit + integration tests)
assets/targets/          embedded matcher needles (placeholder until first real RoK crop)
docs/setup.md            user-facing setup guide
docs/rok_rust_bot_research.md   pre-implementation architecture research
docs/cargo_dependency_audit.md  pre-implementation dep pin audit
spikes/                  verification spikes that validated load-bearing premises
TODOS.md                 deferred work, organized by priority
CLAUDE.md                project instructions for Claude Code agent sessions
```

## Build, test, lint

```sh
cargo build --release --locked
cargo test --locked                                                  # 148 passing
cargo clippy --all-targets --all-features --locked -- -D warnings    # mbrain-style strict
cargo fmt --all -- --check
```

## Design notes worth knowing

- **CG-only coordinate space.** Window enumeration and display classification both use Quartz coords (`kCGWindowBounds` + `CGDisplayBounds`). Mixing in NSScreen would silently misclassify on multi-display setups where displays sit at negative origins.
- **Catalyst Bridge AX is positionless for game UI.** RoK exposes its entire game canvas as a single `AXGenericElement` whose `AXActivationPoint` (read-only) sits at the canvas center. v0.1.5 shipped `AXUIElementPerformAction(kAXPressAction)` as the click path and hit a structural bug: every press fired at the canvas center, not the caller's coords. v0.1.6 reverted to `CGEvent::post(HID)` with explicit `osascript` activation, which is coord-targeted and works in both modes. See TODOS.md for the full 6-path investigation.
- **Auto-raise on synthetic UITouch.** Catalyst's UIKit-on-Mac translation layer auto-raises any window receiving a touch — iOS apps don't have a "stay in background while receiving touch" concept. In Mode 1 this means RoK pops to the foreground per click; the user can't do other work in parallel. In Mode 2 the raise is invisible (RoK lives on a display nothing observes), which is the structural reason Mode 2 exists.
- **Cursor stealth on every click.** `src/click.rs` probes the user's cursor position, disassociates the visible cursor before the HID tap pair, and warps + reassociates after. A RAII `CursorStealth::Drop` handles cleanup on panic so the user's cursor never gets stuck. Necessary even on Mode 2 because the HID tap targets virtual-display coords; without stealth the user's visible cursor would jump off-screen and back.
- **Mode 2 lifecycle is still manual.** v0.1.6 expects the operator to attach a BetterDisplay virtual display and drag RoK to it by hand. The full lifecycle design (RAII disconnect, panic-safe cleanup, drop-detection, state file) is deferred to v0.2.
- **Bundle-ID anti-spoof.** Owner name + window title alone are spoofable by any local process. Once gameplay automation lands, that's a real hijack vector. v0.1 already gates on `NSRunningApplication.bundleIdentifier().starts_with("com.rok.ios.")`.

## License

MIT. Not currently published to crates.io (`publish = false` in Cargo.toml).
