# P2 Spike — VERIFIED 2026-05-06

The bash spike in this directory verified the two load-bearing assumptions of the rok-bot v0.1 architecture against an iOS-on-Mac (Catalyst-class) target.

## Result

| Test | Outcome |
|---|---|
| **P2a:** capture an iOS-on-Mac window across Spaces / on a non-primary display | ✅ PASS — `screencapture -l <wid>` produced a 2238×1776 PNG of RoK on a BetterDisplay virtual screen at `(-1536, 59)` |
| **P2b:** synthetic CGEvent click reaches RoK at virtual-display coordinates | ✅ PASS — 6,159,216 differing bytes between before/after captures |

Side findings recorded in `docs/rok_rust_bot_research.md` § 3:

| Mechanism we tried for "make RoK invisible" | Outcome |
|---|---|
| `osascript` / System Events `set position of window 1` | DEAD — RoK has 0 windows in the AX tree |
| Private CGS `CGSMoveWindow` (default connection) | Silent no-op |
| Private CGS `CGSMoveWindow` (owner connection) | Explicit `kCGErrorCannotComplete` denial |
| `CGEventPostToPid` (process-targeted input) | DEAD — 0 differing bytes |

**Conclusion:** No external process can hide, move, or process-target an iOS-on-Mac window on Apple Silicon. The architectural pivot: park RoK on a virtual display (BetterDisplay free version) and operate on it via standard `CGWindowListCopyWindowInfo` + `screencapture` + `CGEvent.post`. Verified.

## What's in this directory

- **`run.sh`** — the canonical, working spike. Bash + embedded Swift + `screencapture` + `cmp` for diff. Run from any terminal that has Screen Recording + Accessibility granted to it. RoK must be running.
- **`README.md`** — this file.

The original Rust spike was deleted because the `screencapturekit` Rust crate (1.5.x) has an internal Swift bridge that requires the full Xcode SDK to build. v0.1 will use `objc2-screen-capture-kit` instead, which does not have this constraint. See `docs/cargo_dependency_audit.md` for the dependency note.

## To re-run

```bash
./run.sh RiseOfKingdoms
```

Prerequisites:
- RoK running, parked on a virtual display (BetterDisplay or equivalent).
- Three macOS permissions granted to the terminal app: Screen Recording, Accessibility. (Automation/AppleEvents is *not* needed by `run.sh` directly, though some early iterations of the script invoked osascript reposition; the current version skips that step entirely.)

Output is three PNGs in `/tmp/p2_spike_*.png` plus a verdict block on stdout.

## Disposable

This spike is a verification artifact. Once v0.1 is implementing the same architecture in Rust against `objc2-screen-capture-kit`, this directory can be deleted with no loss.
