import AppKit

/// 让 Dock 图标跟随可见窗口：窗口在时以普通应用运行，窗口全部关闭后回到菜单栏应用。
///
/// `show_in_dock = true` 表示常驻 Dock；为 false 时仅在存在可见窗口期间显示，
/// 关闭最后一个窗口后立即隐藏。配置变更会同步更新显示策略。
final class DockVisibilityController {
    private let engine: SearchEngine
    private var observers: [NSObjectProtocol] = []
    private var pendingSync: DispatchWorkItem?
    private var fallbackTimer: Timer?
    private var showInDock = true

    init(engine: SearchEngine) {
        self.engine = engine
    }

    deinit {
        pendingSync?.cancel()
        fallbackTimer?.invalidate()
        observers.forEach { NotificationCenter.default.removeObserver($0) }
    }

    func start() {
        guard observers.isEmpty else { return }

        showInDock = engine.getConfig()?.general.showInDock ?? true

        let center = NotificationCenter.default
        for name in [
            NSWindow.didBecomeKeyNotification,
            NSWindow.didBecomeMainNotification,
            NSWindow.didMiniaturizeNotification,
            NSWindow.didDeminiaturizeNotification,
            NSWindow.willCloseNotification,
            NSWindow.didChangeOcclusionStateNotification,
            NSApplication.didBecomeActiveNotification,
            NSApplication.didResignActiveNotification,
            Notification.Name.lokiiSearchWindowDidHide,
        ] {
            observers.append(center.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                let delay: TimeInterval = name == NSWindow.didBecomeKeyNotification
                    || name == NSWindow.didBecomeMainNotification
                    || name == NSApplication.didBecomeActiveNotification
                    || name == NSWindow.didDeminiaturizeNotification ? 0 : 0.08
                self?.scheduleSync(after: delay)
            })
        }

        observers.append(center.addObserver(
            forName: SearchEngine.configReloadedNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            guard let self else { return }
            self.showInDock = self.engine.getConfig()?.general.showInDock ?? true
            self.scheduleSync(after: 0)
        })

        // macOS 在部分激活路径上会把 accessory 应用提升回 regular；定时做一次幂等校正。
        fallbackTimer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
            self?.sync()
        }

        sync()
    }

    private func scheduleSync(after delay: TimeInterval) {
        pendingSync?.cancel()
        let work = DispatchWorkItem { [weak self] in self?.sync() }
        pendingSync = work
        DispatchQueue.main.asyncAfter(deadline: .now() + delay, execute: work)
    }

    private func sync() {
        let target: NSApplication.ActivationPolicy = (showInDock || hasVisibleUserWindow)
            ? .regular
            : .accessory

        let current = NSApp.activationPolicy()
        guard current != target else { return }
        guard NSApp.setActivationPolicy(target) else { return }

        // 从菜单栏应用切回普通应用后，系统需要一次激活才会显示 App 菜单栏。
        if target == .regular, current == .accessory {
            NSApp.activate(ignoringOtherApps: true)
        }
    }

    private var hasVisibleUserWindow: Bool {
        NSApp.windows.contains { window in
            guard window.isVisible || window.isMiniaturized else { return false }
            guard window.styleMask.contains(.titled) else { return false }
            guard !window.styleMask.contains(.nonactivatingPanel) else { return false }
            return true
        }
    }
}
