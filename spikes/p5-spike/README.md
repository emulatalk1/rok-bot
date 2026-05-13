# P5 spike — macOS Accessibility `AXPress` for Catalyst Bridge apps

Phase 0 spike for v0.1.5. Verifies whether `AXUIElementPerformAction`
with `kAXPressAction` delivers clicks to RoK (an iOS-on-Mac Catalyst
Bridge app) **regardless of window z-order at the click point**.

## ⚠️ FALSE POSITIVE — Verdict superseded by /qa 2026-05-11

The original verdict below ("PASS for z-order overlap") was correct at
the **delivery layer** but wrong at the **targeting layer**. The 2026-05-11
`/qa` end-to-end test against running RoK revealed:

- AX press FFI calls succeed (`AXError 0`).
- RoK observably responds (info panel opens; pixel_diff 500k+).
- But the response is at `AXActivationPoint = (916.91, 487.24)` (window
  center), NOT at the (x, y) passed to `AXUIElementCopyElementAtPosition`.

Catalyst Bridge exposes RoK's entire game canvas as a single
positionless `AXGenericElement`. The (x, y) arguments to
`CopyElementAtPosition` are used to walk the AX tree — but RoK's tree
has no per-control children to walk, so it always resolves to the same
root element with a fixed activation point. `AXPress` fires at that
fixed center, not at the caller's coords. `AXActivationPoint` is
read-only (verified via the `--ax-set-point` mode added to this spike's
`src/main.rs`).

Subsequent research (same day) tested 5 other click delivery paths
(HID+osascript, stealth HID, cua-driver default + count:3, bare
SLEventPostToPid). All paths that fire the castle also auto-raise RoK
— Catalyst Bridge auto-raises on any synthetic UITouch that wakes its
event pipeline. **No Mode 1 click-delivery mechanism satisfies "works
when covered + doesn't move cursor + doesn't raise RoK + fires castle"
simultaneously.** See `TODOS.md` P0 entry for the full 6-path matrix.

**Implication for this spike's verdict:** "AX press works on Catalyst
Bridge" is true at the delivery layer (the FFI returns success and RoK
reacts) and false at the targeting layer (the reaction happens at the
wrong coord). The v0.1.5 production code at `src/ax.rs::press_at`
inherits the targeting bug.

**Scope lesson for future spikes:** "delivery works" is necessary but
not sufficient. A spike's PASS verdict must include "the intended UI
element fired," not just "RoK reacted." p3-spike (HID), p4-spike
(`CGEventPostToPid`), and this spike all conflated the two. For v0.2
work, every click-path spike must end with a visually-verified
INTENDED-target press, not just "something happened in RoK."

**Path forward:** v0.2 Mode 2 (BetterDisplay virtual display). On a
virtual display, the AX targeting bug becomes invisible because nothing
observes RoK's surface — the off-target popup that opens at the canvas
center is acceptable as long as it produces a state change the matcher
can re-orient from. See `TODOS.md` P2 v0.2 entries.

The `--inspect`, `--app-tree`, `--stealth`, `--ax-set-point`, and
`--skylight` modes added to `src/main.rs` on 2026-05-11 are the
diagnostic artifacts that proved the canvas-positionless behavior. They
should be preserved as project record for future Catalyst-Bridge
investigations.

---

## TL;DR (original — see addendum above)

**PASS for z-order overlap on the same Space.** AX press at a screen
point covered by iTerm (overlapping RoK on the same Space) still reached
RoK and produced a visible response.

**FAIL across Spaces.** When iTerm is full-screen (RoK's window pushed
to a hidden Space), AX press returns `AXError -25200 (kAXErrorFailure)`
even though the element query at the same coordinates still succeeds.

v0.1.5 can migrate `click_at` to AX press and drop the topmost-at-click
half of `validate_at_click_site`, but **operational constraint**: the
bot requires RoK's window to be on the currently-displayed Space.
Documentation must call this out. Mode 1 users cannot put another app
into native macOS full-screen mode while the bot is running on the
built-in display. Mode 2's virtual-display design sidesteps this by
keeping RoK's Space always "displayed" on the virtual screen.

## Why this exists

p4-spike confirmed `CGEventPostToPid` is **dead** for Catalyst Bridge
apps — the AppKit→UIKit translation layer silently drops PID-targeted
events. That ruled out the "use the non-HID Quartz API" path for the
"bot runs while user does other things" goal.

The macOS Accessibility API (`AXUIElement`) is the next candidate.
Apple's [Accessibility design for Mac Catalyst][1] documents that
UIKit accessibility auto-bridges to macOS Accessibility for Catalyst
apps, and Hammerspoon (`hs.axuielement`), AXorcist, and DFAXUIElement
all use this path in production for screen-position-independent
automation.

The open question for v0.1.5: does AX press actually route to RoK's
UIKit elements through the bridge, and does it do so regardless of
which window is z-order topmost at the screen pixel?

[1]: https://developer.apple.com/documentation/accessibility/accessibility_design_for_mac_catalyst

## Prerequisites

- macOS Sequoia 15+ on Apple Silicon.
- RoK running and parked on the built-in display (Mode 1).
- Accessibility granted to the terminal you run the spike from. The
  spike binary inherits TCC trust from a granted parent in most cases
  (observed 2026-05-11 first-run); if not, the spike will fire the
  Accessibility prompt and exit 13.

## API surface

Three calls, all in `ApplicationServices.framework`:

```c
AXUIElementRef AXUIElementCreateApplication(pid_t pid);
AXError        AXUIElementCopyElementAtPosition(
                   AXUIElementRef application,
                   Float32 x, Float32 y,
                   AXUIElementRef *element);
AXError        AXUIElementPerformAction(
                   AXUIElementRef element,
                   CFStringRef    action);  // pass kAXPressAction = "AXPress"
```

Coordinates are CG-style top-left-origin screen pixels — same space
the main bot's `screen_point` translation produces, same space
p4-spike used for `post_to_pid`.

`AXUIElementCopyElementAtPosition` scoped to an application element
(rather than system-wide) returns the deepest descendant in *that
application's* AX hierarchy at the given screen coordinates — which
is the behavior we need to bypass z-order.

## Run procedure

### 1. Identify the click point

Same as p4-spike: from the parent project, `cargo run --release`
logs `translated match to screen point` — those are the coordinates.
As of 2026-05-11, RoK at frame `(391, 61, 1051, 820)` gives screen
point `(461.0, 833.5)`. Note: per the v0.1.4 ship checkpoint, the
needle crop at this point was flagged "wrong" by the operator; for
the spike's purpose (does AX reach RoK at all?), the exact button
doesn't matter — any UI element RoK exposes via AX will do.

### 2. Find RoK's pid

```sh
pgrep -fl RiseofKingdoms | head -1
```

### 3. Arrange iTerm to cover the click point

This is the failure condition for the v0.1.4 HID-tap path. Park
iTerm at `(36, 34, 1010, 858)` (same as p4-spike) so its frame
contains the screen pixel from step 1.

### 4. Build the spike

```sh
cargo build --release
```

### 5. Run the baseline (--noop, AX preflight only)

```sh
./target/release/p5-spike <pid> <x> <y> --noop
```

Fires the AX TCC prompt on a first-run binary that doesn't inherit
trust from its parent. Grant the binary in System Settings > Privacy
& Security > Accessibility and re-run. Expected exit 0 once granted.

### 6. Run the control (--hid)

```sh
./target/release/p5-spike <pid> <x> <y> --hid
```

Watch what gets clicked. Expected: iTerm absorbs the click (cursor
moves, selection changes, or window content shifts). RoK does not
respond. This confirms the spike's control matches v0.1.4's current
behavior.

### 7. Run the experiment (AX press)

```sh
./target/release/p5-spike <pid> <x> <y>
```

Watch RoK. Expected outcome: an AX-mediated press at the screen point
inside RoK's element hierarchy. If a button is at that point, the
button presses. If a map element is at that point, RoK opens that
element's info panel. The key signal is **something inside the RoK
window visibly changes** while iTerm at the screen point does NOT
respond as it did in step 6.

## Expected outcomes

| Outcome | What it means | v0.1.5 implication |
|---|---|---|
| Step 7 produces a visible RoK response + iTerm unchanged | AX press routes through Catalyst bridge to the RoK element hierarchy, bypassing screen z-order. | Migrate `click_at` to AX press. Drop or soft-warn the `topmost-at-click` branch of `validate_at_click_site`. Mode 2's "user does other things" goal becomes achievable on Mode 1. |
| Step 7 produces no RoK response + step 6 absorbs to iTerm | AX press did not reach RoK — either the Catalyst bridge doesn't expose UI through AX, or `AXUIElementCopyElementAtPosition` returns iTerm's element (system-wide-like behavior) and presses there. Spike output's AXError code will tell which. | v0.1.4 HID path stays. Fall back to `NSRunningApplication::activate` + HID for Mode 1 production. Mode 2 still relies on virtual-display isolation. |
| Step 7 AXError -25208 ActionUnsupported | The element at that point exists in RoK's AX hierarchy but doesn't expose AXPress. UIButton typically bridges, map elements may not. | Re-derive coords to land on an interactive control (button, label with action). If even buttons don't expose AXPress, AX is dead and fallback path applies. |
| Step 7 AXError -25214 NoValue | No AX element at those coords within the RoK application. Likely the AX scope is stricter than expected. | Verify the coord is inside the RoK frame. Try `AXUIElementCreateSystemWide` instead; document the difference. |

## Decision branches

- **AX press works (the PASS case below)**: v0.1.5 migrates `click_at`
  to AX press. New `src/ax.rs` with the three-call FFI. The
  `topmost-at-click` check in `window::validate_at_click_site` is
  removed or downgraded to a soft warning (z-order no longer matters
  for click delivery, but the WID + frame checks remain useful).
  Documentation in `docs/setup.md` and `CLAUDE.md` updates to reflect
  screen-position-independent behavior. Mode 2's design simplifies.

- **AX press doesn't work**: keep the v0.1.4 HID path. Add
  `NSRunningApplication::activate` (or `osascript ... tell ... to
  activate`) before each click as a documented Mode 1 fallback. Mode
  2's virtual-display isolation is unchanged.

## Why a separate spike from p4-spike

p4-spike answered: "does `CGEventPostToPid` (non-HID Quartz event
dispatch) route through Catalyst Bridge?" → NO.

p5-spike answers a different question: "does the **macOS
Accessibility API** route through the UIKit→AppKit accessibility
bridge?" These are entirely different code paths inside macOS. p4-spike
ruling one out doesn't predict the other; only empirical evidence does.

## Result (2026-05-11)

**Verdict: `AXUIElementPerformAction(kAXPressAction)` works for RoK
(Catalyst Bridge) only when RoK's window is on the currently-displayed
Space. Z-order occlusion on the same Space is defeated; Space isolation
is not.**

Test matrix run against RoK at frame `(391, 61, 1051, 820)`, click
point `(461.0, 833.5)` (bottom-left of the RoK window — a map area
in world view), iTerm parked at `(36, 34, 1010, 858)` covering the
click point.

| Case | Click-point box (iTerm region) Δ | RoK visible-strip Δ | Verdict |
|---|---|---|---|
| `--noop` (baseline) | n/a | n/a | AX TCC granted, exit 0. No click posted. |
| `--hid` (control, v0.1.4 path) | **CHANGED** (iTerm content shifted: "thinking ⏐ session limit" → "thinking ⏐") | (not measured) | HID click absorbed by iTerm at the screen point. Matches v0.1.4 expected behavior. |
| AX press (experiment) | (iTerm content also shifted slightly — incidental, iTerm is an active terminal) | **CHANGED** (RoK opened a Vietnamese player info panel showing "Thống đốc 226326161 / X:851 Y:270 / Sức mạnh 7.886 / NHẬP" — element wasn't in the pre-capture) | AX press routed through Catalyst bridge to a RoK map element, bypassing iTerm's z-order. |

Interpretation:

- All three AX FFI calls succeeded (`AXUIElementCreateApplication` →
  non-null; `AXUIElementCopyElementAtPosition` → `AXError 0` + non-null
  element; `AXUIElementPerformAction(AXPress)` → `AXError 0`).
- The visible response inside the RoK window (an info panel that wasn't
  there before the press) is direct evidence the bridge translated
  AXPress into a UIKit tap on a UIKit element. The bridge layer Apple
  documents for Catalyst (UIKit accessibility auto-bridges to macOS
  Accessibility) handles the round-trip.
- iTerm at the click point did NOT receive the press as it did under
  `--hid`. The minor iTerm content shift between AX pre/post captures
  is consistent with iTerm's normal active-terminal updates over a
  multi-second window, not a click-handled response.

The visible response wasn't a city/world toggle because (461.0, 833.5)
sits on a map element (other player's troops near map coord 851,270),
not on the city-button medallion. The needle crop being "wrong" was
flagged in the v0.1.4 ship checkpoint; for the spike's purpose
(empirical proof AX press reaches RoK at all), this is irrelevant. Any
RoK-responsive element at the click point would have closed the
question.

## Result extension — full-screen iTerm test (Spaces isolation)

After the overlap test passed, ran a second matrix variant: iTerm
switched to native macOS full-screen mode. macOS moves a full-screen
app to its own Space; RoK's Space becomes hidden. CGWindowList
confirmed: with `kCGWindowListOptionOnScreenOnly` RoK is invisible
(zero windows returned); with `kCGWindowListOptionAll` RoK's WID 76346
still appears at bounds `(391, 61, 1051, 820)` — same screen
coordinates, just on a hidden Space.

Capture method: `screencapture -l 76346` captures by window ID and
works across Spaces (the WindowServer keeps a cached snapshot of
inactive-Space windows). Pre and post captures both 2238×1776 Retina
PNGs.

Result:

| Call | Outcome | Inference |
|---|---|---|
| `AXUIElementCreateApplication(30612)` | non-null | Process-element creation is process-scoped, ignores Space topology. |
| `AXUIElementCopyElementAtPosition(app, 461.0, 833.5, &elem)` | `AXError 0` + non-null elem | AX hierarchy is process-resident, queryable across Spaces. |
| `AXUIElementPerformAction(elem, kAXPressAction)` | **`AXError -25200 (kAXErrorFailure)`** | Action delivery requires Space-visibility. The bridge refuses. |
| RoK pre vs post pixel diff (via `screencapture -l`) | **identical** | No state change — consistent with the explicit AXError. |

## Disambiguation — `--inspect` mode

The new `--inspect` subcommand reads `AXRole`, `AXSubrole`,
`AXRoleDescription`, `AXTitle`, `AXPosition`, `AXSize`, `AXChildren`,
and the full `AXActionNames` list of the element resolved at (x, y).
It does NOT post any action — purely read-only.

Run against the same `(30612, 461.0, 833.5)` while iTerm full-screen
keeps RoK on a hidden Space:

```
[AXRole]            = AXMenuBar
[AXRoleDescription] = menu bar
[AXPosition]        = x:0.000000 y:0.000000 (kAXValueCGPointType)
[AXSize]            = w:1512.000000 h:33.000000 (kAXValueCGSizeType)
[AXEnabled]         = true
[AXFocused]         = false
[AXChildren]        = 6 elements (the menu items: Apple menu, app name, etc.)
[AXActionNames]     = ["AXCancel"]   (AXPress NOT in list)
```

**The element resolved at (461, 833.5) is the menu bar.** It sits at
(0, 0, 1512, 33) — nowhere near the requested coordinates. AX
`CopyElementAtPosition` degrades gracefully when the target window
isn't on the active Space: instead of returning the element at the
asked coords (which doesn't exist in the active rendering), it returns
some other element from the app's AX hierarchy — empirically, the menu
bar. The menu bar exposes only `AXCancel`, not `AXPress`, so the
earlier `AXError -25200 (Failure)` is the correct refusal: "can't
press a menu bar."

This resolves the three hypotheses:

- ~~Hypothesis 1 (macOS blocks AX actions across Spaces).~~ False:
  the action layer didn't refuse for a Space reason, it refused because
  AXPress isn't supported on the (wrong) element returned.
- ~~Hypothesis 2 (AppKit→UIKit bridge requires render cycles).~~ Cannot
  be falsified directly by this run, but is now irrelevant — the press
  never reached the bridge because the element wasn't a UIKit element
  in the first place.
- **Hypothesis 3 (stale / wrong element)** — confirmed. The element
  returned by `CopyElementAtPosition` was NOT the element at the screen
  coordinates. It was the menu bar, returned as a fallback when the
  hierarchy can't be mapped by position because the window isn't drawn.

## Safety note

This degraded behavior is a sharper safety concern than just
"clicks don't reach RoK". If a future bot bug accidentally targets an
action that the menu bar DOES support (`AXCancel`, or a menu item's
`AXPress`), the AX layer would happily perform it. A bot intended to
press a button in RoK could end up canceling a system dialog or
opening a menu instead.

For v0.1.5, this elevates the Space-visibility preflight from "nice to
have" to "required safety guard". The bot must refuse to issue any AX
action when RoK's window is not in `kCGWindowListOptionOnScreenOnly`.

## Implication for v0.1.5

`AXUIElementPerformAction(kAXPressAction)` is the production click
path for v0.1.5, with an explicit operational constraint:
**RoK's window must be on the currently-displayed Space.** Migration
scope:

1. **New `src/ax.rs`** with FFI to `AXUIElementCreateApplication`,
   `AXUIElementCopyElementAtPosition`, `AXUIElementPerformAction` and
   the `kAXPressAction` constant. FFI boilerplate copies from this
   spike's `src/main.rs`. CFRelease both refs.
2. **Modify `src/click.rs`** to call `ax::press_at(pid, x, y)`
   instead of `CGEvent::post(HID)`. Keep `CLICK_GAP_MS` (or
   collapse to zero — AX press is single-event, not a down/up pair,
   so the gap is moot). Map `AXError -25200 (kAXErrorFailure)` to a
   new error variant (`BotError::ClickFailed { reason: "ax_press_refused" }`)
   so the operator sees a specific signal when RoK is on a hidden Space.
3. **REQUIRED Space-visibility preflight in
   `window::validate_at_click_site`**: check that RoK's window appears
   in `kCGWindowListOptionOnScreenOnly`, not just
   `kCGWindowListOptionAll`. If it's only in the All list, RoK is on a
   hidden Space → fail-fast with a typed error so the operator gets a
   clear "bring RoK's Space to front" message. This is a **safety
   gate**, not just an ergonomic one: without it, `CopyElementAtPosition`
   returns a stand-in element (empirically the menu bar) and any AX
   action would silently target the wrong UI surface. Possible
   confused-deputy risk if AXCancel/AXMenuItem actions were ever
   wired in.
4. **Downgrade the topmost-at-click branch of
   `window::validate_at_click_site`**: drop entirely or keep as soft
   `warn!`. Z-order occlusion no longer blocks click delivery. WID +
   frame TOCTOU checks remain.
5. **`src/verify.rs` unchanged.** Pixel-diff after-state verification
   remains useful — AX press routing successfully doesn't mean the
   element handled the press meaningfully (e.g., pressing a label
   that exposes AXPress but has no handler).
6. **Documentation updates**: `CLAUDE.md`, `README.md`, `docs/setup.md`
   all reference the v0.1.4 topmost-at-click invariant. Replace with
   the Space-visibility invariant. Call out explicitly: native
   full-screen apps and Spaces-aware tools (Mission Control, additional
   Desktops) will break the bot in Mode 1.

The Mode 1 "RoK can be background while another window overlaps it,
bot still works" goal is achieved. The Mode 1 "user does anything else
while bot runs" goal is **partially** achieved — anything that stays on
RoK's Space is fine; anything that moves to a different Space (native
full-screen, new Desktop) breaks click delivery until the user switches
back. Mode 2's virtual-display design becomes the path for "completely
parallel" use; it sidesteps Spaces entirely by giving RoK its own
display whose Space is always "displayed" by definition.

## Open questions for v0.1.5 implementation

1. **Which AX element does `CopyElementAtPosition` return for the
   city-button medallion specifically?** Once the city-button needle
   is re-cropped correctly, run the spike against the corrected coords
   and observe. If the element is a UIKit-styled button that exposes
   `AXPress`, great. If it's a generic container that doesn't,
   `ActionUnsupported` will fire and we'll need to enumerate children
   to find the press-able descendant.
2. **Does AX press work when RoK is on a BetterDisplay virtual
   display (Mode 2)?** The bridge probably doesn't care about display
   topology — the AX hierarchy is process-local — but worth verifying
   before Mode 2 lands.
3. **How does AX press behave during RoK's loading screens?** v0.1.4's
   pixel-diff verify already handles "click landed but RoK was in a
   transition state"; AX press should behave the same.
4. **AX TCC behavior across binary updates.** TCC grants are
   per-binary-by-code-signature. `cargo build` doesn't reproducibly
   sign, so a fresh build might re-prompt. The spike binary inherited
   trust from its parent terminal in the 2026-05-11 run, but the
   production v0.1.5 binary may behave differently. Test on a clean
   TCC state before shipping.

## Decommissioning

When v0.1.5 ships, `docs/setup.md` and `TODOS.md` get a sentence
pointing to this spike's result. The spike directory itself stays as
historical evidence (precedent: p2, p3, p4 all stayed).
