import Foundation
import CoreGraphics
import AppKit

let needle = CommandLine.arguments.dropFirst().first ?? "RiseOfKingdoms"

// 1. Enumerate displays via CGDirectDisplayID
print("=== DISPLAYS ===")
var onlineCount: UInt32 = 0
CGGetOnlineDisplayList(0, nil, &onlineCount)
var onlineIDs = [CGDirectDisplayID](repeating: 0, count: Int(onlineCount))
CGGetOnlineDisplayList(onlineCount, &onlineIDs, &onlineCount)
for did in onlineIDs {
    let bounds = CGDisplayBounds(did)
    let isMain = CGDisplayIsMain(did) != 0
    let isActive = CGDisplayIsActive(did) != 0
    let isOnline = CGDisplayIsOnline(did) != 0
    let isVirtual = CGDisplayIsAlwaysInMirrorSet(did) != 0  // weak signal
    print("displayID=\(did) bounds=(\(Int(bounds.origin.x)),\(Int(bounds.origin.y)) \(Int(bounds.size.width))x\(Int(bounds.size.height))) main=\(isMain) active=\(isActive) online=\(isOnline)")
}

print("=== NSSCREENS ===")
for s in NSScreen.screens {
    let f = s.frame
    let did = (s.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value ?? 0
    let name = s.localizedName
    print("nsscreen displayID=\(did) frame=(\(Int(f.origin.x)),\(Int(f.origin.y)) \(Int(f.size.width))x\(Int(f.size.height))) name=\"\(name)\"")
}

// 2. Find RoK windows
print("=== ROK WINDOWS ===")
guard let infoList = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as NSArray? else {
    print("ERROR: CGWindowListCopyWindowInfo returned nil")
    exit(1)
}
var found = 0
for case let info as NSDictionary in infoList {
    let owner = info[kCGWindowOwnerName as String] as? String ?? ""
    let title = info[kCGWindowName as String] as? String ?? ""
    if owner.lowercased().contains(needle.lowercased()) || title.lowercased().contains(needle.lowercased()) {
        let id = info[kCGWindowNumber as String] as? Int ?? -1
        let pid = info[kCGWindowOwnerPID as String] as? Int ?? -1
        let layer = info[kCGWindowLayer as String] as? Int ?? -1
        let bounds = info[kCGWindowBounds as String] as? [String: Any]
        let x = bounds?["X"] as? Double ?? 0
        let y = bounds?["Y"] as? Double ?? 0
        let w = bounds?["Width"] as? Double ?? 0
        let h = bounds?["Height"] as? Double ?? 0
        let cx = x + w/2.0
        let cy = y + h/2.0

        // Resolve which display the center is on
        var onDisplay: CGDirectDisplayID = 0
        for did in onlineIDs {
            let b = CGDisplayBounds(did)
            if cx >= b.origin.x && cx < b.origin.x + b.size.width &&
               cy >= b.origin.y && cy < b.origin.y + b.size.height {
                onDisplay = did
                break
            }
        }
        let mainID = CGMainDisplayID()
        let modeLabel = onDisplay == 0 ? "OFF_ALL_DISPLAYS" : (onDisplay == mainID ? "Mode1_Visible(primary)" : "Mode2_Virtual(displayID=\(onDisplay))")

        print("window id=\(id) pid=\(pid) layer=\(layer) bounds=(\(Int(x)),\(Int(y)) \(Int(w))x\(Int(h))) center=(\(Int(cx)),\(Int(cy))) -> \(modeLabel) owner=\"\(owner)\" title=\"\(title)\"")
        found += 1
    }
}
if found == 0 {
    print("NO_ROK_WINDOWS_FOUND")
}
print("===")
