import Cocoa
import UniformTypeIdentifiers

// MARK: - Standalone Test Harness for SearchMainSplitViewController

let sourceFiles = [
    "Lokii/Lokii/L10n.swift",
    "Lokii/Lokii/LocalizedMenu.swift",
    "Lokii/Lokii/ResultDirectoryTree.swift",
    "Lokii/Lokii/SidebarTreeViewController.swift",
    "Lokii/Lokii/SearchResultsViewController.swift",
    "Lokii/Lokii/SearchMainSplitViewController.swift",
]

for path in sourceFiles {
    guard FileManager.default.fileExists(atPath: path) else {
        fputs("tests/split_view_controller_test.swift: error: 找不到 \(path)\n", stderr)
        exit(1)
    }
}

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

var loadedSources: [String] = []
for path in sourceFiles {
    guard let content = try? String(contentsOfFile: path, encoding: .utf8) else {
        fputs("tests/split_view_controller_test.swift: error: 无法读取 \(path)\n", stderr)
        exit(1)
    }
    loadedSources.append(content)
}

let testCasesRunner = """
final class MockSplitDelegate: SearchMainSplitViewControllerDelegate {
    var lastSelectedResult: FfiSearchResult?
    var selectCallCount = 0
    var lastDoubleClickedResult: FfiSearchResult?
    var doubleClickCallCount = 0
    var rebuildCallCount = 0

    func searchMainSplitViewDidSelect(result: FfiSearchResult?) {
        lastSelectedResult = result
        selectCallCount += 1
    }

    func searchMainSplitViewDidDoubleClick(result: FfiSearchResult) {
        lastDoubleClickedResult = result
        doubleClickCallCount += 1
    }

    func searchMainSplitViewDidRequestRebuild() {
        rebuildCallCount += 1
    }

    var focusSearchFieldCallCount = 0

    func searchMainSplitViewDidRequestFocusSearchField() {
        focusSearchFieldCallCount += 1
    }

    var trashedPaths: Set<String> = []
    var trashedRemainingCount = 0
    var trashCallCount = 0

    func searchMainSplitViewDidTrashItems(paths: Set<String>, remainingCount: Int) {
        trashedPaths = paths
        trashedRemainingCount = remainingCount
        trashCallCount += 1
    }
}

print("[1/5] 测试双栏容器分栏条目（NSSplitViewItem）配置参数...")
let splitVC = SearchMainSplitViewController()
_ = splitVC.view

assertEqual(splitVC.splitViewItems.count, 2, "主分栏容器必须严格包含 2 个分栏条目（左目录树 + 右主列表）")

let sidebarItem = splitVC.sidebarSplitViewItem
assertEqual(sidebarItem.behavior, .sidebar, "左栏必须采用 .sidebar 规范")
assertEqual(sidebarItem.minimumThickness, 180, "左栏最小宽度必须为 180 pt")
assertEqual(sidebarItem.maximumThickness, 320, "左栏最大宽度必须为 320 pt")
assertEqual(sidebarItem.holdingPriority.rawValue, 260, "左栏 HoldingPriority 必须为 260")
assertEqual(sidebarItem.canCollapse, true, "左栏必须支持折叠")
assertEqual(sidebarItem.isCollapsed, true, "左栏默认必须为折叠收起状态")

let contentItem = splitVC.contentSplitViewItem
assertEqual(contentItem.behavior, .contentList, "主列表栏必须采用 .contentList 规范")
assertEqual(contentItem.minimumThickness, 460, "主列表栏最小宽度必须硬性守卫为 460 pt 以防止塌陷")
assertEqual(contentItem.holdingPriority.rawValue, 200, "主列表栏 HoldingPriority 必须为 200 以吸收拉伸")
assertEqual(contentItem.canCollapse, false, "主列表栏必须绝对禁止折叠")

print("[2/5] 测试侧边栏折叠显隐控制（toggleSidebar 及 Cocoa 动作重写）...")
assertEqual(splitVC.isSidebarCollapsed, true, "初始左栏处于折叠状态")

// 兼容已有无参调用
splitVC.toggleSidebar()
assertEqual(splitVC.isSidebarCollapsed, false, "执行 toggleSidebar() 后左栏应展开")
assertEqual(sidebarItem.isCollapsed, false, "左栏 splitViewItem.isCollapsed 应为 false")

// 带 sender: Any? 的重写动作调用（nil 传参）
splitVC.toggleSidebar(nil)
assertEqual(splitVC.isSidebarCollapsed, true, "执行 toggleSidebar(nil) 后左栏应折叠")
assertEqual(sidebarItem.isCollapsed, true, "左栏 splitViewItem.isCollapsed 应为 true")

// 带 sender: Any? 的重写动作调用（任意对象传参，模拟 Cocoa 响应者链事件源）
splitVC.toggleSidebar(NSObject())
assertEqual(splitVC.isSidebarCollapsed, false, "执行 toggleSidebar(sender) 后左栏应展开")
assertEqual(sidebarItem.isCollapsed, false, "左栏 splitViewItem.isCollapsed 应为 false")

// 恢复折叠
splitVC.toggleSidebar(nil)
assertEqual(splitVC.isSidebarCollapsed, true, "再次执行 toggleSidebar(nil) 后左栏应折叠")
assertEqual(sidebarItem.isCollapsed, true, "左栏 splitViewItem.isCollapsed 应为 true")

print("[3/5] 测试中介协调器检索结果集派发与初始全量展现（updateSearchResults）...")
let itemA = FfiSearchResult(name: "main.rs", path: "/workspace/src/main.rs", extension: "rs", isDir: false, size: 1024, created: 1000, modified: 2000)
let itemB = FfiSearchResult(name: "lib.rs", path: "/workspace/src/lib.rs", extension: "rs", isDir: false, size: 2048, created: 1000, modified: 2100)
let itemC = FfiSearchResult(name: "readme.md", path: "/workspace/docs/readme.md", extension: "md", isDir: false, size: 512, created: 1000, modified: 2200)
let testRawResults = [itemA, itemB, itemC]
let testTree = ResultDirectoryTree.build(from: testRawResults)

splitVC.updateSearchResults(rawResults: testRawResults, tree: testTree)

assertEqual(splitVC.rawResults.count, 3, "中介协调器应缓存全量结果 3 项")
assertEqual(splitVC.sidebarTreeViewController.currentTree?.totalMatchCount, 3, "左侧目录树总命中数应为 3")
assertEqual(splitVC.searchResultsViewController.results.count, 3, "中栏列表初始默认展示全部 3 项结果")

print("[4/5] 测试左栏单选作用域过滤联动（单向推入中栏列表）...")
splitVC.sidebarDidSelectDirectory(path: "/workspace/src")
assertEqual(splitVC.searchResultsViewController.results.count, 2, "过滤后中栏列表应仅包含 /workspace/src 下的 2 项条目")
for res in splitVC.searchResultsViewController.results {
    assertTrue(res.path.hasPrefix("/workspace/src"), "过滤结果条目路径必须位于 /workspace/src 下")
}

splitVC.sidebarDidSelectDirectory(path: nil)
assertEqual(splitVC.searchResultsViewController.results.count, 3, "选择全部结果后中栏列表应恢复展示全量 3 项")

print("[5/5] 测试中栏列表单选驱动外部窗口与重置状态清空（无反向环路污染）...")
let mockDelegate = MockSplitDelegate()
splitVC.delegate = mockDelegate

// 模拟中栏列表单选 itemA
splitVC.searchResultsDidSelect(result: itemA)
assertEqual(mockDelegate.lastSelectedResult, itemA, "外部窗口代理应接收到选中的 itemA")
assertEqual(mockDelegate.selectCallCount, 1, "选中代理方法应被调用 1 次")

// 严格验证单向性：中间列表的选择绝不反向污染或修改左侧目录树的选区与过滤状态
assertEqual(splitVC.sidebarTreeViewController.selectedPath, nil, "中间列表选中变更绝不反向改动左栏选中的目录路径")

// 测试通过虚拟列表原生行选择变更驱动全链路联动
splitVC.searchResultsViewController.selectFirstRow()
assertEqual(splitVC.searchResultsViewController.selectedResult, itemA, "selectFirstRow 应选中 itemA")

// 模拟双击中栏列表项
splitVC.searchResultsDidDoubleClick(result: itemA)
assertEqual(mockDelegate.lastDoubleClickedResult, itemA, "外部窗口代理应接收到双击的 itemA")
assertEqual(mockDelegate.doubleClickCallCount, 1, "双击代理方法应被调用 1 次")

// 模拟中栏列表触发索引重建请求并验证中介转发
splitVC.searchResultsDidRequestRebuild()
assertEqual(mockDelegate.rebuildCallCount, 1, "中介协调器应将索引重建请求转发至外部代理")

// 模拟中栏列表首行上移回焦请求并验证中介转发
splitVC.searchResultsDidRequestFocusSearchField()
assertEqual(mockDelegate.focusSearchFieldCallCount, 1, "中介协调器应将回焦搜索框请求转发至外部代理")

// 模拟中栏列表完成移入废纸篓，验证中介协调器同步移除 rawResults、重建目录树并向外部代理通知
splitVC.searchResultsDidTrashItems(paths: [itemA.path])
assertEqual(splitVC.rawResults.count, 2, "中介协调器 rawResults 应已同步移除被删除项，从 3 项变为 2 项")
assertTrue(!splitVC.rawResults.contains(itemA), "剩余项中不应再包含 itemA")
assertTrue(splitVC.rawResults.contains(itemB), "剩余项中应包含 itemB")
assertTrue(splitVC.rawResults.contains(itemC), "剩余项中应包含 itemC")
assertEqual(mockDelegate.trashCallCount, 1, "外部代理应收到废纸篓完成通知")
assertEqual(mockDelegate.trashedRemainingCount, 2, "外部代理收到的剩余计数应为 2")
assertTrue(mockDelegate.trashedPaths.contains(itemA.path), "外部代理收到的路径集合应包含 itemA")

// 测试空结果集与重置状态清空（Empty State & Clear）
splitVC.updateSearchResults(rawResults: [], tree: nil)
assertEqual(splitVC.rawResults.isEmpty, true, "清空后 rawResults 应为空")
assertEqual(splitVC.sidebarTreeViewController.currentTree == nil, true, "清空后左侧目录树应恢复为 nil")
assertEqual(splitVC.searchResultsViewController.results.isEmpty, true, "清空后中栏列表应为空")

splitVC.onLanguageChanged()

print("测试全部通过：5/5 项断言成功。")
"""

let combinedSource = """
import Cocoa
import UniformTypeIdentifiers

\(mockFfiSource)

\(loadedSources.joined(separator: "\n\n"))

func assertEqual<T: Equatable>(_ actual: T, _ expected: T, _ message: String = "", file: StaticString = #file, line: UInt = #line) {
    if actual != expected {
        fputs("断言失败: [\\(actual)] != [\\(expected)] - \\(message) (\\(file):\\(line))\\n", stderr)
        exit(1)
    }
}

func assertTrue(_ condition: Bool, _ message: String = "", file: StaticString = #file, line: UInt = #line) {
    if !condition {
        fputs("断言失败: 条件不满足 - \\(message) (\\(file):\\(line))\\n", stderr)
        exit(1)
    }
}

\(testCasesRunner)
"""

let tempDir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
try! FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
defer { try? FileManager.default.removeItem(at: tempDir) }

let harnessFile = tempDir.appendingPathComponent("main.swift")
try! combinedSource.write(to: harnessFile, atomically: true, encoding: .utf8)

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
