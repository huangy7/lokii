import AppKit

/// Custom window subclass that handles keyboard events for the search interface.
/// Escape closes the window, Enter opens the selected result,
/// and arrow keys navigate while the search field has focus.
final class LokiiWindow: NSWindow {
    var onEscape: (() -> Void)?
    var onReturn: (() -> Void)?
    var onCommandF: (() -> Void)?
    var onOptionF: (() -> Void)?

    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        let flags = event.modifierFlags.intersection([.command, .option, .control, .shift])
        if flags == .option && event.charactersIgnoringModifiers?.lowercased() == "f" {
            if let onOptionF = onOptionF {
                onOptionF()
                return true
            }
        }
        return super.performKeyEquivalent(with: event)
    }

    override func keyDown(with event: NSEvent) {
        switch Int(event.keyCode) {
        case 53: // Escape
            onEscape?()
        case 36: // Return
            onReturn?()
        default:
            let flags = event.modifierFlags.intersection([.command, .option, .control, .shift])
            if flags == .command,
               event.charactersIgnoringModifiers?.lowercased() == "f" {
                onCommandF?()
                return
            }
            if flags == .option,
               event.charactersIgnoringModifiers?.lowercased() == "f" {
                onOptionF?()
                return
            }
            super.keyDown(with: event)
        }
    }

    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { true }
}
