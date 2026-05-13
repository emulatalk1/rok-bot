# rok-bot

Rust-based macOS automation experiment for Rise of Kingdoms on Apple Silicon. Personal/learning project — see `README.md` for the public-facing intro, `docs/rok_rust_bot_research.md` for the architecture research, and `docs/cargo_dependency_audit.md` for the verified dependency pins.

## Project status

**v0.1.6 — Mode 1 (visible) + Mode 2 (virtual display) end-to-end pipeline with stealth HID click delivery.** Find the RoK main window via `CGWindowListCopyWindowInfo` + bundle-ID anti-spoof (`com.rok.ios.*` prefix), with a fallback to `kCGWindowListOptionAll` when not on a displayed Space — surfaces `WindowChanged { REASON_NOT_VISIBLE }` (exit 19) for "running but hidden" vs `WindowNotFound` (exit 10) for "not running" so the operator knows whether to switch Spaces or launch RoK. Classify which display the window lives on (`CGDisplayIsBuiltin` → `Mode::Visible`, anything else → `Mode::Virtual`); v0.1.6 lets both modes proceed (the v0.1.5 exit-12 gate was deleted along with `BotError::RokNotOnPrimary`). Capture the window via `screencapture -l <wid>` CLI — works regardless of display visibility or external z-order occluders. Locate a known UI element via `imageproc::match_template_parallel` (NCC sliding window, `MATCH_THRESHOLD = 0.85`). TOCTOU-validate the window at the click site via the v0.1.5 4-check pipeline: WID+PID present in `kCGWindowListOptionAll` (catches WID reuse), WID+PID present in `OnScreenOnly` (catches hidden-Space drift mid-flow), frame within tolerance, click point inside expected frame. The validation runs BEFORE the Accessibility hard check so a hidden-Space exit-19 doesn't first prompt the operator to grant AX permission. Deliver the click via stealth HID tap + `osascript` activation in `src/click.rs`: shell out to `osascript -e 'tell application "System Events" to set frontmost of (first process whose unix id is N) to true'` → 50ms settle → probe user cursor via `CGEvent::new(source).location()` → `CGAssociateMouseAndMouseCursorPosition(false)` to detach visible cursor → `CGEvent::post(HID)` `LeftMouseDown` + 80ms gap + `LeftMouseUp` → `CGDisplay::warp_mouse_cursor_position(saved)` + reassociate. A RAII `CursorStealth` guard ensures reassociation on panic/early-return. The v0.1.5 AX-press path (`src/ax.rs`) was deleted: `AXUIElementPerformAction(kAXPressAction)` is positionless on Catalyst Bridge — every press fires at the canvas-center `AXActivationPoint` (read-only) regardless of (x, y); see TODOS.md P0 for the 6-path Mode 1 click-delivery investigation. Sleep `VERIFY_DELAY_MS = 500ms`, re-validate window presence (post-click 3-check: gone + hidden-Space + frame; topmost dropped because activation guarantees foreground), re-capture to `rok-capture-post.png`, and verify via pixel-diff over decoded Luma8 against `PIXEL_DIFF_REJECT_THRESHOLD = 1000` differing pixels. Below the threshold → exit 20 (`ClickNotVerified`). Preflights: Screen Recording TCC at boot (with first-install `request()` fallback) + Accessibility TCC right before the click — Accessibility is required for `CGEvent::post(HID)` to actually deliver, not just for the deleted AX press. Haystack decode goes through `image::ImageReader` with `Limits` (8192×8192 cap) to refuse decompression-bomb PNGs. The placeholder needle's structural sentinel pattern (`matcher::needle_has_placeholder_sentinel`) still fail-closes BEFORE NCC runs as a safety brake. Exit codes: 10-11, 13-20 (slot 12 left unused after `RokNotOnPrimary` deletion to avoid silent meaning-swap for shell users with stale `case "$?" in 12)` arms). **Mode 2 is the recommended operating mode**: RoK on a BetterDisplay virtual display means the cursor isn't there, the auto-raise is invisible, and the user can keep working on the built-in display. Next milestone is v0.2 continuous loop (FFT-NCC to fix the ~21s match latency, `objc2-screen-capture-kit` migration, full lifecycle automation).

## Project structure

```
src/
├── main.rs          boot wiring + tracing init + structured exit codes
├── error.rs         BotError taxonomy (10 variants, exit codes 10-11 + 13-20)
├── permissions.rs   TCC Screen Recording + Accessibility preflight + first-run request fallback
├── window.rs        CGWindowList wrapper, owner+title filter, bundle-ID anti-spoof, 4-check
│                    pre-click + 3-check post-click TOCTOU validators
├── display.rs       CG-only Mode detection (classify Visible vs Virtual; both proceed in v0.1.6)
├── capture.rs       screencapture CLI wrapper for window screenshot
├── matcher.rs       NCC template matching via imageproc + placeholder-sentinel safety brake
├── click.rs         stealth HID tap + osascript activation + RAII CursorStealth guard (v0.1.6)
└── verify.rs        after-state pixel-diff verification over decoded Luma8
assets/targets/
└── city-button.png  embedded target needle (placeholder with sentinel pattern until first real RoK crop)
docs/
├── setup.md                    user-facing setup guide (Mode 1 + Mode 2 supported)
├── cargo_dependency_audit.md   pre-implementation pin audit (superseded by Cargo.toml)
└── rok_rust_bot_research.md    architecture research (pre-implementation)
spikes/
├── p2-spike/        screen capture verification (uses Apple's screencapture CLI)
├── p3-spike/        Rust CGEvent + CGEventPost path verification
├── p4-spike/        CGEventPostToPid → dead for Catalyst Bridge apps
├── p5-spike/        AX press false-positive + AX tree dump + SkyLight diagnostic modes
│                    (v0.2 BD-with-negative-coord procedure in README)
└── p7-spike/        Premise 7 verification (RoK auto-migrates across BD reconnect)
TODOS.md             deferred work, organized by priority
```

## Build, test, lint

```sh
cargo build --release --locked
cargo test --locked                                                   # 136 tests as of v0.1.6
cargo clippy --all-targets --all-features --locked -- -D warnings     # strict, mbrain-style
cargo fmt --all -- --check
```

Pre-commit hooks in `.pre-commit-config.yaml` wire fmt at commit and clippy + tests at push (see `docs/setup.md` for install).

## Skill routing

When the user's request matches an available skill, invoke it via the Skill tool. When in doubt, invoke the skill.

Key routing rules:
- Product ideas/brainstorming → invoke /office-hours
- Strategy/scope → invoke /plan-ceo-review
- Architecture → invoke /plan-eng-review
- Design system/plan review → invoke /design-consultation or /plan-design-review
- Full review pipeline → invoke /autoplan
- Bugs/errors → invoke /investigate
- QA/testing site behavior → invoke /qa or /qa-only
- Code review/diff check → invoke /review
- Visual polish → invoke /design-review
- Ship/deploy/PR → invoke /ship or /land-and-deploy
- Save progress → invoke /context-save
- Resume context → invoke /context-restore
