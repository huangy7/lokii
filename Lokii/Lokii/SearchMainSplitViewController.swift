import Cocoa

// MARK: - SearchMainSplitViewControllerDelegate

/// 主分栏视图中介协调器代理协议
protocol SearchMainSplitViewControllerDelegate: AnyObject {
    /// 检索结果条目单选变更回调（用于联动更新底部路径栏等）
    func searchMainSplitViewDidSelect(result: FfiSearchResult?)
    /// 检索结果条目双击确认回调（用于触发打开文件动作）
    func searchMainSplitViewDidDoubleClick(result: FfiSearchResult)
    /// 索引重建请求回调
    func searchMainSplitViewDidRequestRebuild()
    /// 虚拟列表首行上移回调，用于将焦点交还搜索输入框
    func searchMainSplitViewDidRequestFocusSearchField()
    /// 检索结果条目被移入废纸篓回调（用于联动外部窗口更新命中项计数等）
    func searchMainSplitViewDidTrashItems(paths: Set<String>, remainingCount: Int)
    /// 检索结果条目被有效消费回调（如访达定位、终端唤起、复制、预览、拖拽等），用于归档有效搜索历史
    func searchMainSplitViewDidConsumeResult()
    /// 左侧目录作用域过滤引起主列表可见结果数变化时回调（用于联动更新底栏命中计数）
    func searchMainSplitViewDidFilterResults(filteredCount: Int, totalCount: Int, activeDirectory: String?)
}

extension SearchMainSplitViewControllerDelegate {
    func searchMainSplitViewDidSelect(result: FfiSearchResult?) {}
    func searchMainSplitViewDidDoubleClick(result: FfiSearchResult) {}
    func searchMainSplitViewDidRequestRebuild() {}
    func searchMainSplitViewDidRequestFocusSearchField() {}
    func searchMainSplitViewDidTrashItems(paths: Set<String>, remainingCount: Int) {}
    func searchMainSplitViewDidConsumeResult() {}
    func searchMainSplitViewDidFilterResults(filteredCount: Int, totalCount: Int, activeDirectory: String?) {}
}

// MARK: - SearchMainSplitViewController

/// 原生 AppKit 双栏分栏容器控制器与单向中介协调器
///
/// 承载左侧目录聚合树（SidebarTreeViewController）与右侧主检索虚拟列表（SearchResultsViewController）的容器组装。
///
/// 作为中介者协调器（Mediator），维护严格的单向数据流与作用域派发：
/// 1. 左栏变更：选中目录前缀时，基于前缀对全量结果集进行范围过滤并推入主列表；
/// 2. 主列表变更：选中文件条目时，通知外部窗口更新路径栏，
///    绝对阻断向左侧目录树的反向过滤或回写，确保数据流向单向无环；
/// 3. 工具栏联动：暴露左栏折叠显隐控制与状态查询接口。
final class SearchMainSplitViewController: NSSplitViewController {
    weak var delegate: SearchMainSplitViewControllerDelegate?

    let sidebarTreeViewController: SidebarTreeViewController
    let searchResultsViewController: SearchResultsViewController

    let sidebarSplitViewItem: NSSplitViewItem
    let contentSplitViewItem: NSSplitViewItem

    private(set) var rawResults: [FfiSearchResult] = []
    private var isUpdatingSearchResults = false
    /// 侧边栏折叠期间推迟了目录树构建的脏标记，待用户展开侧边栏或触发目录过滤时即时补建
    private var isDirectoryTreeDirty = false
    /// 当前结果集是否应自动预选首行（活跃检索为 true，默认空查询列表为 false）
    private var autoSelectFirstResult = false

    /// 当前是否需要立即维护左侧目录聚合树（仅在侧边栏展开或存在生效目录过滤时为 true）
    var needsDirectoryTree: Bool {
        !isSidebarCollapsed || sidebarTreeViewController.selectedPath != nil
    }

    // MARK: - Initialization

    init(
        sidebarTreeViewController: SidebarTreeViewController = SidebarTreeViewController(),
        searchResultsViewController: SearchResultsViewController = SearchResultsViewController()
    ) {
        self.sidebarTreeViewController = sidebarTreeViewController
        self.searchResultsViewController = searchResultsViewController

        // 左栏：目录聚合树，.sidebar 风格，宽度 180~320 pt，可折叠，默认折叠收起
        let sidebarItem = NSSplitViewItem(sidebarWithViewController: sidebarTreeViewController)
        sidebarItem.minimumThickness = 180
        sidebarItem.maximumThickness = 320
        sidebarItem.holdingPriority = NSLayoutConstraint.Priority(260)
        sidebarItem.canCollapse = true
        sidebarItem.isCollapsed = true
        self.sidebarSplitViewItem = sidebarItem

        // 右栏：主检索虚拟列表，.contentList 风格，最小 460 pt，低持有优先级（吸收主视窗横向拉伸），不可折叠以防止塌陷
        let contentItem = NSSplitViewItem(contentListWithViewController: searchResultsViewController)
        contentItem.minimumThickness = 460
        contentItem.holdingPriority = NSLayoutConstraint.Priority(200)
        contentItem.canCollapse = false
        self.contentSplitViewItem = contentItem

        super.init(nibName: nil, bundle: nil)

        addSplitViewItem(sidebarItem)
        addSplitViewItem(contentItem)

        sidebarTreeViewController.delegate = self
        searchResultsViewController.delegate = self
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    // MARK: - Lifecycle

    override func viewDidLoad() {
        super.viewDidLoad()
        splitView.isVertical = true
        splitView.dividerStyle = .thin
    }

    // MARK: - Panel Collapse & Visibility Control

    /// 左侧目录聚合树是否处于折叠隐藏状态
    var isSidebarCollapsed: Bool {
        get {
            sidebarSplitViewItem.isCollapsed
        }
        set {
            sidebarSplitViewItem.animator().isCollapsed = newValue
            if !newValue {
                ensureDirectoryTreeBuiltIfNeeded()
            }
        }
    }

    /// 切换左侧目录聚合树展开与折叠状态（无参调用入口）
    func toggleSidebar() {
        toggleSidebar(nil)
    }

    override func toggleSidebar(_ sender: Any?) {
        super.toggleSidebar(sender)
        if !isSidebarCollapsed {
            ensureDirectoryTreeBuiltIfNeeded()
        }
    }

    override func splitViewDidResizeSubviews(_ notification: Notification) {
        super.splitViewDidResizeSubviews(notification)
        if !isSidebarCollapsed {
            ensureDirectoryTreeBuiltIfNeeded()
        }
    }

    /// 侧边栏由折叠转为展开时，若存在挂起未建的目录聚合树，基于当前全量结果即时补建
    private func ensureDirectoryTreeBuiltIfNeeded() {
        guard isDirectoryTreeDirty else { return }
        isDirectoryTreeDirty = false
        let tree: ResultDirectoryTree? = rawResults.isEmpty ? nil : ResultDirectoryTree.build(from: rawResults)
        let wasUpdating = isUpdatingSearchResults
        isUpdatingSearchResults = true
        defer { isUpdatingSearchResults = wasUpdating }
        sidebarTreeViewController.updateTree(tree)
    }

    // MARK: - Mediator Public API

    /// 派发并同步最新的即时检索结果集与聚合树
    ///
    /// - Parameters:
    ///   - rawResults: 检索后端返回的未经目录筛选的原始命中集合
    ///   - tree: 后台构建的目录聚合前缀树；若为 nil 则使左侧树恢复空状态（除非启用 deferTreeBuild）
    ///   - deferTreeBuild: 当侧边栏折叠且无活动目录过滤时，置 true 跳过侧边栏建树与 OutlineView 刷新
    ///   - autoSelectFirstRow: 非空结果集是否自动预选首行（不抢占搜索框焦点）
    func updateSearchResults(
        rawResults: [FfiSearchResult],
        tree: ResultDirectoryTree?,
        deferTreeBuild: Bool = false,
        autoSelectFirstRow: Bool = false
    ) {
        isUpdatingSearchResults = true
        defer { isUpdatingSearchResults = false }

        self.rawResults = rawResults
        self.autoSelectFirstResult = autoSelectFirstRow

        if deferTreeBuild && !needsDirectoryTree {
            // 侧边栏折叠且无目录过滤时跳过 OutlineView 刷新，标记脏状态留待展开时补建
            isDirectoryTreeDirty = true
        } else {
            isDirectoryTreeDirty = false
            // 1. 同步更新左侧目录聚合树（内部会静默恢复用户先前的展开与选中路径）
            sidebarTreeViewController.updateTree(tree)
        }

        // 2. 基于左侧当前选中的目录作用域过滤全量结果集，并推入主虚拟列表渲染
        let activePrefix = sidebarTreeViewController.selectedPath
        displayResults(byDirectoryPrefix: activePrefix)
    }

    /// 按目录作用域裁剪全量结果并刷新主列表，供首轮渲染、侧边栏选择与面包屑过滤共用
    private func displayResults(byDirectoryPrefix path: String?) {
        let filtered = ResultDirectoryTree.filter(results: rawResults, byDirectoryPrefix: path)
        searchResultsViewController.displayResults(
            filtered,
            preserveSelection: false,
            autoSelectFirstRow: autoSelectFirstResult
        )
        delegate?.searchMainSplitViewDidFilterResults(
            filteredCount: filtered.count,
            totalCount: rawResults.count,
            activeDirectory: path
        )
    }

    /// 基于指定目录路径过滤检索结果，并联动更新侧边栏选中状态
    ///
    /// - Parameter path: 目标过滤目录路径
    func filterByDirectory(path: String) {
        // 1. 若侧边栏当前处于收起状态，展开以提供视觉上下文并按需补建目录树
        if isSidebarCollapsed {
            isSidebarCollapsed = false
        }
        ensureDirectoryTreeBuiltIfNeeded()

        // 2. 在左侧目录树中高亮精确节点或承载单链压缩段的折叠节点（静默选区避免触发双次刷新）
        let wasUpdating = isUpdatingSearchResults
        isUpdatingSearchResults = true
        sidebarTreeViewController.selectDirectory(path: path)
        isUpdatingSearchResults = wasUpdating

        // 3. 核心兜底过滤：无论该路径是否在单链压缩折叠段中，统一执行一次全量结果集前缀裁剪
        displayResults(byDirectoryPrefix: path)
    }

    // MARK: - Notifications & Localization

    /// 语言切换由本中介统一下发至各子栏，子栏不再各自订阅同一通知
    @objc func onLanguageChanged() {
        searchResultsViewController.onLanguageChanged()
    }
}

// MARK: - SidebarTreeViewControllerDelegate

extension SearchMainSplitViewController: SidebarTreeViewControllerDelegate {
    func sidebarDidSelectDirectory(path: String?) {
        guard !isUpdatingSearchResults else { return }
        displayResults(byDirectoryPrefix: path)
    }
}

// MARK: - SearchResultsViewControllerDelegate

extension SearchMainSplitViewController: SearchResultsViewControllerDelegate {
    func searchResultsDidSelect(result: FfiSearchResult?) {
        // 单向流动：主列表单选变更通知外部窗口，绝不反向改动左侧目录树
        delegate?.searchMainSplitViewDidSelect(result: result)
    }

    func searchResultsDidDoubleClick(result: FfiSearchResult) {
        delegate?.searchMainSplitViewDidDoubleClick(result: result)
    }

    func searchResultsDidRequestRebuild() {
        delegate?.searchMainSplitViewDidRequestRebuild()
    }

    func searchResultsDidRequestFocusSearchField() {
        delegate?.searchMainSplitViewDidRequestFocusSearchField()
    }

    func searchResultsDidTrashItems(paths: Set<String>) {
        rawResults.removeAll { paths.contains($0.path) }
        if needsDirectoryTree {
            isDirectoryTreeDirty = false
            let tree = ResultDirectoryTree.build(from: rawResults)
            sidebarTreeViewController.updateTree(tree)
        } else {
            isDirectoryTreeDirty = true
        }
        delegate?.searchMainSplitViewDidTrashItems(paths: paths, remainingCount: rawResults.count)
    }

    func searchResultsDidRequestScopeFilter(path: String) {
        filterByDirectory(path: path)
    }

    func searchResultsDidConsumeResult() {
        delegate?.searchMainSplitViewDidConsumeResult()
    }
}
