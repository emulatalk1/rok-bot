# Setup

> **v0.1 status:** Only **Mode 1 (visible)** is implemented today. The Mode 2 sections below describe the v0.2 plan and remain useful as a forward-looking guide, but if you run v0.1 with RoK on a non-built-in display, the bot will exit `RokNotOnPrimary` (exit 12) with a message asking you to drag RoK to your built-in display. See [TODOS.md](../TODOS.md) for the v0.2 Mode 2 lifecycle work.

`rok-bot` is designed to run in two modes depending on where Rise of Kingdoms is parked.

| Mode | RoK is on... | Bot behavior | Your machine while bot runs | Status |
|---|---|---|---|---|
| **Mode 1 — Visible** | Built-in display (laptop Retina panel; `CGDisplayIsBuiltin` test, NOT the menu-bar display) | Detect window, capture to PNG, template-match a known target, log coords + score; v0.1.3 adds the click | Bot is observable today (capture written, match logged); once click synthesis lands, you'll watch it interact | **v0.1.2 ✅ shipped** |
| **Mode 2 — Background** | A non-built-in display (a BetterDisplay virtual display, an HDMI dummy plug, an iPad via Sidecar, or a real second monitor) | Bot runs invisibly on that display via BD virtual-display lifecycle management | You keep using the Mac normally | **v0.2 — planned, not yet shipped** |

The bot auto-detects which mode to use by reading where RoK's window is. No CLI flag, no config file. Drag RoK between displays to switch.

The Mode 2 sections below cover setup with **BetterDisplay** (free, no real hardware required). If you already have a second monitor, HDMI dummy plug, or iPad via Sidecar, you can skip the shortcut authoring entirely — see [the alternatives section](#optional-hdmi-dummy-plug-or-ipad-sidecar-instead-of-betterdisplay) at the bottom. The bot's Mode 2 detection will work with any always-on non-built-in display once v0.2 ships; the shortcuts are only needed when the bot manages BetterDisplay's virtual-display lifecycle.

---

## Mode 1 — zero setup (v0.1 shipped)

Make sure RoK is on your built-in display, then run the bot.

```sh
cargo run --release
```

Boot sequence (logged to stderr via `tracing`):
1. **Screen Recording preflight.** First run on a fresh Mac triggers macOS's permission prompt and registers your terminal in System Settings → Privacy & Security → Screen Recording. Subsequent runs are silent. If you deny, exit code 13 (`PermissionsMissing`).
2. **Find the RoK main window.** Filtered by `kCGWindowOwnerName == kCGWindowName == "RiseOfKingdoms"` AND backed by a process whose bundle ID starts with `com.rok.ios.` (anti-spoof gate against any local process that names itself "RiseOfKingdoms"). Exit 10 (`WindowNotFound`) if no match.
3. **Classify display.** `CGDisplayIsBuiltin` test — built-in (laptop Retina panel) → `Mode::Visible`, anything else → `Mode::Virtual`. Exit 12 (`RokNotOnPrimary`) on Virtual in v0.1.
4. **Mode::Visible:** capture the RoK window to `./rok-capture.png` via `/usr/sbin/screencapture -l <window_id> -x -o` (silent, no shadow). Exit 14 (`CaptureFailed`) if `screencapture` returns non-zero.
5. **Template-match the target inside the capture.** Decode the haystack PNG via `image::ImageReader` with `Limits` (`max_image_width`/`max_image_height` = 8192) so a malformed or oversized PNG fails fast as `ImageLoadFailed` (exit 16) instead of OOM-ing the process. Decode the embedded needle (`assets/targets/city-button.png`), run `imageproc::match_template_parallel` with `CrossCorrelationNormalized`, find the highest-scoring position, and compare against `MATCH_THRESHOLD = 0.85`. Exit 0 with an info log (`x`, `y`, `score`, `elapsed_ms`) on match; exit 15 (`TargetNotFound`) on no match; exit 17 (`TargetTooLarge`) if the needle is strictly larger than the haystack in either dimension. v0.1.2 stops here. Future sub-milestones add click synthesis (v0.1.3) and after-state verification (v0.1.4).

Exit codes for shell users:
- `0` = Mode 1 happy path (capture written, target located)
- `10` = `WindowNotFound`
- `11` = `WindowScreenUnresolved`
- `12` = `RokNotOnPrimary` (drag RoK to your built-in display, re-run)
- `13` = `PermissionsMissing` (grant Screen Recording, re-run)
- `14` = `CaptureFailed` (rare; usually means Screen Recording was revoked between preflight and capture)
- `15` = `TargetNotFound` (capture decoded fine, but best NCC score below `MATCH_THRESHOLD` — the most likely cause is the placeholder embedded asset; see [the v0.1.2 placeholder note](#v012-target-asset-placeholder) below)
- `16` = `ImageLoadFailed` (haystack PNG missing, malformed, or larger than 8192×8192; or embedded needle decode fails — defensive arm for a corrupt asset commit)
- `17` = `TargetTooLarge` (needle dims strictly greater than haystack dims; rare in practice)

### v0.1.2 target asset placeholder

The committed `assets/targets/city-button.png` is a 80×40 synthetic placeholder, not the real RoK city/world toggle button. **The bot will exit 15 (`TargetNotFound`) on a real RoK capture** until you replace the asset with a real crop. Workflow once you have a fresh `rok-capture.png`:

1. Open `rok-capture.png` in Preview.
2. Crop the bottom-right city/world toggle button (~80×40 pixels — exact dims aren't critical).
3. Save as `assets/targets/city-button.png` (overwrite the placeholder).
4. `cargo build --release` to re-embed the new bytes via `include_bytes!`.
5. `cargo run --release` and confirm exit 0 with a high-confidence score (`> 0.95` for an exact crop).

The matcher's logic is identical regardless of which bytes are embedded; the placeholder exists only so the build compiles before the first live capture is taken.

## Optional — install pre-commit hooks (contributors)

The repo ships `.pre-commit-config.yaml` with `cargo fmt --check` (pre-commit), `cargo clippy --locked -- -D warnings` (pre-push), and `cargo test --locked` (pre-push). The hooks are dead config until installed:

```sh
brew install pre-commit       # one-time, if not already installed
pre-commit install --install-hooks --hook-type pre-commit --hook-type pre-push
```

After that, every commit runs fmt-check and every push runs clippy + tests. Skip if you'd rather rely on local manual `cargo` invocations.

---

## Mode 2 — one-time setup (v0.2 planned, not yet shipped)

> The setup below describes the v0.2 plan. Authoring the shortcuts and creating the BD virtual display now is harmless and prepares you for v0.2 — but on v0.1 the bot will not actually use them. RoK on a non-built-in display in v0.1 returns `RokNotOnPrimary` (exit 12).

### 1. Install BetterDisplay

Free download: https://github.com/waydabber/BetterDisplay/releases (or via Homebrew: `brew install --cask betterdisplay`).

After install, grant Screen Recording permission when prompted.

### 2. Create a virtual display

1. Click the BetterDisplay icon in your menu bar
2. **Create New Virtual Screen**
3. Pick **Default** profile (1920×1080-class is fine — RoK will adapt)
4. Name it whatever you want — the bot finds it by being non-primary, not by name

The new display appears in `System Settings → Displays`. You can drag RoK onto it like any external monitor.

### 3. Author two shortcuts

The bot drives BetterDisplay's virtual display lifecycle through macOS Shortcuts. You author the shortcuts once; the bot calls them every session via `shortcuts run`.

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

### 4. Drag RoK to the virtual display

Click and hold RoK's title bar, drag it onto your BetterDisplay virtual screen.

### 5. Run the bot

```sh
cargo run
```

The bot detects RoK is on a non-primary display, runs `shortcuts run "RokBot Connect Display"` (~1 second), waits for RoK to migrate back if the virtual was disconnected, runs the task, then runs `shortcuts run "RokBot Disconnect Display"` on clean exit. You keep using the Mac the whole time.

---

## Why the `Stop and output` trick matters

Without it: the first time the bot calls `shortcuts run "RokBot Connect Display"`, macOS pops a dialog:

> Allow "RokBot Connect Display" to output 1 boolean? [Don't Allow] [Allow Once] [Always Allow]

Even if you click "Always Allow," the BD action still returns a boolean to the parent process every run, with variable latency (3-13 seconds per call). Adding `Stop and output` with no input makes the shortcut return nothing — the disclosure dialog never fires, and runtime drops to under 1 second per call.

This is generic to any third-party App Intent invoked via `shortcuts run`. Apple gates intent return values from leaving the Shortcuts.app sandbox; clearing the output suppresses the gate.

---

## Troubleshooting

**Bot exits with `ShortcutsNotInstalled`.** You skipped step 3, or named the shortcuts something other than `RokBot Connect Display` / `RokBot Disconnect Display`. Run `shortcuts list | grep RokBot` to confirm. Names are case-sensitive and space-sensitive.

**Bot exits with `RokDidNotMigrate`.** The bot connected the virtual display but RoK didn't auto-return to it within 3 seconds. Most likely cause: you haven't dragged RoK to the virtual display yet (step 4). Drag it there once and re-run.

**Bot exits with `BetterDisplayMissing`.** BetterDisplay isn't running, or its virtual display has been deleted. Open the BetterDisplay menu bar item and confirm the virtual display still exists.

**`shortcuts run` takes 5+ seconds and a dialog flashes on screen.** You skipped the `Stop and output` step in either shortcut. Edit each shortcut, add `Stop and output`, clear the input chip (Delete key), save. Re-run the bot.

**Multiple BetterDisplay virtual displays.** v0.1 assumes one. If you have several, the BD action picker lets you target a specific one when authoring the shortcuts; just make sure both shortcuts target the same display. Multi-instance is on the roadmap.

---

## Optional: HDMI dummy plug or iPad Sidecar instead of BetterDisplay

The bot's Mode 2 detection works with any non-primary display, not just BetterDisplay. If you have an HDMI dummy plug (~) or an iPad you can use via Sidecar:

- Plug in / connect the alternative display
- Drag RoK there
- Run the bot — it'll detect Mode 2 the same way

The shortcuts in step 3 are only needed if you want the bot to manage the BetterDisplay virtual display's lifecycle (toggle on/off per session). With a dummy plug or Sidecar that's always-on, the bot just operates and never touches the display state. The shortcuts being missing in this case won't cause an error — the bot detects the display is already there and runs the warm path directly.
