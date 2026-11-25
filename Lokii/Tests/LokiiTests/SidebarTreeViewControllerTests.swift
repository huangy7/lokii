import Cocoa
import XCTest
@testable import LokiiAppLib

final class SidebarTreeViewControllerTests: XCTestCase {
    private final class MockSidebarDelegate: SidebarTreeViewControllerDelegate {
        var selectedPath: String?
        var callbackCount = 0

        func sidebarDidSelectDirectory(path: String?) {
            selectedPath = path
            callbackCount += 1
        }
    }

    func testInitializationAndEmptyStates() {
        let controller = SidebarTreeViewController()
        _ = controller.view

        XCTAssertTrue(controller.view.subviews.contains(controller.scrollView))
        XCTAssertTrue(controller.view.subviews.contains(controller.emptyLabel))
        XCTAssertTrue(controller.scrollView.documentView is NSOutlineView)
        XCTAssertEqual(controller.outlineView.style, .sourceList)
        XCTAssertEqual(controller.outlineView.numberOfColumns, 1)
        XCTAssertFalse(controller.outlineView.allowsMultipleSelection)
        XCTAssertEqual(controller.emptyLabel.stringValue, L("search.sidebar.emptyHint"))

        XCTAssertFalse(controller.emptyLabel.isHidden)
        XCTAssertTrue(controller.scrollView.isHidden)

        controller.updateTree(nil)
        XCTAssertFalse(controller.emptyLabel.isHidden)
        XCTAssertTrue(controller.scrollView.isHidden)

        let emptyTree = ResultDirectoryTree(rootNodes: [], totalMatchCount: 0)
        controller.updateTree(emptyTree)
        XCTAssertFalse(controller.emptyLabel.isHidden)
        XCTAssertTrue(controller.scrollView.isHidden)
    }

    func testTreeBindingSelectionExpansionAndFoldedAncestors() {
        let controller = SidebarTreeViewController()
        _ = controller.view

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

        XCTAssertTrue(controller.emptyLabel.isHidden)
        XCTAssertFalse(controller.scrollView.isHidden)
        XCTAssertEqual(controller.allResultsItem.count, 7)

        XCTAssertEqual(controller.outlineView(controller.outlineView, numberOfChildrenOfItem: nil), 1)
        XCTAssertTrue(controller.outlineView(controller.outlineView, isItemExpandable: controller.allResultsItem))
        XCTAssertEqual(controller.outlineView(controller.outlineView, numberOfChildrenOfItem: controller.allResultsItem), 2)
        XCTAssertTrue(controller.outlineView(controller.outlineView, isItemExpandable: srcNode))
        XCTAssertFalse(controller.outlineView(controller.outlineView, isItemExpandable: docsNode))

        let column = controller.outlineView.tableColumns[0]
        let allResultsCell = controller.outlineView(controller.outlineView, viewFor: column, item: controller.allResultsItem) as? SidebarTreeCellView
        XCTAssertNotNil(allResultsCell)
        XCTAssertEqual(allResultsCell?.textField?.stringValue, L("search.sidebar.allResults"))
        XCTAssertEqual(allResultsCell?.countBadgeLabel.stringValue, "7")
        XCTAssertNil(allResultsCell?.toolTip)

        let srcCell = controller.outlineView(controller.outlineView, viewFor: column, item: srcNode) as? SidebarTreeCellView
        XCTAssertEqual(srcCell?.textField?.stringValue, "src")
        XCTAssertEqual(srcCell?.countBadgeLabel.stringValue, "5")
        XCTAssertEqual(srcCell?.toolTip, srcNode.fullPath)
        XCTAssertEqual(srcCell?.textField?.toolTip, srcNode.fullPath)

        let mockDelegate = MockSidebarDelegate()
        controller.delegate = mockDelegate
        XCTAssertNil(controller.selectedPath)

        controller.selectDirectory(path: "/workspace/src")
        XCTAssertEqual(controller.selectedPath, "/workspace/src")
        XCTAssertEqual(mockDelegate.selectedPath, "/workspace/src")
        XCTAssertEqual(mockDelegate.callbackCount, 1)

        controller.selectDirectory(path: "/workspace/src/core")
        XCTAssertEqual(controller.selectedPath, "/workspace/src/core")
        XCTAssertEqual(mockDelegate.selectedPath, "/workspace/src/core")
        XCTAssertEqual(mockDelegate.callbackCount, 2)

        controller.selectAllResults()
        XCTAssertNil(controller.selectedPath)
        XCTAssertNil(mockDelegate.selectedPath)
        XCTAssertEqual(mockDelegate.callbackCount, 3)

        // 文件夹展开记忆与静默恢复
        controller.outlineView.expandItem(srcNode)
        XCTAssertTrue(controller.expandedPathSet.contains("/workspace/src"))

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
        controller.updateTree(ResultDirectoryTree(rootNodes: [updatedSrc, updatedDocs], totalMatchCount: 7))

        XCTAssertTrue(controller.expandedPathSet.contains("/workspace/src"))
        XCTAssertTrue(controller.outlineView.isItemExpanded(updatedSrc))

        controller.outlineView.collapseItem(updatedSrc)
        XCTAssertFalse(controller.expandedPathSet.contains("/workspace/src"))

        // clear() 重置
        controller.clear()
        XCTAssertNil(controller.currentTree)
        XCTAssertNil(controller.selectedPath)
        XCTAssertTrue(controller.expandedPathSet.isEmpty)
        XCTAssertFalse(controller.emptyLabel.isHidden)
        XCTAssertTrue(controller.scrollView.isHidden)

        // 单链折叠目录祖先过滤选中与退出
        let foldedResults = [
            TestFixture.searchResult(name: "main.rs", path: "/workspace/lokii/src/main.rs", extension: "rs", size: 100, created: 1, modified: 1),
            TestFixture.searchResult(name: "lib.rs", path: "/workspace/lokii/src/lib.rs", extension: "rs", size: 100, created: 1, modified: 1),
            TestFixture.searchResult(name: "notes.txt", path: "/other/docs/notes.txt", extension: "txt", size: 100, created: 1, modified: 1),
        ]
        controller.updateTree(ResultDirectoryTree.build(from: foldedResults))
        mockDelegate.callbackCount = 0
        controller.selectDirectory(path: "/other")
        XCTAssertEqual(controller.selectedPath, "/other")
        XCTAssertGreaterThan(controller.outlineView.selectedRow, 0)
        XCTAssertEqual(mockDelegate.selectedPath, "/other")

        controller.selectAllResults()
        XCTAssertNil(controller.selectedPath)
        XCTAssertNil(mockDelegate.selectedPath)
    }
}
