# TODOS

Items deferred from planning sessions. Each entry should be self-contained enough that picking it up months later is feasible.

---

## ✅ DONE — v0.2 SHIPPED: Continuous loop

**Shipped 2026-05-16**, tag `v0.2`. Turns the v0.1.x one-shot `capture→match→click→verify` pipeline into a continuous loop targeting the state-neutral city↔world toggle — a deliberate plumbing milestone (D1 from `/plan-eng-review`): it proves the loop machinery, it does not do a real in-game task.

New `src/run_loop.rs` (loop engine: pure decision core `classify_error`/`next_failure_count`/`should_stop`/`parse_max_ticks` + live `tick()`/`run_loop()`). `ctrlc` SIGINT handler → `AtomicBool` checked at tick boundaries; `ROK_BOT_MAX_TICKS` env cap (default 100; `=1` reproduces the one-shot). Error policy D5/D12: FATAL aborts immediately, TRANSIENT counts toward a 3-consecutive-failure budget → `BotError::LoopAborted` exit 21. `matcher.rs` generalized to `find_best_needle` (2-needle best-of-N, `NEEDLES`, `NeedleMatch`, `select_roi`, `last_position_roi`). `verify.rs` reworked to needle-swap (`confirm_needle_swap`); the pixel-diff path was deleted. `click.rs` gained `ClickGuard` RAII. `main.rs` = boot + delegate; Accessibility promoted to a hard boot check. `assets/targets/world-button.png` ships as a sentinel placeholder (operator crops the real world-view art — see `docs/setup.md` "v0.2 world-needle crop"). 15 locked decisions (D1-D15) from `/plan-eng-review`; 179 unit tests + 6 `#[ignore]`'d integration tests; full `/review` (5 specialists + Claude adversarial); live `/qa` on a Mode 2 BD virtual display confirmed both loop exit paths (`ROK_BOT_MAX_TICKS=1` → 0, `=3` → 21).

The three v0.2.x/v0.3 items D13 (anti-bot cadence jitter), D14 (window re-discovery after RoK relaunch), and D15 (`not_visible` wait-and-retry) added by the v0.2 `/plan-eng-review` are listed under P2/P3 below. D14 + D15 shipped in v0.2.1 — see the next section.

---

## ✅ DONE — v0.2.1 SHIPPED: Window-lifecycle resilience + needle-swap discrimination fix

**Shipped 2026-05-18**, tag `v0.2.1`. Bundles two things on top of v0.2: the planned window-lifecycle-resilience milestone (Part A) and a needle-swap discrimination fix found via `/investigate` (Part B). 200 unit tests + 12 `#[ignore]`'d integration tests; `cargo clippy --all-targets --all-features --locked -- -D warnings` and `cargo fmt --all -- --check` clean. Exit codes unchanged from v0.2 (10-11, 13-21).

**Part A — window-lifecycle resilience** (10 locked decisions L1-L10 from `/plan-eng-review`). The v0.2 continuous loop now survives (a) a RoK crash + relaunch mid-loop, (b) a system screensaver / hidden window, (c) a BetterDisplay virtual-display reconnect race at boot. `main.rs` retries `find_rok_window` + `detect_mode` at boot (3 attempts, 150/400ms backoff) on a `WindowScreenUnresolved` error, invalidating the SCK cache between attempts. `run_loop` owns the `RokWindow` by value; the per-tick error classifier is now four-way — `Fatal` / `Transient` / `WindowGone` / `WindowHidden`. Each tick opens with a `validate_window_present` pre-capture liveness probe (catches a relaunch or hidden window before the ~140ms capture; logs a `probe_ms` line). On a `WindowGone` / `WindowHidden` signal the loop enters a `recover_window` recovery sub-loop — re-discovering a relaunched RoK or waiting out a hidden window, then resuming — instead of aborting; a successful recovery resets the consecutive-failure streak and the last-match position. Recovery is gated to `ROK_BOT_MAX_TICKS >= 2`, so `ROK_BOT_MAX_TICKS=1` stays an exact one-shot. A pre-landing `/review` caught an unbounded-recovery-spin bug (a crash-looping RoK would recover forever) — fixed with a `RECOVERY_BUDGET` of 3 plus a `consecutive_recoveries` counter that only a genuinely successful tick resets. New `LoopAborted` (exit 21) reason tags: `window_recovery_exhausted`, `visibility_recovery_exhausted`, `recovery_budget_exhausted` (alongside the existing `failure_budget_exhausted` and `signal_handler_install_failed`). This closes the v0.2 P2 window-lifecycle cluster — D14 (window re-discovery after RoK relaunch), D15 (`not_visible` wait-and-retry), the BD reconfig-race snapshot retry loop, and re-validate-window-identity-at-capture-time are all subsumed.

**Caveat — AC-D15 validation pending.** The D15 `not_visible` recovery CODE shipped in v0.2.1, but the empirical check that a `WindowChanged{not_visible}` actually fires for a window on a BetterDisplay virtual display when the system screensaver / display sleep kicks in has NOT been run yet. If `not_visible` never fires in that scenario the recovery path is dead code for the screensaver case. Tracked as a P3 below ("AC-D15: validate not_visible fires under screensaver").

**Part B — needle-swap discrimination fix** (found via `/investigate`). v0.2 shipped `assets/targets/world-button.png` as a magenta-X sentinel placeholder and the docs said an operator had to crop the real world needle before the loop was "functional". `/investigate` proved that wrong: by reading the bot's own `rok-capture-pre.png` / `rok-capture-post.png` it confirmed the click was ALWAYS toggling the city↔world view correctly every tick. Needle-swap verify was blind for two compounding reasons: (1) `world-button.png` was the placeholder, so it was sentinel-skipped; (2) `city-button.png` was a full 180×180 crop of the whole castle-medallion toggle button, and the city↔world toggle keeps an identical gold-rim/blue-circle chrome in both views (only the small inner glyph differs — castle towers vs folded map), so `imageproc`'s non-mean-centered `CrossCorrelationNormalized` let the shared chrome dominate and the city needle NCC-matched the *other* view's button at 0.96 (above the 0.85 threshold). `find_best_needle` therefore returned the same needle index for both views and needle-swap saw idx 0 → idx 0 → `no_swap` every tick. The fix re-cropped BOTH needles as **96×96 tight inner-glyph crops** (castle-towers glyph, folded-map glyph; shared chrome excluded) — cross-view NCC drops 0.96 → 0.88, so `find_best_needle`'s best-of-N picks the correct-view needle each view (correct ≈1.0, wrong ≈0.88). No matcher *code* change — two real discriminative assets plus matcher test updates. The headline consequence: the v0.2 loop is no longer a plumbing smoke test, it is a working continuous loop confirming real city↔world toggles, and the "operator must crop the placeholder" step is done and gone. Live-verified on a Mode 2 BD virtual display: `ROK_BOT_MAX_TICKS=1` → exit 0 with a `needle-swap verify passed` log line, `ROK_BOT_MAX_TICKS=3` → exit 0 with 3 confirmed toggles (was exit 21 `LoopAborted` before the fix).

---

## ✅ DONE — v0.1.6 SHIPPED: Mode 2 click delivery via stealth HID + osascript activation

**Resolved 2026-05-13** by commit `2c96790` (live-confirmed with RoK on a BetterDisplay virtual display: exit 0, pixel_diff 3.19M, castle press observable in pre/post captures, cursor stayed put, RoK didn't visibly raise on built-in).

### What v0.1.6 ships

- `src/click.rs` rewritten: probe cursor → disassociate visible cursor → `osascript` activate (System Events, pid-targeted) → 50ms settle → `CGEvent::post(HID)` `LeftMouseDown` → 80ms gap → `LeftMouseUp` → warp logical cursor back → reassociate. RAII `CursorStealth` guard ensures reassociation on panic/early-return.
- `src/ax.rs` deleted. AX TCC preflight kept in `permissions.rs` (CGEventPost(HID) still requires Accessibility on macOS 10.14+).
- `src/main.rs:142` Mode 2 exit-12 gate dropped. Both `Mode::Visible` and `Mode::Virtual` branch to logging and proceed through the same pipeline.
- `src/display.rs::mode_to_result` + its 2 arm-mapping tests deleted.
- `src/error.rs::RokNotOnPrimary` variant deleted; exit code 12 left unused (not reassigned). New `ClickFailed` reason tags: `activation_failed`, `probe`, `disassociate`, `source`, `down`, `up`.
- Tag `v0.1.5-rc` at commit `5110f6a` marks the ship-blocked AX-press attempt for historical record.

### Why v0.1.5 was ship-blocked (preserved for future Catalyst-Bridge work)

**Bug 1: AX press fires the wrong UI element.** `AXUIElementCopyElementAtPosition` resolves RoK's entire game canvas to a single `AXGenericElement` whose `AXActivationPoint` (read-only) sits at the canvas center. Every `AXPress` fires at that fixed center regardless of the (x, y) passed in. AX tree dump from `spikes/p5-spike --app-tree`:

- `AXApplication "RiseOfKingdoms"` → `AXWindow` → `AXGroup iOSContentGroup` (Catalyst canvas wrapper, NO actions) → single grandchild `AXGroup` (actions `[AXScrollToVisible, AXCancel, AXShowMenu]`, NO AXPress).
- `AXButton` close/minimize/fullscreen + `AXStaticText` title bar — macOS chrome only, not game UI.
- `AXActivationPoint` is **read-only** (verified via `--ax-set-point`: `AXUIElementIsAttributeSettable` returned false).

`/qa` 2026-05-11 confirmed across three live runs: matcher located castle at (441.5, 827.5) score >0.99, AX press FFI succeeded, pixel_diff 580k-680k — but RoK opened a center-screen governor popup at game coord (X:851, Y:271), NOT the castle. Full QA report at `~/.gstack/projects/emulatalk1-rok-bot/hbchuc-main-test-outcome-20260511-222219.md`.

**Bug 2: Catalyst Bridge auto-raises RoK on any synthetic UITouch.** Six click-delivery paths tested against Mode 1 constraints (works when covered, doesn't move cursor, doesn't raise RoK, fires castle):

| Path | Cursor | Cover OK | RoK stays back | Castle fires |
|---|---|---|---|---|
| AX press (v0.1.5) | ✅ | ✅ | ✅ | ❌ wrong target |
| HID + osascript activate | ❌ warps | ✅ | ❌ raises | ✅ |
| Stealth HID + osascript activate | ✅ | ✅ | ❌ raises | ✅ |
| cua-driver default (FocusWithoutRaise + SkyLight) | ✅ | ✅ | ❌ raises | ✅ |
| cua-driver count:3 (no FocusWithoutRaise) | ✅ | ✅ | ❌ raises | ✅ |
| Bare `SLEventPostToPid` only (p5-spike `--skylight`) | ✅ | ✅ | ✅ | ❌ ignored |

**No row satisfies all four constraints on Mode 1.** RoK requires SOMETHING that wakes its event pipeline before it accepts UITouch input, and that wake is what triggers the visible raise. Catalyst's UIKit-on-Mac translation layer auto-raises any window receiving a touch because iOS apps don't have a "stay in background while receiving touch" concept. Not fixable from outside the process.

**v0.1.6 resolution** — flip "Stealth HID + osascript activate" from ❌ to ✅ by moving observability constraints out of scope. On a BetterDisplay virtual display the cursor isn't there, the raise is invisible, and the click is coord-targeted (the AX-press positionless bug doesn't apply because we're not using AX press). p5-spike `--inspect` on the virtual display confirmed `AXPosition` and `AXActivationPoint` update per-display, so even if AX press were used it would target the correct (invisible) display — but the HID + activation path is the cleaner solution since it preserves coord targeting.

### Why this slipped past v0.1.5 defenses

- **Unit tests (140 debug / 141 release):** validate FFI signatures, error mapping, RAII Drop, TOCTOU 4-check, reason-tag string-of-truth. None validate that the press semantically targets the intended UI element on a real RoK install. The AX layer was correct at every layer the tests reached.
- **p5-spike:** verified `AXUIElementPerformAction` returned success and that observable response happened in RoK. Did not verify which UI element fired. The `spikes/p5-spike/README.md` false-positive addendum (commit `60c3ab6`) records this scope gap.
- **Pixel-diff verify:** trivially exceeded by ANY popup or view change. Cannot distinguish "right button fired" from "wrong action triggered."
- **Post-click re-match diagnostic:** finds the castle button still in the backing store at the same coords — because Catalyst auxiliary windows (the popup) live outside RoK's main backing, and the castle button is genuinely still on screen, just unpressed.

### Lessons logged for future work

- **Spike verdicts must verify intended-target outcomes, not just "delivery works."** p3, p4, p5 all conflated the two. Any future click-delivery spike on Catalyst Bridge apps must explicitly press a known coord-bound target and confirm the right action fired.
- **`screencapture -l <wid>` captures RoK's backing store regardless of on-screen overlays** — confirmed via `/qa` test 3. Robustness win for the matcher: exit 15 cannot be triggered by external z-order occluders.
- **Catalyst Bridge AX tree has no per-control nodes** for game UI. Walk-by-identifier is structurally impossible. Coord-based clicks are the only path.
- **`AXPosition` / `AXSize` / `AXActivationPoint` update per-display.** AX bridge is not display-cached; moving RoK to a virtual display correctly updates the canvas geometry. v0.2 procedure in `spikes/p5-spike/README.md` documents the runbook.
- **AX dump utilities at `spikes/p5-spike --inspect | --app-tree | --ax-set-point`** stay as project-record diagnostic tools for future Catalyst app investigations.
- **cua-driver** installed at `~/.local/bin/cua-driver` → `/Applications/CuaDriver.app/Contents/MacOS/cua-driver`, registered as MCP server. SkyLight FFI recipe (`SLEventPostToPid`, `SLPSPostEventRecordTo`, yabai-style `_SLPSGetFrontProcess` focus-without-raise) works against Chromium/AppKit but does NOT defeat Catalyst's auto-raise. Potentially useful for higher-level Mode 2 automation but not load-bearing.

---

## ❌ DEAD — `CGEventPostToPid` migration for screen-position-independent clicks

**Source:** live-smoke 2026-05-11 (exit 19 `not_topmost_at_click` whenever RoK
was positioned such that another window covered the click point on screen),
spike `spikes/p4-spike/`
**Verdict:** Catalyst Bridge silently drops PID-posted events.

The hope was that `CGEvent::post_to_pid(pid)` would deliver clicks to RoK's
process queue, bypassing screen-pixel-level z-order dispatch entirely and
making the v0.1.4 TOCTOU topmost-at-click check unnecessary. Empirical
result (see spike README): the call returns success and the event is
constructed correctly, but RoK's AppKit→UIKit Catalyst translation layer
drops the event somewhere. Zero observable change at the click point (so
NOT screen-routed to iTerm either), zero observable change inside RoK
above the ambient animation noise floor.

Implication: the screen-pixel dependency of v0.1.3's click-delivery is
NOT fixable at the click-delivery layer for iOS-on-Mac apps. Path forward:

1. ~~**Mode 1 production model:** activate RoK before each click.~~
   **RULED OUT by 2026-05-11 click-delivery research** (see P0 above):
   any synthetic UITouch that wakes RoK's event pipeline also triggers
   Catalyst's auto-raise. Tested across 6 distinct delivery paths
   (HID+osascript, stealth HID, cua-driver default, cua-driver count:3,
   bare SLEventPostToPid, AX press) — every path that fires the castle
   also raises RoK. No "click while RoK stays back" mechanism exists
   from outside the process on Catalyst Bridge.
2. **Mode 2 (v0.2):** virtual-display isolation. RoK on a BetterDisplay
   virtual display where no other windows ever live and the user never
   sees RoK's surface. Screen dispatch, z-order topmost, AND auto-raise
   all become trivially correct because nothing observes RoK. This is
   what Mode 2 was always going to do and the spike + 6-path research
   confirm it's the **only** viable path.

Do not re-investigate `CGEventPostToPid` without first reading the spike
README — there's no second pass that produces a different result on
Catalyst Bridge apps until Apple changes the bridge implementation.

---

## ✅ DONE — BetterDisplay setup automation

**Resolved 2026-05-06** via `docs/setup.md` (commit `f8ba718`). Covers Mode 1 (zero
setup), Mode 2 (BetterDisplay walkthrough + Stop-and-output trick), troubleshooting,
and alternative non-BD displays (HDMI dummy plug, Sidecar). Originally specced as
P1.

---

## ✅ DONE — v0.1.8 SHIPPED: In-process ScreenCaptureKit migration

**Resolved 2026-05-15** via commit `85d79bb` (`feat(v0.1.8): SCK in-process capture migration`). v0.1.8 retires the `/usr/sbin/screencapture` CLI shellout and replaces it with `SCScreenshotManager.captureImageWithFilter` from `objc2-screen-capture-kit` 0.3.x. Live-confirmed on Mode 2 BD virtual display: ~140ms steady-state per capture vs ~280-1400ms via the CLI. New `src/cg_bootstrap.rs` (NSApplicationLoad), full rewrites of `src/capture.rs` and `src/window.rs`, `Arc<*Slot>` UAF fix, `O_NOFOLLOW + O_EXCL` symlink-safe PNG write. Ten locked decisions from `/plan-eng-review` (D1-D10 + T1-T4 cross-model tensions) all implemented; 149 unit tests + 5 `#[ignore]`'d integration tests; pre-landing `/review` caught + fixed 5 critical bugs (2 UAFs, symlink TOCTOU, codex-caught regression in the symlink fix, codex-caught error swallowing in find_rok_window); /qa exit 0 on first try. Originally specced 2026-05-06 as the v0.1 day-one path; deferred to v0.1.8 because the v0.1.x ship needed a working spike first.

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

## ✅ DONE — TCC permissions preflight (Screen Recording + Accessibility)

**Source:** /plan-eng-review codex tension 4
**Resolved:** v0.1 (Screen Recording) + v0.1.3 (Accessibility), `src/permissions.rs`.

- **Screen Recording** via the safe `core_graphics::access::ScreenCaptureAccess::preflight()` wrapper. Falls back to `request()` on first-run denial — without that, fresh-install Macs hit a permanent exit 13 with no UI to grant from.
- **Accessibility** via hand-rolled FFI to `AXIsProcessTrustedWithOptions(prompt=true)` (verified in `spikes/p3-spike/` before landing). `peek_accessibility()` exists for non-prompting probes. Required for `CGEventPost` at click site.

Both surface `BotError::PermissionsMissing { which: "..." }` (exit 13). `docs/setup.md` covers both grants in the boot sequence.

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

## P2: v0.2.x — Cut PNG round-trip; matcher consumes RGBA bytes directly (deferred from /plan-eng-review 2026-05-15)

**Source:** /plan-eng-review for v0.1.8 SCK migration (D9). Codex outside-voice echoed the deferral as "PNG round-trip is brittle" (codex #10 framing).
**Effort:** human ~1.5 hours / CC ~30 min
**Depends on:** v0.1.8 SCK migration lands AND v0.2 continuous loop (the consumer that justifies the matcher API change).

v0.1.8 ships SCK in-process capture but still writes a PNG to disk and reloads it for the matcher. The round-trip is ~30 ms encode + ~37 ms decode = ~67 ms per capture, pure overhead at continuous-loop cadence. v0.1.8 keeps the disk PNG for forensic debugging (operator can `open rok-capture-pre.png` to see what the matcher saw); v0.2.x's continuous loop needs that overhead off the hot path.

**Approach for v0.2.x:**
- Add a `MatchInput` enum or two parallel `match_in_path` / `match_in_bytes` entries in `matcher.rs`. Continuous-loop callers pass RGBA bytes directly; one-shot callers (still useful for `verify.rs`'s diagnostic re-match) keep the PNG path.
- `capture.rs` returns `(Vec<u8>, width, height)` for the in-memory path; the PNG-on-disk path becomes an opt-in `--debug-capture-png` CLI flag.
- Matcher's existing `load_haystack` → `GrayImage` conversion stays; just feed it from in-memory bytes instead of a `Path`.
- Test: existing matcher tests use synthesized PNGs; add a parallel `match_from_rgba_bytes` test using the same fixtures decoded once at test-setup time.

---

## P3: v0.2.x+ — Discover display backing scale instead of hardcoding ×2 (deferred from /plan-eng-review 2026-05-15)

**Source:** /plan-eng-review for v0.1.8 SCK migration (D10). Codex outside-voice flagged the canary-only approach (codex #10).
**Effort:** human ~1 hour / CC ~20 min
**Depends on:** v0.1.8 SCK migration lands AND a real operator hits the canary on a non-Retina or scaled display.

v0.1.8 hardcodes `SCStreamConfiguration::setWidth(frame.width as usize * 2)` because every display in the project's current configs is Retina. A canary asserts that captured dims match `frame * 2`; mismatch fires `BotError::CaptureFailed { stage: "cgimage_decode", ... }` with the expected vs actual dims. Correct behavior, wrong coverage: the bot can't actually run on a non-Retina BD or a scaled external display until this is fixed.

**Approach for v0.2.x+:**
- At capture time, look up the window's display: `CGWindowListCopyWindowInfo` → `kCGWindowOwnerPID` + `NSScreen.screens` → matching `NSScreen.backingScaleFactor`. Or use SCK-side: `SCDisplay.frame` × `SCDisplay.pixelWidth / SCDisplay.width`.
- Replace the hardcoded `* 2` with the looked-up scale (typically 1.0 or 2.0).
- Delete the canary or repurpose it to assert the looked-up scale was applied correctly.
- Test: parametrize the capture builder by scale; pin behavior at 1x, 2x, and the rare 3x cases.

Low urgency — fires only when an operator runs on a non-Retina display config. The v0.1.8 canary buys us time to capture real data before designing the fix.

---

## P3: v0.3 — SCStream continuous-frame delegate (deferred from /plan-eng-review 2026-05-15)

**Source:** /plan-eng-review for v0.1.8 SCK migration (D8).
**Effort:** human ~4-6 hours / CC ~1.5 hours
**Depends on:** v0.2 continuous loop (the consumer that justifies SCStream's frame-delegate complexity).

v0.1.8 uses `SCScreenshotManager.captureImageWithFilter` — SCK's one-shot API. Each capture pays the full setup cost (SCContentFilter init, SCStreamConfiguration build, BGRA pixel-format roundtrip). The streaming API (`SCStream` with a frame delegate via `SCStreamOutput`) delivers frames continuously and amortizes setup across captures.

**Approach for v0.3:**
- Introduce `trait Captor` (deferred from v0.1.8 D3) with `fn capture(&self) -> Result<Frame>`. Two impls: `OneShotCaptor` (wraps current v0.1.8 code; kept for the post-click `verify.rs` re-match) and `StreamCaptor` (owns an `SCStream`, returns latest frame).
- StreamCaptor lifecycle: `start()` creates the stream + frame-delegate-driven channel; `capture()` reads the latest frame from the channel (or blocks briefly if no frame yet); `stop()` cleans up. Continuous loop owns one `StreamCaptor` across all ticks.
- Target: sub-50 ms per-tick capture after warmup. v0.1.8 single-shot is ~141 ms; SCStream should hit ~30-50 ms once warmed up (no per-call filter rebuild, no BGRA encode/decode if we keep bytes hot).
- Backpressure: SCK delivers frames continuously even if the consumer is slow. Use a single-slot mailbox (latest-wins) rather than a queue; matches the bot's "use newest frame, drop older ones" semantics.

---

## P2: v0.2 — FFT-based NCC for continuous-loop matching (deferred from /plan-eng-review 2026-05-07)

**Source:** /plan-eng-review for v0.1.2 (CMT-7 + TODO-A) — Codex outside-voice flagged that v0.2's continuous loop will need order-of-magnitude faster matching than v0.1.2's parallel sliding-window.
**Effort:** human ~2 hours / CC ~30 min
**Depends on:** v0.2 capture-pipeline migration to `objc2-screen-capture-kit`

v0.1.2 ships with `imageproc::template_matching::match_template_parallel` — rayon-parallel naive NCC. ~500ms-1s per match on a 2102×1640 Retina haystack with ~80×40 needle. Fine for one-shot v0.1.x demos; blocker for v0.2's per-tick capture loop where we want ≥1Hz match cadence.

**Approach for v0.2:**
- Frequency-domain NCC: FFT both haystack and needle (zero-padded to a common power-of-two size), multiply in frequency domain, inverse FFT to recover the correlation map. O(N log N) instead of O(N²). For our shape, ~50ms instead of ~1s.
- Crate options: `rustfft` (pure Rust, well-maintained) for the FFT primitive; FFT-NCC layer would be hand-rolled (~50 LOC). Or pull in `corrmatch` if its pyramid search reaches maturity.
- Same Match API surface (`Result<Option<Match>>`); v0.1.3+ callers don't change.
- Test against the existing match_in fixtures — score values should be within float-eps of the parallel sliding window's output.

---

## P3: v0.1.3+ — multi-match disambiguation (deferred from /plan-eng-review 2026-05-07)

**Source:** /plan-eng-review for v0.1.2 (TODO-C) — Codex flagged imageproc's lex-smallest tie-break for duplicate-max scores.
**Effort:** human ~30 min / CC ~10 min
**Depends on:** v0.1.3 click synthesis (the policy decision only matters when the bot acts on the match)

`imageproc::template_matching::find_extremes` resolves duplicate maximum scores by returning the lexicographically smallest (x, y). If RoK ever has two identical UI elements visible at once (two city slots, two duplicate buttons), v0.1.2's matcher silently picks the top-left one without any signal that the choice was ambiguous. v0.1.3's click would then act on the top-left without knowing the right was equally good — wrong choice, no diagnostic.

**Approach:**
- Extend `Match` (or add a sibling type) to carry a `runner_up_score: Option<f32>` field. If the second-best score is within ε of the best, surface that to the caller.
- Caller policy: in v0.1.3 click logic, if matches are ambiguous, decide based on context (game state, previous click history). Explicit policy call beats silent lex-tie.
- Test: plant the same needle at three positions in the haystack; assert all three are reported (or that the runner_up_score field signals the tie).

---

## P3: v0.1+ — asset rot detection (deferred from /plan-eng-review 2026-05-07)

**Source:** /plan-eng-review for v0.1.2 (TODO-B) — committed needle is a pixel-exact crop of today's RoK build; UI updates rot the asset silently.
**Effort:** human ~1 hour / CC ~20 min
**Depends on:** real run data (need v0.1.3+'s loop to produce a stream of best_score values)

Our committed `assets/targets/<name>.png` is a pixel-exact crop of today's RoK rendering. If RoK ships:
- A UI redesign (button art changes)
- A DPI/scale change (renderer at different logical resolution)
- An OS theme/blur change that affects how the window is composited

...the asset rots. The bot will start returning `Ok(None)` with low best_score values. Today the operator sees only "target not found" — no signal that the issue is asset rot vs. target legitimately not on screen vs. matcher bug.

**Approach:**
- Track `best_score` over recent runs in a small ring buffer (e.g., last 100 invocations). Persist to `~/.rok-bot/match-history.jsonl` or similar.
- On each run, compare current best_score against the rolling distribution. If sustained drops below the historic floor, log a `warn!(target: "rok_bot", "asset rot suspected")`.
- Could also add a `cargo xtask refresh-assets` workflow that re-crops fixtures from a fresh capture.
- Threshold tuning: empirical, needs real run data to set. Skip until v0.1.3's loop produces enough samples.

---

## P3: v0.1+ — masked NCC for transparent target assets (deferred from /plan-eng-review 2026-05-07)

**Source:** /plan-eng-review for v0.1.2 (TODO-D / Codex open-question) — `to_luma8` discards alpha.
**Effort:** human ~1 day / CC ~2 hours
**Depends on:** a v0.1.x+ target asset that legitimately has transparency (none today)

v0.1.2's matcher converts RGBA8 → Luma8 via `image::DynamicImage::to_luma8()` (ITU-R BT.601 weights). Alpha is silently discarded. v0.1.x's policy: target assets MUST be opaque rectangular crops (documented in `matcher.rs` rustdoc).

If a future target needs transparency — e.g., a button with anti-aliased edges that appears against varying backgrounds — plain NCC scores poorly because the masked-out pixels still contribute to the correlation. Masked NCC is the correct primitive: only correlate pixels where the alpha channel says "this is part of the target."

**Approach options for the future:**
- Pull in `opencv-rust`: cv::matchTemplate with TM_CCOEFF_NORMED + a mask argument. Heavy dep (system OpenCV install) — see existing P2 lessons from v0.1.0.
- Hand-rolled masked NCC over imageproc: read alpha channel into a `mask: GrayImage`, modify the NCC sum to ignore zero-mask pixels. ~50 LOC of math; needs careful float handling.
- Defer until a real use case shows up.

For now: the v0.1.x rustdoc on matcher.rs documents the opaque-only requirement. Future authors who want masked support find this TODO.

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

---

## P3: v0.1.3+ — `--dry-run` flag for click synthesis (deferred from /plan-eng-review 2026-05-08)

**Source:** /plan-eng-review for v0.1.3 (A12 + Codex CMT-7) — defer to TODO since Phase 0 spike + placeholder-needle gate cover most live-click risk in v0.1.3 itself.
**Effort:** human ~30 min / CC ~30 min
**Depends on:** v0.1.3 click landed; first signs of needing safer iteration on coord conversion or matcher tuning against new targets.

v0.1.3 ships click synthesis without a `--dry-run` flag. Risk is gated by (a) Phase 0 spike validating CGEventPost+HID semantics before integration, (b) placeholder needle in `assets/targets/city-button.png` won't match real RoK → exit 15 → no click, (c) target = state-neutral city/world toggle. Once a real RoK crop replaces the placeholder, debugging coord conversion against new targets without firing live events benefits from a dry-run mode.

**Approach:**
- Add `clap = { version = "4", features = ["derive"] }` (or hand-rolled `std::env::args` if avoiding deps) for `--dry-run` flag.
- In `main.rs::run`: if `--dry-run`, log "would click at screen=({x:.2}, {y:.2}) for capture=({cx}, {cy}) with score={score}" and skip `click_at`, exit 0.
- Pin behavior with one integration test that runs the binary with the flag against a synthesized capture and asserts no CGEvent posted (out-of-process verification — set an AX-blocked test fixture, expect no exit-13).

**Why deferred:** v0.1.3 keeps zero CLI surface to match v0.1.x's no-args philosophy. Adding clap or arg parsing is a meaningful surface change worth its own consideration once a real need surfaces.

---

## P3: v0.1.3+ — additional click types (right-click, double-click, drag)

**Source:** /plan-eng-review for v0.1.3 (A12-adjacent + Codex review).
**Effort:** human ~30 min each / CC ~30 min each
**Depends on:** v0.1.3 click landed; future bot flow needs them.

v0.1.3 ships left single-click only via `click_at(point)`. The CGEvent API surfaces all of:
- Right-click: `CGMouseButton::Right` + `RightMouseDown` / `RightMouseUp` event types
- Double-click: same coords, two left-click pairs separated by ~50ms (or set `kCGMouseEventClickState` field to 2)
- Drag: `LeftMouseDown` + `LeftMouseDragged` events along a path + `LeftMouseUp`

Future RoK automation (map panning, troop movement, context menus, multi-action workflows) will need at least drag and double-click. Each is a small extension to `src/click.rs` — same FFI shape, different event types.

**Approach when needed:** extend `click.rs` with `right_click_at(point)`, `double_click_at(point)`, `drag(start, end)`. Keep the same live/pure split (build_*_events as pure builders, *_at as live shims). Add tests pinning event types + button + click-state fields.

**Why deferred:** v0.1.3 hello-world is a left tap, nothing more. Adding click variants without a caller is YAGNI.

---

## ❌ SUPERSEDED — v0.1.3+ Mode 1 terminal/IDE occlusion blocks click

**Source:** v0.1.3 smoke test post-commit. `validate_at_click_site` refused to click 4/4 runs because iTerm2's window frame contained the click point.
**Originally "Resolved" by:** v0.1.5 commits 611bc8a (AX press) + e4b0f68 (drop topmost walk). **That resolution was a false positive** — see P0 above.

p5-spike claimed AX press delivers to Catalyst Bridge apps regardless of z-order. /qa 2026-05-11 proved AX press delivers to a positionless canvas, NOT to the coord-targeted button — the z-order finding was real but irrelevant because the click semantics were wrong. The `learnings/ax-press-works-catalyst` entry is misleading and should be annotated.

In v0.2 Mode 2, occlusion stops mattering because RoK lives on a virtual display nothing can occlude. The v0.1.3 `REASON_NOT_TOPMOST` exit was never the right diagnostic for Catalyst Bridge — auto-raise behavior means "topmost-at-click" is structurally tautological once any click delivery wakes the event pipeline. Defer the topmost-check decision to whatever v0.2 click path lands.

---

## P3: v0.1.3+ — SIGINT during click_at strands mouseDown (no mouseUp posted)

**Source:** `/review` adversarial subagent finding (confidence 6/10, INFORMATIONAL) and Codex adversarial pass (Medium severity).
**Effort:** human ~10 min / CC ~30 min
**Depends on:** v0.1.3 landed; the rare path actually being hit (or v0.2 continuous-loop where the exposure is N×).

`click::click_at` posts `LeftMouseDown`, sleeps 80 ms (`CLICK_GAP_MS`), then posts `LeftMouseUp`. If the process receives SIGINT (operator Ctrl-C) or crashes between the two posts, the down event is delivered to RoK but the up event is not. RoK then sees a long-press / drag-start / pressed-button-held condition with no recovery path until another up event arrives from a real human click or another bot run.

**Approach:**
- Implement a `ClickGuard` struct that holds a fallback "post-up-on-drop" closure. Its `Drop` impl posts `LeftMouseUp` if the normal up post didn't happen. This catches both SIGINT (when std panics into Drop) and panic paths.
- Or: install a SIGINT handler at boot that posts `LeftMouseUp` for the active source before exiting. Heavier, requires global state.
- The Drop guard is preferred because it's local to click_at, type-safe, and covers panic + signal in one mechanism. v0.1.3 doesn't enable `panic = "abort"` (Cargo.toml has `panic = "unwind"` for v0.2 Drop guards), so Drop guards run on panic.

**Why deferred:** the exposure window is 80 ms per click. v0.1 single-shot bot fires one click per run; SIGINT in that 80 ms window is a ~80/3000 = 2.6% chance assuming uniform random termination. v0.2 continuous loop turns the exposure cumulative (N clicks × 80 ms), which is when this needs to land.

---

## P3: v0.1.3+ — 80ms thread::sleep gap can drift under scheduler pressure

**Source:** `/review` adversarial subagent finding (confidence 7/10) and Codex adversarial pass (Medium severity).
**Effort:** human ~10 min / CC ~30 min
**Depends on:** observation of actual drift in real bot runs (v0.2 continuous loop is when this becomes detectable).

`click::click_at` uses `std::thread::sleep(Duration::from_millis(CLICK_GAP_MS))` (80 ms) between down and up events. Under macOS scheduler pressure (App Nap, OS backgrounding, system load, the process being demoted by the kernel), `thread::sleep` can drift to 200 ms+. RoK's anti-bot heuristics may flag both "too fast" and "too slow" synthetic clicks.

**Approach:**
- Replace `thread::sleep` with `mach_wait_until` (Darwin-native, deadline-based via `mach_absolute_time + ns_to_ticks`). Sub-millisecond precision; doesn't drift under load. FFI shape is well-documented.
- Cheaper alternative: keep `thread::sleep` but log a `tracing::warn!` when actual elapsed time exceeds 2× requested (160 ms). Operator-visible diagnostic without changing timing primitive.

**Why deferred:** the spike's verification at 80 ms succeeded against live RoK (3.05M / 5.51M byte diffs in two runs). RoK's anti-bot detection didn't flag the synthetic clicks under spike conditions. This becomes worth fixing when (a) drift is observed in real runs, or (b) v0.2 continuous loop creates enough click volume that even rare drift events accumulate into detectable signal.

---

## P2: v0.1.4+ — server-roundtrip click verify (retry-and-poll at multiple delays)

**Source:** /plan-eng-review for v0.1.4 (Outside Voice F1, confidence 8/10) — RoK click responses span 50ms (button highlight) to 3s (server roundtrip on resource spend, troop dispatch, server sync).
**Effort:** human ~30 min / CC ~30 min
**Depends on:** v0.1.4 landed; first real bot deployment where the operator hits a server-bound click target.

v0.1.4 ships `VERIFY_DELAY_MS = 500` covering UI-local transitions (toggle, dropdown, modal-open). Server-bound RoK actions (build, march, dispatch) show a UI spinner for 1-3s before the state change renders. With fixed 500ms, those clicks capture mid-spinner and false-fail `ClickNotVerified { reason: "screen_unchanged" }` exit 20.

**Approach:**
- Extend `verify::after_state` (or add `verify::after_state_with_retry`) that captures + pixel-diffs at staggered delays (e.g., 300ms / 1000ms / 2000ms). Returns Ok as soon as pixel_diff ≥ threshold; returns Err after all retries.
- Tuning: 3 tiers covers 80% of RoK click types. Each tier is a separate `capture_window` call, so total worst-case latency is 2s + ~300ms capture overhead per retry.
- Test plan: extend the synthetic-fixture integration tests with a "succeeds on second retry" case (pre/intermediate/final PNG triples).
- v0.2 continuous loop calibration: if observed RoK click types cluster (e.g., 80% local + 20% server), bias the retry schedule (more retries near the server-bound delay band).

**Why deferred:** v0.1.4 hello-world scope is UI-local clicks (placeholder needle, eventually city/world toggle). Adding retry-and-poll to v0.1.4 is scope creep without calibration data. v0.2 design phase has real RoK target candidates to drive retry tier tuning.

---

## P3: v0.1.4+ — Mode re-validation at post-capture (cross-display drag edge case)

**Source:** /plan-eng-review for v0.1.4 (Outside Voice F5, confidence 6/10) — operator can drag RoK across displays in the 500ms VERIFY_DELAY_MS window; v0.1.4's `validate_window_present` checks WID + frame but not display residency.
**Effort:** human ~5 min / CC ~15 min
**Depends on:** v0.1.4 landed; race observed in real runs OR v0.2 continuous loop where exposure is N×.

v0.1.4 splits the TOCTOU check: `validate_at_click_site` (3-check: WID + frame + topmost) runs pre-click; `validate_window_present` (2-check: WID + frame) runs pre-post-capture. The 2-check catches in-display drag (frame moved within tolerance fails). It does NOT catch "RoK frame moved off the built-in display to a virtual display" — frame check might pass if drag distance < FRAME_TOLERANCE_POINTS, but Mode classification (`detect_mode` → `mode_to_result`) has now switched.

**Approach:**
- After `validate_window_present(&window)?` and before `capture_window` for the post path, call `detect_mode(&window)?` and `mode_to_result(mode)?`. Returns `BotError::RokNotOnPrimary` exit 12 if RoK moved to a virtual display mid-run.
- Reuse the existing detect_mode + mode_to_result pair from main.rs::run boot sequence — same code, just called at second site.
- Test: extend window::tests with a synthetic `validate_window_present_inner` case where the window's frame is barely within tolerance but center pixel landed on a different display (need a paired display fixture).

**Why deferred:** edge case. Operator deliberately dragging RoK across displays mid-bot-run is "config error" pattern — next run exits 12 anyway with the boot-time check. v0.2's capture-pipeline migration (objc2-screen-capture-kit, P1 TODO) reshapes the surrounding TOCTOU surface; revalidate Mode there.

---

## P3: v0.1.4+ — empirical calibration of PIXEL_DIFF_REJECT_THRESHOLD

**Source:** /plan-eng-review for v0.1.4 (Outside Voice F3 follow-on) — pixel-diff threshold of 1000 is a guess; needs real-data calibration.
**Effort:** human ~1 hour run + ~30 min analysis / CC ~30 min for script + ~15 min for analysis
**Depends on:** v0.1.4 landed AND real RoK target needle in `assets/targets/city-button.png` AND ~50 live runs producing logged pixel_diff values.

v0.1.4 ships `PIXEL_DIFF_REJECT_THRESHOLD = 1000 pixels` as a starting point. The actual distribution of pixel_diff values for the four scenarios is unknown:
- (a) RoK quiescent + no-effect click (false-positive risk on threshold too low)
- (b) RoK quiescent + successful click (true-positive band — what does a real "click landed" look like?)
- (c) RoK animating (water/troops/weather) + no-effect click (false-positive risk on animation alone)
- (d) RoK animating + successful click (typical real-world case)

Without data, 1000 could be too low (1000 pixels = 0.03% of a 2102×1640 capture; trivially exceeded by water shimmer alone) or too high (subtle UI button-press flash might produce 500-pixel diff, getting rejected).

**Approach:**
- v0.1.4's `tracing::info!` log line already emits pixel_diff value on every run. Operator collects 20-50 runs covering scenarios (a)-(d) via `RUST_LOG=info cargo run --release 2>&1 | tee runs.log`.
- Parse the log via shell or a small Rust xtask: extract pixel_diff values per scenario, compute distributions, set threshold at 95th percentile of (a)+(c) (false-positive ceiling). Expected: somewhere in the 1000-50000 range based on p2-spike/p3-spike data (3-6M byte-diff with ambient animation; pixel-diff is proportionally smaller but still significant).
- Update `PIXEL_DIFF_REJECT_THRESHOLD` constant + the `pixel_diff_reject_threshold_in_sane_range` unit test bounds.

**Why deferred:** v0.1.4 ships with conservative 1000 + diagnostic logging. Calibration without real-data is wasted motion. Real runs gate this naturally.

---

## P3: v0.1.4+ — diff image artifact save (pre/post/visual-diff PNG for operator debugging)

**Source:** /plan-eng-review for v0.1.4 (Outside Voice F6, confidence 7/10) — verify hard-fails with a 1-reason tag; operator gets `exit 20 "screen_unchanged"` with no diagnostic about WHERE the screen didn't change.
**Effort:** human ~30 min / CC ~30 min
**Depends on:** v0.1.4 landed AND first observed verify failure in practice that confused the operator.

v0.1.4 saves `rok-capture-pre.png` + `rok-capture-post.png` at project root; operator can open both in Preview to manually diff. Outside Voice argued for a third artifact: `rok-capture-diff.png` — a visual diff image highlighting which pixels changed (e.g., red overlay on changed pixels, original luma elsewhere). Operator opens diff.png once and sees the answer.

**Approach:**
- On `ClickNotVerified` fail (and maybe always, for diagnostic), generate `rok-capture-diff.png` showing the pixel-diff regions. The `image` crate already in deps supports this — build a new GrayImage where `diff_image[i] = if pre.luma()[i] != post.luma()[i] { 255 } else { pre.luma()[i] / 4 }` (highlight changed pixels white, dim everything else). Save as PNG via `diff.save(&diff_path)?`.
- Path: `rok-capture-diff.png` at project root; `.gitignore` already covers `rok-capture-*.png` wildcard after v0.1.4.
- Wire into `verify::after_state` — generate diff unconditionally before the verdict, log path on both success and failure.
- Test: add an integration test that writes synthetic pre+post fixtures with known diff regions, asserts the diff image contains the expected highlight pattern.

**Why deferred:** YAGNI for v0.1.4 hello-world. Operator can still manually diff pre+post in Preview. Build when first real failure pattern observed and the manual-diff workflow proves friction.

---

## ✅ DONE — Verify AX press tolerates negative-origin CG coords

**Resolved 2026-05-13** via `spikes/p5-spike --inspect` against RoK on a BetterDisplay virtual display (frame `(-1125, 92, 1051, 820)`).

Outcome at canvas-center probe `(-599.5, 502)`:
- `AXPosition = (-1125, 124)` — bridge updates to new display origin (was `(391, 93)` on built-in).
- `AXSize = (1051.82, 788.48)` — unchanged.
- `AXActivationPoint = (-599.09, 518.24)` — bridge updates to new canvas center (was `(916.91, 487.24)` on built-in).
- `AXChildren = ()` — still no per-control nodes (Catalyst Bridge structural).

Conclusion: the AX bridge is **not** display-cached; coord-bound AX queries work correctly across negative-origin displays. The v0.1.5 AX-press positionless bug remains (`AXPress` fires at `AXActivationPoint`, not the caller's coords) but it's no longer a Mode 2 blocker because v0.1.6 reverted from AX press to HID + activation, which IS coord-targeted. The result also unblocks any future AX-based work on Mode 2 — global CG coords are the right input.

Procedure runbook preserved at `spikes/p5-spike/README.md` "v0.2 procedure — verify AX behavior with RoK on a BetterDisplay virtual display" for future Catalyst-Bridge investigations.

---

## ✅ DONE — v0.1.7 SHIPPED: ROI-cropped NCC cuts match latency ~22s → ~440ms (50×)

**Resolved 2026-05-14** by commit `7cb1e5a`, tag `v0.1.7`. Live-confirmed against RoK on a BetterDisplay virtual display: match at capture-pixel (129, 1460) score 0.922, screen point (-1015.5, 867), pixel_diff 113K, exit 0, click visually confirmed. NCC dropped from 22,280ms (v0.1.6 baseline) to 442ms — 50× speedup. End-to-end wall: 46s → 2.7s.

### What v0.1.7 ships

- `src/matcher.rs`: new `Roi` struct, four `CASTLE_BUTTON_ROI_FRACTION_*` consts (X=0.0, Y=0.75, W=0.20, H=0.25), `castle_button_roi(w, h)` constructor with edge clamping, three public entries (`find_target`, `find_target_in_roi`, `find_target_in_castle_roi`) sharing `find_target_impl` via a closure that receives haystack dims and returns the ROI. `match_in` is unchanged (still pure); the offset is restored at the `Match` boundary in `find_target_impl`. `roi_offset_x`/`roi_offset_y` added to the timing log so operators tuning the ROI from the no-match warn always see the full-capture-space translation.
- `src/main.rs`: live pipeline switched to `find_target_in_castle_roi`. `capture_ms` added to pre/post `screencapture` log lines for v0.2 profiling.
- `src/verify.rs`: post-click diagnostic re-match uses the same castle-ROI entry. Bounded to the same sub-region as the pre-click match.
- 12 new tests (148 total): fraction-const pinning, observed-match containment for 2102×1640, bounds across DPI configs, zero-capture degeneracy, full-capture-space offset, capture_dims preservation, ROI exclusion of out-of-ROI needles, OOB ROI rejection, zero-size ROI rejection, end-to-end castle-ROI matching at observed coords, full vs castle-ROI cross-check (both entries agree on coords + capture_dims when needle is in-quadrant).

### Why ROI not FFT (and why FFT-NCC is now indefinitely deferred)

The original TODO listed three mitigation options: (a) shrink the haystack region, (b) FFT-based NCC via `opencv-rust`, (c) custom SIMD path. Profiled the current match first: 22.28s for NCC, 37ms for haystack decode, ~0ms for needle decode — NCC is 99.7% of match time. Back-of-envelope `O(heatmap_pixels × needle_pixels)` for 2102×1640 vs 180×180 is ~91 billion ops per pass. Heatmap was 2.81M positions.

Picked option (a) "shrink the haystack region" because the castle medallion lives consistently in the bottom-left quadrant across Mode 1 (built-in Retina 2102×1640) and Mode 2 (BD virtual 2102×1640) captures. Cropping to `(0..420, 1230..1640)` (a 20% × 25% ROI of the capture) dropped the heatmap to 56K positions — 50× reduction in NCC work, matching the projected speedup exactly. FFT-NCC (option b) is now indefinitely deferred: ROI alone brings single-shot pipeline to ~2.7s end-to-end and per-tick NCC well under v0.2's continuous-loop budget. opencv-rust dep adds significant build complexity for no remaining latency need.

### Known limitations carried into v0.2

- **ROI < needle on sub-900×720 captures.** Current Mode 2 BD capture is 2102×1640 (well above threshold), so this doesn't fire in practice. v0.2 continuous loop should add a full-frame fallback (search full haystack when ROI returns `TargetTooLarge`).
- **ROI-local false-positive risk.** Pre-ROI, the full-capture NCC compared every candidate against the whole image; any false positive had to beat the real castle's 0.98 score. Post-ROI, a false positive only has to clear 0.85 inside the ROI. Real-needle distinctive shape + observed live score of 0.92 makes this unlikely in practice. Mitigate empirically: tighten `MATCH_THRESHOLD` from 0.85 toward 0.90 after ≥10 live runs show score distribution.
- **MATCH_THRESHOLD calibration deferred.** Same as above — need real-data points before tightening.

---

## ❌ DEAD — AX messaging timeout calibration

**Verdict 2026-05-13:** `src/ax.rs` deleted in v0.1.6 along with its `AX_MESSAGING_TIMEOUT_SECONDS` constant. v0.1.6 click delivery is `CGEvent::post(HID)`, not AX press, so there is no AX RPC to time out. If a future revision re-introduces AX for some other purpose, calibrate then.

## P3: v0.1.6+ — `ACTIVATION_SETTLE_MS` / `CLICK_GAP_MS` empirical calibration

**Source:** v0.1.6 ship checkpoint. The two timing constants in `src/click.rs` (`ACTIVATION_SETTLE_MS = 50`, `CLICK_GAP_MS = 80`) are educated guesses derived from p5-spike behavior, not empirical RoK measurement against the full Mode 2 pipeline.
**Effort:** human ~20 min run-and-tail / CC ~10 min for the calibration log script
**Depends on:** ≥20 real Mode 2 runs across game states (city view, world view, mid-loading, server-bound action).

Too tight `ACTIVATION_SETTLE_MS`: RoK ignores the HID tap because the Catalyst Bridge translation layer hasn't applied the foreground change yet. Too tight `CLICK_GAP_MS`: RoK's event loop collapses the down+up pair into a no-op. Too loose either: visible per-click latency that accumulates in v0.2's continuous loop.

**Approach:**
- Add per-run timing logs at the activate/down/up boundaries.
- Collect 20+ Mode 2 runs covering varied game states; record the success-rate vs. timing relationship.
- Pick the smallest values that hold ≥95% success.
- Update the constants + the `in_sane_range` pin tests if needed.

**Why deferred:** v0.1.6 ships with conservative initial values that passed a live run on 2026-05-13. Calibration without volume of data would be motion. Re-open when v0.2's continuous loop produces enough samples to differentiate "click landed" from "click missed because of timing."

---

## P3: v0.2+ — anti-bot cadence jitter for the continuous loop (deferred from /plan-eng-review 2026-05-16)

**Source:** /plan-eng-review of the v0.2 continuous loop — outside-voice (Claude subagent) finding #3.
**Effort:** human ~2 hours / CC ~40 min
**Depends on:** v0.2 continuous loop landed; becomes load-bearing only when the loop drives a real account (a real in-game task — the D1 option-B/C direction).

The v0.2 loop clicks at a fixed ~500ms+ cadence, at the exact same screen coordinate every tick, with fixed `ACTIVATION_SETTLE_MS` / `CLICK_GAP_MS` internal delays, and re-runs `osascript` activation every tick. Perfectly periodic taps with zero coordinate variance are a textbook automated-clicker signature. v0.2's plumbing milestone targets the state-neutral city/world toggle, so detection stakes are ~zero — but a loop later pointed at a real account inherits the fingerprint.

**Approach:**
- Make the timing constants ranges, not points: jitter the inter-tick delay, `ACTIVATION_SETTLE_MS`, `CLICK_GAP_MS` by a randomized ± fraction.
- Jitter the click coordinate within the matched needle's bounds (a few px off the exact match centre) so taps aren't pixel-identical.
- Skip the per-tick `osascript` activation when RoK is already frontmost (track it) to cut a redundant observable.
- Inject the RNG (or a seed) so `cargo test` stays reproducible. This is why it was NOT built into v0.2 — it would compromise the pure-function test determinism the v0.2 loop locked in (D9/T1).

---

## P3: v0.2.x — AC-D15: validate not_visible fires under screensaver (deferred from v0.2.1 ship)

**Source:** v0.2.1 ship — the D15 `not_visible` recovery code shipped but the empirical acceptance check (AC-D15) was not run.
**Effort:** human ~15 min (run the loop, trigger the screensaver, observe).
**Depends on:** v0.2.1 shipped.

v0.2.1's `recover_window` sub-loop treats a `WindowHidden` (`WindowChanged{not_visible}`) signal by waiting the window out instead of aborting. The recovery code is in and unit-tested, but it has NOT been confirmed that a `WindowChanged{not_visible}` actually *fires* for a window on a BetterDisplay virtual display when the system screensaver / display sleep kicks in. If `not_visible` never fires in that scenario, the screensaver branch of the recovery path is dead code and Mode 2 still wouldn't survive a real idle.

**Approach:**
- Run the loop on a Mode 2 BD virtual display with `ROK_BOT_MAX_TICKS` high enough to outlast the screensaver delay.
- Trigger the screensaver / display sleep manually (hot corner, or wait out the idle timer).
- Observe: does a tick log a `WindowHidden` / `not_visible` signal and enter recovery, then resume when the screensaver dismisses? Or does the loop sail through unaffected (capture still succeeds), or fail some other way?
- If `not_visible` never fires, close AC-D15 as a non-issue and note that Mode 2 capture survives a screensaver natively. If it fires, confirm the recovery sub-loop rides it out and the run resumes.

---

## P3: v0.2.x — slim the L4 pre-capture liveness probe if per-tick timing shows it heavy (deferred from /plan-eng-review 2026-05-17)

**Source:** /plan-eng-review of the v0.2.1 window-lifecycle design — decision D9 (outside-voice finding #2).
**Effort:** human ~30 min / CC ~15 min
**Depends on:** v0.2.1 shipped AND per-tick probe timing logs collected from real runs.

v0.2.1's L4 runs a full `validate_window_present` at the start of every loop tick to detect a RoK relaunch (`wid_gone`) or a hidden window (`not_visible`) before the ~140ms capture. `validate_window_present` does two `copy_window_info` CGWindow enumerations (`OnScreenOnly` + `All`), each parsing every window on the system. D9 kept the full probe (it catches `not_visible` pre-capture, avoiding a wasted capture on a hidden window) and added a per-tick timing log line alongside `capture_ms` so the cost is observable.

If real runs on a busy desktop show the probe is heavy relative to the per-tick budget, slim it:

**Approach:**
- Probe only `kCGWindowListOptionAll` for the `(WID, PID)` pair — one enumeration instead of two. This still catches a relaunch (`wid_gone`) but loses pre-capture `not_visible` detection: a hidden RoK would then burn a full capture + match each tick until the post-click `validate_window_present` notices.
- Alternatively, add a cheaper liveness primitive (e.g., a PID-only `kill(pid, 0)` liveness check) as a fast pre-filter, falling back to the full `validate_window_present` only when the fast check is ambiguous.
- Decide the tradeoff with timing data in hand — do not slim speculatively.

**Why deferred:** the probe is expected to be a few ms against a ~140ms capture; slimming it without data is premature, and the cheaper variant trades away a real correctness property (pre-capture hidden detection). The v0.2.1 timing log gates this naturally.
