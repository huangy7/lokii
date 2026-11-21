import Cocoa
import UniformTypeIdentifiers
import Quartz

// MARK: - Standalone Test Harness for SearchResultsViewController

let searchResultsSourcePath = "Lokii/Lokii/SearchResultsViewController.swift"
guard FileManager.default.fileExists(atPath: searchResultsSourcePath) else {
    fputs("tests/search_results_view_controller_test.swift: error: 找不到 \(searchResultsSourcePath)\n", stderr)
    exit(1)
}

guard let searchResultsSource = try? String(contentsOfFile: searchResultsSourcePath, encoding: .utf8) else {
    fputs("tests/search_results_view_controller_test.swift: error: 无法读取 \(searchResultsSourcePath)\n", stderr)
    exit(1)
}

guard let l10nSource = try? String(contentsOfFile: "Lokii/Lokii/L10n.swift", encoding: .utf8) else {
    fputs("tests/search_results_view_controller_test.swift: error: 无法读取 L10n.swift\n", stderr)
    exit(1)
}

guard let localizedMenuSource = try? String(contentsOfFile: "Lokii/Lokii/LocalizedMenu.swift", encoding: .utf8) else {
    fputs("tests/search_results_view_controller_test.swift: error: 无法读取 LocalizedMenu.swift\n", stderr)
    exit(1)
}

guard let terminalLauncherSource = try? String(contentsOfFile: "Lokii/Lokii/TerminalLauncher.swift", encoding: .utf8) else {
    fputs("tests/search_results_view_controller_test.swift: error: 无法读取 TerminalLauncher.swift\n", stderr)
    exit(1)
}

// Mock FfiSearchResult
let mockFfiSource = """
public struct FfiMatchRange: Equatable, Hashable {
    public var start: UInt32
    public var end: UInt32
    public init(start: UInt32 = 0, end: UInt32 = 0) {
        self.start = start
        self.end = end
    }
}

public struct FfiSearchResult: Equatable, Hashable {
    public var name: String
    public var path: String
    public var `extension`: String
    public var isDir: Bool
    public var size: UInt64
    public var created: Int64
    public var modified: Int64
    public var matchPositions: [FfiMatchRange]
    public var parentMatchPositions: [FfiMatchRange]
    public var isOffline: Bool
    public var volumeName: String?

    public init(
        name: String,
        path: String,
        `extension`: String,
        isDir: Bool,
        size: UInt64,
        created: Int64,
        modified: Int64,
        matchPositions: [FfiMatchRange] = [],
        parentMatchPositions: [FfiMatchRange] = [],
        isOffline: Bool = false,
        volumeName: String? = nil
    ) {
        self.name = name
        self.path = path
        self.extension = `extension`
        self.isDir = isDir
        self.size = size
        self.created = created
        self.modified = modified
        self.matchPositions = matchPositions
        self.parentMatchPositions = parentMatchPositions
        self.isOffline = isOffline
        self.volumeName = volumeName
    }
}
"""

let runnerSource = """
import Cocoa
import UniformTypeIdentifiers
import Quartz

\(mockFfiSource)
\(l10nSource)
\(localizedMenuSource)
\(terminalLauncherSource)
\(searchResultsSource)

func assertEqual<T: Equatable>(_ actual: T, _ expected: T, _ message: String, file: StaticString = #file, line: UInt = #line) {
    if actual != expected {
        fputs("断言失败: [\\(file):\\(line)] \\(message)\\n实际值: \\(actual)\\n期望值: \\(expected)\\n", stderr)
        exit(1)
    }
}

func assertTrue(_ condition: Bool, _ message: String, file: StaticString = #file, line: UInt = #line) {
    if !condition {
        fputs("断言失败: [\\(file):\\(line)] \\(message)\\n", stderr)
        exit(1)
    }
}

final class MockDelegate: SearchResultsViewControllerDelegate {
    var selectedResult: FfiSearchResult?
    var selectCallCount = 0
    var doubleClickedResult: FfiSearchResult?
    var doubleClickCallCount = 0
    var rebuildCallCount = 0

    func searchResultsDidSelect(result: FfiSearchResult?) {
        selectedResult = result
        selectCallCount += 1
    }

    func searchResultsDidDoubleClick(result: FfiSearchResult) {
        doubleClickedResult = result
        doubleClickCallCount += 1
    }

    func searchResultsDidRequestRebuild() {
        rebuildCallCount += 1
    }

    var focusSearchFieldCallCount = 0

    func searchResultsDidRequestFocusSearchField() {
        focusSearchFieldCallCount += 1
    }

    var trashedPaths: Set<String> = []
    var trashCallCount = 0

    func searchResultsDidTrashItems(paths: Set<String>) {
        trashedPaths = paths
        trashCallCount += 1
    }
}

print("[1/12] 测试控制器初始化与内聚底栏视图层次构建...")
UserDefaults.standard.removeObject(forKey: "NSTableView Columns SearchResultsTable")
let controller = SearchResultsViewController()
let _ = controller.view // 触发 loadView 与 viewDidLoad

assertTrue(controller.emptyStateContainer.isHidden, "首次结果集到达前不应闪现未找到匹配项")
controller.displayResults([], preserveSelection: false)
assertTrue(!controller.emptyStateContainer.isHidden, "没有结果时应显示空态说明")
assertEqual(controller.emptyTitleLabel.stringValue, L("search.empty.title"), "普通无结果应显示匹配项标题")
assertEqual(controller.emptySubtitleLabel.stringValue, L("search.empty.subtitle"), "普通无结果应显示检索建议")
assertTrue(controller.emptyRuleRow.isHidden, "普通无结果应隐藏排除规则信息")

controller.emptyState = .excluded(rule: "Contents")
assertEqual(controller.emptyTitleLabel.stringValue, L("search.empty.excluded.title"), "排除路径应显示未索引标题")
assertEqual(controller.emptyRuleNameLabel.stringValue, "Contents", "规则名应保留后端返回的命中片段")
assertTrue(!controller.emptyRuleRow.isHidden, "排除路径应显示规则信息")
let manageRulesButton = controller.emptyStateContainer.arrangedSubviews.compactMap { $0 as? NSButton }.first!
assertTrue(!manageRulesButton.isHidden, "排除路径应提供管理规则入口")
var manageRulesCallCount = 0
controller.onManageIndexRules = { manageRulesCallCount += 1 }
manageRulesButton.sendAction(manageRulesButton.action!, to: manageRulesButton.target)
assertEqual(manageRulesCallCount, 1, "点击管理按钮应派发设置导航回调")
controller.emptyTitleLabel.stringValue = "旧语言标题"
controller.onLanguageChanged()
assertEqual(controller.emptyTitleLabel.stringValue, L("search.empty.excluded.title"), "语言变化应重新渲染当前空态")
assertEqual(controller.emptyRuleNameLabel.stringValue, "Contents", "语言变化不应丢失命中规则")

controller.emptyState = .indexing
assertEqual(controller.emptyTitleLabel.stringValue, L("search.empty.indexing.title"), "索引未完成时应显示建库标题")
assertEqual(controller.emptySubtitleLabel.stringValue, L("search.empty.indexing.subtitle"), "建库期间应说明暂时无法搜索的原因")
assertTrue(controller.emptyRuleRow.isHidden && manageRulesButton.isHidden, "建库期间应隐藏规则信息与管理按钮")
controller.emptyState = .noMatches

assertTrue(controller.view.subviews.contains(controller.tableScrollView), "根视图应包含 tableScrollView")
assertTrue(controller.view.subviews.contains(controller.separator), "根视图应包含分割线")
assertTrue(controller.view.subviews.contains(controller.bottomBar), "根视图应包含 bottomBar")
assertEqual(controller.tableScrollView.automaticallyAdjustsContentInsets, false, "tableScrollView 必须显式禁用自动边距推导")
assertEqual(controller.tableScrollView.contentInsets.top, 0, "tableScrollView 顶部内边距应为 0")
assertEqual(controller.tableScrollView.contentInsets.bottom, 0, "tableScrollView 底部内边距应为 0")
let topConstraint = controller.view.constraints.first {
    $0.firstItem as? NSScrollView == controller.tableScrollView && $0.firstAttribute == .top
}
assertTrue(topConstraint?.secondItem is NSLayoutGuide, "tableScrollView 顶端必须锚定至 safeAreaLayoutGuide")
assertEqual(controller.tableView.numberOfColumns, 4, "虚拟列表应具备 4 个排版列 (名称/路径/修改时间/大小)")
assertTrue(controller.tableView.autosaveTableColumns, "虚拟列表应开启列配置自动持久化")
assertEqual(controller.tableView.autosaveName, "SearchResultsTable", "虚拟列表 autosaveName 应为 SearchResultsTable")
assertTrue(controller.tableView.usesAlternatingRowBackgroundColors, "虚拟列表应启用斑马纹交替背景色")
assertEqual(controller.tableView.rowHeight, 24, "行高应为 24 pt")

// 验证底栏内部控件装配
assertTrue(controller.bottomBar.subviews.contains(controller.pathBar), "底栏应包含 pathBar")
assertTrue(controller.bottomBar.subviews.contains(controller.statusLabel), "底栏应包含 statusLabel")
assertTrue(controller.bottomBar.subviews.contains(controller.inlineProgressIndicator), "底栏应包含 inlineProgressIndicator")
assertTrue(controller.bottomBar.subviews.contains(controller.rebuildContainer), "底栏应包含 rebuildContainer")
assertTrue(controller.rebuildContainer.subviews.contains(controller.rebuildButton), "重建容器应包含 rebuildButton")

let bottomBarHeightConstraint = controller.bottomBar.constraints.first { $0.firstAttribute == .height }
assertEqual(bottomBarHeightConstraint?.constant, 28, "底栏高度约束应为 28 pt")

print("[2/12] 测试展示检索结果与单元格渲染 (displayResults)...")
let sampleItems = [
    FfiSearchResult(name: "alpha.rs", path: "/tmp/src/alpha.rs", extension: "rs", isDir: false, size: 2048, created: 100, modified: 300, matchPositions: [FfiMatchRange(start: 0, end: 5)]),
    FfiSearchResult(name: "beta.md", path: "/tmp/docs/beta.md", extension: "md", isDir: false, size: 1024, created: 100, modified: 200),
    FfiSearchResult(name: "gamma_dir", path: "/tmp/gamma_dir", extension: "", isDir: true, size: 0, created: 100, modified: 100),
]

controller.displayResults(sampleItems, preserveSelection: false)
assertTrue(controller.emptyStateContainer.isHidden, "返回结果后应立即隐藏空态")
assertEqual(controller.results.count, 3, "控制器结果集合应包含 3 个条目")
assertEqual(controller.numberOfRows(in: controller.tableView), 3, "表格数据源行数应为 3")

let nameCol = controller.tableView.tableColumns.first { $0.identifier == .columnName }
let pathCol = controller.tableView.tableColumns.first { $0.identifier == .columnPath }
let sizeCol = controller.tableView.tableColumns.first { $0.identifier == .columnSize }

let nameCellView = controller.tableView(controller.tableView, viewFor: nameCol, row: 0)
assertTrue(nameCellView is SearchNameTableCellView, "名称列应返回定制高亮单元格 SearchNameTableCellView")
assertEqual(nameCellView?.toolTip, "alpha.rs", "名称列单元格 toolTip 应为完整文件名")
assertEqual((nameCellView as? SearchNameTableCellView)?.textField?.toolTip, "alpha.rs", "名称列文本框 toolTip 应为完整文件名")

let nameCellViewNoMatch = controller.tableView(controller.tableView, viewFor: nameCol, row: 1) as? SearchNameTableCellView
assertTrue(nameCellViewNoMatch != nil, "名称列无匹配范围时应能正常渲染")
assertEqual(nameCellViewNoMatch?.textField?.attributedStringValue.string, "beta.md", "未高亮单元格文本内容应与文件名一致")
assertEqual(nameCellViewNoMatch?.toolTip, "beta.md", "未高亮单元格 toolTip 应为完整文件名")

let pathCellView = controller.tableView(controller.tableView, viewFor: pathCol, row: 0) as? NSTableCellView
assertEqual(pathCellView?.textField?.stringValue, "/tmp/src", "路径列应显示父目录路径")
assertEqual(pathCellView?.toolTip, "/tmp/src/alpha.rs", "路径列单元格 toolTip 应为完整绝对路径")
assertEqual(pathCellView?.textField?.toolTip, "/tmp/src/alpha.rs", "路径列文本框 toolTip 应为完整绝对路径")

let sizeCellView = controller.tableView(controller.tableView, viewFor: sizeCol, row: 2) as? NSTableCellView
assertEqual(sizeCellView?.textField?.stringValue, "--", "目录行大小应显示为 --")

print("[3/12] 测试表头多字段排序联动 (sortDescriptorsDidChange)...")
// 按名称降序排序
let nameDescSort = NSSortDescriptor(key: "name", ascending: false)
controller.tableView.sortDescriptors = [nameDescSort]
controller.tableView(controller.tableView, sortDescriptorsDidChange: [])
assertEqual(controller.results[0].name, "gamma_dir", "按名称降序排在首位的应为 gamma_dir")
assertEqual(controller.results[2].name, "alpha.rs", "按名称降序排在末位的应为 alpha.rs")

// 按大小降序排序
let sizeDescSort = NSSortDescriptor(key: "size", ascending: false)
controller.tableView.sortDescriptors = [sizeDescSort]
controller.tableView(controller.tableView, sortDescriptorsDidChange: [])
assertEqual(controller.results[0].name, "alpha.rs", "按大小降序排在首位的应为 alpha.rs (2048 bytes)")
assertEqual(controller.results[2].name, "gamma_dir", "按大小降序排在末位的应为 gamma_dir (0 bytes)")

print("[4/12] 测试保留选区智能恢复 (preserveSelection)...")
// 选中 beta.md
controller.selectFirstRow()
let selectedItem = controller.results[1]
let targetIndex = controller.results.firstIndex(of: selectedItem)!
controller.tableView.selectRowIndexes(IndexSet(integer: targetIndex), byExtendingSelection: false)
assertEqual(controller.selectedResult?.path, selectedItem.path, "选区应已指向目标条目")

// 以倒序重新加载，启用 preserveSelection: true
let reversedItems = Array(sampleItems.reversed())
controller.displayResults(reversedItems, preserveSelection: true)
assertEqual(controller.selectedResult?.path, selectedItem.path, "刷新后应恢复原条目选中状态")

// 重新加载不包含已选条目的子集，验证安全清空
let filteredItems = [sampleItems[0]]
controller.displayResults(filteredItems, preserveSelection: true)
assertEqual(controller.selectedResult, nil, "已选条目不在新集合中时应安全清空选区")

print("[5/12] 测试 selectFirstRow 与代理回调机制...")
let mockDelegate = MockDelegate()
controller.delegate = mockDelegate

controller.displayResults(sampleItems, preserveSelection: false)
controller.selectFirstRow()
assertEqual(controller.selectedResult?.name, "alpha.rs", "selectFirstRow 应选中首行")
assertEqual(mockDelegate.selectedResult?.name, "alpha.rs", "代理应接收到首行选区变更回调")

controller.openSelected()
assertEqual(mockDelegate.doubleClickedResult?.name, "alpha.rs", "openSelected 应触发首选条目确认回调")

print("[6/12] 测试剪贴板拷贝行为与右键菜单结构 (copyFile, copyPath, copyName & contextMenu)...")
controller.selectFirstRow()
controller.copyFile()
let copiedUrls = NSPasteboard.general.readObjects(forClasses: [NSURL.self], options: nil) as? [URL]
assertEqual(copiedUrls?.first?.path, "/tmp/src/alpha.rs", "copyFile 应将所选条目作为文件对象写入系统剪贴板")
let pasteboardFilePath = NSPasteboard.general.string(forType: .string)
assertEqual(pasteboardFilePath, "/tmp/src/alpha.rs", "copyFile 应同时将所选条目路径文本写入系统剪贴板作为兼容降级")

controller.copyPath()
let pasteboardPath = NSPasteboard.general.string(forType: .string)
assertEqual(pasteboardPath, "/tmp/src/alpha.rs", "copyPath 应将所选条目路径写入系统剪贴板")

controller.copyName()
let pasteboardName = NSPasteboard.general.string(forType: .string)
assertEqual(pasteboardName, "alpha.rs", "copyName 应将所选条目名称写入系统剪贴板")

// 校验右键菜单项结构与动作绑定
guard let menu = controller.tableView.menu else {
    fputs("断言失败: tableView.menu 不能为空\\n", stderr)
    exit(1)
}
assertEqual(menu.items.count, 15, "右键上下文菜单应包含 12 个功能项加 3 个分隔线")
assertEqual(menu.items[0].action, #selector(controller.openSelected), "第 0 项应绑定 openSelected")
assertTrue(menu.items[1].hasSubmenu, "第 1 项应为打开方式子菜单")
assertEqual(menu.items[2].action, #selector(controller.quickLookSelected), "第 2 项应绑定 quickLookSelected")
assertEqual(menu.items[2].keyEquivalent, " ", "quickLookSelected 快捷键应为 Space")
assertTrue(menu.items[2].keyEquivalentModifierMask.isEmpty, "quickLookSelected 修饰键掩码应为空")
assertEqual(menu.items[3].action, #selector(controller.revealSelected), "第 3 项应绑定 revealSelected")
assertEqual(menu.items[4].action, #selector(controller.filterByDirectorySelected), "第 4 项应绑定 filterByDirectorySelected")
assertEqual(menu.items[5].action, #selector(controller.openInTerminalSelected), "第 5 项应绑定 openInTerminalSelected")
assertEqual(menu.items[6].action, #selector(controller.showInfoSelected), "第 6 项应绑定 showInfoSelected")
assertEqual(menu.items[6].keyEquivalent, "i", "showInfoSelected 快捷键应为 i")
assertTrue(menu.items[7].isSeparatorItem, "第 7 项应为分隔线")
assertEqual(menu.items[8].action, #selector(controller.copyFile), "第 8 项应绑定 copyFile")
assertEqual(menu.items[8].keyEquivalent, "c", "copyFile 快捷键应为 c")
assertEqual(menu.items[9].action, #selector(controller.copyPath), "第 9 项应绑定 copyPath")
assertEqual(menu.items[10].action, #selector(controller.copyName), "第 10 项应绑定 copyName")
assertTrue(menu.items[11].isSeparatorItem, "第 11 项应为分隔线")
assertEqual(menu.items[12].action, #selector(controller.excludeDirectorySelected), "第 12 项应绑定 excludeDirectorySelected")
assertTrue(menu.items[13].isSeparatorItem, "第 13 项应为分隔线")
assertEqual(menu.items[14].action, #selector(controller.moveToTrashSelected), "第 14 项应绑定 moveToTrashSelected")
assertEqual(menu.items[14].keyEquivalent, String(format: "%c", NSDeleteCharacter), "moveToTrashSelected 快捷键应为 Delete")
assertEqual(menu.items[14].keyEquivalentModifierMask, [.command], "moveToTrashSelected 修饰键应为 Command")
assertTrue(menu.allowsContextMenuPlugIns, "tableView.menu 必须开启 allowsContextMenuPlugIns 以由 AppKit 原生托管系统服务")

// 校验菜单项可用性验证逻辑 (validateMenuItem)
for item in menu.items where !item.isSeparatorItem && !item.hasSubmenu {
    assertTrue(controller.validateMenuItem(item), "有选中项时上下文菜单项 \\(item.title) 应通过校验")
}

// 校验 openInTerminalSelected 单选与多选条目在终端中打开行为
var interceptedTerminalURLs: [URL] = []
controller.openInTerminalHandler = { urls in
    interceptedTerminalURLs = urls
}

controller.selectFirstRow()
interceptedTerminalURLs.removeAll()
controller.openInTerminalSelected()
assertEqual(interceptedTerminalURLs.map { $0.path }, ["/tmp/src/alpha.rs"], "单选条目时 openInTerminalSelected 应派发所选项路径")

controller.tableView.selectRowIndexes(IndexSet([0, 1]), byExtendingSelection: false)
interceptedTerminalURLs.removeAll()
controller.openInTerminalSelected()
assertEqual(interceptedTerminalURLs.map { $0.path }, ["/tmp/src/alpha.rs", "/tmp/docs/beta.md"], "多选条目时 openInTerminalSelected 应批量派发全部选中项路径")

// 校验 revealSelected 单选与多选条目在访达中高亮定位行为，以及底栏点击与面包屑定位联动
var interceptedRevealURLs: [URL] = []
controller.revealFilesHandler = { urls in
    interceptedRevealURLs = urls
}

// 1. 单选条目触发 revealSelected：应精准传递该条目 URL，严禁误触发文件打开或进入目录
controller.selectFirstRow()
interceptedRevealURLs.removeAll()
controller.revealSelected()
assertEqual(interceptedRevealURLs.map { $0.path }, ["/tmp/src/alpha.rs"], "单选条目时 revealSelected 应通过 revealFilesHandler 定位所选项路径")

// 2. 多选条目触发 revealSelected：应批量传递全部选中条目的 URL
controller.tableView.selectRowIndexes(IndexSet([0, 1]), byExtendingSelection: false)
interceptedRevealURLs.removeAll()
controller.revealSelected()
assertEqual(interceptedRevealURLs.map { $0.path }, ["/tmp/src/alpha.rs", "/tmp/docs/beta.md"], "多选条目时 revealSelected 应批量定位全部选中项路径")

// 3. 点击底栏路径项触发 pathBarClicked：应通过 revealFilesHandler 定位该节点路径
controller.selectFirstRow()
controller.updateBottomPath(with: "/tmp/src/alpha.rs")
interceptedRevealURLs.removeAll()
if let pathBarAction = controller.pathBar.action {
    _ = controller.pathBar.target?.perform(pathBarAction, with: controller.pathBar)
}
assertEqual(interceptedRevealURLs.map { $0.path }, ["/tmp/src/alpha.rs"], "点击底栏路径栏项应通过 revealFilesHandler 定位节点路径")

// 校验 SearchTableView 键盘事件分发与 ⌘⌫ 按键等效处理
let deleteEvent = NSEvent.keyEvent(
    with: .keyDown,
    location: .zero,
    modifierFlags: [.command],
    timestamp: 0,
    windowNumber: 0,
    context: nil,
    characters: "\\u{7f}",
    charactersIgnoringModifiers: "\\u{7f}",
    isARepeat: false,
    keyCode: 51
)!
assertTrue(controller.tableView.performKeyEquivalent(with: deleteEvent), "tableView.performKeyEquivalent 应识别并拦截 ⌘⌫")

// 校验 moveToTrashSelected 异步执行、依赖注入、选区顺延与中介者代理通知
var interceptedTrashURLs: [URL] = []
controller.trashItemHandler = { url in
    interceptedTrashURLs.append(url)
    return url
}
var confirmRequested = false
controller.confirmTrashHandler = { count, completion in
    confirmRequested = true
    completion(true)
}

let initialCount = controller.results.count
controller.selectFirstRow()
controller.moveToTrashSelected()

// 等待后台队列文件 I/O 与主线程 UI 回调完成
let deadline = Date().addingTimeInterval(2.0)
while mockDelegate.trashCallCount == 0 && Date() < deadline {
    RunLoop.current.run(mode: .default, before: Date(timeIntervalSinceNow: 0.05))
}

assertEqual(mockDelegate.trashCallCount, 1, "移到废纸篓完成后应向代理派发 searchResultsDidTrashItems 回调")
assertTrue(mockDelegate.trashedPaths.contains("/tmp/src/alpha.rs"), "代理回调的路径集合中应包含所删除的项目路径")
assertEqual(interceptedTrashURLs.first?.path, "/tmp/src/alpha.rs", "trashItemHandler 应接收到对应文件 URL")
assertEqual(controller.results.count, initialCount - 1, "本地检索结果集中应已剔除被删除的项目")
assertEqual(controller.selectedResult?.name, "beta.md", "删除首行后选区应顺延聚焦至原第 1 项 (beta.md)")

// 校验自愈容错：当目标文件在磁盘上已不存在（抛出 NSFileNoSuchFileError）时，静默自愈剔除而不报警
let countBeforeStale = controller.results.count
controller.trashItemHandler = { _ in
    throw NSError(domain: NSCocoaErrorDomain, code: NSFileNoSuchFileError, userInfo: nil)
}
controller.moveToTrashSelected()
let deadlineStale = Date().addingTimeInterval(2.0)
while mockDelegate.trashCallCount == 1 && Date() < deadlineStale {
    RunLoop.current.run(mode: .default, before: Date(timeIntervalSinceNow: 0.05))
}
assertEqual(mockDelegate.trashCallCount, 2, "遇到已不存在的文件时应自愈完成并向代理派发移除回调")
assertEqual(controller.results.count, countBeforeStale - 1, "已不存在的幽灵条目应自愈从本地列表中剔除")

print("[7/12] 测试 Quick Look 快速预览响应者协议与数据源契约...")
// 无选中条目时
controller.displayResults([], preserveSelection: false)
assertEqual(controller.acceptsPreviewPanelControl(nil), false, "列表为空或无选区时 acceptsPreviewPanelControl 应返回 false")
assertEqual(controller.numberOfPreviewItems(in: nil), 0, "无选中时预览项数量应为 0")
for item in menu.items where !item.isSeparatorItem {
    assertTrue(!controller.validateMenuItem(item), "无选中项时上下文菜单项 \\(item.title) 应禁用")
}

// 有选中条目时
controller.displayResults(sampleItems, preserveSelection: false)
controller.selectFirstRow()
assertEqual(controller.acceptsPreviewPanelControl(nil), true, "存在选中条目时 acceptsPreviewPanelControl 应返回 true")
assertEqual(controller.numberOfPreviewItems(in: nil), 1, "存在选中条目时预览项数量应为 1")
let previewItem = controller.previewPanel(nil, previewItemAt: 0)
assertEqual((previewItem as? NSURL)?.path, "/tmp/src/alpha.rs", "预览项路径应与当前选中条目一致")

// sourceFrameOnScreenFor 视口与屏幕安全降级校验：脱离活跃视窗时应安全回退为 .zero
let offscreenFrame = controller.previewPanel(nil, sourceFrameOnScreenFor: previewItem)
assertEqual(offscreenFrame, NSRect.zero, "脱离屏幕视窗时 sourceFrameOnScreenFor 应安全回退为 .zero")

print("[8/12] 测试底栏状态更新、进度指示与重建动作代理派发...")
// 路径更新
controller.updateBottomPath(with: "/tmp/src/alpha.rs")
assertEqual(controller.pathBar.url?.path, "/tmp/src/alpha.rs", "updateBottomPath 应正确设置路径")
assertEqual(controller.pathBar.toolTip, "/tmp/src/alpha.rs", "updateBottomPath 应同步设置底栏 toolTip 为完整路径")
controller.updateBottomPath(with: nil)
assertEqual(controller.pathBar.url, nil, "updateBottomPath(with: nil) 应清空路径")
assertEqual(controller.pathBar.toolTip, nil, "updateBottomPath(with: nil) 应同步清空底栏 toolTip")

// 状态文本更新
controller.updateStatusText("已匹配 10 项")
assertEqual(controller.statusLabel.stringValue, "已匹配 10 项", "updateStatusText 应正确刷新状态文本")

// 状态栏微反馈与权威状态隔离测试
controller.updateStatusText("已索引 100 个文件")
assertEqual(controller.statusLabel.stringValue, "已索引 100 个文件", "updateStatusText 正常更新持久状态")

controller.notifyBackgroundIndexingActive()
assertEqual(controller.statusLabel.stringValue, L("search.backgroundRefresh"), "触发后台刷新后状态栏展示临时刷新文案")
assertEqual(controller.statusLabel.textColor, NSColor.controlAccentColor, "临时文案应使用 Accent 强调色")
assertTrue(!controller.statusFeedbackCapsule.isHidden, "触发后台刷新后应显现轻量气泡")
assertTrue(controller.statusFeedbackCapsule.isSpinning, "气泡内应持续旋转指示微动效")
assertEqual(controller.statusFeedbackCapsule.label.stringValue, L("search.backgroundRefresh"), "气泡内文案应匹配后台刷新提示")

// 在展示临时文案期间，后台进度更新不冲刷屏幕，但静默更新持久状态
controller.updateStatusText("已索引 200 个文件")
assertEqual(controller.statusLabel.stringValue, L("search.backgroundRefresh"), "临时文案展示期间不被后台进度通知冲掉")

// 取消临时状态时，平滑恢复最新的持久状态
controller.cancelTemporaryStatus()
assertEqual(controller.statusLabel.stringValue, "已索引 200 个文件", "取消临时状态后应恢复最新的持久状态")
assertEqual(controller.statusLabel.textColor, NSColor.secondaryLabelColor, "恢复后文本颜色复位为 secondaryLabelColor")
assertTrue(controller.statusFeedbackCapsule.isHidden, "取消临时状态后气泡应隐藏")
assertTrue(!controller.statusFeedbackCapsule.isSpinning, "取消临时状态后气泡旋转应停止")

// 内联进度指示器显隐控制
controller.setInlineProgressVisible(true)
assertEqual(controller.inlineProgressIndicator.isHidden, false, "setInlineProgressVisible(true) 应显示指示器")
controller.setInlineProgressVisible(false)
assertEqual(controller.inlineProgressIndicator.isHidden, true, "setInlineProgressVisible(false) 应隐藏指示器")

// 重建按钮状态与动画
controller.setRebuildEnabled(true)
assertEqual(controller.rebuildButton.isEnabled, true, "setRebuildEnabled(true) 应启用重建按钮")
controller.setRebuildSpinning(true)
assertTrue(controller.rebuildButton.layer?.animation(forKey: "spin") != nil, "setRebuildSpinning(true) 应附加旋转动画")
controller.setRebuildSpinning(false)
assertTrue(controller.rebuildButton.layer?.animation(forKey: "spin") == nil, "setRebuildSpinning(false) 应移除旋转动画")

// 重建按钮点击事件代理派发
let mockRebuildDelegate = MockDelegate()
controller.delegate = mockRebuildDelegate
controller.rebuildButton.sendAction(controller.rebuildButton.action!, to: controller.rebuildButton.target)
assertEqual(mockRebuildDelegate.rebuildCallCount, 1, "点击重建按钮应通过代理派发 searchResultsDidRequestRebuild")

print("[9/12] 测试键盘导航焦点接力与首行上移回调 (focusResults & SearchTableView.moveUp)...")
let navDelegate = MockDelegate()
controller.delegate = navDelegate

// 1. 测试空结果集时的守卫
controller.displayResults([], preserveSelection: false)
let emptyFocusResult = controller.focusResults()
assertEqual(emptyFocusResult, false, "空结果集调用 focusResults 应安全返回 false")

// 2. 恢复结果集并测试 focusResults 选行与焦点激活
controller.displayResults(sampleItems, preserveSelection: false)
let focusRow1Result = controller.focusResults(row: 1)
assertEqual(focusRow1Result, true, "存在结果集时 focusResults 应返回 true")
assertEqual(controller.tableView.selectedRow, 1, "focusResults(row: 1) 应选中第 1 行")

let upArrowEvent = NSEvent.keyEvent(
    with: .keyDown,
    location: .zero,
    modifierFlags: [],
    timestamp: 0,
    windowNumber: 0,
    context: nil,
    characters: "\\u{F700}",
    charactersIgnoringModifiers: "\\u{F700}",
    isARepeat: false,
    keyCode: 126
)!

// 3. 测试非首行时按方向上键正常向上选行，不触发回焦
controller.tableView.keyDown(with: upArrowEvent)
assertEqual(controller.tableView.selectedRow, 0, "第 1 行按方向上键应自然移动到第 0 行")
assertEqual(navDelegate.focusSearchFieldCallCount, 0, "非首行按方向上键不应触发回焦搜索框代理")

// 4. 测试首行（selectedRow == 0）时按方向上键触发回焦回调
controller.tableView.keyDown(with: upArrowEvent)
assertEqual(controller.tableView.selectedRow, 0, "第 0 行按方向上键应保持在第 0 行")
assertEqual(navDelegate.focusSearchFieldCallCount, 1, "第 0 行按方向上键应触发 searchResultsDidRequestFocusSearchField 代理")

// 5. 测试带有 ⌘ 修饰键的方向上键（⌘↑）不触发回焦代理，保留原生滚动行为
let cmdUpArrowEvent = NSEvent.keyEvent(
    with: .keyDown,
    location: .zero,
    modifierFlags: [.command],
    timestamp: 0,
    windowNumber: 0,
    context: nil,
    characters: "\\u{F700}",
    charactersIgnoringModifiers: "\\u{F700}",
    isARepeat: false,
    keyCode: 126
)!
controller.tableView.keyDown(with: cmdUpArrowEvent)
assertEqual(navDelegate.focusSearchFieldCallCount, 1, "带有 ⌘ 修饰键的方向上键不应触发回焦代理")

// 6. 测试带有 ⇧ 修饰键的方向上键（⇧↑）不触发回焦代理，保留原生多选行为
let shiftUpArrowEvent = NSEvent.keyEvent(
    with: .keyDown,
    location: .zero,
    modifierFlags: [.shift],
    timestamp: 0,
    windowNumber: 0,
    context: nil,
    characters: "\\u{F700}",
    charactersIgnoringModifiers: "\\u{F700}",
    isARepeat: false,
    keyCode: 126
)!
controller.tableView.keyDown(with: shiftUpArrowEvent)
assertEqual(navDelegate.focusSearchFieldCallCount, 1, "带有 ⇧ 修饰键的方向上键不应触发回焦代理")

print("[10/12] 测试排版密度模型与动态字号/行高/图标/列宽联动 (applyFontSize)...")
// 测试应用紧凑字号 11pt
controller.applyFontSize(CGFloat(TableDensity.compact.rawValue))
assertEqual(controller.currentFontSize, 11, "当前生效字号应为 11 pt")
assertEqual(controller.tableView.rowHeight, 21, "紧凑模式行高应为 21 pt")
assertEqual(controller.currentIconSize, 14, "紧凑模式图标尺寸应为 14 pt")

// 测试应用舒适字号 15pt
controller.applyFontSize(CGFloat(TableDensity.comfortable.rawValue))
assertEqual(controller.currentFontSize, 15, "当前生效字号应为 15 pt")
assertEqual(controller.tableView.rowHeight, 27, "舒适模式行高应为 27 pt")
assertEqual(controller.currentIconSize, 18, "舒适模式图标尺寸应为 18 pt")

// 测试应用大号字号 17pt
controller.applyFontSize(CGFloat(TableDensity.large.rawValue))
assertEqual(controller.currentFontSize, 17, "当前生效字号应为 17 pt")
assertEqual(controller.tableView.rowHeight, 30, "大号模式行高应为 30 pt")
assertEqual(controller.currentIconSize, 20, "大号模式图标尺寸应为 20 pt")

// 恢复标准字号 13pt
controller.applyFontSize(CGFloat(TableDensity.standard.rawValue))
assertEqual(controller.currentFontSize, 13, "当前生效字号应恢复为 13 pt")
assertEqual(controller.tableView.rowHeight, 24, "标准模式行高应为 24 pt")
assertEqual(controller.currentIconSize, 16, "标准模式图标尺寸应为 16 pt")

print("[11/12] 测试完全磁盘访问权限受限提示与底栏路径让位互斥逻辑 (updateFDAStatus & updateBottomPath)...")
// 1. 模拟未获得完全磁盘访问权限，未选中文件时应呈现提示按钮并隐藏路径栏
controller.updateFDAStatus(hasFDA: false)
controller.updateBottomPath(with: nil)
assertEqual(controller.fdaHintButton.isHidden, false, "未获得 FDA 权限且无选中文件时应展示受限提示按钮")
assertEqual(controller.pathBar.isHidden, true, "未获得 FDA 权限且无选中文件时应隐藏路径栏")

// 2. 选中具体文件时，路径展示优先，受限提示应主动让位隐藏
controller.updateBottomPath(with: "/System/Library/CoreServices/Finder.app")
assertEqual(controller.pathBar.isHidden, false, "选中具体文件时路径栏应正常展示")
assertEqual(controller.fdaHintButton.isHidden, true, "选中具体文件时受限提示应让位隐藏")

// 3. 取消文件选中后，应重新恢复展示受限提示
controller.updateBottomPath(with: nil)
assertEqual(controller.fdaHintButton.isHidden, false, "取消文件选中后应恢复展示受限提示按钮")
assertEqual(controller.pathBar.isHidden, true, "取消文件选中后路径栏应隐藏")

// 4. 授权成功后，受限提示应静默隐藏
controller.updateFDAStatus(hasFDA: true)
assertEqual(controller.fdaHintButton.isHidden, true, "完全磁盘访问权限授予后应静默隐藏受限提示按钮")
assertEqual(controller.pathBar.isHidden, false, "完全磁盘访问权限授予后路径栏应恢复可用")

// 5. 点击受限提示按钮时触发引导回调闭包
var fdaGuideTriggered = false
controller.onOpenFDAGuide = { fdaGuideTriggered = true }
if let action = controller.fdaHintButton.action {
    _ = controller.fdaHintButton.target?.perform(action, with: controller.fdaHintButton)
}
assertEqual(fdaGuideTriggered, true, "点击受限提示按钮应成功触发引导回调闭包")

print("[12/12] 测试表格列拖拽重排与动态末列对齐联动 (tableViewColumnDidMove)...")
let modifiedCol = controller.tableView.tableColumns.first { $0.identifier == .columnModified }!
let testSizeCol = controller.tableView.tableColumns.first { $0.identifier == .columnSize }!
let testPathCol = controller.tableView.tableColumns.first { $0.identifier == .columnPath }!
let testNameCol = controller.tableView.tableColumns.first { $0.identifier == .columnName }!

// 1. 初始状态：大小位于末列，应为右对齐；修改时间位于中间列，应为左对齐
assertEqual(controller.tableView.tableColumns.last?.identifier, .columnSize, "默认末列应为大小列")
assertEqual(testSizeCol.headerCell.alignment, .right, "默认状态下大小列表头应为右对齐")
let initialSizeCell = controller.tableView(controller.tableView, viewFor: testSizeCol, row: 0) as? NSTableCellView
assertEqual(initialSizeCell?.textField?.alignment, .right, "默认状态下大小列单元格应为右对齐")

assertEqual(modifiedCol.headerCell.alignment, .left, "默认状态下修改时间列表头应为左对齐")
let initialModifiedCell = controller.tableView(controller.tableView, viewFor: modifiedCol, row: 0) as? NSTableCellView
assertEqual(initialModifiedCell?.textField?.alignment, .left, "默认状态下修改时间列单元格应为左对齐")

// 2. 模拟将大小列从第 3 列拖动到第 1 列 (Name, Size, Path, Modified)
let oldSizeIndex = controller.tableView.column(withIdentifier: .columnSize)
controller.tableView.moveColumn(oldSizeIndex, toColumn: 1)
controller.tableViewColumnDidMove(Notification(
    name: NSTableView.columnDidMoveNotification,
    object: controller.tableView,
    userInfo: ["NSOldColumn": oldSizeIndex, "NSNewColumn": 1]
))

assertEqual(controller.tableView.tableColumns.last?.identifier, .columnModified, "拖拽后末列应变为修改时间列")
assertEqual(testSizeCol.headerCell.alignment, .left, "移至中间列后大小列表头应自适应为左对齐")
let movedSizeCell = controller.tableView(controller.tableView, viewFor: testSizeCol, row: 0) as? NSTableCellView
assertEqual(movedSizeCell?.textField?.alignment, .left, "移至中间列后大小列单元格应自适应为左对齐")

assertEqual(modifiedCol.headerCell.alignment, .right, "成为末列后修改时间列表头应自适应为右对齐")
let movedModifiedCell = controller.tableView(controller.tableView, viewFor: modifiedCol, row: 0) as? NSTableCellView
assertEqual(movedModifiedCell?.textField?.alignment, .right, "成为末列后修改时间列单元格应自适应为右对齐")

// 3. 模拟将路径列拖动到末列 (Name, Size, Modified, Path)
let oldPathIndex = controller.tableView.column(withIdentifier: .columnPath)
let targetLastIndex = controller.tableView.tableColumns.count - 1
controller.tableView.moveColumn(oldPathIndex, toColumn: targetLastIndex)
controller.tableViewColumnDidMove(Notification(
    name: NSTableView.columnDidMoveNotification,
    object: controller.tableView,
    userInfo: ["NSOldColumn": oldPathIndex, "NSNewColumn": targetLastIndex]
))

assertEqual(controller.tableView.tableColumns.last?.identifier, .columnPath, "拖拽后末列应变为路径列")
assertEqual(testPathCol.headerCell.alignment, .left, "路径列移至末列仍应保持左对齐")
let movedPathCell = controller.tableView(controller.tableView, viewFor: testPathCol, row: 0) as? NSTableCellView
assertEqual(movedPathCell?.textField?.alignment, .left, "路径列移至末列单元格仍应保持左对齐")
assertEqual(modifiedCol.headerCell.alignment, .left, "修改时间列离开末列后应恢复为左对齐")

print("[13/14] 测试离线搜索结果渲染样式、零磁盘 I/O 图标与操作拦截...")
var workspaceIconCalls: [String] = []
controller.workspaceIconProvider = { path in
    workspaceIconCalls.append(path)
    return NSImage()
}
let offlineDelegate = MockDelegate()
controller.delegate = offlineDelegate
var offlineRevealCalls = 0
var offlineTerminalCalls = 0
var offlineTrashCalls = 0
controller.revealFilesHandler = { _ in offlineRevealCalls += 1 }
controller.openInTerminalHandler = { _ in offlineTerminalCalls += 1 }
controller.trashItemHandler = { url in
    offlineTrashCalls += 1
    return url
}

let offlineItem = FfiSearchResult(
    name: "budget.xlsx",
    path: "/Volumes/OfflineNAS/finance/budget.xlsx",
    extension: "xlsx",
    isDir: false,
    size: 4096,
    created: 1000,
    modified: 2000,
    isOffline: true,
    volumeName: "OfflineNAS"
)
let onlineItem = FfiSearchResult(
    name: "notes.txt",
    path: "/tmp/local_notes.txt",
    extension: "txt",
    isDir: false,
    size: 128,
    created: 1000,
    modified: 2000,
    isOffline: false,
    volumeName: nil
)
controller.displayResults([offlineItem, onlineItem], preserveSelection: false)
controller.updateStatusText("2 items")

let offlineNameCell = controller.tableView(controller.tableView, viewFor: testNameCol, row: 0) as? SearchNameTableCellView
let offlinePathCell = controller.tableView(controller.tableView, viewFor: testPathCol, row: 0) as? SearchPathTableCellView
assertTrue(!workspaceIconCalls.contains(offlineItem.path), "离线项绝不可触发 NSWorkspace.shared.icon(forFile:)")
assertTrue((offlineNameCell?.textField?.alphaValue ?? 1.0) < 0.9, "离线名称单元格应呈淡化透明度")
assertTrue(offlinePathCell?.textField?.stringValue.contains(L("search.result.offline")) == true, "离线路径单元格应附带离线状态标记")

let onlineNameCell = controller.tableView(controller.tableView, viewFor: testNameCol, row: 1) as? SearchNameTableCellView
let onlinePathCell = controller.tableView(controller.tableView, viewFor: testPathCol, row: 1) as? SearchPathTableCellView
assertEqual(onlineNameCell?.textField?.alphaValue, 1.0, "在线名称单元格应维持正常不透明状态")
assertTrue(onlinePathCell?.textField?.stringValue.contains(L("search.result.offline")) == false, "在线路径单元格不应显示离线标记")

// 校验拖拽与右键/快捷操作对离线项的完整拦截
assertTrue(controller.tableView(controller.tableView, pasteboardWriterForRow: 0) == nil, "离线行不可提供 fileURL 拖拽写入器")
assertTrue(controller.tableView(controller.tableView, pasteboardWriterForRow: 1) != nil, "在线行应正常提供拖拽写入器")

controller.tableView.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
assertEqual(controller.acceptsPreviewPanelControl(QLPreviewPanel.shared()), false, "仅选中离线项时应拒绝 QuickLook 接管")
assertEqual(controller.numberOfPreviewItems(in: QLPreviewPanel.shared()), 0, "离线选中项 QuickLook 条目数应为 0")

controller.openSelected()
controller.revealSelected()
controller.openInTerminalSelected()
controller.quickLookSelected()
controller.showInfoSelected()
controller.copyFile()
controller.moveToTrashSelected()

assertEqual(offlineDelegate.doubleClickCallCount, 0, "离线项 openSelected 不得触发打开回调")
assertEqual(offlineRevealCalls, 0, "离线项 revealSelected 不得触发访达回调")
assertEqual(offlineTerminalCalls, 0, "离线项 openInTerminalSelected 不得触发终端回调")
assertEqual(offlineTrashCalls, 0, "离线项 moveToTrashSelected 不得触发废纸篓 I/O")
assertEqual(controller.statusLabel.stringValue, L("search.offline.actionBlocked"), "拦截离线操作后应更新底栏提示")

// 校验菜单禁用与复制路径/文件名放行
assertEqual(controller.validateMenuItem(NSMenuItem(title: "Open", action: #selector(SearchResultsViewController.openSelected(_:)), keyEquivalent: "")), false, "离线项应禁用打开菜单项")
assertEqual(controller.validateMenuItem(NSMenuItem(title: "Reveal", action: #selector(SearchResultsViewController.revealSelected(_:)), keyEquivalent: "")), false, "离线项应禁用访达定位菜单项")
assertEqual(controller.validateMenuItem(NSMenuItem(title: "Copy File", action: #selector(SearchResultsViewController.copyFile(_:)), keyEquivalent: "")), false, "离线项应禁用拷贝文件菜单项")
assertEqual(controller.validateMenuItem(NSMenuItem(title: "Trash", action: #selector(SearchResultsViewController.moveToTrashSelected(_:)), keyEquivalent: "")), false, "离线项应禁用移到废纸篓菜单项")
assertEqual(controller.validateMenuItem(NSMenuItem(title: "Copy Path", action: #selector(SearchResultsViewController.copyPath(_:)), keyEquivalent: "")), true, "离线项仍允许拷贝路径")
assertEqual(controller.validateMenuItem(NSMenuItem(title: "Copy Name", action: #selector(SearchResultsViewController.copyName(_:)), keyEquivalent: "")), true, "离线项仍允许拷贝名称")

let offlineBreadcrumbReveal = NSMenuItem(title: "Reveal", action: NSSelectorFromString("breadcrumbRevealInFinder:"), keyEquivalent: "")
offlineBreadcrumbReveal.representedObject = URL(fileURLWithPath: "/Volumes/OfflineNAS/finance")
assertEqual(controller.validateMenuItem(offlineBreadcrumbReveal), false, "离线父级面包屑应直接禁用并跳过磁盘探测")

// 校验 NSFilenamesPboardType 在离线选区下同样被 validRequestor 阻断
let filenamesPboardType = NSPasteboard.PasteboardType("NSFilenamesPboardType")
assertTrue(controller.validRequestor(forSendType: filenamesPboardType, returnType: nil) == nil, "离线选区下 validRequestor 应拦截 NSFilenamesPboardType")

controller.copyPath()
assertEqual(NSPasteboard.general.string(forType: .string), offlineItem.path, "离线项拷贝路径应写入完整路径字符串")
controller.copyName()
assertEqual(NSPasteboard.general.string(forType: .string), offlineItem.name, "离线项拷贝名称应写入文件名字符串")
controller.cancelTemporaryStatus()
assertEqual(controller.statusLabel.stringValue, "2 items", "取消临时提示后应恢复持久状态栏文本")

// 验证离线面包屑控件不写入 pathBar.url 且不挂接磁盘 URL 探测
controller.updateBottomPath(with: offlineItem.path)
assertEqual(controller.pathBar.isOfflinePathMode, true, "选中离线项时底栏面包屑应进入纯内存离线模式")
assertTrue(controller.pathBar.url == nil, "离线面包屑模式绝不可向 pathBar.url 写入路径")

// 校验离线选中状态下根路径 "/" 面包屑同样禁用访达操作，而切换至在线项后 "/" 与 "/Volumes" 正常可用
let rootBreadcrumbReveal = NSMenuItem(title: "Reveal Root", action: NSSelectorFromString("breadcrumbRevealInFinder:"), keyEquivalent: "")
rootBreadcrumbReveal.representedObject = URL(fileURLWithPath: "/")
assertEqual(controller.validateMenuItem(rootBreadcrumbReveal), false, "选中离线行 (isOfflinePathMode=true) 时根路径面包屑应禁用访达定位")

assertEqual(controller.isPathOffline("/"), false, "存在离线结果时根路径 / 绝不可被误判为离线")
assertEqual(controller.isPathOffline("/Volumes"), false, "存在离线结果时 /Volumes 容器目录绝不可被误判为离线")

controller.tableView.selectRowIndexes(IndexSet(integer: 1), byExtendingSelection: false)
controller.updateBottomPath(with: onlineItem.path)
assertEqual(controller.pathBar.isOfflinePathMode, false, "选中在线行时底栏应退出离线面包屑模式")
assertEqual(controller.validateMenuItem(rootBreadcrumbReveal), true, "选中在线行时根路径 / 面包屑访达定位应保持启用")

// 校验 excludeDirectorySelected 对离线目录与离线文件的纯内存路径推导（不将离线目录降级为父目录）
var excludedPaths: [String] = []
controller.onExcludeDirectory = { path in
    excludedPaths.append(path)
}
let offlineDirItem = FfiSearchResult(
    name: "docs",
    path: "/Volumes/OfflineNAS/docs",
    extension: "",
    isDir: true,
    size: 0,
    created: 1000,
    modified: 2000,
    isOffline: true,
    volumeName: "OfflineNAS"
)
let offlineFileInRoot = FfiSearchResult(
    name: "readme.md",
    path: "/Volumes/OfflineNAS/readme.md",
    extension: "md",
    isDir: false,
    size: 512,
    created: 1000,
    modified: 2000,
    isOffline: true,
    volumeName: "OfflineNAS"
)
controller.displayResults([offlineDirItem, offlineFileInRoot, onlineItem], preserveSelection: false)
controller.tableView.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
controller.excludeDirectorySelected()
assertEqual(excludedPaths.last, "/Volumes/OfflineNAS/docs", "对离线目录执行排除目录必须直接传入该离线目录本身而非父目录")

controller.tableView.selectRowIndexes(IndexSet(integer: 1), byExtendingSelection: false)
controller.excludeDirectorySelected()
assertEqual(excludedPaths.last, "/Volumes/OfflineNAS", "对离线普通文件执行排除目录必须传入其所在父目录")

controller.displayResults([offlineItem, onlineItem], preserveSelection: false)

print("[14/15] 测试混合选区（在线 + 离线）全链路统一拦截...")
controller.tableView.selectRowIndexes(IndexSet([0, 1]), byExtendingSelection: false)
controller.openSelected()
controller.revealSelected()
controller.openInTerminalSelected()
controller.quickLookSelected()
controller.showInfoSelected()
controller.copyFile()
controller.moveToTrashSelected()
let fakeOpenWithItem = NSMenuItem(title: "TextEdit", action: #selector(SearchResultsViewController.openWithApp(_:)), keyEquivalent: "")
fakeOpenWithItem.representedObject = URL(fileURLWithPath: "/System/Applications/TextEdit.app")
controller.openWithApp(fakeOpenWithItem)
controller.chooseOtherAppToOpen()

assertEqual(offlineDelegate.doubleClickCallCount, 0, "混合选区中包含离线项时 openSelected 必须拦截")
assertEqual(offlineRevealCalls, 0, "混合选区中包含离线项时 revealSelected 必须拦截")
assertEqual(offlineTerminalCalls, 0, "混合选区中包含离线项时 openInTerminalSelected 必须拦截")
assertEqual(offlineTrashCalls, 0, "混合选区中包含离线项时 moveToTrashSelected 必须拦截")
assertEqual(controller.statusLabel.stringValue, L("search.offline.actionBlocked"), "混合选区被拦截时应展示离线阻断提示")
assertEqual(controller.acceptsPreviewPanelControl(QLPreviewPanel.shared()), false, "混合选区中包含离线项时 QuickLook 必须拒绝接管")
assertEqual(controller.numberOfPreviewItems(in: QLPreviewPanel.shared()), 0, "混合选区 QuickLook 预览数应为 0")
assertEqual(controller.validateMenuItem(NSMenuItem(title: "Open", action: #selector(SearchResultsViewController.openSelected(_:)), keyEquivalent: "")), false, "混合选区应禁用打开菜单")
assertEqual(controller.validateMenuItem(NSMenuItem(title: "Terminal", action: #selector(SearchResultsViewController.openInTerminalSelected(_:)), keyEquivalent: "")), false, "混合选区应禁用终端打开菜单")
assertEqual(controller.validateMenuItem(NSMenuItem(title: "Open With", action: #selector(SearchResultsViewController.openWithApp(_:)), keyEquivalent: "")), false, "混合选区应禁用打开方式菜单")
controller.cancelTemporaryStatus()

print("[15/15] 测试引擎离线路径回调动态降级 (isPathOfflineHandler) 与即时重绘 (refreshOfflineStates)...")
workspaceIconCalls.removeAll()
var dynamicOfflineEnabled = false
controller.isPathOfflineHandler = { path in
    dynamicOfflineEnabled && path.hasPrefix("/Volumes/DisconnectedDisk")
}
let dynamicallyOffline = FfiSearchResult(
    name: "archive.tar",
    path: "/Volumes/DisconnectedDisk/archive.tar",
    extension: "tar",
    isDir: false,
    size: 512,
    created: 1000,
    modified: 2000,
    isOffline: false,
    volumeName: nil
)
controller.displayResults([dynamicallyOffline], preserveSelection: false)
controller.tableView.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
assertTrue(controller.tableView(controller.tableView, pasteboardWriterForRow: 0) != nil, "卷仍在线时应允许文件拖拽")

// 模拟卷在未重新查询的情况下突然断开并刷新视觉与拦截状态
dynamicOfflineEnabled = true
controller.refreshOfflineStates()
let refreshedPathCell = controller.tableView(controller.tableView, viewFor: testPathCol, row: 0) as? SearchPathTableCellView
assertTrue(refreshedPathCell?.textField?.stringValue.contains(L("search.result.offline")) == true, "refreshOfflineStates 后应立即显现离线标记")
assertTrue(!workspaceIconCalls.contains(dynamicallyOffline.path), "动态降级离线后 iconForFile 绝不可调用 NSWorkspace")
assertTrue(controller.tableView(controller.tableView, pasteboardWriterForRow: 0) == nil, "动态命中离线路径的行不应允许文件拖拽")
assertEqual(controller.pathBar.isOfflinePathMode, true, "refreshOfflineStates 应同步将底栏切换至离线模式")

print("[16/16] 测试自动预选首行、文本编辑态 ⌘⌫ 保护、普通文件零主线程磁盘图标与 O(1) 底栏更新...")
workspaceIconCalls.removeAll()
controller.isPathOfflineHandler = nil
let autoSelectDelegate = MockDelegate()
controller.delegate = autoSelectDelegate
let localCodeItem = FfiSearchResult(
    name: "engine.swift",
    path: "/tmp/project/engine.swift",
    extension: "swift",
    isDir: false,
    size: 2048,
    created: 1000,
    modified: 2000
)
let localDocItem = FfiSearchResult(
    name: "guide.md",
    path: "/tmp/project/guide.md",
    extension: "md",
    isDir: false,
    size: 512,
    created: 1000,
    modified: 1900
)
controller.displayResults([localCodeItem, localDocItem], preserveSelection: false, autoSelectFirstRow: true)
assertEqual(controller.tableView.selectedRow, 0, "启用 autoSelectFirstRow 时非空结果集应自动预选第 0 行")
assertEqual(autoSelectDelegate.selectedResult?.path, localCodeItem.path, "自动预选首行后应立即通知代理更新底栏路径")

// 普通文件渲染绝不在主线程同步调用 NSWorkspace.icon(forFile:)
_ = controller.tableView(controller.tableView, viewFor: testNameCol, row: 0)
assertTrue(!workspaceIconCalls.contains(localCodeItem.path), "普通代码文件应走纯内存 UTType 类型图标，严禁主线程同步查盘")

// O(1) updateBottomPath(for:) 接口验证
controller.updateBottomPath(for: localCodeItem)
assertEqual(controller.pathBar.url?.path, localCodeItem.path, "updateBottomPath(for:) 应正确更新在线文件路径")
controller.updateBottomPath(for: offlineItem)
assertEqual(controller.pathBar.isOfflinePathMode, true, "updateBottomPath(for:) 应直接识别离线条目进入纯内存模式")
assertTrue(controller.pathBar.url == nil, "updateBottomPath(for:) 离线模式绝不可写入 pathBar.url")

// 验证打开方式子菜单挂载独立按需代理，避免主右键菜单同步查询 LaunchServices
assertTrue(menu.items[1].submenu?.delegate != nil, "打开方式子菜单必须挂载独立 NSMenuDelegate 实现展开时按需构建")

// 验证焦点在文本编辑器（搜索框）时，⌘⌫ 不得被 SearchTableView 劫持触发废纸篓删除
let hostWin = NSWindow(
    contentRect: NSRect(x: 0, y: 0, width: 400, height: 300),
    styleMask: [.titled],
    backing: .buffered,
    defer: false
)
let textEditor = NSTextView(frame: NSRect(x: 0, y: 0, width: 200, height: 30))
hostWin.contentView?.addSubview(controller.view)
hostWin.contentView?.addSubview(textEditor)
hostWin.makeFirstResponder(textEditor)
assertEqual(controller.tableView.performKeyEquivalent(with: deleteEvent), false, "焦点位于文本编辑器时 ⌘⌫ 必须交还给文本编辑器，不得误删预选首行")

print("测试全部通过：16/16 项断言成功。")
"""

let tempDir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
try! FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
defer { try? FileManager.default.removeItem(at: tempDir) }

let harnessFile = tempDir.appendingPathComponent("main.swift")
try! runnerSource.write(to: harnessFile, atomically: true, encoding: .utf8)

let process = Process()
process.executableURL = URL(fileURLWithPath: "/usr/bin/swift")
process.arguments = [harnessFile.path]
process.standardOutput = FileHandle.standardOutput
process.standardError = FileHandle.standardError

try! process.run()
process.waitUntilExit()

if process.terminationStatus != 0 {
    exit(process.terminationStatus)
}
