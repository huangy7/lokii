import Cocoa

// MARK: - SidebarTreeViewControllerDelegate

/// 左侧目录聚合树事件代理协议
protocol SidebarTreeViewControllerDelegate: AnyObject {
    /// 目录树单选变更回调；选中“全部结果”时回调 nil，选中具体目录时回调完整路径
    func sidebarDidSelectDirectory(path: String?)
}

// MARK: - SidebarAllResultsItem

/// “全部结果”根节点模型
///
/// 作为 NSOutlineView 的顶层固定根条目，汇聚展示当前检索命中的全量条目总数。
final class SidebarAllResultsItem: NSObject {
    var count: Int

    init(count: Int = 0) {
        self.count = count
    }
}

// MARK: - SidebarTreeViewController

/// 左侧目录聚合树控制器与空状态占位
///
/// 承载基于即时检索结果动态构建的前缀聚合树（ResultDirectoryTree）。
/// 提供 .sourceList 风格树形导航、单链空目录折叠展现、高频打字下的展开态静默恢复与单选作用域过滤。
final class SidebarTreeViewController: NSViewController {
    weak var delegate: SidebarTreeViewControllerDelegate?

    let outlineView = SidebarOutlineView()
    let scrollView = NSScrollView()
    let emptyLabel = NSTextField(wrappingLabelWithString: "")

    let allResultsItem = SidebarAllResultsItem()
    private(set) var currentTree: ResultDirectoryTree?
    private(set) var selectedPath: String?
    private(set) var expandedPathSet: Set<String> = []

    private var isUpdatingTree = false
    private var allResultsTitle = L("search.sidebar.allResults")

    // 单元格图标按类型复用，规避每次刷新重建符号图像
    private static let allResultsIcon = NSImage(systemSymbolName: "magnifyingglass", accessibilityDescription: "All Results")
    private static let folderIcon = NSImage(systemSymbolName: "folder", accessibilityDescription: "Folder")

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
        containerView.translatesAutoresizingMaskIntoConstraints = false

        // 1. 滚动视图与目录树
        scrollView.translatesAutoresizingMaskIntoConstraints = false
        scrollView.documentView = outlineView
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = false
        scrollView.autohidesScrollers = true
        scrollView.borderType = .noBorder
        scrollView.drawsBackground = false
        containerView.addSubview(scrollView)

        // 2. 空状态居中占位提示
        emptyLabel.translatesAutoresizingMaskIntoConstraints = false
        emptyLabel.font = .systemFont(ofSize: 12)
        emptyLabel.textColor = .secondaryLabelColor
        emptyLabel.alignment = .center
        emptyLabel.isEditable = false
        emptyLabel.isSelectable = false
        emptyLabel.drawsBackground = false
        emptyLabel.isBezeled = false
        emptyLabel.maximumNumberOfLines = 0
        emptyLabel.lineBreakMode = .byWordWrapping
        emptyLabel.stringValue = L("search.sidebar.emptyHint")
        containerView.addSubview(emptyLabel)

        NSLayoutConstraint.activate([
            scrollView.topAnchor.constraint(equalTo: containerView.topAnchor),
            scrollView.bottomAnchor.constraint(equalTo: containerView.bottomAnchor),
            scrollView.leadingAnchor.constraint(equalTo: containerView.leadingAnchor),
            scrollView.trailingAnchor.constraint(equalTo: containerView.trailingAnchor),

            emptyLabel.centerXAnchor.constraint(equalTo: containerView.centerXAnchor),
            emptyLabel.centerYAnchor.constraint(equalTo: containerView.centerYAnchor),
            emptyLabel.leadingAnchor.constraint(greaterThanOrEqualTo: containerView.leadingAnchor, constant: 16),
            emptyLabel.trailingAnchor.constraint(lessThanOrEqualTo: containerView.trailingAnchor, constant: -16),
        ])

        self.view = containerView
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        setupOutlineView()

        // 初始为空状态
        emptyLabel.isHidden = false
        scrollView.isHidden = true

        NotificationCenter.default.addObserver(
            self,
            selector: #selector(onLanguageChanged),
            name: L10n.languageChangedNotification,
            object: nil
        )
    }

    private func setupOutlineView() {
        outlineView.style = .sourceList
        outlineView.rowHeight = 24
        outlineView.intercellSpacing = NSSize(width: 0, height: 2)
        outlineView.floatsGroupRows = false
        outlineView.headerView = nil
        outlineView.allowsMultipleSelection = false
        outlineView.allowsEmptySelection = false
        outlineView.selectionHighlightStyle = .regular

        let column = NSTableColumn(identifier: .sidebarColumn)
        column.isEditable = false
        outlineView.addTableColumn(column)
        outlineView.outlineTableColumn = column

        outlineView.dataSource = self
        outlineView.delegate = self
    }

    // MARK: - Public API

    /// 更新并渲染动态目录聚合树
    ///
    /// - Parameter tree: 即时检索结果构建的前缀聚合树；若为 nil 或无节点则展示空状态占位
    func updateTree(_ tree: ResultDirectoryTree?) {
        isUpdatingTree = true
        defer { isUpdatingTree = false }

        guard let tree = tree, !tree.rootNodes.isEmpty else {
            currentTree = nil
            selectedPath = nil
            allResultsItem.count = 0
            outlineView.reloadData()
            emptyLabel.isHidden = false
            scrollView.isHidden = true
            return
        }

        let previousSelectedPath = selectedPath
        currentTree = tree
        allResultsItem.count = tree.totalMatchCount

        emptyLabel.isHidden = true
        scrollView.isHidden = false
        outlineView.reloadData()

        // 默认展开“全部结果”根节点
        outlineView.expandItem(allResultsItem)

        // 静默恢复先前用户的文件夹展开状态
        restoreExpansion(for: tree.rootNodes)

        // 恢复先前选中的目录路径；若该目录已不在新结果集中则安全回退至“全部结果”
        var targetRow = 0
        selectedPath = nil
        if let prevPath = previousSelectedPath, let row = row(forPath: prevPath) {
            targetRow = row
            selectedPath = prevPath
        }

        outlineView.selectRowIndexes(IndexSet(integer: targetRow), byExtendingSelection: false)
        outlineView.scrollRowToVisible(targetRow)

        if selectedPath != previousSelectedPath {
            delegate?.sidebarDidSelectDirectory(path: selectedPath)
        }
    }

    /// 清空聚合树并恢复空状态占位
    func clear() {
        expandedPathSet.removeAll()
        updateTree(nil)
    }

    /// 选中“全部结果”项并通知代理
    func selectAllResults() {
        guard currentTree != nil else { return }
        outlineView.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
        outlineView.scrollRowToVisible(0)
        notifySelectionChanged()
    }

    /// 编程式选中指定路径目录并触发范围过滤
    func selectDirectory(path: String) {
        guard let tree = currentTree else { return }
        expandAncestors(of: path, in: tree.rootNodes)
        if let row = row(forPath: path) {
            outlineView.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
            outlineView.scrollRowToVisible(row)
            notifySelectionChanged()
        }
    }

    // MARK: - Notifications

    @objc private func onLanguageChanged() {
        allResultsTitle = L("search.sidebar.allResults")
        emptyLabel.stringValue = L("search.sidebar.emptyHint")
        outlineView.reloadData()
    }

    // MARK: - Private Helpers

    private func restoreExpansion(for nodes: [DirectoryTreeNode]) {
        for node in nodes {
            if expandedPathSet.contains(node.fullPath) {
                outlineView.expandItem(node)
                restoreExpansion(for: node.children)
            }
        }
    }

    private func expandAncestors(of targetPath: String, in nodes: [DirectoryTreeNode]) {
        for node in nodes {
            if node.fullPath == targetPath {
                return
            }
            if ResultDirectoryTree.isContained(targetPath, inDirectory: node.fullPath) {
                outlineView.expandItem(node)
                expandedPathSet.insert(node.fullPath)
                expandAncestors(of: targetPath, in: node.children)
            }
        }
    }

    private func row(forPath path: String) -> Int? {
        for r in 0..<outlineView.numberOfRows {
            if let node = outlineView.item(atRow: r) as? DirectoryTreeNode, node.fullPath == path {
                return r
            }
        }
        return nil
    }

    private func notifySelectionChanged() {
        let row = outlineView.selectedRow
        guard row >= 0, row < outlineView.numberOfRows else { return }
        let newPath = (outlineView.item(atRow: row) as? DirectoryTreeNode)?.fullPath
        guard newPath != selectedPath else { return }
        selectedPath = newPath
        delegate?.sidebarDidSelectDirectory(path: newPath)
    }
}

// MARK: - NSOutlineViewDataSource

extension SidebarTreeViewController: NSOutlineViewDataSource {
    func outlineView(_ outlineView: NSOutlineView, numberOfChildrenOfItem item: Any?) -> Int {
        if item == nil {
            guard let tree = currentTree, !tree.rootNodes.isEmpty else { return 0 }
            return 1
        }
        if item is SidebarAllResultsItem {
            return currentTree?.rootNodes.count ?? 0
        }
        if let node = item as? DirectoryTreeNode {
            return node.children.count
        }
        return 0
    }

    func outlineView(_ outlineView: NSOutlineView, isItemExpandable item: Any) -> Bool {
        if item is SidebarAllResultsItem {
            return !(currentTree?.rootNodes.isEmpty ?? true)
        }
        if let node = item as? DirectoryTreeNode {
            return !node.children.isEmpty
        }
        return false
    }

    func outlineView(_ outlineView: NSOutlineView, child index: Int, ofItem item: Any?) -> Any {
        if item == nil {
            return allResultsItem
        }
        if item is SidebarAllResultsItem {
            guard let tree = currentTree, tree.rootNodes.indices.contains(index) else {
                return DirectoryTreeNode(component: "", fullPath: "")
            }
            return tree.rootNodes[index]
        }
        if let node = item as? DirectoryTreeNode {
            guard node.children.indices.contains(index) else {
                return DirectoryTreeNode(component: "", fullPath: "")
            }
            return node.children[index]
        }
        fatalError("未知节点类型")
    }
}

// MARK: - NSOutlineViewDelegate

extension SidebarTreeViewController: NSOutlineViewDelegate {
    func outlineView(_ outlineView: NSOutlineView, viewFor tableColumn: NSTableColumn?, item: Any) -> NSView? {
        let cell = outlineView.makeView(withIdentifier: .sidebarCell, owner: nil) as? SidebarTreeCellView ?? SidebarTreeCellView()
        cell.identifier = .sidebarCell

        if let allResults = item as? SidebarAllResultsItem {
            cell.configure(
                title: allResultsTitle,
                count: allResults.count,
                icon: Self.allResultsIcon
            )
            return cell
        }

        if let node = item as? DirectoryTreeNode {
            cell.configure(
                title: node.component,
                count: node.recursiveMatchCount,
                icon: Self.folderIcon
            )
            return cell
        }

        return nil
    }

    func outlineViewSelectionDidChange(_ notification: Notification) {
        guard !isUpdatingTree else { return }
        notifySelectionChanged()
    }

    func outlineViewItemDidExpand(_ notification: Notification) {
        guard !isUpdatingTree else { return }
        if let node = notification.userInfo?["NSObject"] as? DirectoryTreeNode {
            expandedPathSet.insert(node.fullPath)
        }
    }

    func outlineViewItemDidCollapse(_ notification: Notification) {
        guard !isUpdatingTree else { return }
        if let node = notification.userInfo?["NSObject"] as? DirectoryTreeNode {
            expandedPathSet.remove(node.fullPath)
            // 若当前选中的节点随着折叠变为不可见，将选区收拢至被折叠的父目录
            if outlineView.selectedRow < 0 {
                if let parentRow = row(forPath: node.fullPath) {
                    outlineView.selectRowIndexes(IndexSet(integer: parentRow), byExtendingSelection: false)
                } else {
                    outlineView.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
                }
            }
        }
    }
}

// MARK: - SidebarOutlineView

/// 原生 AppKit 侧边栏树视图，支持前台即时单次点击响应
final class SidebarOutlineView: NSOutlineView {
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool {
        return true
    }
}

// MARK: - SidebarTreeCellView

/// 侧边栏聚合目录单元格视图，支持系统图标、目录名称截短与命中数量徽标
final class SidebarTreeCellView: NSTableCellView {
    let iconImageView = NSImageView()
    let countBadgeLabel = NSTextField(labelWithString: "")

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        setupViews()
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        setupViews()
    }

    private func setupViews() {
        iconImageView.translatesAutoresizingMaskIntoConstraints = false
        iconImageView.imageScaling = .scaleProportionallyUpOrDown
        addSubview(iconImageView)
        self.imageView = iconImageView

        let label = NSTextField(labelWithString: "")
        label.translatesAutoresizingMaskIntoConstraints = false
        label.font = .systemFont(ofSize: 13)
        label.lineBreakMode = .byTruncatingTail
        label.maximumNumberOfLines = 1
        label.cell?.wraps = false
        label.cell?.isScrollable = false
        label.isEditable = false
        label.isBezeled = false
        label.drawsBackground = false
        addSubview(label)
        self.textField = label

        countBadgeLabel.translatesAutoresizingMaskIntoConstraints = false
        countBadgeLabel.font = .monospacedDigitSystemFont(ofSize: 11, weight: .regular)
        countBadgeLabel.alignment = .right
        countBadgeLabel.textColor = .secondaryLabelColor
        countBadgeLabel.setContentHuggingPriority(.required, for: .horizontal)
        countBadgeLabel.setContentCompressionResistancePriority(.required, for: .horizontal)
        addSubview(countBadgeLabel)

        NSLayoutConstraint.activate([
            iconImageView.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 4),
            iconImageView.centerYAnchor.constraint(equalTo: centerYAnchor),
            iconImageView.widthAnchor.constraint(equalToConstant: 16),
            iconImageView.heightAnchor.constraint(equalToConstant: 16),

            label.leadingAnchor.constraint(equalTo: iconImageView.trailingAnchor, constant: 6),
            label.centerYAnchor.constraint(equalTo: centerYAnchor),
            label.trailingAnchor.constraint(lessThanOrEqualTo: countBadgeLabel.leadingAnchor, constant: -6),

            countBadgeLabel.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -6),
            countBadgeLabel.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    func configure(title: String, count: Int, icon: NSImage?) {
        textField?.stringValue = title
        countBadgeLabel.stringValue = "\(count)"
        imageView?.image = icon
        updateColors()
    }

    override var backgroundStyle: NSView.BackgroundStyle {
        didSet {
            updateColors()
        }
    }

    private func updateColors() {
        let isSelected = backgroundStyle == .emphasized
        textField?.textColor = isSelected ? .white : .labelColor
        countBadgeLabel.textColor = isSelected ? NSColor.white.withAlphaComponent(0.8) : .secondaryLabelColor
    }
}

// MARK: - Identifiers

extension NSUserInterfaceItemIdentifier {
    static let sidebarCell = NSUserInterfaceItemIdentifier("SidebarTreeCell")
    static let sidebarColumn = NSUserInterfaceItemIdentifier("SidebarColumn")
}
