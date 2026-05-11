# rok-bot

Rust-based macOS automation experiment for Rise of Kingdoms on Apple Silicon. Personal/learning project — see `README.md` for the public-facing intro, `docs/rok_rust_bot_research.md` for the architecture research, and `docs/cargo_dependency_audit.md` for the verified dependency pins.

## Project status

**v0.1.4 — Mode 1 (visible) end-to-end click pipeline with after-state verification.** Find the RoK main window via `CGWindowListCopyWindowInfo` + bundle-ID anti-spoof (`com.rok.ios.*` prefix), classify which display it lives on (built-in via `CGDisplayIsBuiltin` → `Mode::Visible`, anything else → `Mode::Virtual` → exit 12), capture the window via `screencapture -l <wid>` CLI, locate a known UI element via `imageproc::match_template_parallel` (NCC sliding window, `MATCH_THRESHOLD = 0.85`), TOCTOU-validate the window at the click site (WID + frame + topmost), post a synthetic left-click at HID event-tap level via `CGEventPost(kCGHIDEventTap, ...)`, sleep `VERIFY_DELAY_MS = 500ms`, re-validate window presence (WID + frame, no topmost — a click may legitimately spawn a modal), re-capture to `rok-capture-post.png`, and verify the click changed the screen via pixel-diff over decoded Luma8 against `PIXEL_DIFF_REJECT_THRESHOLD = 1000` differing pixels. Below the threshold → exit 20 (`ClickNotVerified`). Preflights run before any of this: Screen Recording TCC (with first-install `request()` fallback), Accessibility TCC (required for `CGEventPost`). Haystack decode goes through `image::ImageReader` with `Limits` (8192×8192 cap) to refuse decompression-bomb PNGs. The placeholder needle in `assets/targets/city-button.png` carries a structural sentinel pattern (`[255, 0, 255, 0]` top-left luma + xorshift32 noise body); `matcher::needle_has_placeholder_sentinel` fail-closes BEFORE NCC runs so the bot can never synthetically click against a falsely-matched placeholder — a live-QA-caught safety brake (see `d2ce910`). Exit codes: 10-20. Next milestone is v0.2 continuous Mode 2 (drop the `Mode::Virtual` gate, loop capture+match+click+verify against RoK on a BetterDisplay virtual display).

## Project structure

```
src/
├── main.rs          boot wiring + tracing init + structured exit codes
├── error.rs         BotError taxonomy (11 variants, exit codes 10-20)
├── permissions.rs   TCC Screen Recording + Accessibility preflight + first-run request fallback
├── window.rs        CGWindowList wrapper, owner+title filter, bundle-ID anti-spoof, TOCTOU validators (v0.1.3 click-site + v0.1.4 post-click)
├── display.rs       CG-only Mode detection, mode_to_result mapping
├── capture.rs       screencapture CLI wrapper for window screenshot (v0.1.1)
├── matcher.rs       NCC template matching via imageproc + placeholder-sentinel safety brake (v0.1.2 + v0.1.4)
├── click.rs         synthetic left-click via CGEventPost at HID event-tap level (v0.1.3)
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
└── p7-spike/        Premise 7 verification (RoK auto-migrates across BD reconnect)
TODOS.md             deferred work, organized by priority
```

## Build, test, lint

```sh
cargo build --release --locked
cargo test --locked                                                   # 136 tests as of v0.1.4 (137 in --release)
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
