# rok-bot

Rust-based macOS automation experiment for **Rise of Kingdoms** on Apple Silicon. Personal/learning project. Public so others can read the design choices, not because it's polished or supported.

## Status: v0.1 — Mode 1 (visible) end-to-end click pipeline + after-state verification

What works today (v0.1.4):
- Find the RoK main window via `CGWindowListCopyWindowInfo` (filtered by `owner == title == "RiseOfKingdoms"` plus a bundle-ID anti-spoof check against `com.rok.ios.*`).
- Classify which display it's on via `CGDisplayIsBuiltin` (the laptop's Retina panel specifically — NOT the menu-bar display, which the user can move).
- Screen Recording **and** Accessibility TCC preflights with first-install `request()` fallback so brand-new Macs aren't trapped in a permission dead-end.
- **Capture the RoK window to `rok-capture-pre.png`** (Mode 1 only) via Apple's `screencapture -l <wid> -x -o` CLI — silent, no shadow, ~50-100ms per call.
- **Template-match a known UI element** via `imageproc::match_template_parallel` (rayon-parallel NCC sliding window). Embedded needle, configurable `MATCH_THRESHOLD` (default `0.85`). Sub-second on M-series for the standard 2102×1640 Retina haystack.
- **Placeholder-sentinel safety brake.** The shipped needle is a structural placeholder (`[255, 0, 255, 0]` top-left luma + xorshift32 noise body); `matcher::needle_has_placeholder_sentinel` fail-closes BEFORE NCC runs so the bot can never synthetically click against a falsely-matched placeholder. /qa caught a real placeholder false-match at 0.93 NCC; this brake prevents the class.
- **TOCTOU-validate the window at the click site** (WID + frame + topmost) before posting any synthetic input.
- **Synthesize a left-click via `CGEvent::post(kCGHIDEventTap, ...)`** at HID event-tap level — the same level the OS uses for real mouse input.
- **Verify the click landed visibly.** Re-validate window presence (WID + frame — no topmost check; a click may legitimately spawn a modal), sleep `VERIFY_DELAY_MS = 500ms`, re-capture to `rok-capture-post.png`, and pixel-diff the two haystacks over decoded Luma8. Below `PIXEL_DIFF_REJECT_THRESHOLD = 1000` differing pixels → exit 20 (`ClickNotVerified`). Pixel-diff (not file-bytes) because PNG DEFLATE is non-deterministic.
- Decompression-bomb guard on haystack decode (`image::ImageReader` with `Limits { max_image_width: 8192, max_image_height: 8192 }`).
- Structured exit codes (10–20) for shell consumers; tracing logs to stderr with `error_kind` + `exit_code` fields.

Not yet built (see [TODOS.md](TODOS.md)):
- **Mode 2** (background on a BetterDisplay virtual display) — designed, deferred to v0.2. The shipped pipeline still exits 12 when RoK is on any non-built-in display.
- **Continuous capture+match+click+verify loop** — v0.1 is single-shot. v0.2 wraps the v0.1.4 verify primitive per-click and migrates from the `screencapture` CLI to `objc2-screen-capture-kit` for zero subprocess overhead. NCC → FFT-NCC migration becomes load-bearing at that point (verify roughly doubles per-cycle matcher cost).
- **Server-roundtrip click verify** — 500ms covers UI-local transitions (toggle, dropdown, modal). Server-bound clicks (resource spend, troop dispatch) show a 1-3s spinner; retry-and-poll at multiple delay tiers is queued.
- **Real RoK city-button crop.** The needle today is a sentinel-carrying placeholder; the matcher refuses to operate against it. Replacing the placeholder is the biggest gate to running against live RoK.
- **Multi-instance / "farm" support.**

## Quick start

Requires macOS Sequoia 15+ on Apple Silicon, Rust toolchain (auto-pinned via `rust-toolchain.toml`), and Rise of Kingdoms installed from the Mac App Store.

```sh
# First run: macOS will prompt for Screen Recording permission
cargo run --release
```

Expected outputs:
- RoK on built-in display, target visible, click verified → `[INFO] Mode 1 (visible) … captured RoK window … found target x=… y=… score=… click verified (pixel_diff=…)` exit 0, `rok-capture-pre.png` + `rok-capture-post.png` written to cwd.
- RoK not running → `[ERROR] RoK window not found — is the game running?` exit 10
- RoK on virtual / external display → `[ERROR] RoK is not on the primary display. v0.1 supports Mode 1 (visible) only.` exit 12
- Screen Recording or Accessibility denied → `[ERROR] Missing macOS permission: …` exit 13
- `screencapture` failed (rare) → `[ERROR] screencapture failed (exit code: …)` exit 14
- Target absent / below `MATCH_THRESHOLD` / placeholder-sentinel detected → `[ERROR] target not found in capture (best match below confidence threshold)` exit 15. Sentinel detection logs `WARN placeholder sentinel needle detected (top-left luma [255,0,255,0]); refusing to match`.
- Haystack PNG missing or oversized → `[ERROR] failed to load … image …` exit 16
- Needle strictly larger than haystack in either dim → `[ERROR] target image is too large …` exit 17
- `CGEvent::post` failed → `[ERROR] click failed: …` exit 18
- Window vanished or moved between match and click → `[ERROR] window changed: …` exit 19
- Click posted but screen didn't change → `[ERROR] click not verified: …` exit 20 (reason tags: `screen_unchanged`, `dim_mismatch`).

Full setup walkthrough: [docs/setup.md](docs/setup.md). Pre-commit hook install instructions are in there too.

## Repo layout

```
src/                     v0.1 Rust source (9 modules, 136 unit + integration tests)
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
cargo test --locked                                                  # 136 passing (137 in --release)
cargo clippy --all-targets --all-features --locked -- -D warnings    # mbrain-style strict
cargo fmt --all -- --check
```

## Design notes worth knowing

- **CG-only coordinate space.** Window enumeration and display classification both use Quartz coords (`kCGWindowBounds` + `CGDisplayBounds`). Mixing in NSScreen would silently misclassify on multi-display setups where displays sit at negative origins. The single `#![allow(unsafe_code)]` in `src/window.rs` is scoped to the FFI surface for `CGRectMakeWithDictionaryRepresentation` and CFString constant reads.
- **Mode 2 is harder than it looks.** The shipped Mode 2 design (in `~/.gstack/projects/`) covers BetterDisplay virtual-display lifecycle, panic-safe disconnect, drop-detection thread, state-file persistence to disambiguate "user wanted Mode 2 cold-path" from "user wanted Mode 1." None of that exists in v0.1.
- **Bundle-ID anti-spoof.** Owner name + window title alone are spoofable by any local process. Once gameplay automation lands, that's a real hijack vector. v0.1 already gates on `NSRunningApplication.bundleIdentifier().starts_with("com.rok.ios.")`.

## License

MIT. Not currently published to crates.io (`publish = false` in Cargo.toml).
