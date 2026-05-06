#!/bin/bash
# P2 spike — VERIFIED 2026-05-06.
#
# Re-runnable verification of the rok-bot v0.1 architecture against an
# iOS-on-Mac (Catalyst-class) target. Uses standard, public macOS APIs:
#
#   Capture: CGWindowListCopyWindowInfo + screencapture -l <wid>
#   Click:   CGEvent.post(tap: .cghidEventTap)
#
# Prerequisite: RoK is running AND parked on a virtual display (created via
# BetterDisplay or equivalent). If RoK is on the primary display, the script
# warns and continues — the test will still produce data but the architecture
# claim ("invisible to the user") doesn't hold.
#
# Permissions required (granted to the terminal that runs this):
#   - Screen Recording
#   - Accessibility
#
# Run:
#   ./run.sh [needle]
#
# `needle` defaults to "RiseOfKingdoms" — substring matched against window
# title or owning app name.

set -uo pipefail

NEEDLE="${1:-RiseOfKingdoms}"
POST_CLICK_MS=1200

log()  { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }
fail() { log "ERROR: $*"; exit 1; }

# ------------------------------------------------------------- enumerate
log "STEP 0/4 — find RoK window via CGWindowListCopyWindowInfo (sees all displays + Spaces)"
WIN_INFO=$(swift /dev/stdin "$NEEDLE" <<'SWIFT_EOF'
import Foundation
import CoreGraphics
import AppKit

guard let infoList = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as NSArray? else {
    FileHandle.standardError.write("CGWindowListCopyWindowInfo returned nil\n".data(using: .utf8)!)
    exit(1)
}
let needle = CommandLine.arguments.dropFirst().first ?? ""
let lowerNeedle = needle.lowercased()

var best: (id: Int, x: Int, y: Int, w: Int, h: Int, owner: String, area: Int)?
for case let info as NSDictionary in infoList {
    let owner = info[kCGWindowOwnerName as String] as? String ?? ""
    let title = info[kCGWindowName as String] as? String ?? ""
    let layer = info[kCGWindowLayer as String] as? Int ?? -999
    guard layer == 0,
          owner.lowercased().contains(lowerNeedle) || title.lowercased().contains(lowerNeedle)
    else { continue }
    let id = info[kCGWindowNumber as String] as? Int ?? -1
    guard id >= 0 else { continue }
    let bounds = info[kCGWindowBounds as String] as? [String: Any]
    let x = Int((bounds?["X"] as? Double) ?? 0)
    let y = Int((bounds?["Y"] as? Double) ?? 0)
    let w = Int((bounds?["Width"] as? Double) ?? 0)
    let h = Int((bounds?["Height"] as? Double) ?? 0)
    let area = w * h
    if best == nil || area > best!.area {
        best = (id, x, y, w, h, owner, area)
    }
}

guard let b = best else {
    FileHandle.standardError.write("no window matching \"\(needle)\" with layer 0\n".data(using: .utf8)!)
    exit(2)
}

let cx = Double(b.x + b.w / 2)
let cy = Double(b.y + b.h / 2)
let centerPt = NSPoint(x: cx, y: cy)
var displayName = "<no display>"
var isPrimary = false
for s in NSScreen.screens {
    if NSPointInRect(centerPt, s.frame) {
        displayName = s.localizedName
        isPrimary = (s == NSScreen.main)
        break
    }
}
print("\(b.id)|\(b.x)|\(b.y)|\(b.w)|\(b.h)|\(b.owner)|\(displayName)|\(isPrimary)")
SWIFT_EOF
)
[ -z "$WIN_INFO" ] && fail "window not found (RoK running?)"

IFS='|' read -r WID X Y W H OWNER DISPLAY IS_PRIMARY <<<"$WIN_INFO"
CX=$((X + W / 2))
CY=$((Y + H / 2))
log "  found: id=$WID owner=\"$OWNER\" pos=($X,$Y) size=${W}x${H}"
log "  click center: ($CX, $CY) on display \"$DISPLAY\""

if [ "$IS_PRIMARY" = "true" ]; then
    log "  ⚠️  RoK is on the PRIMARY display — the architecture's 'invisible' claim doesn't hold."
    log "      Park RoK on a virtual display (BetterDisplay) before relying on this in production."
fi

# ------------------------------------------------------------- capture before
log "STEP 1/4 — capture before click"
screencapture -l "$WID" -x /tmp/p2_spike_before.png 2>/dev/null
[ -s /tmp/p2_spike_before.png ] || fail "before-capture produced no file (Screen Recording denied?)"
SA=$(stat -f %z /tmp/p2_spike_before.png)
log "  saved /tmp/p2_spike_before.png ($SA bytes)"

# ------------------------------------------------------------- click
log "STEP 2/4 — post CGEvent click at ($CX, $CY)"
swift /dev/stdin "$CX" "$CY" <<'SWIFT_CLICK_EOF'
import Foundation
import CoreGraphics
let x = Double(CommandLine.arguments[1])!
let y = Double(CommandLine.arguments[2])!
let pos = CGPoint(x: x, y: y)
guard let src = CGEventSource(stateID: .hidSystemState) else {
    FileHandle.standardError.write("could not create CGEventSource — Accessibility denied?\n".data(using: .utf8)!)
    exit(1)
}
let down = CGEvent(mouseEventSource: src, mouseType: .leftMouseDown, mouseCursorPosition: pos, mouseButton: .left)!
let up   = CGEvent(mouseEventSource: src, mouseType: .leftMouseUp,   mouseCursorPosition: pos, mouseButton: .left)!
down.post(tap: .cghidEventTap)
Thread.sleep(forTimeInterval: 0.1)
up.post(tap: .cghidEventTap)
SWIFT_CLICK_EOF
sleep 1.2

# ------------------------------------------------------------- capture after
log "STEP 3/4 — capture after click"
screencapture -l "$WID" -x /tmp/p2_spike_after.png 2>/dev/null
[ -s /tmp/p2_spike_after.png ] || fail "after-capture produced no file"
SB=$(stat -f %z /tmp/p2_spike_after.png)
log "  saved /tmp/p2_spike_after.png ($SB bytes)"

# ------------------------------------------------------------- diff + verdict
log "STEP 4/4 — diff"
DIFF=$(cmp -l /tmp/p2_spike_before.png /tmp/p2_spike_after.png 2>/dev/null | wc -l | tr -d ' ')
log "  $DIFF differing bytes"

echo
echo "=================================================="
echo "       P2 SPIKE VERDICT"
echo "=================================================="
echo " Window:        id=$WID on \"$DISPLAY\""
echo " Click center:  ($CX, $CY)"
echo " Frame sizes:   $SA → $SB bytes"
echo " Diff:          $DIFF bytes"
echo "--------------------------------------------------"
if [ "$DIFF" -gt 100000 ]; then
    echo " ✅ ARCHITECTURE ALIVE — capture + click both work end-to-end"
elif [ "$DIFF" -gt 1000 ]; then
    echo " ⚠️  click effect ambiguous — may be animation noise"
else
    echo " ❌ click had no visible effect — RoK on a static screen, OR"
    echo "    click did not land. Re-run when RoK shows an animated state."
fi
if [ "$IS_PRIMARY" = "true" ]; then
    echo
    echo " NOTE: RoK is on the primary display. The 'invisible to user'"
    echo "       claim only holds when RoK is on a non-primary display."
fi
echo "=================================================="
