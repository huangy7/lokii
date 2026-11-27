import AppKit
import Combine
import Quartz
import UniformTypeIdentifiers

/// 降低透明度无障碍模式下的不透明底衬视图，确保禁用半透明效果时文字对比度稳定可读
private final class FloatingBarReduceTransparencyBackingView: NSView {
    var cornerRadius: CGFloat = FloatingSearchBarLayout.collapsedCornerRadius {
        didSet {
            if oldValue != cornerRadius {
                needsDisplay = true
            }
        }
    }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    override func hitTest(_ point: NSPoint) -> NSView? { nil }
    override var wantsUpdateLayer: Bool { true }

    override func updateLayer() {
        super.updateLayer()
        guard let layer else { return }
        layer.cornerCurve = .continuous
        layer.cornerRadius = cornerRadius
        layer.masksToBounds = true
        if NSWorkspace.shared.accessibilityDisplayShouldReduceTransparency {
            layer.backgroundColor = NSColor.windowBackgroundColor.cgColor
        } else {
            layer.backgroundColor = NSColor.clear.cgColor
        }
    }
}

/// 兼容旧版系统的半透明镜面高光与细线玻璃描边覆盖层，完全透传鼠标命中事件
private final class FloatingBarSpecularRimView: NSView {
    var cornerRadius: CGFloat = FloatingSearchBarLayout.collapsedCornerRadius {
        didSet {
            if oldValue != cornerRadius {
                needsDisplay = true
            }
        }
    }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    override func hitTest(_ point: NSPoint) -> NSView? { nil }
    override var wantsUpdateLayer: Bool { true }

    override func updateLayer() {
        super.updateLayer()
        guard let layer else { return }
        let isDark = effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        layer.cornerCurve = .continuous
        layer.cornerRadius = cornerRadius
        layer.borderWidth = 1.0
        layer.backgroundColor = NSColor.clear.cgColor
        layer.borderColor = (isDark
            ? NSColor.white.withAlphaComponent(0.20)
            : NSColor.white.withAlphaComponent(0.55)).cgColor
    }
}

/// 悬浮搜索条专属列表视图：支持无遮挡空格快速查看、首次点击即选中与无直角框右键菜单弹出
private final class FloatingResultsTableView: NSTableView {
    var onQuickLookSpace: (() -> Void)?
    var rowContextMenu: NSMenu?

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func keyDown(with event: NSEvent) {
        if event.keyCode == 53 { // Escape
            window?.cancelOperation(nil)
            return
        }
        if event.keyCode == 49 { // Space
            onQuickLookSpace?()
            return
        }
        super.keyDown(with: event)
    }

    override func mouseDown(with event: NSEvent) {
        if event.modifierFlags.contains(.control) {
            rightMouseDown(with: event)
            return
        }
        super.mouseDown(with: event)
    }

    override func rightMouseDown(with event: NSEvent) {
        let point = convert(event.locationInWindow, from: nil)
        let row = self.row(at: point)
        guard row >= 0, row < numberOfRows, let menu = rowContextMenu else { return }
        if !selectedRowIndexes.contains(row) {
            selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        }
        // 直接调用 NSMenu.popUpContextMenu，避免 NSTableView 原生右键流程在胶囊选区外叠加直角蓝色高亮框
        NSMenu.popUpContextMenu(menu, with: event, for: self)
    }
}

/// 无标题栏、空输入自动收折、液态玻璃质感、用完即走的悬浮快速搜索条控制器
final class FloatingSearchWindowController: NSWindowController {
    private let engine: SearchEngine
    private let positionStore = FloatingBarPositionStore.shared

    var onRequestSwitchToStandardWindow: ((SearchHandoffState) -> Void)?
    var onOpenIndexPreferences: ((String?) -> Void)?
    private(set) var isDraggingOut: Bool = false

    // MARK: - Search, Position & Mode State
    private var results: [FfiSearchResult] = []
    private var lastQuery: String = ""
    private var currentMode: FfiSearchMode = .auto
    private var currentCaseSensitivity: FfiCaseSensitivity = .auto
    private var hasAppliedFilterSyntaxHighlighting = false
    private var searchWorkItem: DispatchWorkItem?
    private var isIndexingActive: Bool = false
    private var expansionState: FloatingSearchBarLayout.ExpansionState = .collapsed

    private var shouldRememberWindowPosition = false
    private var isApplyingProgrammaticFrame = false

    private let searchGenerationLock = NSLock()
    private var latestSearchGeneration: UInt64 = 0
    private var hideAnimationToken: UInt64 = 0
    private var temporaryStatusWorkItem: DispatchWorkItem?
    private var isLiveScrolling = false
    private var isSnappingScroll = false
    private var scrollSnapWorkItem: DispatchWorkItem?

    // MARK: - Glass Surface & Views
    private let glassHostView = NSView()
    private let contentContainer = NSView()
    private let reduceTransparencyBackingView = FloatingBarReduceTransparencyBackingView()
    private var glassEffectViewRef: AnyObject?
    private var fallbackEffectView: NSVisualEffectView?
    private var fallbackSpecularRimView: FloatingBarSpecularRimView?

    private let inputBarContainer = NSView()
    private let searchIconButton = NSButton()
    private let indexingSpinner = NSProgressIndicator()
    private var searchField: NSSearchField!
    private let selectedPreviewIconButton = NSButton()
    private var selectedPreviewPath: String?
    private let clearButton = NSButton()
    private let caseSensitivityButton = CaseSensitivityButton()
    private let fuzzyModeButton = FuzzyModeButton()
    private let regexModeButton = RegexModeButton()
    private let modeDivider = NSBox()
    private let filterButton = FilterTriggerButton()
    private let expandWindowButton = NSButton()
    private let filterCoordinator = FilterQuerySyncCoordinator()
    private var isFilterBarExpanded = false

    private let topDivider = FloatingBarHairlineDividerView()
    private let inlineFilterBar = FloatingInlineFilterBarView()
    private let bodyContainer = NSView()
    private let tableScrollView = NSScrollView()
    private let tableView = FloatingResultsTableView()
    private let emptyStateView = FloatingGlassEmptyStateView()

    private var topDividerHeightConstraint: NSLayoutConstraint!
    private var inlineFilterBarHeightConstraint: NSLayoutConstraint!
    private var bodyTopConstraint: NSLayoutConstraint!
    private var bodyBottomConstraint: NSLayoutConstraint!

    private let transientToastView = NSView()
    private let transientToastLabel = NSTextField(labelWithString: "")

    // MARK: - Icon Cache
    private var typeIconCache: [String: NSImage] = [:]
    private let iconCache = NSCache<NSString, NSImage>()
    private let iconLoadQueue = DispatchQueue(label: "com.lokii.floatingBar.iconLoad", qos: .utility)

    private var volumeStatusCancellable: AnyCancellable?

    private static let recentSearchesKey = "LokiiRecentSearches"
    private var recentSearches: [String] {
        get { Array((UserDefaults.standard.stringArray(forKey: Self.recentSearchesKey) ?? []).prefix(SearchWindow.maxRecentSearches)) }
        set { UserDefaults.standard.set(Array(newValue.prefix(SearchWindow.maxRecentSearches)), forKey: Self.recentSearchesKey) }
    }

    init(engine: SearchEngine) {
        self.engine = engine

        let screen = NSScreen.main ?? NSScreen.screens.first
        let initialRect = screen.map {
            FloatingSearchBarLayout.initialFrame(on: ScreenMetrics(screen: $0), state: .collapsed)
        } ?? NSRect(x: 0, y: 0, width: FloatingSearchBarLayout.defaultWidth, height: FloatingSearchBarLayout.inputBarHeight)

        let panel = FloatingSearchPanel(contentRect: initialRect)
        super.init(window: panel)

        panel.delegate = self
        panel.previewPanelHandler = self
        panel.onEscape = { [weak self] in self?.handleEscape() }
        panel.onCommandShiftM = { [weak self] in self?.switchToStandardWindow() }
        panel.onOptionF = { [weak self] in
            self?.toggleFilterBar()
        }
        panel.onCommandF = { [weak self] in self?.focusSearchField() }
        panel.onCommandReturn = { [weak self] in self?.revealSelectedAndDismiss() }
        panel.onCommandShiftC = { [weak self] in self?.copySelectedPathAndDismiss() }
        panel.onCommandT = { [weak self] in self?.openSelectedInTerminalAndDismiss() }
        panel.onQuickLookShortcut = { [weak self] in self?.toggleQuickLook() }

        iconCache.countLimit = 256
        reloadWindowBehavior()
        setupUI()
        setupContextMenu()
        setupNotifications()
        refreshLocalization()
        applyExpansionState(.collapsed, animate: false)

        volumeStatusCancellable = engine.$volumeStatuses
            .receive(on: DispatchQueue.main)
            .sink { [weak self] _ in
                self?.refreshVisibleRowsAfterVolumeChange()
            }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    deinit {
        searchWorkItem?.cancel()
        temporaryStatusWorkItem?.cancel()
        scrollSnapWorkItem?.cancel()
        volumeStatusCancellable?.cancel()
        NotificationCenter.default.removeObserver(self)
        NSWorkspace.shared.notificationCenter.removeObserver(self)
    }

    // MARK: - Show / Hide / Position Memory

    override func showWindow(_ sender: Any?) {
        showWindow(anchorRect: nil)
    }

    func showWindow(anchorRect: NSRect?) {
        guard let panel = window as? FloatingSearchPanel else { return }
        hideAnimationToken &+= 1
        let isFirstAppear = !panel.isVisible

        if isFirstAppear {
            let anchorScreen: NSScreen? = anchorRect.flatMap { rect in
                let mid = NSPoint(x: rect.midX, y: rect.midY)
                return NSScreen.screens.first { $0.frame.contains(mid) }
            }
            let mouseLoc = NSEvent.mouseLocation
            let mouseScreen = NSScreen.screens.first { NSMouseInRect(mouseLoc, $0.frame, false) }
            if let targetScreen = anchorScreen ?? mouseScreen ?? panel.screen ?? NSScreen.main ?? NSScreen.screens.first {
                let allMetrics = NSScreen.screens.map(ScreenMetrics.init(screen:))
                let targetFrame = FloatingSearchBarLayout.resolveShowFrame(
                    state: expansionState,
                    rememberPosition: shouldRememberWindowPosition,
                    savedTopLeft: positionStore.savedTopLeft(),
                    anchorRect: anchorRect,
                    targetScreen: ScreenMetrics(screen: targetScreen),
                    allScreens: allMetrics,
                    filterBarExpanded: isFilterBarExpanded
                )
                applyProgrammaticFrame(targetFrame, display: false, animate: false)
            }
            panel.alphaValue = 0.0
            if !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion,
               let layer = contentContainer.layer {
                let center = CGPoint(x: layer.bounds.midX, y: layer.bounds.midY)
                var transform = CATransform3DIdentity
                transform = CATransform3DTranslate(transform, center.x, center.y, 0)
                transform = CATransform3DScale(transform, 0.96, 0.96, 1.0)
                transform = CATransform3DTranslate(transform, -center.x, -center.y, 0)
                layer.transform = transform
            }
        }

        super.showWindow(nil)
        panel.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        focusSearchField(selectAll: !searchField.stringValue.isEmpty)
        panel.invalidateShadow()

        if isFirstAppear && !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion {
            NSAnimationContext.runAnimationGroup({ ctx in
                ctx.duration = 0.14
                ctx.timingFunction = CAMediaTimingFunction(name: .easeOut)
                panel.animator().alphaValue = 1.0
            }, completionHandler: { [weak self] in
                self?.window?.invalidateShadow()
            })
            if let layer = contentContainer.layer {
                let spring = CASpringAnimation(keyPath: "transform")
                spring.fromValue = layer.transform
                spring.toValue = CATransform3DIdentity
                spring.damping = 22
                spring.stiffness = 340
                spring.mass = 0.9
                spring.duration = spring.settlingDuration
                layer.transform = CATransform3DIdentity
                layer.add(spring, forKey: "floatingSpringScale")
            }
        } else {
            panel.alphaValue = 1.0
            contentContainer.layer?.transform = CATransform3DIdentity
        }
    }

    func hideWindow(animated: Bool = false) {
        if QLPreviewPanel.sharedPreviewPanelExists() && QLPreviewPanel.shared().isVisible {
            QLPreviewPanel.shared().orderOut(nil)
        }
        guard let panel = window, panel.isVisible else { return }

        hideAnimationToken &+= 1
        let token = hideAnimationToken

        guard animated && !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion else {
            panel.orderOut(nil)
            panel.alphaValue = 1.0
            contentContainer.layer?.transform = CATransform3DIdentity
            return
        }

        NSAnimationContext.runAnimationGroup({ ctx in
            ctx.duration = 0.10
            ctx.timingFunction = CAMediaTimingFunction(name: .easeOut)
            panel.animator().alphaValue = 0.0
        }, completionHandler: { [weak self] in
            guard let self, self.hideAnimationToken == token else { return }
            self.window?.orderOut(nil)
            self.window?.alphaValue = 1.0
            self.contentContainer.layer?.transform = CATransform3DIdentity
        })
    }

    /// 清除已记忆的悬浮搜索条坐标，若当前可见则平滑复位至屏幕中上位置
    @objc func resetSavedPosition() {
        positionStore.clearTopLeft()
        guard let panel = window, panel.isVisible else { return }
        let targetScreen = panel.screen ?? NSScreen.main ?? NSScreen.screens.first
        guard let targetScreen else { return }
        let frame = FloatingSearchBarLayout.initialFrame(
            on: ScreenMetrics(screen: targetScreen),
            state: expansionState,
            filterBarExpanded: isFilterBarExpanded
        )
        applyProgrammaticFrame(frame, display: true, animate: true)
        panel.invalidateShadow()
    }

    private func applyProgrammaticFrame(_ frame: NSRect, display: Bool, animate: Bool) {
        guard let panel = window else { return }
        isApplyingProgrammaticFrame = true
        defer { isApplyingProgrammaticFrame = false }
        let shouldAnimate = animate && !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
        panel.setFrame(frame, display: display, animate: shouldAnimate)
    }

    private func reloadWindowBehavior() {
        shouldRememberWindowPosition = engine.getConfig()?.window.rememberPosition ?? false
    }

    /// 用完即走收起：归档有效历史检索词，清空输入框与结果并重置回 58pt 单行收折态
    private func dismissAfterAction() {
        recordCurrentSearchQueryIfNeeded()
        if QLPreviewPanel.sharedPreviewPanelExists() && QLPreviewPanel.shared().isVisible {
            QLPreviewPanel.shared().orderOut(nil)
        }
        hideAnimationToken &+= 1
        window?.orderOut(nil)
        window?.alphaValue = 1.0
        contentContainer.layer?.transform = CATransform3DIdentity
        resetToCollapsedIdleState(animate: false)
    }

    private func resetToCollapsedIdleState(animate: Bool) {
        searchWorkItem?.cancel()
        scrollSnapWorkItem?.cancel()
        _ = nextSearchGeneration()
        lastQuery = ""
        hasAppliedFilterSyntaxHighlighting = false
        isFilterBarExpanded = false
        results = []
        searchField.stringValue = ""
        clearButton.isHidden = true
        updateSelectedPreviewBadge(for: nil)
        inlineFilterBar.update(state: FilterState())
        updateFilterButtonState(forQuery: "")
        updateCaseSensitivityButtonState()
        updateRegexModeButtonState()
        tableView.reloadData()
        applyExpansionState(.collapsed, animate: animate)
    }

    // MARK: - Handoff Between Modes

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
        searchField.stringValue = state.query
        lastQuery = state.query
        clearButton.isHidden = state.query.isEmpty
        updateCaseSensitivityButtonState()
        updateFuzzyModeButtonState()
        updateRegexModeButtonState()
        updateFilterButtonState(forQuery: state.query)
        applyQuerySyntaxHighlighting()

        let trimmed = state.query.trimmingCharacters(in: .whitespacesAndNewlines)
        let filterState = FilterQuerySyncCoordinator.deserialize(query: state.query).state
        inlineFilterBar.update(state: filterState)
        if trimmed.isEmpty && filterState.isUnconstrained {
            results = []
            updateSelectedPreviewBadge(for: nil)
            tableView.reloadData()
            applyExpansionState(.collapsed, animate: false)
        } else {
            performSearch(query: state.query)
        }
    }

    @objc private func switchToStandardWindow() {
        let state = currentHandoffState()
        if QLPreviewPanel.sharedPreviewPanelExists() && QLPreviewPanel.shared().isVisible {
            QLPreviewPanel.shared().orderOut(nil)
        }
        onRequestSwitchToStandardWindow?(state)
    }

    // MARK: - UI Setup

    private func setupUI() {
        guard let panel = window else { return }

        // 使用纯透明、连续圆角裁剪的宿主视图包裹原生玻璃视图，裁除激活态玻璃着色器溢出的四角微弱通道，同时完整保留外层窗口阴影
        glassHostView.frame = panel.contentLayoutRect
        glassHostView.autoresizingMask = [.width, .height]
        glassHostView.wantsLayer = true
        glassHostView.layer?.backgroundColor = NSColor.clear.cgColor
        glassHostView.layer?.cornerCurve = .continuous
        glassHostView.layer?.cornerRadius = FloatingSearchBarLayout.collapsedCornerRadius
        glassHostView.layer?.masksToBounds = true
        panel.contentView = glassHostView

        contentContainer.translatesAutoresizingMaskIntoConstraints = false
        contentContainer.wantsLayer = true
        contentContainer.layer?.backgroundColor = NSColor.clear.cgColor
        contentContainer.layer?.cornerCurve = .continuous
        contentContainer.layer?.cornerRadius = FloatingSearchBarLayout.collapsedCornerRadius
        contentContainer.layer?.masksToBounds = true

        reduceTransparencyBackingView.translatesAutoresizingMaskIntoConstraints = false
        contentContainer.addSubview(reduceTransparencyBackingView)

        if #available(macOS 26.0, *) {
            let glassView = NSGlassEffectView(frame: glassHostView.bounds)
            glassView.autoresizingMask = [.width, .height]
            glassView.style = .regular
            glassView.cornerRadius = FloatingSearchBarLayout.collapsedCornerRadius
            glassView.contentView = contentContainer
            glassHostView.addSubview(glassView)
            glassEffectViewRef = glassView
        } else {
            let visualView = NSVisualEffectView(frame: glassHostView.bounds)
            visualView.autoresizingMask = [.width, .height]
            visualView.material = .hudWindow
            visualView.blendingMode = .behindWindow
            visualView.state = .active
            visualView.wantsLayer = true
            visualView.layer?.backgroundColor = NSColor.clear.cgColor
            visualView.layer?.cornerCurve = .continuous
            visualView.layer?.cornerRadius = FloatingSearchBarLayout.collapsedCornerRadius
            visualView.layer?.masksToBounds = true
            glassHostView.addSubview(visualView)
            fallbackEffectView = visualView

            visualView.addSubview(contentContainer)

            let rimView = FloatingBarSpecularRimView()
            rimView.translatesAutoresizingMaskIntoConstraints = false
            visualView.addSubview(rimView)
            fallbackSpecularRimView = rimView

            NSLayoutConstraint.activate([
                contentContainer.leadingAnchor.constraint(equalTo: visualView.leadingAnchor),
                contentContainer.trailingAnchor.constraint(equalTo: visualView.trailingAnchor),
                contentContainer.topAnchor.constraint(equalTo: visualView.topAnchor),
                contentContainer.bottomAnchor.constraint(equalTo: visualView.bottomAnchor),

                rimView.leadingAnchor.constraint(equalTo: visualView.leadingAnchor),
                rimView.trailingAnchor.constraint(equalTo: visualView.trailingAnchor),
                rimView.topAnchor.constraint(equalTo: visualView.topAnchor),
                rimView.bottomAnchor.constraint(equalTo: visualView.bottomAnchor),
            ])
        }

        NSLayoutConstraint.activate([
            reduceTransparencyBackingView.leadingAnchor.constraint(equalTo: contentContainer.leadingAnchor),
            reduceTransparencyBackingView.trailingAnchor.constraint(equalTo: contentContainer.trailingAnchor),
            reduceTransparencyBackingView.topAnchor.constraint(equalTo: contentContainer.topAnchor),
            reduceTransparencyBackingView.bottomAnchor.constraint(equalTo: contentContainer.bottomAnchor),
        ])

        setupInputBar()
        setupDividers()
        setupBodyContainer()
        setupTransientToast()
        updateControlVisualOpacity()
    }

    private func setupInputBar() {
        inputBarContainer.translatesAutoresizingMaskIntoConstraints = false
        contentContainer.addSubview(inputBarContainer)

        let iconConfig = NSImage.SymbolConfiguration(pointSize: 19, weight: .regular)
        searchIconButton.translatesAutoresizingMaskIntoConstraints = false
        searchIconButton.isBordered = false
        searchIconButton.focusRingType = .none
        searchIconButton.image = NSImage(systemSymbolName: "magnifyingglass", accessibilityDescription: L("search.history.title"))?
            .withSymbolConfiguration(iconConfig)
        searchIconButton.contentTintColor = .secondaryLabelColor
        searchIconButton.target = self
        searchIconButton.action = #selector(showRecentSearchesMenu(_:))

        indexingSpinner.translatesAutoresizingMaskIntoConstraints = false
        indexingSpinner.style = .spinning
        indexingSpinner.controlSize = .small
        indexingSpinner.isHidden = true

        searchField = NSSearchField()
        searchField.translatesAutoresizingMaskIntoConstraints = false
        searchField.isBordered = false
        searchField.isBezeled = false
        searchField.drawsBackground = false
        searchField.focusRingType = .none
        searchField.font = .systemFont(ofSize: 20, weight: .regular)
        searchField.delegate = self
        searchField.sendsSearchStringImmediately = false
        searchField.sendsWholeSearchString = false
        if let cell = searchField.cell as? NSSearchFieldCell {
            cell.searchButtonCell = nil
            cell.cancelButtonCell = nil
            cell.isScrollable = true
            cell.wraps = false
            cell.lineBreakMode = .byClipping
        }

        selectedPreviewIconButton.translatesAutoresizingMaskIntoConstraints = false
        selectedPreviewIconButton.isBordered = false
        selectedPreviewIconButton.focusRingType = .none
        selectedPreviewIconButton.imageScaling = .scaleProportionallyUpOrDown
        selectedPreviewIconButton.wantsLayer = true
        selectedPreviewIconButton.layer?.cornerRadius = 7
        selectedPreviewIconButton.layer?.masksToBounds = true
        selectedPreviewIconButton.target = self
        selectedPreviewIconButton.action = #selector(selectedPreviewClicked)
        selectedPreviewIconButton.isHidden = true

        let clearConfig = NSImage.SymbolConfiguration(pointSize: 14, weight: .regular)
        clearButton.translatesAutoresizingMaskIntoConstraints = false
        clearButton.isBordered = false
        clearButton.focusRingType = .none
        clearButton.image = NSImage(systemSymbolName: "xmark.circle.fill", accessibilityDescription: L("floatingBar.clear"))?
            .withSymbolConfiguration(clearConfig)
        clearButton.contentTintColor = .tertiaryLabelColor
        clearButton.target = self
        clearButton.action = #selector(clearButtonClicked)
        clearButton.isHidden = true

        caseSensitivityButton.translatesAutoresizingMaskIntoConstraints = false
        caseSensitivityButton.target = self
        caseSensitivityButton.action = #selector(caseSensitivityButtonClicked)

        fuzzyModeButton.translatesAutoresizingMaskIntoConstraints = false
        fuzzyModeButton.target = self
        fuzzyModeButton.action = #selector(fuzzyModeButtonClicked)

        regexModeButton.translatesAutoresizingMaskIntoConstraints = false
        regexModeButton.target = self
        regexModeButton.action = #selector(regexModeButtonClicked)

        modeDivider.translatesAutoresizingMaskIntoConstraints = false
        modeDivider.boxType = .custom
        modeDivider.borderWidth = 0
        modeDivider.fillColor = NSColor.separatorColor.withAlphaComponent(0.45)

        filterButton.translatesAutoresizingMaskIntoConstraints = false
        filterButton.isBordered = false
        filterButton.focusRingType = .none
        filterButton.target = self
        filterButton.action = #selector(filterButtonClicked)

        let expandConfig = NSImage.SymbolConfiguration(pointSize: 12, weight: .medium)
        expandWindowButton.translatesAutoresizingMaskIntoConstraints = false
        expandWindowButton.isBordered = false
        expandWindowButton.focusRingType = .none
        expandWindowButton.image = NSImage(systemSymbolName: "arrow.up.left.and.arrow.down.right", accessibilityDescription: L("floatingBar.expandToStandard"))?
            .withSymbolConfiguration(expandConfig)
        expandWindowButton.contentTintColor = .secondaryLabelColor
        expandWindowButton.target = self
        expandWindowButton.action = #selector(switchToStandardWindow)

        let controlsStack = NSStackView(views: [
            clearButton,
            caseSensitivityButton,
            fuzzyModeButton,
            regexModeButton,
            modeDivider,
            filterButton,
            expandWindowButton,
            selectedPreviewIconButton,
        ])
        controlsStack.translatesAutoresizingMaskIntoConstraints = false
        controlsStack.orientation = .horizontal
        controlsStack.alignment = .centerY
        controlsStack.spacing = 4
        controlsStack.setHuggingPriority(.required, for: .horizontal)

        inputBarContainer.addSubview(searchIconButton)
        inputBarContainer.addSubview(indexingSpinner)
        inputBarContainer.addSubview(searchField)
        inputBarContainer.addSubview(controlsStack)

        NSLayoutConstraint.activate([
            inputBarContainer.topAnchor.constraint(equalTo: contentContainer.topAnchor),
            inputBarContainer.leadingAnchor.constraint(equalTo: contentContainer.leadingAnchor),
            inputBarContainer.trailingAnchor.constraint(equalTo: contentContainer.trailingAnchor),
            inputBarContainer.heightAnchor.constraint(equalToConstant: FloatingSearchBarLayout.inputBarHeight),

            searchIconButton.leadingAnchor.constraint(equalTo: inputBarContainer.leadingAnchor, constant: 18),
            searchIconButton.centerYAnchor.constraint(equalTo: inputBarContainer.centerYAnchor),
            searchIconButton.widthAnchor.constraint(equalToConstant: 24),
            searchIconButton.heightAnchor.constraint(equalToConstant: 24),

            indexingSpinner.centerXAnchor.constraint(equalTo: searchIconButton.centerXAnchor),
            indexingSpinner.centerYAnchor.constraint(equalTo: searchIconButton.centerYAnchor),
            indexingSpinner.widthAnchor.constraint(equalToConstant: 16),
            indexingSpinner.heightAnchor.constraint(equalToConstant: 16),

            searchField.leadingAnchor.constraint(equalTo: searchIconButton.trailingAnchor, constant: 10),
            searchField.trailingAnchor.constraint(equalTo: controlsStack.leadingAnchor, constant: -8),
            searchField.centerYAnchor.constraint(equalTo: inputBarContainer.centerYAnchor),
            searchField.heightAnchor.constraint(equalToConstant: 30),

            controlsStack.trailingAnchor.constraint(equalTo: inputBarContainer.trailingAnchor, constant: -16),
            controlsStack.centerYAnchor.constraint(equalTo: inputBarContainer.centerYAnchor),

            clearButton.widthAnchor.constraint(equalToConstant: 20),
            clearButton.heightAnchor.constraint(equalToConstant: 20),
            caseSensitivityButton.widthAnchor.constraint(equalToConstant: 25),
            caseSensitivityButton.heightAnchor.constraint(equalToConstant: 20),
            fuzzyModeButton.widthAnchor.constraint(equalToConstant: 25),
            fuzzyModeButton.heightAnchor.constraint(equalToConstant: 20),
            regexModeButton.widthAnchor.constraint(equalToConstant: 25),
            regexModeButton.heightAnchor.constraint(equalToConstant: 20),
            modeDivider.widthAnchor.constraint(equalToConstant: 1),
            modeDivider.heightAnchor.constraint(equalToConstant: 14),
            filterButton.widthAnchor.constraint(equalToConstant: 36),
            filterButton.heightAnchor.constraint(equalToConstant: 22),
            expandWindowButton.widthAnchor.constraint(equalToConstant: 24),
            expandWindowButton.heightAnchor.constraint(equalToConstant: 22),
            selectedPreviewIconButton.widthAnchor.constraint(equalToConstant: 28),
            selectedPreviewIconButton.heightAnchor.constraint(equalToConstant: 28),
        ])

        inlineFilterBar.onStateChanged = { [weak self] newState in
            guard let self else { return }
            self.filterCoordinator.updateQueryFromUI(state: newState, searchField: self.searchField) { [weak self] newQuery in
                guard let self else { return }
                self.lastQuery = newQuery
                self.clearButton.isHidden = newQuery.isEmpty
                self.updateFilterButton(state: newState)
                self.applyQuerySyntaxHighlighting()
                self.evaluateQueryAndSearch(query: newQuery)
            }
        }
        updateFilterButtonState(forQuery: "")
    }

    private func setupDividers() {
        topDivider.translatesAutoresizingMaskIntoConstraints = false
        contentContainer.addSubview(topDivider)
    }

    private func setupBodyContainer() {
        inlineFilterBar.translatesAutoresizingMaskIntoConstraints = false
        inlineFilterBar.isHidden = true
        contentContainer.addSubview(inlineFilterBar)

        bodyContainer.translatesAutoresizingMaskIntoConstraints = false
        contentContainer.addSubview(bodyContainer)

        tableScrollView.translatesAutoresizingMaskIntoConstraints = false
        tableScrollView.hasVerticalScroller = false
        tableScrollView.hasHorizontalScroller = false
        tableScrollView.autohidesScrollers = true
        tableScrollView.scrollerStyle = .overlay
        tableScrollView.automaticallyAdjustsContentInsets = false
        tableScrollView.contentInsets = NSEdgeInsetsZero
        tableScrollView.borderType = .noBorder
        tableScrollView.drawsBackground = false
        tableScrollView.focusRingType = .none
        tableScrollView.contentView.postsBoundsChangedNotifications = true

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("FloatingResultColumn"))
        column.resizingMask = .autoresizingMask
        tableView.addTableColumn(column)
        tableView.headerView = nil
        tableView.focusRingType = .none
        tableView.rowHeight = FloatingSearchBarLayout.rowHeight
        tableView.intercellSpacing = .zero
        tableView.style = .plain
        tableView.backgroundColor = .clear
        tableView.usesAlternatingRowBackgroundColors = false
        tableView.allowsMultipleSelection = false
        tableView.allowsEmptySelection = false
        tableView.columnAutoresizingStyle = .uniformColumnAutoresizingStyle
        tableView.draggingDestinationFeedbackStyle = .none
        tableView.setDraggingSourceOperationMask([.copy, .move], forLocal: false)
        tableView.dataSource = self
        tableView.delegate = self
        tableView.doubleAction = #selector(tableViewDoubleClicked)
        tableView.target = self
        tableView.onQuickLookSpace = { [weak self] in self?.toggleQuickLook() }

        tableScrollView.documentView = tableView
        bodyContainer.addSubview(tableScrollView)

        emptyStateView.translatesAutoresizingMaskIntoConstraints = false
        emptyStateView.isHidden = true
        bodyContainer.addSubview(emptyStateView)

        topDividerHeightConstraint = topDivider.heightAnchor.constraint(equalToConstant: 0)
        inlineFilterBarHeightConstraint = inlineFilterBar.heightAnchor.constraint(equalToConstant: 0)
        bodyTopConstraint = bodyContainer.topAnchor.constraint(equalTo: inlineFilterBar.bottomAnchor, constant: 0)
        bodyBottomConstraint = bodyContainer.bottomAnchor.constraint(equalTo: contentContainer.bottomAnchor, constant: 0)

        NSLayoutConstraint.activate([
            topDivider.topAnchor.constraint(equalTo: inputBarContainer.bottomAnchor),
            topDivider.leadingAnchor.constraint(equalTo: contentContainer.leadingAnchor, constant: 16),
            topDivider.trailingAnchor.constraint(equalTo: contentContainer.trailingAnchor, constant: -16),
            topDividerHeightConstraint,

            inlineFilterBar.topAnchor.constraint(equalTo: topDivider.bottomAnchor),
            inlineFilterBar.leadingAnchor.constraint(equalTo: contentContainer.leadingAnchor),
            inlineFilterBar.trailingAnchor.constraint(equalTo: contentContainer.trailingAnchor),
            inlineFilterBarHeightConstraint,

            bodyTopConstraint,
            bodyContainer.leadingAnchor.constraint(equalTo: contentContainer.leadingAnchor),
            bodyContainer.trailingAnchor.constraint(equalTo: contentContainer.trailingAnchor),
            bodyBottomConstraint,

            tableScrollView.topAnchor.constraint(equalTo: bodyContainer.topAnchor),
            tableScrollView.leadingAnchor.constraint(equalTo: bodyContainer.leadingAnchor),
            tableScrollView.trailingAnchor.constraint(equalTo: bodyContainer.trailingAnchor),
            tableScrollView.bottomAnchor.constraint(equalTo: bodyContainer.bottomAnchor),

            emptyStateView.topAnchor.constraint(equalTo: bodyContainer.topAnchor),
            emptyStateView.leadingAnchor.constraint(equalTo: bodyContainer.leadingAnchor),
            emptyStateView.trailingAnchor.constraint(equalTo: bodyContainer.trailingAnchor),
            emptyStateView.bottomAnchor.constraint(equalTo: bodyContainer.bottomAnchor),
        ])
    }

    private func setupTransientToast() {
        transientToastView.translatesAutoresizingMaskIntoConstraints = false
        transientToastView.wantsLayer = true
        transientToastView.layer?.cornerRadius = 11
        transientToastView.layer?.backgroundColor = NSColor.black.withAlphaComponent(0.72).cgColor
        transientToastView.isHidden = true
        transientToastView.alphaValue = 0.0

        transientToastLabel.translatesAutoresizingMaskIntoConstraints = false
        transientToastLabel.font = .systemFont(ofSize: 11.5, weight: .medium)
        transientToastLabel.textColor = .white
        transientToastLabel.alignment = .center
        transientToastLabel.lineBreakMode = .byTruncatingTail

        transientToastView.addSubview(transientToastLabel)
        contentContainer.addSubview(transientToastView)

        NSLayoutConstraint.activate([
            transientToastLabel.leadingAnchor.constraint(equalTo: transientToastView.leadingAnchor, constant: 12),
            transientToastLabel.trailingAnchor.constraint(equalTo: transientToastView.trailingAnchor, constant: -12),
            transientToastLabel.topAnchor.constraint(equalTo: transientToastView.topAnchor, constant: 5),
            transientToastLabel.bottomAnchor.constraint(equalTo: transientToastView.bottomAnchor, constant: -5),

            transientToastView.centerXAnchor.constraint(equalTo: contentContainer.centerXAnchor),
            transientToastView.bottomAnchor.constraint(equalTo: contentContainer.bottomAnchor, constant: -10),
            transientToastView.widthAnchor.constraint(lessThanOrEqualTo: contentContainer.widthAnchor, constant: -40),
        ])
    }

    private func updateControlVisualOpacity() {
        caseSensitivityButton.alphaValue = (caseSensitivityButton.visualState == .inactive) ? 0.72 : 1.0
        fuzzyModeButton.alphaValue = (fuzzyModeButton.visualState == .inactive) ? 0.72 : 1.0
        regexModeButton.alphaValue = (regexModeButton.visualState == .inactive) ? 0.72 : 1.0
        filterButton.alphaValue = (isFilterBarExpanded || filterButton.activeCount > 0) ? 1.0 : 0.72
        expandWindowButton.alphaValue = 0.72
    }

    private func setupContextMenu() {
        let menu = NSMenu()
        menu.autoenablesItems = true
        menu.addItem(makeCtxItem(key: "search.ctx.open", action: #selector(contextOpenItem), keyEquivalent: ""))
        menu.addItem(makeCtxItem(key: "search.ctx.revealInFinder", action: #selector(contextRevealItem), keyEquivalent: ""))
        menu.addItem(makeCtxItem(key: "search.ctx.openInTerminal", action: #selector(contextOpenInTerminal), keyEquivalent: ""))
        menu.addItem(makeCtxItem(key: "search.ctx.quickLook", action: #selector(contextQuickLook), keyEquivalent: ""))
        menu.addItem(NSMenuItem.separator())
        menu.addItem(makeCtxItem(key: "search.ctx.copyPath", action: #selector(contextCopyPath), keyEquivalent: ""))
        menu.addItem(makeCtxItem(key: "search.ctx.copyName", action: #selector(contextCopyName), keyEquivalent: ""))
        menu.addItem(NSMenuItem.separator())
        menu.addItem(makeCtxItem(key: "floatingBar.ctx.openInStandard", action: #selector(switchToStandardWindow), keyEquivalent: ""))
        tableView.rowContextMenu = menu
    }

    private func makeCtxItem(key: String, action: Selector, keyEquivalent: String) -> NSMenuItem {
        let item = NSMenuItem(localizedKey: key, action: action, keyEquivalent: keyEquivalent)
        item.target = self
        return item
    }

    // MARK: - Expansion, Glass Morphing & Frame Transitions

    private func updateGlassCornerRadius(for state: FloatingSearchBarLayout.ExpansionState) {
        let radius = FloatingSearchBarLayout.cornerRadius(for: state, filterBarExpanded: isFilterBarExpanded)
        glassHostView.layer?.cornerRadius = radius
        contentContainer.layer?.cornerRadius = radius
        reduceTransparencyBackingView.cornerRadius = radius
        if #available(macOS 26.0, *), let glassView = glassEffectViewRef as? NSGlassEffectView {
            glassView.cornerRadius = radius
        }
        fallbackEffectView?.layer?.cornerRadius = radius
        fallbackSpecularRimView?.cornerRadius = radius
    }

    private func applyExpansionState(_ newState: FloatingSearchBarLayout.ExpansionState, animate: Bool) {
        expansionState = newState
        let hasBody = (newState != .collapsed)
        let showTopDivider = hasBody || isFilterBarExpanded

        topDivider.isHidden = !showTopDivider
        topDividerHeightConstraint.constant = showTopDivider ? FloatingSearchBarLayout.dividerHeight : 0
        inlineFilterBar.isHidden = !isFilterBarExpanded
        inlineFilterBarHeightConstraint.constant = isFilterBarExpanded ? FloatingSearchBarLayout.filterStripHeight : 0
        bodyContainer.isHidden = !hasBody
        bodyTopConstraint.constant = hasBody ? FloatingSearchBarLayout.bodyTopPadding : 0
        if hasBody {
            bodyBottomConstraint.constant = -FloatingSearchBarLayout.bodyBottomPadding
        } else if isFilterBarExpanded {
            bodyBottomConstraint.constant = -FloatingSearchBarLayout.filterOnlyBottomPadding
        } else {
            bodyBottomConstraint.constant = 0
        }

        updateGlassCornerRadius(for: newState)
        updateControlVisualOpacity()

        switch newState {
        case .collapsed:
            tableScrollView.isHidden = true
            emptyStateView.isHidden = true
            updateSelectedPreviewBadge(for: nil)
        case .results:
            tableScrollView.isHidden = false
            emptyStateView.isHidden = true
        case .message:
            tableScrollView.isHidden = true
            emptyStateView.isHidden = false
            updateSelectedPreviewBadge(for: nil)
        }

        guard let panel = window else { return }
        let screen = panel.screen ?? NSScreen.main ?? NSScreen.screens.first
        let metrics = screen.map { ScreenMetrics(screen: $0) }
            ?? ScreenMetrics(frame: panel.frame, visibleFrame: NSRect(x: 0, y: 0, width: 1440, height: 900))
        if case .results(let count) = newState {
            let visibleRows = FloatingSearchBarLayout.visibleRowCount(
                for: count,
                maxVisibleHeight: metrics.visibleFrame.height,
                filterBarExpanded: isFilterBarExpanded
            )
            tableScrollView.hasVerticalScroller = (count > visibleRows)
        } else {
            tableScrollView.hasVerticalScroller = false
        }

        let targetHeight = FloatingSearchBarLayout.targetHeight(
            for: newState,
            maxVisibleHeight: metrics.visibleFrame.height,
            filterBarExpanded: isFilterBarExpanded
        )
        let targetFrame = FloatingSearchBarLayout.resizedFrame(
            preservingTopOf: panel.frame,
            newHeight: targetHeight,
            on: metrics
        )
        if panel.frame != targetFrame {
            applyProgrammaticFrame(targetFrame, display: true, animate: animate && panel.isVisible)
        }
        panel.invalidateShadow()
        if animate && panel.isVisible {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.14) { [weak panel] in
                panel?.invalidateShadow()
            }
        }
    }

    // MARK: - Scroll Row Snapping

    @objc private func onWillStartLiveScroll(_ note: Notification) {
        isLiveScrolling = true
        scrollSnapWorkItem?.cancel()
        scrollSnapWorkItem = nil
    }

    @objc private func onDidEndLiveScroll(_ note: Notification) {
        isLiveScrolling = false
        snapScrollToRowBoundary(animated: true)
    }

    @objc private func onScrollClipBoundsChanged(_ note: Notification) {
        guard !isLiveScrolling, !isSnappingScroll, results.count > 0 else { return }
        scrollSnapWorkItem?.cancel()
        let item = DispatchWorkItem { [weak self] in
            self?.snapScrollToRowBoundary(animated: true)
        }
        scrollSnapWorkItem = item
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.12, execute: item)
    }

    private func snapScrollToRowBoundary(animated: Bool) {
        scrollSnapWorkItem?.cancel()
        scrollSnapWorkItem = nil
        guard !results.isEmpty, !isSnappingScroll else { return }

        let clipView = tableScrollView.contentView
        let currentY = clipView.bounds.origin.y
        let viewportHeight = clipView.bounds.height
        let contentHeight = tableView.bounds.height
        let targetY = FloatingSearchBarLayout.snappedScrollOffsetY(
            currentY: currentY,
            contentHeight: contentHeight,
            viewportHeight: viewportHeight
        )
        guard abs(targetY - currentY) >= 0.5 else { return }

        isSnappingScroll = true
        let targetOrigin = NSPoint(x: 0, y: targetY)
        if animated && !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion {
            NSAnimationContext.runAnimationGroup({ ctx in
                ctx.duration = 0.12
                ctx.timingFunction = CAMediaTimingFunction(name: .easeOut)
                clipView.animator().setBoundsOrigin(targetOrigin)
            }, completionHandler: { [weak self] in
                guard let self else { return }
                self.tableScrollView.reflectScrolledClipView(self.tableScrollView.contentView)
                self.isSnappingScroll = false
            })
        } else {
            clipView.setBoundsOrigin(targetOrigin)
            tableScrollView.reflectScrolledClipView(clipView)
            isSnappingScroll = false
        }
    }

    // MARK: - Notifications & Localization

    private func setupNotifications() {
        let nc = NotificationCenter.default
        nc.addObserver(self, selector: #selector(refreshLocalization), name: L10n.languageChangedNotification, object: nil)
        nc.addObserver(self, selector: #selector(onIndexProgress(_:)), name: SearchEngine.indexProgressNotification, object: nil)
        nc.addObserver(self, selector: #selector(onIndexReady(_:)), name: SearchEngine.indexReadyNotification, object: nil)
        nc.addObserver(self, selector: #selector(onStartupMode(_:)), name: SearchEngine.startupModeNotification, object: nil)
        nc.addObserver(self, selector: #selector(onVolumeStatusesChanged), name: SearchEngine.volumeStatusesChangedNotification, object: nil)
        nc.addObserver(self, selector: #selector(onConfigReloaded), name: SearchEngine.configReloadedNotification, object: nil)
        nc.addObserver(self, selector: #selector(resetSavedPosition), name: .lokiiResetWindowSize, object: nil)
        nc.addObserver(self, selector: #selector(onWillStartLiveScroll(_:)), name: NSScrollView.willStartLiveScrollNotification, object: tableScrollView)
        nc.addObserver(self, selector: #selector(onDidEndLiveScroll(_:)), name: NSScrollView.didEndLiveScrollNotification, object: tableScrollView)
        nc.addObserver(self, selector: #selector(onScrollClipBoundsChanged(_:)), name: NSView.boundsDidChangeNotification, object: tableScrollView.contentView)

        NSWorkspace.shared.notificationCenter.addObserver(
            self,
            selector: #selector(onAccessibilityDisplayOptionsChanged),
            name: NSWorkspace.accessibilityDisplayOptionsDidChangeNotification,
            object: nil
        )
    }

    @objc private func onAccessibilityDisplayOptionsChanged() {
        reduceTransparencyBackingView.needsDisplay = true
    }

    @objc private func onConfigReloaded() {
        reloadWindowBehavior()
        if !shouldRememberWindowPosition {
            positionStore.clearTopLeft()
        } else if positionStore.savedTopLeft() == nil, let panel = window, panel.isVisible {
            positionStore.saveTopLeft(NSPoint(x: panel.frame.minX, y: panel.frame.maxY))
        }
    }

    @objc private func refreshLocalization() {
        FloatingSearchResultCellView.clearLocalizationCaches()
        searchField.placeholderString = L("floatingBar.placeholder")
        searchIconButton.toolTip = L("search.history.title")
        clearButton.toolTip = L("floatingBar.clear")
        expandWindowButton.toolTip = L("floatingBar.expandToStandard")
        caseSensitivityButton.updateTooltipAndAccessibility()
        fuzzyModeButton.updateTooltipAndAccessibility()
        regexModeButton.updateTooltipAndAccessibility()
        updateFilterButtonState(forQuery: searchField.stringValue)
        inlineFilterBar.refreshLocalization()
        updateControlVisualOpacity()
        tableView.rowContextMenu?.refreshLocalizedTitles()
        tableView.reloadData()
    }

    @objc private func onStartupMode(_ note: Notification) {
        isIndexingActive = true
        updateIndexingSpinner()
    }

    @objc private func onIndexProgress(_ note: Notification) {
        let phase = note.userInfo?["phase"] as? String ?? ""
        guard phase != "verifying" else { return }
        isIndexingActive = true
        updateIndexingSpinner()
    }

    @objc private func onIndexReady(_ note: Notification) {
        isIndexingActive = false
        updateIndexingSpinner()
        let query = searchField.stringValue
        if !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            performSearch(query: query)
        }
    }

    @objc private func onVolumeStatusesChanged() {
        refreshVisibleRowsAfterVolumeChange()
    }

    private func refreshVisibleRowsAfterVolumeChange() {
        let query = searchField.stringValue
        if !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            performSearch(query: query)
        } else {
            tableView.reloadData()
        }
    }

    private func updateIndexingSpinner() {
        searchIconButton.isHidden = isIndexingActive
        indexingSpinner.isHidden = !isIndexingActive
        if isIndexingActive {
            indexingSpinner.startAnimation(nil)
        } else {
            indexingSpinner.stopAnimation(nil)
        }
    }

    // MARK: - Search Execution

    private func nextSearchGeneration() -> UInt64 {
        searchGenerationLock.lock()
        latestSearchGeneration &+= 1
        let gen = latestSearchGeneration
        searchGenerationLock.unlock()
        return gen
    }

    private func isCurrentSearchGeneration(_ generation: UInt64) -> Bool {
        searchGenerationLock.lock()
        let current = (latestSearchGeneration == generation)
        searchGenerationLock.unlock()
        return current
    }

    @objc private func searchChanged() {
        let fieldEditor = searchField.currentEditor() as? NSTextView
        if fieldEditor?.hasMarkedText() != true,
           filterCoordinator.flushPendingStateIfNeeded(searchField: searchField, performSearch: { [weak self] newQuery in
               guard let self else { return }
               self.lastQuery = newQuery
               self.clearButton.isHidden = newQuery.isEmpty
               self.updateFilterButtonState(forQuery: newQuery)
               self.applyQuerySyntaxHighlighting()
               self.evaluateQueryAndSearch(query: newQuery)
           }) {
            return
        }
        let query = searchField.stringValue
        clearButton.isHidden = query.isEmpty
        if fieldEditor?.hasMarkedText() != true && filterCoordinator.pendingState == nil {
            filterCoordinator.updateUIFromQuery(query: query) { [weak self] state in
                self?.inlineFilterBar.update(state: state)
                self?.updateFilterButton(state: state)
            }
        } else {
            updateFilterButtonState(forQuery: query)
        }
        guard query != lastQuery else { return }
        lastQuery = query
        evaluateQueryAndSearch(query: query)
    }

    private func evaluateQueryAndSearch(query: String) {
        searchWorkItem?.cancel()

        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        let filterState = FilterQuerySyncCoordinator.deserialize(query: query).state

        if trimmed.isEmpty && filterState.isUnconstrained {
            _ = nextSearchGeneration()
            results = []
            updateCaseSensitivityButtonState()
            updateRegexModeButtonState()
            tableView.reloadData()
            applyExpansionState(.collapsed, animate: true)
            return
        }

        let item = DispatchWorkItem { [weak self] in
            self?.performSearch(query: query)
        }
        searchWorkItem = item
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.016, execute: item)
    }

    private func performSearch(query: String) {
        let generation = nextSearchGeneration()
        let mode = currentMode
        let caseSensitivity = currentCaseSensitivity

        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            guard let self else { return }
            let response = self.engine.search(query: query, mode: mode, caseSensitivity: caseSensitivity)
            guard self.isCurrentSearchGeneration(generation) else { return }

            DispatchQueue.main.async {
                guard self.isCurrentSearchGeneration(generation) else { return }
                self.results = response.results
                self.updateCaseSensitivityButtonState(responseCaseSensitive: response.isCaseSensitive)
                self.updateRegexModeButtonState(effectiveMode: response.effectiveMode)
                self.tableView.reloadData()

                if response.results.isEmpty {
                    self.configureMessageView(for: response)
                    self.applyExpansionState(.message, animate: true)
                } else {
                    self.tableView.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
                    self.tableView.scrollRowToVisible(0)
                    self.updateSelectedPreviewBadge(for: response.results.first)
                    self.applyExpansionState(.results(count: response.results.count), animate: true)
                }
                if QLPreviewPanel.sharedPreviewPanelExists() && QLPreviewPanel.shared().isVisible {
                    QLPreviewPanel.shared().reloadData()
                }
            }
        }
    }

    private func updateSelectedPreviewBadge(for result: FfiSearchResult?) {
        guard let result else {
            selectedPreviewPath = nil
            selectedPreviewIconButton.image = nil
            selectedPreviewIconButton.isHidden = true
            return
        }
        let offline = isResultOffline(result)
        selectedPreviewPath = result.path
        selectedPreviewIconButton.image = iconForFile(
            at: result.path,
            isDir: result.isDir,
            isOffline: offline,
            fileExtension: result.extension
        )
        selectedPreviewIconButton.alphaValue = offline ? 0.55 : 1.0
        selectedPreviewIconButton.toolTip = result.name
        selectedPreviewIconButton.isHidden = false

        guard !offline,
              !engine.isPathOffline(result.path),
              !SettingsPathStatusResolver.isUnderVolumesNamespace(result.path),
              SearchResultsViewController.needsCustomBundleIcon(path: result.path),
              iconCache.object(forKey: result.path as NSString) == nil else {
            return
        }
        let targetPath = result.path
        iconLoadQueue.async { [weak self] in
            let loaded = NSWorkspace.shared.icon(forFile: targetPath)
            DispatchQueue.main.async {
                guard let self else { return }
                self.iconCache.setObject(loaded, forKey: targetPath as NSString)
                if self.selectedPreviewPath == targetPath {
                    self.selectedPreviewIconButton.image = loaded
                }
            }
        }
    }

    private func configureMessageView(for response: FfiSearchResponse) {
        if let error = response.error {
            emptyStateView.apply(.init(
                symbolName: "exclamationmark.triangle",
                title: L("search.error.syntax"),
                subtitle: error,
                actionTitle: nil,
                onAction: nil
            ))
            return
        }

        if let hint = response.hint {
            emptyStateView.apply(.init(
                symbolName: "folder.badge.minus",
                title: L("search.empty.excluded.title"),
                subtitle: "\(L("search.empty.excluded.tag")): \(hint)",
                actionTitle: L("search.empty.manageRules"),
                onAction: { [weak self] in
                    guard let self else { return }
                    let pathHint = SearchWindow.extractDiagnosticPathCandidate(from: self.searchField.stringValue)
                    self.hideWindow(animated: false)
                    self.onOpenIndexPreferences?(pathHint)
                }
            ))
            return
        }

        if isIndexingActive && !engine.stats.isReady {
            emptyStateView.apply(.init(
                symbolName: "magnifyingglass",
                title: L("search.buildingIndex"),
                subtitle: L("search.scanningFiles"),
                actionTitle: nil,
                onAction: nil
            ))
            return
        }

        let currentFilterState = FilterQuerySyncCoordinator.deserialize(query: searchField.stringValue).state
        if !currentFilterState.isUnconstrained {
            emptyStateView.apply(.init(
                symbolName: "magnifyingglass",
                title: L("floatingBar.noMatches"),
                subtitle: L("floatingBar.noMatches.filteredHint"),
                actionTitle: L("floatingBar.clearFilters"),
                onAction: { [weak self] in
                    self?.inlineFilterBar.clearAllFilters()
                    self?.focusSearchField()
                }
            ))
            return
        }

        if currentMode != .fuzzy {
            emptyStateView.apply(.init(
                symbolName: "magnifyingglass",
                title: L("floatingBar.noMatches"),
                subtitle: L("floatingBar.noMatches.fuzzyHint"),
                actionTitle: L("floatingBar.enableFuzzy"),
                onAction: { [weak self] in
                    self?.toggleMode(.fuzzy)
                    self?.focusSearchField()
                }
            ))
            return
        }

        emptyStateView.apply(.init(
            symbolName: "magnifyingglass",
            title: L("floatingBar.noMatches"),
            subtitle: L("floatingBar.noMatches.genericHint"),
            actionTitle: nil,
            onAction: nil
        ))
    }

    private func showTransientNotice(_ message: String) {
        temporaryStatusWorkItem?.cancel()
        transientToastLabel.stringValue = message
        transientToastView.isHidden = false
        NSAnimationContext.runAnimationGroup { ctx in
            ctx.duration = 0.14
            transientToastView.animator().alphaValue = 1.0
        }
        let item = DispatchWorkItem { [weak self] in
            guard let self else { return }
            NSAnimationContext.runAnimationGroup({ ctx in
                ctx.duration = 0.18
                self.transientToastView.animator().alphaValue = 0.0
            }, completionHandler: {
                self.transientToastView.isHidden = true
            })
        }
        temporaryStatusWorkItem = item
        DispatchQueue.main.asyncAfter(deadline: .now() + 2.0, execute: item)
    }

    // MARK: - Actions & Keyboard Control

    @objc private func clearButtonClicked() {
        resetToCollapsedIdleState(animate: true)
        focusSearchField()
    }

    @objc private func selectedPreviewClicked() {
        openSelectedAndDismiss()
    }

    @objc private func tableViewDoubleClicked() {
        let clickedRow = tableView.clickedRow
        if clickedRow >= 0, clickedRow < results.count {
            tableView.selectRowIndexes(IndexSet(integer: clickedRow), byExtendingSelection: false)
        }
        openSelectedAndDismiss()
    }

    private var currentSelectedResult: FfiSearchResult? {
        let row = tableView.selectedRow
        guard row >= 0, row < results.count else { return nil }
        return results[row]
    }

    private func isResultOffline(_ result: FfiSearchResult) -> Bool {
        result.isOffline || engine.isPathOffline(result.path)
    }

    private func openSelectedAndDismiss() {
        guard let item = currentSelectedResult else { return }
        if isResultOffline(item) {
            showTransientNotice(L("search.offline.actionBlocked"))
            return
        }
        engine.openFileAsync(path: item.path) { [weak self] outcome in
            guard let self, !outcome.succeeded else { return }
            let message = outcome.errorMessage ?? L("search.offline.actionBlocked")
            self.showTransientNotice(message)
        }
        dismissAfterAction()
    }

    private func revealSelectedAndDismiss() {
        guard let item = currentSelectedResult else { return }
        if isResultOffline(item) {
            showTransientNotice(L("search.offline.actionBlocked"))
            return
        }
        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: item.path)])
        dismissAfterAction()
    }

    private func copySelectedPathAndDismiss() {
        guard let item = currentSelectedResult else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(item.path, forType: .string)
        dismissAfterAction()
    }

    private func openSelectedInTerminalAndDismiss() {
        guard let item = currentSelectedResult else { return }
        if isResultOffline(item) {
            showTransientNotice(L("search.offline.actionBlocked"))
            return
        }
        TerminalLauncher.openBatch(
            at: [URL(fileURLWithPath: item.path)],
            preferredIdentifier: engine.getConfig()?.general.terminalBundleId
        )
        dismissAfterAction()
    }

    private func toggleQuickLook() {
        if QLPreviewPanel.sharedPreviewPanelExists() && QLPreviewPanel.shared().isVisible {
            QLPreviewPanel.shared().orderOut(nil)
            return
        }
        guard let item = currentSelectedResult else { return }
        if isResultOffline(item) {
            showTransientNotice(L("search.offline.actionBlocked"))
            return
        }
        let panel = QLPreviewPanel.shared()
        panel?.makeKeyAndOrderFront(nil)
        panel?.reloadData()
    }

    // MARK: - Context Menu Actions

    @objc private func contextOpenItem() { openSelectedAndDismiss() }
    @objc private func contextRevealItem() { revealSelectedAndDismiss() }
    @objc private func contextOpenInTerminal() { openSelectedInTerminalAndDismiss() }
    @objc private func contextQuickLook() { toggleQuickLook() }
    @objc private func contextCopyPath() { copySelectedPathAndDismiss() }

    @objc private func contextCopyName() {
        guard let item = currentSelectedResult else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(item.name, forType: .string)
        dismissAfterAction()
    }

    // MARK: - Mode Toggles & Filter Popover

    @objc private func caseSensitivityButtonClicked() {
        switch currentCaseSensitivity {
        case .auto: currentCaseSensitivity = .sensitive
        case .sensitive: currentCaseSensitivity = .insensitive
        case .insensitive: currentCaseSensitivity = .auto
        }
        updateCaseSensitivityButtonState()
        let query = searchField.stringValue
        if !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
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
                let hasUpper = SearchWindow.hasEffectiveUppercase(query: searchField?.stringValue ?? "", mode: currentMode)
                caseSensitivityButton.visualState = hasUpper ? .autoActive : .inactive
            }
        }
        updateControlVisualOpacity()
    }

    @objc private func fuzzyModeButtonClicked() { toggleMode(.fuzzy) }
    @objc private func regexModeButtonClicked() { toggleMode(.regex) }

    private func toggleMode(_ mode: FfiSearchMode) {
        currentMode = (currentMode == mode) ? .auto : mode
        updateFuzzyModeButtonState()
        updateRegexModeButtonState()
        let query = searchField.stringValue
        if !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            performSearch(query: query)
        }
    }

    private func updateFuzzyModeButtonState() {
        fuzzyModeButton.visualState = (currentMode == .fuzzy) ? .locked : .inactive
        updateControlVisualOpacity()
    }

    private func updateRegexModeButtonState(effectiveMode: FfiSearchMode? = nil) {
        if currentMode == .regex {
            regexModeButton.visualState = .locked
        } else if let effective = effectiveMode {
            regexModeButton.visualState = (effective == .regex) ? .autoActive : .inactive
        } else {
            let hasSugar = SearchWindow.hasRegexSugar(query: searchField?.stringValue ?? "")
            regexModeButton.visualState = hasSugar ? .autoActive : .inactive
        }
        updateControlVisualOpacity()
    }

    @objc private func filterButtonClicked() {
        toggleFilterBar()
    }

    private func toggleFilterBar() {
        isFilterBarExpanded.toggle()
        if isFilterBarExpanded {
            let state = FilterQuerySyncCoordinator.deserialize(query: searchField.stringValue).state
            inlineFilterBar.update(state: state)
        }
        applyExpansionState(expansionState, animate: true)
        focusSearchField()
    }

    private func updateFilterButtonState(forQuery query: String) {
        let (_, state) = FilterQuerySyncCoordinator.deserialize(query: query)
        updateFilterButton(state: state)
    }

    private func updateFilterButton(state: FilterState) {
        let count = state.activeCount
        filterButton.activeCount = count
        let config = NSImage.SymbolConfiguration(pointSize: 12, weight: .medium)
        filterButton.image = NSImage(systemSymbolName: "line.3.horizontal.decrease", accessibilityDescription: L("filter.title"))?
            .withSymbolConfiguration(config)
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
        updateControlVisualOpacity()
    }

    // MARK: - Recent Searches Menu

    @objc private func showRecentSearchesMenu(_ sender: NSButton) {
        let menu = NSMenu(title: L("search.history.title"))
        let header = NSMenuItem(title: L("search.history.title"), action: nil, keyEquivalent: "")
        header.isEnabled = false
        menu.addItem(header)

        let history = recentSearches
        if history.isEmpty {
            let emptyItem = NSMenuItem(title: L("search.history.empty"), action: nil, keyEquivalent: "")
            emptyItem.isEnabled = false
            menu.addItem(emptyItem)
        } else {
            let iconConfig = NSImage.SymbolConfiguration(pointSize: 12, weight: .regular)
            let clockIcon = NSImage(systemSymbolName: "clock", accessibilityDescription: nil)?.withSymbolConfiguration(iconConfig)
            clockIcon?.isTemplate = true
            for query in history {
                let item = NSMenuItem(
                    title: SearchWindow.truncatedHistoryDisplayTitle(query),
                    action: #selector(recentSearchSelected(_:)),
                    keyEquivalent: ""
                )
                item.target = self
                item.representedObject = query
                item.toolTip = query
                item.image = clockIcon
                menu.addItem(item)
            }
            menu.addItem(NSMenuItem.separator())
            let clearItem = NSMenuItem(title: L("search.history.clear"), action: #selector(clearRecentSearches), keyEquivalent: "")
            clearItem.target = self
            menu.addItem(clearItem)
        }

        let location = NSPoint(x: 0, y: sender.bounds.height + 6)
        menu.popUp(positioning: nil, at: location, in: sender)
    }

    @objc private func recentSearchSelected(_ sender: NSMenuItem) {
        guard let query = sender.representedObject as? String else { return }
        searchField.stringValue = query
        lastQuery = query
        clearButton.isHidden = query.isEmpty
        applyQuerySyntaxHighlighting()
        updateCaseSensitivityButtonState()
        updateRegexModeButtonState()
        focusSearchField()
        evaluateQueryAndSearch(query: query)
    }

    @objc private func clearRecentSearches() {
        recentSearches = []
    }

    private func recordCurrentSearchQueryIfNeeded() {
        let trimmed = searchField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !results.isEmpty, trimmed.count >= 2 else { return }
        let lowerTrimmed = trimmed.lowercased()
        let current = recentSearches
        let updated = Array(([trimmed] + current.filter { !lowerTrimmed.hasPrefix($0.lowercased()) }).prefix(SearchWindow.maxRecentSearches))
        guard updated != current else { return }
        recentSearches = updated
    }

    // MARK: - Syntax Highlighting & Escape / Focus

    private func applyQuerySyntaxHighlighting() {
        guard let fieldEditor = searchField.currentEditor() as? NSTextView,
              !fieldEditor.hasMarkedText(),
              let storage = fieldEditor.textStorage else {
            return
        }
        let query = fieldEditor.string
        let hasFilterSyntax = FilterQuerySyncCoordinator.containsFilterSyntax(query)
        if !hasFilterSyntax && !hasAppliedFilterSyntaxHighlighting {
            return
        }
        PillLayoutManager.ensureInstalled(on: fieldEditor)
        let baseFont = searchField.font ?? .systemFont(ofSize: 20)
        let formatted = FilterQuerySyncCoordinator.formatQuerySyntax(query, baseFont: baseFont)
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
        fieldEditor.typingAttributes = [
            .font: baseFont,
            .foregroundColor: NSColor.labelColor,
        ]
    }

    private func handleEscape() {
        if QLPreviewPanel.sharedPreviewPanelExists() && QLPreviewPanel.shared().isVisible {
            QLPreviewPanel.shared().orderOut(nil)
            return
        }
        if isFilterBarExpanded {
            isFilterBarExpanded = false
            applyExpansionState(expansionState, animate: true)
            focusSearchField()
            return
        }
        let filterState = FilterQuerySyncCoordinator.deserialize(query: searchField.stringValue).state
        if !searchField.stringValue.isEmpty || !filterState.isUnconstrained {
            resetToCollapsedIdleState(animate: true)
            focusSearchField()
            return
        }
        hideWindow(animated: true)
    }

    func focusSearchField(selectAll: Bool = false) {
        guard let window, let searchField else { return }
        window.makeFirstResponder(searchField)
        if let editor = searchField.currentEditor() as? NSTextView {
            let length = (searchField.stringValue as NSString).length
            let targetRange = selectAll ? NSRange(location: 0, length: length) : NSRange(location: length, length: 0)
            editor.selectedRange = targetRange
            editor.scrollRangeToVisible(targetRange)
        }
        applyQuerySyntaxHighlighting()
    }

    // MARK: - Safe Icon Loading

    private func iconForFile(at path: String, isDir: Bool, isOffline: Bool, fileExtension: String?) -> NSImage {
        if !isOffline && !engine.isPathOffline(path),
           !SettingsPathStatusResolver.isUnderVolumesNamespace(path),
           SearchResultsViewController.needsCustomBundleIcon(path: path),
           let cached = iconCache.object(forKey: path as NSString) {
            return cached
        }
        return nonBlockingTypeIcon(for: path, isDir: isDir, fileExtension: fileExtension)
    }

    private func nonBlockingTypeIcon(for path: String, isDir: Bool, fileExtension: String?) -> NSImage {
        let isBundleDir = isDir && (SearchResultsViewController.needsCustomBundleIcon(path: path) || SearchResultsViewController.isPackageBundle(path: path))
        if isDir && !isBundleDir {
            let key = "__dir__"
            if let cached = typeIconCache[key] { return cached }
            let icon = NSWorkspace.shared.icon(for: .folder)
            typeIconCache[key] = icon
            return icon
        }
        let ext = (fileExtension?.isEmpty == false ? fileExtension!.lowercased() : (path as NSString).pathExtension.lowercased())
        let key = isBundleDir ? "__pkg_\(ext)" : "__ext_\(ext)"
        if let cached = typeIconCache[key] { return cached }
        let utType = SearchResultsViewController.resolvedUTType(forExtension: ext, isBundleDir: isBundleDir)
        let icon = NSWorkspace.shared.icon(for: utType)
        typeIconCache[key] = icon
        return icon
    }

    private func loadCustomBundleIconIfNeeded(for result: FfiSearchResult, isOffline: Bool, into cell: FloatingSearchResultCellView) {
        let path = result.path
        cell.representedPath = path
        guard !isOffline,
              !engine.isPathOffline(path),
              !SettingsPathStatusResolver.isUnderVolumesNamespace(path),
              SearchResultsViewController.needsCustomBundleIcon(path: path),
              iconCache.object(forKey: path as NSString) == nil else {
            return
        }
        iconLoadQueue.async { [weak self, weak cell] in
            let loaded = NSWorkspace.shared.icon(forFile: path)
            DispatchQueue.main.async {
                guard let self else { return }
                self.iconCache.setObject(loaded, forKey: path as NSString)
                if let cell, cell.representedPath == path {
                    cell.iconView.image = loaded
                }
            }
        }
    }
}

// MARK: - NSWindowDelegate

extension FloatingSearchWindowController: NSWindowDelegate {
    func windowDidMove(_ notification: Notification) {
        guard !isApplyingProgrammaticFrame,
              shouldRememberWindowPosition,
              let panel = window,
              panel.isVisible else {
            return
        }
        positionStore.saveTopLeft(NSPoint(x: panel.frame.minX, y: panel.frame.maxY))
    }
}

// MARK: - NSSearchFieldDelegate

extension FloatingSearchWindowController: NSSearchFieldDelegate {
    func controlTextDidChange(_ obj: Notification) {
        applyQuerySyntaxHighlighting()
        updateCaseSensitivityButtonState()
        updateRegexModeButtonState()
        searchChanged()
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy commandSelector: Selector) -> Bool {
        if commandSelector == #selector(NSResponder.insertNewline(_:)) {
            if textView.hasMarkedText() {
                return false
            }
            if NSApp.currentEvent?.modifierFlags.contains(.command) == true {
                revealSelectedAndDismiss()
            } else {
                openSelectedAndDismiss()
            }
            return true
        }

        if commandSelector == #selector(NSResponder.cancelOperation(_:)) {
            if textView.hasMarkedText() {
                return false
            }
            handleEscape()
            return true
        }

        if commandSelector == #selector(NSResponder.moveDown(_:)) {
            guard !results.isEmpty else { return false }
            let currentRow = tableView.selectedRow
            let nextRow = currentRow < 0 ? 0 : min(currentRow + 1, results.count - 1)
            tableView.selectRowIndexes(IndexSet(integer: nextRow), byExtendingSelection: false)
            tableView.scrollRowToVisible(nextRow)
            snapScrollToRowBoundary(animated: false)
            return true
        }

        if commandSelector == #selector(NSResponder.moveUp(_:)) {
            if (textView.string as NSString).length == 0, let last = recentSearches.first {
                searchField.stringValue = last
                lastQuery = last
                clearButton.isHidden = false
                focusSearchField()
                evaluateQueryAndSearch(query: last)
                return true
            }
            guard !results.isEmpty else { return false }
            let currentRow = tableView.selectedRow
            let prevRow = max(currentRow - 1, 0)
            tableView.selectRowIndexes(IndexSet(integer: prevRow), byExtendingSelection: false)
            tableView.scrollRowToVisible(prevRow)
            snapScrollToRowBoundary(animated: false)
            return true
        }

        return false
    }
}

// MARK: - NSTableViewDataSource & NSTableViewDelegate

extension FloatingSearchWindowController: NSTableViewDataSource, NSTableViewDelegate {
    func numberOfRows(in tableView: NSTableView) -> Int {
        results.count
    }

    func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
        FloatingGlassRowView()
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard row >= 0, row < results.count else { return nil }
        let result = results[row]
        let offline = isResultOffline(result)
        let icon = iconForFile(at: result.path, isDir: result.isDir, isOffline: offline, fileExtension: result.extension)

        let cell = (tableView.makeView(withIdentifier: FloatingSearchResultCellView.reuseIdentifier, owner: nil) as? FloatingSearchResultCellView)
            ?? {
                let created = FloatingSearchResultCellView()
                created.identifier = FloatingSearchResultCellView.reuseIdentifier
                return created
            }()

        cell.configure(result: result, icon: icon, isOffline: offline)
        cell.setRowSelected(tableView.selectedRow == row)
        loadCustomBundleIconIfNeeded(for: result, isOffline: offline, into: cell)
        return cell
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        updateSelectedPreviewBadge(for: currentSelectedResult)
        if QLPreviewPanel.sharedPreviewPanelExists() && QLPreviewPanel.shared().isVisible {
            QLPreviewPanel.shared().reloadData()
        }
    }

    func tableView(_ tableView: NSTableView, pasteboardWriterForRow row: Int) -> NSPasteboardWriting? {
        guard row >= 0, row < results.count else { return nil }
        let result = results[row]
        if isResultOffline(result) {
            return nil
        }
        return NSURL(fileURLWithPath: result.path)
    }

    func tableView(_ tableView: NSTableView, draggingSession session: NSDraggingSession, willBeginAt screenPoint: NSPoint, forRowIndexes rowIndexes: IndexSet) {
        isDraggingOut = true
        recordCurrentSearchQueryIfNeeded()
    }

    func tableView(_ tableView: NSTableView, draggingSession session: NSDraggingSession, endedAt screenPoint: NSPoint, operation: NSDragOperation) {
        isDraggingOut = false
    }
}

// MARK: - NSMenuItemValidation

extension FloatingSearchWindowController: NSMenuItemValidation {
    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        if menuItem.action == #selector(switchToStandardWindow) {
            return true
        }
        guard let item = currentSelectedResult else { return false }
        switch menuItem.action {
        case #selector(contextOpenItem),
             #selector(contextRevealItem),
             #selector(contextOpenInTerminal),
             #selector(contextQuickLook):
            return !isResultOffline(item)
        case #selector(contextCopyPath),
             #selector(contextCopyName):
            return true
        default:
            return true
        }
    }
}

// MARK: - Quick Look Integration

extension FloatingSearchWindowController: FloatingSearchPreviewPanelHandling, QLPreviewPanelDataSource, QLPreviewPanelDelegate {
    func floatingPanel(_ panel: NSPanel, acceptsPreviewPanelControl previewPanel: QLPreviewPanel!) -> Bool {
        true
    }

    func floatingPanel(_ panel: NSPanel, beginPreviewPanelControl previewPanel: QLPreviewPanel!) {
        previewPanel.dataSource = self
        previewPanel.delegate = self
    }

    func floatingPanel(_ panel: NSPanel, endPreviewPanelControl previewPanel: QLPreviewPanel!) {
        previewPanel.dataSource = nil
        previewPanel.delegate = nil
    }

    func numberOfPreviewItems(in panel: QLPreviewPanel!) -> Int {
        guard let item = currentSelectedResult, !isResultOffline(item) else { return 0 }
        return 1
    }

    func previewPanel(_ panel: QLPreviewPanel!, previewItemAt index: Int) -> (any QLPreviewItem)! {
        guard let item = currentSelectedResult, !isResultOffline(item) else { return nil }
        return NSURL(fileURLWithPath: item.path)
    }

    func previewPanel(_ panel: QLPreviewPanel!, sourceFrameOnScreenFor item: (any QLPreviewItem)!) -> NSRect {
        let row = tableView.selectedRow
        guard row >= 0, row < results.count else { return .zero }
        let rowRect = tableView.rect(ofRow: row)
        let windowRect = tableView.convert(rowRect, to: nil)
        return window?.convertToScreen(windowRect) ?? .zero
    }
}
