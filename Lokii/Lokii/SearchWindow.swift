import AppKit
import Combine
import UniformTypeIdentifiers
import Quartz
import SwiftUI

/// Container view that intercepts clicks outside interactiveCardView while routing clicks inside to the card.
private final class OverlayContainerView: NSView {
    weak var interactiveCardView: NSView?

    override var acceptsFirstResponder: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func hitTest(_ point: NSPoint) -> NSView? {
        if let card = interactiveCardView {
            let pointInCard = card.convert(point, from: self)
            if card.bounds.contains(pointInCard), let hitView = card.hitTest(pointInCard) {
                return hitView
            }
        }
        return self
    }

    override func mouseDown(with event: NSEvent) {}
    override func mouseUp(with event: NSEvent) {}
    override func keyDown(with event: NSEvent) {}
    override func keyUp(with event: NSEvent) {}
}

final class SearchWindow: NSWindowController {
    private let engine: SearchEngine
    private var defaultWindowFrame: NSRect {
        Self.calculateDefaultWindowFrame()
    }

    /// 基于指定或当前主屏幕可用工作区几何尺寸，按黄金视觉比例自适应计算默认尺寸与居中位置
    static func calculateDefaultWindowFrame(on targetScreen: NSScreen? = nil) -> NSRect {
        guard let screen = targetScreen ?? NSScreen.main ?? NSScreen.screens.first else {
            return NSRect(x: 0, y: 0, width: 960, height: 640)
        }
        return WindowFrameManager.defaultFrame(on: ScreenMetrics(screen: screen))
    }

    /// 在目标屏幕可见工作区内保留既有窗口宽高并黄金居中，避免跨屏唤醒重置用户自定义窗口尺寸
    static func calculateCenteredFrame(preservingSize size: NSSize, on screen: NSScreen) -> NSRect {
        WindowFrameManager.centeredFrame(preservingSize: size, on: ScreenMetrics(screen: screen))
    }

    private let frameManager = WindowFrameManager()
    /// 程序主动调整窗口几何期间置 true，阻断 `windowDidResize` / `windowDidMove` 将自动适配写回用户偏好
    private var isApplyingProgrammaticFrame = false
    /// 跨屏拖拽彻底松手静止后的安全溢出保护状态（仅当窗口宽高真实大于目标屏可见工作区时才温和收缩）
    private var pendingScreenOverflowCheck = false
    private var screenOverflowDebounceWorkItem: DispatchWorkItem?
    private var screenOverflowButtonPollTimer: Timer?
    private static let screenOverflowSettleDelay: TimeInterval = 0.16
    /// 进入空搜索隐藏态期间的重入守卫，防止同步派发空结果回调再次触发 `loadDefaultResults()`
    private var isApplyingIdleEmptyState = false
    let splitViewController = SearchMainSplitViewController()
    var searchResultsViewController: SearchResultsViewController { splitViewController.searchResultsViewController }
    var sidebarTreeViewController: SidebarTreeViewController { splitViewController.sidebarTreeViewController }

    // MARK: - Pin State & Window Level
    private(set) var isPinned: Bool = false
    private let pinButton = NSButton()
    /// 窗口即将显示前的钩子（用于同步升格应用激活策略）；返回 true 表示本次刚从 `.accessory` 切至 `.regular`。
    var onWillShow: (() -> Bool)?
    /// 窗口完成 `orderOut` 收起后的通知钩子，用于驱动动态程序坞图标策略重算。
    var onDidHide: (() -> Void)?
    /// 请求切换至悬浮搜索条形态时回调，并携带当前查询上下文。
    var onRequestSwitchToFloatingBar: ((SearchHandoffState) -> Void)?
    /// 淡出隐藏动画世代令牌：快速重新唤起时递增，取消尚未落地的旧淡出回调防止误收起。
    private var hideAnimationToken: UInt64 = 0

    @objc func togglePin() {
        isPinned.toggle()
        guard let window = window else { return }
        window.level = isPinned ? .floating : .normal
        updatePinButtonState()
    }

    private func updatePinButtonState() {
        let symbolName = isPinned ? "pin.fill" : "pin"
        let config = NSImage.SymbolConfiguration(pointSize: 13, weight: isPinned ? .semibold : .regular)
        pinButton.image = NSImage(systemSymbolName: symbolName, accessibilityDescription: L("search.toolbar.pin"))?
            .withSymbolConfiguration(config)
        pinButton.contentTintColor = isPinned ? .controlAccentColor : .secondaryLabelColor
        pinButton.toolTip = L("search.toolbar.pin")
    }

    private var searchField: NSSearchField!
    private let searchContainer = NSView()
    private let searchDivider = NSBox()
    private let caseSensitivityButton = CaseSensitivityButton()
    private let fuzzyModeButton = FuzzyModeButton()
    private let regexModeButton = RegexModeButton()
    private let filterButton = FilterTriggerButton()
    private let filterPopoverController = FilterPopoverController()
    private var searchWorkItem: DispatchWorkItem?
    static let maxRecentSearches = 8
    static let maxHistoryDisplayLength = 36
    private var currentMode: FfiSearchMode = .auto
    private var currentCaseSensitivity: FfiCaseSensitivity = .auto
    private var currentSort: FfiSearchSort = .relevance
    private var statusText: LocalizedText = .key("search.indexing") {
        didSet { updateStatusLabel() }
    }
    private var searchMatchText: LocalizedText? = nil {
        didSet { updateStatusLabel() }
    }

    // Loading overlay
    private var loadingOverlay: NSView?
    private var overlayState: IndexingOverlayState?
    private var overlayShownAt: Date?
    private var isColdStart: Bool = true  // Default true until startup mode known
    private var isInlineProgressActive: Bool = false
    private var originalSearchPlaceholder: String { L("search.placeholder") }

    var isOverlayVisible: Bool {
        loadingOverlay != nil
    }

    /// Tracks why the loading overlay is showing, so `onIndexReady` from
    /// a concurrent initialize/verify doesn't accidentally dismiss a rebuild overlay.
    private enum IndexingState { case idle, initializing, rebuilding, failed }
    private var indexingState: IndexingState = .initializing {
        didSet { updateEmptyState() }
    }
    private var volumeStatusCancellable: AnyCancellable?
    /// 本次会话中用户点「稍后再说」收起的待确认卷 ID 集合；有新卷进入待确认或重启后自动重新展示。
    private var dismissedConfirmVolumeIds: Set<String> = []

    init(engine: SearchEngine) {
        self.engine = engine

        let window = LokiiWindow(
            contentRect: Self.calculateDefaultWindowFrame(),
            styleMask: [.titled, .closable, .resizable, .miniaturizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        // 不启用 titlebarAppearsTransparent：统一工具栏（.unified）与分栏控制器已原生承载无界顶栏，
        // 透明标题栏会开启点击穿透并诱发客户端与 WindowServer 服务端双重拖拽冲突。
        window.titleVisibility = .hidden
        window.title = "Lokii"
        window.toolbarStyle = .unified
        window.minSize = NSSize(width: 780, height: 420)
        window.isReleasedWhenClosed = false

        super.init(window: window)

        splitViewController.delegate = self
        window.contentViewController = splitViewController

        window.onEscape = { [weak self] in
            self?.handleEscape()
        }
        window.onReturn = { [weak self] in
            self?.openSelected()
        }
        window.onCommandF = { [weak self] in
            self?.focusSearchField()
        }
        window.onOptionF = { [weak self] in
            guard let self = self else { return }
            self.filterPopoverController.toggle(relativeTo: self.filterButton)
        }
        window.onCommandP = { [weak self] in
            self?.togglePin()
        }
        window.onCommandShiftM = { [weak self] in
            self?.requestSwitchToFloatingBar()
        }

        setupUI()
        reloadWindowBehavior()
        applyInitialWindowFrame()
        window.delegate = self
        window.contentView?.wantsLayer = true
        setupRecentSearchesMenu()
        setupNotifications()
        onLanguageChanged()
        updateEmptyState()

        searchResultsViewController.onManageIndexRules = { [weak self] in
            let pathHint: String?
            if let self, self.lastExclusionHint != nil {
                pathHint = Self.extractDiagnosticPathCandidate(from: self.searchField?.stringValue ?? "")
            } else {
                pathHint = nil
            }
            (NSApp.delegate as? AppDelegate)?.openIndexPreferences(diagnosticPath: pathHint)
        }
        searchResultsViewController.onVolumeBannerConfirmAll = { [weak engine] in
            engine?.confirmAllPendingVolumeScans()
        }
        searchResultsViewController.onVolumeBannerCancelSyncing = { [weak engine] in
            guard let engine else { return }
            let syncingVolumes = engine.getVolumeStatuses().filter { $0.state == .syncing }
            for status in syncingVolumes {
                engine.cancelVolumeScan(pathOrId: status.volumeId)
            }
        }
        searchResultsViewController.onVolumeBannerManage = {
            (NSApp.delegate as? AppDelegate)?.openIndexPreferences()
        }
        searchResultsViewController.onVolumeBannerDismiss = { [weak self, weak engine] in
            guard let self, let engine else { return }
            let pendingIds = engine.getVolumeStatuses()
                .filter { $0.state == .needsConfirmation }
                .map(\.volumeId)
            self.dismissedConfirmVolumeIds.formUnion(pendingIds)
            self.refreshVolumeBanner(with: engine.getVolumeStatuses())
        }
        volumeStatusCancellable = engine.$volumeStatuses
            .receive(on: DispatchQueue.main)
            .sink { [weak self] statuses in
                self?.refreshVolumeBanner(with: statuses)
            }
        refreshVolumeBanner(with: engine.getVolumeStatuses())
        searchResultsViewController.onSelectSyntaxExample = { [weak self] example in
            self?.applySyntaxExample(example)
        }
        searchResultsViewController.onSelectRecentSearch = { [weak self] query in
            self?.applySyntaxExample(query)
        }
        searchResultsViewController.onClearRecentSearches = { [weak self] in
            self?.clearRecentSearches()
        }
        searchResultsViewController.onFocusSearchField = { [weak self] in
            self?.focusSearchField()
        }
        searchResultsViewController.onEnableFuzzyMode = { [weak self] in
            guard let self, self.currentMode != .fuzzy else { return }
            self.toggleMode(.fuzzy)
        }
        searchResultsViewController.onOpenFilterPopover = { [weak self] in
            guard let self else { return }
            self.filterPopoverController.toggle(relativeTo: self.filterButton)
        }
        searchResultsViewController.onOpenFDAGuide = {
            FDAGuideWindowController.show()
        }
        searchResultsViewController.isPathOfflineHandler = { [weak engine] path in
            engine?.isPathOffline(path) ?? false
        }
        searchResultsViewController.onExcludeDirectory = { [weak self, weak engine] dirPath in
            guard let self = self, let engine = engine else { return }
            guard var config = engine.getConfig() else { return }
            if !config.index.excludePaths.contains(dirPath) {
                config.index.excludePaths.append(dirPath)
                _ = engine.updateConfig(config)
                let currentQuery = self.searchField.stringValue
                if currentQuery.isEmpty {
                    self.loadDefaultResults()
                } else {
                    self.performSearch(query: currentQuery)
                }
            }
        }
        // 终端唤起按当前配置派发，闭包在装配点注入而非依赖进程级全局状态
        searchResultsViewController.openInTerminalHandler = { [weak engine] urls in
            TerminalLauncher.openBatch(
                at: urls,
                preferredIdentifier: engine?.getConfig()?.general.terminalBundleId
            )
        }
        searchResultsViewController.updateFDAStatus(hasFDA: engine.hasFullDiskAccess)
        if engine.stats.isReady {
            loadDefaultResults()
        }
    }

    /// 一键恢复自适应默认窗口尺寸并清除全部按屏尺寸与持久化位置
    @objc func resetToDefaultWindowFrame() {
        guard let window else { return }
        frameManager.clearAllCustomSizesAndOrigin()
        NotificationCenter.default.post(name: .lokiiWindowSizesDidChange, object: nil)
        let targetScreen = window.screen ?? NSScreen.main ?? NSScreen.screens.first
        let targetFrame = Self.calculateDefaultWindowFrame(on: targetScreen)
        applyProgrammaticFrame(targetFrame, display: true, animate: true)
    }

    func currentHandoffState() -> SearchHandoffState {
        SearchHandoffState(
            query: searchField?.stringValue ?? "",
            mode: currentMode,
            caseSensitivity: currentCaseSensitivity
        )
    }

    func applyHandoffState(_ state: SearchHandoffState) {
        currentMode = state.mode
        currentCaseSensitivity = state.caseSensitivity
        currentSort = .relevance
        searchField.stringValue = state.query
        lastQuery = state.query
        updateCaseSensitivityButtonState()
        updateFuzzyModeButtonState()
        updateRegexModeButtonState()
        updateFilterButtonState(forQuery: state.query)
        applyQuerySyntaxHighlighting()
        updateEmptyState()

        if state.query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            loadDefaultResults()
        } else {
            performSearch(query: state.query)
        }
    }

    @objc private func requestSwitchToFloatingBar() {
        onRequestSwitchToFloatingBar?(currentHandoffState())
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    deinit {
        volumeStatusCancellable?.cancel()
        cancelPendingScreenOverflowCheck()
        NotificationCenter.default.removeObserver(self)
    }

    private func refreshVolumeBanner(with statuses: [FfiVolumeStatus]) {
        // 离开待确认态（例如确认扫描完成后或取消回退前）自动从收起集合清理，确保后续再次需要确认时正常弹出。
        let currentPendingIds = Set(statuses.filter { $0.state == .needsConfirmation }.map(\.volumeId))
        dismissedConfirmVolumeIds.formIntersection(currentPendingIds)
        let bannerState = VolumeBannerResolver.resolve(
            statuses: statuses,
            dismissedVolumeIds: dismissedConfirmVolumeIds
        )
        searchResultsViewController.updateVolumeBanner(state: bannerState)
    }

    override func showWindow(_ sender: Any?) {
        guard let window = window else { return }

        hideAnimationToken &+= 1
        let showToken = hideAnimationToken
        let upgradedToRegular = onWillShow?() ?? false

        if window.isMiniaturized {
            window.deminiaturize(nil)
        }

        // 若窗口当前可见但停留在其他桌面空间（Space），先静默移出再于当前活跃空间就地呈现；
        // 避免常驻 .moveToActiveSpace 在「显示器具有单独的空间」模式下于跨屏交界处干扰拖拽。
        if window.isVisible && !window.isOnActiveSpace {
            window.orderOut(nil)
        }

        let isFirstAppear = !window.isVisible
        if isFirstAppear {
            let mouseLoc = NSEvent.mouseLocation
            let mouseScreen = NSScreen.screens.first { NSMouseInRect(mouseLoc, $0.frame, false) }
            if let targetScreen = mouseScreen ?? window.screen ?? NSScreen.main ?? NSScreen.screens.first {
                let allMetrics = NSScreen.screens.map { ScreenMetrics(screen: $0) }
                let newFrame = frameManager.frameOnShow(
                    currentFrame: window.frame,
                    targetScreen: ScreenMetrics(screen: targetScreen),
                    allScreens: allMetrics,
                    rememberPosition: shouldRememberWindowPosition
                )
                applyProgrammaticFrame(newFrame, display: false, animate: false)
            }

            window.alphaValue = 0.0
            if let layer = window.contentView?.layer {
                let center = CGPoint(x: layer.bounds.midX, y: layer.bounds.midY)
                var transform = CATransform3DIdentity
                transform = CATransform3DTranslate(transform, center.x, center.y, 0)
                transform = CATransform3DScale(transform, 0.9, 0.9, 1.0) // Start smaller for more pop
                transform = CATransform3DTranslate(transform, -center.x, -center.y, 0)
                layer.transform = transform
            }
        }
        
        super.showWindow(sender)
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        window.makeFirstResponder(searchField)
        if let editor = searchField.currentEditor() {
            editor.selectAll(nil)
        }

        // 从 `.accessory` 升格到 `.regular` 时，若系统异步完成进程类型变换导致首帧未夺回焦点，
        // 在下一轮主线程 tick 做一次受令牌保护的补激活，避免搜索框无法直接键入。
        if upgradedToRegular {
            DispatchQueue.main.async { [weak self] in
                guard let self, self.hideAnimationToken == showToken,
                      let window = self.window, window.isVisible, !NSApp.isActive else { return }
                NSApp.activate(ignoringOtherApps: true)
                window.makeKeyAndOrderFront(nil)
                window.makeFirstResponder(self.searchField)
            }
        }
        
        if isFirstAppear {
            // Quick fade in
            NSAnimationContext.runAnimationGroup({ ctx in
                ctx.duration = 0.15
                window.animator().alphaValue = 1.0
            })
            
            // 窗口弹出采用弹性阻尼微动效，增强视觉响应与交互质感
            if let layer = window.contentView?.layer {
                let spring = CASpringAnimation(keyPath: "transform")
                spring.fromValue = layer.transform
                spring.toValue = CATransform3DIdentity
                spring.damping = 20
                spring.stiffness = 300
                spring.mass = 1.0
                spring.duration = spring.settlingDuration
                layer.transform = CATransform3DIdentity
                layer.add(spring, forKey: "springScale")
            }
        } else {
            // 窗口可能正处于淡出收起动画中途（isVisible 仍为 true）；用零时长动画覆盖在途的透明度衰减，
            // 防止旧淡出回调被令牌取消后窗口停留在全透明状态。
            NSAnimationContext.runAnimationGroup { ctx in
                ctx.duration = 0
                window.animator().alphaValue = 1.0
            }
            window.alphaValue = 1.0
            window.contentView?.layer?.transform = CATransform3DIdentity
        }
    }

    /// 统一窗口收起入口：成对关闭过滤浮层与快速查看面板，避免失焦隐藏或全局快捷键收起残留浮窗
    func hideWindow(animated: Bool = true) {
        cancelPendingScreenOverflowCheck()
        if filterPopoverController.isShown {
            filterPopoverController.close()
        }
        if QLPreviewPanel.sharedPreviewPanelExists() && QLPreviewPanel.shared().isVisible {
            QLPreviewPanel.shared().orderOut(nil)
        }
        guard let window = window, window.isVisible else { return }

        hideAnimationToken &+= 1
        let hideToken = hideAnimationToken

        guard animated else {
            NSAnimationContext.runAnimationGroup { ctx in
                ctx.duration = 0
                window.animator().alphaValue = 1.0
            }
            window.orderOut(nil)
            window.alphaValue = 1.0
            window.contentView?.layer?.transform = CATransform3DIdentity
            onDidHide?()
            return
        }

        // Remove scale pop on close for cleaner exit
        NSAnimationContext.runAnimationGroup({ ctx in
            ctx.duration = 0.15
            ctx.timingFunction = CAMediaTimingFunction(name: .easeOut)
            window.animator().alphaValue = 0.0
        }, completionHandler: { [weak self] in
            guard let self, self.hideAnimationToken == hideToken else { return }
            self.window?.orderOut(nil)
            self.window?.alphaValue = 1.0
            self.window?.contentView?.layer?.transform = CATransform3DIdentity
            self.onDidHide?()
        })
    }

    @objc private func hideWindowWithAnimation() {
        hideWindow(animated: true)
    }

    // MARK: - UI Setup

    private func setupUI() {
        // ── Toolbar with standard sidebar toggle, tracking separator, and search field ──
        searchContainer.translatesAutoresizingMaskIntoConstraints = false

        searchField = NSSearchField()
        searchField.translatesAutoresizingMaskIntoConstraints = false
        searchField.placeholderString = L("search.placeholder")
        searchField.font = .systemFont(ofSize: 14)
        // 启用单行文本原生水平滚动，确保输入与粘贴超长路径时光标始终在末尾可视区
        searchField.cell?.isScrollable = true
        searchField.cell?.wraps = false
        searchField.cell?.lineBreakMode = .byClipping
        // 禁用内层独立外焦点环，由外层工具栏胶囊统一承载边缘轮廓，消除双重线框嵌套
        searchField.focusRingType = .none
        searchField.delegate = self
        searchField.sendsSearchStringImmediately = false
        searchField.sendsWholeSearchString = false

        searchDivider.translatesAutoresizingMaskIntoConstraints = false
        searchDivider.boxType = .custom
        searchDivider.borderWidth = 0
        searchDivider.fillColor = .separatorColor

        caseSensitivityButton.translatesAutoresizingMaskIntoConstraints = false
        caseSensitivityButton.target = self
        caseSensitivityButton.action = #selector(caseSensitivityButtonClicked(_:))
        caseSensitivityButton.setContentHuggingPriority(.required, for: .horizontal)
        caseSensitivityButton.setContentCompressionResistancePriority(.required, for: .horizontal)

        fuzzyModeButton.translatesAutoresizingMaskIntoConstraints = false
        fuzzyModeButton.target = self
        fuzzyModeButton.action = #selector(fuzzyModeButtonClicked(_:))
        fuzzyModeButton.setContentHuggingPriority(.required, for: .horizontal)
        fuzzyModeButton.setContentCompressionResistancePriority(.required, for: .horizontal)

        regexModeButton.translatesAutoresizingMaskIntoConstraints = false
        regexModeButton.target = self
        regexModeButton.action = #selector(regexModeButtonClicked(_:))
        regexModeButton.setContentHuggingPriority(.required, for: .horizontal)
        regexModeButton.setContentCompressionResistancePriority(.required, for: .horizontal)

        filterButton.translatesAutoresizingMaskIntoConstraints = false
        filterButton.isBordered = false
        filterButton.focusRingType = .none
        filterButton.imagePosition = .imageLeading
        filterButton.font = .systemFont(ofSize: 11, weight: .semibold)
        filterButton.target = self
        filterButton.action = #selector(filterButtonClicked(_:))
        filterButton.setContentHuggingPriority(.required, for: .horizontal)
        filterButton.setContentCompressionResistancePriority(.required, for: .horizontal)

        searchContainer.addSubview(searchField)
        searchContainer.addSubview(caseSensitivityButton)
        searchContainer.addSubview(fuzzyModeButton)
        searchContainer.addSubview(regexModeButton)
        searchContainer.addSubview(searchDivider)
        searchContainer.addSubview(filterButton)

        NSLayoutConstraint.activate([
            searchContainer.heightAnchor.constraint(equalToConstant: 28),
            searchContainer.widthAnchor.constraint(greaterThanOrEqualToConstant: 360),

            searchField.widthAnchor.constraint(greaterThanOrEqualToConstant: 320),
            searchField.leadingAnchor.constraint(equalTo: searchContainer.leadingAnchor),
            searchField.trailingAnchor.constraint(equalTo: caseSensitivityButton.leadingAnchor, constant: -6),
            searchField.centerYAnchor.constraint(equalTo: searchContainer.centerYAnchor),
            searchField.heightAnchor.constraint(equalToConstant: 28),

            caseSensitivityButton.trailingAnchor.constraint(equalTo: fuzzyModeButton.leadingAnchor, constant: -4),
            caseSensitivityButton.centerYAnchor.constraint(equalTo: searchContainer.centerYAnchor),
            caseSensitivityButton.widthAnchor.constraint(equalToConstant: 28),
            caseSensitivityButton.heightAnchor.constraint(equalToConstant: 20),

            fuzzyModeButton.trailingAnchor.constraint(equalTo: regexModeButton.leadingAnchor, constant: -4),
            fuzzyModeButton.centerYAnchor.constraint(equalTo: searchContainer.centerYAnchor),
            fuzzyModeButton.widthAnchor.constraint(equalToConstant: 28),
            fuzzyModeButton.heightAnchor.constraint(equalToConstant: 20),

            regexModeButton.trailingAnchor.constraint(equalTo: searchDivider.leadingAnchor, constant: -5),
            regexModeButton.centerYAnchor.constraint(equalTo: searchContainer.centerYAnchor),
            regexModeButton.widthAnchor.constraint(equalToConstant: 28),
            regexModeButton.heightAnchor.constraint(equalToConstant: 20),

            searchDivider.trailingAnchor.constraint(equalTo: filterButton.leadingAnchor, constant: -5),
            searchDivider.centerYAnchor.constraint(equalTo: searchContainer.centerYAnchor),
            searchDivider.widthAnchor.constraint(equalToConstant: 1),
            searchDivider.heightAnchor.constraint(equalToConstant: 14),

            filterButton.trailingAnchor.constraint(equalTo: searchContainer.trailingAnchor, constant: -4),
            filterButton.centerYAnchor.constraint(equalTo: searchContainer.centerYAnchor),
            filterButton.widthAnchor.constraint(equalToConstant: 38),
            filterButton.heightAnchor.constraint(equalToConstant: 22),
        ])

        filterPopoverController.configure(
            searchField: searchField,
            onSearch: { [weak self] query in
                guard let self = self else { return }
                self.currentSort = .relevance
                self.lastQuery = query
                if query.isEmpty {
                    self.searchResultsViewController.updateBottomPath(for: nil)
                    self.loadDefaultResults()
                } else {
                    self.performSearch(query: query)
                }
            },
            onFilterStateChanged: { [weak self] state in
                self?.updateFilterButton(state: state)
            }
        )
        updateFilterButtonState(forQuery: searchField.stringValue)

        let toolbar = LokiiToolbar(identifier: "LokiiMainToolbar")
        toolbar.delegate = self
        toolbar.displayMode = .iconOnly
        toolbar.allowsUserCustomization = false
        if #available(macOS 15.0, *) {
            toolbar.allowsDisplayModeCustomization = false
        }
        window?.toolbar = toolbar
    }

    // MARK: - Recent Searches Menu (on the magnifying glass)

    private let recentSearchesKey = "LokiiRecentSearches"
    private var recentSearches: [String] {
        get { Array((UserDefaults.standard.stringArray(forKey: recentSearchesKey) ?? []).prefix(Self.maxRecentSearches)) }
        set { UserDefaults.standard.set(Array(newValue.prefix(Self.maxRecentSearches)), forKey: recentSearchesKey) }
    }

    /// 针对菜单展示层对超长历史检索词执行中间省略（保留首尾关键字符），
    /// 防止深层目录路径将下拉菜单横向撑爆，完整检索词仍保存在 `representedObject` 与 `toolTip` 中。
    static func truncatedHistoryDisplayTitle(_ query: String) -> String {
        let maxLength = maxHistoryDisplayLength
        guard query.count > maxLength else { return query }
        let headCount = (maxLength - 1) / 2
        let tailCount = maxLength - 1 - headCount
        let head = query.prefix(headCount)
        let tail = query.suffix(tailCount)
        return "\(head)…\(tail)"
    }

    /// 归档当前检索词：
    /// 1. 要求存在实际命中结果且首尾裁剪后长度 >= 2，拦截零命中错别字与单字符碎片；
    /// 2. 前缀演进合并：仅移除与新词相同或为其更短前缀的旧记录（忽略大小写，如先搜 "v2ray" 后搜 "v2rayn"），
    ///    保护已归档的更长完整检索词（如已存在 "lokii_promo_optimized.mp4" 时再搜 "lokii" 不会误删前者）；
    /// 3. 队列未变化时跳过写盘与菜单重建，避免连续消费动作重复开销。
    private func recordCurrentSearchQueryIfNeeded() {
        let trimmed = searchField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !splitViewController.rawResults.isEmpty, trimmed.count >= 2 else { return }
        let lowerTrimmed = trimmed.lowercased()
        let current = recentSearches
        let updated = Array(([trimmed] + current.filter { !lowerTrimmed.hasPrefix($0.lowercased()) }).prefix(Self.maxRecentSearches))
        guard updated != current else { return }
        recentSearches = updated
        setupRecentSearchesMenu()
    }

    @objc private func recentSearchSelected(_ sender: NSMenuItem) {
        guard let query = sender.representedObject as? String else { return }
        currentSort = .relevance
        lastQuery = query
        searchField.stringValue = query
        applyQuerySyntaxHighlighting()
        updateCaseSensitivityButtonState()
        updateRegexModeButtonState()
        focusSearchField()
        performSearch(query: query)
    }

    @objc private func removeRecentSearchItem(_ sender: NSMenuItem) {
        guard let query = sender.representedObject as? String else { return }
        recentSearches = recentSearches.filter { $0.caseInsensitiveCompare(query) != .orderedSame }
        setupRecentSearchesMenu()
    }

    @objc private func clearRecentSearches() {
        recentSearches = []
        setupRecentSearchesMenu()
    }

    private func setupRecentSearchesMenu() {
        let historyTitle = L("search.history.title")
        let menu = NSMenu(title: historyTitle)

        let headerItem = NSMenuItem(title: historyTitle, action: nil, keyEquivalent: "")
        headerItem.isEnabled = false
        menu.addItem(headerItem)

        let history = recentSearches
        if history.isEmpty {
            let emptyItem = NSMenuItem(title: L("search.history.empty"), action: nil, keyEquivalent: "")
            emptyItem.isEnabled = false
            menu.addItem(emptyItem)
        } else {
            let iconConfig = NSImage.SymbolConfiguration(pointSize: 12, weight: .regular)
            let clockIcon = NSImage(systemSymbolName: "clock", accessibilityDescription: nil)?.withSymbolConfiguration(iconConfig)
            clockIcon?.isTemplate = true
            let trashIcon = NSImage(systemSymbolName: "trash", accessibilityDescription: nil)?.withSymbolConfiguration(iconConfig)
            trashIcon?.isTemplate = true

            for query in history {
                let displayTitle = Self.truncatedHistoryDisplayTitle(query)
                let item = NSMenuItem(title: displayTitle, action: #selector(recentSearchSelected(_:)), keyEquivalent: "")
                item.target = self
                item.representedObject = query
                item.toolTip = query
                item.keyEquivalentModifierMask = []
                item.image = clockIcon
                menu.addItem(item)

                // 按住 Option (⌥) 键时动态切换为单条历史移除项
                let removeTitle = String(format: L("search.history.removeItem"), displayTitle)
                let alternateItem = NSMenuItem(title: removeTitle, action: #selector(removeRecentSearchItem(_:)), keyEquivalent: "")
                alternateItem.target = self
                alternateItem.representedObject = query
                alternateItem.toolTip = query
                alternateItem.keyEquivalentModifierMask = [.option]
                alternateItem.isAlternate = true
                alternateItem.image = trashIcon
                menu.addItem(alternateItem)
            }

            menu.addItem(NSMenuItem.separator())
            let clearItem = NSMenuItem(title: L("search.history.clear"), action: #selector(clearRecentSearches), keyEquivalent: "")
            clearItem.target = self
            menu.addItem(clearItem)
        }

        if let searchCell = searchField?.cell as? NSSearchFieldCell {
            searchCell.searchMenuTemplate = menu
            if let searchButtonCell = searchCell.searchButtonCell {
                let config = NSImage.SymbolConfiguration(pointSize: 14, weight: .regular)
                if let image = NSImage(systemSymbolName: "magnifyingglass", accessibilityDescription: historyTitle)?.withSymbolConfiguration(config) {
                    image.isTemplate = true
                    searchButtonCell.image = image
                }
            }
        }
        searchField?.toolTip = historyTitle
        searchField?.needsDisplay = true
        searchResultsViewController.updateRecentSearches(history)
    }

    // MARK: - Notifications

    private func setupNotifications() {
        let nc = NotificationCenter.default
        nc.addObserver(self, selector: #selector(onLanguageChanged),
                       name: L10n.languageChangedNotification, object: nil)
        nc.addObserver(self, selector: #selector(onIndexProgress(_:)),
                       name: SearchEngine.indexProgressNotification, object: nil)
        nc.addObserver(self, selector: #selector(onIndexReady(_:)),
                       name: SearchEngine.indexReadyNotification, object: nil)
        nc.addObserver(self, selector: #selector(onIndexUpdated(_:)),
                       name: SearchEngine.indexUpdatedNotification, object: nil)
        nc.addObserver(self, selector: #selector(onWatcherError(_:)),
                       name: SearchEngine.watcherErrorNotification, object: nil)
        nc.addObserver(self, selector: #selector(onStartupMode(_:)),
                       name: SearchEngine.startupModeNotification, object: nil)
        nc.addObserver(self, selector: #selector(onInitializationFailed(_:)),
                       name: SearchEngine.initializationFailedNotification, object: nil)
        nc.addObserver(self, selector: #selector(onConfigReloaded(_:)),
                       name: SearchEngine.configReloadedNotification, object: nil)
        nc.addObserver(self, selector: #selector(onVolumeStatusesChanged),
                       name: SearchEngine.volumeStatusesChangedNotification, object: nil)
        nc.addObserver(self, selector: #selector(onAppDidBecomeActive),
                       name: NSApplication.didBecomeActiveNotification, object: nil)
        nc.addObserver(self, selector: #selector(resetToDefaultWindowFrame),
                       name: .lokiiResetWindowSize, object: nil)
    }

    @objc private func onAppDidBecomeActive() {
        let hasFDA = engine.hasFullDiskAccess
        searchResultsViewController.updateFDAStatus(hasFDA: hasFDA)
        guard let state = overlayState else { return }
        state.syncDismissedState()
        if hasFDA {
            state.markFDAGranted()
        }
    }

    @objc private func onLanguageChanged() {
        searchField.placeholderString = L(isInlineProgressActive ? "search.indexingIncomplete" : "search.placeholder")
        if let toolbar = window?.toolbar {
            for item in toolbar.items {
                if item.itemIdentifier == .toggleSidebar {
                    item.label = L("search.toggleSidebar")
                    item.paletteLabel = L("search.toggleSidebar")
                    item.toolTip = L("search.toggleSidebar")
                } else if item.itemIdentifier == .searchField {
                    item.label = L("search.placeholder")
                    item.paletteLabel = L("search.placeholder")
                } else if item.itemIdentifier == .pinWindow {
                    item.label = L("search.toolbar.pin")
                    item.paletteLabel = L("search.toolbar.pin")
                    item.toolTip = L("search.toolbar.pin")
                    updatePinButtonState()
                } else if item.itemIdentifier == .resetWindowSize {
                    item.label = L("window.viewOptions")
                    item.paletteLabel = L("window.viewOptions")
                    item.toolTip = L("window.viewOptions")
                    if let button = item.view as? NSButton {
                        button.toolTip = L("window.viewOptions")
                        button.menu = buildViewOptionsMenu()
                    }
                }
            }
        }
        splitViewController.onLanguageChanged()
        setupRecentSearchesMenu()
        updateStatusLabel()
        updateFilterButtonState(forQuery: searchField.stringValue)
        caseSensitivityButton.updateTooltipAndAccessibility()
        fuzzyModeButton.updateTooltipAndAccessibility()
        regexModeButton.updateTooltipAndAccessibility()
        overlayState?.headingText = L("search.buildingIndex")
        overlayState?.statusText = L("search.scanningFiles")
    }

    private func updateStatusLabel() {
        let formatted: String
        if let match = searchMatchText, !match.string.isEmpty {
            let template = L("search.statusWithMatches")
            formatted = template == "search.statusWithMatches"
                ? "\(match.string) | \(statusText.string)"
                : String(format: template, match.string, statusText.string)
        } else {
            formatted = statusText.string
        }
        searchResultsViewController.updateStatusText(formatted)
    }

    @objc private func onIndexProgress(_ note: Notification) {
        guard indexingState != .failed else { return }
        let phase = note.userInfo?["phase"] as? String ?? ""
        let source = note.userInfo?["source"] as? String ?? ""
        // 偏好设置页或外部入口触发手动重建时，同步将主视窗切入 rebuilding 态以保持双端进度与完成刷表一致
        if indexingState == .idle && source == "rebuild" {
            indexingState = .rebuilding
            searchResultsViewController.setRebuildEnabled(false)
            searchResultsViewController.setRebuildSpinning(true)
            showInlineProgress()
        }
        guard phase != "verifying", indexingState != .idle else { return }
        if indexingState == .initializing && !isColdStart { return }
        if indexingState == .rebuilding && source != "rebuild" { return }
        if indexingState == .initializing && source == "rebuild" { return }

        let count = note.userInfo?["scannedFiles"] as? UInt64 ?? 0
        let formatted = formatFileCount(count)

        statusText = .formatted("search.indexingCount", formatted)
        // 冷启动期间额外把扫描计数同步到全屏建库遮罩卡片
        if isColdStart && indexingState == .initializing {
            overlayState?.scannedCount = count
        }
    }

    @objc private func onIndexReady(_ note: Notification) {
        let total = note.userInfo?["total"] as? UInt64 ?? 0
        let source = note.userInfo?["source"] as? String ?? ""

        guard indexingState != .failed else { return }
        statusText = .formatted("search.indexedCount", formatFileCount(total))

        // 完成事件必须与当前所处阶段同源：并发触发的 initialize/rebuild 事件
        // 到达时仅刷新计数，不得据此结束当前阶段；但若外部触发的快速重建在进度轮询前即已完成，仍需刷新当前视图结果
        switch indexingState {
        case .idle:
            if source == "rebuild" {
                searchResultsViewController.updateFDAStatus(hasFDA: engine.hasFullDiskAccess)
                let currentText = searchField.stringValue
                if currentText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    loadDefaultResults()
                } else {
                    performSearch(query: currentText)
                }
            }
            return
        case .initializing where source == "rebuild":
            return
        case .rebuilding where source != "rebuild":
            return
        default:
            break
        }

        // 冷启动以全窗遮罩呈现进度，热启动与重建以内联指示器呈现，故收起方式不同
        if indexingState == .initializing && isColdStart {
            hideLoadingOverlay()
        } else {
            hideInlineProgress()
        }
        searchResultsViewController.setRebuildSpinning(false)
        searchResultsViewController.setRebuildEnabled(true)
        searchResultsViewController.updateFDAStatus(hasFDA: engine.hasFullDiskAccess)
        indexingState = .idle
        let currentText = searchField.stringValue
        if currentText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            loadDefaultResults()
        } else {
            performSearch(query: currentText)
        }
    }

    @objc private func onIndexUpdated(_ note: Notification) {
        guard indexingState != .failed else { return }
        let total = note.userInfo?["total"] as? UInt64 ?? 0
        statusText = .formatted("search.indexedCount", formatFileCount(total))
        overlayState?.scannedCount = total
    }

    @objc private func onWatcherError(_ note: Notification) {
        guard indexingState == .rebuilding else { return }
        let source = note.userInfo?["source"] as? String ?? ""
        guard source == "rebuild" else { return }
        hideInlineProgress()
        searchResultsViewController.setRebuildSpinning(false)
        searchResultsViewController.setRebuildEnabled(true)
        indexingState = .idle
        statusText = .formatted("search.indexedCount", formatFileCount(engine.stats.totalCount))
    }

    @objc private func onStartupMode(_ note: Notification) {
        guard indexingState != .failed else { return }
        let coldStart = note.userInfo?["isColdStart"] as? Bool ?? true
        isColdStart = coldStart
        if coldStart {
            // Cold start — show overlay
            showLoadingOverlay(heading: L("search.buildingIndex"))
        } else {
            statusText = .key("search.loadingIndex")
        }
    }

    @objc private func onInitializationFailed(_ note: Notification) {
        indexingState = .failed
        searchWorkItem?.cancel()
        loadingOverlay?.removeFromSuperview()
        loadingOverlay = nil
        overlayState = nil
        hideInlineProgress()
        searchField.isEnabled = false
        searchResultsViewController.setRebuildEnabled(false)
        searchResultsViewController.setRebuildSpinning(false)
        statusText = .key("search.initializationFailed")

        let alert = NSAlert()
        alert.alertStyle = .critical
        alert.messageText = L("search.initializationFailed")
        alert.informativeText = L("search.initializationFailedHelp") + "\n\n"
            + (note.userInfo?["message"] as? String ?? "")
        alert.addButton(withTitle: L("ok"))
        if let window {
            alert.beginSheetModal(for: window)
        }
    }

    @objc private func onConfigReloaded(_ note: Notification) {
        reloadWindowBehavior()
        applyWindowBehaviorFromConfig()
        let currentQuery = searchField.stringValue
        if currentQuery.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            loadDefaultResults()
        } else {
            performSearch(query: currentQuery)
        }
    }

    // MARK: - Window Behavior

    private func applyProgrammaticFrame(_ frame: NSRect, display: Bool, animate: Bool = false) {
        guard let window else { return }
        isApplyingProgrammaticFrame = true
        defer { isApplyingProgrammaticFrame = false }
        let shouldAnimate = animate && !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
        window.setFrame(frame, display: display, animate: shouldAnimate)
    }

    private func applyInitialWindowFrame() {
        guard window != nil else { return }
        let primaryScreen = NSScreen.main ?? NSScreen.screens.first
        let primaryMetrics = primaryScreen.map { ScreenMetrics(screen: $0) }
        let allMetrics = NSScreen.screens.map { ScreenMetrics(screen: $0) }
        frameManager.migrateLegacyFrameIfNeeded(
            allScreens: allMetrics,
            currentScreen: primaryMetrics,
            rememberPosition: shouldRememberWindowPosition
        )
        guard let primaryMetrics else {
            applyProgrammaticFrame(defaultWindowFrame, display: false, animate: false)
            return
        }
        let initial = frameManager.initialFrame(
            on: primaryMetrics,
            allScreens: allMetrics,
            rememberPosition: shouldRememberWindowPosition
        )
        applyProgrammaticFrame(initial, display: false, animate: false)
    }

    private func applyWindowBehaviorFromConfig() {
        guard let window else { return }
        if shouldRememberWindowPosition {
            if frameManager.savedOrigin() == nil {
                frameManager.saveOrigin(window.frame.origin)
            }
        } else {
            frameManager.clearSavedOrigin()
        }
    }

    /// 窗口位置记忆开关与空搜索隐藏列表开关的本地缓存，随配置重载刷新。
    /// 拖动与缩放回调按显示刷新率触发，若每次实时跨 FFI 解码整份配置将产生大量冗余开销。
    private var shouldRememberWindowPosition = false
    private var hideResultsWhenEmpty = true

    private var shouldShowIdleEmptyState: Bool {
        // 以搜索框权威文本反序列化得到的过滤状态为准，避免过滤浮层收起后其内部残留状态永久阻塞空态
        let queryFilterState = FilterQuerySyncCoordinator.deserialize(query: searchField.stringValue).state
        let isFilterUnconstrained = queryFilterState.isUnconstrained
            && (!filterPopoverController.isShown || filterPopoverController.currentState.isUnconstrained)
        return SearchResultsViewController.shouldShowIdleEmptyState(
            hideWhenEmpty: hideResultsWhenEmpty,
            query: searchField.stringValue,
            isFilterUnconstrained: isFilterUnconstrained,
            hasActiveDirectory: splitViewController.sidebarTreeViewController.selectedPath != nil,
            isIndexingIdle: indexingState == .idle
        )
    }

    private func reloadWindowBehavior() {
        let cfg = engine.getConfig()
        shouldRememberWindowPosition = cfg?.window.rememberPosition ?? false
        hideResultsWhenEmpty = cfg?.general.hideResultsWhenEmpty ?? true
    }

    private func saveWindowSizeIfNeeded() {
        guard !isApplyingProgrammaticFrame, let window, window.isVisible, let screen = window.screen else { return }
        frameManager.saveSize(window.frame.size, for: ScreenMetrics(screen: screen))
        NotificationCenter.default.post(name: .lokiiWindowSizesDidChange, object: nil)
    }

    private func saveWindowOriginIfNeeded() {
        guard !isApplyingProgrammaticFrame, shouldRememberWindowPosition, let window, window.isVisible else { return }
        frameManager.saveOrigin(window.frame.origin)
    }

    private static func isSystemLeftMouseButtonDown() -> Bool {
        CGEventSource.buttonState(.combinedSessionState, button: .left)
    }

    private func cancelPendingScreenOverflowCheck() {
        pendingScreenOverflowCheck = false
        screenOverflowDebounceWorkItem?.cancel()
        screenOverflowDebounceWorkItem = nil
        screenOverflowButtonPollTimer?.invalidate()
        screenOverflowButtonPollTimer = nil
    }

    private func scheduleScreenOverflowCheck() {
        guard !isApplyingProgrammaticFrame, let window, window.isVisible else { return }
        pendingScreenOverflowCheck = true
        resetScreenOverflowDebounce()
    }

    private func resetScreenOverflowDebounce() {
        screenOverflowDebounceWorkItem?.cancel()
        screenOverflowButtonPollTimer?.invalidate()
        screenOverflowButtonPollTimer = nil

        let workItem = DispatchWorkItem { [weak self] in
            self?.handleScreenOverflowDebounceFired()
        }
        screenOverflowDebounceWorkItem = workItem
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.screenOverflowSettleDelay, execute: workItem)
    }

    private func handleScreenOverflowDebounceFired() {
        guard pendingScreenOverflowCheck else { return }
        if WindowFrameManager.shouldDeferOverflowClamp(isLeftMouseButtonDown: Self.isSystemLeftMouseButtonDown()) {
            // 用户在跨屏拖拽中途停顿但手指仍未抬起（含触控板三指拖移），轮询底层系统事件源直到真实松手
            startPollingForLeftButtonRelease()
            return
        }
        cancelPendingScreenOverflowCheck()
        performScreenOverflowClampIfNeeded()
    }

    private func startPollingForLeftButtonRelease() {
        guard screenOverflowButtonPollTimer == nil else { return }
        let timer = Timer(timeInterval: 0.05, repeats: true) { [weak self] _ in
            guard let self else { return }
            guard self.pendingScreenOverflowCheck else {
                self.cancelPendingScreenOverflowCheck()
                return
            }
            if !WindowFrameManager.shouldDeferOverflowClamp(isLeftMouseButtonDown: Self.isSystemLeftMouseButtonDown()) {
                self.cancelPendingScreenOverflowCheck()
                self.performScreenOverflowClampIfNeeded()
            }
        }
        RunLoop.main.add(timer, forMode: .common)
        screenOverflowButtonPollTimer = timer
    }

    private func performScreenOverflowClampIfNeeded() {
        guard !isApplyingProgrammaticFrame,
              !WindowFrameManager.shouldDeferOverflowClamp(isLeftMouseButtonDown: Self.isSystemLeftMouseButtonDown()),
              let window = self.window,
              window.isVisible,
              !window.inLiveResize,
              !window.isZoomed,
              !window.styleMask.contains(.fullScreen),
              let screen = window.screen else { return }
        guard let clamped = frameManager.clampedFrameForScreenOverflow(
            currentFrame: window.frame,
            newScreen: ScreenMetrics(screen: screen),
            anchorX: NSEvent.mouseLocation.x
        ) else { return }
        applyProgrammaticFrame(clamped, display: true, animate: true)
        if shouldRememberWindowPosition {
            frameManager.saveOrigin(clamped.origin)
        }
    }

    // MARK: - Loading Overlay

    private func showLoadingOverlay(heading: String) {
        searchField.isEnabled = false
        guard loadingOverlay == nil else {
            overlayState?.headingText = heading
            return
        }
        overlayShownAt = Date()
        guard let contentView = window?.contentView else { return }

        let state = IndexingOverlayState(
            hasFDA: engine.hasFullDiskAccess,
            promptDismissed: FDAGuideWindowController.isPromptDismissed,
            heading: heading
        )
        self.overlayState = state

        // Full-window overlay container with selective hit-testing
        let overlay = OverlayContainerView(frame: contentView.bounds)
        overlay.autoresizingMask = [.width, .height]
        overlay.wantsLayer = true
        overlay.alphaValue = 0
        contentView.addSubview(overlay)
        loadingOverlay = overlay

        // Fullscreen indexing overlay with center animation & bottom-trailing pill
        let cardView = IndexingFullScreenOverlayView(state: state)
        let hostingView = NSHostingView(rootView: cardView)
        hostingView.translatesAutoresizingMaskIntoConstraints = false
        hostingView.wantsLayer = true
        overlay.addSubview(hostingView)
        overlay.interactiveCardView = hostingView

        NSLayoutConstraint.activate([
            hostingView.leadingAnchor.constraint(equalTo: overlay.leadingAnchor),
            hostingView.trailingAnchor.constraint(equalTo: overlay.trailingAnchor),
            hostingView.topAnchor.constraint(equalTo: overlay.topAnchor),
            hostingView.bottomAnchor.constraint(equalTo: overlay.bottomAnchor),
        ])

        // Fade in
        NSAnimationContext.runAnimationGroup { ctx in
            ctx.duration = 0.3
            ctx.timingFunction = CAMediaTimingFunction(name: .easeOut)
            overlay.animator().alphaValue = 1.0
        }

        NSAccessibility.post(element: overlay, notification: .layoutChanged)
    }

    private func hideLoadingOverlay() {
        guard let overlay = loadingOverlay else { return }

        // Enforce minimum display time so overlay doesn't flash and disappear
        let minDisplayTime: TimeInterval = 1.0
        let elapsed = Date().timeIntervalSince(overlayShownAt ?? .distantPast)
        if elapsed < minDisplayTime {
            DispatchQueue.main.asyncAfter(deadline: .now() + (minDisplayTime - elapsed)) { [weak self] in
                self?.hideLoadingOverlay()
            }
            return
        }

        searchField.isEnabled = true

        // Accessibility: announce index ready
        if let mainWindow = NSApp.mainWindow {
            NSAccessibility.post(element: mainWindow as Any, notification: .announcementRequested,
                                 userInfo: [NSAccessibility.NotificationUserInfoKey.announcement: L("search.indexReady")])
        }
        NSAnimationContext.runAnimationGroup({ ctx in
            ctx.duration = 0.3
            ctx.timingFunction = CAMediaTimingFunction(name: .easeIn)
            overlay.animator().alphaValue = 0.0
        }, completionHandler: { [weak self] in
            overlay.removeFromSuperview()
            self?.loadingOverlay = nil
            self?.overlayState = nil
            // 遮罩已彻底收起，此时再弹出权限引导，避免引导浮窗被建库遮罩覆盖
            (NSApp.delegate as? AppDelegate)?.checkAndPromptFDA()
        })
    }

    // MARK: - Inline Progress (Hot Start / Rebuild)

    private func showInlineProgress() {
        isInlineProgressActive = true
        searchResultsViewController.setInlineProgressVisible(true)
        searchField.placeholderString = L("search.indexingIncomplete")
    }

    private func hideInlineProgress() {
        isInlineProgressActive = false
        searchResultsViewController.setInlineProgressVisible(false)
        searchField.placeholderString = originalSearchPlaceholder
    }

    // MARK: - Rebuild Action

    var canTriggerRebuild: Bool {
        indexingState == .idle && !engine.isScanning
    }

    func triggerRebuildIndex() {
        rebuildIndexClicked()
    }

    @objc private func rebuildIndexClicked() {
        guard indexingState == .idle else { return }

        if engine.isScanning {
            searchResultsViewController.notifyBackgroundIndexingActive()
            return
        }

        ManualRebuildCoordinator.requestRebuild(
            window: window,
            statuses: engine.getVolumeStatuses(),
            onPrimaryOnly: { [weak self] in
                self?.performManualRebuild(includeExternal: false)
            },
            onRebuildAll: { [weak self] in
                self?.performManualRebuild(includeExternal: true)
            }
        )
    }

    private func performManualRebuild(includeExternal: Bool) {
        guard indexingState == .idle else { return }
        if engine.isScanning {
            searchResultsViewController.notifyBackgroundIndexingActive()
            return
        }

        indexingState = .rebuilding
        searchResultsViewController.setRebuildEnabled(false)
        searchResultsViewController.setRebuildSpinning(true)
        showInlineProgress()
        statusText = .key("search.indexing")
        if includeExternal {
            engine.rebuildIndex()
        } else {
            engine.rebuildPrimaryIndex()
        }
    }

    // MARK: - Search

    private var lastQuery: String = ""

    @objc private func searchChanged() {
        guard indexingState != .failed else { return }

        // 优先检查并释放因输入法组字而暂存的过滤变更，避免后续陈旧短词覆盖已应用过滤的即时检索
        if filterPopoverController.flushPendingStateIfNeeded() {
            return
        }

        let query = searchField.stringValue
        updateFilterButtonState(forQuery: query)
        if filterPopoverController.isShown {
            filterPopoverController.syncStateFromQuery(query)
        }
        guard query != lastQuery else { return }
        lastQuery = query
        currentSort = .relevance
        searchWorkItem?.cancel()

        if query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            filterPopoverController.syncStateFromQuery("")
            lastExclusionHint = nil
            searchResultsViewController.updateBottomPath(for: nil)
            updateCaseSensitivityButtonState()
            loadDefaultResults()
            return
        }

        let item = DispatchWorkItem { [weak self] in
            self?.performSearch(query: query)
        }
        searchWorkItem = item
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.016, execute: item)
    }

    /// 递增检索代际令牌，用于淘汰陈旧后台检索并杜绝过期结果在主线程触发冗余刷表
    private let searchGenerationLock = NSLock()
    private var latestSearchGeneration: UInt64 = 0
    private var lastSearchError: String? = nil

    private func nextSearchGeneration() -> UInt64 {
        searchGenerationLock.lock()
        latestSearchGeneration &+= 1
        let gen = latestSearchGeneration
        searchGenerationLock.unlock()
        return gen
    }

    private func isCurrentSearchGeneration(_ generation: UInt64) -> Bool {
        searchGenerationLock.lock()
        let isCurrent = (latestSearchGeneration == generation)
        searchGenerationLock.unlock()
        return isCurrent
    }

    private func updateMatchCountText(_ count: Int) {
        if count == 1 {
            searchMatchText = .key("search.matchedCount.one")
        } else {
            searchMatchText = .formatted("search.matchedCount", formatFileCount(UInt64(count)))
        }
    }

    /// 点击空搜索语法速查卡片后，将示例填入搜索框并将光标定位到末尾，立即触发一次检索
    private func applySyntaxExample(_ example: String) {
        searchField.stringValue = example
        window?.makeFirstResponder(searchField)
        if let editor = searchField.currentEditor() {
            let end = (example as NSString).length
            editor.selectedRange = NSRange(location: end, length: 0)
        }
        applyQuerySyntaxHighlighting()
        updateCaseSensitivityButtonState()
        updateRegexModeButtonState()
        searchChanged()
    }

    /// 搜索框为空时自动呈现全量索引文件与目录树（按修改时间倒序，时间相同时按文件名自然字母顺序排列）；
    /// 当用户开启「未输入时隐藏结果列表」且无过滤约束时，跳过通配查询并展示轻量语法速查空态。
    private func loadDefaultResults() {
        guard indexingState != .failed else { return }
        if shouldShowIdleEmptyState {
            guard !isApplyingIdleEmptyState else { return }
            isApplyingIdleEmptyState = true
            defer { isApplyingIdleEmptyState = false }
            _ = nextSearchGeneration()
            lastExclusionHint = nil
            lastSearchError = nil
            searchMatchText = nil
            // 必须在派发空结果前先将空态置为 .idleEmpty，防止同步回调期间因状态未切换而重入
            searchResultsViewController.emptyState = .idleEmpty
            splitViewController.updateSearchResults(rawResults: [], tree: nil, deferTreeBuild: false, autoSelectFirstRow: false)
            searchResultsViewController.updateBottomPath(for: nil)
            return
        }
        let generation = nextSearchGeneration()
        let shouldBuildTree = splitViewController.needsDirectoryTree
        let sort = currentSort

        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            guard let self else { return }
            let response = self.engine.search(query: "*", mode: .wildcard, sort: sort)
            guard self.isCurrentSearchGeneration(generation) else { return }

            let sorted: [FfiSearchResult]
            if sort == .relevance {
                // 默认未检索状态下按修改时间倒序排列（最近活跃优先），时间相同时按文件名自然字母顺序排列
                sorted = response.results.sorted {
                    if $0.modified != $1.modified {
                        return $0.modified > $1.modified
                    }
                    return $0.name.localizedStandardCompare($1.name) == .orderedAscending
                }
            } else {
                sorted = response.results
            }
            guard self.isCurrentSearchGeneration(generation) else { return }

            // 循环外一次性预归一化卷状态快照；无外部卷且后端已返回空高亮区间时零拷贝直通
            let volumeStatuses = self.engine.getVolumeStatuses()
            let items: [FfiSearchResult]
            if volumeStatuses.isEmpty && sorted.allSatisfy({ $0.matchPositions.isEmpty && $0.parentMatchPositions.isEmpty }) {
                items = sorted
            } else {
                let matcher = SettingsPathStatusResolver.VolumeStatusMatcher(volumeStatuses: volumeStatuses)
                items = sorted.map { item in
                    let matchedStatus = matcher.isEmpty ? nil : matcher.match(path: item.path)
                    let offline = item.isOffline || (matchedStatus.map { $0.state == .offline || $0.state == .missing } ?? false)
                    let resolvedVolumeName = item.volumeName ?? matchedStatus?.displayName
                    return FfiSearchResult(
                        name: item.name,
                        path: item.path,
                        isDir: item.isDir,
                        size: item.size,
                        modified: item.modified,
                        created: item.created,
                        extension: item.extension,
                        score: item.score,
                        matchPositions: [],
                        parentMatchPositions: [],
                        searchMode: item.searchMode,
                        isOffline: offline,
                        volumeName: resolvedVolumeName
                    )
                }
            }
            guard self.isCurrentSearchGeneration(generation) else { return }

            let tree: ResultDirectoryTree? = (shouldBuildTree && !items.isEmpty)
                ? ResultDirectoryTree.build(from: items)
                : nil
            guard self.isCurrentSearchGeneration(generation) else { return }

            DispatchQueue.main.async {
                guard self.indexingState != .failed, self.isCurrentSearchGeneration(generation) else { return }
                self.updateFilterButtonState(forQuery: self.searchField.stringValue)
                self.lastExclusionHint = nil
                self.lastSearchError = nil
                self.updateEmptyState()
                self.splitViewController.updateSearchResults(
                    rawResults: items,
                    tree: tree,
                    deferTreeBuild: !shouldBuildTree,
                    autoSelectFirstRow: false,
                    preserveSortDescriptors: sort != .relevance,
                    skipLocalSort: sort != .relevance
                )
                if self.sidebarTreeViewController.selectedPath == nil {
                    self.searchMatchText = nil
                }
                self.updateStatusLabel()
            }
        }
    }

    private func performSearch(query: String, autoSelectFirstRow: Bool = true) {
        guard indexingState != .failed else { return }
        let generation = nextSearchGeneration()
        let mode = currentMode
        let caseSensitivity = currentCaseSensitivity
        let sort = currentSort
        let shouldBuildTree = splitViewController.needsDirectoryTree

        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            guard let self else { return }
            let response = self.engine.search(query: query, mode: mode, caseSensitivity: caseSensitivity, sort: sort)
            guard self.isCurrentSearchGeneration(generation) else { return }

            let tree: ResultDirectoryTree? = (shouldBuildTree && !response.results.isEmpty)
                ? ResultDirectoryTree.build(from: response.results)
                : nil
            guard self.isCurrentSearchGeneration(generation) else { return }

            DispatchQueue.main.async {
                guard self.indexingState != .failed, self.isCurrentSearchGeneration(generation) else { return }
                self.lastExclusionHint = response.hint
                self.lastSearchError = response.error
                self.updateEmptyState()
                self.splitViewController.updateSearchResults(
                    rawResults: response.results,
                    tree: tree,
                    deferTreeBuild: !shouldBuildTree,
                    autoSelectFirstRow: autoSelectFirstRow,
                    preserveSortDescriptors: sort != .relevance,
                    skipLocalSort: sort != .relevance
                )
                self.updateFilterButtonState(forQuery: self.searchField.stringValue)
                self.updateCaseSensitivityButtonState(responseCaseSensitive: response.isCaseSensitive)
                self.updateRegexModeButtonState(effectiveMode: response.effectiveMode)

                if let error = response.error {
                    self.searchMatchText = .verbatim(error)
                } else {
                    self.updateMatchCountText(self.searchResultsViewController.results.count)
                }
            }
        }
    }

    // MARK: - Actions

    private var selectedResults: [FfiSearchResult] {
        searchResultsViewController.selectedResults
    }

    @objc private func openSelected() {
        recordCurrentSearchQueryIfNeeded()
        let selected = selectedResults
        guard !selected.isEmpty else { return }
        if selected.contains(where: { $0.isOffline || engine.isPathOffline($0.path) }) {
            searchResultsViewController.showBlockedActionFeedback()
            return
        }
        for item in selected {
            openResultWithFeedback(item)
        }
    }

    private func openResultWithFeedback(_ item: FfiSearchResult) {
        if item.isOffline || engine.isPathOffline(item.path) {
            searchResultsViewController.showBlockedActionFeedback()
            return
        }
        engine.openFileAsync(path: item.path) { [weak self] outcome in
            guard let self, !outcome.succeeded else { return }
            let message = outcome.errorMessage ?? L("search.offline.actionBlocked")
            self.searchResultsViewController.showBlockedActionFeedback(message)
            if let window = self.window {
                let alert = NSAlert()
                alert.alertStyle = .warning
                alert.messageText = L("search.offline.openFailedTitle")
                alert.informativeText = message
                alert.beginSheetModal(for: window, completionHandler: nil)
            }
        }
    }

    @objc private func onVolumeStatusesChanged() {
        searchResultsViewController.refreshOfflineStates()
        guard indexingState != .failed, engine.stats.isReady else { return }
        let currentText = searchField.stringValue
        if currentText.isEmpty {
            loadDefaultResults()
        } else {
            performSearch(query: currentText)
        }
    }

    // MARK: - Case Sensitivity Actions & State

    @objc private func caseSensitivityButtonClicked(_ sender: NSButton) {
        cycleCaseSensitivity()
    }

    private func cycleCaseSensitivity() {
        switch currentCaseSensitivity {
        case .auto:
            currentCaseSensitivity = .sensitive
        case .sensitive:
            currentCaseSensitivity = .insensitive
        case .insensitive:
            currentCaseSensitivity = .auto
        }
        updateCaseSensitivityButtonState()
        if let query = searchField?.stringValue, !query.isEmpty {
            performSearch(query: query)
        }
    }

    private func updateCaseSensitivityButtonState(responseCaseSensitive: Bool? = nil) {
        switch currentCaseSensitivity {
        case .sensitive:
            caseSensitivityButton.visualState = .locked
        case .insensitive:
            caseSensitivityButton.visualState = .inactiveSlashed
        case .auto:
            if let effective = responseCaseSensitive {
                caseSensitivityButton.visualState = effective ? .autoActive : .inactive
            } else {
                let query = searchField?.stringValue ?? ""
                let hasUpper = Self.hasEffectiveUppercase(query: query, mode: currentMode)
                caseSensitivityButton.visualState = hasUpper ? .autoActive : .inactive
            }
        }
    }

    static func hasEffectiveUppercase(query: String, mode: FfiSearchMode = .auto) -> Bool {
        previewQueryFlags(query: query, mode: mode).hasEffectiveUppercase
    }

    // MARK: - Fuzzy & Regex Mode Actions & State

    @objc private func fuzzyModeButtonClicked(_ sender: NSButton) {
        toggleMode(.fuzzy)
    }

    @objc private func regexModeButtonClicked(_ sender: NSButton) {
        toggleMode(.regex)
    }

    private func toggleMode(_ mode: FfiSearchMode) {
        currentMode = (currentMode == mode) ? .auto : mode
        updateFuzzyModeButtonState()
        updateRegexModeButtonState()
        searchResultsViewController.updateEmptyContext(
            query: searchField?.stringValue ?? "",
            isFuzzyActive: currentMode == .fuzzy
        )
        if let query = searchField?.stringValue, !query.isEmpty {
            performSearch(query: query)
        }
    }

    private func updateFuzzyModeButtonState() {
        fuzzyModeButton.visualState = (currentMode == .fuzzy) ? .locked : .inactive
    }

    private func updateRegexModeButtonState(effectiveMode: FfiSearchMode? = nil) {
        if currentMode == .regex {
            regexModeButton.visualState = .locked
        } else if let effective = effectiveMode {
            regexModeButton.visualState = (effective == .regex) ? .autoActive : .inactive
        } else {
            let hasSugar = Self.hasRegexSugar(query: searchField?.stringValue ?? "")
            regexModeButton.visualState = hasSugar ? .autoActive : .inactive
        }
    }

    static func hasRegexSugar(query: String) -> Bool {
        previewQueryFlags(query: query, mode: .auto).hasRegexSugar
    }

    // MARK: - Filter Popover Actions & State

    @objc private func filterButtonClicked(_ sender: NSButton) {
        filterPopoverController.toggle(relativeTo: filterButton)
    }

    private func updateFilterButtonState(forQuery query: String) {
        let (_, state) = FilterQuerySyncCoordinator.deserialize(query: query)
        updateFilterButton(state: state)
    }

    private func updateFilterButton(state: FilterState) {
        let count = state.activeCount
        filterButton.activeCount = count
        let config = NSImage.SymbolConfiguration(pointSize: 12, weight: .medium)
        filterButton.image = NSImage(systemSymbolName: "line.3.horizontal.decrease", accessibilityDescription: L("filter.title"))?.withSymbolConfiguration(config)

        if count > 0 {
            filterButton.title = "\(count)"
            filterButton.contentTintColor = .controlAccentColor
            filterButton.state = .on
            let template = L("filter.button.tooltip.active")
            filterButton.toolTip = template == "filter.button.tooltip.active"
                ? "\(L("filter.title")): \(count) (⌥F)"
                : String(format: template, count)
        } else {
            filterButton.title = ""
            filterButton.contentTintColor = .secondaryLabelColor
            filterButton.state = .off
            let tooltip = L("filter.button.tooltip")
            filterButton.toolTip = tooltip == "filter.button.tooltip"
                ? "\(L("filter.title")) (⌥F)"
                : tooltip
        }
        filterButton.needsDisplay = true
    }

    // MARK: - Syntax Highlighting

    /// 最近一次空结果检索命中的排除规则；索引状态变化后重算空态时沿用它，避免退回普通无结果
    private var lastExclusionHint: String?

    /// 从搜索框文本中提取首个包含 `/` 的路径词元（兼容 `in:` 前缀与带引号的含空格路径）
    static func extractDiagnosticPathCandidate(from query: String) -> String? {
        var tokens: [String] = []
        var current = ""
        var inQuotes = false
        for ch in query {
            if ch == "\"" {
                inQuotes.toggle()
                continue
            }
            if ch.isWhitespace && !inQuotes {
                if !current.isEmpty {
                    tokens.append(current)
                    current = ""
                }
            } else {
                current.append(ch)
            }
        }
        if !current.isEmpty {
            tokens.append(current)
        }
        return tokens
            .map { $0.hasPrefix("in:") ? String($0.dropFirst(3)) : $0 }
            .first { $0.contains("/") }
    }

    private func updateEmptyState() {
        searchResultsViewController.updateEmptyContext(
            query: searchField?.stringValue ?? "",
            isFuzzyActive: currentMode == .fuzzy
        )
        switch indexingState {
        case .initializing, .rebuilding:
            searchResultsViewController.emptyState = .indexing
        case .idle, .failed:
            // 失败有自己的告警与底栏状态，空态不冒充"正在建立索引"
            if let hint = lastExclusionHint {
                searchResultsViewController.emptyState = .excluded(rule: hint)
            } else if shouldShowIdleEmptyState {
                searchResultsViewController.emptyState = .idleEmpty
            } else {
                searchResultsViewController.emptyState = .noMatches
            }
        }
    }

    private var hasAppliedFilterSyntaxHighlighting: Bool = false

    /// 针对搜索框内限定词（type: / size: / ext:）即时应用语法胶囊高亮
    private func applyQuerySyntaxHighlighting() {
        guard let fieldEditor = searchField.currentEditor() as? NSTextView,
              !fieldEditor.hasMarkedText(),
              let storage = fieldEditor.textStorage else {
            return
        }

        let query = fieldEditor.string
        let hasFilterSyntax = FilterQuerySyncCoordinator.containsFilterSyntax(query)

        // 若当前输入没有限定词语法，且之前未应用过限定词高亮，完全不触碰 textStorage，
        // 彻底交由 AppKit 原生单行滚动与光标机制接管，保证直接粘贴超长路径时光标始终在末尾可视区
        if !hasFilterSyntax && !hasAppliedFilterSyntaxHighlighting {
            return
        }

        PillLayoutManager.ensureInstalled(on: fieldEditor)

        let baseFont = searchField.font ?? .systemFont(ofSize: 14)
        let formatted = FilterQuerySyncCoordinator.formatQuerySyntax(
            query,
            baseFont: baseFont
        )

        hasAppliedFilterSyntaxHighlighting = hasFilterSyntax

        guard formatted.length == storage.length else { return }
        let fullRange = NSRange(location: 0, length: storage.length)
        let savedRange = fieldEditor.selectedRange
        storage.beginEditing()
        formatted.enumerateAttributes(in: fullRange) { attributes, range, _ in
            storage.setAttributes(attributes, range: range)
        }
        storage.endEditing()
        fieldEditor.selectedRange = savedRange
        // 重置编辑态默认属性，防止胶囊末尾紧跟中文/多字节输入法组字时继承胶囊蓝色衬底与字距
        fieldEditor.typingAttributes = [
            .font: baseFont,
            .foregroundColor: NSColor.labelColor,
        ]
    }

    // MARK: - Focus Management

    /// 统一 Esc (Escape) 键分层响应机制：
    /// 1. 若当前开启了快速预览（Quick Look），优先关闭预览面板；
    /// 2. 若展开了过滤气泡浮层，优先关闭气泡浮层；
    /// 3. 若搜索输入框存在文本，清空输入框并重置回默认排序结果集，同时聚焦输入框；
    /// 4. 若搜索输入框为空，触发窗口退出微动效并隐藏视窗。
    func handleEscape() {
        if QLPreviewPanel.sharedPreviewPanelExists() && QLPreviewPanel.shared().isVisible {
            QLPreviewPanel.shared().orderOut(nil)
            return
        }
        if self.filterPopoverController.isShown {
            self.filterPopoverController.close()
            return
        }
        if !searchField.stringValue.isEmpty {
            searchField.stringValue = ""
            applyQuerySyntaxHighlighting()
            updateCaseSensitivityButtonState()
            searchChanged()
            focusSearchField()
            return
        }
        hideWindowWithAnimation()
    }

    /// 聚焦搜索输入框，并将输入光标定位至文本末尾
    func focusSearchField() {
        guard let window = window, let searchField = searchField else { return }
        window.makeFirstResponder(searchField)
        if let editor = searchField.currentEditor() as? NSTextView {
            let length = (searchField.stringValue as NSString).length
            let targetRange = NSRange(location: length, length: 0)
            editor.selectedRange = targetRange
            editor.scrollRangeToVisible(targetRange)
        }
        applyQuerySyntaxHighlighting()
    }
}

// MARK: - SearchMainSplitViewControllerDelegate

extension SearchWindow: SearchMainSplitViewControllerDelegate {
    func searchMainSplitViewDidSelect(result: FfiSearchResult?) {
        searchResultsViewController.updateBottomPath(for: result)
    }

    func searchMainSplitViewDidDoubleClick(result: FfiSearchResult) {
        recordCurrentSearchQueryIfNeeded()
        openResultWithFeedback(result)
    }

    func searchMainSplitViewDidConsumeResult() {
        recordCurrentSearchQueryIfNeeded()
    }

    func searchMainSplitViewDidRequestRebuild() {
        rebuildIndexClicked()
    }

    func searchMainSplitViewDidRequestFocusSearchField() {
        focusSearchField()
    }

    func searchMainSplitViewDidTrashItems(paths: Set<String>, remainingCount: Int) {
        updateMatchCountText(searchResultsViewController.results.count)
    }

    func searchMainSplitViewDidFilterResults(filteredCount: Int, totalCount: Int, activeDirectory: String?) {
        guard lastSearchError == nil else { return }
        if !isApplyingIdleEmptyState
            && !splitViewController.isUpdatingSearchResults
            && activeDirectory == nil
            && shouldShowIdleEmptyState
            && (!splitViewController.rawResults.isEmpty || searchResultsViewController.emptyState != .idleEmpty) {
            loadDefaultResults()
            return
        }
        if !searchField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || activeDirectory != nil {
            updateMatchCountText(filteredCount)
        } else {
            searchMatchText = nil
        }
    }

    func searchMainSplitViewDidChangeServerSort(_ sort: FfiSearchSort) {
        guard indexingState != .failed else { return }
        currentSort = sort
        let query = searchField.stringValue
        if query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            loadDefaultResults()
        } else {
            performSearch(query: query, autoSelectFirstRow: false)
        }
    }

    func searchMainSplitViewDidSwitchToClientSort() {
        currentSort = .relevance
    }
}

// MARK: - NSWindowDelegate

extension SearchWindow: NSWindowDelegate {
    func windowDidMove(_ notification: Notification) {
        saveWindowOriginIfNeeded()
        if !isApplyingProgrammaticFrame && pendingScreenOverflowCheck {
            resetScreenOverflowDebounce()
        }
    }

    func windowDidResize(_ notification: Notification) {
        if !isApplyingProgrammaticFrame {
            cancelPendingScreenOverflowCheck()
        }
        saveWindowSizeIfNeeded()
    }

    func windowDidChangeScreen(_ notification: Notification) {
        scheduleScreenOverflowCheck()
    }
    
    func windowShouldClose(_ sender: NSWindow) -> Bool {
        hideWindowWithAnimation()
        return false
    }
}

// MARK: - NSSearchFieldDelegate

extension SearchWindow: NSSearchFieldDelegate {
    func controlTextDidChange(_ obj: Notification) {
        applyQuerySyntaxHighlighting()
        updateCaseSensitivityButtonState()
        updateRegexModeButtonState()
        searchChanged()
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy commandSelector: Selector) -> Bool {
        // 回车确认命令：输入法组字期间放行上屏；其余场景归档有效搜索历史并打开当前选定条目
        if commandSelector == #selector(NSResponder.insertNewline(_:)) {
            if textView.hasMarkedText() {
                return false
            }
            openSelected()
            return true
        }

        // 取消命令 (Escape)：
        // 1. 若处于输入法组字/候选态（markedText），放行交由输入法取消候选；
        // 2. 其余场景统一由 handleEscape 分层响应（关闭浮层/清空输入/退出窗口）。
        if commandSelector == #selector(NSResponder.cancelOperation(_:)) {
            if textView.hasMarkedText() {
                return false
            }
            handleEscape()
            return true
        }

        // 方向上键语义命令：当输入为空时，按向上键回填上一次搜索词
        if commandSelector == #selector(NSResponder.moveUp(_:)) {
            let length = (textView.string as NSString).length
            if length == 0, let lastQuery = recentSearches.first {
                currentSort = .relevance
                self.lastQuery = lastQuery
                searchField.stringValue = lastQuery
                focusSearchField()
                performSearch(query: lastQuery)
                return true
            }
        }

        // 方向下键语义命令：当光标处于文本末尾或输入为空时，将焦点交接至结果列表；
        // 若首行已随检索自动预选且结果超过 1 条，直接顺滑推进至第 2 行，避免重复按两次下键
        if commandSelector == #selector(NSResponder.moveDown(_:)) {
            let length = (textView.string as NSString).length
            if textView.selectedRange.length == 0 && textView.selectedRange.location == length {
                let count = searchResultsViewController.results.count
                if count > 0 {
                    let targetRow: Int? = (searchResultsViewController.tableView.selectedRow == 0 && count > 1) ? 1 : nil
                    return searchResultsViewController.focusResults(row: targetRow)
                }
            }
        }
        return false
    }
}

// MARK: - Toolbar Item Identifiers

extension NSToolbarItem.Identifier {
    static let searchField = NSToolbarItem.Identifier("lokii.toolbar.searchField")
    static let pinWindow = NSToolbarItem.Identifier("lokii.toolbar.pinWindow")
    static let resetWindowSize = NSToolbarItem.Identifier("lokii.toolbar.resetWindowSize")
}

/// 锁定为纯图标模式的主工具栏，屏蔽系统右键“图标和文本”切换，保证通栏检索布局紧凑稳定
final class LokiiToolbar: NSToolbar {
    override var displayMode: NSToolbar.DisplayMode {
        get { .iconOnly }
        set { /* 强制锁定纯图标显示模式 */ }
    }
}

// MARK: - NSToolbarDelegate

extension SearchWindow: NSToolbarDelegate {
    func toolbar(
        _ toolbar: NSToolbar,
        itemForItemIdentifier itemIdentifier: NSToolbarItem.Identifier,
        willBeInsertedIntoToolbar flag: Bool
    ) -> NSToolbarItem? {
        switch itemIdentifier {
        case .toggleSidebar:
            let item = NSToolbarItem(itemIdentifier: itemIdentifier)
            item.label = L("search.toggleSidebar")
            item.paletteLabel = L("search.toggleSidebar")
            item.toolTip = L("search.toggleSidebar")
            return item

        case .sidebarTrackingSeparator:
            return NSTrackingSeparatorToolbarItem(
                identifier: itemIdentifier,
                splitView: splitViewController.splitView,
                dividerIndex: 0
            )

        case .searchField:
            let item = NSToolbarItem(itemIdentifier: itemIdentifier)
            item.view = searchContainer
            item.label = L("search.placeholder")
            item.paletteLabel = L("search.placeholder")
            item.isNavigational = true
            return item

        case .pinWindow:
            pinButton.bezelStyle = .toolbar
            pinButton.isBordered = false
            pinButton.target = self
            pinButton.action = #selector(togglePin)
            updatePinButtonState()

            let item = NSToolbarItem(itemIdentifier: itemIdentifier)
            item.view = pinButton
            item.label = L("search.toolbar.pin")
            item.paletteLabel = L("search.toolbar.pin")
            item.toolTip = L("search.toolbar.pin")
            return item

        case .resetWindowSize:
            let button = NSButton()
            button.image = NSImage(systemSymbolName: "textformat.size", accessibilityDescription: L("window.viewOptions"))
            button.bezelStyle = .toolbar
            button.isBordered = false
            button.toolTip = L("window.viewOptions")
            button.contentTintColor = .secondaryLabelColor
            button.target = self
            button.action = #selector(showViewOptionsMenu(_:))

            let item = NSToolbarItem(itemIdentifier: itemIdentifier)
            item.view = button
            item.label = L("window.viewOptions")
            item.paletteLabel = L("window.viewOptions")
            item.toolTip = L("window.viewOptions")
            return item

        default:
            return nil
        }
    }

    func toolbarDefaultItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [.toggleSidebar, .sidebarTrackingSeparator, .searchField, .pinWindow, .resetWindowSize]
    }

    func toolbarAllowedItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [.toggleSidebar, .sidebarTrackingSeparator, .searchField, .pinWindow, .resetWindowSize]
    }
}

// MARK: - View Options & Density Control

extension SearchWindow: NSMenuItemValidation {
    private var currentTableFontSize: CGFloat {
        get { TableDensity.savedFontSize }
        set {
            TableDensity.savedFontSize = newValue
            NotificationCenter.default.post(
                name: .tableDensityDidChange,
                object: nil,
                userInfo: ["fontSize": newValue]
            )
        }
    }

    @objc private func showViewOptionsMenu(_ sender: NSButton) {
        let menu = buildViewOptionsMenu()
        let location = NSPoint(x: 0, y: sender.bounds.height + 4)
        menu.popUp(positioning: nil, at: location, in: sender)
    }

    func buildViewOptionsMenu() -> NSMenu {
        let menu = NSMenu(title: L("window.viewOptions"))
        menu.autoenablesItems = true

        let zoomInItem = NSMenuItem(
            title: L("menu.zoomIn"),
            action: #selector(zoomInAction(_:)),
            keyEquivalent: "+"
        )
        zoomInItem.target = self
        menu.addItem(zoomInItem)

        let zoomOutItem = NSMenuItem(
            title: L("menu.zoomOut"),
            action: #selector(zoomOutAction(_:)),
            keyEquivalent: "-"
        )
        zoomOutItem.target = self
        menu.addItem(zoomOutItem)

        let actualSizeItem = NSMenuItem(
            title: L("menu.actualSize"),
            action: #selector(zoomActualSizeAction(_:)),
            keyEquivalent: "0"
        )
        actualSizeItem.target = self
        menu.addItem(actualSizeItem)

        menu.addItem(NSMenuItem.separator())

        for density in TableDensity.allCases {
            let item = NSMenuItem(
                title: L(density.titleKey),
                action: #selector(selectDensityAction(_:)),
                keyEquivalent: ""
            )
            item.tag = density.rawValue
            item.target = self
            menu.addItem(item)
        }

        menu.addItem(NSMenuItem.separator())

        let resetWindowItem = NSMenuItem(
            title: L("window.resetSize"),
            action: #selector(resetToDefaultWindowFrame),
            keyEquivalent: "0"
        )
        resetWindowItem.keyEquivalentModifierMask = [.command, .option]
        resetWindowItem.target = self
        menu.addItem(resetWindowItem)

        let floatingBarItem = NSMenuItem(
            title: L("window.switchToFloatingBar"),
            action: #selector(requestSwitchToFloatingBar),
            keyEquivalent: "m"
        )
        floatingBarItem.keyEquivalentModifierMask = [.command, .shift]
        floatingBarItem.target = self
        menu.addItem(floatingBarItem)

        menu.addItem(NSMenuItem.separator())

        let preferencesItem = NSMenuItem(
            title: L("menu.preferences"),
            action: #selector(openPreferencesAction(_:)),
            keyEquivalent: ","
        )
        preferencesItem.target = self
        menu.addItem(preferencesItem)

        return menu
    }

    /// 按步长缩放列表字号，并钳制在可读区间内
    private func adjustFontSize(by delta: CGFloat) {
        let current = currentTableFontSize
        let next = min(
            max(current + delta, CGFloat(TableDensity.minFontSize)),
            CGFloat(TableDensity.maxFontSize)
        )
        if next != current {
            currentTableFontSize = next
        }
    }

    @objc private func zoomInAction(_ sender: Any?) {
        adjustFontSize(by: 2)
    }

    @objc private func zoomOutAction(_ sender: Any?) {
        adjustFontSize(by: -2)
    }

    @objc private func zoomActualSizeAction(_ sender: Any?) {
        currentTableFontSize = CGFloat(TableDensity.defaultFontSize)
    }

    @objc private func selectDensityAction(_ sender: NSMenuItem) {
        if let density = TableDensity(rawValue: sender.tag) {
            currentTableFontSize = density.fontSize
        }
    }

    @objc private func openPreferencesAction(_ sender: Any?) {
        (NSApp.delegate as? AppDelegate)?.openPreferences()
    }

    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        if menuItem.action == #selector(zoomInAction(_:)) {
            return currentTableFontSize < CGFloat(TableDensity.maxFontSize)
        }
        if menuItem.action == #selector(zoomOutAction(_:)) {
            return currentTableFontSize > CGFloat(TableDensity.minFontSize)
        }
        if menuItem.action == #selector(zoomActualSizeAction(_:)) {
            return currentTableFontSize != CGFloat(TableDensity.defaultFontSize)
        }
        if menuItem.action == #selector(selectDensityAction(_:)) {
            menuItem.state = (Int(currentTableFontSize) == menuItem.tag) ? .on : .off
            return true
        }
        return true
    }
}

// MARK: - 内嵌过滤触发按钮

/// 镶嵌于搜索框右侧的内联过滤触发按钮。
/// 核心设计：
/// 1. 锁定固定几何尺寸（38 x 22pt），彻底消除激活数字角标出现/消失时引发的工具栏重排与浮层定位漂移；
/// 2. 自绘轻量级悬停与激活态圆角胶囊底衬，不产生冗余系统级双层套环。
/// 点击透传的堆栈容器，承载过滤图标与计数标签而不拦截按钮鼠标事件。
private final class FilterTriggerContentStack: NSStackView {
    override func hitTest(_ point: NSPoint) -> NSView? { nil }
}

final class FilterTriggerButton: NSButton {
    var activeCount: Int = 0 {
        didSet {
            if oldValue != activeCount {
                updateContentAppearance()
            }
        }
    }

    override var isHighlighted: Bool {
        didSet {
            if oldValue != isHighlighted {
                updateContentAppearance()
            }
        }
    }

    private var isHovered: Bool = false {
        didSet {
            if oldValue != isHovered {
                updateContentAppearance()
            }
        }
    }

    private let contentStack = FilterTriggerContentStack()
    private let iconView = NSImageView()
    private let countLabel = NSTextField(labelWithString: "")
    private var trackingAreaRef: NSTrackingArea?

    override var intrinsicContentSize: NSSize {
        NSSize(width: 38, height: 22)
    }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        setupContentViews()
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        setupContentViews()
    }

    private func setupContentViews() {
        contentStack.translatesAutoresizingMaskIntoConstraints = false
        contentStack.orientation = .horizontal
        contentStack.alignment = .centerY
        contentStack.spacing = 3

        iconView.imageScaling = .scaleNone
        countLabel.font = .monospacedDigitSystemFont(ofSize: 11, weight: .semibold)
        countLabel.isEditable = false
        countLabel.isSelectable = false
        countLabel.isBezeled = false
        countLabel.drawsBackground = false

        contentStack.addArrangedSubview(iconView)
        contentStack.addArrangedSubview(countLabel)
        addSubview(contentStack)

        NSLayoutConstraint.activate([
            contentStack.centerXAnchor.constraint(equalTo: centerXAnchor),
            contentStack.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])

        updateContentAppearance()
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        updateContentAppearance()
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let existing = trackingAreaRef {
            removeTrackingArea(existing)
        }
        let area = NSTrackingArea(
            rect: bounds,
            options: [.mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect],
            owner: self,
            userInfo: nil
        )
        addTrackingArea(area)
        trackingAreaRef = area
    }

    override func mouseEntered(with event: NSEvent) {
        super.mouseEntered(with: event)
        isHovered = true
    }

    override func mouseExited(with event: NSEvent) {
        super.mouseExited(with: event)
        isHovered = false
    }

    private func updateContentAppearance() {
        let isActive = activeCount > 0
        let fgColor: NSColor = isActive ? .controlAccentColor : (isHovered ? .labelColor : .secondaryLabelColor)
        let symbolConfig = NSImage.SymbolConfiguration(pointSize: 11.5, weight: isActive ? .semibold : .medium)
        iconView.image = NSImage(systemSymbolName: "line.3.horizontal.decrease", accessibilityDescription: nil)?
            .withSymbolConfiguration(symbolConfig)
        iconView.contentTintColor = fgColor
        countLabel.stringValue = isActive ? "\(activeCount)" : ""
        countLabel.textColor = fgColor
        countLabel.isHidden = !isActive
        needsDisplay = true
    }

    override func draw(_ dirtyRect: NSRect) {
        let isActive = activeCount > 0
        let isPressed = isHighlighted

        // 仅自绘轻量级悬停或激活态圆角微胶囊底衬，图标与计数由原生子视图渲染
        let pillRect = bounds.insetBy(dx: 1, dy: 1)
        let pillPath = NSBezierPath(roundedRect: pillRect, xRadius: 6, yRadius: 6)

        if isActive {
            let alpha: CGFloat = isPressed ? 0.22 : (isHovered ? 0.17 : 0.12)
            NSColor.controlAccentColor.withAlphaComponent(alpha).setFill()
            pillPath.fill()
        } else if isPressed {
            NSColor.labelColor.withAlphaComponent(0.10).setFill()
            pillPath.fill()
        } else if isHovered {
            NSColor.labelColor.withAlphaComponent(0.06).setFill()
            pillPath.fill()
        }
    }
}


