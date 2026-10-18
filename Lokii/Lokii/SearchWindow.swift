import AppKit
import UniformTypeIdentifiers
import Quartz

/// Blocks all mouse and keyboard events so the UI beneath is not interactive.
private final class EventBlockingView: NSView {
    override var acceptsFirstResponder: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
    override func hitTest(_ point: NSPoint) -> NSView? { self }
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

    /// 基于当前主屏幕可用工作区几何尺寸，按黄金视觉比例自适应计算默认尺寸与居中位置
    static func calculateDefaultWindowFrame() -> NSRect {
        guard let screen = NSScreen.main ?? NSScreen.screens.first else {
            return NSRect(x: 0, y: 0, width: 960, height: 640)
        }
        let visible = screen.visibleFrame
        // 宽度取可见屏幕宽度的 60%，钳制在 [960, 1280] pt
        let width = min(max(visible.width * 0.60, 960), 1280)
        // 高度取可见屏幕高度的 62%，钳制在 [600, 860] pt
        let height = min(max(visible.height * 0.62, 600), 860)

        // 水平居中，垂直方向位于视窗黄金中心（屏幕中心微偏上 5% 屏幕高度）
        let x = visible.origin.x + (visible.width - width) / 2
        let y = visible.origin.y + (visible.height - height) / 2 + (visible.height * 0.05)
        return NSRect(x: round(x), y: round(y), width: round(width), height: round(height))
    }

    private let savedWindowFrameKey = "LokiiMainWindowFrame"
    let splitViewController = SearchMainSplitViewController()
    var searchResultsViewController: SearchResultsViewController { splitViewController.searchResultsViewController }
    var sidebarTreeViewController: SidebarTreeViewController { splitViewController.sidebarTreeViewController }

    private var searchField: NSSearchField!
    private let searchContainer = NSView()
    private var searchWorkItem: DispatchWorkItem?
    private var currentMode: FfiSearchMode = .auto
    private var statusText: LocalizedText = .key("search.indexing") {
        didSet { updateStatusLabel() }
    }
    private var searchMatchText: LocalizedText? = nil {
        didSet { updateStatusLabel() }
    }
    private var overlaySubtext: LocalizedText = .key("search.scanningFiles") {
        didSet { overlaySubtextLabel?.stringValue = overlaySubtext.string }
    }
    private weak var refreshToastLabel: NSTextField?

    // Loading overlay
    private var loadingOverlay: NSView?
    private var overlayHeadingLabel: NSTextField?
    private var overlaySubtextLabel: NSTextField?
    private var overlayShownAt: Date?
    private var isColdStart: Bool = true  // Default true until startup mode known
    private var overlayProgressIndicator: NSProgressIndicator?
    private var isInlineProgressActive: Bool = false
    private var originalSearchPlaceholder: String { L("search.placeholder") }

    /// Tracks why the loading overlay is showing, so `onIndexReady` from
    /// a concurrent initialize/verify doesn't accidentally dismiss a rebuild overlay.
    private enum IndexingState { case idle, initializing, rebuilding, failed }
    private var indexingState: IndexingState = .initializing

    // Shared formatters
    private static let fullDateFormatter: DateFormatter = {
        let f = DateFormatter()
        f.dateStyle = .long
        f.timeStyle = .medium
        return f
    }()
    private static let countFormatter: NumberFormatter = {
        let f = NumberFormatter()
        f.numberStyle = .decimal
        return f
    }()

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
            self?.hideWindowWithAnimation()
        }
        window.onReturn = { [weak self] in
            self?.openSelected()
        }
        window.onCommandF = { [weak self] in
            self?.focusSearchField()
        }

        setupUI()
        applyInitialWindowFrame()
        window.delegate = self
        window.contentView?.wantsLayer = true
        setupSearchModeMenu()
        setupNotifications()
        onLanguageChanged()

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
        searchField.focusRingType = .exterior
        searchField.delegate = self
        searchField.sendsSearchStringImmediately = false
        searchField.sendsWholeSearchString = false

        searchContainer.addSubview(searchField)

        NSLayoutConstraint.activate([
            searchContainer.heightAnchor.constraint(equalToConstant: 28),
            searchContainer.widthAnchor.constraint(greaterThanOrEqualToConstant: 320),

            searchField.widthAnchor.constraint(greaterThanOrEqualToConstant: 320),
            searchField.leadingAnchor.constraint(equalTo: searchContainer.leadingAnchor),
            searchField.trailingAnchor.constraint(equalTo: searchContainer.trailingAnchor),
            searchField.centerYAnchor.constraint(equalTo: searchContainer.centerYAnchor),
            searchField.heightAnchor.constraint(equalToConstant: 28),
        ])

        let toolbar = NSToolbar(identifier: "LokiiMainToolbar")
        toolbar.delegate = self
        toolbar.displayMode = .iconOnly
        window?.toolbar = toolbar
    }

    // MARK: - Panel Collapse Actions

    @objc func toggleSidebar(_ sender: Any? = nil) {
        splitViewController.toggleSidebar()
    }

    // MARK: - Search Mode Menu (on the magnifying glass)

    private func setupSearchModeMenu() {
        let menu = NSMenu(title: L("search.mode.title"))
        let keys = [
            "search.mode.auto",
            "search.mode.substring",
            "search.mode.wildcard",
            "search.mode.regex",
            "search.mode.fuzzy",
        ]
        for (i, key) in keys.enumerated() {
            let item = NSMenuItem(localizedKey: key, action: #selector(searchModeSelected(_:)), keyEquivalent: "")
            item.target = self
            item.tag = i
            if i == 0 { item.state = .on }
            menu.addItem(item)
        }
        (searchField.cell as? NSSearchFieldCell)?.searchMenuTemplate = menu

        // D-04: Optimize magnifying glass icon for visual symmetry
        if let searchButtonCell = (searchField.cell as? NSSearchFieldCell)?.searchButtonCell {
            let config = NSImage.SymbolConfiguration(pointSize: 14, weight: .regular)
            searchButtonCell.image = NSImage(systemSymbolName: "magnifyingglass", accessibilityDescription: "Search")?.withSymbolConfiguration(config)
        }
    }

    @objc private func searchModeSelected(_ sender: NSMenuItem) {
        let modes: [FfiSearchMode] = [.auto, .substring, .wildcard, .regex, .fuzzy]
        let tag = sender.tag
        guard tag >= 0, tag < modes.count else { return }
        currentMode = modes[tag]
        if let menu = sender.menu {
            for item in menu.items { item.state = .off }
        }
        sender.state = .on
        if let query = searchField?.stringValue, !query.isEmpty {
            performSearch(query: query)
        }
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
    }

    @objc private func onLanguageChanged() {
        Self.fullDateFormatter.locale = L10n.locale
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
                } else if item.itemIdentifier == .resetWindowSize {
                    item.label = L("window.resetSize")
                    item.paletteLabel = L("window.resetSize")
                    item.toolTip = L("window.resetSize")
                    (item.view as? NSButton)?.toolTip = L("window.resetSize")
                }
            }
        }
        splitViewController.onLanguageChanged()
        if let menu = (searchField.cell as? NSSearchFieldCell)?.searchMenuTemplate {
            menu.title = L("search.mode.title")
            menu.refreshLocalizedTitles()
        }
        updateStatusLabel()
        overlayHeadingLabel?.stringValue = L("search.buildingIndex")
        overlaySubtextLabel?.stringValue = overlaySubtext.string
        refreshToastLabel?.stringValue = L("search.backgroundRefresh")
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
        let formatted = formatCount(count)

        if isColdStart && indexingState == .initializing {
            // Cold start: update overlay
            overlaySubtext = .formatted("search.scannedCount", formatted)
            statusText = .formatted("search.indexingCount", formatted)
        } else {
            // Hot start or rebuild: update inline status bar
            statusText = .formatted("search.indexingCount", formatted)
        }
    }

    @objc private func onIndexReady(_ note: Notification) {
        let total = note.userInfo?["total"] as? UInt64 ?? 0
        let source = note.userInfo?["source"] as? String ?? ""

        switch indexingState {
        case .failed:
            return
        case .initializing:
            guard source != "rebuild" else {
                statusText = .formatted("search.indexedCount", formatCount(total))
                return
            }
            statusText = .formatted("search.indexedCount", formatCount(total))
            if isColdStart {
                hideLoadingOverlay()
            } else {
                hideInlineProgress()
            }
            searchResultsViewController.setRebuildEnabled(true)
            searchResultsViewController.setRebuildSpinning(false)
            indexingState = .idle
            if searchField.stringValue.isEmpty {
                loadDefaultResults()
            }
        case .rebuilding:
            guard source == "rebuild" else {
                statusText = .formatted("search.indexedCount", formatCount(total))
                return
            }
            statusText = .formatted("search.indexedCount", formatCount(total))
            hideInlineProgress()
            searchResultsViewController.setRebuildSpinning(false)
            searchResultsViewController.setRebuildEnabled(true)
            indexingState = .idle
            if searchField.stringValue.isEmpty {
                loadDefaultResults()
            }
        case .idle:
            statusText = .formatted("search.indexedCount", formatCount(total))
        }
    }

    @objc private func onIndexUpdated(_ note: Notification) {
        guard indexingState != .failed else { return }
        let total = note.userInfo?["total"] as? UInt64 ?? 0
        statusText = .formatted("search.indexedCount", formatCount(total))
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
        overlayHeadingLabel = nil
        overlaySubtextLabel = nil
        overlayProgressIndicator?.stopAnimation(nil)
        overlayProgressIndicator = nil
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
        applyWindowBehaviorFromConfig()
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

    private var shouldRememberWindowPosition: Bool {
        engine.getConfig()?.window.rememberPosition ?? false
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
            overlayHeadingLabel?.stringValue = heading
            return
        }
        overlayShownAt = Date()
        overlaySubtext = .key("search.scanningFiles")
        guard let contentView = window?.contentView else { return }

        // Full-window event-blocking overlay
        let overlay = EventBlockingView(frame: contentView.bounds)
        overlay.autoresizingMask = [.width, .height]
        overlay.wantsLayer = true
        overlay.alphaValue = 0
        contentView.addSubview(overlay)
        loadingOverlay = overlay

        // Frosted glass background (per D-02: full-window blur)
        let blurView = NSVisualEffectView(frame: overlay.bounds)
        blurView.autoresizingMask = [.width, .height]
        blurView.material = .underWindowBackground
        blurView.blendingMode = .behindWindow
        blurView.state = .active
        overlay.addSubview(blurView)

        // Center card (per D-03)
        let card = NSView()
        card.translatesAutoresizingMaskIntoConstraints = false
        overlay.addSubview(card)

        // Magnifying glass icon — STATIC, no pulse animation (per D-03)
        let iconConfig = NSImage.SymbolConfiguration(pointSize: 40, weight: .light)
        let iconImage = NSImage(systemSymbolName: "magnifyingglass", accessibilityDescription: "Indexing")!
            .withSymbolConfiguration(iconConfig)!
        let iconView = NSImageView(image: iconImage)
        iconView.translatesAutoresizingMaskIntoConstraints = false
        iconView.contentTintColor = .secondaryLabelColor
        card.addSubview(iconView)

        // Indeterminate progress bar (per D-04)
        let progressBar = NSProgressIndicator()
        progressBar.translatesAutoresizingMaskIntoConstraints = false
        progressBar.style = .bar
        progressBar.isIndeterminate = true
        progressBar.startAnimation(nil)
        card.addSubview(progressBar)
        overlayProgressIndicator = progressBar

        // Heading label: "Building Index..."
        let headingLabel = NSTextField(labelWithString: heading)
        headingLabel.translatesAutoresizingMaskIntoConstraints = false
        headingLabel.font = .systemFont(ofSize: 16, weight: .semibold)
        headingLabel.textColor = .labelColor
        headingLabel.alignment = .center
        card.addSubview(headingLabel)
        overlayHeadingLabel = headingLabel

        // Subtext: live file count
        let subtextLabel = NSTextField(labelWithString: L("search.scanningFiles"))
        subtextLabel.translatesAutoresizingMaskIntoConstraints = false
        subtextLabel.font = .systemFont(ofSize: 13, weight: .regular)
        subtextLabel.textColor = .secondaryLabelColor
        subtextLabel.alignment = .center
        card.addSubview(subtextLabel)
        overlaySubtextLabel = subtextLabel

        // Layout
        NSLayoutConstraint.activate([
            card.centerXAnchor.constraint(equalTo: overlay.centerXAnchor),
            card.centerYAnchor.constraint(equalTo: overlay.centerYAnchor),
            card.widthAnchor.constraint(equalToConstant: 260),

            iconView.topAnchor.constraint(equalTo: card.topAnchor, constant: 24),
            iconView.centerXAnchor.constraint(equalTo: card.centerXAnchor),
            iconView.widthAnchor.constraint(equalToConstant: 48),
            iconView.heightAnchor.constraint(equalToConstant: 48),

            progressBar.topAnchor.constraint(equalTo: iconView.bottomAnchor, constant: 20),
            progressBar.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 24),
            progressBar.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -24),

            headingLabel.topAnchor.constraint(equalTo: progressBar.bottomAnchor, constant: 16),
            headingLabel.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 16),
            headingLabel.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -16),

            subtextLabel.topAnchor.constraint(equalTo: headingLabel.bottomAnchor, constant: 4),
            subtextLabel.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 16),
            subtextLabel.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -16),
            subtextLabel.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -24),
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
            self?.overlayHeadingLabel = nil
            self?.overlaySubtextLabel = nil
            self?.overlayProgressIndicator = nil
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

    // MARK: - Toast

    private func showBackgroundRefreshToast() {
        guard let contentView = window?.contentView else { return }

        // Card — same material as loading overlay for visual consistency
        let card = NSVisualEffectView()
        card.translatesAutoresizingMaskIntoConstraints = false
        card.material = .hudWindow
        card.state = .active
        card.wantsLayer = true
        card.layer?.cornerRadius = 12
        card.alphaValue = 0
        contentView.addSubview(card)

        // Spinning refresh icon — wrapped in a stable container
        let iconContainer = SearchResultsCenterSpinContainerView()
        iconContainer.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(iconContainer)

        let iconConfig = NSImage.SymbolConfiguration(pointSize: 32, weight: .light)
        let iconView = NSImageView(image: NSImage(systemSymbolName: "arrow.triangle.2.circlepath",
                                                   accessibilityDescription: "Refreshing")!
            .withSymbolConfiguration(iconConfig)!)
        iconView.frame = NSRect(x: 0, y: 0, width: 40, height: 40)
        iconView.contentTintColor = .secondaryLabelColor
        iconView.wantsLayer = true
        iconContainer.addSubview(iconView)

        // Heading — same font as loading overlay
        let label = NSTextField(labelWithString: L("search.backgroundRefresh"))
        refreshToastLabel = label
        label.translatesAutoresizingMaskIntoConstraints = false
        label.font = .systemFont(ofSize: 16, weight: .semibold)
        label.textColor = .labelColor
        label.alignment = .center
        label.lineBreakMode = .byWordWrapping
        label.maximumNumberOfLines = 2
        card.addSubview(label)

        // Layout — matches loading overlay card proportions
        NSLayoutConstraint.activate([
            card.centerXAnchor.constraint(equalTo: contentView.centerXAnchor),
            card.centerYAnchor.constraint(equalTo: contentView.centerYAnchor),
            card.widthAnchor.constraint(equalToConstant: 200),

            iconContainer.topAnchor.constraint(equalTo: card.topAnchor, constant: 24),
            iconContainer.centerXAnchor.constraint(equalTo: card.centerXAnchor),
            iconContainer.widthAnchor.constraint(equalToConstant: 40),
            iconContainer.heightAnchor.constraint(equalToConstant: 40),

            label.topAnchor.constraint(equalTo: iconContainer.bottomAnchor, constant: 16),
            label.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 16),
            label.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -16),
            label.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -24),
        ])

        // Spin animation (must be linear to avoid stuttering)
        iconView.layer?.addInfiniteSpin()

        // Fade in + Spring Scale for Apple native pop effect
        card.layer?.transform = CATransform3DMakeScale(0.8, 0.8, 1.0)
        
        NSAnimationContext.runAnimationGroup { ctx in
            ctx.duration = 0.15
            ctx.timingFunction = CAMediaTimingFunction(name: .easeOut)
            card.animator().alphaValue = 1.0
            
            let spring = CASpringAnimation(keyPath: "transform")
            spring.fromValue = CATransform3DMakeScale(0.8, 0.8, 1.0)
            spring.toValue = CATransform3DIdentity
            spring.damping = 15
            spring.stiffness = 300
            spring.duration = spring.settlingDuration
            card.layer?.transform = CATransform3DIdentity
            card.layer?.add(spring, forKey: "pop")
        }

        // Auto dismiss after 2 seconds
        DispatchQueue.main.asyncAfter(deadline: .now() + 2.0) {
            NSAnimationContext.runAnimationGroup({ ctx in
                ctx.duration = 0.2
                ctx.allowsImplicitAnimation = true
                ctx.timingFunction = CAMediaTimingFunction(name: .easeIn)
                card.animator().alphaValue = 0.0
                card.layer?.transform = CATransform3DMakeScale(0.9, 0.9, 1.0)
            }, completionHandler: {
                card.removeFromSuperview()
            })
        }
    }

    // MARK: - Rebuild Action

    @objc private func rebuildIndexClicked() {
        guard indexingState == .idle else { return }

        if engine.isScanning {
            showBackgroundRefreshToast()
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
        let query = searchField.stringValue
        guard query != lastQuery else { return }
        lastQuery = query
        searchWorkItem?.cancel()

        if query.isEmpty {
            searchResultsViewController.updateBottomPath(with: nil)
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
            // 按文件名自然字母顺序（A-Z）排列，并清除匹配范围（默认未检索状态下不施加高亮）
            let items: [FfiSearchResult] = response.results
                .sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
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
                guard self.searchField.stringValue.isEmpty else { return }
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
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            guard let self else { return }
            let response = self.engine.search(query: query, mode: mode)
            let tree: ResultDirectoryTree? = response.results.isEmpty ? nil : ResultDirectoryTree.build(from: response.results)
            DispatchQueue.main.async {
                self.isSearching = false
                guard self.indexingState != .failed else { return }
                self.splitViewController.updateSearchResults(rawResults: response.results, tree: tree)

                if let error = response.error {
                    self.searchMatchText = .verbatim(error)
                } else {
                    let count = response.results.count
                    if count == 1 {
                        self.searchMatchText = .key("search.matchedCount.one")
                    } else {
                        self.searchMatchText = .formatted("search.matchedCount", self.formatCount(UInt64(count)))
                    }
                }

                // If user typed more while searching, trigger new search
                let current = self.searchField.stringValue
                if current != query && !current.isEmpty {
                    self.lastQuery = current
                    self.performSearch(query: current)
                }
            }
        }
    }

    // MARK: - Actions

    private var selectedResults: [FfiSearchResult] {
        searchResultsViewController.selectedResults
    }

    @objc private func openSelected() {
        let selected = selectedResults
        guard !selected.isEmpty else { return }
        for item in selected {
            engine.openFile(path: item.path)
        }
    }

    // MARK: - Focus Management

    /// 聚焦搜索输入框，并将输入光标定位至文本末尾
    func focusSearchField() {
        guard let window = window, let searchField = searchField else { return }
        window.makeFirstResponder(searchField)
        if let editor = searchField.currentEditor() {
            let length = (searchField.stringValue as NSString).length
            editor.selectedRange = NSRange(location: length, length: 0)
        }
    }

    // MARK: - Helpers

    private func formatCount(_ n: UInt64) -> String {
        Self.countFormatter.string(from: NSNumber(value: n)) ?? "\(n)"
    }
}

// MARK: - SearchMainSplitViewControllerDelegate

extension SearchWindow: SearchMainSplitViewControllerDelegate {
    func searchMainSplitViewDidSelect(result: FfiSearchResult?) {
        searchResultsViewController.updateBottomPath(with: result?.path)
    }

    func searchMainSplitViewDidDoubleClick(result: FfiSearchResult) {
        engine.openFile(path: result.path)
    }

    func searchMainSplitViewDidRequestRebuild() {
        rebuildIndexClicked()
    }

    func searchMainSplitViewDidRequestFocusSearchField() {
        focusSearchField()
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
        searchChanged()
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy commandSelector: Selector) -> Bool {
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
    static let resetWindowSize = NSToolbarItem.Identifier("lokii.toolbar.resetWindowSize")
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

        case .resetWindowSize:
            let button = NSButton()
            button.image = NSImage(systemSymbolName: "arrow.down.right.and.arrow.up.left", accessibilityDescription: L("window.resetSize"))
            button.bezelStyle = .toolbar
            button.isBordered = false
            button.toolTip = L("window.resetSize")
            button.contentTintColor = .secondaryLabelColor
            button.target = self
            button.action = #selector(resetToDefaultWindowFrame)

            let item = NSToolbarItem(itemIdentifier: itemIdentifier)
            item.view = button
            item.label = L("window.resetSize")
            item.paletteLabel = L("window.resetSize")
            item.toolTip = L("window.resetSize")
            return item

        default:
            return nil
        }
    }

    func toolbarDefaultItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [.toggleSidebar, .sidebarTrackingSeparator, .searchField, .resetWindowSize]
    }

    func toolbarAllowedItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [.toggleSidebar, .sidebarTrackingSeparator, .searchField, .resetWindowSize]
    }
}

