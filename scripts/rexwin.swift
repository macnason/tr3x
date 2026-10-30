// Prints the window number of the Rex app's main window, for `screencapture -l`: the largest
// on-screen Rex window that isn't Settings or Diagnostics. A window's name needs the Screen
// Recording permission, so size decides when names are hidden.
import CoreGraphics

let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
func area(_ w: [String: Any]) -> Double {
    let b = w["kCGWindowBounds"] as? [String: Any] ?? [:]
    return (b["Width"] as? Double ?? 0) * (b["Height"] as? Double ?? 0)
}
let rex = list.filter {
    let name = $0["kCGWindowName"] as? String ?? ""
    return ($0["kCGWindowOwnerName"] as? String ?? "").hasPrefix("Rex")
        && ($0["kCGWindowLayer"] as? Int ?? 1) == 0
        && !name.contains("Settings") && !name.contains("Diagnostics")
        && ((($0["kCGWindowBounds"] as? [String: Any])?["Height"] as? Double) ?? 0) > 200
}
if let w = rex.max(by: { area($0) < area($1) }), let n = w["kCGWindowNumber"] { print(n) }
