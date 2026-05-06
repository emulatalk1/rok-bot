# P7 Spike — RoK Auto-Migration on BD Reconnect

Verifies Premise 7 of the two-mode runtime design: does RoK auto-return to its
BetterDisplay virtual display when that display is disconnected and reconnected?

## Result (2026-05-06)

✅ **Yes, RoK auto-returns.** Same window ID, same PID, same coords as baseline.

Bonus finding: `CGDirectDisplayID` is NOT stable across BD disconnect/reconnect
cycles — the same virtual display got displayID=11 before, 12 after. Any tool
that caches the ID will break on a BD toggle.

## How to re-run

```bash
# 1. Park RoK on BD virtual display, take baseline
swift spikes/p7-spike/spike_snapshot.swift RiseOfKingdoms

# 2. Manually disconnect the virtual display via BetterDisplay menu bar.
swift spikes/p7-spike/spike_snapshot.swift RiseOfKingdoms

# 3. Manually reconnect.
swift spikes/p7-spike/spike_snapshot.swift RiseOfKingdoms
```

Compare the three snapshots — main window (title="RiseOfKingdoms") should:
- Baseline: be on a non-primary display
- Post-disconnect: be on primary, same size, new origin
- Post-reconnect: be back on virtual, same coords as baseline, same window ID
