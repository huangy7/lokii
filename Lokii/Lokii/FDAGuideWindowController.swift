import AppKit
import SwiftUI

final class FDAGuideWindowController: NSWindowController, NSWindowDelegate {
    static var current: FDAGuideWindowController?

    static func show() {
        if let existing = current, existing.window?.isVisible == true {
            existing.window?.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }
        let controller = FDAGuideWindowController()
        current = controller
        controller.showWindow(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    static func closeIfOpen() {
        current?.close()
        current = nil
    }

    init() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 380, height: 154),
            styleMask: [.titled, .closable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.title = L("fda.windowTitle")
        window.titleVisibility = .hidden
        window.isReleasedWhenClosed = false
        window.isMovableByWindowBackground = true
        window.titlebarAppearsTransparent = true
        window.level = .floating
        window.center()

        super.init(window: window)
        window.delegate = self

        let guideView = FDAGuideView(
            onLater: { [weak self] dontShowAgain in
                self?.handleDismiss(dontShowAgain: dontShowAgain)
            },
            onOpenSettings: { [weak self] dontShowAgain in
                self?.handleOpenSettings(dontShowAgain: dontShowAgain)
            }
        )
        window.contentView = NSHostingView(rootView: guideView)
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    func windowWillClose(_ notification: Notification) {
        if Self.current === self {
            Self.current = nil
        }
    }

    private func handleDismiss(dontShowAgain: Bool) {
        updateDismissedState(dontShowAgain: dontShowAgain)
        window?.close()
        if Self.current === self {
            Self.current = nil
        }
    }

    private func handleOpenSettings(dontShowAgain: Bool) {
        updateDismissedState(dontShowAgain: dontShowAgain)
        Self.openSystemSettingsPage()
        // Window stays open and floating so user can drag the icon into System Settings!
    }

    private func updateDismissedState(dontShowAgain: Bool) {
        if dontShowAgain {
            UserDefaults.standard.set(true, forKey: "FDAPromptDismissed")
        } else {
            UserDefaults.standard.removeObject(forKey: "FDAPromptDismissed")
        }
    }

    static func openSystemSettingsPage() {
        let urls = [
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles",
            "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles",
        ]

        for rawValue in urls {
            guard let url = URL(string: rawValue) else { continue }
            if NSWorkspace.shared.open(url) {
                return
            }
        }
    }
}
