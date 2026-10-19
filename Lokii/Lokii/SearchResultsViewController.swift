import Cocoa
import UniformTypeIdentifiers
import Quartz

/// 承载旋转微动效的居中稳定容器，锁定中心锚点规避 AppKit 图层坐标漂移
final class SearchResultsCenterSpinContainerView: NSView {
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

/// 底栏克制权限提示按钮：提供原生手型指针交互响应
private final class InteractiveHintButton: NSButton {
    override func resetCursorRects() {
        super.resetCursorRects()
        addCursorRect(bounds, cursor: .pointingHand)
    }
}

extension CALayer {
    /// 追加匀速无限旋转；AppKit 图层坐标系中逆时针为正，故顺时针取负值
    func addInfiniteSpin() {
        let spin = CABasicAnimation(keyPath: "transform.rotation.z")
        spin.fromValue = 0
        spin.toValue = -CGFloat.pi * 2
        spin.duration = 1.0
        spin.repeatCount = .infinity
        spin.timingFunction = CAMediaTimingFunction(name: .linear)
        spin.isRemovedOnCompletion = false
        add(spin, forKey: "spin")
    }
}

// MARK: - Notification & Density Model

extension Notification.Name {
    static let tableDensityDidChange = Notification.Name("LokiiTableDensityDidChange")
}

/// 检索结果列表排版密度与尺寸联动模型
public enum TableDensity: Int, CaseIterable {
    case compact = 11
    case standard = 13
    case comfortable = 15
    case large = 17

    public static let minFontSize: Int = 11
    public static let maxFontSize: Int = 19
    public static let defaultFontSize: Int = 13
    public static let fontSizeUserDefaultsKey = "LokiiTableFontSize"

    public var fontSize: CGFloat { CGFloat(rawValue) }

    /// 档位对应的本地化标题键
    var titleKey: String {
        switch self {
        case .compact: return "density.compact"
        case .standard: return "density.standard"
        case .comfortable: return "density.comfortable"
        case .large: return "density.large"
        }
    }

    /// 生效字号，读写统一落在同一 UserDefaults 键；未持久化时回退默认档位
    static var savedFontSize: CGFloat {
        get {
            let saved = UserDefaults.standard.double(forKey: fontSizeUserDefaultsKey)
            return saved > 0 ? CGFloat(saved) : CGFloat(defaultFontSize)
        }
        set {
            UserDefaults.standard.set(Double(newValue), forKey: fontSizeUserDefaultsKey)
        }
    }

    /// 行高、图标与日期列宽随字号连续缩放，三者共享同一套分档边界，故边界只在此定义一次
    private static func tier(for fontSize: CGFloat) -> Int {
        switch Int(fontSize) {
        case ...11: return 0
        case 12...13: return 1
        case 14...15: return 2
        case 16...17: return 3
        default: return 4
        }
    }

    private static let rowHeights: [CGFloat] = [21, 24, 27, 30, 33]
    private static let iconSizes: [CGFloat] = [14, 16, 18, 20, 22]
    private static let dateColumnMinWidths: [CGFloat] = [130, 150, 170, 190, 210]

    public static func calculateRowHeight(for fontSize: CGFloat) -> CGFloat {
        rowHeights[tier(for: fontSize)]
    }

    public static func calculateIconSize(for fontSize: CGFloat) -> CGFloat {
        iconSizes[tier(for: fontSize)]
    }

    public static func calculateDateMinWidth(for fontSize: CGFloat) -> CGFloat {
        dateColumnMinWidths[tier(for: fontSize)]
    }
}

/// 列表字体缓存池：按生效字号集中构建，避免高频滚动时重复分配 CoreText 字体描述符
struct TableFontCache {
    let regular: NSFont
    let bold: NSFont
    let monospacedDigit: NSFont

    init(fontSize: CGFloat) {
        self.regular = .systemFont(ofSize: fontSize)
        self.bold = .systemFont(ofSize: fontSize, weight: .bold)
        self.monospacedDigit = .monospacedDigitSystemFont(ofSize: fontSize, weight: .regular)
    }
}

// MARK: - SearchResultsViewControllerDelegate

/// 检索虚拟列表事件代理协议
protocol SearchResultsViewControllerDelegate: AnyObject {
    /// 虚拟列表选中项变更回调
    func searchResultsDidSelect(result: FfiSearchResult?)
    /// 虚拟列表行双击确认回调
    func searchResultsDidDoubleClick(result: FfiSearchResult)
    /// 索引重建请求回调
    func searchResultsDidRequestRebuild()
    /// 虚拟列表首行上移回调，用于将焦点交还搜索输入框
    func searchResultsDidRequestFocusSearchField()
    /// 检索结果条目成功移入废纸篓回调（用于联动上层容器与目录聚合树同步）
    func searchResultsDidTrashItems(paths: Set<String>)
}

extension SearchResultsViewControllerDelegate {
    func searchResultsDidRequestRebuild() {}
    func searchResultsDidRequestFocusSearchField() {}
    func searchResultsDidTrashItems(paths: Set<String>) {}
}

// MARK: - SearchResultsViewController

/// 原生 AppKit 高信息密度虚拟列表控制器
///
/// 承载检索命中结果的高性能虚拟列表渲染、多列权重排版、表头点击排序、
/// 匹配字符高亮、系统图标缓存、上下文菜单交互以及内聚式底栏状态呈现。
final class SearchResultsViewController: NSViewController {
    weak var delegate: SearchResultsViewControllerDelegate?

    let tableView = SearchTableView()
    let tableScrollView = NSScrollView()

    // ── 内聚底栏核心组件 ──
    let separator = NSBox()
    let bottomBar = NSView()
    let pathBar = NSPathControl()
    let statusLabel = NSTextField(labelWithString: "")
    let inlineProgressIndicator: NSProgressIndicator = {
        let pi = NSProgressIndicator()
        pi.translatesAutoresizingMaskIntoConstraints = false
        pi.style = .spinning
        pi.controlSize = .small
        pi.isIndeterminate = true
        pi.isDisplayedWhenStopped = false
        pi.isHidden = true
        pi.wantsLayer = true
        return pi
    }()
    let rebuildContainer = SearchResultsCenterSpinContainerView()
    let rebuildButton = NSButton()
    let fdaHintButton: NSButton = InteractiveHintButton()
    private var hasFullDiskAccess: Bool = true
    /// 完全磁盘访问受限指引点击回调
    var onOpenFDAGuide: (() -> Void)?

    private(set) var results: [FfiSearchResult] = []
    private let iconCache: NSCache<NSString, NSImage> = {
        let cache = NSCache<NSString, NSImage>()
        cache.countLimit = 512
        return cache
    }()
    private var lastReportedPath: String?
    private(set) var currentFontSize: CGFloat = CGFloat(TableDensity.defaultFontSize)
    private(set) var fontCache = TableFontCache(fontSize: CGFloat(TableDensity.defaultFontSize))
    private(set) var currentIconSize: CGFloat = TableDensity.calculateIconSize(for: CGFloat(TableDensity.defaultFontSize))

    /// 注入式废纸篓操作闭包，默认调用系统 Foundation 标准接口
    var trashItemHandler: (URL) throws -> URL = { url in
        var resultingURL: NSURL?
        try FileManager.default.trashItem(at: url, resultingItemURL: &resultingURL)
        return (resultingURL as URL?) ?? url
    }

    /// 注入式确认弹窗闭包，默认通过主视窗弹出原生确认 Sheet，返回用户是否确认执行
    var confirmTrashHandler: ((Int, @escaping (Bool) -> Void) -> Void)?

    /// 时间与体积的展示串只取决于原始数值与当前语言，按原始值缓存以规避滚动时对每个进入视口的行重复格式化
    private var dateTextCache: [TimeInterval: String] = [:]
    private var sizeTextCache: [UInt64: String] = [:]

    private static let dateFormatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateFormat = "yyyy/M/d, HH:mm"
        return formatter
    }()

    private static let sizeFormatter: ByteCountFormatter = {
        let formatter = ByteCountFormatter()
        formatter.countStyle = .file
        return formatter
    }()

    init() {
        super.init(nibName: nil, bundle: nil)
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
    }

    deinit {
        NotificationCenter.default.removeObserver(self)
    }

    override func loadView() {
        let containerView = NSView()
        containerView.wantsLayer = true

        tableScrollView.translatesAutoresizingMaskIntoConstraints = false
        tableScrollView.documentView = tableView
        tableScrollView.hasVerticalScroller = true
        tableScrollView.hasHorizontalScroller = false
        tableScrollView.autohidesScrollers = true
        tableScrollView.borderType = .noBorder
        tableScrollView.drawsBackground = true
        // 显式禁用自动内容边距推导，由 safeAreaLayoutGuide 约束精确控制视口边界，
        // 彻底杜绝 AppKit 默认滚动边距机制导致数据行向上穿越表头并渗透进标题栏工具区
        tableScrollView.automaticallyAdjustsContentInsets = false
        tableScrollView.contentInsets = NSEdgeInsetsZero
        containerView.addSubview(tableScrollView)

        separator.translatesAutoresizingMaskIntoConstraints = false
        separator.boxType = .separator
        containerView.addSubview(separator)

        bottomBar.translatesAutoresizingMaskIntoConstraints = false
        bottomBar.wantsLayer = true
        containerView.addSubview(bottomBar)

        setupBottomBar()

        NSLayoutConstraint.activate([
            // 顶端锚定至安全区布局辅助线，使表头与滚动视口始终对齐至窗口工具栏底缘，
            // 滚动时列表内容在表头底线处干净截断
            tableScrollView.topAnchor.constraint(equalTo: containerView.safeAreaLayoutGuide.topAnchor),
            tableScrollView.leadingAnchor.constraint(equalTo: containerView.leadingAnchor),
            tableScrollView.trailingAnchor.constraint(equalTo: containerView.trailingAnchor),
            tableScrollView.bottomAnchor.constraint(equalTo: separator.topAnchor),

            separator.leadingAnchor.constraint(equalTo: containerView.leadingAnchor),
            separator.trailingAnchor.constraint(equalTo: containerView.trailingAnchor),
            separator.bottomAnchor.constraint(equalTo: bottomBar.topAnchor),

            bottomBar.leadingAnchor.constraint(equalTo: containerView.leadingAnchor),
            bottomBar.trailingAnchor.constraint(equalTo: containerView.trailingAnchor),
            bottomBar.bottomAnchor.constraint(equalTo: containerView.bottomAnchor),
            bottomBar.heightAnchor.constraint(equalToConstant: 28),
        ])

        self.view = containerView
    }

    private func setupBottomBar() {
        pathBar.translatesAutoresizingMaskIntoConstraints = false
        pathBar.pathStyle = .standard
        pathBar.isEditable = false
        pathBar.font = .systemFont(ofSize: 12)
        pathBar.backgroundColor = .clear
        pathBar.focusRingType = .none
        pathBar.refusesFirstResponder = true
        pathBar.target = self
        pathBar.action = #selector(pathBarClicked)
        pathBar.url = nil
        pathBar.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        pathBar.setContentHuggingPriority(.defaultLow, for: .horizontal)
        bottomBar.addSubview(pathBar)

        fdaHintButton.translatesAutoresizingMaskIntoConstraints = false
        fdaHintButton.isBordered = false
        fdaHintButton.bezelStyle = .inline
        fdaHintButton.setButtonType(.momentaryPushIn)
        fdaHintButton.focusRingType = .none
        fdaHintButton.refusesFirstResponder = true
        fdaHintButton.target = self
        fdaHintButton.action = #selector(fdaHintButtonClicked)
        fdaHintButton.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        fdaHintButton.setContentHuggingPriority(.defaultLow, for: .horizontal)
        fdaHintButton.isHidden = true
        updateFDAButtonContent()
        bottomBar.addSubview(fdaHintButton)

        bottomBar.addSubview(inlineProgressIndicator)

        statusLabel.translatesAutoresizingMaskIntoConstraints = false
        statusLabel.font = .systemFont(ofSize: 12)
        statusLabel.textColor = .secondaryLabelColor
        statusLabel.alignment = .right
        statusLabel.setContentHuggingPriority(.defaultHigh, for: .horizontal)
        bottomBar.addSubview(statusLabel)

        rebuildContainer.translatesAutoresizingMaskIntoConstraints = false
        bottomBar.addSubview(rebuildContainer)

        rebuildButton.translatesAutoresizingMaskIntoConstraints = true
        rebuildButton.frame = NSRect(x: 0, y: 0, width: 20, height: 20)
        rebuildButton.wantsLayer = true
        rebuildButton.image = NSImage(systemSymbolName: "arrow.clockwise", accessibilityDescription: "Rebuild Index")
        rebuildButton.bezelStyle = .toolbar
        rebuildButton.isBordered = false
        rebuildButton.toolTip = L("search.rebuildIndex")
        rebuildButton.contentTintColor = .secondaryLabelColor
        rebuildButton.target = self
        rebuildButton.action = #selector(rebuildButtonClicked)
        rebuildButton.isEnabled = false
        rebuildContainer.addSubview(rebuildButton)

        NSLayoutConstraint.activate([
            pathBar.leadingAnchor.constraint(equalTo: bottomBar.leadingAnchor, constant: 14),
            pathBar.centerYAnchor.constraint(equalTo: bottomBar.centerYAnchor),
            pathBar.trailingAnchor.constraint(lessThanOrEqualTo: inlineProgressIndicator.leadingAnchor, constant: -8),

            fdaHintButton.leadingAnchor.constraint(equalTo: bottomBar.leadingAnchor, constant: 14),
            fdaHintButton.centerYAnchor.constraint(equalTo: bottomBar.centerYAnchor),
            fdaHintButton.trailingAnchor.constraint(lessThanOrEqualTo: inlineProgressIndicator.leadingAnchor, constant: -8),

            inlineProgressIndicator.trailingAnchor.constraint(equalTo: statusLabel.leadingAnchor, constant: -4),
            inlineProgressIndicator.centerYAnchor.constraint(equalTo: bottomBar.centerYAnchor),
            inlineProgressIndicator.widthAnchor.constraint(equalToConstant: 16),
            inlineProgressIndicator.heightAnchor.constraint(equalToConstant: 16),

            statusLabel.trailingAnchor.constraint(equalTo: rebuildContainer.leadingAnchor, constant: -4),
            statusLabel.centerYAnchor.constraint(equalTo: bottomBar.centerYAnchor),

            rebuildContainer.trailingAnchor.constraint(equalTo: bottomBar.trailingAnchor, constant: -14),
            rebuildContainer.centerYAnchor.constraint(equalTo: bottomBar.centerYAnchor),
            rebuildContainer.widthAnchor.constraint(equalToConstant: 20),
            rebuildContainer.heightAnchor.constraint(equalToConstant: 20),
        ])

        updateBottomBarVisibility()
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        setupTableView()
        addTableColumns()
        setupContextMenu()

        // 语言切换经主分栏中介统一转发（SearchWindow → SearchMainSplitViewController），
        // 此处不再自行订阅，避免同一通知触发两次整表刷新
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(onTableDensityChanged(_:)),
            name: .tableDensityDidChange,
            object: nil
        )
    }

    private func setupTableView() {
        tableView.style = .fullWidth
        let initialFontSize = TableDensity.savedFontSize
        currentFontSize = initialFontSize
        fontCache = TableFontCache(fontSize: initialFontSize)
        currentIconSize = TableDensity.calculateIconSize(for: initialFontSize)
        tableView.rowHeight = TableDensity.calculateRowHeight(for: initialFontSize)
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
        tableView.onMoveUpAtTop = { [weak self] in
            self?.delegate?.searchResultsDidRequestFocusSearchField()
        }
    }

    private func addTableColumns() {
        let nameCol = NSTableColumn(identifier: .columnName)
        nameCol.title = L("search.col.name")
        nameCol.width = 280
        nameCol.minWidth = 140
        nameCol.sortDescriptorPrototype = NSSortDescriptor(
            key: "name",
            ascending: true,
            selector: #selector(NSString.localizedStandardCompare(_:))
        )
        tableView.addTableColumn(nameCol)

        let pathCol = NSTableColumn(identifier: .columnPath)
        pathCol.title = L("search.col.path")
        pathCol.width = 240
        pathCol.minWidth = 100
        pathCol.sortDescriptorPrototype = NSSortDescriptor(
            key: "path",
            ascending: true,
            selector: #selector(NSString.localizedStandardCompare(_:))
        )
        tableView.addTableColumn(pathCol)

        let modifiedCol = NSTableColumn(identifier: .columnModified)
        modifiedCol.title = L("search.col.dateModified")
        modifiedCol.width = 160
        modifiedCol.minWidth = TableDensity.calculateDateMinWidth(for: currentFontSize)
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

    @objc private func onTableDensityChanged(_ notification: Notification) {
        let newSize = notification.userInfo?["fontSize"] as? CGFloat ?? TableDensity.savedFontSize
        applyFontSize(newSize)
    }

    func applyFontSize(_ fontSize: CGFloat) {
        currentFontSize = fontSize
        fontCache = TableFontCache(fontSize: fontSize)
        currentIconSize = TableDensity.calculateIconSize(for: fontSize)
        tableView.rowHeight = TableDensity.calculateRowHeight(for: fontSize)

        if let modifiedCol = tableView.tableColumns.first(where: { $0.identifier == .columnModified }) {
            modifiedCol.minWidth = TableDensity.calculateDateMinWidth(for: fontSize)
        }
        tableView.reloadData()
    }

    private func setupContextMenu() {
        let menu = NSMenu()
        menu.addLocalizedItem("search.ctx.open", action: #selector(openSelected), keyEquivalent: "\r")
        let qlItem = menu.addLocalizedItem("search.ctx.quickLook", action: #selector(quickLookSelected), keyEquivalent: " ")
        qlItem.keyEquivalentModifierMask = []
        menu.addLocalizedItem("search.ctx.revealInFinder", action: #selector(revealSelected), keyEquivalent: "")
        menu.addLocalizedItem("search.ctx.getInfo", action: #selector(showInfoSelected), keyEquivalent: "i")
        menu.addItem(NSMenuItem.separator())
        menu.addLocalizedItem("search.ctx.copyFile", action: #selector(copyFile), keyEquivalent: "c")
        menu.addLocalizedItem("search.ctx.copyPath", action: #selector(copyPath), keyEquivalent: "")
        menu.addLocalizedItem("search.ctx.copyName", action: #selector(copyName), keyEquivalent: "")
        menu.addItem(NSMenuItem.separator())
        let trashItem = menu.addLocalizedItem(
            "search.ctx.moveToTrash",
            action: #selector(moveToTrashSelected),
            keyEquivalent: String(format: "%c", NSDeleteCharacter)
        )
        trashItem.keyEquivalentModifierMask = [.command]
        tableView.menu = menu
    }

    // MARK: - Public API

    /// 当前选中的首个条目
    var selectedResult: FfiSearchResult? {
        let row = tableView.selectedRow
        guard row >= 0, row < results.count else { return nil }
        return results[row]
    }

    /// 当前选中的全部条目集合
    var selectedResults: [FfiSearchResult] {
        let indices = tableView.selectedRowIndexes.filter { $0 >= 0 && $0 < results.count }
        return indices.map { results[$0] }
    }

    /// 更新并渲染检索结果集合
    func displayResults(_ results: [FfiSearchResult], preserveSelection: Bool) {
        let previousPath = preserveSelection ? selectedResult?.path : nil
        self.results = results
        // 图标按路径稳定复用，跨检索保留；展示串缓存随结果集更替释放，避免无限增长
        dateTextCache.removeAll()
        sizeTextCache.removeAll()

        if !preserveSelection {
            tableView.sortDescriptors = []
        } else if let descriptor = tableView.sortDescriptors.first, let key = descriptor.key {
            sortResults(by: key, ascending: descriptor.ascending)
        }

        reloadWithFade()

        if let previousPath,
           let matchIndex = self.results.firstIndex(where: { $0.path == previousPath }) {
            tableView.selectRowIndexes(IndexSet(integer: matchIndex), byExtendingSelection: false)
            tableView.scrollRowToVisible(matchIndex)
        } else {
            tableView.deselectAll(nil)
        }
        notifySelectionChanged()
    }

    /// 选中首行并滚动至可见视野
    func selectFirstRow() {
        guard !results.isEmpty else { return }
        tableView.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
        tableView.scrollRowToVisible(0)
        notifySelectionChanged()
    }

    /// 激活检索结果列表键盘焦点，并根据指定行号或已有选区联动可视区域与状态反馈
    @discardableResult
    func focusResults(row: Int? = nil) -> Bool {
        guard !results.isEmpty else { return false }
        let targetRow: Int
        if let row = row, row >= 0 && row < results.count {
            targetRow = row
        } else if tableView.selectedRow >= 0 && tableView.selectedRow < results.count {
            targetRow = tableView.selectedRow
        } else {
            targetRow = 0
        }
        tableView.selectRowIndexes(IndexSet(integer: targetRow), byExtendingSelection: false)
        tableView.scrollRowToVisible(targetRow)
        notifySelectionChanged()
        view.window?.makeFirstResponder(tableView)
        return true
    }

    // MARK: - Bottom Bar Interface

    /// 更新底栏文件路径展示并协同控制与权限提示的互斥显隐
    func updateBottomPath(with path: String?) {
        pathBar.url = path.map { URL(fileURLWithPath: $0) }
        updateBottomBarVisibility()
    }

    /// 更新当前完全磁盘访问权限状态并刷新底栏互斥展示
    func updateFDAStatus(hasFDA: Bool) {
        guard self.hasFullDiskAccess != hasFDA else { return }
        self.hasFullDiskAccess = hasFDA
        updateBottomBarVisibility()
    }

    /// 根据当前路径选中状态与完全磁盘访问权限协同控制显隐让位
    private func updateBottomBarVisibility() {
        // 路径展示优先级高于权限提示：有选中路径时始终让位给路径
        let showFDAHint = pathBar.url == nil && !hasFullDiskAccess
        pathBar.isHidden = showFDAHint
        fdaHintButton.isHidden = !showFDAHint
    }

    /// 更新底栏状态统计文本
    func updateStatusText(_ text: String) {
        statusLabel.stringValue = text
    }

    /// 控制底栏内联进度指示器显隐与动画
    func setInlineProgressVisible(_ visible: Bool) {
        inlineProgressIndicator.isHidden = !visible
        if visible {
            inlineProgressIndicator.startAnimation(nil)
        } else {
            inlineProgressIndicator.stopAnimation(nil)
        }
    }

    /// 控制索引重建按钮交互可用性
    func setRebuildEnabled(_ enabled: Bool) {
        rebuildButton.isEnabled = enabled
    }

    /// 控制索引重建按钮旋转微动效
    func setRebuildSpinning(_ spinning: Bool) {
        if spinning {
            startRebuildButtonAnimation()
        } else {
            stopRebuildButtonAnimation()
        }
    }

    // MARK: - Notifications

    @objc func onLanguageChanged() {
        let columns: [(NSUserInterfaceItemIdentifier, String)] = [
            (.columnName, "search.col.name"),
            (.columnPath, "search.col.path"),
            (.columnModified, "search.col.dateModified"),
            (.columnSize, "search.col.size"),
        ]
        for (identifier, key) in columns {
            tableView.tableColumn(withIdentifier: identifier)?.title = L(key)
        }
        tableView.headerView?.needsDisplay = true
        tableView.menu?.refreshLocalizedTitles()
        // 格式化结果随语言环境变化，缓存需整体失效
        dateTextCache.removeAll()
        sizeTextCache.removeAll()
        tableView.reloadData()
        rebuildButton.toolTip = L("search.rebuildIndex")
        rebuildButton.setAccessibilityLabel(L("search.rebuildIndex"))
        updateFDAButtonContent()
    }

    // MARK: - Bottom Bar Actions & Animation

    @objc private func fdaHintButtonClicked() {
        onOpenFDAGuide?()
    }

    private func updateFDAButtonContent() {
        let font = NSFont.systemFont(ofSize: 11.5, weight: .regular)
        let config = NSImage.SymbolConfiguration(pointSize: 11, weight: .medium)
            .applying(.init(paletteColors: [.systemOrange]))
        let image = NSImage(systemSymbolName: "exclamationmark.triangle.fill", accessibilityDescription: nil)?
            .withSymbolConfiguration(config)

        fdaHintButton.image = image
        fdaHintButton.imagePosition = .imageLeading
        fdaHintButton.imageHugsTitle = true

        let paragraphStyle = NSMutableParagraphStyle()
        paragraphStyle.lineBreakMode = .byTruncatingTail

        let attributedTitle = NSAttributedString(
            string: " " + L("fda.restrictedHint"),
            attributes: [
                .font: font,
                .foregroundColor: NSColor.secondaryLabelColor,
                .paragraphStyle: paragraphStyle,
            ]
        )
        fdaHintButton.attributedTitle = attributedTitle
    }

    @objc private func pathBarClicked() {
        if let url = pathBar.clickedPathItem?.url ?? pathBar.url {
            NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: url.path)
        }
    }

    @objc private func rebuildButtonClicked() {
        delegate?.searchResultsDidRequestRebuild()
    }

    private func startRebuildButtonAnimation() {
        rebuildButton.layer?.addInfiniteSpin()
    }

    private func stopRebuildButtonAnimation() {
        guard let layer = rebuildButton.layer else { return }

        if let currentRotation = layer.presentation()?.value(forKeyPath: "transform.rotation.z") as? CGFloat {
            layer.removeAnimation(forKey: "spin")

            let fullRotation = -CGFloat.pi * 2
            var targetMultiplier = ceil(currentRotation / fullRotation)
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
            layer.transform = CATransform3DIdentity
        }

        rebuildButton.contentTintColor = .secondaryLabelColor
    }

    // MARK: - Private Selection & Actions

    /// 整表刷新并叠加淡入过渡，规避结果集整体跳变
    private func reloadWithFade() {
        let transition = CATransition()
        transition.type = .fade
        transition.duration = 0.15
        transition.timingFunction = CAMediaTimingFunction(name: .easeOut)
        tableView.layer?.add(transition, forKey: "fade")
        tableView.reloadData()
    }

    private func notifySelectionChanged() {
        let current = selectedResult
        let currentPath = current?.path
        if currentPath != lastReportedPath {
            lastReportedPath = currentPath
            delegate?.searchResultsDidSelect(result: current)
        }
    }

    @objc private func tableDoubleClicked() {
        let row = tableView.clickedRow
        guard row >= 0, row < results.count else { return }
        let result = results[row]
        delegate?.searchResultsDidDoubleClick(result: result)
    }

    @objc func copyFile(_ sender: Any? = nil) {
        let selected = selectedResults
        guard !selected.isEmpty else { return }
        let urls = selected.map { NSURL(fileURLWithPath: $0.path) }
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.writeObjects(urls)
        let joinedPaths = selected.map(\.path).joined(separator: "\n")
        pasteboard.setString(joinedPaths, forType: .string)
    }

    @objc func quickLookSelected(_ sender: Any? = nil) {
        guard selectedResult != nil else { return }
        let panel = QLPreviewPanel.shared()
        panel?.reloadData()
        panel?.makeKeyAndOrderFront(nil)
    }

    @objc func showInfoSelected(_ sender: Any? = nil) {
        let selected = selectedResults
        guard !selected.isEmpty else { return }
        // 限制并发打开的最大窗口数，避免多选场景下瞬时弹出过多视窗造成系统卡顿
        let targets = selected.prefix(10).filter { FileManager.default.fileExists(atPath: $0.path) }
        guard !targets.isEmpty else { return }

        DispatchQueue.global(qos: .userInitiated).async {
            let statements = targets.map { item -> String in
                let escaped = item.path
                    .replacingOccurrences(of: "\\", with: "\\\\")
                    .replacingOccurrences(of: "\"", with: "\\\"")
                return "open information window of (POSIX file \"\(escaped)\" as alias)"
            }.joined(separator: "\n            ")

            let scriptSource = """
            tell application "Finder"
                \(statements)
                activate
            end tell
            """

            var error: NSDictionary?
            if let script = NSAppleScript(source: scriptSource) {
                script.executeAndReturnError(&error)
            }
        }
    }

    @objc func copyPath(_ sender: Any? = nil) {
        let selected = selectedResults
        guard !selected.isEmpty else { return }
        let joined = selected.map(\.path).joined(separator: "\n")
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(joined, forType: .string)
    }

    @objc func copyName(_ sender: Any? = nil) {
        let selected = selectedResults
        guard !selected.isEmpty else { return }
        let joined = selected.map(\.name).joined(separator: "\n")
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(joined, forType: .string)
    }

    @objc func openSelected(_ sender: Any? = nil) {
        let selected = selectedResults
        guard !selected.isEmpty else { return }
        for item in selected {
            delegate?.searchResultsDidDoubleClick(result: item)
        }
    }

    @objc func revealSelected(_ sender: Any? = nil) {
        let selected = selectedResults
        guard !selected.isEmpty else { return }
        if selected.count == 1 {
            NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: selected[0].path)
        } else {
            let urls = selected.map { URL(fileURLWithPath: $0.path) }
            NSWorkspace.shared.activateFileViewerSelecting(urls)
        }
    }

    @objc func moveToTrashSelected(_ sender: Any? = nil) {
        let selected = selectedResults
        guard !selected.isEmpty else { return }

        confirmTrashIfNeeded(count: selected.count) { [weak self] confirmed in
            guard let self = self, confirmed else { return }

            let handler = self.trashItemHandler
            // 调度至后台用户交互队列执行同步阻塞式文件系统 I/O，规避大目录或外接磁盘下的主线程掉帧
            DispatchQueue.global(qos: .userInitiated).async { [weak self] in
                var trashedPaths = Set<String>()
                var errors: [(String, Error)] = []

                for item in selected {
                    let url = URL(fileURLWithPath: item.path)
                    do {
                        _ = try handler(url)
                        trashedPaths.insert(item.path)
                    } catch {
                        let nsError = error as NSError
                        if nsError.domain == NSCocoaErrorDomain && nsError.code == NSFileNoSuchFileError {
                            // 边界守卫与自愈容错：若目标文件在物理文件系统上已不存在（如已在终端被清理），
                            // 表明该项目在源路径消除的业务诉求已达成，直接顺势将其作为已移除项从视图与结果集中剔除，
                            // 规避误报弹窗打扰用户
                            trashedPaths.insert(item.path)
                        } else {
                            errors.append((item.name, error))
                        }
                    }
                }

                DispatchQueue.main.async { [weak self] in
                    guard let self = self else { return }
                    if !trashedPaths.isEmpty {
                        self.applyTrashedItems(trashedPaths)
                        self.playTrashSound()
                    }
                    if !errors.isEmpty {
                        self.showTrashErrors(errors)
                    }
                }
            }
        }
    }

    /// 批量删除安全阈值确认：超过 5 项时弹出原生警示表单，防止误触全选造成破坏性删除
    private func confirmTrashIfNeeded(count: Int, completion: @escaping (Bool) -> Void) {
        if let customHandler = confirmTrashHandler {
            customHandler(count, completion)
            return
        }
        guard count > 5, let window = view.window else {
            completion(true)
            return
        }
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = String(format: L("search.trash.confirmTitle"), count)
        alert.informativeText = L("search.trash.confirmMessage")
        alert.addButton(withTitle: L("search.trash.confirmButton"))
        alert.addButton(withTitle: L("search.trash.cancelButton"))
        alert.beginSheetModal(for: window) { response in
            completion(response == .alertFirstButtonReturn)
        }
    }

    /// 乐观更新：在主列表内使用原生行滑动与淡出动画即时剔除已移入废纸篓的项目，并自动维护选区顺延
    private func applyTrashedItems(_ trashedPaths: Set<String>) {
        let selectedRow = tableView.selectedRow
        let indexSet = NSMutableIndexSet()
        for (idx, item) in results.enumerated() {
            if trashedPaths.contains(item.path) {
                indexSet.add(idx)
            }
        }

        results.removeAll { trashedPaths.contains($0.path) }

        if indexSet.count > 0 {
            tableView.beginUpdates()
            tableView.removeRows(at: indexSet as IndexSet, withAnimation: [.slideUp, .effectFade])
            tableView.endUpdates()
        }

        if !results.isEmpty {
            let nextRow = min(max(0, selectedRow), results.count - 1)
            tableView.selectRowIndexes(IndexSet(integer: nextRow), byExtendingSelection: false)
            tableView.scrollRowToVisible(nextRow)
        } else {
            tableView.deselectAll(nil)
        }

        notifySelectionChanged()
        delegate?.searchResultsDidTrashItems(paths: trashedPaths)
    }

    private func showTrashErrors(_ errors: [(String, Error)]) {
        guard let window = view.window else { return }
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = L("search.trash.failedTitle")
        let detail = errors.map { "\($0.0): \($0.1.localizedDescription)" }.joined(separator: "\n")
        alert.informativeText = String(format: L("search.trash.failedMessage"), errors.count, detail)
        alert.addButton(withTitle: L("ok"))
        alert.beginSheetModal(for: window, completionHandler: nil)
    }

    private func playTrashSound() {
        let soundPath = "/System/Library/Components/CoreAudio.component/Contents/SharedSupport/SystemSounds/dock/drag to trash.aif"
        if FileManager.default.fileExists(atPath: soundPath) {
            NSSound(contentsOfFile: soundPath, byReference: true)?.play()
        }
    }

    private func sortResults(by key: String, ascending: Bool) {
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
            return ascending ? cmp == .orderedAscending : cmp == .orderedDescending
        }
    }

    func iconForFile(at path: String) -> NSImage {
        if let cached = iconCache.object(forKey: path as NSString) { return cached }
        let icon = NSWorkspace.shared.icon(forFile: path)
        iconCache.setObject(icon, forKey: path as NSString)
        return icon
    }
}

// MARK: - NSTableViewDataSource

extension SearchResultsViewController: NSTableViewDataSource {
    func numberOfRows(in tableView: NSTableView) -> Int {
        results.count
    }

    func tableView(_ tableView: NSTableView, sortDescriptorsDidChange oldDescriptors: [NSSortDescriptor]) {
        guard let descriptor = tableView.sortDescriptors.first,
              let key = descriptor.key else { return }

        sortResults(by: key, ascending: descriptor.ascending)

        tableView.deselectAll(nil)

        reloadWithFade()
        notifySelectionChanged()
    }
}

// MARK: - NSTableViewDelegate

extension SearchResultsViewController: NSTableViewDelegate {
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
            return textCell(
                string: dateText(for: TimeInterval(result.modified)),
                font: fontCache.regular,
                color: .labelColor,
                alignment: .left,
                identifier: colID,
                in: tableView
            )
        case .columnSize:
            let str = result.isDir ? "--" : sizeText(for: result.size)
            return textCell(
                string: str,
                font: fontCache.monospacedDigit,
                color: .secondaryLabelColor,
                alignment: .right,
                identifier: colID,
                in: tableView
            )
        default:
            return nil
        }
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        notifySelectionChanged()
        if QLPreviewPanel.sharedPreviewPanelExists() && QLPreviewPanel.shared().isVisible {
            QLPreviewPanel.shared().reloadData()
        }
    }

    /// 修改时间展示串；按原始时间戳命中缓存，避免滚动时逐行重复格式化
    private func dateText(for modified: TimeInterval) -> String {
        if let cached = dateTextCache[modified] { return cached }
        let text = Self.dateFormatter.string(from: Date(timeIntervalSince1970: modified))
        dateTextCache[modified] = text
        return text
    }

    /// 文件体积展示串；按原始字节数命中缓存，避免滚动时逐行重复格式化
    private func sizeText(for size: UInt64) -> String {
        if let cached = sizeTextCache[size] { return cached }
        let text = Self.sizeFormatter.string(fromByteCount: Int64(size))
        sizeTextCache[size] = text
        return text
    }

    private func nameCell(for result: FfiSearchResult, in tableView: NSTableView) -> NSTableCellView {
        let id = NSUserInterfaceItemIdentifier("NameCell")
        if let reused = tableView.makeView(withIdentifier: id, owner: nil) as? SearchNameTableCellView {
            reused.imageView?.image = iconForFile(at: result.path)
            reused.updateIconSize(currentIconSize)
            reused.applyFonts(regular: fontCache.regular, bold: fontCache.bold)
            reused.configure(name: result.name, matchPositions: result.matchPositions)
            return reused
        }

        let cell = SearchNameTableCellView()
        cell.identifier = id

        let imageView = NSImageView()
        imageView.translatesAutoresizingMaskIntoConstraints = false
        imageView.imageScaling = .scaleProportionallyUpOrDown
        imageView.image = iconForFile(at: result.path)
        cell.addSubview(imageView)
        cell.imageView = imageView

        let textField = NSTextField(labelWithString: "")
        textField.translatesAutoresizingMaskIntoConstraints = false
        textField.font = fontCache.regular
        textField.lineBreakMode = .byTruncatingTail
        textField.maximumNumberOfLines = 1
        textField.cell?.wraps = false
        textField.cell?.isScrollable = false
        textField.isEditable = false
        textField.isBezeled = false
        textField.drawsBackground = false
        cell.addSubview(textField)
        cell.textField = textField

        let widthConstraint = imageView.widthAnchor.constraint(equalToConstant: currentIconSize)
        let heightConstraint = imageView.heightAnchor.constraint(equalToConstant: currentIconSize)

        NSLayoutConstraint.activate([
            imageView.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 4),
            imageView.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
            widthConstraint,
            heightConstraint,
            textField.leadingAnchor.constraint(equalTo: imageView.trailingAnchor, constant: 4),
            textField.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -2),
            textField.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
        ])

        cell.setIconConstraints(width: widthConstraint, height: heightConstraint)
        cell.applyFonts(regular: fontCache.regular, bold: fontCache.bold)
        cell.configure(name: result.name, matchPositions: result.matchPositions)

        return cell
    }

    private func pathCell(for result: FfiSearchResult, in tableView: NSTableView) -> NSTableCellView {
        let id = NSUserInterfaceItemIdentifier("PathCell")
        if let reused = tableView.makeView(withIdentifier: id, owner: nil) as? SearchPathTableCellView {
            reused.applyFonts(regular: fontCache.regular, bold: fontCache.bold)
            reused.configure(fullPath: result.path, parentMatchPositions: result.parentMatchPositions)
            return reused
        }

        let cell = SearchPathTableCellView()
        cell.identifier = id

        let textField = NSTextField(labelWithString: "")
        textField.translatesAutoresizingMaskIntoConstraints = false
        textField.font = fontCache.regular
        textField.lineBreakMode = .byTruncatingTail
        textField.maximumNumberOfLines = 1
        textField.cell?.wraps = false
        textField.cell?.isScrollable = false
        textField.isEditable = false
        textField.isBezeled = false
        textField.drawsBackground = false
        cell.addSubview(textField)
        cell.textField = textField

        NSLayoutConstraint.activate([
            textField.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 4),
            textField.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -4),
            textField.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
        ])

        cell.applyFonts(regular: fontCache.regular, bold: fontCache.bold)
        cell.configure(fullPath: result.path, parentMatchPositions: result.parentMatchPositions)
        return cell
    }

    private func textCell(
        string: String,
        font: NSFont,
        color: NSColor,
        alignment: NSTextAlignment,
        identifier: NSUserInterfaceItemIdentifier,
        in tableView: NSTableView
    ) -> NSView {
        let cellID = NSUserInterfaceItemIdentifier(identifier.rawValue + "Cell")
        if let reused = tableView.makeView(withIdentifier: cellID, owner: nil) as? NSTableCellView {
            reused.textField?.stringValue = string
            reused.textField?.font = font
            reused.textField?.textColor = color
            reused.textField?.alignment = alignment
            return reused
        }

        let cell = NSTableCellView()
        cell.identifier = cellID

        let textField = NSTextField(labelWithString: string)
        textField.translatesAutoresizingMaskIntoConstraints = false
        textField.font = font
        textField.textColor = color
        textField.alignment = alignment
        textField.lineBreakMode = .byTruncatingTail
        textField.isEditable = false
        textField.isBezeled = false
        textField.drawsBackground = false
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

// MARK: - NSMenuItemValidation

extension SearchResultsViewController: NSMenuItemValidation {
    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        switch menuItem.action {
        case #selector(openSelected),
             #selector(quickLookSelected),
             #selector(revealSelected),
             #selector(showInfoSelected),
             #selector(copyFile),
             #selector(copyPath),
             #selector(copyName),
             #selector(moveToTrashSelected):
            return !selectedResults.isEmpty
        default:
            return true
        }
    }
}

// MARK: - QLPreviewPanelDataSource & QLPreviewPanelDelegate

extension SearchResultsViewController: QLPreviewPanelDataSource, QLPreviewPanelDelegate {
    override func acceptsPreviewPanelControl(_ panel: QLPreviewPanel!) -> Bool {
        selectedResult != nil
    }

    override func beginPreviewPanelControl(_ panel: QLPreviewPanel!) {
        panel.delegate = self
        panel.dataSource = self
    }

    override func endPreviewPanelControl(_ panel: QLPreviewPanel!) {
        panel.delegate = nil
        panel.dataSource = nil
    }

    func numberOfPreviewItems(in panel: QLPreviewPanel!) -> Int {
        selectedResult != nil ? 1 : 0
    }

    func previewPanel(_ panel: QLPreviewPanel!, previewItemAt index: Int) -> QLPreviewItem! {
        guard let path = selectedResult?.path else { return nil }
        return NSURL(fileURLWithPath: path)
    }

    /// 当前选中项对应的文件图标视图，用作快速预览等比缩放动画的屏幕锚点与过渡位图
    private var selectedIconImageView: NSImageView? {
        let row = tableView.selectedRow
        guard row >= 0, row < tableView.numberOfRows else { return nil }
        let col = tableView.column(withIdentifier: .columnName)
        guard col >= 0,
              let cellView = tableView.view(atColumn: col, row: row, makeIfNecessary: false) as? NSTableCellView else {
            return nil
        }
        return cellView.imageView
    }

    func previewPanel(_ panel: QLPreviewPanel!, sourceFrameOnScreenFor item: QLPreviewItem!) -> NSRect {
        guard let imageView = selectedIconImageView,
              let window = imageView.window,
              imageView.visibleRect.height > 0 else {
            return .zero
        }
        let rectInWindow = imageView.convert(imageView.bounds, to: nil)
        return window.convertToScreen(rectInWindow)
    }

    func previewPanel(_ panel: QLPreviewPanel!, transitionImageFor item: QLPreviewItem!, contentRect: UnsafeMutablePointer<NSRect>!) -> Any! {
        guard let image = selectedIconImageView?.image else { return nil }
        contentRect?.pointee = NSRect(origin: .zero, size: image.size)
        return image
    }
}

// MARK: - SearchTableView

/// 原生 AppKit 高信息密度虚拟列表，封装右键智能选区联动与前台即时响应机制。
final class SearchTableView: NSTableView {
    // 仅在真实数据行有效区域内绘制交替斑马纹，末尾空白区域填充统一纯色背景
    override func drawBackground(inClipRect clipRect: NSRect) {
        guard usesAlternatingRowBackgroundColors else {
            super.drawBackground(inClipRect: clipRect)
            return
        }

        let totalRows = numberOfRows
        guard totalRows > 0 else {
            backgroundColor.setFill()
            clipRect.fill()
            return
        }

        let lastRowRect = rect(ofRow: totalRows - 1)
        let contentBottom = lastRowRect.maxY

        let contentRect = clipRect.intersection(NSRect(
            x: bounds.minX,
            y: bounds.minY,
            width: bounds.width,
            height: contentBottom
        ))
        if !contentRect.isEmpty {
            super.drawBackground(inClipRect: contentRect)
        }

        if clipRect.minY < bounds.minY {
            let topOverhang = NSRect(
                x: clipRect.minX,
                y: clipRect.minY,
                width: clipRect.width,
                height: bounds.minY - clipRect.minY
            )
            backgroundColor.setFill()
            topOverhang.fill()
        }

        if clipRect.maxY > contentBottom {
            let bottomOverhang = NSRect(
                x: clipRect.minX,
                y: max(clipRect.minY, contentBottom),
                width: clipRect.width,
                height: clipRect.maxY - max(clipRect.minY, contentBottom)
            )
            backgroundColor.setFill()
            bottomOverhang.fill()
        }
    }

    /// 列表焦点位于首行（或无选区）时继续向上移动的接力回调
    var onMoveUpAtTop: (() -> Void)?

    override func keyDown(with event: NSEvent) {
        if event.keyCode == 49 { // Space key
            if QLPreviewPanel.sharedPreviewPanelExists() && QLPreviewPanel.shared().isVisible {
                QLPreviewPanel.shared().orderOut(nil)
            } else if selectedRow >= 0 {
                QLPreviewPanel.shared().makeKeyAndOrderFront(nil)
            }
            return
        }

        // 方向上键（keyCode 126）：当选区停在首行或无选区时，将焦点与光标接力回搜索输入框末尾。
        // 规避 ⇧/⌘ 组合键，确保多选与滚动至顶部行为不受影响。
        let navigationModifiers: NSEvent.ModifierFlags = [.command, .shift, .control]
        if event.keyCode == 126,
           event.modifierFlags.intersection(navigationModifiers).isEmpty,
           selectedRow <= 0 {
            onMoveUpAtTop?()
            return
        }

        // ⌘⌫ (Command + Delete，keyCode 51)：当焦点处于列表且存在选区时，触发移到废纸篓动作
        if event.keyCode == 51 && event.modifierFlags.contains(.command) {
            if let targetController = delegate as? SearchResultsViewController,
               !targetController.selectedResults.isEmpty {
                targetController.moveToTrashSelected()
                return
            }
        }

        super.keyDown(with: event)
    }

    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        if let menu = self.menu, menu.performKeyEquivalent(with: event) {
            return true
        }
        return super.performKeyEquivalent(with: event)
    }
    override func menu(for event: NSEvent) -> NSMenu? {
        let point = convert(event.locationInWindow, from: nil)
        let row = self.row(at: point)

        // 边界守卫：点击虚拟列表末尾空白区域时阻断上下文菜单，规避悬空操作
        guard row >= 0 && row < numberOfRows else {
            return nil
        }

        // 选区智能维护：
        // 若右键命中的行未被包含在当前选区，则将选区收拢重置为该单行；
        // 若命中的行已在选中集合内（例如用户已多选多项），则完整保持多选集合不被破坏。
        if !selectedRowIndexes.contains(row) {
            selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        }

        return super.menu(for: event)
    }

    // 视窗在非激活状态下接收初次点按，保证点击即选中，消除二次点击开销
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool {
        return true
    }
}

// MARK: - Highlighted Table Cell Views

/// 用户家目录，进程生命周期内恒定。
private let homeDirectory = NSHomeDirectory()

/// 家目录缩写结果：展示串与被替换前缀的 UTF-8 字节数。
private struct AbbreviatedHomePath {
    let display: String
    /// 被替换的家目录前缀字节数；命中区间起点落在此范围内即属于被缩写部分。
    let prefixBytes: Int

    /// 将原绝对路径上的 UTF-8 字节区间重映射到展示串上；若区间起点落在被缩写的前缀内则返回 nil。
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
class SearchHighlightCellView: NSTableCellView {
    /// 未选中时的基准字色。
    var baseColor: NSColor { .labelColor }
    /// 选中态下基准字色的不透明度，突出命中字符。
    var selectedAlpha: CGFloat { 1 }

    /// 字体对象随全局排版密度动态绑定，并复用外部缓存池
    private(set) var regularFont: NSFont = NSFont.systemFont(ofSize: 13)
    private(set) var boldFont: NSFont = NSFont.systemFont(ofSize: 13, weight: .bold)

    func applyFonts(regular: NSFont, bold: NSFont) {
        if self.regularFont != regular || self.boldFont != bold {
            self.regularFont = regular
            self.boldFont = bold
            renderText()
        }
    }

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
            .font: regularFont,
            .foregroundColor: defaultColor,
        ])

        for range in displayRanges {
            attributed.addAttributes([
                .font: boldFont,
                .foregroundColor: highlightColor,
            ], range: range)
        }
        textField?.attributedStringValue = attributed
    }
}

// MARK: - Search Name Table Cell View

final class SearchNameTableCellView: SearchHighlightCellView {
    /// 选中态下普通字符用微透明白保持层级，命中字符用纯白加粗突出。
    override var selectedAlpha: CGFloat { 0.82 }

    private(set) var iconWidthConstraint: NSLayoutConstraint?
    private(set) var iconHeightConstraint: NSLayoutConstraint?

    func setIconConstraints(width: NSLayoutConstraint, height: NSLayoutConstraint) {
        self.iconWidthConstraint = width
        self.iconHeightConstraint = height
    }

    func updateIconSize(_ size: CGFloat) {
        iconWidthConstraint?.constant = size
        iconHeightConstraint?.constant = size
    }

    func configure(name: String, matchPositions: [FfiMatchRange]) {
        setDisplay(name, ranges: matchPositions.map { (Int($0.start), Int($0.end)) })
    }
}

// MARK: - Search Path Table Cell View

/// 路径列单元格：父目录高亮 + 家目录缩写重映射。
final class SearchPathTableCellView: SearchHighlightCellView {
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

// MARK: - Column Identifiers

extension NSUserInterfaceItemIdentifier {
    static let columnName = NSUserInterfaceItemIdentifier("Name")
    static let columnPath = NSUserInterfaceItemIdentifier("Path")
    static let columnSize = NSUserInterfaceItemIdentifier("Size")
    static let columnModified = NSUserInterfaceItemIdentifier("DateModified")
}
