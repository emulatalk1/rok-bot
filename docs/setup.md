# Setup

> **v0.2.1 status:** rok-bot runs a **continuous loop** — `capture → match → click → verify` per tick, run-until-Ctrl-C, targeting the state-neutral city↔world toggle. As of v0.2.1 the loop **confirms real city↔world toggles** (both matcher needles ship as real 96×96 tight inner-glyph crops — see "Matcher needles" below — so a multi-tick run exits 0 with confirmed toggles, no operator crop step needed). `ROK_BOT_MAX_TICKS` caps the run (default 100); `ROK_BOT_MAX_TICKS=1` reproduces the v0.1.x one-shot. Both **Mode 1 (visible)** and **Mode 2 (virtual display)** work; Mode 2 is recommended — RoK lives on a BetterDisplay virtual display, the bot operates invisibly, you keep using the Mac. **macOS 14.0+ required** and **Screen Recording + Accessibility must be granted to the `rok-bot` binary itself** — see "First-run TCC grant" below. v0.2 makes Accessibility a HARD boot check (the loop always clicks). New in v0.2.1: **window-lifecycle resilience** — the loop survives a RoK crash + relaunch mid-loop, a hidden window / screensaver, and a BetterDisplay reconnect race at boot, re-discovering the window via a recovery sub-loop instead of aborting. Mode 2 lifecycle automation (shortcuts that connect/disconnect the virtual display) is still deferred.

## First-run TCC grant (v0.1.8 UX regression)

v0.1.x's CLI shellout to `/usr/sbin/screencapture` inherited Screen Recording trust from the parent terminal — granting Terminal/iTerm SR was enough.

v0.1.8's in-process SCK requires the `rok-bot` binary itself to be granted SR. On first run after upgrading you'll see:

```
[ERROR] RoK window capture failed (stage: no_shareable_content, exit code: …)
[ERROR] ScreenCaptureKit could not enumerate shareable content. Most likely
        cause: Screen Recording not granted to the rok-bot binary itself.
        Grant in System Settings → Privacy & Security → Screen Recording
        (look for 'rok-bot' in the list)…
```

Grant the binary in **System Settings → Privacy & Security → Screen Recording**, toggle the new `rok-bot` entry on, then re-run. One-time per binary path (re-grant if you `cargo build --release` to a different target).

**v0.2 also hard-requires Accessibility at boot.** v0.1.x peeked Accessibility (warn-only) and only hard-checked it at the click site. The v0.2 loop always clicks, so there is no capture-only mode — `rok-bot` demands Accessibility up front and exits 13 (`PermissionsMissing`) if it's denied. Grant the `rok-bot` binary in **System Settings → Privacy & Security → Accessibility** the same way.


`rok-bot` is designed to run in two modes depending on where Rise of Kingdoms is parked.

| Mode | RoK is on... | Bot behavior | Your machine while bot runs | Status |
|---|---|---|---|---|
| **Mode 1 — Visible** | Built-in display (laptop Retina panel; `CGDisplayIsBuiltin` test, NOT the menu-bar display) | Detect window, capture to PNG, template-match a known target, 4-check TOCTOU-validate the click site (incl. hidden-Space detection), deliver a stealth HID click via `osascript` activation + `CGEvent::post(HID)`, re-capture and pixel-diff to verify the click changed the screen | Each click visibly raises RoK to foreground (Catalyst auto-raise behavior). You see what the bot sees. | **v0.1.6 ✅ shipped** |
| **Mode 2 — Background (recommended)** | A non-built-in display (BetterDisplay virtual, HDMI dummy plug, iPad via Sidecar, or a real second monitor) | Same pipeline; same click path; same captures. RoK's auto-raise lands on a display nothing observes. | You keep using the Mac normally. Cursor stealth keeps your visible cursor in place during clicks. | **v0.1.6 ✅ shipped (manual BD setup; lifecycle automation deferred to v0.2)** |

The bot auto-detects which mode to use by reading where RoK's window is. No CLI flag, no config file. Drag RoK between displays to switch.

The Mode 2 sections below cover setup with **BetterDisplay** (free, no real hardware required). If you already have a second monitor, HDMI dummy plug, or iPad via Sidecar, you can skip the shortcut authoring entirely — see [the alternatives section](#optional-hdmi-dummy-plug-or-ipad-sidecar-instead-of-betterdisplay) at the bottom.

---

## Mode 1 — zero setup (v0.1.6 shipped, v0.1.8 SCK migration)

Make sure RoK is on your built-in display, then run the bot.

```sh
cargo run --release
```

> **Mode 1 caveat:** Catalyst Bridge auto-raises RoK to the foreground on every synthetic UITouch (this is the structural reason Mode 2 exists). You'll see RoK pop forward each click. For unattended use, prefer Mode 2.

**v0.2 — the bot runs a loop.** `cargo run --release` boots once, then loops `capture → match → click → verify` until Ctrl-C (the SIGINT handler finishes the in-flight tick, then exits 0) or the `ROK_BOT_MAX_TICKS` cap (default 100). `ROK_BOT_MAX_TICKS=1` reproduces the v0.1.x one-shot exactly. The numbered steps below are: 1-4 boot once, 5-11 repeat each tick.

Boot + per-tick sequence (logged to stderr via `tracing`):
1. **Screen Recording preflight.** First run on a fresh Mac triggers macOS's prompt. Exit 13 (`PermissionsMissing`) if denied.
2. **Accessibility hard check (v0.2).** The loop always clicks, so Accessibility is demanded at boot — not the v0.1.x warn-only peek. Exit 13 (`PermissionsMissing`) if denied. Also: a `ctrlc` SIGINT handler is installed so Ctrl-C stops the loop cleanly; if it fails to install, the loop refuses to start (exit 21 `LoopAborted`, reason `signal_handler_install_failed`).
3. **Find the RoK main window.** Filtered by `kCGWindowOwnerName == kCGWindowName == "RiseOfKingdoms"` AND backed by a process whose bundle ID starts with `com.rok.ios.` (anti-spoof gate). Search `kCGWindowListOptionOnScreenOnly` first; if no match, fall back to `kCGWindowListOptionAll`. Three outcomes: found visible → continue. Found in All but not OnScreenOnly → exit 19 `WindowChanged { not_visible }` (RoK is running but on a hidden Space, minimized, or transient state — switch to its Space or unminimize). No match anywhere → exit 10 `WindowNotFound`.
4. **Classify display.** `CGDisplayIsBuiltin` test — built-in (laptop Retina panel) → `Mode::Visible`, anything else → `Mode::Virtual`. v0.1.6 lets both modes proceed; v0.1.5's exit-12 gate was removed. **v0.2.1 boot retry:** steps 3-4 retry up to 3 times (150/400ms backoff, SCK cache invalidated between attempts) on a `WindowScreenUnresolved` error so a BetterDisplay virtual-display reconnect race at boot doesn't fail the run.
5. **Pre-capture liveness probe + capture (v0.2.1).** Each tick first runs `validate_window_present` — a pre-capture liveness probe (logs a `probe_ms` line) that catches a RoK relaunch (`WindowGone`) or a hidden window (`WindowHidden`) before paying the ~140ms capture; either signal drops the loop into the `recover_window` recovery sub-loop (see step 11). Then capture the RoK window to `./rok-capture-pre.png` via in-process `SCScreenshotManager.captureImageWithFilter` (v0.1.8 — replaces the v0.1.x `/usr/sbin/screencapture -l <wid>` CLI shellout). ~191ms cold, ~126ms with `SCShareableContent` cache hit. PNG write via `O_NOFOLLOW + O_EXCL` atomic open (race-free against symlink TOCTOU). Exit 14 (`CaptureFailed { stage: … }`) on failure with one of: `symlink_refused`, `no_shareable_content`, `window_not_found`, `capture_returned_nil`, `cgimage_decode`, `png_write`.
6. **Template-match the target inside the pre-capture.** Decode the haystack PNG via `image::ImageReader` with `Limits` (`max_image_width`/`max_image_height` = 8192) so a malformed or oversized PNG fails fast as `ImageLoadFailed` (exit 16) instead of OOM-ing. `find_best_needle` decodes both embedded needles (`assets/targets/city-button.png`, `assets/targets/world-button.png`) and runs best-of-N — both ship as real 96×96 tight inner-glyph crops as of v0.2.1. The `needle_has_placeholder_sentinel` gate runs first per needle — if a needle carries the placeholder sentinel pattern (`[255, 0, 255, 0]` top-left luma) it is skipped with `WARN placeholder sentinel needle detected; refusing to match` (dormant since v0.2.1 ships both needles real). Then crop the haystack to the castle-button ROI (bottom-left quadrant, 20% × 25% of the capture per `CASTLE_BUTTON_ROI_FRACTION_*` constants in `matcher.rs`) and run `imageproc::match_template_parallel` with `CrossCorrelationNormalized` against `MATCH_THRESHOLD = 0.85`; the winning needle's index is tagged on the `NeedleMatch`. Match coords are restored to full-capture space before returning. Exit 15 (`TargetNotFound`) if no needle matches; exit 17 (`TargetTooLarge`) if a needle is strictly larger than the ROI in either dimension. Current latency (v0.1.7): ~440ms on M-series for a 2102×1640 Retina haystack, down from ~22s in v0.1.6 (50× speedup via ROI cropping — FFT-NCC migration is no longer needed and indefinitely deferred).
7. **4-check pre-click TOCTOU validation.** Anchors on `(WID, PID)` and checks both `OnScreenOnly` and `All` window lists: (a) `(WID, PID)` present in `All` (catches WID reuse), (b) `(WID, PID)` present in `OnScreenOnly` (catches mid-flow hide), (c) frame within tolerance, (d) click point inside expected frame. Maps to `WindowChanged` reasons `window_id_gone` / `not_visible` / `frame_moved` / `point_outside_frame` (all exit 19). **Validation runs BEFORE the Accessibility hard check** so a hidden-Space exit doesn't first trigger the AX TCC prompt.
8. **Accessibility hard check.** A match was found AND the window is reachable, so we're about to click. Demand Accessibility now — first-run UX is "exit 13, grant in Settings, re-run." Exit 13 (`PermissionsMissing`) if denied. (Required because `CGEvent::post(HID)` silently no-ops without AX trust on macOS 10.14+.)
9. **Deliver the click via stealth HID + osascript activation.** Sequence: (a) shell out to `osascript -e 'tell application "System Events" to set frontmost of (first process whose unix id is N) to true'` to put RoK frontmost — Catalyst Bridge apps require activation before they accept synthetic UITouch input. (b) Sleep 50ms (`ACTIVATION_SETTLE_MS`) for the AppKit→UIKit translation to apply. (c) Probe the user's cursor position via `CGEvent::new(source).location()`, then disassociate the visible cursor via `CGAssociateMouseAndMouseCursorPosition(false)` so the HID tap doesn't visibly warp the user's cursor. (d) Post `LeftMouseDown` at `(x, y)` via `CGEvent::post(kCGHIDEventTap)`, sleep `CLICK_GAP_MS = 80ms`, post `LeftMouseUp` at the same point. (e) `CGDisplay::warp_mouse_cursor_position(saved)` + reassociate. A RAII `CursorStealth` guard handles steps (e) on panic / early-return so the user's cursor is never left disassociated. Exit 18 (`ClickFailed`) with one of `activation_failed`, `probe`, `disassociate`, `source`, `down`, `up` on failure. The v0.1.5 AX-press path was deleted; see [TODOS.md](../TODOS.md) for the 6-path investigation that led to this design.
10. **Post-click 3-check validation.** Re-check `(WID, PID)` in All, then in OnScreenOnly, then frame within tolerance. No click-point check (we've already clicked). Exit 19 (`WindowChanged`) on `window_id_gone` / `not_visible` / `frame_moved`.
11. **Verify the click via needle-swap (v0.2 — replaces v0.1.4 pixel-diff).** Sleep `VERIFY_DELAY_MS = 500ms`, re-capture to `./rok-capture-post.png`, and re-match the toggle ROI against both needles (`confirm_needle_swap`). A **different** needle winning post-click confirms the city↔world view toggled — the tick logs `needle-swap verify passed`. The same needle → exit 20 (`ClickNotVerified`, reason `no_swap`). Neither needle → exit 20 (reason `neither_needle` — likely a mid-transition frame). A confirmed tick then returns to step 5 for the next tick. Needle-swap replaced pixel-diff because pixel-diff false-passed a missed click during RoK's ambient water/troop animation; with v0.2.1's two real discriminative needles a healthy multi-tick run confirms a real toggle every tick. **Tick error policy (v0.2.1 four-way classifier):** a FATAL error (`PermissionsMissing`, `ClickFailed`, `WindowChanged{window_id_gone}`) aborts the loop immediately with that error's exit code; a TRANSIENT error (`TargetNotFound`, `ClickNotVerified`, `CaptureFailed`) counts toward a 3-consecutive-failure budget — a successful tick resets the streak, three in a row → exit 21 (`LoopAborted`, reason `failure_budget_exhausted`); a `WindowGone` or `WindowHidden` signal (a relaunched RoK, or a hidden window caught by the per-tick `validate_window_present` liveness probe) drops the loop into the `recover_window` recovery sub-loop instead of aborting. Recovery is bounded by a `RECOVERY_BUDGET` of 3; exhausting it aborts exit 21 with `window_recovery_exhausted` / `visibility_recovery_exhausted` / `recovery_budget_exhausted`. A genuinely successful tick resets both the failure streak and the recovery counter. Recovery is gated to `ROK_BOT_MAX_TICKS >= 2` — `ROK_BOT_MAX_TICKS=1` stays an exact one-shot.

Exit codes for shell users:
- `0` = happy path (pre + post captures written, target located, click posted, change verified). Mode 1 or Mode 2.
- `10` = `WindowNotFound`
- `11` = `WindowScreenUnresolved`
- (exit `12` unused — was `RokNotOnPrimary` in v0.1.5, deleted in v0.1.6 when the Mode 2 gate opened)
- `13` = `PermissionsMissing` (grant Screen Recording and/or Accessibility, re-run)
- `14` = `CaptureFailed` (v0.1.8: stage tag in error message — `symlink_refused`, `no_shareable_content`, `window_not_found`, `capture_returned_nil`, `cgimage_decode`, `png_write`. Most common cause: SR not granted to the rok-bot binary itself, see "First-run TCC grant" above.)
- `15` = `TargetNotFound` (no needle matched — capture decoded fine but the best NCC score is below `MATCH_THRESHOLD`, or every needle was sentinel-skipped; see [Matcher needles](#matcher-needles-both-ship-real--no-operator-crop-step) below)
- `16` = `ImageLoadFailed` (haystack PNG missing, malformed, or larger than 8192×8192; or embedded needle decode fails — defensive arm for a corrupt asset commit)
- `17` = `TargetTooLarge` (needle dims strictly greater than haystack dims; rare in practice)
- `18` = `ClickFailed` (a step in the click pipeline failed; v0.1.6 reason tags `activation_failed`, `probe`, `disassociate`, `source`, `down`, `up`)
- `19` = `WindowChanged` (window vanished, hidden on a non-displayed Space, moved/resized, or click point outside frame — 4-check TOCTOU defense; reason tags `window_id_gone`, `not_visible`, `frame_moved`, `point_outside_frame`)
- `20` = `ClickNotVerified` (click delivered but the needle-swap verify did not confirm a toggle; reason tags: `no_swap` — post-click matched the same needle; `neither_needle` — neither needle matched, likely a mid-transition frame)
- `21` = `LoopAborted` (continuous loop aborted; reason tags: `failure_budget_exhausted` — 3 consecutive transient tick failures (RoK frozen); `window_recovery_exhausted` — a relaunched RoK could not be re-discovered within the recovery budget; `visibility_recovery_exhausted` — a hidden window never reappeared within the recovery budget; `recovery_budget_exhausted` — too many recovery cycles without a genuinely successful tick (a crash-looping RoK); `signal_handler_install_failed` — the SIGINT handler could not be installed at boot. A multi-tick run on a healthy RoK exits 0 with confirmed toggles — exit 21 means a genuine abort.)

### Matcher needles (both ship real — no operator crop step)

The loop carries **two** needles: `assets/targets/city-button.png` and `assets/targets/world-button.png`. The needle-swap verify confirms a click by checking that the post-click capture matches a **different** needle than the pre-click capture did — so it genuinely needs one discriminative needle per view.

As of **v0.2.1 both needles ship as real 96×96 tight inner-glyph crops** of the city↔world toggle button — `city-button.png` is the castle-towers glyph, `world-button.png` is the folded-map glyph. The v0.2 sentinel placeholder is gone; there is no operator crop step. Multi-tick runs confirm real toggles and exit 0.

The crops are deliberately **tight**. The city↔world toggle keeps an identical gold-rim/blue-circle chrome in both views — only the small inner glyph differs. The matcher's `imageproc` `CrossCorrelationNormalized` is non-mean-centered, so a full-button crop scores ~0.96 against the *wrong* view (the shared chrome dominates the correlation) — that false-match is exactly what kept the v0.2 loop a smoke test. A tight inner-glyph crop that excludes the gold-rim chrome drops cross-view NCC to ~0.88, so `find_best_needle`'s best-of-N reliably picks the correct-view needle (correct ≈1.0, wrong ≈0.88).

**If RoK's UI updates and the needles rot** (NCC scores fall, ticks start failing `no_swap` / `neither_needle`):

1. Capture RoK in **city view** and in **world view** (a loop run writes `rok-capture-pre.png` / `rok-capture-post.png`; one lands in each view if the click toggled).
2. In each capture, crop **tight around just the inner glyph** of the city↔world toggle button — the castle-towers glyph for `city-button.png`, the folded-map glyph for `world-button.png`. **Exclude the shared gold-rim / blue-circle chrome** — including it scores ~0.96 against the wrong view and breaks discrimination. ~96×96 is a good size.
3. Save over `assets/targets/city-button.png` and `assets/targets/world-button.png`.
4. `cargo build --release` to re-embed the new bytes via `include_bytes!`.
5. The `embedded_world_needle_carries_no_sentinel` and `embedded_needle_carries_no_sentinel` tests assert neither needle carries the placeholder sentinel — keep them passing.
6. Run the loop and confirm ticks log `needle-swap verify passed` and the run exits 0 on the tick cap / Ctrl-C.

## Optional — install pre-commit hooks (contributors)

The repo ships `.pre-commit-config.yaml` with `cargo fmt --check` (pre-commit), `cargo clippy --locked -- -D warnings` (pre-push), and `cargo test --locked` (pre-push). The hooks are dead config until installed:

```sh
brew install pre-commit       # one-time, if not already installed
pre-commit install --install-hooks --hook-type pre-commit --hook-type pre-push
```

After that, every commit runs fmt-check and every push runs clippy + tests. Skip if you'd rather rely on local manual `cargo` invocations.

---

## Mode 2 — one-time setup (v0.1.6 shipped; manual BD setup, lifecycle automation in v0.2)

> **What works today:** v0.1.6 detects RoK on any non-built-in display and runs the full pipeline (capture, match, click via stealth HID + activation, verify). You set up BetterDisplay once manually; the bot uses whatever you've configured. **What's still v0.2:** the optional Shortcuts integration that lets the bot connect/disconnect the virtual display per session. For now, leave BD's virtual display connected whenever you want to run the bot.

### 1. Install BetterDisplay

Free download: https://github.com/waydabber/BetterDisplay/releases (or via Homebrew: `brew install --cask betterdisplay`).

After install, grant Screen Recording permission when prompted.

### 2. Create a virtual display

1. Click the BetterDisplay icon in your menu bar
2. **Create New Virtual Screen**
3. Pick **Default** profile (1920×1080-class is fine — RoK will adapt). 1512×945 logical works well as a clone of the built-in panel.
4. Name it whatever you want — the bot finds it by being non-primary, not by name
5. Arrange it in `System Settings → Displays` (typically LEFT of the built-in so its CG origin is negative; this matches v0.1.6 test coverage)

The new display appears in `System Settings → Displays`. You can drag RoK onto it like any external monitor.

### 3. Drag RoK to the virtual display

Click and hold RoK's title bar, drag it onto your BetterDisplay virtual screen. The window stays there across sessions unless you drag it back.

### 4. Run the bot

```sh
cargo run --release
```

The bot detects RoK is on a non-built-in display and runs the Mode 2 pipeline. Each click:

- Activates RoK on its (invisible) virtual display via `osascript`.
- Disassociates your visible cursor so the HID tap doesn't make your cursor jump off-screen.
- Posts the click pair to the virtual-display coords.
- Restores your cursor and reassociates.

You see nothing happen visually on your built-in display. The bot's pre/post captures (`rok-capture-pre.png` and `rok-capture-post.png` in cwd) show you exactly what RoK is doing on the invisible display.

### 5. (Optional) Author the v0.2 lifecycle shortcuts now

The shortcuts below are not used by v0.1.6, but authoring them now prepares you for v0.2's automatic BD lifecycle management. You can skip this step until v0.2 ships.

**Open `Shortcuts.app`** (built-in macOS app — Cmd+Space, type "Shortcuts").

**Shortcut #1 — `RokBot Connect Display`:**

1. Click the `+` icon to create a new shortcut
2. In the search field on the right pane, type **`Connect or Disconnect a Display`** — drag the action into the shortcut
3. In the action: set **Display** to your virtual display, set **Action** to **Connect**
4. **Important — silences the per-session approval dialog:** below the BD action, search for and drag in **`Stop and output`**. After dropping it, an input chip will auto-populate (something like `[Connect or Disconnect a Display]`). **Click that chip and press Delete to clear it.** The action should read just `Stop and output` with no input.
5. Rename the shortcut at the top: **`RokBot Connect Display`** (exact name, exact spaces)
6. Cmd+S to save

**Shortcut #2 — `RokBot Disconnect Display`:**

Same flow as above, with two changes:
- In the BD action, set **Action** to **Disconnect**
- Name it **`RokBot Disconnect Display`**
- Same `Stop and output` cleanup at the bottom

These shortcuts sit dormant until v0.2's lifecycle module starts calling `shortcuts run "RokBot Connect Display"` / `RokBot Disconnect Display` per session.

---

## Why the `Stop and output` trick matters

Without it: the first time the bot calls `shortcuts run "RokBot Connect Display"`, macOS pops a dialog:

> Allow "RokBot Connect Display" to output 1 boolean? [Don't Allow] [Allow Once] [Always Allow]

Even if you click "Always Allow," the BD action still returns a boolean to the parent process every run, with variable latency (3-13 seconds per call). Adding `Stop and output` with no input makes the shortcut return nothing — the disclosure dialog never fires, and runtime drops to under 1 second per call.

This is generic to any third-party App Intent invoked via `shortcuts run`. Apple gates intent return values from leaving the Shortcuts.app sandbox; clearing the output suppresses the gate.

---

## Troubleshooting

**Bot exits with `ClickFailed { reason: "activation_failed" }`.** `osascript` returned non-zero when trying to set RoK frontmost. Most likely RoK's pid changed between window discovery and the click site (you closed and re-opened RoK mid-run). Re-run the bot.

**Bot exits with `ClickFailed { reason: "disassociate" }`.** `CGAssociateMouseAndMouseCursorPosition(false)` refused. Rare; usually means another process has the cursor association locked. Try `pkill -SIGCONT WindowServer` or restart the Mac if it persists.

**Cursor stays disassociated after the bot crashed.** Move your trackpad / mouse — the system reassociates after a real input event. The RAII `CursorStealth::Drop` guard handles this in v0.1.6, but if the bot crashed in a way that didn't run Drop (extremely rare on `panic = "unwind"`), a trackpad gesture is the recovery.

**Bot exits with `WindowChanged { reason: "not_visible" }` even though RoK is on the virtual display.** The BetterDisplay virtual display may have disconnected mid-run, or RoK got pushed to a hidden Space. Reopen BD, confirm the virtual display is still connected, switch to RoK's Space if needed, re-run.

**`screencapture -l <wid>` returns a black or stale capture.** Confirm Screen Recording permission is granted to the terminal you're running from (not just the bot binary; first-run TCC is per-process). System Settings → Privacy & Security → Screen Recording.

**Multiple BetterDisplay virtual displays.** v0.1.6 doesn't care which one RoK is on — it classifies by `CGDisplayIsBuiltin`, so any non-built-in display works. If you want the v0.2 lifecycle shortcuts to target a specific virtual display, the BD action picker lets you choose when authoring.

---

## Optional: HDMI dummy plug or iPad Sidecar instead of BetterDisplay

The bot's Mode 2 detection works with any non-built-in display, not just BetterDisplay. If you have an HDMI dummy plug (~$5 on Amazon) or an iPad you can use via Sidecar:

- Plug in / connect the alternative display
- Drag RoK there
- Run the bot — it'll detect Mode 2 the same way

In v0.1.6 the bot never touches display lifecycle, so these alternatives are functionally identical to BetterDisplay for Mode 2 operation. The v0.2 lifecycle shortcuts (step 5 above) are BetterDisplay-specific; with a dummy plug or Sidecar that's always-on, there's nothing for the shortcuts to do.
