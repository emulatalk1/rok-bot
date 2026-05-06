# TODOS

Items deferred from `/plan-ceo-review` and other planning sessions. Each entry should be self-contained enough that picking it up months later is feasible.

---

## P3: osascript-positioning backup plan

**Source:** `/plan-ceo-review` 2026-05-06 (D4)
**Effort:** human ~30 min / CC ~10 min
**Depends on:** nothing

The headless trick (off-screen RoK window via `osascript -e 'tell System Events ... set position'`) is a single load-bearing primitive. AppleEvents have been incrementally restricted across the last several macOS releases. If Apple deprecates window-position-via-System-Events in macOS 16/17 (or lifts the bar to require something heavier than the Automation prompt), the entire headlessness story breaks at once.

**Backup plan (one paragraph to write down before it bites):** run RoK as a *visible* window with focus-stealing prevention. Concretely:
- Position the window where the user has agreed it can sit (e.g., a hidden Space, off-monitor, or a corner the user accepts).
- Prevent it from stealing focus using NSWindow tricks: `canBecomeKey = false` for inactive states, `NSWindowCollectionBehavior.transient` style, or running RoK under a wrapper that intercepts activation events.
- Trade-off: visible window means the user sees the bot working; the engineering-flex demo angle gets *better* (you can show what the bot is doing in real time) but the "second player runs my account while I work" UX gets worse.

Write this paragraph into `docs/rok_rust_bot_research.md` § 3 (Headless Mode Analysis) as a "Plan B" subsection so it survives session loss.
