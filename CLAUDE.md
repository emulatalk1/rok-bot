# rok-bot

Rust-based macOS automation experiment for Rise of Kingdoms on Apple Silicon. Personal/learning project — see `README.md` for the public-facing intro, `docs/rok_rust_bot_research.md` for the architecture research, and `docs/cargo_dependency_audit.md` for the verified dependency pins.

## Project status

**v0.1.8 — In-process ScreenCaptureKit replaces the v0.1.x `screencapture` CLI subprocess.** Capture via `SCScreenshotManager.captureImageWithFilter` from `objc2-screen-capture-kit` 0.3.x (~140 ms steady-state per capture vs ~280-1400 ms via the CLI shellout; live-confirmed Mode 2 BD virtual display: 191 ms cold + 126 ms cache-hit). New `src/cg_bootstrap.rs` calls `NSApplicationLoad()` from every SCK entrypoint (T2 from `/plan-eng-review`: `permissions::check_sck_grant`, `window::find_rok_window`, `capture::capture_window`) so a CLI Rust binary doesn't trip `CGS_REQUIRE_INIT` when `SCStreamConfiguration::new` runs. Discovery moved from `CGWindowListCopyWindowInfo` to `SCShareableContent.windows` filtered by `SCWindow.title == "RiseOfKingdoms"` AND `SCWindow.owningApplication.bundleIdentifier` starting with `com.rok.ios.`; CGWindow enumeration retained only for the `OnScreenOnly` membership check that distinguishes hidden-Space (`exit 19 not_visible`) from window-gone (`exit 10 WindowNotFound`). Threading model per T1: spike's main-thread `DispatchSemaphore` pattern verbatim BUT explicit timeouts (5s capture, 10s shareable-content) replacing `DispatchTime::FOREVER`, mapping to `CaptureFailed { stage }`. Per-call slot is `Arc<ImageSlot>` / `Arc<ContentSlot>` (newtype + `unsafe Send + Sync` because objc2's `Retained<T>` doesn't auto-derive Send for SCShareableContent/CGImage despite Apple-docs thread-safety + atomic refcounting); the Arc keeps the slot alive past a timeout so SCK's late-firing block can't dereference a freed stack-local — closes a UAF that pre-landing `/review` security specialist + codex caught in the spike pattern. PNG write via `O_NOFOLLOW + O_EXCL` atomic open in `open_capture_output_safely`: try open; on `EEXIST` lstat to distinguish symlink (refuse with `STAGE_SYMLINK_REFUSED`) vs regular file (unlink + retry, race-free against any planted symlink because the next open is still `O_NOFOLLOW + O_EXCL`); replaces v0.1.x's `symlink_metadata` pre-check which had a ~141 ms-5 s TOCTOU window. SCShareableContent cache (D2/T3): `OnceLock<ContentCache>` with `invalidate_shareable_content_cache` on `STAGE_WINDOW_NOT_FOUND` so a relaunched RoK gets a fresh enumeration; `find_rok_window` skips the redundant invalidate+refetch when the first miss came from a fresh fetch (cold-path optimization caught by `/review`). `CaptureFailed { stage: &'static str, exit_code: Option<i32> }` (D4) — six documented stages: `symlink_refused`, `no_shareable_content`, `window_not_found`, `capture_returned_nil`, `cgimage_decode`, `png_write`. Validator (D1) preserves v0.1.5/v0.1.6 reason-tag taxonomy: 3 logical checks via 4-way CGWindow membership lookup (in `All` → distinguishes `wid_gone` from `not_visible`) + SCK frame-drift check + click-point-in-frame check. v0.1.8 hard-requires macOS 14.0+ (SCScreenshotManager is 14+) and Screen Recording grant on the rok-bot binary itself (per-binary; v0.1.x inherited TCC from parent terminal — UX regression documented in setup.md and the `STAGE_NO_SHAREABLE_CONTENT` boot log line). 149 unit tests + 5 `#[ignore]`'d integration tests (`tests/sck_integration.rs`, requires running RoK + per-binary SR grant). Live-smoke /qa exit 0 on first try with 91% pixel diff (world-view → city-view transition confirmed). Next milestone: v0.2 continuous loop (full BD lifecycle automation, last-position ROI for per-tick near-zero NCC, full-frame fallback when castle ROI returns no match). Deferred: SCStream continuous-frame delegate (TODOS D8), cut PNG round-trip (TODOS D9), discover display backing scale (TODOS D10).

**v0.1.7 — ROI-cropped NCC cuts match latency ~22s → ~440ms (50×) on top of v0.1.6's stealth HID click delivery.** Find the RoK main window via `CGWindowListCopyWindowInfo` + bundle-ID anti-spoof (`com.rok.ios.*` prefix), with a fallback to `kCGWindowListOptionAll` when not on a displayed Space — surfaces `WindowChanged { REASON_NOT_VISIBLE }` (exit 19) for "running but hidden" vs `WindowNotFound` (exit 10) for "not running" so the operator knows whether to switch Spaces or launch RoK. Classify which display the window lives on (`CGDisplayIsBuiltin` → `Mode::Visible`, anything else → `Mode::Virtual`); v0.1.6 lets both modes proceed (the v0.1.5 exit-12 gate was deleted along with `BotError::RokNotOnPrimary`). Capture the window via `screencapture -l <wid>` CLI — works regardless of display visibility or external z-order occluders. Locate a known UI element via `imageproc::match_template_parallel` (NCC sliding window, `MATCH_THRESHOLD = 0.85`) restricted to a castle-button ROI: `find_target_in_castle_roi` crops the haystack to the bottom-left quadrant (`CASTLE_BUTTON_ROI_FRACTION_*` constants: X=0.0, Y=0.75, W=0.20, H=0.25 → `(0..420, 1230..1640)` on a 2102×1640 Retina capture) before NCC, dropping the heatmap from 2.81M to 56K positions. Match coords are restored to full-capture space inside `find_target_impl` so `screen_point` math is unchanged; `roi_offset_x`/`roi_offset_y` surface in the timing log so operators tuning the ROI from the no-match warn always see the full-capture translation. TOCTOU-validate the window at the click site via the v0.1.5 4-check pipeline: WID+PID present in `kCGWindowListOptionAll` (catches WID reuse), WID+PID present in `OnScreenOnly` (catches hidden-Space drift mid-flow), frame within tolerance, click point inside expected frame. The validation runs BEFORE the Accessibility hard check so a hidden-Space exit-19 doesn't first prompt the operator to grant AX permission. Deliver the click via stealth HID tap + `osascript` activation in `src/click.rs`: shell out to `osascript -e 'tell application "System Events" to set frontmost of (first process whose unix id is N) to true'` → 50ms settle → probe user cursor via `CGEvent::new(source).location()` → `CGAssociateMouseAndMouseCursorPosition(false)` to detach visible cursor → `CGEvent::post(HID)` `LeftMouseDown` + 80ms gap + `LeftMouseUp` → `CGDisplay::warp_mouse_cursor_position(saved)` + reassociate. A RAII `CursorStealth` guard ensures reassociation on panic/early-return. The v0.1.5 AX-press path (`src/ax.rs`) was deleted: `AXUIElementPerformAction(kAXPressAction)` is positionless on Catalyst Bridge — every press fires at the canvas-center `AXActivationPoint` (read-only) regardless of (x, y); see TODOS.md P0 for the 6-path Mode 1 click-delivery investigation. Sleep `VERIFY_DELAY_MS = 500ms`, re-validate window presence (post-click 3-check: gone + hidden-Space + frame; topmost dropped because activation guarantees foreground), re-capture to `rok-capture-post.png`, and verify via pixel-diff over decoded Luma8 against `PIXEL_DIFF_REJECT_THRESHOLD = 1000` differing pixels. Below the threshold → exit 20 (`ClickNotVerified`). The post-click diagnostic re-match also runs through the castle ROI for consistency. Preflights: Screen Recording TCC at boot (with first-install `request()` fallback) + Accessibility TCC right before the click — Accessibility is required for `CGEvent::post(HID)` to actually deliver, not just for the deleted AX press. Haystack decode goes through `image::ImageReader` with `Limits` (8192×8192 cap) to refuse decompression-bomb PNGs. The placeholder needle's structural sentinel pattern (`matcher::needle_has_placeholder_sentinel`) still fail-closes BEFORE NCC runs as a safety brake. Exit codes: 10-11, 13-20 (slot 12 left unused after `RokNotOnPrimary` deletion to avoid silent meaning-swap for shell users with stale `case "$?" in 12)` arms). **Mode 2 is the recommended operating mode**: RoK on a BetterDisplay virtual display means the cursor isn't there, the auto-raise is invisible, and the user can keep working on the built-in display. Next milestone is v0.2 continuous loop (`objc2-screen-capture-kit` migration to drop the `screencapture` CLI subprocess, full BD lifecycle automation, last-position ROI for per-tick near-zero NCC, full-frame fallback when castle ROI returns no match). FFT-NCC is indefinitely deferred — ROI alone hit the v0.2 cadence target.

## Project structure

```
src/
├── main.rs          boot wiring + tracing init + structured exit codes
├── error.rs         BotError taxonomy (10 variants, exit codes 10-11 + 13-20)
├── permissions.rs   TCC Screen Recording + Accessibility preflight + first-run request fallback
├── window.rs        SCK enumeration (SCShareableContent + bundle-ID filter) + CGWindow OnScreenOnly check, RokWindow struct,
│                    Arc<ContentSlot>, OnceLock<ContentCache>, 3-check validators (v0.1.8)
├── display.rs       CG-only Mode detection (classify Visible vs Virtual; both proceed in v0.1.6)
├── cg_bootstrap.rs  NSApplicationLoad WindowServer bootstrap, called from every SCK entrypoint (v0.1.8)
├── capture.rs       in-process SCK capture via SCScreenshotManager + Arc<ImageSlot> + O_NOFOLLOW+O_EXCL safe PNG write (v0.1.8)
├── matcher.rs       ROI-cropped NCC template matching via imageproc + placeholder-sentinel safety brake (v0.1.7)
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
cargo test --locked                                                   # 149 unit tests + 5 #[ignore]'d integration tests as of v0.1.8
cargo test --test sck_integration -- --ignored                        # live integration tests (require running RoK + per-binary SR grant)
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
