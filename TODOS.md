# TODOS

Items deferred from planning sessions. Each entry should be self-contained enough that picking it up months later is feasible.

---

## P1: BetterDisplay setup automation

**Source:** P2 spike outcome 2026-05-06
**Effort:** human ~1 hour / CC ~15 min
**Depends on:** v0.1 binary working

When v0.1 ships, write a one-page setup guide for the user (or a future contributor) that captures the manual prerequisites: install BetterDisplay, create a virtual display, drag RoK there once. Currently this lives only in conversation history. Should be `docs/setup.md` with screenshots — at minimum the four-step happy path (install → menu bar → create virtual screen → connect → drag RoK).

If the bot runs on a Mac without BetterDisplay's virtual display, it will detect RoK at primary-display coordinates and either (a) refuse to run with a clear error or (b) run on the visible primary, both of which are degenerate cases worth documenting.

---

## P1: Migrate from `screencapturekit` Rust crate to `objc2-screen-capture-kit`

**Source:** P2 spike build failure 2026-05-06
**Effort:** human ~half day / CC ~1 hour
**Depends on:** v0.1 implementation phase

The `screencapturekit` Rust crate (1.5.x) wraps an internal Swift package via `swift-bridge`. The build script invokes `xcrun --sdk macosx --show-sdk-platform-path`, which fails on machines with only Xcode Command Line Tools installed (full Xcode required). This makes the dep a hard install-time blocker for any contributor without ~14 GB of Xcode.

`objc2-screen-capture-kit` (0.3.x) is the canonical alternative — direct ObjC2 message-passing bindings, no Swift bridge, no Xcode requirement. v0.1 should use it from day one.

Until then, the spike at `spikes/p2-spike/run.sh` shells out to Apple's `screencapture -l <wid>` CLI for capture, which works fine for verification but is not appropriate for v0.1 (subprocess overhead per frame).

---

## P3: Multi-instance feasibility (the "farm" 10x dream)

**Source:** office-hours D7 (P1 in the original premise list)
**Effort:** human ~1 hour spike / CC ~15 min
**Depends on:** v0.1 hello-world working

The autonomous-twin → multi-account-farm path requires running multiple RoK instances on a single Mac. Mac App Store's iOS-on-Mac runtime may enforce single-instance per bundle ID, in which case the farm vision needs separate Macs / VMs / remote machines.

**Cheapest verification when ready:** launch RoK twice via `open -na /Applications/RiseOfKingdoms.app` (or whatever the actual install path is) and observe whether two distinct processes survive, or whether the second invocation just brings the first to focus.

If multi-instance works, the v0.1 architecture (single binary, single account) refactors to a coordinator + per-account workers. If multi-instance is blocked, the farm dream needs additional Macs or BetterDisplay virtual-display arrangements per Mac.
