import AppKit
import SwiftUI

final class FDAGuideWindowController: NSWindowController, NSWindowDelegate {
    static var current: FDAGuideWindowController?

    /// “不再提示”标记的持久化键；引导窗、视图与应用代理共用同一份定义，避免字面量拼写漂移
    static let promptDismissedKey = "FDAPromptDismissed"

    /// 引导窗是否正在展示；供上层判断当前是否处于权限引导流程
    static var isPresenting: Bool {
        current?.window?.isVisible == true
    }

    /// 用户是否已勾选“不再提示”
    static var isPromptDismissed: Bool {
        UserDefaults.standard.bool(forKey: promptDismissedKey)
    }

    /// 清除“不再提示”标记，使权限引导在下次权限受限时重新可见
    static func clearPromptDismissed() {
        UserDefaults.standard.removeObject(forKey: promptDismissedKey)
    }

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
            contentRect: NSRect(x: 0, y: 0, width: 410, height: 166),
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

        // 隐藏不适用的最小化与缩放按钮，保持轻量弹窗整洁外观
        window.standardWindowButton(.miniaturizeButton)?.isHidden = true
        window.standardWindowButton(.zoomButton)?.isHidden = true

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
            UserDefaults.standard.set(true, forKey: Self.promptDismissedKey)
        } else {
            Self.clearPromptDismissed()
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
