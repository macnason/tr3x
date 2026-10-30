// Prints the window number of the Rex app's main window, for `screencapture -l`.
import CoreGraphics

let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
let rex = list.filter {
    ($0["kCGWindowOwnerName"] as? String ?? "").hasPrefix("Rex")
        && ($0["kCGWindowLayer"] as? Int ?? 1) == 0
        && (($0["kCGWindowBounds"] as? [String: Any])?["Height"] as? Double ?? 0) > 200
}
if let w = rex.first, let n = w["kCGWindowNumber"] { print(n) }
