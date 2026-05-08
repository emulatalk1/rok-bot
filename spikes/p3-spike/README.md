# P3 spike — Rust CGEvent + CGEventPost path verification

Phase 0 spike for v0.1.3. Verifies that the Rust `core-graphics` crate's
`CGEvent::new_mouse_event` + `CGEvent::post(CGEventTapLocation::HID)`
path produces a click that RoK responds to, when the binary's
Accessibility grant is bootstrapped via hand-rolled FFI to
`AXIsProcessTrustedWithOptions(prompt=true)`.

## Why this exists (separate from p2-spike)

p2-spike (`spikes/p2-spike/run.sh`) already proved at the architecture
level that a synthetic click via Swift `CGEventCreateMouseEvent` +
`CGEventPost` reaches RoK on a virtual display: 6,159,216 differing
bytes between before/after captures.

p3-spike answers a narrower question. Before committing v0.1.3 to a
production implementation against the `core-graphics` crate, two
specific risks need empirical answers:

1. Does the Rust binding's `new_mouse_event` constructor produce an
   event with the same field layout as Swift's `CGEvent(mouseEventSource:)`,
   such that RoK responds equivalently?
2. Does hand-rolled FFI to `AXIsProcessTrustedWithOptions(prompt=true)`
   from a freshly-built standalone Rust binary correctly bootstrap the
   Accessibility grant? The prompt is asynchronous, so the expected
   first-run UX is "prompt fires, binary exits with 13, user grants in
   System Settings, user re-runs and it succeeds" — not "binary blocks
   on prompt then continues."

If both succeed, v0.1.3 implementation can proceed against `core-graphics`
with no integration surprises and with the AX preflight pattern locked
in. If either fails, the v0.1.3 design doc has revisit branches (cursor
warp, absorb v0.1.4).

## Prerequisites

- RoK is running and parked on a virtual display (see p2-spike README
  for BetterDisplay setup).
- A recent `rok-capture.png` exists in the parent project. If not,
  `cd ../..` and run `cargo run --release` once to produce one.
- Screen Recording is granted to your terminal (you'll want it for
  before/after captures during eyeballing; not strictly required by
  the spike binary itself).

## Run procedure

### 1. Pick a state-neutral target

Open `rok-capture.png` in Preview. Identify a UI element whose click
has an obvious visual response **without** changing game state
(resources spent, troops dispatched, decisions committed). Good
candidates:

- City ↔ World view toggle (bottom-right) — fully reversible.
- Chat bubble icon — opens a panel, dismissable.
- Minimap recentre — view-only.

Avoid: build buttons, march buttons, dispatch buttons, anything that
consumes resources or commits a tactical decision.

### 2. Translate capture pixel coords to global screen coords

`rok-capture.png` is the window contents at backing-store pixel
resolution (typically 2x on Retina). Global screen coords are in points.

Read the most recent run's window pos/dims from the parent `rok-bot`
log line (something like `window: id=N owner=...  pos=(X,Y) size=WxH`).
Then:

```text
target_screen_x = window_pos_x + (capture_target_x_px / capture_width_px)  * window_width
target_screen_y = window_pos_y + (capture_target_y_px / capture_height_px) * window_height
```

(Both axes scale identically on a single-display/single-DPI capture, so
this simplifies to `window_pos + 0.5 * window_dim` for window-centre
clicks. Use the long form for off-centre targets.)

### 3. Build + run

```bash
cd spikes/p3-spike
cargo build --release --locked
./target/release/p3-spike --x <SX> --y <SY>
```

**First run:** macOS will show an Accessibility prompt for the spike
binary (or fail silently if the prompt was already dismissed). The
spike will exit with code 13. Click "Open System Settings" → Privacy
& Security → Accessibility → enable `p3-spike` (toggle on). Re-run the
same command.

The async prompt + exit-13 + re-run cycle is expected behavior, not a
bug — see v0.1.3 design doc CMT-2 for the analysis.

### 4. Observe RoK

Within ~1 second of the spike printing
`click posted at (x, y) — observe RoK`, did the target you picked
respond visibly (toggle flipped, panel opened, view recentred)?

### 5. Record outcome

Append a row to the table below.

## Outcome record

| Date       | Target                | (x, y)         | RoK responded? | Notes                                                                                                                                                                                                                |
| ---------- | --------------------- | -------------- | -------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-05-08 | Window center (city)  | (-525.5, 513)  | Y              | RoK on virtual display at frame (-1051, 103) 1051×820. Two consecutive runs produced 3.05M and 5.51M byte diffs (>30× the p2-spike 100K threshold). Run 1's "after" persisted as Run 2's "before" (~80% size jump) — proves click landed and state stuck, not ambient noise. Spike exit 0 both runs. AX inherited from build chain (no first-run prompt). |

## Outcome gate (decides v0.1.3 ship strategy)

- **Y** → proceed with v0.1.3 separate from v0.1.4 (current plan, Phase
  1 implementation per design doc).
- **N** → first try a cursor-warp variant: modify `post_click` to call
  `CGWarpMouseCursorPosition(pt)` before posting events. If still N,
  absorb v0.1.4 (after-state verify) into v0.1.3 since shipping click
  separately is unprovable without verification.

## Disposable

Once v0.1.3 lands, this spike directory can be deleted with no loss.
The decisions it produced are recorded in the v0.1.3 design doc
(`~/.gstack/projects/emulatalk1-rok-bot/hbchuc-main-design-v0.1.3-*.md`).
