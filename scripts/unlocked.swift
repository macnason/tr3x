// Exits 0 when the console session is unlocked and the main display is awake, else 1.
import CoreGraphics

let session = CGSessionCopyCurrentDictionary() as? [String: Any] ?? [:]
let locked = (session["CGSSessionScreenIsLocked"] as? Bool) ?? false
let asleep = CGDisplayIsAsleep(CGMainDisplayID()) != 0
exit(locked || asleep ? 1 : 0)
