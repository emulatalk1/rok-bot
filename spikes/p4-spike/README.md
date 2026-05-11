# P4 spike — `CGEventPostToPid` for Catalyst Bridge apps

Phase 0 spike for v0.1.5. Verifies whether `CGEvent::post_to_pid(pid)`
delivers clicks to RoK (an iOS-on-Mac Catalyst Bridge app) **regardless
of window z-order at the click point**.

## Why this exists

v0.1.4 ships a click pipeline that posts events at the HID event-tap
level via `CGEvent::post(CGEventTapLocation::HID)`. That's screen-pixel
dispatch: the OS routes the click to whatever window is z-order topmost
at the screen pixel. The bot's TOCTOU validator (`validate_at_click_site`)
exists because of this — it refuses to post a click if RoK isn't
topmost, on the grounds that the click would otherwise land on whatever
window is occluding RoK at that point.

This makes the bot fragile to window stacking. Live-smoke 2026-05-11
exposed it: with RoK left-of-center on the screen and iTerm at default
position covering the click point (the city-button medallion in the
bottom-left of the RoK window), every click was refused with exit 19
`not_topmost_at_click`. Moving iTerm out of the way unblocks it, but
that's not a sustainable production model — Mode 2's whole premise is
that the bot runs while the user does something else.

`CGEventPostToPid(pid, event)` is the documented Apple API for posting
events **directly to a process's event queue**, bypassing screen-level
dispatch. If it works for RoK, the click lands on RoK regardless of
what window is in front, the TOCTOU topmost-check becomes unnecessary
(the WID + frame checks remain useful), and Mode 2 becomes actually
viable.

**Open question**: RoK is an iOS-on-Mac app. The Catalyst Bridge
translates AppKit events into UIKit events. Whether that translation
respects PID-posted events the same way it respects HID-tap events is
not documented anywhere we found. p4-spike's job is to give that
question a yes/no answer with empirical evidence, before v0.1.5
commits to the migration.

## Prerequisites

- macOS Sequoia 15+ on Apple Silicon (same as the parent project).
- RoK is running and parked on the built-in display (Mode 1).
- A recent `rok-capture-pre.png` exists in the parent project root.
  If not, `cd ../..` and run `cargo run --release` once to produce
  one (will exit 15 against the sentinel placeholder — that's fine,
  the pre-capture still lands on disk).
- Screen Recording AND Accessibility granted to the terminal you run
  the spike from. Both are already required for the main bot; the
  spike binary will trigger the same grant prompts on first run if
  not inherited.

## Run procedure

### 1. Identify the click point

The same city-button medallion the main bot targets. From the parent
project, run the bot once (`cargo run --release`); the log line
`translated match to screen point` reports the screen coordinates. As
of 2026-05-11 with RoK at frame `(391, 61, 1051, 820)`, that's
`(461.0, 833.5)`. If your RoK window has moved, re-derive.

### 2. Find RoK's pid

```sh
pgrep -fl RiseofKingdoms | head -1
```

(Note the lowercase 'of' in the process name — it differs from the
window owner-name `RiseOfKingdoms`.)

### 3. Arrange windows so iTerm (or another window) covers the click point

This is the failure condition for the v0.1.3 HID-tap path. Drag iTerm
so its frame contains the screen pixel from step 1. Confirm visually
that the city-button is occluded.

### 4. Build the spike

```sh
cargo build --release
```

### 5. Run the experiment (post_to_pid)

```sh
./target/release/p4-spike <pid> <x> <y>
```

Watch RoK. The expected outcome is the city/world toggle visibly flips
(world view → city view, or city → world, depending on RoK's current
state). The bot didn't move iTerm; if RoK responded anyway, that's a
**PASS** — PID-posted events reach Catalyst Bridge apps.

### 6. Run the control (HID tap)

```sh
./target/release/p4-spike <pid> <x> <y> --hid
```

Watch what gets clicked. Expected: iTerm (or whatever's on top at that
point) absorbs the click. RoK does not respond. This confirms the
spike's control matches v0.1.3's current behavior.

### 7. Re-toggle for cleanup (optional)

If step 5 worked and you ended up in city view, re-run step 5 to
toggle back, or click the medallion manually.

## Expected outcomes

| Outcome | What it means | v0.1.5 implication |
|---|---|---|
| Step 5 PASS + step 6 absorbs to iTerm | `post_to_pid` reaches RoK; HID is screen-dispatched as expected. | Migrate `click_at` to `post_to_pid`. Drop the `topmost-at-click` half of `validate_at_click_site`. Keep WID + frame validation. |
| Step 5 NO RESPONSE + step 6 absorbs to iTerm | PID-posted events don't reach Catalyst-bridged apps. The Apple bridge layer drops them. | v0.1.3 HID path stays. Mode 2's "user does other things" goal needs a different mechanism (window activation before each click, or a virtual display that nothing else can occlude). |
| Step 5 PASS + step 6 also PASS | iTerm wasn't actually covering the click point (run-procedure step 3 mis-arranged), OR HID-tap is doing something we don't expect. | Re-run with deliberately verified occlusion (use `screencapture -R` to confirm what's at the click point). |
| Step 5 fires Accessibility prompt | Spike binary hasn't been granted Accessibility yet. | Grant it in System Settings > Privacy & Security > Accessibility (the binary will show up under the path Cargo writes to, e.g., `target/release/p4-spike`). Re-run. |

## Decision branches

- **`post_to_pid` works (most likely)**: v0.1.5 migrates `click_at` to
  use it. The `topmost-at-click` check in `window::validate_at_click_site`
  is removed (or kept as a soft warning). Documentation in `docs/setup.md`
  and `CLAUDE.md` updates to reflect the screen-position-independent
  behavior. Mode 2's design is materially simpler.

- **`post_to_pid` doesn't work**: keep the v0.1.3 HID path. Add a new
  TODO: "before each click, activate RoK so it's z-order topmost at
  the click point" — likely via `NSRunningApplication::activate` or
  `osascript -e 'tell application "RiseofKingdoms" to activate'`. Mode
  2 needs a virtual display anyway, so screen-stacking concerns shift
  to display-level isolation.

## Why a separate spike from p3-spike

p3-spike answered "does our Rust `core-graphics` binding's
`CGEvent::post(HID)` produce events RoK responds to?" — yes. That was
about the BINDING, with HID-tap-level routing assumed correct.

p4-spike asks a different question: "does the **non-HID** code path
(`post_to_pid`) route correctly through the Catalyst Bridge?" — this
is about Apple's process-targeted event dispatch through their iOS-on-Mac
translation layer, which is undocumented behavior at our level.

## Result (2026-05-11)

**Verdict: `CGEventPostToPid` does NOT work for RoK (Catalyst Bridge).**

Test matrix run against RoK at frame `(391, 61, 1051, 820)`, click point
`(461, 833.5)` (center of the city-button medallion), iTerm parked at
`(36, 34, 1010, 858)` covering the click point.

| Case | click-point Δ | RoK-window Δ |
|---|---|---|
| noop (no click, ambient baseline) | 0.00% | 0.97% |
| `post_to_pid` (experiment) | 0.00% | 1.12% |
| `--hid` (control) | **2.59%** | 1.11% |

Interpretation:

- The HID control hit iTerm (click-point Δ 2.59% — terminal cursor / selection
  moved). That's screen-pixel dispatch landing on the topmost window. Exactly
  the v0.1.3 path that motivated this spike.
- `post_to_pid` produced zero observable change at the click point AND zero
  observable change inside RoK above the ambient noise floor (0.97% from
  background animation; experiment 1.12% is within 0.15pp of that). The event
  was successfully constructed and posted (Accessibility granted, `rc=0`),
  but RoK's Catalyst Bridge AppKit→UIKit translation didn't process it as
  a meaningful click. The event was silently dropped somewhere in the bridge.

This matches what Apple's Catalyst documentation hints at but doesn't state
outright: Catalyst-bridged apps receive their input through a translation
layer that subscribes to standard AppKit input dispatch (HID tap / window
event routing), not the PID-targeted Quartz event stream.

## Implication for v0.1.5

The `CGEventPostToPid` migration is **DEAD**. Do not proceed.

The screen-pixel-dependency observed in live-smoke 2026-05-11 is therefore
NOT fixable at the click-delivery layer for iOS-on-Mac apps. The fix has to
come from somewhere else:

1. **Activate RoK before every click** (Mode 1 production model). Add a
   `NSRunningApplication::activate` (or equivalent) call to `click_at`
   immediately before posting. Trade-off: RoK keeps grabbing foreground;
   user can't do meaningful other work in parallel. Acceptable for Mode 1.
2. **Virtual-display isolation** (Mode 2's original design — reaffirmed).
   RoK on a BetterDisplay virtual display where no other windows ever live.
   Screen-pixel dispatch and z-order topmost both become trivially correct
   because nothing can occlude RoK on that display. This is the path Mode 2
   was always going to take and the spike result confirms it's the right
   one.

Mode 1 production users should ensure RoK is foreground before invoking
the bot (the existing exit 19 documentation already implies this). The
post-to-pid hope of "bot runs while user does other things in Mode 1" is
dead; that goal belongs entirely to Mode 2.

## Decommissioning

When v0.1.5 ships (or definitively decides not to migrate),
`docs/setup.md` and `TODOS.md` get a sentence pointing to this spike's
result. The spike directory itself can either stay (as historical
evidence for the design decision) or be deleted with a CHANGELOG-equivalent
note in `TODOS.md`. p2-spike and p3-spike both stayed; precedent is to
leave it.
