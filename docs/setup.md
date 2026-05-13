# Setup

> **v0.1.6 status:** Both **Mode 1 (visible)** and **Mode 2 (virtual display)** ship today. Mode 2 is the recommended operating mode — RoK lives on a BetterDisplay virtual display, the bot operates invisibly, you keep using the Mac. The Mode 2 lifecycle automation (shortcuts that connect/disconnect the virtual display per session) is **still deferred to v0.2**; for now you set BD up once manually and the bot uses whatever's there.

`rok-bot` is designed to run in two modes depending on where Rise of Kingdoms is parked.

| Mode | RoK is on... | Bot behavior | Your machine while bot runs | Status |
|---|---|---|---|---|
| **Mode 1 — Visible** | Built-in display (laptop Retina panel; `CGDisplayIsBuiltin` test, NOT the menu-bar display) | Detect window, capture to PNG, template-match a known target, 4-check TOCTOU-validate the click site (incl. hidden-Space detection), deliver a stealth HID click via `osascript` activation + `CGEvent::post(HID)`, re-capture and pixel-diff to verify the click changed the screen | Each click visibly raises RoK to foreground (Catalyst auto-raise behavior). You see what the bot sees. | **v0.1.6 ✅ shipped** |
| **Mode 2 — Background (recommended)** | A non-built-in display (BetterDisplay virtual, HDMI dummy plug, iPad via Sidecar, or a real second monitor) | Same pipeline; same click path; same captures. RoK's auto-raise lands on a display nothing observes. | You keep using the Mac normally. Cursor stealth keeps your visible cursor in place during clicks. | **v0.1.6 ✅ shipped (manual BD setup; lifecycle automation deferred to v0.2)** |

The bot auto-detects which mode to use by reading where RoK's window is. No CLI flag, no config file. Drag RoK between displays to switch.

The Mode 2 sections below cover setup with **BetterDisplay** (free, no real hardware required). If you already have a second monitor, HDMI dummy plug, or iPad via Sidecar, you can skip the shortcut authoring entirely — see [the alternatives section](#optional-hdmi-dummy-plug-or-ipad-sidecar-instead-of-betterdisplay) at the bottom.

---

## Mode 1 — zero setup (v0.1.6 shipped)

Make sure RoK is on your built-in display, then run the bot.

```sh
cargo run --release
```

> **Mode 1 caveat:** Catalyst Bridge auto-raises RoK to the foreground on every synthetic UITouch (this is the structural reason Mode 2 exists). You'll see RoK pop forward each click. For unattended use, prefer Mode 2.

Boot sequence (logged to stderr via `tracing`):
1. **Screen Recording preflight.** First run on a fresh Mac triggers macOS's prompt and registers your terminal in System Settings → Privacy & Security → Screen Recording. Exit 13 (`PermissionsMissing`) if denied.
2. **Accessibility peek (warn-only).** Logs a warning if AX isn't yet granted; the hard check fires later only if we're actually about to deliver a click.
3. **Find the RoK main window.** Filtered by `kCGWindowOwnerName == kCGWindowName == "RiseOfKingdoms"` AND backed by a process whose bundle ID starts with `com.rok.ios.` (anti-spoof gate). Search `kCGWindowListOptionOnScreenOnly` first; if no match, fall back to `kCGWindowListOptionAll`. Three outcomes: found visible → continue. Found in All but not OnScreenOnly → exit 19 `WindowChanged { not_visible }` (RoK is running but on a hidden Space, minimized, or transient state — switch to its Space or unminimize). No match anywhere → exit 10 `WindowNotFound`.
4. **Classify display.** `CGDisplayIsBuiltin` test — built-in (laptop Retina panel) → `Mode::Visible`, anything else → `Mode::Virtual`. v0.1.6 lets both modes proceed; v0.1.5's exit-12 gate was removed.
5. **Pre-click capture.** Capture the RoK window to `./rok-capture-pre.png` via `/usr/sbin/screencapture -l <window_id> -x -o` (silent, no shadow). Works regardless of which display RoK is on or whether other windows overlap RoK's frame on screen. Exit 14 (`CaptureFailed`) if `screencapture` returns non-zero.
6. **Template-match the target inside the pre-capture.** Decode the haystack PNG via `image::ImageReader` with `Limits` (`max_image_width`/`max_image_height` = 8192) so a malformed or oversized PNG fails fast as `ImageLoadFailed` (exit 16) instead of OOM-ing. Decode the embedded needle (`assets/targets/city-button.png`). The `needle_has_placeholder_sentinel` gate runs first — if the needle still carries the placeholder sentinel pattern (`[255, 0, 255, 0]` top-left luma), it logs `WARN placeholder sentinel needle detected; refusing to match` and returns exit 15 (`TargetNotFound`) BEFORE any NCC math runs. Otherwise, run `imageproc::match_template_parallel` with `CrossCorrelationNormalized` and compare against `MATCH_THRESHOLD = 0.85`. Exit 15 (`TargetNotFound`) on no match; exit 17 (`TargetTooLarge`) if the needle is strictly larger than the haystack in either dimension. Current latency: ~21s on M-series for a 2102×1640 Retina haystack (v0.2 FFT-NCC migration tracked in TODOS).
7. **4-check pre-click TOCTOU validation.** Anchors on `(WID, PID)` and checks both `OnScreenOnly` and `All` window lists: (a) `(WID, PID)` present in `All` (catches WID reuse), (b) `(WID, PID)` present in `OnScreenOnly` (catches mid-flow hide), (c) frame within tolerance, (d) click point inside expected frame. Maps to `WindowChanged` reasons `window_id_gone` / `not_visible` / `frame_moved` / `point_outside_frame` (all exit 19). **Validation runs BEFORE the Accessibility hard check** so a hidden-Space exit doesn't first trigger the AX TCC prompt.
8. **Accessibility hard check.** A match was found AND the window is reachable, so we're about to click. Demand Accessibility now — first-run UX is "exit 13, grant in Settings, re-run." Exit 13 (`PermissionsMissing`) if denied. (Required because `CGEvent::post(HID)` silently no-ops without AX trust on macOS 10.14+.)
9. **Deliver the click via stealth HID + osascript activation.** Sequence: (a) shell out to `osascript -e 'tell application "System Events" to set frontmost of (first process whose unix id is N) to true'` to put RoK frontmost — Catalyst Bridge apps require activation before they accept synthetic UITouch input. (b) Sleep 50ms (`ACTIVATION_SETTLE_MS`) for the AppKit→UIKit translation to apply. (c) Probe the user's cursor position via `CGEvent::new(source).location()`, then disassociate the visible cursor via `CGAssociateMouseAndMouseCursorPosition(false)` so the HID tap doesn't visibly warp the user's cursor. (d) Post `LeftMouseDown` at `(x, y)` via `CGEvent::post(kCGHIDEventTap)`, sleep `CLICK_GAP_MS = 80ms`, post `LeftMouseUp` at the same point. (e) `CGDisplay::warp_mouse_cursor_position(saved)` + reassociate. A RAII `CursorStealth` guard handles steps (e) on panic / early-return so the user's cursor is never left disassociated. Exit 18 (`ClickFailed`) with one of `activation_failed`, `probe`, `disassociate`, `source`, `down`, `up` on failure. The v0.1.5 AX-press path was deleted; see [TODOS.md](../TODOS.md) for the 6-path investigation that led to this design.
10. **Post-click 3-check validation.** Re-check `(WID, PID)` in All, then in OnScreenOnly, then frame within tolerance. No click-point check (we've already clicked). Exit 19 (`WindowChanged`) on `window_id_gone` / `not_visible` / `frame_moved`.
11. **Verify the click landed visibly.** Sleep `VERIFY_DELAY_MS = 500ms`, re-capture to `./rok-capture-post.png`, decode both captures to Luma8, and pixel-diff. If the differing-pixel count is `< PIXEL_DIFF_REJECT_THRESHOLD = 1000`, exit 20 (`ClickNotVerified`) with reason `screen_unchanged`. Dim mismatch between pre/post captures fail-closes with reason `dim_mismatch` (same exit 20). Otherwise exit 0 with an info log (`x`, `y`, `match_score`, `pixel_diff`, `elapsed_ms`).

Exit codes for shell users:
- `0` = happy path (pre + post captures written, target located, click posted, change verified). Mode 1 or Mode 2.
- `10` = `WindowNotFound`
- `11` = `WindowScreenUnresolved`
- (exit `12` unused — was `RokNotOnPrimary` in v0.1.5, deleted in v0.1.6 when the Mode 2 gate opened)
- `13` = `PermissionsMissing` (grant Screen Recording and/or Accessibility, re-run)
- `14` = `CaptureFailed` (rare; usually means Screen Recording was revoked between preflight and capture)
- `15` = `TargetNotFound` (placeholder-sentinel gate fired, OR capture decoded fine but best NCC score below `MATCH_THRESHOLD`; see [the placeholder note](#v014-target-asset-placeholder--sentinel-gate) below)
- `16` = `ImageLoadFailed` (haystack PNG missing, malformed, or larger than 8192×8192; or embedded needle decode fails — defensive arm for a corrupt asset commit)
- `17` = `TargetTooLarge` (needle dims strictly greater than haystack dims; rare in practice)
- `18` = `ClickFailed` (a step in the click pipeline failed; v0.1.6 reason tags `activation_failed`, `probe`, `disassociate`, `source`, `down`, `up`)
- `19` = `WindowChanged` (window vanished, hidden on a non-displayed Space, moved/resized, or click point outside frame — 4-check TOCTOU defense; reason tags `window_id_gone`, `not_visible`, `frame_moved`, `point_outside_frame`)
- `20` = `ClickNotVerified` (click delivered but pre/post pixel-diff was below `PIXEL_DIFF_REJECT_THRESHOLD = 1000` pixels; reason tags: `screen_unchanged`, `dim_mismatch`)

### v0.1.4 target asset placeholder + sentinel gate

The committed `assets/targets/city-button.png` is a 80×40 synthetic placeholder, not the real RoK city/world toggle button. **The bot will exit 15 (`TargetNotFound`) on a real RoK capture** until you replace the asset with a real crop. Two things to know:

1. **The placeholder carries a structural sentinel pattern** (top-left four pixels luma `[255, 0, 255, 0]` + xorshift32 high-entropy noise body). `matcher::needle_has_placeholder_sentinel` detects this pattern and fail-closes BEFORE running NCC. Without this gate, imageproc's `CrossCorrelationNormalized` scores low-entropy placeholders 0.9+ against arbitrary structured images — a real false-match at 0.9267 was caught against live RoK during /qa, 5ms from posting a synthetic click. With the gate, you get `WARN placeholder sentinel needle detected (top-left luma [255,0,255,0]); refusing to match` followed by exit 15.

2. **Replacing the placeholder removes the sentinel.** Real natural images effectively never contain pixel-perfect `[255, 0, 255, 0]` horizontally-adjacent extremes in the top-left, so the gate goes dormant the moment you commit a real crop. The build-time `embedded_placeholder_carries_sentinel` test will fail at that transition — that's expected; delete the test (it exists specifically to catch accidental sentinel removal).

Workflow once you have a fresh `rok-capture-pre.png`:

1. Open `rok-capture-pre.png` in Preview.
2. Crop the bottom-right city/world toggle button (~80×40 pixels — exact dims aren't critical).
3. Save as `assets/targets/city-button.png` (overwrite the placeholder).
4. `cargo build --release` to re-embed the new bytes via `include_bytes!`.
5. Delete the `embedded_placeholder_carries_sentinel` test (it will fail by design).
6. `cargo run --release` and confirm exit 0 with a high-confidence score (`> 0.95` for an exact crop) and a `pixel_diff` value well above 1000 (the click should produce a visible UI change).

The matcher's logic is identical regardless of which bytes are embedded; the placeholder + sentinel gate exists so the build compiles AND the bot refuses to operate before the first live capture is taken.

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
