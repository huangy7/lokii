import AppKit

/// Custom window subclass that handles keyboard events for the search interface.
/// Escape closes the window, Enter opens the selected result,
/// and arrow keys navigate while the search field has focus.
final class LokiiWindow: NSWindow {
    var onEscape: (() -> Void)?
    var onReturn: (() -> Void)?

    override func keyDown(with event: NSEvent) {
        switch Int(event.keyCode) {
        case 53: // Escape
            onEscape?()
        case 36: // Return
            onReturn?()
        default:
            super.keyDown(with: event)
        }
    }

    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { true }
}
