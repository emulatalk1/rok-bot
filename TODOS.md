# TODOS

Items deferred from planning sessions. Each entry should be self-contained enough that picking it up months later is feasible.

---

## ✅ DONE — BetterDisplay setup automation

**Resolved 2026-05-06** via `docs/setup.md` (commit `f8ba718`). Covers Mode 1 (zero
setup), Mode 2 (BetterDisplay walkthrough + Stop-and-output trick), troubleshooting,
and alternative non-BD displays (HDMI dummy plug, Sidecar). Originally specced as
P1.

---

## P1: Migrate from `screencapturekit` Rust crate to `objc2-screen-capture-kit`

**Source:** P2 spike build failure 2026-05-06
**Effort:** human ~half day / CC ~1 hour
**Depends on:** v0.1 implementation phase

The `screencapturekit` Rust crate (1.5.x) wraps an internal Swift package via `swift-bridge`. The build script invokes `xcrun --sdk macosx --show-sdk-platform-path`, which fails on machines with only Xcode Command Line Tools installed (full Xcode required). This makes the dep a hard install-time blocker for any contributor without ~14 GB of Xcode.

`objc2-screen-capture-kit` (0.3.x) is the canonical alternative — direct ObjC2 message-passing bindings, no Swift bridge, no Xcode requirement. v0.1 should use it from day one.

Until then, the spike at `spikes/p2-spike/run.sh` shells out to Apple's `screencapture -l <wid>` CLI for capture, which works fine for verification but is not appropriate for v0.1 (subprocess overhead per frame).

---

## P2: v0.2 — Mode 2 lifecycle (deferred from /plan-eng-review 2026-05-07)

**Source:** /plan-eng-review of `~/.gstack/projects/emulatalk1-rok-bot/hbchuc-main-design-20260506-202938.md`
**Effort:** human ~1 day / CC ~2 hours
**Depends on:** v0.1 Mode 1 binary shipped (cargo init + window.rs + display.rs + main.rs visible-mode)

The two-mode runtime contract design splits into two milestones per Codex tension review:

- **v0.1 (this PR):** Mode 1 only — RoK on primary, bot runs visibly. NO shortcuts module, NO lifecycle module, NO panic hook, NO state file. Smallest path to a working `cargo run`. Ships in days.
- **v0.2 (this TODO):** Mode 2 lifecycle. Adds:
  - `src/shortcuts.rs` with a `Shortcuts` trait + `SystemShortcuts` impl (shells out to `shortcuts list` / `shortcuts run`). `wait-timeout` crate for 5s subprocess timeout. `MockShortcuts` for tests.
  - `src/lifecycle.rs` with `LifecycleGuard` (RAII Drop), `std::panic::set_hook`, `ctrlc::set_handler`. Static `OnceLock<Arc<Mutex<LifecycleState>>>` for `connected_by_us` + `already_disconnected` dedup. 500ms drop-detection thread tolerating 3 consecutive errors before canceling.
  - **State file `~/.rok-bot/last-mode`** — written `mode2` on Mode 2 clean exit, deleted on Mode 1 exit. Boot logic: if file says `mode2` AND RoK on primary, force cold-path connect (per Codex tension 1, prevents mode-signal pollution after a clean disconnect).
  - Cold/warm path branches in `src/main.rs`.
  - Full unit test coverage via the trait seam.

**Why split:** Codex review surfaced that designing RAII + signal handling before there's a minimal visible bot is wasted motion. Get something working, then add the magic.

**Reference:** the pre-split design is in `~/.gstack/projects/emulatalk1-rok-bot/hbchuc-main-design-20260506-202938.md`. Eng review decisions logged in this branch's review jsonl.

---

## P2 (partially done): TCC permissions preflight

**Source:** /plan-eng-review codex tension 4
**Status:** Screen Recording check ✅ DONE in v0.1 (`src/permissions.rs::check_screen_recording`). Uses the safe `core_graphics::access::ScreenCaptureAccess::preflight()` wrapper. After the /review pass at 76f9d41, also falls back to `request()` on first-run denial — without that, fresh-install Macs hit a permanent exit 13 with no UI to grant from. Returns `BotError::PermissionsMissing { which: "Screen Recording" }` (exit code 13).

**Remaining: Accessibility check.**
**Effort:** human ~1 hour / CC ~15 min
**Depends on:** the synthetic-input milestone (first `CGEvent.post` call landing in src/)

The bot will need Accessibility once it starts injecting clicks. Approach when that milestone arrives:
- Mirror the Screen Recording shape: a `check_accessibility()` in `src/permissions.rs` returning `BotError::PermissionsMissing { which: "Accessibility" }` on denial.
- macOS API: `AXIsProcessTrustedWithOptions` (in `ApplicationServices`); the safe wrapper lives in the `accessibility-sys` crate or can be a tiny extern. Or call `CGRequestPostEventAccess()` (introduced macOS 10.15) which returns bool.
- Update `docs/setup.md` with the Accessibility grant step alongside the existing Screen Recording note.
- Wire the new check into `main.rs::run()` immediately after `check_screen_recording()`.

---

## P2: v0.2 — snapshot retry loop for BD reconfig race (deferred from /review 76f9d41)

**Source:** /review cross-model finding (Claude adversarial A8 + Codex adversarial #4, multi-confirmed)
**Effort:** human ~1 hour / CC ~20 min
**Depends on:** v0.2 Mode 2 lifecycle (the race becomes critical when the bot manages BD lifecycle)

`main::run` takes two unrelated snapshots: first the RoK window frame via `find_rok_window`, then the live display arrangement via `display::detect_mode`. Between those calls (microseconds normally, longer under BetterDisplay reconnect storms / Sidecar attach-detach / sleep-wake / a user dragging the window between displays), the two snapshots can describe different worlds. Result: transient `WindowScreenUnresolved` or wrong-mode classification even when the steady-state setup is valid.

**Approach for v0.2:**
- Wrap the find-window + detect-mode pair in a retry loop with exponential backoff (~3 attempts, 100ms / 250ms / 500ms gaps). If the window+display pair stays inconsistent across all attempts, surface the original error.
- Tolerate up to N consecutive transient errors before failing (per /plan-eng-review A3 — same pattern that v0.2's drop-detection thread will use).

---

## P2: v0.2 — switch from `OnScreenOnly` to `optionAll` with state-aware filtering (deferred from /review 76f9d41)

**Source:** /review cross-model finding (Claude adversarial A10 + Codex adversarial #5, multi-confirmed)
**Effort:** human ~2 hours / CC ~30 min
**Depends on:** v0.2 Mode 2 lifecycle

`src/window.rs` calls `copy_window_info(kCGWindowListOptionOnScreenOnly, ...)`. That flag excludes RoK windows that are minimized, on a different macOS Space, or temporarily off-screen during BD display churn. v0.1's narrow scope (RoK on built-in, foreground) doesn't expose this — but v0.2 Mode 2 explicitly involves moving RoK across displays, and Spaces interaction will surface false `WindowNotFound` errors that should really be "RoK is alive but not visible right now."

**Approach for v0.2:**
- Switch to `kCGWindowListOptionAll` (or `OnScreenOnly | IncludingWindow` if window IDs are cached).
- Add a state filter: only consider windows where `kCGWindowIsOnscreen == 1` for the Mode classification step, but keep all RoK-owned windows in scope for "is RoK running at all?" detection.
- Distinguish three states in error reporting: RoK process not running (rare), RoK running but window minimized/off-Space, RoK present and on-screen.

---

## P3: v0.1+ — area-majority window classification (deferred from /review 76f9d41)

**Source:** /review Codex adversarial #6
**Effort:** human ~30 min / CC ~10 min
**Depends on:** the synthetic-input + capture milestone (the bug only matters when click coords or capture rects depend on the window's full area, not its center)

`src/display.rs::classify` currently returns the Mode based on the window's center pixel. A window that's 90% on a virtual display but with center pixel landing on the built-in (drag a window so it straddles two displays) returns `Mode::Visible` — silently misclassifying. Today this is harmless (we just exit on either branch); once capture/click logic depends on the full client area, the wrong display assignment becomes a bug.

**Approach:**
- Replace center-point test with area-majority: compute the intersection area of the window frame with each display's bounds; pick the display with the largest overlap. `rect_contains` becomes `rect_intersection_area(window, display)`.
- Test cases: window 100% on one display (current behavior preserved), window straddling 50/50 (deterministic tie-breaker — pick the first display in iteration order, document it), window straddling 90/10 (returns the 90% display).

---

## P3: Multi-instance feasibility (the "farm" 10x dream)

**Source:** office-hours D7 (P1 in the original premise list)
**Effort:** human ~1 hour spike / CC ~15 min
**Depends on:** v0.1 hello-world working

The autonomous-twin → multi-account-farm path requires running multiple RoK instances on a single Mac. Mac App Store's iOS-on-Mac runtime may enforce single-instance per bundle ID, in which case the farm vision needs separate Macs / VMs / remote machines.

**Cheapest verification when ready:** launch RoK twice via `open -na /Applications/RiseOfKingdoms.app` (or whatever the actual install path is) and observe whether two distinct processes survive, or whether the second invocation just brings the first to focus.

If multi-instance works, the v0.1 architecture (single binary, single account) refactors to a coordinator + per-account workers. If multi-instance is blocked, the farm dream needs additional Macs or BetterDisplay virtual-display arrangements per Mac.
