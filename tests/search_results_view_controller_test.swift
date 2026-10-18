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
}

print("[1/8] 测试控制器初始化与内聚底栏视图层次构建...")
let controller = SearchResultsViewController()
let _ = controller.view // 触发 loadView 与 viewDidLoad

assertTrue(controller.view.subviews.contains(controller.tableScrollView), "根视图应包含 tableScrollView")
assertTrue(controller.view.subviews.contains(controller.separator), "根视图应包含分割线")
assertTrue(controller.view.subviews.contains(controller.bottomBar), "根视图应包含 bottomBar")
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

print("[6/8] 测试剪贴板拷贝行为 (copyPath & copyName)...")
controller.selectFirstRow()
controller.copyPath()
let pasteboardPath = NSPasteboard.general.string(forType: .string)
assertEqual(pasteboardPath, "/tmp/src/alpha.rs", "copyPath 应将所选条目路径写入系统剪贴板")

controller.copyName()
let pasteboardName = NSPasteboard.general.string(forType: .string)
assertEqual(pasteboardName, "alpha.rs", "copyName 应将所选条目名称写入系统剪贴板")

print("[7/8] 测试 Quick Look 快速预览响应者协议与数据源契约...")
// 无选中条目时
controller.displayResults([], preserveSelection: false)
assertEqual(controller.acceptsPreviewPanelControl(nil), false, "列表为空或无选区时 acceptsPreviewPanelControl 应返回 false")
assertEqual(controller.numberOfPreviewItems(in: nil), 0, "无选中时预览项数量应为 0")

// 有选中条目时
controller.displayResults(sampleItems, preserveSelection: false)
controller.selectFirstRow()
assertEqual(controller.acceptsPreviewPanelControl(nil), true, "存在选中条目时 acceptsPreviewPanelControl 应返回 true")
assertEqual(controller.numberOfPreviewItems(in: nil), 1, "存在选中条目时预览项数量应为 1")
let previewItem = controller.previewPanel(nil, previewItemAt: 0)
assertEqual((previewItem as? NSURL)?.path, "/tmp/src/alpha.rs", "预览项路径应与当前选中条目一致")

print("[8/8] 测试底栏状态更新、进度指示与重建动作代理派发...")
// 路径更新
controller.updateBottomPath(with: "/tmp/src/alpha.rs")
assertEqual(controller.pathBar.url?.path, "/tmp/src/alpha.rs", "updateBottomPath 应正确设置路径")
controller.updateBottomPath(with: nil)
assertEqual(controller.pathBar.url, nil, "updateBottomPath(with: nil) 应清空路径")

// 状态文本更新
controller.updateStatusText("已匹配 10 项")
assertEqual(controller.statusLabel.stringValue, "已匹配 10 项", "updateStatusText 应正确刷新状态文本")

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

print("[9/9] 测试键盘导航焦点接力与首行上移回调 (focusResults & SearchTableView.moveUp)...")
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

print("测试全部通过：9/9 项断言成功。")
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
