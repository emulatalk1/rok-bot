# rok-bot

Rust-based macOS automation experiment for Rise of Kingdoms on Apple Silicon. Personal/learning project — see `README.md` for the public-facing intro, `docs/rok_rust_bot_research.md` for the architecture research, and `docs/cargo_dependency_audit.md` for the verified dependency pins.

## Project status

**v0.1.1 — Mode 1 (visible) + window capture.** Find the RoK main window via `CGWindowListCopyWindowInfo`, classify which display it lives on (built-in via `CGDisplayIsBuiltin` → `Mode::Visible`, anything else → `Mode::Virtual`), capture the window to `rok-capture.png` via `screencapture -l <wid>` CLI, exit 0. Mode::Virtual exits `RokNotOnPrimary` (12). Screen Recording preflight runs first (with first-install `request()` fallback). Bundle-ID anti-spoof check (`com.rok.ios.*` prefix) on the matched window. Exit codes: 10-14. No clicks, no template matching, no Mode 2 lifecycle yet — those are the next sub-milestones (v0.1.2 match, v0.1.3 click + Accessibility preflight, v0.1.4 verify).

## Project structure

```
src/
├── main.rs          boot wiring + tracing init + structured exit codes
├── error.rs         BotError taxonomy (5 variants, exit codes 10-14)
├── permissions.rs   TCC Screen Recording preflight + first-run request fallback
├── window.rs        CGWindowList wrapper, owner+title filter, bundle-ID anti-spoof
├── display.rs       CG-only Mode detection, mode_to_result mapping
└── capture.rs       screencapture CLI wrapper for window screenshot (v0.1.1)
docs/
├── setup.md                    user-facing setup guide (Mode 1 today, Mode 2 in v0.2)
├── cargo_dependency_audit.md   pre-implementation pin audit (now partially superseded by Cargo.toml)
└── rok_rust_bot_research.md    architecture research (pre-implementation)
spikes/
├── p2-spike/        screen capture verification (uses Apple's screencapture CLI)
└── p7-spike/        Premise 7 verification (RoK auto-migrates across BD reconnect)
TODOS.md             deferred work, organized by priority
```

## Build, test, lint

```sh
cargo build --release --locked
cargo test --locked                                                   # 41 tests as of v0.1.1
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
