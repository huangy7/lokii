import AppKit
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
        let visible = screen.visibleFrame
        // 宽度取可见屏幕宽度的 60%，钳制在 [960, 1280] pt（不超过可用屏幕宽度）
        let rawWidth = min(max(visible.width * 0.60, 960), 1280)
        let finalWidth = min(rawWidth, visible.width)
        // 高度取可见屏幕高度的 62%，钳制在 [600, 860] pt（不超过可用屏幕高度）
        let rawHeight = min(max(visible.height * 0.62, 600), 860)
        let finalHeight = min(rawHeight, visible.height)

        // 水平居中，垂直方向位于视窗黄金中心（屏幕中心微偏上 5% 屏幕高度）并钳制在工作区内
        let x = visible.origin.x + (visible.width - finalWidth) / 2
        let targetY = visible.origin.y + (visible.height - finalHeight) / 2 + (visible.height * 0.05)
        let y = min(max(targetY, visible.origin.y), visible.maxY - finalHeight)
        return NSRect(x: round(x), y: round(y), width: round(finalWidth), height: round(finalHeight))
    }

    private let savedWindowFrameKey = "LokiiMainWindowFrame"
    let splitViewController = SearchMainSplitViewController()
    var searchResultsViewController: SearchResultsViewController { splitViewController.searchResultsViewController }
    var sidebarTreeViewController: SidebarTreeViewController { splitViewController.sidebarTreeViewController }

    // MARK: - Pin State & Window Level
    private(set) var isPinned: Bool = false
    private let pinButton = NSButton()

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
    private var indexingState: IndexingState = .initializing

    init(engine: SearchEngine) {
        self.engine = engine

        let window = LokiiWindow(
            contentRect: Self.calculateDefaultWindowFrame(),
            styleMask: [.titled, .closable, .resizable, .miniaturizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.titlebarAppearsTransparent = true
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

        setupUI()
        reloadWindowBehavior()
        applyInitialWindowFrame()
        window.delegate = self
        window.contentView?.wantsLayer = true
        setupRecentSearchesMenu()
        setupNotifications()
        onLanguageChanged()

        searchResultsViewController.onOpenFDAGuide = {
            FDAGuideWindowController.show()
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

    /// 一键恢复自适应默认窗口尺寸并清除持久化位置
    @objc func resetToDefaultWindowFrame() {
        guard let window else { return }
        clearSavedWindowFrame()
        window.animator().setFrame(defaultWindowFrame, display: true)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    override func showWindow(_ sender: Any?) {
        guard let window = window else { return }
        
        let isFirstAppear = !window.isVisible
        if isFirstAppear {
            let mouseLoc = NSEvent.mouseLocation
            let mouseScreen = NSScreen.screens.first { NSMouseInRect(mouseLoc, $0.frame, false) }
            if let targetScreen = mouseScreen, targetScreen != window.screen {
                let newFrame = Self.calculateDefaultWindowFrame(on: targetScreen)
                window.setFrame(newFrame, display: false)
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
        }
    }

    @objc private func hideWindowWithAnimation() {
        if filterPopoverController.isShown {
            filterPopoverController.close()
        }
        if QLPreviewPanel.sharedPreviewPanelExists() && QLPreviewPanel.shared().isVisible {
            QLPreviewPanel.shared().orderOut(nil)
        }
        guard let window = window, window.isVisible else { return }
        
        // Remove scale pop on close for cleaner exit
        NSAnimationContext.runAnimationGroup({ ctx in
            ctx.duration = 0.15
            ctx.timingFunction = CAMediaTimingFunction(name: .easeOut)
            window.animator().alphaValue = 0.0
        }, completionHandler: { [weak self] in
            self?.window?.orderOut(nil)
            self?.window?.alphaValue = 1.0
            self?.window?.contentView?.layer?.transform = CATransform3DIdentity
        })
    }

    // MARK: - UI Setup

    private func setupUI() {
        // ── Toolbar with standard sidebar toggle, tracking separator, and search field ──
        searchContainer.translatesAutoresizingMaskIntoConstraints = false

        searchField = NSSearchField()
        searchField.translatesAutoresizingMaskIntoConstraints = false
        searchField.placeholderString = L("search.placeholder")
        searchField.font = .systemFont(ofSize: 14)
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
                self.lastQuery = query
                if query.isEmpty {
                    self.searchResultsViewController.updateBottomPath(with: nil)
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
        nc.addObserver(self, selector: #selector(onStartupMode(_:)),
                       name: SearchEngine.startupModeNotification, object: nil)
        nc.addObserver(self, selector: #selector(onInitializationFailed(_:)),
                       name: SearchEngine.initializationFailedNotification, object: nil)
        nc.addObserver(self, selector: #selector(onConfigReloaded(_:)),
                       name: SearchEngine.configReloadedNotification, object: nil)
        nc.addObserver(self, selector: #selector(onAppDidBecomeActive),
                       name: NSApplication.didBecomeActiveNotification, object: nil)
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
        guard phase != "verifying", indexingState != .idle else { return }
        if indexingState == .initializing && !isColdStart { return }
        let source = note.userInfo?["source"] as? String ?? ""
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
        // 到达时仅刷新计数，不得据此结束当前阶段
        switch indexingState {
        case .idle:
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
        if currentText.isEmpty {
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
        if currentQuery.isEmpty {
            loadDefaultResults()
        } else {
            performSearch(query: currentQuery)
        }
    }

    // MARK: - Window Behavior

    private func applyInitialWindowFrame() {
        guard let window else { return }
        if shouldRememberWindowPosition, restoreSavedWindowFrame() {
            ensureWindowMeetsMinimumSize(window)
            return
        }
        window.setFrame(defaultWindowFrame, display: false)
        window.center()
    }

    private func applyWindowBehaviorFromConfig() {
        guard let window else { return }
        if shouldRememberWindowPosition {
            if restoreSavedWindowFrame() {
                ensureWindowMeetsMinimumSize(window)
            } else {
                saveWindowFrameIfNeeded()
            }
        } else {
            clearSavedWindowFrame()
        }
    }

    /// 窗口位置记忆开关的本地缓存，随配置重载刷新。
    /// 拖动与缩放回调按显示刷新率触发，若每次实时跨 FFI 解码整份配置将产生大量冗余开销。
    private var shouldRememberWindowPosition = false

    private func reloadWindowBehavior() {
        shouldRememberWindowPosition = engine.getConfig()?.window.rememberPosition ?? false
    }

    private func saveWindowFrameIfNeeded() {
        guard shouldRememberWindowPosition, let window else { return }
        UserDefaults.standard.set(NSStringFromRect(window.frame), forKey: savedWindowFrameKey)
    }

    @discardableResult
    private func restoreSavedWindowFrame() -> Bool {
        guard let frameString = UserDefaults.standard.string(forKey: savedWindowFrameKey),
              !frameString.isEmpty else { return false }
        let frame = NSRectFromString(frameString)
        guard frame.width > 0, frame.height > 0 else { return false }
        window?.setFrame(frame, display: false)
        return true
    }

    private func clearSavedWindowFrame() {
        UserDefaults.standard.removeObject(forKey: savedWindowFrameKey)
    }

    private func ensureWindowMeetsMinimumSize(_ window: NSWindow) {
        guard window.frame.width < window.minSize.width || window.frame.height < window.minSize.height else { return }
        window.setFrame(defaultWindowFrame, display: false)
        window.center()
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

    @objc private func rebuildIndexClicked() {
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
        engine.rebuildIndex()
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
        searchWorkItem?.cancel()

        if query.isEmpty {
            searchResultsViewController.updateBottomPath(with: nil)
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

    private var isSearching = false

    /// 搜索框为空时自动呈现全量索引文件与目录树（按文件名自然字母顺序排列，且清除匹配高亮）
    private func loadDefaultResults() {
        guard indexingState != .failed, !isSearching else { return }
        isSearching = true

        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            guard let self else { return }
            let response = self.engine.search(query: "*", mode: .wildcard)
            // 默认未检索状态下按修改时间倒序排列（最近活跃优先），时间相同时按文件名自然字母顺序排列
            let items: [FfiSearchResult] = response.results
                .sorted {
                    if $0.modified != $1.modified {
                        return $0.modified > $1.modified
                    }
                    return $0.name.localizedStandardCompare($1.name) == .orderedAscending
                }
                .map { item in
                    FfiSearchResult(
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
                        searchMode: item.searchMode
                    )
                }
            let tree: ResultDirectoryTree? = items.isEmpty ? nil : ResultDirectoryTree.build(from: items)

            DispatchQueue.main.async {
                self.isSearching = false
                guard self.indexingState != .failed else { return }
                self.updateFilterButtonState(forQuery: self.searchField.stringValue)

                let currentText = self.searchField.stringValue
                if !currentText.isEmpty {
                    self.lastQuery = currentText
                    self.performSearch(query: currentText)
                    return
                }
                self.searchResultsViewController.hasActiveSearchQuery = false
                self.splitViewController.updateSearchResults(rawResults: items, tree: tree)
                self.searchMatchText = nil
                self.updateStatusLabel()
            }
        }
    }

    private func performSearch(query: String) {
        guard indexingState != .failed, !isSearching else { return }
        isSearching = true
        searchMatchText = .key("search.searching")

        let mode = currentMode
        let caseSensitivity = currentCaseSensitivity

        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            guard let self else { return }
            let response = self.engine.search(query: query, mode: mode, caseSensitivity: caseSensitivity)
            let tree: ResultDirectoryTree? = response.results.isEmpty ? nil : ResultDirectoryTree.build(from: response.results)
            DispatchQueue.main.async {
                self.isSearching = false
                guard self.indexingState != .failed else { return }
                self.searchResultsViewController.hasActiveSearchQuery = !query.isEmpty
                self.splitViewController.updateSearchResults(rawResults: response.results, tree: tree)
                self.updateFilterButtonState(forQuery: self.searchField.stringValue)
                self.updateCaseSensitivityButtonState(responseCaseSensitive: response.isCaseSensitive)
                self.updateRegexModeButtonState(effectiveMode: response.effectiveMode)

                if let error = response.error {
                    let formattedError = error.hasPrefix("⚠️") ? error : "⚠️ \(error)"
                    self.searchMatchText = .verbatim(formattedError)
                } else {
                    let count = response.results.count
                    if count == 1 {
                        self.searchMatchText = .key("search.matchedCount.one")
                    } else {
                        self.searchMatchText = .formatted("search.matchedCount", formatFileCount(UInt64(count)))
                    }
                }

                // 检索期间若输入内容变动或检索模式/大小写敏感发生切换，在此重新调度
                let currentText = self.searchField.stringValue
                if (currentText != query && !currentText.isEmpty) || self.currentMode != mode || self.currentCaseSensitivity != caseSensitivity {
                    if currentText.isEmpty {
                        self.searchResultsViewController.updateBottomPath(with: nil)
                        self.loadDefaultResults()
                    } else {
                        self.lastQuery = currentText
                        self.performSearch(query: currentText)
                    }
                } else if currentText.isEmpty && currentText != query {
                    self.searchResultsViewController.updateBottomPath(with: nil)
                    self.loadDefaultResults()
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
        for item in selected {
            engine.openFile(path: item.path)
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
                let hasUpper = Self.hasEffectiveUppercase(query: query)
                caseSensitivityButton.visualState = hasUpper ? .autoActive : .inactive
            }
        }
    }

    static func hasEffectiveUppercase(query: String) -> Bool {
        for word in query.split(separator: " ") {
            if word.hasPrefix("in:") || word.hasPrefix("type:") || word.hasPrefix("ext:") || word.hasPrefix("size:") {
                continue
            }
            if word.contains("/") {
                if word.hasSuffix("/") { continue }
                if let last = word.split(separator: "/").last,
                   last.contains(where: { $0.isUppercase }) {
                    return true
                }
                continue
            }
            if word.hasPrefix(".") && !word.hasPrefix("..") {
                continue
            }
            if word.contains(where: { $0.isUppercase }) {
                return true
            }
        }
        return false
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
        let trimmed = query.trimmingCharacters(in: .whitespaces)
        if trimmed.hasPrefix("regex:") { return true }
        guard trimmed.hasPrefix("/"), trimmed.count >= 2 else { return false }
        // Check if closed by unescaped slash
        let chars = Array(trimmed)
        var i = chars.count - 1
        // Check for trailing 'i' or 'I'
        if i >= 1 && (chars[i] == "i" || chars[i] == "I") {
            i -= 1
        }
        guard i >= 1 && chars[i] == "/" else { return false }
        var backslashCount = 0
        var j = i - 1
        while j >= 0 && chars[j] == "\\" {
            backslashCount += 1
            j -= 1
        }
        guard backslashCount % 2 == 0 else { return false }

        // Intermediate unescaped slashes indicate a directory path rather than a regex pattern
        var k = 1
        var intermediateBackslashes = 0
        while k < i {
            if chars[k] == "/" && intermediateBackslashes % 2 == 0 {
                return false
            }
            if chars[k] == "\\" {
                intermediateBackslashes += 1
            } else {
                intermediateBackslashes = 0
            }
            k += 1
        }
        return true
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

    /// 针对搜索框内限定词（type: / size: / ext:）即时应用语法胶囊高亮
    private func applyQuerySyntaxHighlighting() {
        guard let fieldEditor = searchField.currentEditor() as? NSTextView,
              !fieldEditor.hasMarkedText(),
              let storage = fieldEditor.textStorage else {
            return
        }

        PillLayoutManager.ensureInstalled(on: fieldEditor)

        let query = fieldEditor.string
        let formatted = FilterQuerySyncCoordinator.formatQuerySyntax(
            query,
            baseFont: searchField.font ?? .systemFont(ofSize: 14)
        )

        let savedRange = fieldEditor.selectedRange
        storage.beginEditing()
        storage.setAttributedString(formatted)
        storage.endEditing()
        fieldEditor.selectedRange = savedRange
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
        applyQuerySyntaxHighlighting()
        if let editor = searchField.currentEditor() {
            let length = (searchField.stringValue as NSString).length
            editor.selectedRange = NSRange(location: length, length: 0)
        }
    }
}

// MARK: - SearchMainSplitViewControllerDelegate

extension SearchWindow: SearchMainSplitViewControllerDelegate {
    func searchMainSplitViewDidSelect(result: FfiSearchResult?) {
        searchResultsViewController.updateBottomPath(with: result?.path)
    }

    func searchMainSplitViewDidDoubleClick(result: FfiSearchResult) {
        recordCurrentSearchQueryIfNeeded()
        engine.openFile(path: result.path)
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
        if remainingCount == 1 {
            searchMatchText = .key("search.matchedCount.one")
        } else {
            searchMatchText = .formatted("search.matchedCount", formatFileCount(UInt64(remainingCount)))
        }
    }
}

// MARK: - NSWindowDelegate

extension SearchWindow: NSWindowDelegate {
    func windowDidMove(_ notification: Notification) {
        saveWindowFrameIfNeeded()
    }

    func windowDidResize(_ notification: Notification) {
        saveWindowFrameIfNeeded()
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
        // 回车确认命令：当存在命中结果时归档有效搜索历史，并打开当前选定条目
        if commandSelector == #selector(NSResponder.insertNewline(_:)) {
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
                searchField.stringValue = lastQuery
                focusSearchField()
                performSearch(query: lastQuery)
                return true
            }
        }

        // 方向下键语义命令：当光标处于文本末尾或输入为空时，将焦点交接至结果列表
        if commandSelector == #selector(NSResponder.moveDown(_:)) {
            let length = (textView.string as NSString).length
            if textView.selectedRange.length == 0 && textView.selectedRange.location == length {
                if searchResultsViewController.results.count > 0 {
                    return searchResultsViewController.focusResults()
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
final class FilterTriggerButton: NSButton {
    var activeCount: Int = 0 {
        didSet {
            if oldValue != activeCount {
                needsDisplay = true
            }
        }
    }

    private var isHovered: Bool = false {
        didSet {
            if oldValue != isHovered {
                needsDisplay = true
            }
        }
    }

    private var trackingAreaRef: NSTrackingArea?

    override var intrinsicContentSize: NSSize {
        NSSize(width: 38, height: 22)
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

    override func draw(_ dirtyRect: NSRect) {
        let isActive = activeCount > 0
        let isPressed = isHighlighted

        // 绘制悬停态或激活态圆角微胶囊底衬
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

        let fgColor: NSColor = isActive ? .controlAccentColor : (isHovered ? .labelColor : .secondaryLabelColor)
        let symbolConfig = NSImage.SymbolConfiguration(pointSize: 11.5, weight: isActive ? .semibold : .medium)
        guard let baseIcon = NSImage(systemSymbolName: "line.3.horizontal.decrease", accessibilityDescription: nil),
              let configuredIcon = baseIcon.withSymbolConfiguration(symbolConfig) else {
            return
        }

        // 渲染着色图标与可选的激活计数
        let iconSize = configuredIcon.size
        if isActive {
            let badgeText = "\(activeCount)" as NSString
            let font = NSFont.monospacedDigitSystemFont(ofSize: 11, weight: .semibold)
            let attrs: [NSAttributedString.Key: Any] = [
                .font: font,
                .foregroundColor: fgColor
            ]
            let textSize = badgeText.size(withAttributes: attrs)
            let spacing: CGFloat = 3.0
            let totalWidth = iconSize.width + spacing + textSize.width
            let startX = round((bounds.width - totalWidth) / 2.0)
            let iconY = round((bounds.height - iconSize.height) / 2.0)

            drawTintedImage(configuredIcon, in: NSRect(x: startX, y: iconY, width: iconSize.width, height: iconSize.height), tint: fgColor)

            let textY = round((bounds.height - textSize.height) / 2.0)
            badgeText.draw(at: NSPoint(x: startX + iconSize.width + spacing, y: textY), withAttributes: attrs)
        } else {
            let iconX = round((bounds.width - iconSize.width) / 2.0)
            let iconY = round((bounds.height - iconSize.height) / 2.0)
            drawTintedImage(configuredIcon, in: NSRect(x: iconX, y: iconY, width: iconSize.width, height: iconSize.height), tint: fgColor)
        }
    }

    private func drawTintedImage(_ image: NSImage, in rect: NSRect, tint: NSColor) {
        guard let cgImage = image.cgImage(forProposedRect: nil, context: nil, hints: nil),
              let ctx = NSGraphicsContext.current?.cgContext else {
            image.draw(in: rect)
            return
        }
        ctx.saveGState()
        // AppKit 按钮视图默认 isFlipped == true，CGContext 需在局部坐标系翻转以正向绘制图标マスク
        ctx.translateBy(x: rect.minX, y: rect.maxY)
        ctx.scaleBy(x: 1.0, y: -1.0)
        let localRect = CGRect(x: 0, y: 0, width: rect.width, height: rect.height)
        ctx.clip(to: localRect, mask: cgImage)
        tint.setFill()
        ctx.fill(localRect)
        ctx.restoreGState()
    }
}


