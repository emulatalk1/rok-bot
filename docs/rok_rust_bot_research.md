# RoK Rust Bot — Research & Dependency Verification Report

> **Purpose:** This document is intended for reverification via Claude Code CLI.
> Each section contains checkable claims, crate versions, and architecture decisions
> that should be verified against crates.io, GitHub, and official documentation.

---

## 1. Platform Clarification

### Claim to Verify
- Rise of Kingdoms runs natively on Apple Silicon Macs (M1/M2/M3/M4)
  via the Mac App Store as an iOS app.
- Release date for macOS: April 6, 2022 (Wikipedia).
- No BlueStacks or ADB required on Apple Silicon.

### Verification Tasks
```
- [ ] Confirm RoK is on Mac App Store: https://apps.apple.com/us/app/rise-of-kingdoms/id1354260888
- [ ] Confirm macOS release date on Wikipedia: https://en.wikipedia.org/wiki/Rise_of_Kingdoms
- [ ] Confirm it runs on Apple Silicon without emulator
```

---

## 2. Existing Bot Architecture Analysis

### 2.1 OSROKBOT (GabrielAgrela)
- **Repo:** https://github.com/GabrielAgrela/OSROKBOT
- **Language:** Python 100%
- **Architecture Pattern:** State Machine + Action Composition

```
StateMachine
  └── State[]
        └── Action[]
              ├── FindAndClickImage      (template matching)
              ├── ManualClickPosition    (hardcoded coords)
              ├── PressKey
              ├── ExtractTextFromImage   (Tesseract OCR)
              ├── SendEmail              (captcha alert)
              └── ChatGPT               (Lyceum quiz answers)
```

**Key design decisions to adopt:**
- States return success/failure → points to next state (clean FSM)
- Actions are composable and reusable across state machines
- Templates stored in `Media/` folder, loaded at runtime
- Captcha detection runs as **parallel state machine** watching every frame
- Coordinates stored as **percentages of screen**, not raw pixels
- Designed strictly for **16:9 ratio** screens

**Features implemented:**
- Scout Exploration
- Farm Barbarians
- Farm Resources
- Captcha detection + email alert
- Lyceum (quiz event)
- Lyceum Midterm/Finals

### Verification Tasks
```
- [ ] Check repo still public and accessible
- [ ] Confirm architecture from Classes/ folder structure
- [ ] Note: Classes/ was not directly readable (robots.txt blocked)
```

---

### 2.2 Dylan-Zheng Bot
- **Repo:** https://github.com/Dylan-Zheng/Rise-of-Kingdoms-Bot
- **Language:** Python 100%
- **Status:** No longer maintained (author's note)
- **Architecture Pattern:** Task-based with scheduler + device abstraction layer

```
main.py
  ├── adb.py              ← device abstraction (all ADB calls here)
  ├── config.py           ← JSON config per device
  ├── utils.py            ← CV helpers: find(), click(), OCR()
  ├── tasks/              ← one file per task
  │     ├── collect.py
  │     ├── train.py
  │     ├── gather.py
  │     └── verify.py
  ├── gui/                ← tkinter GUI
  └── bot_related/        ← building positions cache (JSON)
```

**Key design decisions to adopt:**
- Device abstraction layer — swap transport without touching logic
- Building position cache — scan city once, save to JSON, reuse forever
- Multi-device support — per-device bot loops in parallel threads
- Random task ordering — reduces ban detection pattern
- Config per device — resolution, language, task toggles

**Feature list (fully implemented):**
- Auto-start game if not running
- Locate buildings automatically
- Collect resources, troops, alliance help
- Produce materials
- Open free tavern chest
- Claim quests and daily objectives
- Claim VIP chest
- Donate technology
- Train and upgrade troops
- Attack barbarians
- Heal troops
- Gather resources on world map
- Mystery Merchant
- Multi-device/emulator support
- Captcha bypass via haoi API and 2captcha API
- Simple tkinter GUI

**Requirements from repo:**
- Python 3.7
- ADB 29.0.5-5949299
- Tesseract v5.0.0-alpha
- opencv-python, pytesseract, numpy, pillow, pure-python-adb, requests

### Verification Tasks
```
- [ ] Confirm repo structure matches above
- [ ] Note: uses ADB + BlueStacks (Android emulator) — not applicable to macOS native
- [ ] The architecture patterns (task separation, device abstraction) are still worth porting
```

---

## 3. Headless Mode Analysis

> **Updated 2026-05-06 — major architectural finding from the P2 spike.**
> The original "off-screen window via `osascript`" approach is dead for iOS-on-Mac apps. The autonomous-twin vision is preserved via a different mechanism: parking RoK on a virtual display.

### The Catalyst / iOS-on-Mac Sandbox Constraint

Rise of Kingdoms on Mac App Store is an iOS-on-macOS app (bundle ID `com.rok.ios.vn`, Catalyst-class runtime). These apps are **architecturally sandboxed from external window manipulation** by every mechanism we tested:

| Approach | Result | Evidence |
|---|---|---|
| `osascript` / System Events `set position of window 1` | ❌ DEAD | RoK exposes **0 windows** to the AX tree; only "menu bar" is visible. `tell process "RiseOfKingdoms" to count windows` returns `0`. |
| Private CGS API `CGSMoveWindow` (default connection) | ❌ Silent no-op | Returns 0 (success) but window does not move. The system humors the call but does nothing. |
| Private CGS API `CGSMoveWindow` (owner connection) | ❌ Explicit denial | Returns `kCGErrorCannotComplete` (268435459). The OS actively refuses cross-process manipulation of iOS-on-Mac windows. |
| `CGEventPostToPid` (process-targeted input) | ❌ DEAD | 0 differing bytes after click via `postToPid`; same click via global `CGEvent.post` → 6.2M differing bytes. The API is a no-op for iOS-on-Mac. |

**Conclusion:** No external process can hide, move, or process-target an iOS-on-Mac window on Apple Silicon. The runtime is closed.

### What does work — the virtual-display approach

The autonomous-twin vision ("bot drives RoK while user works") is achievable via a **virtual / phantom display** rather than off-screen window manipulation. The setup:

1. Create a virtual display via [BetterDisplay](https://github.com/waydabber/BetterDisplay) (free version supports basic virtual screens). One-time install, free for non-business use, native Apple Silicon, no kernel extensions.
2. User manually drags RoK to the virtual display once (programmatic move is impossible per above; manual drag works because it goes through standard WindowServer hit-testing).
3. RoK now lives on a display the user cannot see. The user works on the primary display fullscreen.
4. The bot operates on RoK at its virtual-display coordinates using only **standard, public APIs**:
   - **Capture:** `CGWindowListCopyWindowInfo` + `screencapture -l <wid>` (works across all displays and Spaces)
   - **Click:** `CGEvent.post(tap: .cghidEventTap)` at the window's actual coordinates (which now fall on the virtual display)

### Verified end-to-end (2026-05-06 spike)

With RoK parked on a 1536×864 virtual display at `(-1536, 59)`:

| Test | Result |
|---|---|
| `screencapture -l 64793 → 2238×1776 PNG` of RoK at virtual-display coords | ✅ PASS |
| `CGEvent.post` left-click at the window's center on the virtual display | ✅ PASS |
| Differing bytes between before/after frames after the click | **6,159,216** — definitive state change |

### Hardware alternatives to BetterDisplay

If software-virtual-display is undesirable for any reason:
- **HDMI dummy plug** (~$5): plug into a Thunderbolt/USB-C-via-HDMI adapter. Mac sees a 1080p/4K monitor with no physical screen attached. EDID-only emulator, hardware-stable across macOS updates.
- **iPad as Sidecar**: Apple's built-in feature. Park RoK on the iPad, put the iPad face-down. Free if you own a 2018+ iPad.
- **Sidecar via iPhone**: not supported by Apple. Third-party tools (Duet Display etc.) exist but small screen + cost + flakiness make them worse than BetterDisplay or a dummy plug.

### What's *not* required

- No private CGS APIs (we proved they're blocked anyway).
- No osascript / AppleScript (RoK doesn't expose AX windows).
- No focus stealing (clicks land on the virtual display regardless of which Space the user is in).
- No re-architecture if the user later adds a real second monitor (the bot just operates on whatever display RoK is on).

---

## 4. Dependency Verification

> **Instructions for Claude Code:**
> For each crate below, verify:
> 1. Current latest version on crates.io
> 2. Last publish date
> 3. Whether the version listed here is correct
> 4. Any known issues on macOS Apple Silicon

---

### 4.1 Screen Capture

#### `screencapturekit`
- **Latest version:** `1.5.4` (verified 2026-05-06)
- **Last publish:** 2026-03-09
- **crates.io:** https://crates.io/crates/screencapturekit
- **GitHub:** https://github.com/doom-fish/screencapturekit-rs
- **🚨 Build constraint discovered 2026-05-06:** the crate uses an internal Swift-bridge package that requires the **full Xcode** SDK to build (`xcrun --sdk macosx --show-sdk-platform-path` must succeed). Command Line Tools alone are not enough — the build script fails with `unable to lookup item 'PlatformPath' in SDK '/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk'`. **Recommend `objc2-screen-capture-kit` instead** (direct ObjC2 bindings, no Swift bridge, builds with CLT).
- **Requires:** macOS 12.3+
- **API style:** Async-first (tokio compatible), streaming frames via callback

```toml
screencapturekit = "1.5"
```

**Key API (v1.5):**
```rust
use screencapturekit::prelude::*;

// Async window capture
let content = AsyncSCShareableContent::get().await?;
let window = content.windows()
    .iter()
    .find(|w| w.title().contains("Rise of Kingdoms"));

let filter = SCContentFilter::build()
    .window(window)
    .build();

let config = SCStreamConfiguration::builder()
    .width(1920).height(1080)
    .build();

let stream = AsyncSCStream::new(&filter, &config, 30, SCStreamOutputType::Screen);
stream.start_capture()?;
```

**Verify:**
```
- [ ] cargo search screencapturekit — confirm 1.5.0 is latest
- [ ] Check docs.rs build status for 1.5.0
- [ ] Confirm SCK works on off-screen windows
- [ ] Note: docs.rs build failed for some versions — check if source compiles locally
```

---

#### `xcap`
- **Latest version:** `0.9.4` (verified 2026-05-06)
- **Last publish:** 2026-04-09
- **crates.io:** https://crates.io/crates/xcap
- **GitHub:** https://github.com/nashaofu/xcap
- **Note:** Uses `objc2` family under the hood on macOS. The 0.9 line had four point releases in one month (0.9.0–0.9.4 between 2026-03-09 and 2026-04-09) — review CHANGELOG before adopting; API was in flux.

```toml
xcap = "0.9"
```

**Key API:**
```rust
use xcap::Window;

let windows = Window::all().unwrap();
let rok = windows.iter()
    .find(|w| w.title().unwrap_or_default().contains("Kingdoms"))
    .unwrap();

// Will FAIL if window is minimized
let image = rok.capture_image().unwrap();
```

**Verify:**
```
- [ ] cargo search xcap — confirm 0.8.2 is latest
- [ ] Confirm Window::capture_image() works when window is off-screen (not minimized)
- [ ] Check objc2 dependency version compatibility
```

---

### 4.2 Input Injection

#### `enigo`
- **Latest version:** `0.6.1` (verified 2026-05-06)
- **Last publish:** 2025-08-28 (0.6.0 and 0.6.1 shipped same day)
- **crates.io:** https://crates.io/crates/enigo
- **GitHub:** https://github.com/enigo-rs/enigo
- **macOS backend:** Uses `CGEventSource` and CGEvents directly (confirmed from source)
- **Note:** Posts at HID layer — no window focus required. **0.6 is a breaking change from 0.5** — re-verify the `Mouse`/`Keyboard` trait signatures in the API example below before relying on it.

```toml
enigo = "0.6"
```

**Key API:**
```rust
use enigo::{Enigo, Mouse, Button, Coordinate, Direction, Settings};

let mut enigo = Enigo::new(&Settings::default()).unwrap();
enigo.move_mouse(540, 960, Coordinate::Abs).unwrap();
enigo.button(Button::Left, Direction::Click).unwrap();
```

**Known issue to verify:**
```
- [ ] Confirm enigo v0.5 works without window focus on macOS
- [ ] Check if held keys repeat on macOS (known issue #98 — may still be open)
- [ ] cargo search enigo — confirm 0.5.0 is latest
```

---

#### `core-graphics`
- **Latest version:** `0.25.0` (verified 2026-05-06)
- **Last publish:** 2025-05-27
- **crates.io:** https://crates.io/crates/core-graphics
- **GitHub:** https://github.com/servo/core-graphics-rs
- **Downloads:** ~32 million all-time, part of Servo project
- **Use case:** Raw CGEvent creation when enigo abstraction is insufficient
- **🚨 Build-blocker note:** `core-graphics 0.25` is built on top of `core-foundation 0.10.x`. Pinning `core-foundation = "0.9"` alongside `core-graphics = "0.25"` mixes incompatible major versions of the same FFI types and will fail to build cleanly. Use `core-foundation = "0.10"`.

```toml
core-graphics   = "0.25"   # 0.25.0
core-foundation = "0.10"   # 0.10.1 — REQUIRED by core-graphics 0.25
```

**Key API (raw CGEvent tap):**
```rust
use core_graphics::event::*;
use core_graphics::event_source::*;
use core_graphics::geometry::CGPoint;

let src = CGEventSource::new(CGEventSourceStateID::HIDSystemState).unwrap();
let pos = CGPoint::new(540.0, 960.0);

let down = CGEvent::new_mouse_event(
    src.clone(), CGEventType::LeftMouseDown, pos, CGMouseButton::Left
).unwrap();
down.post(CGEventTapLocation::HID);
```

**Verify:**
```
- [ ] cargo search core-graphics — confirm 0.25.0 is latest
- [ ] Confirm CGEventPostToPid works on macOS 14+ (Sonoma) without issues
```

---

### 4.3 Computer Vision

#### `image`
- **Latest version:** `0.25.10` (verified 2026-05-06)
- **Last publish:** 2026-03-10
- **crates.io:** https://crates.io/crates/image
- **Use:** Image loading, saving, pixel manipulation

```toml
image = "0.25"
```

**Verify:**
```
- [ ] cargo search image — confirm 0.25.x is latest stable
```

---

#### `imageproc`
- **Latest version:** `0.26.2` (verified 2026-05-06)
- **Last publish:** 2026-05-01 (0.23.1, 0.24.1, 0.25.1, and 0.26.2 all republished same day — coordinated backport)
- **crates.io:** https://crates.io/crates/imageproc
- **Use:** Template matching via `match_template()`
- **Note:** Pure Rust, no OpenCV dependency — critical for macOS ease of setup. Verify `MatchTemplateMethod::CrossCorrelationNormalized` still exists in 0.26 (Outstanding Question #5).

```toml
imageproc = "0.26"
```

**Key API:**
```rust
use imageproc::template_matching::{match_template, MatchTemplateMethod};
use image::GrayImage;

fn find_on_screen(screen: &GrayImage, template: &GrayImage, threshold: f32) -> Option<(u32, u32)> {
    let result = match_template(screen, template, MatchTemplateMethod::CrossCorrelationNormalized);
    let (mut bx, mut by) = (0u32, 0u32);
    let mut best = 0.0f32;
    for (x, y, px) in result.enumerate_pixels() {
        let v = px[0];
        if v > best { best = v; bx = x; by = y; }
    }
    let (th, tw) = (template.height(), template.width());
    if best >= threshold { Some((bx + tw/2, by + th/2)) } else { None }
}
```

**GPU alternative (optional, if performance needed):**
- `template-matching` crate — GPU-accelerated, but last commit 2022 — use with caution

**Verify:**
```
- [ ] cargo search imageproc — confirm 0.25.x is latest
- [ ] Check MatchTemplateMethod::CrossCorrelationNormalized exists in 0.25
```

---

### 4.4 OCR

#### `tesseract` ← REPLACES `leptess`
- **Latest version:** `0.15.2` (verified 2026-05-06)
- **Last publish:** 2025-04-19
- **crates.io:** https://crates.io/crates/tesseract
- **Note:** `leptess` confirmed stale — last publish 2023-02-21 (3+ years).

```toml
tesseract = "0.15"   # NOT leptess
```

**Why not leptess:**
- Last published: 2023-02-21 (verified) — 3+ years stale as of 2026-05
- No active maintenance signals
- The `tesseract` crate is more recently updated (0.15.2 on 2025-04-19)

**Fallback — subprocess (zero dependency risk):**
```rust
// Requires: brew install tesseract
fn ocr_region(img_path: &str) -> anyhow::Result<String> {
    let out = std::process::Command::new("tesseract")
        .args([img_path, "stdout", "--psm", "7"])
        .output()?;
    Ok(String::from_utf8(out.stdout)?.trim().to_string())
}
```

**Verify:**
```
- [ ] cargo search tesseract — confirm 0.15 is latest, check last publish date
- [ ] cargo search leptess — confirm it IS stale (last publish Feb 2023)
- [ ] Check if tesseract crate compiles cleanly on Apple Silicon (arm64)
```

---

### 4.5 Async Runtime

#### `tokio`
- **Latest version:** `1.52.2` (verified 2026-05-06; published 2026-05-04, two days before audit)
- **crates.io:** https://crates.io/crates/tokio
- **LTS:** 1.43.x maintained until March 2026
- **Downloads:** 645 million all-time

```toml
tokio = { version = "1", features = ["full"] }
```

**Verify:**
```
- [ ] cargo search tokio — confirm 1.52.x is current
- [ ] Confirm "full" feature set is appropriate (or trim to: rt-multi-thread, macros, sync, time)
```

---

### 4.6 HTTP (Captcha API)

#### `reqwest`
- **Latest version:** `0.13.3` (verified 2026-05-06)
- **Last publish:** 2026-04-27 (0.13.0 first shipped 2025-12-30 — 5+ months stable)
- **crates.io:** https://crates.io/crates/reqwest
- **Use:** 2captcha and haoi API calls for captcha bypass
- **Note:** 0.13 is one major behind from the original `"0.12"` pin. Builder API and rustls/tls feature surface typically shift between reqwest minors — review CHANGELOG.

```toml
reqwest = { version = "0.13", features = ["json"] }
```

**Verify:**
```
- [ ] cargo search reqwest — confirm 0.12.x is latest
```

---

### 4.7 Config & Utilities

#### `dotenvy` — NOT `dotenv`
- **Latest version:** `0.15.7` (verified 2026-05-06)
- **Last publish:** 2023-03-22
- **crates.io:** https://crates.io/crates/dotenvy
- **Note:** `dotenv` confirmed abandoned — last publish 2019-10-21 (6+ years).
- **Caveat:** `dotenvy` itself has not shipped a release since 2023-03 — the crate is *stable* (small surface, no churn needed) but not actively developed. Still the right pick over `dotenv`.

```toml
dotenvy = "0.15"   # NOT dotenv
```

**Verify:**
```
- [ ] Confirm dotenv IS abandoned on crates.io
- [ ] Confirm dotenvy IS the maintained replacement
```

---

#### Other utilities (all standard, low-risk)
```toml
serde            = { version = "1", features = ["derive"] }   # 1.0.228
serde_json       = "1"                                        # 1.0.149
rand             = "0.10"                                     # 0.10.1 — see note below
anyhow           = "1"                                        # 1.0.102
tracing          = "0.1"                                      # 0.1.44
tracing-subscriber = { version = "0.3", features = ["env-filter"] }   # 0.3.23
```

**Verified 2026-05-06:**
- ✅ `rand 0.9` and `rand 0.10` are both released. `max_stable` is **0.10.1** (2026-04-11). `0.8.6` was still patched on 2026-04-17, so 0.8 is *not* stale — but for new code, prefer 0.10. Note: 0.9 reorganized the prelude and trait names; 0.10 is a smaller follow-up.
- ✅ All other utility crates above are at current latest.

---

## 5. Full Verified Cargo.toml

> **Audit applied 2026-05-06** against crates.io API. Pin changes from the original draft:
> `xcap 0.8 → 0.9`, `enigo 0.5 → 0.6`, `core-foundation 0.9 → 0.10` (build-blocker fix),
> `imageproc 0.25 → 0.26`, `reqwest 0.12 → 0.13`, `rand 0.8 → 0.10`.
> See `cargo_dependency_audit.md` for the full per-crate report.

```toml
[package]
name    = "rok-bot"
version = "0.1.0"
edition = "2021"

[dependencies]

# Screen Capture
screencapturekit = "1.5"   # 1.5.4 — Apple ScreenCaptureKit, headless-capable, macOS 12.3+
xcap             = "0.9"   # 0.9.4 — fallback capture, NOT for minimized windows

# Input
enigo            = "0.6"   # 0.6.1 — CGEvents under the hood, no focus required
core-graphics    = "0.25"  # raw CGEvent when fine control needed
core-foundation  = "0.10"  # 0.10.x is REQUIRED by core-graphics 0.25 (0.9 will not build)

# Vision
image            = "0.25"  # 0.25.10
imageproc        = "0.26"  # 0.26.2 — pure Rust template matching, no OpenCV

# OCR (NOT leptess — stale since Feb 2023)
tesseract        = "0.15"  # 0.15.2

# Async
tokio            = { version = "1", features = ["full"] }   # 1.52.2

# HTTP
reqwest          = { version = "0.13", features = ["json"] }   # 0.13.3

# Config
serde            = { version = "1", features = ["derive"] }
serde_json       = "1"
dotenvy          = "0.15"  # 0.15.7 — stable since 2023-03 (low churn, not abandoned)

# Logging & Errors
tracing              = "0.1"   # 0.1.44
tracing-subscriber   = { version = "0.3", features = ["env-filter"] }   # 0.3.23
anyhow               = "1"     # 1.0.102

# Utils
rand             = "0.10"  # 0.10.1 — 0.8 still patched but 0.9/0.10 are out
```

---

## 6. Proposed Project Architecture

Synthesized from OSROKBOT (state machine pattern) + Dylan-Zheng (task separation + device abstraction):

```
rok-bot/
├── Cargo.toml
├── config.json                  ← per-account settings (resolution, tasks enabled)
├── templates/                   ← PNG templates per state/action
│   ├── city/
│   │   ├── city_hall.png
│   │   ├── build_queue_empty.png
│   │   └── collect_bubble.png
│   ├── world/
│   │   ├── minimap.png
│   │   └── rss_node.png
│   └── common/
│       ├── confirm_btn.png
│       ├── loading_bar.png
│       └── captcha.png
│
└── src/
    ├── main.rs                  ← tokio runtime, spawn task per account
    ├── capture.rs               ← screencapturekit wrapper, find_rok_window()
    ├── input.rs                 ← enigo tap/swipe/key + human jitter
    ├── vision.rs                ← template_match(), ocr_region(), color_sample()
    ├── config.rs                ← serde Config struct
    │
    ├── state/
    │   ├── mod.rs               ← GameState enum + detect_state()
    │   ├── city.rs              ← MainCity handler
    │   ├── world.rs             ← WorldMap handler
    │   └── loading.rs           ← Loading/unknown recovery
    │
    ├── tasks/                   ← one file per task (Dylan-Zheng pattern)
    │   ├── mod.rs               ← Task trait definition
    │   ├── collect.rs           ← Collect floating resource bubbles
    │   ├── build.rs             ← Check and manage build queue
    │   ├── train.rs             ← Train troops in barracks
    │   ├── gather.rs            ← Send commanders to gather on map
    │   ├── alliance.rs          ← Alliance help + donations
    │   └── captcha.rs           ← Parallel captcha watcher (OSROKBOT pattern)
    │
    └── positions/
        └── cache.rs             ← Scan city once, save building coords to JSON
```

### Task Trait (from OSROKBOT action pattern)
```rust
#[async_trait]
pub trait Task: Send + Sync {
    fn name(&self) -> &str;
    async fn run(&self, ctx: &BotContext) -> anyhow::Result<TaskResult>;
    fn should_run(&self, state: &GameState) -> bool;
}

pub enum TaskResult {
    Success,
    Failure(String),
    NeedsState(GameState),  // redirect to different state
}
```

### Parallel Captcha Watcher (from OSROKBOT pattern)
```rust
// Runs in background tokio task, independent of main loop
pub async fn captcha_watcher(ctx: Arc<BotContext>, notify: Sender<()>) {
    loop {
        let screen = ctx.capture().await;
        if let Some(_) = ctx.vision.find(&screen, "common/captcha.png", 0.90) {
            tracing::warn!("Captcha detected — pausing bot");
            notify.send(()).await.ok();
            // Optionally: call 2captcha API here
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
```

---

## 7. Permissions Required on macOS

| Permission | Location | Required For |
|---|---|---|
| Screen Recording | System Settings → Privacy & Security | `screencapturekit`, `xcap` |
| Accessibility | System Settings → Privacy & Security | `enigo`, `core-graphics` CGEvents |

Grant to Terminal or your compiled binary. First run will trigger prompt automatically.

---

## 8. Outstanding Questions for Reverification

```
1. Does screencapturekit v1.5 correctly filter by window when game is off-screen (-9999, -9999)?
   → Test: SCContentFilter::build().window(&rok_window).build()

2. Does enigo v0.6 inject events to a background/off-screen app without focus?
   → Test: move RoK off-screen, tap via enigo, confirm tap registers in game
   → Note: bumped from v0.5 (audit 2026-05-06) — re-verify on the new API

3. Is tesseract crate v0.15.2 stable on Apple Silicon (arm64-apple-darwin)?
   → Test: cargo build on M-series Mac with brew-installed tesseract

4. ✅ ANSWERED 2026-05-06 — rand 0.9 and 0.10 are both released; max_stable is 0.10.1.
   0.8.6 still patched (2026-04-17), so 0.8 is not stale. New project: prefer 0.10.

5. Does imageproc 0.26 template matching still expose CrossCorrelationNormalized method?
   → Check: docs.rs/imageproc/0.26.2/imageproc/template_matching
   → Note: bumped from 0.25 (audit 2026-05-06) — API may have shifted

6. Does screencapturekit 1.5.4 docs.rs build succeed? (1.5.0 had build failures)
   → Check: docs.rs/crate/screencapturekit/1.5.4

7. Is there a better pure-Rust OCR alternative than tesseract bindings?
   → Search: rust OCR 2026 no-tesseract

8. NEW: reqwest 0.12 → 0.13 migration — does the captcha API call code in §6 still
   compile? Builder API and TLS feature surface typically shift between minors.
   → Test: build a minimal reqwest::Client::new().get(...).json::<T>() against 0.13.3
```

---

## 9. Sources & References

| Topic | URL |
|---|---|
| RoK Mac App Store | https://apps.apple.com/us/app/rise-of-kingdoms/id1354260888 |
| RoK Wikipedia | https://en.wikipedia.org/wiki/Rise_of_Kingdoms |
| OSROKBOT | https://github.com/GabrielAgrela/OSROKBOT |
| Dylan-Zheng Bot | https://github.com/Dylan-Zheng/Rise-of-Kingdoms-Bot |
| screencapturekit crate | https://crates.io/crates/screencapturekit |
| screencapturekit GitHub | https://github.com/doom-fish/screencapturekit-rs |
| xcap crate | https://crates.io/crates/xcap |
| enigo crate | https://crates.io/crates/enigo |
| enigo GitHub | https://github.com/enigo-rs/enigo |
| core-graphics crate | https://crates.io/crates/core-graphics |
| imageproc crate | https://crates.io/crates/imageproc |
| tesseract crate | https://crates.io/crates/tesseract |
| leptess crate (stale) | https://crates.io/crates/leptess |
| tokio crate | https://crates.io/crates/tokio |
| reqwest crate | https://crates.io/crates/reqwest |
| dotenvy crate | https://crates.io/crates/dotenvy |
| template-matching GPU | https://github.com/urholaukkarinen/template-matching |

---

*Generated: 2026-05-06 | Dependency audit applied 2026-05-06 (see cargo_dependency_audit.md) | For reverification via Claude Code CLI*
