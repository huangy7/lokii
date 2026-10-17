import Cocoa

// MARK: - Standalone Test Harness for SidebarTreeViewController

let sidebarSourcePath = "Lokii/Lokii/SidebarTreeViewController.swift"
guard FileManager.default.fileExists(atPath: sidebarSourcePath) else {
    fputs("tests/sidebar_tree_view_controller_test.swift: error: 找不到 \(sidebarSourcePath)\n", stderr)
    exit(1)
}

guard let sidebarSource = try? String(contentsOfFile: sidebarSourcePath, encoding: .utf8) else {
    fputs("tests/sidebar_tree_view_controller_test.swift: error: 无法读取 \(sidebarSourcePath)\n", stderr)
    exit(1)
}

guard let treeSource = try? String(contentsOfFile: "Lokii/Lokii/ResultDirectoryTree.swift", encoding: .utf8) else {
    fputs("tests/sidebar_tree_view_controller_test.swift: error: 无法读取 ResultDirectoryTree.swift\n", stderr)
    exit(1)
}

guard let l10nSource = try? String(contentsOfFile: "Lokii/Lokii/L10n.swift", encoding: .utf8) else {
    fputs("tests/sidebar_tree_view_controller_test.swift: error: 无法读取 L10n.swift\n", stderr)
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

    public init(
        name: String,
        path: String,
        `extension`: String,
        isDir: Bool,
        size: UInt64,
        created: Int64,
        modified: Int64,
        matchPositions: [FfiMatchRange] = []
    ) {
        self.name = name
        self.path = path
        self.extension = `extension`
        self.isDir = isDir
        self.size = size
        self.created = created
        self.modified = modified
        self.matchPositions = matchPositions
    }
}
"""

let runnerSource = """
import Cocoa

\(mockFfiSource)
\(treeSource)
\(l10nSource)
\(sidebarSource)

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

final class MockSidebarDelegate: SidebarTreeViewControllerDelegate {
    var selectedPath: String?
    var callbackCount: Int = 0

    func sidebarDidSelectDirectory(path: String?) {
        selectedPath = path
        callbackCount += 1
    }
}

print("[1/6] 测试控制器初始化与视图层次构建...")
let controller = SidebarTreeViewController()
_ = controller.view // 触发 loadView 与 viewDidLoad

assertTrue(controller.view.subviews.contains(controller.scrollView), "根视图应包含滚动视图")
assertTrue(controller.view.subviews.contains(controller.emptyLabel), "根视图应包含空状态提示标签")
assertTrue(controller.scrollView.documentView is NSOutlineView, "滚动视图应挂载 NSOutlineView")
assertEqual(controller.outlineView.style, .sourceList, "OutlineView 样式应为 .sourceList")
assertEqual(controller.outlineView.numberOfColumns, 1, "OutlineView 应当只有 1 列")
assertEqual(controller.outlineView.allowsMultipleSelection, false, "应当禁用多选，仅支持单选")
assertEqual(controller.emptyLabel.stringValue, L("search.sidebar.emptyHint"), "空状态提示文案应匹配本地化键值")

print("[2/6] 测试初始状态与空数据更新 (Empty State)...")
assertEqual(controller.emptyLabel.isHidden, false, "初始状态下 emptyLabel 应处于可见状态")
assertEqual(controller.scrollView.isHidden, true, "初始状态下 scrollView 应处于隐藏状态")

// 传入 nil
controller.updateTree(nil)
assertEqual(controller.emptyLabel.isHidden, false, "传入 nil 时 emptyLabel 应展示")
assertEqual(controller.scrollView.isHidden, true, "传入 nil 时 scrollView 应隐藏")

// 传入空节点树
let emptyTree = ResultDirectoryTree(rootNodes: [], totalMatchCount: 0)
controller.updateTree(emptyTree)
assertEqual(controller.emptyLabel.isHidden, false, "传入无节点树时 emptyLabel 应展示")
assertEqual(controller.scrollView.isHidden, true, "传入无节点树时 scrollView 应隐藏")

print("[3/6] 测试聚合树数据加载与 OutlineView 数据源绑定...")
let coreChild = DirectoryTreeNode(
    component: "core",
    fullPath: "/workspace/src/core",
    directMatchCount: 3,
    recursiveMatchCount: 3,
    children: []
)
let srcNode = DirectoryTreeNode(
    component: "src",
    fullPath: "/workspace/src",
    directMatchCount: 2,
    recursiveMatchCount: 5,
    children: [coreChild]
)
coreChild.parent = srcNode

let docsNode = DirectoryTreeNode(
    component: "docs",
    fullPath: "/workspace/docs",
    directMatchCount: 2,
    recursiveMatchCount: 2,
    children: []
)

let sampleTree = ResultDirectoryTree(rootNodes: [srcNode, docsNode], totalMatchCount: 7)
controller.updateTree(sampleTree)

assertEqual(controller.emptyLabel.isHidden, true, "树有数据时 emptyLabel 应隐藏")
assertEqual(controller.scrollView.isHidden, false, "树有数据时 scrollView 应展示")
assertEqual(controller.allResultsItem.count, 7, "全部结果模型 count 应为 7")

// 验证数据源
assertEqual(controller.outlineView(controller.outlineView, numberOfChildrenOfItem: nil), 1, "根级别应当仅包含 1 个全部结果项")
assertTrue(controller.outlineView(controller.outlineView, isItemExpandable: controller.allResultsItem), "全部结果项应可展开")
assertEqual(controller.outlineView(controller.outlineView, numberOfChildrenOfItem: controller.allResultsItem), 2, "全部结果项下应包含 2 个顶层节点")
assertTrue(controller.outlineView(controller.outlineView, isItemExpandable: srcNode), "src 节点应可展开 (包含 core 子节点)")
assertEqual(controller.outlineView(controller.outlineView, isItemExpandable: docsNode), false, "docs 节点无子节点，不应可展开")

// 验证单元格渲染
let column = controller.outlineView.tableColumns[0]
let allResultsCell = controller.outlineView(controller.outlineView, viewFor: column, item: controller.allResultsItem) as? SidebarTreeCellView
assertTrue(allResultsCell != nil, "应当返回 SidebarTreeCellView")
assertEqual(allResultsCell?.textField?.stringValue, L("search.sidebar.allResults"), "全部结果单元格标题应为本地化全部结果")
assertEqual(allResultsCell?.countBadgeLabel.stringValue, "7", "全部结果单元格徽标应为 7")

let srcCell = controller.outlineView(controller.outlineView, viewFor: column, item: srcNode) as? SidebarTreeCellView
assertEqual(srcCell?.textField?.stringValue, "src", "src 单元格标题应为 src")
assertEqual(srcCell?.countBadgeLabel.stringValue, "5", "src 单元格徽标应为 5")

print("[4/6] 测试单选切换与代理回调机制...")
let mockDelegate = MockSidebarDelegate()
controller.delegate = mockDelegate

// 初始已默认选中全部结果，未产生变更时不重复回调
assertEqual(controller.selectedPath, nil, "初始选中路径应为 nil")

// 编程式选中 src 目录
controller.selectDirectory(path: "/workspace/src")
assertEqual(controller.selectedPath, "/workspace/src", "当前选中路径应为 /workspace/src")
assertEqual(mockDelegate.selectedPath, "/workspace/src", "代理应接收到 /workspace/src 变更回调")
assertEqual(mockDelegate.callbackCount, 1, "回调次数应为 1")

// 编程式选中子目录 core
controller.selectDirectory(path: "/workspace/src/core")
assertEqual(controller.selectedPath, "/workspace/src/core", "当前选中路径应为 /workspace/src/core")
assertEqual(mockDelegate.selectedPath, "/workspace/src/core", "代理应接收到 /workspace/src/core 变更回调")
assertEqual(mockDelegate.callbackCount, 2, "回调次数应为 2")

// 切回全部结果
controller.selectAllResults()
assertEqual(controller.selectedPath, nil, "切回全部结果后 selectedPath 应为 nil")
assertEqual(mockDelegate.selectedPath, nil, "切回全部结果后代理接收的路径应为 nil")
assertEqual(mockDelegate.callbackCount, 3, "回调次数应为 3")

print("[5/6] 测试文件夹展开记忆与静默恢复 (expandedPathSet)...")
// 手动展开 srcNode
controller.outlineView.expandItem(srcNode)
assertTrue(controller.expandedPathSet.contains("/workspace/src"), "展开 srcNode 后 expandedPathSet 应包含 /workspace/src")

// 模拟用户继续打字，构建更新后的聚合树（命中计数发生变化）
let updatedCore = DirectoryTreeNode(
    component: "core",
    fullPath: "/workspace/src/core",
    directMatchCount: 4,
    recursiveMatchCount: 4,
    children: []
)
let updatedSrc = DirectoryTreeNode(
    component: "src",
    fullPath: "/workspace/src",
    directMatchCount: 2,
    recursiveMatchCount: 6,
    children: [updatedCore]
)
updatedCore.parent = updatedSrc

let updatedDocs = DirectoryTreeNode(
    component: "docs",
    fullPath: "/workspace/docs",
    directMatchCount: 1,
    recursiveMatchCount: 1,
    children: []
)
let updatedTree = ResultDirectoryTree(rootNodes: [updatedSrc, updatedDocs], totalMatchCount: 7)

// 刷新聚合树
controller.updateTree(updatedTree)

// 验证静默恢复：此前展开的 /workspace/src 仍应在 expandedPathSet 中，且 outlineView 中依然展开
assertTrue(controller.expandedPathSet.contains("/workspace/src"), "刷新后 expandedPathSet 应保留 /workspace/src")
assertTrue(controller.outlineView.isItemExpanded(updatedSrc), "刷新后 updatedSrc 在 outlineView 中应自动恢复为展开状态")

// 折叠测试
controller.outlineView.collapseItem(updatedSrc)
assertEqual(controller.expandedPathSet.contains("/workspace/src"), false, "折叠后 expandedPathSet 应移除 /workspace/src")

print("[6/6] 测试 clear() 重置行为...")
controller.clear()
assertEqual(controller.currentTree == nil, true, "clear() 后 currentTree 应为 nil")
assertEqual(controller.selectedPath, nil, "clear() 后 selectedPath 应为 nil")
assertTrue(controller.expandedPathSet.isEmpty, "clear() 后 expandedPathSet 应清空")
assertEqual(controller.emptyLabel.isHidden, false, "clear() 后 emptyLabel 应恢复可见")
assertEqual(controller.scrollView.isHidden, true, "clear() 后 scrollView 应隐藏")

print("测试全部通过：6/6 项断言成功。")
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
