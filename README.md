# rok-bot

Rust-based macOS automation experiment for **Rise of Kingdoms** on Apple Silicon. Personal/learning project. Public so others can read the design choices, not because it's polished or supported.

## Status: v0.1 — Mode 1 (visible) only

What works today:
- Find the RoK main window via `CGWindowListCopyWindowInfo` (filtered by `owner == title == "RiseOfKingdoms"` plus a bundle-ID anti-spoof check against `com.rok.ios.*`).
- Classify which display it's on via `CGDisplayIsBuiltin` (the laptop's Retina panel specifically — NOT the menu-bar display, which the user can move).
- Screen Recording TCC preflight with first-install `request()` fallback so brand-new Macs aren't trapped in a permission dead-end.
- Structured exit codes (10–13) for shell consumers; tracing logs to stderr with `error_kind` + `exit_code` fields.

What's not yet built (see [TODOS.md](TODOS.md)):
- Mode 2 (background on a BetterDisplay virtual display) — designed, deferred to v0.2.
- Capture, target-image matching, click synthesis. v0.1 finds the window and exits.
- Multi-instance / "farm" support.

## Quick start

Requires macOS Sequoia 15+ on Apple Silicon, Rust toolchain (auto-pinned via `rust-toolchain.toml`), and Rise of Kingdoms installed from the Mac App Store.

```sh
# First run: macOS will prompt for Screen Recording permission
cargo run --release
```

Expected outputs:
- RoK on built-in display → `[INFO] Mode 1 (visible) — RoK is on the built-in display.` exit 0
- RoK on virtual / external display → `[ERROR] RoK is not on the primary display. v0.1 supports Mode 1 (visible) only.` exit 12
- RoK not running → `[ERROR] RoK window not found — is the game running?` exit 10
- Screen Recording denied → `[ERROR] Missing macOS permission: Screen Recording.` exit 13

Full setup walkthrough: [docs/setup.md](docs/setup.md). Pre-commit hook install instructions are in there too.

## Repo layout

```
src/                     v0.1 Rust source (5 modules, 35 unit tests)
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
cargo test --locked                                                  # 35 passing
cargo clippy --all-targets --all-features --locked -- -D warnings    # mbrain-style strict
cargo fmt --all -- --check
```

## Design notes worth knowing

- **CG-only coordinate space.** Window enumeration and display classification both use Quartz coords (`kCGWindowBounds` + `CGDisplayBounds`). Mixing in NSScreen would silently misclassify on multi-display setups where displays sit at negative origins. The single `#![allow(unsafe_code)]` in `src/window.rs` is scoped to the FFI surface for `CGRectMakeWithDictionaryRepresentation` and CFString constant reads.
- **Mode 2 is harder than it looks.** The shipped Mode 2 design (in `~/.gstack/projects/`) covers BetterDisplay virtual-display lifecycle, panic-safe disconnect, drop-detection thread, state-file persistence to disambiguate "user wanted Mode 2 cold-path" from "user wanted Mode 1." None of that exists in v0.1.
- **Bundle-ID anti-spoof.** Owner name + window title alone are spoofable by any local process. Once gameplay automation lands, that's a real hijack vector. v0.1 already gates on `NSRunningApplication.bundleIdentifier().starts_with("com.rok.ios.")`.

## License

MIT. Not currently published to crates.io (`publish = false` in Cargo.toml).
