# rok-bot

Rust-based macOS automation experiment for **Rise of Kingdoms** on Apple Silicon. Personal/learning project. Public so others can read the design choices, not because it's polished or supported.

## Status: v0.1.6 — Mode 1 + Mode 2 end-to-end pipeline with stealth HID click delivery

What works today (v0.1.6):
- Find the RoK main window via `CGWindowListCopyWindowInfo` (filtered by `owner == title == "RiseOfKingdoms"` plus a bundle-ID anti-spoof check against `com.rok.ios.*`).
- **Hidden-Space distinction at boot.** If RoK is running but its window isn't on a currently-displayed Space (another app went fullscreen, RoK is minimized to Dock), exit 19 `WindowChanged { not_visible }` with an actionable message — distinct from exit 10 `WindowNotFound` (RoK not running). Detected by falling back to `kCGWindowListOptionAll` when `OnScreenOnly` doesn't match.
- Classify which display RoK is on via `CGDisplayIsBuiltin`. Both built-in (`Mode::Visible`) and non-built-in (`Mode::Virtual` — BetterDisplay virtual, external monitor, Sidecar) proceed through the same pipeline. v0.1.5's exit-12 Mode 2 gate was dropped in v0.1.6.
- Screen Recording **and** Accessibility TCC preflights with first-install `request()` fallback so brand-new Macs aren't trapped in a permission dead-end.
- **Capture the RoK window to `rok-capture-pre.png`** via Apple's `screencapture -l <wid> -x -o` CLI — silent, no shadow, ~50-100ms per call. Captures RoK's backing store regardless of on-screen overlays OR which display the window lives on.
- **Template-match a known UI element** via `imageproc::match_template_parallel` (rayon-parallel NCC sliding window). Embedded needle, configurable `MATCH_THRESHOLD` (default `0.85`). ~21s on M-series for the standard 2102×1640 Retina haystack (v0.2 FFT-NCC migration tracked in TODOS).
- **Placeholder-sentinel safety brake.** The shipped needle has a structural sentinel (`[255, 0, 255, 0]` top-left luma + xorshift32 noise body); `matcher::needle_has_placeholder_sentinel` fail-closes BEFORE NCC runs so the bot can never synthetically click against a falsely-matched placeholder. /qa caught a real placeholder false-match at 0.93 NCC; this brake prevents the class.
- **4-check pre-click TOCTOU validation** (v0.1.5, anchored on WID+PID, runs BEFORE the AX TCC prompt so a hidden-Space exit doesn't waste an Accessibility grant). Maps to four `WindowChanged` reasons: `window_id_gone`, `not_visible`, `frame_moved`, `point_outside_frame`.
- **Click delivery via stealth HID tap + osascript activation** (v0.1.6 — reverts v0.1.5 AX press, which was structurally broken on Mac Catalyst Bridge; see TODOS for the full 6-path investigation). Sequence: `osascript` activate RoK by pid via System Events → 50ms settle → probe + disassociate cursor → `CGEvent::post(HID)` `LeftMouseDown` → 80ms gap → `LeftMouseUp` → warp logical cursor back → reassociate. RAII `CursorStealth` guard restores the cursor on panic/early-return.
- **Mode 2 (virtual display) is the recommended operating mode.** On a BetterDisplay virtual display the cursor is invisible to the user, RoK's auto-raise is invisible (nothing observes its surface), and the user can keep working on the built-in display. See [docs/setup.md](docs/setup.md) for the walkthrough.
- **Verify the click landed visibly.** 3-check post-click re-validation (WID+PID gone / hidden-Space / frame), sleep `VERIFY_DELAY_MS = 500ms`, re-capture to `rok-capture-post.png`, and pixel-diff the two haystacks over decoded Luma8. Below `PIXEL_DIFF_REJECT_THRESHOLD = 1000` differing pixels → exit 20 (`ClickNotVerified`). Pixel-diff (not file-bytes) because PNG DEFLATE is non-deterministic.
- Decompression-bomb guard on haystack decode (`image::ImageReader` with `Limits { max_image_width: 8192, max_image_height: 8192 }`).
- Structured exit codes (10–11, 13–20; slot 12 left unused after v0.1.5's `RokNotOnPrimary` was deleted) for shell consumers; tracing logs to stderr with `error_kind` + `exit_code` fields.

Not yet built (see [TODOS.md](TODOS.md)):
- **Continuous capture+match+click+verify loop** — v0.1.6 is single-shot. v0.2 wraps the v0.1.4 verify primitive per-click and migrates from the `screencapture` CLI to `objc2-screen-capture-kit` for zero subprocess overhead. NCC → FFT-NCC migration becomes load-bearing at that point (the current ~21s match latency is the v0.2 blocker).
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
- `screencapture` failed (rare) → `[ERROR] screencapture failed (exit code: …)` exit 14
- Target absent / below `MATCH_THRESHOLD` / placeholder-sentinel detected → `[ERROR] target not found in capture (best match below confidence threshold)` exit 15. Sentinel detection logs `WARN placeholder sentinel needle detected (top-left luma [255,0,255,0]); refusing to match`.
- Haystack PNG missing or oversized → `[ERROR] failed to load … image …` exit 16
- Needle strictly larger than haystack in either dim → `[ERROR] target image is too large …` exit 17
- Click pipeline step failed → `[ERROR] synthetic click could not be delivered (reason: …)` exit 18. Reason tags (from `src/click.rs`): `activation_failed`, `probe`, `disassociate`, `source`, `down`, `up`.
- Window vanished, hidden, moved, or click point outside frame → `[ERROR] RoK window state changed or unreachable (reason: …)` exit 19. Reason tags (from `src/window.rs`): `window_id_gone`, `not_visible`, `frame_moved`, `point_outside_frame`. `not_visible` is the actionable case where RoK is running but on a hidden Space or minimized — switch Spaces / unminimize rather than restart.
- Click delivered but screen didn't change → `[ERROR] synthetic click delivered but post-state verify failed (reason: …)` exit 20 (reason tags: `screen_unchanged`, `dim_mismatch`).

Full setup walkthrough: [docs/setup.md](docs/setup.md). Pre-commit hook install instructions are in there too.

## Repo layout

```
src/                     v0.1.6 Rust source (9 modules, 136 unit + integration tests)
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
cargo test --locked                                                  # 136 passing
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
