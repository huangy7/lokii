import AppKit
import SwiftUI

final class PreferencesWindowController: NSWindowController, NSWindowDelegate {
    private let engine: SearchEngine
    private var viewModel: PreferencesViewModel?
    private var allowCloseAfterUnsavedPrompt = false

    init(engine: SearchEngine) {
        self.engine = engine

        let vm = PreferencesViewModel(engine: engine)
        self.viewModel = vm

        let host = NSHostingController(rootView: PreferencesView(viewModel: vm))
        let window = NSWindow(contentViewController: host)
        window.styleMask = [.titled, .closable, .resizable, .miniaturizable]
        window.title = L("prefs.windowTitle")
        window.toolbarStyle = .unified
        window.minSize = NSSize(width: 580, height: 480)
        window.setFrameAutosaveName("LokiiPreferencesWindow")
        window.isReleasedWhenClosed = false
        if !window.setFrameUsingName("LokiiPreferencesWindow") {
            window.setContentSize(NSSize(width: 620, height: 520))
            window.center()
        }

        super.init(window: window)
        window.delegate = self
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    override func showWindow(_ sender: Any?) {
        super.showWindow(sender)
        viewModel?.loadConfig()
        viewModel?.updateFDAStatus()
    }

    func showWindow(tab: String) {
        viewModel?.selectedTab = tab
        showWindow(nil)
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        if allowCloseAfterUnsavedPrompt {
            allowCloseAfterUnsavedPrompt = false
            return true
        }

        guard let viewModel = viewModel, viewModel.isIndexDirty else { return true }
        
        handleUnsavedIndexChangesIfNeeded { [weak self] shouldClose in
            guard let self = self, shouldClose else { return }
            self.allowCloseAfterUnsavedPrompt = true
            self.window?.performClose(nil)
        }
        
        return false
    }

    private func handleUnsavedIndexChangesIfNeeded(completion: @escaping (Bool) -> Void) {
        guard let window = window else {
            completion(true)
            return
        }

        let alert = NSAlert()
        alert.messageText = L("prefs.unsavedChanges.title")
        alert.informativeText = L("prefs.unsavedChanges.message")
        
        let saveButton = alert.addButton(withTitle: L("prefs.unsavedChanges.save"))
        saveButton.keyEquivalent = "\r"
        alert.addButton(withTitle: L("prefs.unsavedChanges.discard"))
        alert.addButton(withTitle: L("prefs.unsavedChanges.cancel"))

        alert.beginSheetModal(for: window) { [weak self] response in
            guard let self = self, let viewModel = self.viewModel else {
                completion(false)
                return
            }

            switch response {
            case .alertFirstButtonReturn:
                completion(viewModel.saveIndexChanges())
            case .alertSecondButtonReturn:
                viewModel.discardIndexChanges()
                completion(true)
            default:
                completion(false)
            }
        }
    }
}
