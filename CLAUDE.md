# rok-bot

Rust-based macOS automation experiment for Rise of Kingdoms on Apple Silicon. Personal/learning project — see `README.md` for the public-facing intro, `docs/rok_rust_bot_research.md` for the architecture research, and `docs/cargo_dependency_audit.md` for the verified dependency pins.

## Project status

**v0.1.5 — Mode 1 (visible) end-to-end pipeline with Accessibility-API click delivery.** Find the RoK main window via `CGWindowListCopyWindowInfo` + bundle-ID anti-spoof (`com.rok.ios.*` prefix), with a v0.1.5 fallback to `kCGWindowListOptionAll` when not on a displayed Space — surfaces `WindowChanged { REASON_NOT_VISIBLE }` (exit 19) for "running but hidden" vs `WindowNotFound` (exit 10) for "not running" so the operator knows whether to switch Spaces or launch RoK. Classify which display the window lives on (built-in via `CGDisplayIsBuiltin` → `Mode::Visible`, anything else → `Mode::Virtual` → exit 12). Capture the window via `screencapture -l <wid>` CLI. Locate a known UI element via `imageproc::match_template_parallel` (NCC sliding window, `MATCH_THRESHOLD = 0.85`). TOCTOU-validate the window at the click site via the v0.1.5 4-check pipeline: WID+PID present in `kCGWindowListOptionAll` (catches WID reuse), WID+PID present in `OnScreenOnly` (catches hidden-Space drift mid-flow), frame within tolerance, click point inside expected frame. The validation runs BEFORE the Accessibility hard check so a hidden-Space exit-19 doesn't first prompt the operator to grant AX permission. Deliver the click via `AXUIElementPerformAction(kAXPressAction)` in `src/ax.rs` — `AXUIElementCreateApplication(pid)` → `AXUIElementSetMessagingTimeout(2.0s)` → `AXUIElementCopyElementAtPosition(x, y)` → `AXUIElementPerformAction("AXPress")`. Drops the CGEvent/HID-tap path because `CGEventPostToPid` is silently dropped for Catalyst Bridge apps like RoK (p4-spike). AX press delivers regardless of z-order overlap (p5-spike). Sleep `VERIFY_DELAY_MS = 500ms`, re-validate window presence (post-click 3-check: gone + hidden-Space + frame; topmost dropped because AX delivers through overlap and a modal-on-top is legitimate), re-capture to `rok-capture-post.png`, and verify via pixel-diff over decoded Luma8 against `PIXEL_DIFF_REJECT_THRESHOLD = 1000` differing pixels. Below the threshold → exit 20 (`ClickNotVerified`). Preflights: Screen Recording TCC at boot (with first-install `request()` fallback) + Accessibility TCC right before the click. Haystack decode goes through `image::ImageReader` with `Limits` (8192×8192 cap) to refuse decompression-bomb PNGs. The placeholder needle's structural sentinel pattern (`matcher::needle_has_placeholder_sentinel`) still fail-closes BEFORE NCC runs as a safety brake — preserved across the v0.1.5 click-path swap. Exit codes: 10-20. Next milestone is v0.2 continuous Mode 2 (drop the `Mode::Virtual` gate, loop capture+match+click+verify against RoK on a BetterDisplay virtual display).

## Project structure

```
src/
├── main.rs          boot wiring + tracing init + structured exit codes
├── error.rs         BotError taxonomy (11 variants, exit codes 10-20)
├── permissions.rs   TCC Screen Recording + Accessibility preflight + first-run request fallback
├── window.rs        CGWindowList wrapper, owner+title filter, bundle-ID anti-spoof, 4-check
│                    pre-click + 3-check post-click TOCTOU validators (v0.1.5)
├── display.rs       CG-only Mode detection, mode_to_result mapping
├── capture.rs       screencapture CLI wrapper for window screenshot (v0.1.1)
├── matcher.rs       NCC template matching via imageproc + placeholder-sentinel safety brake (v0.1.2 + v0.1.4)
├── ax.rs            macOS Accessibility API click delivery: AXUIElementPerformAction(kAXPressAction)
│                    with RAII AxRef + 2s messaging timeout (v0.1.5)
├── click.rs         thin wrapper around ax::press_at (v0.1.5; pre-v0.1.5 CGEventPost path in git history)
└── verify.rs        after-state pixel-diff verification over decoded Luma8 (v0.1.4)
assets/targets/
└── city-button.png  embedded target needle (placeholder with sentinel pattern until first real RoK crop)
docs/
├── setup.md                    user-facing setup guide (Mode 1 today, Mode 2 in v0.2)
├── cargo_dependency_audit.md   pre-implementation pin audit (superseded by Cargo.toml)
└── rok_rust_bot_research.md    architecture research (pre-implementation)
spikes/
├── p2-spike/        screen capture verification (uses Apple's screencapture CLI)
├── p3-spike/        Rust CGEvent + CGEventPost path verification (v0.1.3 Phase 0)
├── p4-spike/        CGEventPostToPid → dead for Catalyst Bridge apps (forced v0.1.5 AX switch)
├── p5-spike/        AXUIElementPerformAction(kAXPressAction) → works on RoK (v0.1.5 click path)
└── p7-spike/        Premise 7 verification (RoK auto-migrates across BD reconnect)
TODOS.md             deferred work, organized by priority
```

## Build, test, lint

```sh
cargo build --release --locked
cargo test --locked                                                   # 140 tests as of v0.1.5
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
