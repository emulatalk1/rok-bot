# Setup

`rok-bot` runs in two modes depending on where Rise of Kingdoms is parked.

| Mode | RoK is on... | Bot behavior | Your machine while bot runs |
|---|---|---|---|
| **Mode 1 — Visible** | Primary (built-in) display | Bot runs in foreground, you watch | You can't really use the Mac |
| **Mode 2 — Background** | A non-primary display (a BetterDisplay virtual display, an HDMI dummy plug, an iPad via Sidecar, or a real second monitor) | Bot runs invisibly on that display | You keep using the Mac normally |

The bot auto-detects which mode to use by reading where RoK's window is. No CLI flag, no config file. Drag RoK between displays to switch.

This guide covers Mode 2 setup with **BetterDisplay** (free, no real hardware required). If you already have a second monitor, HDMI dummy plug, or iPad via Sidecar, you can skip the shortcut authoring entirely — see [the alternatives section](#optional-hdmi-dummy-plug-or-ipad-sidecar-instead-of-betterdisplay) at the bottom. The bot's Mode 2 detection works with any always-on non-primary display, and the shortcuts are only needed when the bot manages BetterDisplay's virtual-display lifecycle.

---

## Mode 1 — zero setup

Make sure RoK is on your built-in display, run the bot. That's it. Useful for first-run debugging, demos, or when you don't need the machine for other work.

```sh
cargo run
```

## Optional — install pre-commit hooks (contributors)

The repo ships `.pre-commit-config.yaml` with `cargo fmt --check` (pre-commit), `cargo clippy --locked -- -D warnings` (pre-push), and `cargo test --locked` (pre-push). The hooks are dead config until installed:

```sh
brew install pre-commit       # one-time, if not already installed
pre-commit install --install-hooks --hook-type pre-commit --hook-type pre-push
```

After that, every commit runs fmt-check and every push runs clippy + tests. Skip if you'd rather rely on local manual `cargo` invocations.

---

## Mode 2 — one-time setup

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
