import Foundation

// MARK: - Standalone Test Harness for ResultDirectoryTree

let sourcePath = "Lokii/Lokii/ResultDirectoryTree.swift"
guard FileManager.default.fileExists(atPath: sourcePath) else {
    fputs("tests/result_directory_tree_test.swift: error: 找不到 ResultDirectoryTree 类型 (\(sourcePath) 未实现)\n", stderr)
    exit(1)
}

guard let treeSource = try? String(contentsOfFile: sourcePath, encoding: .utf8) else {
    fputs("tests/result_directory_tree_test.swift: error: 无法读取 \(sourcePath)\n", stderr)
    exit(1)
}

let ffiHeader = """
import Foundation

public struct FfiMatchRange: Equatable, Hashable {
    public var start: UInt32
    public var end: UInt32
    public init(start: UInt32 = 0, end: UInt32 = 0) {
        self.start = start
        self.end = end
    }
}

public enum FfiSearchMode: Equatable, Hashable {
    case auto
    case substring
    case wildcard
    case regex
}

public struct FfiSearchResult: Equatable, Hashable {
    public var name: String
    public var path: String
    public var isDir: Bool
    public var size: UInt64
    public var modified: UInt64
    public var created: UInt64
    public var `extension`: String
    public var score: Double
    public var matchPositions: [FfiMatchRange]
    public var searchMode: FfiSearchMode

    public init(
        name: String,
        path: String,
        isDir: Bool,
        size: UInt64 = 0,
        modified: UInt64 = 0,
        created: UInt64 = 0,
        `extension`: String = "",
        score: Double = 0.0,
        matchPositions: [FfiMatchRange] = [],
        searchMode: FfiSearchMode = .auto
    ) {
        self.name = name
        self.path = path
        self.isDir = isDir
        self.size = size
        self.modified = modified
        self.created = created
        self.`extension` = `extension`
        self.score = score
        self.matchPositions = matchPositions
        self.searchMode = searchMode
    }
}
"""

let testSuiteCode = #"""

func makeFileResult(name: String, path: String) -> FfiSearchResult {
    return FfiSearchResult(name: name, path: path, isDir: false)
}

func makeDirResult(name: String, path: String) -> FfiSearchResult {
    return FfiSearchResult(name: name, path: path, isDir: true)
}

func testEmptyResultsBuild() {
    let tree = ResultDirectoryTree.build(from: [], rootPrefix: nil)
    assert(tree.rootNodes.isEmpty, "空结果集构建的前缀树根节点数组必须为空")
    assert(tree.totalMatchCount == 0, "空结果集的命中统计必须为 0")

    let filtered = ResultDirectoryTree.filter(results: [], byDirectoryPrefix: "/Users")
    assert(filtered.isEmpty, "空结果集过滤后条目数必须为 0")
}

func testStandardDeepPathInsertion() {
    let results: [FfiSearchResult] = [
        makeFileResult(name: "main.swift", path: "/workspace/lokii/Sources/App/main.swift"),
        makeFileResult(name: "AppDelegate.swift", path: "/workspace/lokii/Sources/App/AppDelegate.swift"),
        makeFileResult(name: "Engine.swift", path: "/workspace/lokii/Sources/Core/Engine.swift"),
        makeFileResult(name: "AppTest.swift", path: "/workspace/lokii/Tests/AppTests/AppTest.swift")
    ]

    let tree = ResultDirectoryTree.build(from: results, rootPrefix: nil)
    assert(tree.totalMatchCount == 4, "全树累计命中条目应为 4")
    assert(tree.rootNodes.count == 1, "所有路径均位于 /workspace/lokii 下，应单根聚合")

    let root = tree.rootNodes[0]
    assert(root.component == "workspace/lokii", "无直属命中的单链根前缀应紧凑折叠为 workspace/lokii，实际为 \(root.component)")
    assert(root.fullPath == "/workspace/lokii", "根节点 fullPath 应为 /workspace/lokii")
    assert(root.directMatchCount == 0, "workspace/lokii 无直接匹配文件，直属计数应为 0")
    assert(root.recursiveMatchCount == 4, "workspace/lokii 递归累计计数应为 4")
    assert(root.parent == nil, "根节点的 parent 必须为 nil")
    assert(root.children.count == 2, "workspace/lokii 应包含 Sources 与 Tests 两个子分支")

    // 按名称排序：Sources 排在 Tests 前面
    let sourcesNode = root.children[0]
    let testsNode = root.children[1]

    assert(sourcesNode.component == "Sources", "首个子节点应为 Sources")
    assert(sourcesNode.fullPath == "/workspace/lokii/Sources", "Sources 节点 fullPath 应为 /workspace/lokii/Sources")
    assert(sourcesNode.directMatchCount == 0, "Sources 无直接文件匹配，直属计数应为 0")
    assert(sourcesNode.recursiveMatchCount == 3, "Sources 分支累计命中应为 3")
    assert(sourcesNode.parent === root, "Sources 的 parent 应指向 root")
    assert(sourcesNode.children.count == 2, "Sources 应包含 App 与 Core 两个子目录")

    // Sources 下的 App 与 Core
    let appNode = sourcesNode.children[0]
    let coreNode = sourcesNode.children[1]

    assert(appNode.component == "App", "Sources 子节点应为 App")
    assert(appNode.fullPath == "/workspace/lokii/Sources/App", "App fullPath 应准确")
    assert(appNode.directMatchCount == 2, "App 目录下直接匹配 2 个文件")
    assert(appNode.recursiveMatchCount == 2, "App 递归计数应为 2")
    assert(appNode.children.isEmpty, "App 无子目录")
    assert(appNode.parent === sourcesNode, "App 的 parent 应指向 Sources")

    assert(coreNode.component == "Core", "Sources 子节点应为 Core")
    assert(coreNode.fullPath == "/workspace/lokii/Sources/Core", "Core fullPath 应准确")
    assert(coreNode.directMatchCount == 1, "Core 目录下直接匹配 1 个文件")
    assert(coreNode.recursiveMatchCount == 1, "Core 递归计数应为 1")
    assert(coreNode.children.isEmpty, "Core 无子目录")
    assert(coreNode.parent === sourcesNode, "Core 的 parent 应指向 Sources")

    // Tests 分支：Tests 下仅有 AppTests，应紧凑合并为 Tests/AppTests
    assert(testsNode.component == "Tests/AppTests", "单链空子目录 Tests 与 AppTests 应合并为 Tests/AppTests，实际为 \(testsNode.component)")
    assert(testsNode.fullPath == "/workspace/lokii/Tests/AppTests", "Tests/AppTests fullPath 应为终点路径")
    assert(testsNode.directMatchCount == 1, "Tests/AppTests 下包含 1 个直接匹配")
    assert(testsNode.recursiveMatchCount == 1, "Tests/AppTests 递归计数应为 1")
    assert(testsNode.parent === root, "Tests/AppTests 的 parent 应直接指向 root")
    assert(testsNode.children.isEmpty, "Tests/AppTests 无后续子目录")
}

func testCompactFoldersMerging() {
    // 纯单链多级空目录测试：a -> b -> c -> d (包含 2 个直接匹配)
    let results: [FfiSearchResult] = [
        makeFileResult(name: "file1.txt", path: "/a/b/c/d/file1.txt"),
        makeFileResult(name: "file2.txt", path: "/a/b/c/d/file2.txt")
    ]

    let tree = ResultDirectoryTree.build(from: results, rootPrefix: nil)
    assert(tree.rootNodes.count == 1, "应合并为唯一直属根节点")
    let single = tree.rootNodes[0]
    assert(single.component == "a/b/c/d", "单链中间空目录应全部折叠为 a/b/c/d，实际为: \(single.component)")
    assert(single.fullPath == "/a/b/c/d", "fullPath 应为叶子目录绝对路径")
    assert(single.directMatchCount == 2, "directMatchCount 应为 2")
    assert(single.recursiveMatchCount == 2, "recursiveMatchCount 应为 2")
    assert(single.children.isEmpty, "children 应为空")
    assert(single.parent == nil, "根节点 parent 应为 nil")

    // 增加分叉测试：a/b/c 分叉为 d 与 e
    let branchResults: [FfiSearchResult] = [
        makeFileResult(name: "file1.txt", path: "/a/b/c/d/file1.txt"),
        makeFileResult(name: "file2.txt", path: "/a/b/c/d/file2.txt"),
        makeFileResult(name: "file3.txt", path: "/a/b/c/e/file3.txt")
    ]

    let branchTree = ResultDirectoryTree.build(from: branchResults, rootPrefix: nil)
    assert(branchTree.rootNodes.count == 1)
    let branchRoot = branchTree.rootNodes[0]
    assert(branchRoot.component == "a/b/c", "分叉前单链应紧凑折叠至分叉点 a/b/c，实际为: \(branchRoot.component)")
    assert(branchRoot.fullPath == "/a/b/c")
    assert(branchRoot.directMatchCount == 0, "分叉点本身无直属文件匹配")
    assert(branchRoot.recursiveMatchCount == 3, "分叉点累计命中应为 3")
    assert(branchRoot.children.count == 2, "分叉点应分出 2 个子节点")

    let dChild = branchRoot.children[0]
    let eChild = branchRoot.children[1]

    assert(dChild.component == "d", "子节点 1 为 d")
    assert(dChild.fullPath == "/a/b/c/d")
    assert(dChild.directMatchCount == 2)
    assert(dChild.recursiveMatchCount == 2)
    assert(dChild.parent === branchRoot, "dChild parent 必须指向 branchRoot")

    assert(eChild.component == "e", "子节点 2 为 e")
    assert(eChild.fullPath == "/a/b/c/e")
    assert(eChild.directMatchCount == 1)
    assert(eChild.recursiveMatchCount == 1)
    assert(eChild.parent === branchRoot, "eChild parent 必须指向 branchRoot")
}

func testNonEmptyIntermediateDirectoryDoesNotFold() {
    // 中间目录包含直属匹配项时，严禁与子目录盲目折叠，以保障用户点击可定位到该层级直接匹配
    let results: [FfiSearchResult] = [
        makeFileResult(name: "b_file.txt", path: "/a/b/b_file.txt"),
        makeFileResult(name: "c_file.txt", path: "/a/b/c/c_file.txt")
    ]

    let tree = ResultDirectoryTree.build(from: results, rootPrefix: nil)
    assert(tree.rootNodes.count == 1)
    let root = tree.rootNodes[0]

    // a 无直属，b 有直属：a 与 b 合并为 a/b，但 b 不能与 c 合并
    assert(root.component == "a/b", "无直属命中的 a 应与 b 合并为 a/b，但因 b 拥有直接匹配，不能与 c 继续合并，实际为: \(root.component)")
    assert(root.fullPath == "/a/b")
    assert(root.directMatchCount == 1, "a/b 拥有 1 个直接匹配")
    assert(root.recursiveMatchCount == 2, "a/b 累计拥有 2 个匹配")
    assert(root.children.count == 1, "a/b 应保留 c 为子节点")

    let cNode = root.children[0]
    assert(cNode.component == "c", "c 节点应独立展示")
    assert(cNode.fullPath == "/a/b/c")
    assert(cNode.directMatchCount == 1)
    assert(cNode.recursiveMatchCount == 1)
    assert(cNode.parent === root, "cNode parent 必须指向 a/b 节点")
}

func testDirectoryPrefixFiltering() {
    let r1 = makeFileResult(name: "r1.txt", path: "/project/src/core/r1.txt")
    let r2 = makeFileResult(name: "r2.txt", path: "/project/src/core/sub/r2.txt")
    let r3 = makeFileResult(name: "r3.txt", path: "/project/src/ui/r3.txt")
    let r4 = makeFileResult(name: "r4.txt", path: "/project/src_extra/r4.txt")
    let r5 = makeFileResult(name: "r5.txt", path: "/other/r5.txt")
    let rDir = makeDirResult(name: "core", path: "/project/src/core")

    let allResults = [r1, r2, r3, r4, r5, rDir]

    // 1. nil 前缀：返回全量
    let filterNil = ResultDirectoryTree.filter(results: allResults, byDirectoryPrefix: nil)
    assert(filterNil == allResults, "nil 前缀应原样返回全量结果集")

    // 2. 空字符串前缀：返回全量
    let filterEmpty = ResultDirectoryTree.filter(results: allResults, byDirectoryPrefix: "")
    assert(filterEmpty == allResults, "空字符串前缀应原样返回全量结果集")

    // 3. 过滤 /project/src/core：应包含 r1, r2, rDir
    let filterCore = ResultDirectoryTree.filter(results: allResults, byDirectoryPrefix: "/project/src/core")
    assert(filterCore == [r1, r2, rDir], "应仅保留 /project/src/core 及其子项")

    // 4. 末尾斜杠容错：过滤 /project/src/core/ 与 /project/src/core 等价
    let filterCoreSlash = ResultDirectoryTree.filter(results: allResults, byDirectoryPrefix: "/project/src/core/")
    assert(filterCoreSlash == [r1, r2, rDir], "末尾斜杠容错应与无斜杠结果一致")

    // 5. 路径边界守卫：/project/src 严禁错误匹配到 /project/src_extra
    let filterSrc = ResultDirectoryTree.filter(results: allResults, byDirectoryPrefix: "/project/src")
    assert(!filterSrc.contains(r4), "前缀 /project/src 绝不能误匹配 /project/src_extra 目录下的文件")
    assert(filterSrc.contains(r1) && filterSrc.contains(r2) && filterSrc.contains(r3) && filterSrc.contains(rDir))
    assert(!filterSrc.contains(r5), "前缀 /project/src 绝不能包含 /other 下的文件")

    // 6. 不存在的目录前缀
    let filterNone = ResultDirectoryTree.filter(results: allResults, byDirectoryPrefix: "/nonexistent")
    assert(filterNone.isEmpty, "不存在的前缀应返回空结果集")
}

func testRootPrefixScoping() {
    let r1 = makeFileResult(name: "a.swift", path: "/root/lokii/src/a.swift")
    let r2 = makeFileResult(name: "b.swift", path: "/root/lokii/src/b.swift")
    let r3 = makeFileResult(name: "c.swift", path: "/root/other/c.swift")

    // 指定 rootPrefix 为 /root/lokii
    let tree = ResultDirectoryTree.build(from: [r1, r2, r3], rootPrefix: "/root/lokii")
    assert(tree.totalMatchCount == 2, "在 rootPrefix 作用域内应仅累计属于该目录树的 2 个匹配项")
    assert(tree.rootNodes.count == 1, "rootNodes 应仅展示 /root/lokii 范围内的子树")

    let root = tree.rootNodes[0]
    assert(root.component == "src", "指定 rootPrefix 为 /root/lokii 后，子节点 component 应为 src，实际为: \(root.component)")
    assert(root.fullPath == "/root/lokii/src", "fullPath 仍应保持完整绝对路径")
    assert(root.directMatchCount == 2)
    assert(root.recursiveMatchCount == 2)
}

func testNodeEquatableAndParentWeakReference() {
    let child1 = DirectoryTreeNode(component: "sub", fullPath: "/a/sub", directMatchCount: 1, recursiveMatchCount: 1)
    let parent1 = DirectoryTreeNode(component: "a", fullPath: "/a", directMatchCount: 0, recursiveMatchCount: 1, children: [child1])
    child1.parent = parent1

    let child2 = DirectoryTreeNode(component: "sub", fullPath: "/a/sub", directMatchCount: 1, recursiveMatchCount: 1)
    let parent2 = DirectoryTreeNode(component: "a", fullPath: "/a", directMatchCount: 0, recursiveMatchCount: 1, children: [child2])
    child2.parent = parent2

    assert(parent1 == parent2, "相同结构与数值的树节点应判定相等")

    let differentChild = DirectoryTreeNode(component: "other", fullPath: "/a/other", directMatchCount: 1, recursiveMatchCount: 1)
    let parent3 = DirectoryTreeNode(component: "a", fullPath: "/a", directMatchCount: 0, recursiveMatchCount: 1, children: [differentChild])
    assert(parent1 != parent3, "子节点不同的树节点应判定不相等")
}

print("[1/7] 测试空输入结果集构建...")
testEmptyResultsBuild()

print("[2/7] 测试常规深层路径插入与层级计数准确性...")
testStandardDeepPathInsertion()

print("[3/7] 测试单链空目录紧凑折叠 (Compact Folders)...")
testCompactFoldersMerging()

print("[4/7] 测试中间目录存在直属命中项时不发生跨级折叠...")
testNonEmptyIntermediateDirectoryDoesNotFold()

print("[5/7] 测试基于目录前缀的结果集过滤与边界守卫...")
testDirectoryPrefixFiltering()

print("[6/7] 测试指定根前缀 (rootPrefix) 范围构建...")
testRootPrefixScoping()

print("[7/7] 测试目录节点 Equatable 相等性比较与弱引用父节点...")
testNodeEquatableAndParentWeakReference()

print("测试全部通过：7/7 项断言成功。")
"""#

let tempDir = FileManager.default.temporaryDirectory
let tempFile = tempDir.appendingPathComponent("lokii_result_directory_tree_test_\(UUID().uuidString).swift")

let combined = """
\(ffiHeader)

// MARK: - SUT
\(treeSource)

// MARK: - Test Runner
\(testSuiteCode)
"""

do {
    try combined.write(to: tempFile, atomically: true, encoding: .utf8)
} catch {
    fputs("tests/result_directory_tree_test.swift: error: 无法写入临时测试文件: \(error)\n", stderr)
    exit(1)
}
defer {
    try? FileManager.default.removeItem(at: tempFile)
}

let process = Process()
process.executableURL = URL(fileURLWithPath: "/usr/bin/swift")
process.arguments = [tempFile.path]
process.standardInput = FileHandle.standardInput
process.standardOutput = FileHandle.standardOutput
process.standardError = FileHandle.standardError

do {
    try process.run()
    process.waitUntilExit()
    exit(process.terminationStatus)
} catch {
    fputs("tests/result_directory_tree_test.swift: error: 执行测试进程失败: \(error)\n", stderr)
    exit(1)
}
