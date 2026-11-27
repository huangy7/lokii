import AppKit
import Quartz

/// 用于解耦 AppKit 窗口实例的轻量快照，便于单元测试验证窗口过滤与策略决策逻辑。
struct DockWindowSnapshot: Equatable {
    let identifier: ObjectIdentifier?
    let className: String
    let level: NSWindow.Level
    let isVisible: Bool
    let isMiniaturized: Bool
    let hasTitledStyle: Bool
    let isExcludedAuxiliary: Bool

    init(
        identifier: ObjectIdentifier? = nil,
        className: String,
        level: NSWindow.Level = .normal,
        isVisible: Bool,
        isMiniaturized: Bool = false,
        hasTitledStyle: Bool = true,
        isExcludedAuxiliary: Bool = false
    ) {
        self.identifier = identifier
        self.className = className
        self.level = level
        self.isVisible = isVisible
        self.isMiniaturized = isMiniaturized
        self.hasTitledStyle = hasTitledStyle
        self.isExcludedAuxiliary = isExcludedAuxiliary
    }

    init(window: NSWindow) {
        let name = NSStringFromClass(type(of: window))
        let excludedByClass = Self.isExcludedClassName(name)
        let isQuickLook = window is QLPreviewPanel
        self.init(
            identifier: ObjectIdentifier(window),
            className: name,
            level: window.level,
            isVisible: window.isVisible,
            isMiniaturized: window.isMiniaturized,
            hasTitledStyle: window.styleMask.contains(.titled),
            isExcludedAuxiliary: excludedByClass || isQuickLook
        )
    }

    static func isExcludedClassName(_ className: String) -> Bool {
        className.contains("StatusBar")
            || className.contains("Popover")
            || className.contains("ToolTip")
            || className.contains("MenuWindow")
    }

    /// 仅将可见或已最小化的常规/浮层/模态用户窗口计入活跃窗口；
    /// 自动排除状态栏宿主窗、Popover、ToolTip、菜单窗、QuickLook 及无标题系统辅助浮层。
    var countsAsOpenUserWindow: Bool {
        guard isVisible || isMiniaturized else { return false }
        guard !isExcludedAuxiliary else { return false }
        guard level.rawValue < NSWindow.Level.mainMenu.rawValue else { return false }
        return hasTitledStyle
    }
}

/// 集中协调 macOS 应用激活策略（`.regular` / `.accessory`）与窗口生命周期。
///
/// 仅在窗口呈现前、收起/关闭后与配置变更等离散事件上触发策略评估，
/// 不运行轮询定时器，且在目标策略与当前一致时直接短路，避免多余的系统调用。
final class DockVisibilityCoordinator {
    private(set) var mode: LokiiConfig.DockIconMode
    private var isTerminating = false
    private var syncScheduled = false
    private var pendingExcludedWindowIds: Set<ObjectIdentifier> = []

    private let policyReader: () -> NSApplication.ActivationPolicy
    private let policySetter: (NSApplication.ActivationPolicy) -> Bool
    private let windowSnapshotProvider: () -> [DockWindowSnapshot]
    private let activeWindowRestorerBuilder: () -> (() -> Void)?
    private let asyncScheduler: (@escaping () -> Void) -> Void
    private let notificationCenter: NotificationCenter
    private var observers: [NSObjectProtocol] = []

    init(
        initialMode: LokiiConfig.DockIconMode,
        notificationCenter: NotificationCenter = .default,
        observeWindowNotifications: Bool = true,
        policyReader: @escaping () -> NSApplication.ActivationPolicy = { NSApp.activationPolicy() },
        policySetter: @escaping (NSApplication.ActivationPolicy) -> Bool = { NSApp.setActivationPolicy($0) },
        windowSnapshotProvider: @escaping () -> [DockWindowSnapshot] = {
            NSApp.windows.map(DockWindowSnapshot.init(window:))
        },
        activeWindowRestorerBuilder: @escaping () -> (() -> Void)? = {
            DockVisibilityCoordinator.makeFrontWindowRestorer()
        },
        asyncScheduler: @escaping (@escaping () -> Void) -> Void = { block in
            DispatchQueue.main.async(execute: block)
        }
    ) {
        self.mode = initialMode
        self.notificationCenter = notificationCenter
        self.policyReader = policyReader
        self.policySetter = policySetter
        self.windowSnapshotProvider = windowSnapshotProvider
        self.activeWindowRestorerBuilder = activeWindowRestorerBuilder
        self.asyncScheduler = asyncScheduler

        if observeWindowNotifications {
            installWindowObservers()
        }
    }

    deinit {
        for observer in observers {
            notificationCenter.removeObserver(observer)
        }
    }

    /// 纯策略映射函数：根据用户配置与当前是否存在打开的用户窗口决定目标激活策略。
    static func targetPolicy(
        for mode: LokiiConfig.DockIconMode,
        hasOpenUserWindow: Bool
    ) -> NSApplication.ActivationPolicy {
        switch mode {
        case .always:
            return .regular
        case .whenOpen:
            return hasOpenUserWindow ? .regular : .accessory
        case .never:
            return .accessory
        }
    }

    /// 判断窗口快照集合（剔除正在关闭的窗口）中是否存在打开的用户窗口。
    static func hasOpenUserWindow(
        in snapshots: [DockWindowSnapshot],
        excluding excludedIds: Set<ObjectIdentifier> = []
    ) -> Bool {
        snapshots.contains { snapshot in
            if let id = snapshot.identifier, excludedIds.contains(id) {
                return false
            }
            return snapshot.countsAsOpenUserWindow
        }
    }

    /// 窗口呈现前同步调用：在 `makeKeyAndOrderFront` 与 `NSApp.activate` 之前完成策略升格，
    /// 规避从 `.accessory` 切 `.regular` 时的焦点抢占时序竞争。
    /// - Returns: 若本次调用实际将策略切换至 `.regular`，返回 `true`。
    @discardableResult
    func prepareForWindowPresentation() -> Bool {
        guard !isTerminating else { return false }
        let desired = Self.targetPolicy(for: mode, hasOpenUserWindow: true)
        let changed = applyPolicyIfNeeded(desired, preserveActiveWindow: false)
        return changed && desired == .regular
    }

    /// 更新用户配置的程序坞图标模式并立即生效（无需重启）。
    func updateMode(_ newMode: LokiiConfig.DockIconMode) {
        guard !isTerminating else { return }
        mode = newMode
        syncPolicyNow(preserveActiveWindowIfDroppingToAccessory: true)
    }

    /// 合并同一 runloop 回合内的多次窗口事件，异步重算目标激活策略。
    func schedulePolicySync(excluding window: NSWindow? = nil) {
        guard !isTerminating else { return }
        if let window {
            pendingExcludedWindowIds.insert(ObjectIdentifier(window))
        }
        guard !syncScheduled else { return }
        syncScheduled = true
        asyncScheduler { [weak self] in
            guard let self else { return }
            let excluded = self.pendingExcludedWindowIds
            self.pendingExcludedWindowIds.removeAll()
            self.syncScheduled = false
            self.syncPolicyNow(excluding: excluded, preserveActiveWindowIfDroppingToAccessory: false)
        }
    }

    /// 立即按当前窗口快照评估并应用激活策略。
    func syncPolicyNow(
        excluding excludedIds: Set<ObjectIdentifier> = [],
        preserveActiveWindowIfDroppingToAccessory: Bool = false
    ) {
        guard !isTerminating else { return }
        let snapshots = windowSnapshotProvider()
        let hasOpenWindow = Self.hasOpenUserWindow(in: snapshots, excluding: excludedIds)
        let desired = Self.targetPolicy(for: mode, hasOpenUserWindow: hasOpenWindow)
        // 仅当用户在设置窗等前台界面中主动切入 `.never`（切为 `.accessory` 且仍有可见窗口）时保活前台焦点；
        // 最后一个窗口收起/关闭导致的 `.accessory` 切换不应抢夺焦点，以便自然交还给前一个前台应用。
        let shouldPreserveWindow = preserveActiveWindowIfDroppingToAccessory
            && desired == .accessory
            && hasOpenWindow
        applyPolicyIfNeeded(desired, preserveActiveWindow: shouldPreserveWindow)
    }

    /// 应用进入退出流程时调用，阻止销毁窗口阶段再触发无意义的程序坞图标回落动画。
    func beginTermination() {
        isTerminating = true
        pendingExcludedWindowIds.removeAll()
        syncScheduled = false
    }

    @discardableResult
    private func applyPolicyIfNeeded(
        _ target: NSApplication.ActivationPolicy,
        preserveActiveWindow: Bool
    ) -> Bool {
        let current = policyReader()
        guard current != target else { return false }
        // 在切换策略之前先捕获前台窗口状态，避免 `setActivationPolicy(.accessory)` 同步清掉 `NSApp.isActive`
        let restoreFrontWindow = preserveActiveWindow ? activeWindowRestorerBuilder() : nil
        let applied = policySetter(target)
        if applied {
            restoreFrontWindow?()
        }
        return applied
    }

    private func installWindowObservers() {
        let willClose = notificationCenter.addObserver(
            forName: NSWindow.willCloseNotification,
            object: nil,
            queue: .main
        ) { [weak self] notification in
            let closingWindow = notification.object as? NSWindow
            self?.schedulePolicySync(excluding: closingWindow)
        }

        let didBecomeKey = notificationCenter.addObserver(
            forName: NSWindow.didBecomeKeyNotification,
            object: nil,
            queue: .main
        ) { [weak self] notification in
            guard let self, !self.isTerminating else { return }
            guard let window = notification.object as? NSWindow else { return }
            guard DockWindowSnapshot(window: window).countsAsOpenUserWindow else { return }
            self.syncPolicyNow()
        }

        let didDeminiaturize = notificationCenter.addObserver(
            forName: NSWindow.didDeminiaturizeNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            self?.schedulePolicySync()
        }

        observers = [willClose, didBecomeKey, didDeminiaturize]
    }

    /// 在 `.regular -> .accessory` 切换前捕获当前前台窗口；切换完成后保持该窗口置前并维持激活态，
    /// 防止用户在设置窗口切换为“从不”时窗口被系统自动退至其他应用后方。
    private static func makeFrontWindowRestorer() -> (() -> Void)? {
        guard NSApp.isActive else { return nil }
        let frontWindow = NSApp.keyWindow ?? NSApp.mainWindow
        guard let frontWindow, frontWindow.isVisible else { return nil }
        return { [weak frontWindow] in
            guard let frontWindow, frontWindow.isVisible else { return }
            NSApp.activate(ignoringOtherApps: true)
            frontWindow.makeKeyAndOrderFront(nil)
            DispatchQueue.main.async { [weak frontWindow] in
                guard let frontWindow, frontWindow.isVisible else { return }
                NSApp.activate(ignoringOtherApps: true)
                frontWindow.makeKeyAndOrderFront(nil)
            }
        }
    }
}
