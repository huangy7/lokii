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

    public init(
        name: String,
        path: String,
        `extension`: String,
        isDir: Bool,
        size: UInt64,
        created: Int64,
        modified: Int64,
        matchPositions: [FfiMatchRange] = [],
        parentMatchPositions: [FfiMatchRange] = []
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

print("[1/8] 测试控制器初始化与内聚底栏视图层次构建...")
let controller = SearchResultsViewController()
let _ = controller.view // 触发 loadView 与 viewDidLoad

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

print("[2/8] 测试展示检索结果与单元格渲染 (displayResults)...")
let sampleItems = [
    FfiSearchResult(name: "alpha.rs", path: "/tmp/src/alpha.rs", extension: "rs", isDir: false, size: 2048, created: 100, modified: 300, matchPositions: [FfiMatchRange(start: 0, end: 5)]),
    FfiSearchResult(name: "beta.md", path: "/tmp/docs/beta.md", extension: "md", isDir: false, size: 1024, created: 100, modified: 200),
    FfiSearchResult(name: "gamma_dir", path: "/tmp/gamma_dir", extension: "", isDir: true, size: 0, created: 100, modified: 100),
]

controller.displayResults(sampleItems, preserveSelection: false)
assertEqual(controller.results.count, 3, "控制器结果集合应包含 3 个条目")
assertEqual(controller.numberOfRows(in: controller.tableView), 3, "表格数据源行数应为 3")

let nameCol = controller.tableView.tableColumns.first { $0.identifier == .columnName }
let pathCol = controller.tableView.tableColumns.first { $0.identifier == .columnPath }
let sizeCol = controller.tableView.tableColumns.first { $0.identifier == .columnSize }

let nameCellView = controller.tableView(controller.tableView, viewFor: nameCol, row: 0)
assertTrue(nameCellView is SearchNameTableCellView, "名称列应返回定制高亮单元格 SearchNameTableCellView")

let nameCellViewNoMatch = controller.tableView(controller.tableView, viewFor: nameCol, row: 1) as? SearchNameTableCellView
assertTrue(nameCellViewNoMatch != nil, "名称列无匹配范围时应能正常渲染")
assertEqual(nameCellViewNoMatch?.textField?.attributedStringValue.string, "beta.md", "未高亮单元格文本内容应与文件名一致")

let pathCellView = controller.tableView(controller.tableView, viewFor: pathCol, row: 0) as? NSTableCellView
assertEqual(pathCellView?.textField?.stringValue, "/tmp/src", "路径列应显示父目录路径")

let sizeCellView = controller.tableView(controller.tableView, viewFor: sizeCol, row: 2) as? NSTableCellView
assertEqual(sizeCellView?.textField?.stringValue, "--", "目录行大小应显示为 --")

print("[3/8] 测试表头多字段排序联动 (sortDescriptorsDidChange)...")
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

print("[4/8] 测试保留选区智能恢复 (preserveSelection)...")
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

print("[5/8] 测试 selectFirstRow 与代理回调机制...")
let mockDelegate = MockDelegate()
controller.delegate = mockDelegate

controller.displayResults(sampleItems, preserveSelection: false)
controller.selectFirstRow()
assertEqual(controller.selectedResult?.name, "alpha.rs", "selectFirstRow 应选中首行")
assertEqual(mockDelegate.selectedResult?.name, "alpha.rs", "代理应接收到首行选区变更回调")

controller.openSelected()
assertEqual(mockDelegate.doubleClickedResult?.name, "alpha.rs", "openSelected 应触发首选条目确认回调")

print("[6/8] 测试剪贴板拷贝行为与右键菜单结构 (copyFile, copyPath, copyName & contextMenu)...")
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
assertEqual(menu.items.count, 10, "右键上下文菜单应包含 8 个功能项加 2 个分隔线")
assertEqual(menu.items[0].action, #selector(controller.openSelected), "第 0 项应绑定 openSelected")
assertEqual(menu.items[1].action, #selector(controller.quickLookSelected), "第 1 项应绑定 quickLookSelected")
assertEqual(menu.items[1].keyEquivalent, " ", "quickLookSelected 快捷键应为 Space")
assertTrue(menu.items[1].keyEquivalentModifierMask.isEmpty, "quickLookSelected 修饰键掩码应为空")
assertEqual(menu.items[2].action, #selector(controller.revealSelected), "第 2 项应绑定 revealSelected")
assertEqual(menu.items[3].action, #selector(controller.showInfoSelected), "第 3 项应绑定 showInfoSelected")
assertEqual(menu.items[3].keyEquivalent, "i", "showInfoSelected 快捷键应为 i")
assertTrue(menu.items[4].isSeparatorItem, "第 4 项应为分隔线")
assertEqual(menu.items[5].action, #selector(controller.copyFile), "第 5 项应绑定 copyFile")
assertEqual(menu.items[5].keyEquivalent, "c", "copyFile 快捷键应为 c")
assertEqual(menu.items[6].action, #selector(controller.copyPath), "第 6 项应绑定 copyPath")
assertEqual(menu.items[7].action, #selector(controller.copyName), "第 7 项应绑定 copyName")
assertTrue(menu.items[8].isSeparatorItem, "第 8 项应为分隔线")
assertEqual(menu.items[9].action, #selector(controller.moveToTrashSelected), "第 9 项应绑定 moveToTrashSelected")
assertEqual(menu.items[9].keyEquivalent, String(format: "%c", NSDeleteCharacter), "moveToTrashSelected 快捷键应为 Delete")
assertEqual(menu.items[9].keyEquivalentModifierMask, [.command], "moveToTrashSelected 修饰键应为 Command")
assertTrue(menu.allowsContextMenuPlugIns, "tableView.menu 必须开启 allowsContextMenuPlugIns 以由 AppKit 原生托管系统服务")

// 校验菜单项可用性验证逻辑 (validateMenuItem)
for item in menu.items where !item.isSeparatorItem {
    assertTrue(controller.validateMenuItem(item), "有选中项时上下文菜单项 \\(item.title) 应通过校验")
}

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

print("[7/8] 测试 Quick Look 快速预览响应者协议与数据源契约...")
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

print("[8/8] 测试底栏状态更新、进度指示与重建动作代理派发...")
// 路径更新
controller.updateBottomPath(with: "/tmp/src/alpha.rs")
assertEqual(controller.pathBar.url?.path, "/tmp/src/alpha.rs", "updateBottomPath 应正确设置路径")
controller.updateBottomPath(with: nil)
assertEqual(controller.pathBar.url, nil, "updateBottomPath(with: nil) 应清空路径")

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

print("[9/10] 测试键盘导航焦点接力与首行上移回调 (focusResults & SearchTableView.moveUp)...")
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

print("[10/10] 测试排版密度模型与动态字号/行高/图标/列宽联动 (applyFontSize)...")
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

print("[11/11] 测试完全磁盘访问权限受限提示与底栏路径让位互斥逻辑 (updateFDAStatus & updateBottomPath)...")
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

print("测试全部通过：11/11 项断言成功。")
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
