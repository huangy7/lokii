import Foundation
import XCTest
@testable import LokiiAppLib

final class ResultDirectoryTreeTests: XCTestCase {
    private func makeFileResult(name: String, path: String) -> FfiSearchResult {
        TestFixture.searchResult(name: name, path: path, isDir: false)
    }

    private func makeDirResult(name: String, path: String) -> FfiSearchResult {
        TestFixture.searchResult(name: name, path: path, isDir: true)
    }

    func testEmptyResultsBuild() {
        let tree = ResultDirectoryTree.build(from: [], rootPrefix: nil)
        XCTAssertTrue(tree.rootNodes.isEmpty, "空结果集构建的前缀树根节点数组必须为空")
        XCTAssertEqual(tree.totalMatchCount, 0, "空结果集的命中统计必须为 0")

        let filtered = ResultDirectoryTree.filter(results: [], byDirectoryPrefix: "/Users")
        XCTAssertTrue(filtered.isEmpty, "空结果集过滤后条目数必须为 0")
    }

    func testStandardDeepPathInsertion() {
        let results: [FfiSearchResult] = [
            makeFileResult(name: "main.swift", path: "/workspace/lokii/Sources/App/main.swift"),
            makeFileResult(name: "AppDelegate.swift", path: "/workspace/lokii/Sources/App/AppDelegate.swift"),
            makeFileResult(name: "Engine.swift", path: "/workspace/lokii/Sources/Core/Engine.swift"),
            makeFileResult(name: "AppTest.swift", path: "/workspace/lokii/Tests/AppTests/AppTest.swift"),
        ]

        let tree = ResultDirectoryTree.build(from: results, rootPrefix: nil)
        XCTAssertEqual(tree.totalMatchCount, 4, "全树累计命中条目应为 4")
        XCTAssertEqual(tree.rootNodes.count, 1, "所有路径均位于 /workspace/lokii 下，应单根聚合")

        let root = tree.rootNodes[0]
        XCTAssertEqual(root.component, "workspace/lokii")
        XCTAssertEqual(root.fullPath, "/workspace/lokii")
        XCTAssertEqual(root.directMatchCount, 0)
        XCTAssertEqual(root.recursiveMatchCount, 4)
        XCTAssertNil(root.parent)
        XCTAssertEqual(root.children.count, 2)

        let sourcesNode = root.children[0]
        let testsNode = root.children[1]

        XCTAssertEqual(sourcesNode.component, "Sources")
        XCTAssertEqual(sourcesNode.fullPath, "/workspace/lokii/Sources")
        XCTAssertEqual(sourcesNode.directMatchCount, 0)
        XCTAssertEqual(sourcesNode.recursiveMatchCount, 3)
        XCTAssertTrue(sourcesNode.parent === root)
        XCTAssertEqual(sourcesNode.children.count, 2)

        let appNode = sourcesNode.children[0]
        let coreNode = sourcesNode.children[1]

        XCTAssertEqual(appNode.component, "App")
        XCTAssertEqual(appNode.fullPath, "/workspace/lokii/Sources/App")
        XCTAssertEqual(appNode.directMatchCount, 2)
        XCTAssertEqual(appNode.recursiveMatchCount, 2)
        XCTAssertTrue(appNode.children.isEmpty)
        XCTAssertTrue(appNode.parent === sourcesNode)

        XCTAssertEqual(coreNode.component, "Core")
        XCTAssertEqual(coreNode.fullPath, "/workspace/lokii/Sources/Core")
        XCTAssertEqual(coreNode.directMatchCount, 1)
        XCTAssertEqual(coreNode.recursiveMatchCount, 1)
        XCTAssertTrue(coreNode.children.isEmpty)
        XCTAssertTrue(coreNode.parent === sourcesNode)

        XCTAssertEqual(testsNode.component, "Tests/AppTests")
        XCTAssertEqual(testsNode.fullPath, "/workspace/lokii/Tests/AppTests")
        XCTAssertEqual(testsNode.directMatchCount, 1)
        XCTAssertEqual(testsNode.recursiveMatchCount, 1)
        XCTAssertTrue(testsNode.parent === root)
        XCTAssertTrue(testsNode.children.isEmpty)
    }

    func testCompactFoldersMerging() {
        let results: [FfiSearchResult] = [
            makeFileResult(name: "file1.txt", path: "/a/b/c/d/file1.txt"),
            makeFileResult(name: "file2.txt", path: "/a/b/c/d/file2.txt"),
        ]

        let tree = ResultDirectoryTree.build(from: results, rootPrefix: nil)
        XCTAssertEqual(tree.rootNodes.count, 1)
        let single = tree.rootNodes[0]
        XCTAssertEqual(single.component, "a/b/c/d")
        XCTAssertEqual(single.fullPath, "/a/b/c/d")
        XCTAssertEqual(single.directMatchCount, 2)
        XCTAssertEqual(single.recursiveMatchCount, 2)
        XCTAssertTrue(single.children.isEmpty)
        XCTAssertNil(single.parent)

        let branchResults: [FfiSearchResult] = [
            makeFileResult(name: "file1.txt", path: "/a/b/c/d/file1.txt"),
            makeFileResult(name: "file2.txt", path: "/a/b/c/d/file2.txt"),
            makeFileResult(name: "file3.txt", path: "/a/b/c/e/file3.txt"),
        ]

        let branchTree = ResultDirectoryTree.build(from: branchResults, rootPrefix: nil)
        XCTAssertEqual(branchTree.rootNodes.count, 1)
        let branchRoot = branchTree.rootNodes[0]
        XCTAssertEqual(branchRoot.component, "a/b/c")
        XCTAssertEqual(branchRoot.fullPath, "/a/b/c")
        XCTAssertEqual(branchRoot.directMatchCount, 0)
        XCTAssertEqual(branchRoot.recursiveMatchCount, 3)
        XCTAssertEqual(branchRoot.children.count, 2)

        let dChild = branchRoot.children[0]
        let eChild = branchRoot.children[1]

        XCTAssertEqual(dChild.component, "d")
        XCTAssertEqual(dChild.fullPath, "/a/b/c/d")
        XCTAssertEqual(dChild.directMatchCount, 2)
        XCTAssertEqual(dChild.recursiveMatchCount, 2)
        XCTAssertTrue(dChild.parent === branchRoot)

        XCTAssertEqual(eChild.component, "e")
        XCTAssertEqual(eChild.fullPath, "/a/b/c/e")
        XCTAssertEqual(eChild.directMatchCount, 1)
        XCTAssertEqual(eChild.recursiveMatchCount, 1)
        XCTAssertTrue(eChild.parent === branchRoot)
    }

    func testNonEmptyIntermediateDirectoryDoesNotFold() {
        let results: [FfiSearchResult] = [
            makeFileResult(name: "b_file.txt", path: "/a/b/b_file.txt"),
            makeFileResult(name: "c_file.txt", path: "/a/b/c/c_file.txt"),
        ]

        let tree = ResultDirectoryTree.build(from: results, rootPrefix: nil)
        XCTAssertEqual(tree.rootNodes.count, 1)
        let root = tree.rootNodes[0]

        XCTAssertEqual(root.component, "a/b")
        XCTAssertEqual(root.fullPath, "/a/b")
        XCTAssertEqual(root.directMatchCount, 1)
        XCTAssertEqual(root.recursiveMatchCount, 2)
        XCTAssertEqual(root.children.count, 1)

        let cNode = root.children[0]
        XCTAssertEqual(cNode.component, "c")
        XCTAssertEqual(cNode.fullPath, "/a/b/c")
        XCTAssertEqual(cNode.directMatchCount, 1)
        XCTAssertEqual(cNode.recursiveMatchCount, 1)
        XCTAssertTrue(cNode.parent === root)
    }

    func testDirectoryPrefixFiltering() {
        let r1 = makeFileResult(name: "r1.txt", path: "/project/src/core/r1.txt")
        let r2 = makeFileResult(name: "r2.txt", path: "/project/src/core/sub/r2.txt")
        let r3 = makeFileResult(name: "r3.txt", path: "/project/src/ui/r3.txt")
        let r4 = makeFileResult(name: "r4.txt", path: "/project/src_extra/r4.txt")
        let r5 = makeFileResult(name: "r5.txt", path: "/other/r5.txt")
        let rDir = makeDirResult(name: "core", path: "/project/src/core")

        let allResults = [r1, r2, r3, r4, r5, rDir]

        XCTAssertEqual(ResultDirectoryTree.filter(results: allResults, byDirectoryPrefix: nil), allResults)
        XCTAssertEqual(ResultDirectoryTree.filter(results: allResults, byDirectoryPrefix: ""), allResults)
        XCTAssertEqual(ResultDirectoryTree.filter(results: allResults, byDirectoryPrefix: "/project/src/core"), [r1, r2, rDir])
        XCTAssertEqual(ResultDirectoryTree.filter(results: allResults, byDirectoryPrefix: "/project/src/core/"), [r1, r2, rDir])

        let filterSrc = ResultDirectoryTree.filter(results: allResults, byDirectoryPrefix: "/project/src")
        XCTAssertFalse(filterSrc.contains(r4))
        XCTAssertTrue(filterSrc.contains(r1) && filterSrc.contains(r2) && filterSrc.contains(r3) && filterSrc.contains(rDir))
        XCTAssertFalse(filterSrc.contains(r5))

        XCTAssertTrue(ResultDirectoryTree.filter(results: allResults, byDirectoryPrefix: "/nonexistent").isEmpty)
    }

    func testRootPrefixScoping() {
        let r1 = makeFileResult(name: "a.swift", path: "/root/lokii/src/a.swift")
        let r2 = makeFileResult(name: "b.swift", path: "/root/lokii/src/b.swift")
        let r3 = makeFileResult(name: "c.swift", path: "/root/other/c.swift")

        let tree = ResultDirectoryTree.build(from: [r1, r2, r3], rootPrefix: "/root/lokii")
        XCTAssertEqual(tree.totalMatchCount, 2)
        XCTAssertEqual(tree.rootNodes.count, 1)

        let root = tree.rootNodes[0]
        XCTAssertEqual(root.component, "src")
        XCTAssertEqual(root.fullPath, "/root/lokii/src")
        XCTAssertEqual(root.directMatchCount, 2)
        XCTAssertEqual(root.recursiveMatchCount, 2)
    }

    func testNodeEquatableAndParentWeakReference() {
        let child1 = DirectoryTreeNode(component: "sub", fullPath: "/a/sub", directMatchCount: 1, recursiveMatchCount: 1)
        let parent1 = DirectoryTreeNode(component: "a", fullPath: "/a", directMatchCount: 0, recursiveMatchCount: 1, children: [child1])
        child1.parent = parent1

        let child2 = DirectoryTreeNode(component: "sub", fullPath: "/a/sub", directMatchCount: 1, recursiveMatchCount: 1)
        let parent2 = DirectoryTreeNode(component: "a", fullPath: "/a", directMatchCount: 0, recursiveMatchCount: 1, children: [child2])
        child2.parent = parent2

        XCTAssertEqual(parent1, parent2)

        let differentChild = DirectoryTreeNode(component: "other", fullPath: "/a/other", directMatchCount: 1, recursiveMatchCount: 1)
        let parent3 = DirectoryTreeNode(component: "a", fullPath: "/a", directMatchCount: 0, recursiveMatchCount: 1, children: [differentChild])
        XCTAssertNotEqual(parent1, parent3)
    }
}
