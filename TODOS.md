# TODOS

Items deferred from planning sessions. Each entry should be self-contained enough that picking it up months later is feasible.

---

## 🚨 P0: v0.1.5 SHIP BLOCKER — Mode 1 click delivery structurally non-viable on Catalyst Bridge; pivot to v0.2 Mode 2

**Source:** `/qa` 2026-05-11 live click-target verification (3 end-to-end runs against running RoK) + same-day click-delivery research across 6 distinct paths.
**Status:** Blocks `/ship` of v0.1.5. 8 commits sit on local `main` (carryover `5b56906` + `b1ab9ac`, then v0.1.5 chain `2fe892b`..`5110f6a`), unpushed. **Do not push until v0.2 Mode 2 lands.**
**Effort:** RESOLVED — Mode 1 ruled out. Remaining work is v0.2 Mode 2 (BetterDisplay virtual display) scope, tracked in the P2 v0.2 entries below.

### Bug 1: AX press fires the wrong UI element

`src/ax.rs::press_at` calls `AXUIElementCopyElementAtPosition(app_ref, x, y)` then `AXUIElementPerformAction(element, kAXPressAction)`. Every unit test passes. FFI signatures verified against Apple SDK. p5-spike showed "AX press works on Catalyst Bridge." Live runs show exit 0 with pixel_diff well above threshold.

**But the bot has never actually pressed the intended button.** Three runs targeted the castle/world toggle button at screen (441.5, 827.5) — the matcher correctly located it at score >0.99. The AX press FFI succeeded. The screen changed substantially (pixel_diff 580k-680k). And what RoK actually did was open a center-screen governor profile popup for the game-map entity at coord X:851 Y:271. The castle button is visible in the post-click capture, untouched, no press highlight. Three runs, three identical wrong outcomes. Full QA report at `~/.gstack/projects/emulatalk1-rok-bot/hbchuc-main-test-outcome-20260511-222219.md`.

**Root cause (confirmed via `spikes/p5-spike/ ... --inspect | --app-tree | --ax-set-point`).** Mac Catalyst Bridge maps RoK's entire game canvas to a single `AXGenericElement` covering the full window content area. Tree dump:

- `AXApplication "RiseOfKingdoms"`
  - `AXWindow "RiseOfKingdoms" (SceneWindow)` with 5 children:
    - `AXGroup iOSContentGroup` (frame 391,93 1051x788) — Catalyst Bridge canvas wrapper. NO actions. 1 grandchild `AXGroup` (1051.82×788.48, actions `[AXScrollToVisible, AXCancel, AXShowMenu]`, NO AXPress).
    - `AXButton AXCloseButton / MinimizeButton / FullScreenButton` — macOS window chrome, not game UI.
    - `AXStaticText` — title bar.
  - `AXMenuBar`.

`AXUIElementCopyElementAtPosition` at three different game-canvas coords — lower-left (441.5, 827.5), center (916, 471), mid-bottom (800, 700) — all return the same `AXGenericElement` with identical `AXActivationPoint = (916.91, 487.24)` (window center). `AXPress` fires at that fixed AXActivationPoint, NOT at the (x, y) passed to `CopyElementAtPosition`. `AXActivationPoint` is **read-only** (verified via `--ax-set-point` mode: `AXUIElementIsAttributeSettable` returned false). The parameterized attribute list contains only `AXReplaceRangeWithText` — no hit-test-by-point parameterized attribute exists. **AX hierarchy walk is not viable** because there are no per-control nodes to walk.

### Bug 2: Catalyst Bridge auto-raises RoK on any synthetic UITouch — no Mode 1 click path satisfies all constraints

After ruling out AX repair, researched and tested every reasonable click-delivery mechanism. Six paths tested against the user's hard constraints (works when covered, doesn't move cursor, doesn't raise RoK, fires castle):

| Path | Cursor | Cover OK | RoK stays back | Castle fires |
|---|---|---|---|---|
| AX press (v0.1.5 current) | ✅ | ✅ | ✅ | ❌ wrong target |
| HID + osascript activate | ❌ warps | ✅ | ❌ raises | ✅ |
| Stealth HID + osascript activate | ✅ | ✅ | ❌ raises | ✅ |
| cua-driver default (FocusWithoutRaise + auth-signed SkyLight) | ✅ | ✅ | ❌ raises | ✅ |
| cua-driver count:3 (no FocusWithoutRaise) | ✅ | ✅ | ❌ raises | ✅ |
| Bare `SLEventPostToPid` only (p5-spike `--skylight`) | ✅ | ✅ | ✅ | ❌ ignored |

**No row satisfies all four constraints.** The bare `SLEventPostToPid`-only test was the cleanest minimal recipe (no activation, no focus dance) and RoK simply did not react — meaning RoK requires SOMETHING that wakes its event pipeline before it accepts UITouch input, and **that wake is what triggers the visible raise**. Catalyst's UIKit-on-Mac translation layer auto-raises any window receiving a touch because iOS apps don't have a "stay in background while receiving touch" concept. This is not fixable from outside the process.

The cua-driver recipe (SkyLight private FFI: `SLEventPostToPid`, `SLPSPostEventRecordTo`, yabai-style `_SLPSGetFrontProcess` focus-without-raise) is technically real and correct — confirmed via source review of `Sources/CuaDriverCore/Input/{SkyLightEventPost,FocusWithoutRaise,MouseInput}.swift`. It works against Chromium/AppKit. It does NOT defeat Catalyst's auto-raise.

### Why this slipped past existing defenses

- **Unit tests (140 debug / 141 release):** validate FFI signatures, error mapping, RAII Drop, TOCTOU 4-check, reason-tag string-of-truth. None validate that the press semantically targets the intended UI element on a real RoK install. The AX layer is correct at every layer the tests reach.
- **p5-spike:** verified `AXUIElementPerformAction` returned success and that observable response happened in RoK. Did not verify which UI element fired. Update `spikes/p5-spike/README.md` to record this scope gap (same pattern as p3/p4: "delivery works" ≠ "intended target fires").
- **Pixel-diff verify:** trivially exceeded by ANY popup or view change. Cannot distinguish "right button fired" from "wrong action triggered."
- **Post-click re-match diagnostic:** finds the castle button still in the backing store at the same coords — because Catalyst auxiliary windows (the popup) live outside RoK's main backing, and the castle button is genuinely still on screen, just unpressed.

### Resolution: v0.2 Mode 2 (BetterDisplay virtual display)

Mode 2 was always the planned v0.2 milestone. With Mode 1 click delivery ruled out across 6 paths, Mode 2 is now the **only** viable resolution. Structural fix: RoK on a BetterDisplay virtual display means the auto-raise still happens, but on a display the user can't see, with no user cursor present, and no other apps competing. All four constraints become trivially satisfied because the constraints themselves are observability-driven.

**Permissions are not the gate.** Accessibility + Screen Recording were already granted to terminal and cua-driver throughout. No code-signing or entitlement work required for v0.2.

### What survives from v0.1.5

- `2fe892b` C1 — CGWindowList centralization. Independent of click mechanism. Correct.
- `e4b0f68` C2 — 4-check TOCTOU + hidden-Space detection. Confirmed working by `/qa` test 2 (exit 19 `not_visible` clean). Independent of click mechanism. Correct.
- The expanded `spikes/p5-spike/src/main.rs` diagnostic modes (`--inspect`, `--app-tree`, `--stealth`, `--ax-set-point`, `--skylight`) are the most useful surviving artifacts for v0.2 work. Port pieces into rok-bot proper if needed.

### What needs reframing

- `43b259b` C3, `611bc8a` C4, `1ac228e` C5, `5110f6a` C6 describe AX as v0.1.5's working click delivery. C4 doesn't deliver the user-facing contract; C3 / C5 / C6 inherit that vocabulary. Two options:
  - **Pin commits as `v0.1.5-rc, ship-blocked`**, leave unpushed on `main`, branch v0.2 from there. Cheapest, preserves the AX investigation as a project-record artifact.
  - **Revert C3-C6.** Keep C1-C2. Rewrite docs + error vocabulary for whatever v0.2's click delivery layer ends up being (likely the AX press path, since on a virtual display its targeting bug becomes a non-issue — the popup that opens at AXActivationPoint is invisible to the user; what matters is "did RoK observably do something" and the matcher can re-orient from the new state).
- `~/.gstack/projects/emulatalk1-rok-bot/hbchuc-main-test-outcome-20260511-222219.md` needs an appendix noting the cua-driver / SkyLight Mode 1 dead-end discovered after the report was written.
- README, CLAUDE.md, `docs/setup.md` currently describe the AX path as v0.1.5's working behavior. Either reword as "experimental, ship-blocked" or wait until v0.2 lands and write fresh.

### Crucial context for resume

- **Unit tests pass but the bot doesn't click the intended button.** Don't trust unit tests as evidence the bot works end-to-end — visual verification against real RoK is the only way. p3, p4, p5 spikes all had false-positive scope ("delivery works" not "intended target fires"). For v0.2, every spike verdict must include intended-target verification, not just "RoK reacted."
- **`screencapture -l <wid>` captures RoK's backing store regardless of on-screen overlays** — confirmed during `/qa` test 3 (covered castle button with iTerm; matcher still found it at 0.9951). This is a robustness win, not a bug. Exit 15 (TargetNotFound) cannot be triggered by external overlays.
- **RoK pid changes per launch.** Across this session: 75094, 75838, 78028, 81777. Always re-query via `pgrep -if "rise.*kingdoms"` or `cua-driver call list_windows`.
- **Window frame across launches: (391, 61, 1051, 820).** Stable while RoK on the built-in display. Castle button at screen (441.5, 827.5); image-pixel center (101, 1533) in the 2x Retina capture (2102×1640); window-local logical (50.5, 766.5).
- **cua-driver is installed at `~/.local/bin/cua-driver` → `/Applications/CuaDriver.app/Contents/MacOS/cua-driver`** and registered as an MCP server. May be useful for v0.2 high-level automation but probably not load-bearing.
- **AX dump utilities live in `spikes/p5-spike --inspect | --app-tree | --ax-set-point`** for future Catalyst app investigations.

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

## ✅ DONE — TCC permissions preflight (Screen Recording + Accessibility)

**Source:** /plan-eng-review codex tension 4
**Resolved:** v0.1 (Screen Recording) + v0.1.3 (Accessibility), `src/permissions.rs`.

- **Screen Recording** via the safe `core_graphics::access::ScreenCaptureAccess::preflight()` wrapper. Falls back to `request()` on first-run denial — without that, fresh-install Macs hit a permanent exit 13 with no UI to grant from.
- **Accessibility** via hand-rolled FFI to `AXIsProcessTrustedWithOptions(prompt=true)` (verified in `spikes/p3-spike/` before landing). `peek_accessibility()` exists for non-prompting probes. Required for `CGEventPost` at click site.

Both surface `BotError::PermissionsMissing { which: "..." }` (exit 13). `docs/setup.md` covers both grants in the boot sequence.

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

## P2: v0.2 — re-validate window identity at capture time (deferred from /review cf45fd5)

**Source:** /review of v0.1.1 capture milestone — multi-confirmed by Claude adversarial subagent + Codex adversarial (D3 in the AskUserQuestion batch)
**Effort:** human ~30 min / CC ~10 min
**Depends on:** v0.2 migration to `objc2-screen-capture-kit`

`src/window.rs::find_rok_window` validates RoK by owner+title+bundle-ID prefix during enumeration, but `src/main.rs::run` then captures using only the cached `window.id` 50–150ms later. macOS aggressively reuses CGWindowIDs after a window closes — if RoK quits mid-flow (player reflex-closing the game, a crash, BD reconnect dance), the same u32 can be reassigned to another app's window. Best case `screencapture` fails with non-zero exit and we see exit 14. Worst case we capture a different app's window: still a valid PNG, the bot logs success, and v0.1.2 template-match silently misses with no signal it captured the wrong thing.

Not addressed in v0.1.1 because v0.2's capture-pipeline migration to `objc2-screen-capture-kit` reshapes this entirely — any revalidation we add now is throwaway.

**Approach for v0.2:**
- At the SCStream / SCContentFilter setup step, re-resolve the target by owner+title+bundle-ID prefix rather than by cached CGWindowID. SCK's content-filter API takes an `SCWindow` reference, not a raw window ID, so identity is bound at use-time by construction.
- If we still want CGWindowID for diagnostics, add a re-validation call: `CGWindowListCreateDescriptionFromArray([wid])` immediately before capture, fail with a new variant (e.g., `WindowVanished`) if owner/title/bundle no longer match.

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

## P2: v0.2 prerequisite — verify AX press tolerates negative-origin CG coords

**Source:** v0.1.5 ship checkpoint. The v0.1.5 unit tests prove `validate_inner` handles negative-origin CG coords (BetterDisplay virtual displays land at `x = -1051` per P3 spike); the AX press path was only smoke-tested on positive coords during p5-spike (RoK on the built-in display).
**Effort:** human ~10 min / CC ~5 min spike rebuild
**Depends on:** v0.1.5 landed; BetterDisplay virtual display attached and RoK migrated to it (v0.2 setup work — currently exits 12).

`AXUIElementCopyElementAtPosition` takes `float x, float y` and the AX framework's coordinate system is documented as "top-left relative screen coordinates." Whether that means *global* CG screen space (which spans negative coords across virtual displays) or *the displayed Space's own origin-anchored coords* is not explicit in Apple's docs. p5-spike validated positive coords on the built-in display only. Before v0.2 enables Mode 2, smoke-test:

1. Attach a BetterDisplay virtual display in `System Settings → Displays`.
2. Drag RoK to the virtual display.
3. Run `cargo run --release -- spike-mode-2-stub` (or hand-call `ax::press_at` from a tiny binary with the virtual-display window coords). Expected: press lands.
4. If it doesn't land: AX coord space is per-Space, and v0.2 needs to translate global CG → Space-local before passing to AX.

**Why deferred:** v0.1.5 explicitly fails at `Mode::Virtual` with exit 12. Until v0.2 is in progress, the negative-coord scenario can't actually reach the AX path. Spike now would be hypothetical; spike during v0.2 is real-cost-vs-real-value.

---

## P2: v0.2 blocker — 23-second NCC match latency on Retina 2102×1640 captures

**Source:** /plan-eng-review for v0.1.5 (2026-05-11). Observed during v0.1.4 smoke test: `imageproc::match_template_parallel` with `CrossCorrelationNormalized` takes ~23s wall on M-series for the standard Retina haystack against the 56,900-byte city-button needle.
**Effort:** human ~1 hour audit / CC ~4-6 hours for FFT-NCC swap
**Depends on:** v0.2 continuous-loop scope (P2: FFT-based NCC for continuous-loop matching, line ~146) — same root cause.

v0.1 single-shot tolerates 23s per run (operator waits, sees verify success/failure). v0.2 continuous loop fires every few seconds; 23s per match is fatal — the bot spends 90%+ of wall time inside the matcher, can't catch UI state changes that resolve faster than the match window, and CPU pegs at ~80% sustained.

**Approach:**
- Empirical first: profile the current match to confirm 23s is NCC, not capture/decode. `cargo flamegraph --release -- <args>` against a known-input run.
- Then either: (a) shrink the haystack region (only NCC against the expected bottom-right corner where city-button lives — see P3 "v0.1.3+ multi-match disambiguation" for related cropping work), OR (b) switch to FFT-based NCC (`opencv-rust` cv::matchTemplate with TM_CCOEFF_NORMED via FFT; heavy dep but production-proven), OR (c) custom SIMD path via `wide` / `pulp`.
- Pick after profiling tells us where the time goes.

**Why deferred:** v0.1.5 ships single-shot; 23s is annoying but tolerable. The fix work overlaps significantly with v0.2's continuous-loop matcher rewrite, and doing it twice would be wasted motion.

---

## P3: v0.1.5+ — AX messaging timeout calibration

**Source:** v0.1.5 C4 implementation (`src/ax.rs::AX_MESSAGING_TIMEOUT_SECONDS = 2.0`). The 2.0s value came from /plan-eng-review codex outside-voice recommendation, not empirical RoK measurement.
**Effort:** human ~10 min / CC ~10 min
**Depends on:** v0.1.5 landed; ≥20 real runs where the operator observed AX response latency under variable RoK state (idle, mid-animation, mid-loading-screen, server-bound action).

Too tight: a busy-but-not-hung RoK (mid-loading-screen, server-bound click) would false-fail with `ax_timeout` exit 18 even though the press would have eventually succeeded. Too loose: a truly hung target wedges the bot for the full timeout window per click. 2.0s is a conservative initial guess covering "RoK is responsive" without making "RoK is wedged" too patient.

**Approach:**
- v0.1.5's tracing logs already emit AX call timings (the per-step `AXUIElementCopyElementAtPosition failed` / `...PerformAction failed` warn lines, plus the `AX press dispatched` info on success). Add explicit start/end timestamps if needed.
- Collect ~20 runs across scenarios. Compute p50/p95/p99 of (a) successful press call duration, (b) any `ax_timeout` failure duration.
- Set `AX_MESSAGING_TIMEOUT_SECONDS = max(p99 success, 1.5) + 0.5s slack` rounded to one decimal. Update the in-range pin test (`ax_messaging_timeout_is_in_practical_range`) range if needed.

**Why deferred:** no observed timeouts yet (v0.1.5 only smoke-tested without AX-side latency stress). Calibration without real data would be motion.
