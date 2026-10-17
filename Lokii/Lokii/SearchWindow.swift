import AppKit
import UniformTypeIdentifiers

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

/// Container view that perfectly stabilizes rotation for a subview
/// to prevent AppKit's frame syncing from breaking the anchor point.
private final class CenterSpinContainerView: NSView {
    override func layout() {
        super.layout()
        guard let subview = subviews.first else { return }
        subview.frame = bounds
        if let layer = subview.layer {
            layer.anchorPoint = CGPoint(x: 0.5, y: 0.5)
            layer.position = CGPoint(x: bounds.midX, y: bounds.midY)
        }
    }
}

/// 搜索结果虚拟列表：右键联动选区，非激活状态下点击即选中。
private final class SearchTableView: NSTableView {
    override func menu(for event: NSEvent) -> NSMenu? {
        let point = convert(event.locationInWindow, from: nil)
        let row = self.row(at: point)

        // 空白区域不弹菜单；row(at:) 对非行区域返回 -1，非负即已排除越界
        guard row >= 0 else {
            return nil
        }

        // 命中行不在选区内时收拢为单行；已在选区内则保持多选集合，避免误清空
        if !selectedRowIndexes.contains(row) {
            selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        }

        return super.menu(for: event)
    }

    // 非激活状态下点击即选中，省掉一次用于激活窗口的点击
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool {
        true
    }
}

final class SearchWindow: NSWindowController {
    private let engine: SearchEngine
    private let defaultWindowFrame = NSRect(x: 0, y: 0, width: 900, height: 580)
    private let savedWindowFrameKey = "LokiiMainWindowFrame"
    private var results: [FfiSearchResult] = []
    private var searchField: NSSearchField!
    private var tableView: SearchTableView!
    private var countLabel: NSTextField!
    private var pathBar: NSPathControl!
    private var statusLabel: NSTextField!
    private var searchWorkItem: DispatchWorkItem?
    private var currentMode: FfiSearchMode = .auto
    private var statusText: LocalizedText = .key("search.indexing") {
        didSet { statusLabel?.stringValue = statusText.string }
    }
    private var resultCountText: LocalizedText = .verbatim("") {
        didSet { countLabel?.stringValue = resultCountText.string }
    }
    private var overlaySubtext: LocalizedText = .key("search.scanningFiles") {
        didSet { overlaySubtextLabel?.stringValue = overlaySubtext.string }
    }
    private weak var refreshToastLabel: NSTextField?

    // Info panel
    private var infoPanelView: NSView!
    private var infoIconView: NSImageView!
    private var infoNameLabel: NSTextField!
    private var infoKindLabel: NSTextField!
    private var infoDetailLabels: [(key: NSTextField, value: NSTextField)] = []
    private var infoPanelWidth: NSLayoutConstraint!
    private var infoPanelVisible = false
    private var toggleInfoButton: NSButton!

    // Rebuild button and loading overlay
    private var rebuildButton: NSButton!
    private var loadingOverlay: NSView?
    private var overlayHeadingLabel: NSTextField?
    private var overlaySubtextLabel: NSTextField?
    private var overlayShownAt: Date?
    private var isColdStart: Bool = true  // Default true until startup mode known
    private var overlayProgressIndicator: NSProgressIndicator?
    private var inlineProgressIndicator: NSView?
    private var originalSearchPlaceholder: String { L("search.placeholder") }

    /// Tracks why the loading overlay is showing, so `onIndexReady` from
    /// a concurrent initialize/verify doesn't accidentally dismiss a rebuild overlay.
    private enum IndexingState { case idle, initializing, rebuilding, failed }
    private var indexingState: IndexingState = .initializing

    // Main split: table scroll + info panel
    private var tableScrollView: NSScrollView!

    // Icon cache
    private var iconCache: [String: NSImage] = [:]

    // Shared formatters
    private static let dateFormatter: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "yyyy/M/d, HH:mm"
        return f
    }()
    private static let fullDateFormatter: DateFormatter = {
        let f = DateFormatter()
        f.dateStyle = .long
        f.timeStyle = .medium
        return f
    }()
    private static let sizeFormatter: ByteCountFormatter = {
        let f = ByteCountFormatter()
        f.countStyle = .file
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
            contentRect: defaultWindowFrame,
            styleMask: [.titled, .closable, .resizable, .miniaturizable],
            backing: .buffered,
            defer: false
        )
        window.titleVisibility = .hidden
        window.title = "Lokii"
        window.toolbarStyle = .unified
        window.minSize = NSSize(width: 700, height: 400)
        window.contentMinSize = NSSize(width: 700, height: 370)
        window.isReleasedWhenClosed = false

        super.init(window: window)

        window.onEscape = { [weak self] in
            self?.hideWindowWithAnimation()
        }
        window.onReturn = { [weak self] in
            self?.openSelected()
        }

        setupUI()
        applyInitialWindowFrame()
        window.delegate = self
        window.contentView?.wantsLayer = true
        setupSearchModeMenu()
        setupNotifications()
        onLanguageChanged()
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

    // MARK: - Toolbar item identifiers

    private static let searchBarItem = NSToolbarItem.Identifier("searchBar")

    // MARK: - UI Setup

    private func setupUI() {
        guard let contentView = window?.contentView,
              let layoutGuide = window?.contentLayoutGuide as? NSLayoutGuide else { return }

        // ── Toolbar with search field ──

        searchField = NSSearchField()
        searchField.placeholderString = L("search.placeholder")
        searchField.font = .systemFont(ofSize: 14)
        searchField.focusRingType = .exterior
        searchField.delegate = self
        searchField.sendsSearchStringImmediately = false
        searchField.sendsWholeSearchString = false

        countLabel = NSTextField(labelWithString: "")
        countLabel.font = .systemFont(ofSize: 12)
        countLabel.textColor = .secondaryLabelColor
        countLabel.alignment = .right
        countLabel.setContentHuggingPriority(.defaultHigh, for: .horizontal)

        // Toggle info panel button (sidebar icon)
        toggleInfoButton = NSButton(image: NSImage(systemSymbolName: "sidebar.right",
                                                   accessibilityDescription: "Toggle Info")!,
                                    target: self, action: #selector(toggleInfoPanel))
        toggleInfoButton.bezelStyle = .toolbar
        toggleInfoButton.isBordered = false
        toggleInfoButton.toolTip = L("search.toggleInfo")

        let toolbar = NSToolbar(identifier: "LokiiMainToolbar")
        toolbar.delegate = self
        toolbar.displayMode = .iconOnly
        window?.toolbar = toolbar

        // Rebuild index button — placed in bottom bar, not toolbar
        rebuildButton = NSButton(image: NSImage(systemSymbolName: "arrow.clockwise",
                                                 accessibilityDescription: "Rebuild Index")!,
                                  target: self, action: #selector(rebuildIndexClicked))
        rebuildButton.bezelStyle = .toolbar
        rebuildButton.isBordered = false
        rebuildButton.toolTip = L("search.rebuildIndex")
        rebuildButton.contentTintColor = .secondaryLabelColor

        // ── Middle area: table + info panel side by side ──
        let middleView = NSView()
        middleView.translatesAutoresizingMaskIntoConstraints = false
        middleView.wantsLayer = true
        middleView.layer?.masksToBounds = true
        contentView.addSubview(middleView)

        // Table
        tableView = SearchTableView()
        tableView.style = .fullWidth
        tableView.rowHeight = 20
        tableView.intercellSpacing = NSSize(width: 0, height: 0)
        tableView.columnAutoresizingStyle = .uniformColumnAutoresizingStyle
        tableView.allowsColumnReordering = true
        tableView.allowsColumnResizing = true
        tableView.allowsMultipleSelection = true
        tableView.allowsColumnSelection = false
        tableView.gridStyleMask = []
        tableView.selectionHighlightStyle = .regular
        tableView.usesAlternatingRowBackgroundColors = true
        tableView.dataSource = self
        tableView.delegate = self
        tableView.doubleAction = #selector(tableDoubleClicked)
        tableView.target = self

        addTableColumns()

        tableScrollView = NSScrollView()
        tableScrollView.translatesAutoresizingMaskIntoConstraints = false
        tableScrollView.documentView = tableView
        tableScrollView.hasVerticalScroller = true
        tableScrollView.hasHorizontalScroller = false
        tableScrollView.autohidesScrollers = true
        tableScrollView.borderType = .noBorder
        tableScrollView.drawsBackground = true
        middleView.addSubview(tableScrollView)

        // Info panel (right side, initially hidden)
        setupInfoPanel(in: middleView)

        // ── Bottom bar: path bar + status ──
        let bottomBar = NSView()
        bottomBar.translatesAutoresizingMaskIntoConstraints = false
        bottomBar.wantsLayer = true
        contentView.addSubview(bottomBar)

        // Separator line above bottom bar
        let separator = NSBox()
        separator.translatesAutoresizingMaskIntoConstraints = false
        separator.boxType = .separator
        contentView.addSubview(separator)

        pathBar = NSPathControl()
        pathBar.translatesAutoresizingMaskIntoConstraints = false
        pathBar.pathStyle = .standard
        pathBar.isEditable = false
        pathBar.font = .systemFont(ofSize: 11)
        pathBar.backgroundColor = .clear
        pathBar.focusRingType = .none
        pathBar.refusesFirstResponder = true
        pathBar.target = self
        pathBar.action = #selector(pathBarClicked)
        pathBar.url = nil
        pathBar.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        pathBar.setContentHuggingPriority(.defaultLow, for: .horizontal)
        bottomBar.addSubview(pathBar)

        statusLabel = NSTextField(labelWithString: L("search.indexing"))
        statusLabel.translatesAutoresizingMaskIntoConstraints = false
        statusLabel.font = .systemFont(ofSize: 11)
        statusLabel.textColor = .tertiaryLabelColor
        statusLabel.alignment = .right
        statusLabel.setContentHuggingPriority(.defaultHigh, for: .horizontal)
        bottomBar.addSubview(statusLabel)

        // Container to stabilize anchor point for rotation
        let rebuildContainer = CenterSpinContainerView()
        rebuildContainer.translatesAutoresizingMaskIntoConstraints = false
        bottomBar.addSubview(rebuildContainer)

        rebuildButton.translatesAutoresizingMaskIntoConstraints = true
        rebuildButton.frame = NSRect(x: 0, y: 0, width: 20, height: 20)
        rebuildButton.wantsLayer = true
        rebuildContainer.addSubview(rebuildButton)

        // ── Layout ──
        NSLayoutConstraint.activate([
            middleView.topAnchor.constraint(equalTo: layoutGuide.topAnchor),
            middleView.leadingAnchor.constraint(equalTo: contentView.leadingAnchor),
            middleView.trailingAnchor.constraint(equalTo: contentView.trailingAnchor),
            middleView.bottomAnchor.constraint(equalTo: separator.topAnchor),
            middleView.heightAnchor.constraint(greaterThanOrEqualToConstant: 300),

            tableScrollView.topAnchor.constraint(equalTo: middleView.topAnchor),
            tableScrollView.leadingAnchor.constraint(equalTo: middleView.leadingAnchor),
            tableScrollView.bottomAnchor.constraint(equalTo: middleView.bottomAnchor),

            separator.leadingAnchor.constraint(equalTo: contentView.leadingAnchor),
            separator.trailingAnchor.constraint(equalTo: contentView.trailingAnchor),
            separator.bottomAnchor.constraint(equalTo: bottomBar.topAnchor),

            bottomBar.leadingAnchor.constraint(equalTo: contentView.leadingAnchor),
            bottomBar.trailingAnchor.constraint(equalTo: contentView.trailingAnchor),
            bottomBar.bottomAnchor.constraint(equalTo: contentView.bottomAnchor),
            bottomBar.heightAnchor.constraint(equalToConstant: 24),

            pathBar.leadingAnchor.constraint(equalTo: bottomBar.leadingAnchor, constant: 8),
            pathBar.centerYAnchor.constraint(equalTo: bottomBar.centerYAnchor),
            pathBar.trailingAnchor.constraint(lessThanOrEqualTo: statusLabel.leadingAnchor, constant: -8),

            statusLabel.trailingAnchor.constraint(equalTo: rebuildContainer.leadingAnchor, constant: -4),
            statusLabel.centerYAnchor.constraint(equalTo: bottomBar.centerYAnchor),

            rebuildContainer.trailingAnchor.constraint(equalTo: bottomBar.trailingAnchor, constant: -8),
            rebuildContainer.centerYAnchor.constraint(equalTo: bottomBar.centerYAnchor),
            rebuildContainer.widthAnchor.constraint(equalToConstant: 20),
            rebuildContainer.heightAnchor.constraint(equalToConstant: 20),
        ])

        // Table takes full width when info panel hidden
        tableScrollView.trailingAnchor.constraint(equalTo: infoPanelView.leadingAnchor).isActive = true

        rebuildButton.isEnabled = false

        // Context menu
        let menu = NSMenu()
        menu.addLocalizedItem("search.ctx.open", action: #selector(openSelected), keyEquivalent: "\r")
        menu.addLocalizedItem("search.ctx.revealInFinder", action: #selector(revealSelected), keyEquivalent: "")
        menu.addItem(NSMenuItem.separator())
        menu.addLocalizedItem("search.ctx.copyPath", action: #selector(copyPath), keyEquivalent: "c")
        menu.addLocalizedItem("search.ctx.copyName", action: #selector(copyName), keyEquivalent: "")
        tableView.menu = menu
    }

    private func addTableColumns() {
        let nameCol = NSTableColumn(identifier: .columnName)
        nameCol.title = L("search.col.name")
        nameCol.width = 280
        nameCol.minWidth = 140
        nameCol.sortDescriptorPrototype = NSSortDescriptor(key: "name", ascending: true,
                                                           selector: #selector(NSString.localizedStandardCompare(_:)))
        tableView.addTableColumn(nameCol)

        let pathCol = NSTableColumn(identifier: .columnPath)
        pathCol.title = L("search.col.path")
        pathCol.width = 240
        pathCol.minWidth = 100
        pathCol.sortDescriptorPrototype = NSSortDescriptor(key: "path", ascending: true,
            selector: #selector(NSString.localizedStandardCompare(_:)))
        tableView.addTableColumn(pathCol)

        let modifiedCol = NSTableColumn(identifier: .columnModified)
        modifiedCol.title = L("search.col.dateModified")
        modifiedCol.width = 160
        modifiedCol.minWidth = 100
        modifiedCol.sortDescriptorPrototype = NSSortDescriptor(key: "modified", ascending: false)
        tableView.addTableColumn(modifiedCol)

        let sizeCol = NSTableColumn(identifier: .columnSize)
        sizeCol.title = L("search.col.size")
        sizeCol.width = 80
        sizeCol.minWidth = 50
        sizeCol.headerCell.alignment = .right
        sizeCol.sortDescriptorPrototype = NSSortDescriptor(key: "size", ascending: false)
        tableView.addTableColumn(sizeCol)
    }

    // MARK: - Info Panel

    private func setupInfoPanel(in parent: NSView) {
        infoPanelView = ThemedBackgroundView()
        infoPanelView.translatesAutoresizingMaskIntoConstraints = false
        infoPanelView.wantsLayer = true
        infoPanelView.layer?.masksToBounds = true
        parent.addSubview(infoPanelView)

        // Vertical separator
        let sep = NSBox()
        sep.translatesAutoresizingMaskIntoConstraints = false
        sep.boxType = .separator
        infoPanelView.addSubview(sep)

        // Large icon
        infoIconView = NSImageView()
        infoIconView.translatesAutoresizingMaskIntoConstraints = false
        infoIconView.imageScaling = .scaleProportionallyUpOrDown
        infoPanelView.addSubview(infoIconView)

        // Name
        infoNameLabel = NSTextField(wrappingLabelWithString: "")
        infoNameLabel.translatesAutoresizingMaskIntoConstraints = false
        infoNameLabel.font = .boldSystemFont(ofSize: 13)
        infoNameLabel.alignment = .center
        infoNameLabel.maximumNumberOfLines = 2
        infoNameLabel.lineBreakMode = .byTruncatingMiddle
        infoPanelView.addSubview(infoNameLabel)

        // Kind subtitle
        infoKindLabel = NSTextField(labelWithString: "")
        infoKindLabel.translatesAutoresizingMaskIntoConstraints = false
        infoKindLabel.font = .systemFont(ofSize: 11)
        infoKindLabel.textColor = .secondaryLabelColor
        infoKindLabel.alignment = .center
        infoPanelView.addSubview(infoKindLabel)

        // Detail rows container
        let detailStack = NSStackView()
        detailStack.translatesAutoresizingMaskIntoConstraints = false
        detailStack.orientation = .vertical
        detailStack.alignment = .leading
        detailStack.spacing = 4
        infoPanelView.addSubview(detailStack)

        let fields = [
            L("search.info.kind"),
            L("search.info.size"),
            L("search.info.created"),
            L("search.info.modified"),
            L("search.info.where"),
        ]
        for (i, label) in fields.enumerated() {
            let row = NSStackView()
            row.orientation = .horizontal
            row.spacing = 6
            row.alignment = .top

            let key = NSTextField(labelWithString: label)
            key.font = .systemFont(ofSize: 11)
            key.textColor = .secondaryLabelColor
            key.setContentHuggingPriority(.defaultHigh, for: .horizontal)
            key.widthAnchor.constraint(equalToConstant: 60).isActive = true
            key.alignment = .right

            let value = NSTextField(wrappingLabelWithString: "—")
            value.font = .systemFont(ofSize: 11)
            value.textColor = .labelColor
            value.maximumNumberOfLines = 2
            value.lineBreakMode = .byTruncatingMiddle

            if i == 4 {  // Where (last field)
                value.maximumNumberOfLines = 0
                value.lineBreakMode = .byCharWrapping
            }

            row.addArrangedSubview(key)
            row.addArrangedSubview(value)
            detailStack.addArrangedSubview(row)
            infoDetailLabels.append((key: key, value: value))
        }

        infoPanelWidth = infoPanelView.widthAnchor.constraint(equalToConstant: 0)

        NSLayoutConstraint.activate([
            infoPanelView.topAnchor.constraint(equalTo: parent.topAnchor),
            infoPanelView.trailingAnchor.constraint(equalTo: parent.trailingAnchor),
            infoPanelView.bottomAnchor.constraint(equalTo: parent.bottomAnchor),
            infoPanelWidth,

            sep.topAnchor.constraint(equalTo: infoPanelView.topAnchor),
            sep.bottomAnchor.constraint(equalTo: infoPanelView.bottomAnchor),
            sep.leadingAnchor.constraint(equalTo: infoPanelView.leadingAnchor),
            sep.widthAnchor.constraint(equalToConstant: 1),

            infoIconView.topAnchor.constraint(equalTo: infoPanelView.topAnchor, constant: 20),
            infoIconView.centerXAnchor.constraint(equalTo: infoPanelView.centerXAnchor),
            infoIconView.widthAnchor.constraint(equalToConstant: 64),
            infoIconView.heightAnchor.constraint(equalToConstant: 64),

            infoNameLabel.topAnchor.constraint(equalTo: infoIconView.bottomAnchor, constant: 8),
            infoNameLabel.leadingAnchor.constraint(equalTo: sep.trailingAnchor, constant: 12),
            infoNameLabel.trailingAnchor.constraint(equalTo: infoPanelView.trailingAnchor, constant: -12),

            infoKindLabel.topAnchor.constraint(equalTo: infoNameLabel.bottomAnchor, constant: 2),
            infoKindLabel.leadingAnchor.constraint(equalTo: infoNameLabel.leadingAnchor),
            infoKindLabel.trailingAnchor.constraint(equalTo: infoNameLabel.trailingAnchor),

            detailStack.topAnchor.constraint(equalTo: infoKindLabel.bottomAnchor, constant: 16),
            detailStack.leadingAnchor.constraint(equalTo: sep.trailingAnchor, constant: 8),
            detailStack.trailingAnchor.constraint(equalTo: infoPanelView.trailingAnchor, constant: -8),
        ])
    }

    @objc private func toggleInfoPanel() {
        infoPanelVisible.toggle()
        NSAnimationContext.runAnimationGroup({ ctx in
            ctx.duration = 0.25
            // Swift, native snappy curve
            ctx.timingFunction = CAMediaTimingFunction(controlPoints: 0.2, 0.8, 0.4, 1.0)
            ctx.allowsImplicitAnimation = true
            infoPanelWidth.constant = infoPanelVisible ? 240 : 0
            window?.contentView?.layoutSubtreeIfNeeded()
        }) { [weak self] in
            self?.tableView.sizeLastColumnToFit()
        }
        if infoPanelVisible {
            updateInfoPanel()
        }
    }

    private func updateInfoPanel() {
        let row = tableView.selectedRow
        guard row >= 0, row < results.count else {
            infoIconView.image = nil
            infoNameLabel.stringValue = ""
            infoKindLabel.stringValue = ""
            for (_, v) in infoDetailLabels { v.stringValue = "—" }
            return
        }
        let result = results[row]

        let icon = NSWorkspace.shared.icon(forFile: result.path)
        icon.size = NSSize(width: 64, height: 64)
        infoIconView.image = icon

        infoNameLabel.stringValue = result.name
        infoKindLabel.stringValue = kindDescription(for: result)

        // Kind
        infoDetailLabels[0].value.stringValue = kindDescription(for: result)

        // Size
        infoDetailLabels[1].value.stringValue = result.isDir ? "--"
            : Self.sizeFormatter.string(fromByteCount: Int64(result.size))

        // Created
        if result.created > 0 {
            let createdDate = Date(timeIntervalSince1970: TimeInterval(result.created))
            infoDetailLabels[2].value.stringValue = Self.fullDateFormatter.string(from: createdDate)
        } else {
            infoDetailLabels[2].value.stringValue = "---"
        }

        // Modified
        let date = Date(timeIntervalSince1970: TimeInterval(result.modified))
        infoDetailLabels[3].value.stringValue = Self.fullDateFormatter.string(from: date)

        // Where (parent dir)
        let parent = (result.path as NSString).deletingLastPathComponent
        infoDetailLabels[4].value.stringValue = abbreviatePath(parent)
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
        searchField.placeholderString = L(inlineProgressIndicator == nil ? "search.placeholder" : "search.indexingIncomplete")
        toggleInfoButton.toolTip = L("search.toggleInfo")
        toggleInfoButton.setAccessibilityLabel(L("search.toggleInfo"))
        rebuildButton.toolTip = L("search.rebuildIndex")
        rebuildButton.setAccessibilityLabel(L("search.rebuildIndex"))
        let columns: [(NSUserInterfaceItemIdentifier, String)] = [
            (.columnName, "search.col.name"), (.columnPath, "search.col.path"),
            (.columnModified, "search.col.dateModified"), (.columnSize, "search.col.size"),
        ]
        for (identifier, key) in columns {
            tableView.tableColumn(withIdentifier: identifier)?.title = L(key)
        }
        tableView.headerView?.needsDisplay = true
        let detailKeys = ["search.info.kind", "search.info.size", "search.info.created", "search.info.modified", "search.info.where"]
        for (labels, key) in zip(infoDetailLabels, detailKeys) { labels.key.stringValue = L(key) }
        tableView.menu?.refreshLocalizedTitles()
        if let menu = (searchField.cell as? NSSearchFieldCell)?.searchMenuTemplate {
            menu.title = L("search.mode.title")
            menu.refreshLocalizedTitles()
        }
        statusLabel.stringValue = statusText.string
        countLabel.stringValue = resultCountText.string
        overlayHeadingLabel?.stringValue = L("search.buildingIndex")
        overlaySubtextLabel?.stringValue = overlaySubtext.string
        refreshToastLabel?.stringValue = L("search.backgroundRefresh")
        updateInfoPanel()
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
            rebuildButton.isEnabled = true
            indexingState = .idle
        case .rebuilding:
            guard source == "rebuild" else {
                statusText = .formatted("search.indexedCount", formatCount(total))
                return
            }
            statusText = .formatted("search.indexedCount", formatCount(total))
            hideInlineProgress()
            stopRebuildButtonAnimation()
            indexingState = .idle
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
        rebuildButton.isEnabled = false
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
        guard inlineProgressIndicator == nil else { return }

        // Small indeterminate spinner in status bar area (per D-06)
        let spinnerContainer: NSView
        if #available(macOS 14.0, *) {
            let imageView = NSImageView()
            let config = NSImage.SymbolConfiguration(pointSize: 13, weight: .semibold)
            imageView.image = NSImage(systemSymbolName: "arrow.triangle.2.circlepath", accessibilityDescription: nil)?.withSymbolConfiguration(config)
            imageView.contentTintColor = .secondaryLabelColor
            imageView.addSymbolEffect(.variableColor.iterative.reversing)
            spinnerContainer = imageView
        } else {
            let pi = NSProgressIndicator()
            pi.style = .spinning
            pi.controlSize = .small
            pi.isIndeterminate = true
            pi.wantsLayer = true
            pi.startAnimation(nil)
            spinnerContainer = pi
        }
        spinnerContainer.translatesAutoresizingMaskIntoConstraints = false

        // Insert spinner into bottom bar, before statusLabel
        guard let bottomBar = statusLabel.superview else { return }
        bottomBar.addSubview(spinnerContainer)
        inlineProgressIndicator = spinnerContainer

        // Layout: place spinner just before statusLabel (per D-07)
        NSLayoutConstraint.activate([
            spinnerContainer.trailingAnchor.constraint(equalTo: statusLabel.leadingAnchor, constant: -4),
            spinnerContainer.centerYAnchor.constraint(equalTo: bottomBar.centerYAnchor),
            spinnerContainer.widthAnchor.constraint(equalToConstant: 16),
            spinnerContainer.heightAnchor.constraint(equalToConstant: 16),
        ])

        // Change search field placeholder (per D-08)
        searchField.placeholderString = L("search.indexingIncomplete")
    }

    private func hideInlineProgress() {
        if let pi = inlineProgressIndicator as? NSProgressIndicator {
            pi.stopAnimation(nil)
        } else if #available(macOS 14.0, *) {
            if let imageView = inlineProgressIndicator as? NSImageView {
                imageView.removeAllSymbolEffects()
            }
        }
        inlineProgressIndicator?.removeFromSuperview()
        inlineProgressIndicator = nil

        // Restore search field placeholder (per D-09)
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
        let iconContainer = CenterSpinContainerView()
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
        let spin = CABasicAnimation(keyPath: "transform.rotation.z")
        spin.fromValue = 0
        spin.toValue = -CGFloat.pi * 2 // AppKit clockwise
        spin.duration = 1.0
        spin.repeatCount = .infinity
        spin.timingFunction = CAMediaTimingFunction(name: .linear)
        spin.isRemovedOnCompletion = false
        iconView.layer?.add(spin, forKey: "spin")

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
        rebuildButton.isEnabled = false
        startRebuildButtonAnimation()
        showInlineProgress()
        statusText = .key("search.indexing")
        engine.rebuildIndex()
    }

    private func startRebuildButtonAnimation() {
        guard let layer = rebuildButton.layer else { return }

        let rotation = CABasicAnimation(keyPath: "transform.rotation.z")
        rotation.fromValue = 0
        rotation.toValue = -CGFloat.pi * 2 // AppKit: negative is clockwise
        rotation.duration = 1.0
        rotation.repeatCount = .infinity
        // Linear is strictly required for continuous rotation so it doesn't stutter
        rotation.timingFunction = CAMediaTimingFunction(name: .linear)
        rotation.isRemovedOnCompletion = false
        layer.add(rotation, forKey: "spin")
    }

    private func stopRebuildButtonAnimation() {
        guard let layer = rebuildButton.layer else { return }
        
        if let currentRotation = layer.presentation()?.value(forKeyPath: "transform.rotation.z") as? CGFloat {
            layer.removeAnimation(forKey: "spin")
            
            let fullRotation = -CGFloat.pi * 2
            var targetMultiplier = ceil(currentRotation / fullRotation)
            // If it's too close to snapping, give it an extra spin to decelerate gracefully
            if abs((targetMultiplier * fullRotation) - currentRotation) < 0.5 {
                targetMultiplier += 1
            }
            let targetRotation = targetMultiplier * fullRotation
            
            CATransaction.begin()
            CATransaction.setCompletionBlock {
                layer.transform = CATransform3DIdentity
                layer.removeAnimation(forKey: "stopSpin")
            }
            
            let stop = CABasicAnimation(keyPath: "transform.rotation.z")
            stop.fromValue = currentRotation
            stop.toValue = targetRotation
            stop.duration = 0.4
            stop.timingFunction = CAMediaTimingFunction(controlPoints: 0.1, 0.7, 0.3, 1.0)
            stop.fillMode = .forwards
            stop.isRemovedOnCompletion = false
            
            layer.add(stop, forKey: "stopSpin")
            CATransaction.commit()
        } else {
            layer.removeAnimation(forKey: "spin")
        }
        
        rebuildButton.isEnabled = true
        rebuildButton.contentTintColor = .secondaryLabelColor
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
            results = []
            iconCache.removeAll()
            tableView.sortDescriptors = []
            tableView.reloadData()
            resultCountText = .verbatim("")
            pathBar.url = nil
            let stats = engine.stats
            statusText = stats.isReady ? .formatted("search.indexedCount", formatCount(stats.totalCount)) : .key("search.indexing")
            return
        }

        let item = DispatchWorkItem { [weak self] in
            self?.performSearch(query: query)
        }
        searchWorkItem = item
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.016, execute: item)
    }

    private var isSearching = false

    private func performSearch(query: String) {
        guard indexingState != .failed, !isSearching else { return }
        isSearching = true
        resultCountText = .key("search.searching")

        let mode = currentMode
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            guard let self else { return }
            let response = self.engine.search(query: query, mode: mode)
            DispatchQueue.main.async {
                self.isSearching = false
                guard self.indexingState != .failed else { return }
                self.results = response.results
                self.iconCache.removeAll()
                self.tableView.sortDescriptors = []  // Reset sort to relevance order (D-06)
                
                // Crossfade animation for list refresh
                let transition = CATransition()
                transition.type = .fade
                transition.duration = 0.15
                transition.timingFunction = CAMediaTimingFunction(name: .easeOut)
                self.tableView.layer?.add(transition, forKey: "fade")
                
                self.tableView.reloadData()

                if let error = response.error {
                    self.resultCountText = .verbatim(error)
                } else {
                    self.resultCountText = .formatted("search.foundCount", self.formatCount(UInt64(self.results.count)))
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

    @objc private func pathBarClicked() {
        guard let item = pathBar.clickedPathItem,
              let url = item.url else { return }
        NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: url.path)
    }

    @objc private func tableDoubleClicked() {
        let row = tableView.clickedRow
        guard row >= 0, row < results.count else { return }
        engine.openFile(path: results[row].path)
    }

    private var selectedResults: [FfiSearchResult] {
        let indices = tableView.selectedRowIndexes.filter { $0 >= 0 && $0 < results.count }
        return indices.map { results[$0] }
    }

    @objc private func openSelected() {
        let selected = selectedResults
        guard !selected.isEmpty else { return }
        for item in selected {
            engine.openFile(path: item.path)
        }
    }

    @objc private func revealSelected() {
        let selected = selectedResults
        guard !selected.isEmpty else { return }
        if selected.count == 1 {
            engine.revealInFinder(path: selected[0].path)
        } else {
            let urls = selected.map { URL(fileURLWithPath: $0.path) }
            NSWorkspace.shared.activateFileViewerSelecting(urls)
        }
    }

    @objc private func copyPath() {
        copySelected(\.path)
    }

    @objc private func copyName() {
        copySelected(\.name)
    }

    /// 以换行连接选区的指定字段写入剪贴板，便于直接粘贴到列表或脚本。
    private func copySelected(_ field: KeyPath<FfiSearchResult, String>) {
        let selected = selectedResults
        guard !selected.isEmpty else { return }
        let joined = selected.map { $0[keyPath: field] }.joined(separator: "\n")
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(joined, forType: .string)
    }

    // MARK: - Helpers

    private func formatCount(_ n: UInt64) -> String {
        Self.countFormatter.string(from: NSNumber(value: n)) ?? "\(n)"
    }

    private func iconForFile(at path: String, isDir: Bool) -> NSImage {
        if let cached = iconCache[path] { return cached }
        let icon = NSWorkspace.shared.icon(forFile: path)
        icon.size = NSSize(width: 16, height: 16)
        iconCache[path] = icon
        return icon
    }

    fileprivate func kindDescription(for result: FfiSearchResult) -> String {
        if result.isDir { return L("kind.folder") }

        let ext = result.extension.lowercased()

        // Tier 1: Custom mapping table — developer file types (D-07: custom map takes priority over UTType)
        if !ext.isEmpty, let custom = FileKindMap.extensionToKind[ext] {
            return custom
        }

        // Tier 2: UTType system description — PDF, PNG, MP3, etc. (D-08)
        // CRITICAL: Must check isDeclared to avoid dyn.xxx identifiers for unknown extensions
        if !ext.isEmpty, let uttype = UTType(filenameExtension: ext),
           uttype.isDeclared,
           let desc = uttype.localizedDescription {
            return desc
        }

        // Tier 3: Fallback (D-09, D-10)
        if ext.isEmpty {
            return L("kind.document")  // D-10: no extension, non-directory
        }
        return "\(ext.uppercased()) \(L("kind.file"))"  // D-09: unknown extension -> "XYZ File"
    }

    private func abbreviatePath(_ path: String) -> String {
        abbreviateHomePrefix(path).display
    }
}

// MARK: - NSTableViewDataSource

extension SearchWindow: NSTableViewDataSource {
    func numberOfRows(in tableView: NSTableView) -> Int {
        results.count
    }

    func tableView(_ tableView: NSTableView, sortDescriptorsDidChange oldDescriptors: [NSSortDescriptor]) {
        guard let descriptor = tableView.sortDescriptors.first,
              let key = descriptor.key else { return }

        results.sort { a, b in
            let cmp: ComparisonResult
            switch key {
            case "name":
                cmp = a.name.localizedStandardCompare(b.name)
            case "path":
                let pathA = (a.path as NSString).deletingLastPathComponent
                let pathB = (b.path as NSString).deletingLastPathComponent
                cmp = pathA.localizedStandardCompare(pathB)
            case "modified":
                if a.modified < b.modified { cmp = .orderedAscending }
                else if a.modified > b.modified { cmp = .orderedDescending }
                else { cmp = .orderedSame }
            case "size":
                if a.size < b.size { cmp = .orderedAscending }
                else if a.size > b.size { cmp = .orderedDescending }
                else { cmp = .orderedSame }
            default:
                cmp = .orderedSame
            }
            return descriptor.ascending ? cmp == .orderedAscending : cmp == .orderedDescending
        }

        // Clear selection (simpler than preserving; matches Finder behavior)
        tableView.deselectAll(nil)
        pathBar.url = nil
        
        let transition = CATransition()
        transition.type = .fade
        transition.duration = 0.15
        transition.timingFunction = CAMediaTimingFunction(name: .easeOut)
        tableView.layer?.add(transition, forKey: "fade")
        
        tableView.reloadData()
    }
}

// MARK: - NSTableViewDelegate

extension SearchWindow: NSTableViewDelegate {
    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard row < results.count, let column = tableColumn else { return nil }
        let result = results[row]
        let colID = column.identifier

        switch colID {
        case .columnName:
            return nameCell(for: result, in: tableView)
        case .columnPath:
            return pathCell(for: result, in: tableView)
        case .columnModified:
            let date = Date(timeIntervalSince1970: TimeInterval(result.modified))
            let str = Self.dateFormatter.string(from: date)
            return textCell(string: str, font: .systemFont(ofSize: 13),
                            color: .labelColor, alignment: .left, identifier: colID, in: tableView)
        case .columnSize:
            let str = result.isDir ? "--" : Self.sizeFormatter.string(fromByteCount: Int64(result.size))
            return textCell(string: str, font: .monospacedDigitSystemFont(ofSize: 13, weight: .regular),
                            color: .secondaryLabelColor, alignment: .right, identifier: colID, in: tableView)
        default:
            return nil
        }
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        let row = tableView.selectedRow
        if row >= 0, row < results.count {
            pathBar.url = URL(fileURLWithPath: results[row].path)
        } else {
            pathBar.url = nil
        }
        if infoPanelVisible {
            updateInfoPanel()
        }
    }

    /// 构造单元格单行标签：统一关闭换行、滚动、编辑与背景绘制。
    private func makeCellLabel(lineBreakMode: NSLineBreakMode = .byTruncatingTail,
                               font: NSFont = .systemFont(ofSize: 13),
                               color: NSColor? = nil,
                               alignment: NSTextAlignment = .left) -> NSTextField {
        let textField = NSTextField(labelWithString: "")
        textField.translatesAutoresizingMaskIntoConstraints = false
        textField.font = font
        if let color { textField.textColor = color }
        textField.alignment = alignment
        textField.lineBreakMode = lineBreakMode
        textField.maximumNumberOfLines = 1
        textField.cell?.wraps = false
        textField.cell?.isScrollable = false
        textField.isEditable = false
        textField.isBezeled = false
        textField.drawsBackground = false
        return textField
    }

    private func nameCell(for result: FfiSearchResult, in tableView: NSTableView) -> NSTableCellView {
        let id = NSUserInterfaceItemIdentifier("NameCell")
        if let reused = tableView.makeView(withIdentifier: id, owner: nil) as? SearchNameTableCellView {
            reused.imageView?.image = iconForFile(at: result.path, isDir: result.isDir)
            reused.configure(name: result.name, matchPositions: result.matchPositions)
            return reused
        }

        let cell = SearchNameTableCellView()
        cell.identifier = id

        let imageView = NSImageView()
        imageView.translatesAutoresizingMaskIntoConstraints = false
        imageView.imageScaling = .scaleProportionallyUpOrDown
        imageView.image = iconForFile(at: result.path, isDir: result.isDir)
        cell.addSubview(imageView)
        cell.imageView = imageView

        let textField = makeCellLabel(lineBreakMode: .byTruncatingTail)
        cell.addSubview(textField)
        cell.textField = textField

        NSLayoutConstraint.activate([
            imageView.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 4),
            imageView.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
            imageView.widthAnchor.constraint(equalToConstant: 16),
            imageView.heightAnchor.constraint(equalToConstant: 16),
            textField.leadingAnchor.constraint(equalTo: imageView.trailingAnchor, constant: 4),
            textField.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -2),
            textField.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
        ])

        cell.configure(name: result.name, matchPositions: result.matchPositions)

        return cell
    }

    private func pathCell(for result: FfiSearchResult, in tableView: NSTableView) -> NSTableCellView {
        let id = NSUserInterfaceItemIdentifier("PathCell")
        if let reused = tableView.makeView(withIdentifier: id, owner: nil) as? SearchPathTableCellView {
            reused.configure(fullPath: result.path, parentMatchPositions: result.parentMatchPositions)
            return reused
        }

        let cell = SearchPathTableCellView()
        cell.identifier = id

        let textField = makeCellLabel(lineBreakMode: .byTruncatingMiddle)
        cell.addSubview(textField)
        cell.textField = textField

        NSLayoutConstraint.activate([
            textField.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 4),
            textField.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -4),
            textField.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
        ])

        cell.configure(fullPath: result.path, parentMatchPositions: result.parentMatchPositions)
        return cell
    }

    private func textCell(string: String, font: NSFont, color: NSColor, alignment: NSTextAlignment,
                          identifier: NSUserInterfaceItemIdentifier, in tableView: NSTableView) -> NSView {
        let cellID = NSUserInterfaceItemIdentifier(identifier.rawValue + "Cell")
        if let reused = tableView.makeView(withIdentifier: cellID, owner: nil) as? NSTableCellView {
            reused.textField?.stringValue = string
            return reused
        }

        let cell = NSTableCellView()
        cell.identifier = cellID

        let textField = makeCellLabel(font: font, color: color, alignment: alignment)
        textField.stringValue = string
        cell.addSubview(textField)
        cell.textField = textField

        NSLayoutConstraint.activate([
            textField.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 4),
            textField.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -4),
            textField.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
        ])

        return cell
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

// MARK: - Column Identifiers

extension NSUserInterfaceItemIdentifier {
    static let columnName = NSUserInterfaceItemIdentifier("Name")
    static let columnPath = NSUserInterfaceItemIdentifier("Path")
    static let columnSize = NSUserInterfaceItemIdentifier("Size")
    static let columnModified = NSUserInterfaceItemIdentifier("DateModified")
}

// MARK: - NSSearchFieldDelegate

extension SearchWindow: NSSearchFieldDelegate {
    func controlTextDidChange(_ obj: Notification) {
        searchChanged()
    }
}

// MARK: - NSToolbarDelegate

extension SearchWindow: NSToolbarDelegate {
    func toolbar(_ toolbar: NSToolbar, itemForItemIdentifier itemIdentifier: NSToolbarItem.Identifier, willBeInsertedIntoToolbar flag: Bool) -> NSToolbarItem? {
        guard itemIdentifier == Self.searchBarItem else { return nil }

        // Container holds searchField + countLabel + toggleInfoButton in a single toolbar item
        let container = NSView()
        container.translatesAutoresizingMaskIntoConstraints = false

        searchField.translatesAutoresizingMaskIntoConstraints = true
        countLabel.translatesAutoresizingMaskIntoConstraints = true
        toggleInfoButton.translatesAutoresizingMaskIntoConstraints = true

        searchField.translatesAutoresizingMaskIntoConstraints = false
        countLabel.translatesAutoresizingMaskIntoConstraints = false
        toggleInfoButton.translatesAutoresizingMaskIntoConstraints = false

        container.addSubview(searchField)
        container.addSubview(countLabel)
        container.addSubview(toggleInfoButton)

        countLabel.setContentCompressionResistancePriority(.required, for: .horizontal)
        countLabel.setContentHuggingPriority(.required, for: .horizontal)
        toggleInfoButton.setContentHuggingPriority(.required, for: .horizontal)
        searchField.setContentHuggingPriority(.defaultLow, for: .horizontal)

        NSLayoutConstraint.activate([
            searchField.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            searchField.centerYAnchor.constraint(equalTo: container.centerYAnchor),

            countLabel.leadingAnchor.constraint(equalTo: searchField.trailingAnchor, constant: 10),
            countLabel.centerYAnchor.constraint(equalTo: container.centerYAnchor),

            toggleInfoButton.leadingAnchor.constraint(equalTo: countLabel.trailingAnchor, constant: 8),
            toggleInfoButton.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            toggleInfoButton.centerYAnchor.constraint(equalTo: container.centerYAnchor),
            toggleInfoButton.widthAnchor.constraint(equalToConstant: 28),

            container.heightAnchor.constraint(equalToConstant: 28),
        ])

        let item = NSToolbarItem(itemIdentifier: itemIdentifier)
        item.view = container
        item.label = ""
        item.isNavigational = true  // Expand to fill available toolbar space
        return item
    }

    func toolbarDefaultItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [Self.searchBarItem]
    }

    func toolbarAllowedItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [Self.searchBarItem]
    }
}

// MARK: - Themed Background View
final class ThemedBackgroundView: NSView {
    override var wantsUpdateLayer: Bool { true }

    override func updateLayer() {
        layer?.backgroundColor = NSColor.windowBackgroundColor.cgColor
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        updateLayer()
    }
}

// MARK: - Highlighted Table Cell Views

/// 用户家目录，进程内恒定。
private let homeDirectory = NSHomeDirectory()

/// 家目录缩写结果：展示串与换算所需的前缀字节数。
private struct AbbreviatedHomePath {
    let display: String
    /// 被 '~' 替换掉的前缀字节数；为 0 表示未发生缩写。
    let prefixBytes: Int

    /// 把原串的 UTF-8 字节区间映射到展示串。
    ///
    /// 起点落在被缩写前缀内的区间没有对应字符，返回 nil 交由调用方丢弃。
    func displayRange(for range: FfiMatchRange) -> (start: Int, end: Int)? {
        let start = Int(range.start)
        guard start >= prefixBytes else { return nil }
        // '~' 占 1 字节，故整体前移 (prefixBytes - 1)；prefixBytes 为 0 时位移取 0
        let shift = max(prefixBytes - 1, 0)
        return (start - shift, Int(range.end) - shift)
    }
}

/// 把路径开头的家目录前缀缩写为 '~'；无需缩写时原样返回。
private func abbreviateHomePrefix(_ path: String) -> AbbreviatedHomePath {
    // 必须精确匹配家目录或其子路径，避免误伤同名前缀的其他目录
    guard path == homeDirectory || path.hasPrefix(homeDirectory + "/") else {
        return AbbreviatedHomePath(display: path, prefixBytes: 0)
    }

    let prefixIndex = path.index(path.startIndex, offsetBy: homeDirectory.count)
    // 直接量 path 切片内的实际字节数，规避 NFC/NFD 规范化差异带来的偏移
    let prefixBytes = path[..<prefixIndex].utf8.count
    return AbbreviatedHomePath(display: "~" + path[prefixIndex...], prefixBytes: prefixBytes)
}

/// 高亮单元格基类：统一字体、选中态配色与高亮渲染。
///
/// 展示串与 NSRange 在 `setDisplay` 阶段算好缓存，选中态变化只重新着色。
private class SearchHighlightCellView: NSTableCellView {
    /// 未选中时的基准字色。
    var baseColor: NSColor { .labelColor }
    /// 选中态下基准字色的不透明度，突出命中字符。
    var selectedAlpha: CGFloat { 1 }

    /// 字号恒定，字体对象全类复用。
    private static let regularFont = NSFont.systemFont(ofSize: 13)
    private static let boldFont = NSFont.systemFont(ofSize: 13, weight: .bold)

    private var displayString = ""
    private var displayRanges: [NSRange] = []

    override var backgroundStyle: NSView.BackgroundStyle {
        didSet {
            renderText()
        }
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        renderText()
    }

    /// 更新展示串与 UTF-8 字节高亮区间；区间在此一次性换算为 NSRange 并缓存。
    func setDisplay(_ string: String, ranges: [(start: Int, end: Int)]) {
        displayString = string
        displayRanges = Self.nsRanges(in: string, byteRanges: ranges)
        renderText()
    }

    /// 后端下发的是 UTF-8 字节偏移，需夹取后映射为 NSRange。
    private static func nsRanges(in string: String,
                                 byteRanges: [(start: Int, end: Int)]) -> [NSRange] {
        let utf8 = string.utf8
        return byteRanges.compactMap { range in
            guard range.start < range.end,
                  let start = utf8.index(utf8.startIndex, offsetBy: range.start, limitedBy: utf8.endIndex),
                  let end = utf8.index(utf8.startIndex, offsetBy: range.end, limitedBy: utf8.endIndex),
                  start < end else {
                return nil
            }
            return NSRange(start..<end, in: string)
        }
    }

    private func renderText() {
        guard !displayString.isEmpty else {
            textField?.stringValue = ""
            return
        }

        let isSelected = backgroundStyle == .emphasized
        let isDark = effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        let defaultColor: NSColor = isSelected ? NSColor.white.withAlphaComponent(selectedAlpha) : baseColor
        let highlightColor: NSColor = isSelected ? .white : (isDark ? .controlAccentColor : .labelColor)

        let attributed = NSMutableAttributedString(string: displayString, attributes: [
            .font: Self.regularFont,
            .foregroundColor: defaultColor,
        ])

        for range in displayRanges {
            attributed.addAttributes([
                .font: Self.boldFont,
                .foregroundColor: highlightColor,
            ], range: range)
        }
        textField?.attributedStringValue = attributed
    }
}

// MARK: - Search Name Table Cell View

private final class SearchNameTableCellView: SearchHighlightCellView {
    /// 选中态下普通字符用微透明白保持层级，命中字符用纯白加粗突出。
    override var selectedAlpha: CGFloat { 0.82 }

    func configure(name: String, matchPositions: [FfiMatchRange]) {
        setDisplay(name, ranges: matchPositions.map { (Int($0.start), Int($0.end)) })
    }
}

// MARK: - Search Path Table Cell View

/// 路径列单元格：父目录高亮 + 家目录缩写重映射。
private final class SearchPathTableCellView: SearchHighlightCellView {
    override var baseColor: NSColor { .secondaryLabelColor }
    override var selectedAlpha: CGFloat { 0.75 }

    func configure(fullPath: String, parentMatchPositions: [FfiMatchRange]) {
        let parent = (fullPath as NSString).deletingLastPathComponent
        guard !parent.isEmpty else {
            setDisplay("", ranges: [])
            return
        }

        let abbreviated = abbreviateHomePrefix(parent)
        setDisplay(abbreviated.display,
                   ranges: parentMatchPositions.compactMap { abbreviated.displayRange(for: $0) })
    }
}

// MARK: - NSMenuItemValidation

extension SearchWindow: NSMenuItemValidation {
    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        switch menuItem.action {
        case #selector(openSelected),
             #selector(revealSelected),
             #selector(copyPath),
             #selector(copyName):
            return !selectedResults.isEmpty
        default:
            return true
        }
    }
}
