# P2 Spike

Throwaway binary that verifies the two load-bearing assumptions of `rok-bot` v0.1:

| Premise | What it tests |
|---|---|
| **P2a** | Can xcap capture an off-screen RoK window? |
| **P2b** | Does an off-screen window accept a synthetic CGEvent click via enigo? |

If P2a fails, the v0.1 architecture must use `screencapturekit` instead of `xcap`. If P2b fails, the off-screen-window technique is dead and v0.1 needs the visible-window-with-anti-focus-steal fallback documented in `../../TODOS.md`.

## Prerequisites

1. **Rise of Kingdoms running.** Mac App Store version on Apple Silicon, macOS 12.3+. Launch it and let it sit at the world or city view.
2. **Three permissions** for whatever process runs this binary (Terminal if `cargo run`, the binary itself if you build a release artifact). Each grant requires a binary restart:
   - **Screen Recording** — for xcap's capture
   - **Accessibility** — for enigo / CGEvent
   - **Automation / AppleEvents** — for `osascript` controlling System Events

   First run will prompt; the binary will exit with a clear error each time. Grant the permission, re-run.

## Run

```bash
cd spikes/p2-spike
cargo run --release -- --title "Rise of Kingdoms"
```

If your installed RoK has a different window title (localization, regional version), pass a substring that matches it.

## What it does

1. Enumerates windows via xcap, finds the RoK window, prints its title/app/position/size.
2. Captures a baseline frame while the window is still visible (sanity check).
3. Repositions the window to `(-9999, -9999)` via `osascript`.
4. Verifies the window actually moved.
5. **P2a:** captures an off-screen frame.
6. **P2b:** posts a click at the off-screen window's center via enigo, waits 800ms, captures again, computes pixel diff.
7. Restores the window to its original position.
8. Prints PASS / LIKELY FAIL verdicts.

## Outputs

Three PNGs in `/tmp/`:
- `p2_spike_0_baseline.png` — visible RoK, sanity baseline.
- `p2_spike_1_offscreen_before_click.png` — off-screen, pre-click.
- `p2_spike_2_offscreen_after_click.png` — off-screen, post-click.

The verdict on P2b is *probabilistic*: animations cause ~0.5–2.0 L1 channel delta per pixel even without any click effect. A real click landing on a button or popup typically pushes diff well above that. Visual inspection of the two off-screen PNGs is the final answer.

## Throwaway

Once P2 is resolved, delete `spikes/p2-spike/` and start v0.1. This directory has zero dependencies on the rest of the project and is safe to remove.
