import AppKit

SingleInstanceGuard.acquireOrActivateExistingAndExit()

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.run()
