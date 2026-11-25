import Cocoa
import UniformTypeIdentifiers
import XCTest
@testable import LokiiAppLib

final class SplitViewControllerTests: XCTestCase {
    private final class MockSplitDelegate: SearchMainSplitViewControllerDelegate {
        var lastSelectedResult: FfiSearchResult?
        var selectCallCount = 0
        var lastDoubleClickedResult: FfiSearchResult?
        var doubleClickCallCount = 0
        var rebuildCallCount = 0
        var focusSearchFieldCallCount = 0
        var trashedPaths: Set<String> = []
        var trashedRemainingCount = 0
        var trashCallCount = 0
        var lastFilteredCount: Int?
        var lastFilteredDirectory: String?

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

        func searchMainSplitViewDidRequestFocusSearchField() {
            focusSearchFieldCallCount += 1
        }

        func searchMainSplitViewDidTrashItems(paths: Set<String>, remainingCount: Int) {
            trashedPaths = paths
            trashedRemainingCount = remainingCount
            trashCallCount += 1
        }

        func searchMainSplitViewDidFilterResults(filteredCount: Int, totalCount: Int, activeDirectory: String?) {
            lastFilteredCount = filteredCount
            lastFilteredDirectory = activeDirectory
        }
    }

    private final class IdleEmptyRecursionGuardDelegate: SearchMainSplitViewControllerDelegate {
        weak var splitVC: SearchMainSplitViewController?
        var observedUpdatingFlags: [Bool] = []
        var enterIdleCallCount = 0

        func enterIdleEmpty() {
            enterIdleCallCount += 1
            XCTAssertLessThanOrEqual(enterIdleCallCount, 2, "禁止在 updateSearchResults 同步回调中无限递归重入 enterIdleEmpty")
            guard let splitVC else { return }
            splitVC.searchResultsViewController.emptyState = .idleEmpty
            splitVC.updateSearchResults(rawResults: [], tree: nil, deferTreeBuild: false, autoSelectFirstRow: false)
        }

        func searchMainSplitViewDidSelect(result: FfiSearchResult?) {}
        func searchMainSplitViewDidDoubleClick(result: FfiSearchResult) {}
        func searchMainSplitViewDidFilterResults(filteredCount: Int, totalCount: Int, activeDirectory: String?) {
            guard let splitVC else { return }
            observedUpdatingFlags.append(splitVC.isUpdatingSearchResults)
            if !splitVC.isUpdatingSearchResults && activeDirectory == nil {
                enterIdleEmpty()
            }
        }
    }

    func testSplitViewItemsAndSidebarToggle() {
        let splitVC = SearchMainSplitViewController()
        _ = splitVC.view

        XCTAssertEqual(splitVC.splitViewItems.count, 2)

        let sidebarItem = splitVC.sidebarSplitViewItem
        XCTAssertEqual(sidebarItem.behavior, .sidebar)
        XCTAssertEqual(sidebarItem.minimumThickness, 180)
        XCTAssertEqual(sidebarItem.maximumThickness, 320)
        XCTAssertEqual(sidebarItem.holdingPriority.rawValue, 260)
        XCTAssertTrue(sidebarItem.canCollapse)
        XCTAssertTrue(sidebarItem.isCollapsed)

        let contentItem = splitVC.contentSplitViewItem
        XCTAssertEqual(contentItem.behavior, .contentList)
        XCTAssertEqual(contentItem.minimumThickness, 460)
        XCTAssertEqual(contentItem.holdingPriority.rawValue, 200)
        XCTAssertFalse(contentItem.canCollapse)

        XCTAssertTrue(splitVC.isSidebarCollapsed)
        splitVC.toggleSidebar()
        XCTAssertFalse(splitVC.isSidebarCollapsed)
        XCTAssertFalse(sidebarItem.isCollapsed)

        splitVC.toggleSidebar(nil)
        XCTAssertTrue(splitVC.isSidebarCollapsed)
        XCTAssertTrue(sidebarItem.isCollapsed)

        splitVC.toggleSidebar(NSObject())
        XCTAssertFalse(splitVC.isSidebarCollapsed)
        XCTAssertFalse(sidebarItem.isCollapsed)

        splitVC.toggleSidebar(nil)
        XCTAssertTrue(splitVC.isSidebarCollapsed)
        XCTAssertTrue(sidebarItem.isCollapsed)
    }

    func testCoordinatorFilteringDelegateForwardingAndLazyTreeBuild() {
        let splitVC = SearchMainSplitViewController()
        _ = splitVC.view

        let itemA = TestFixture.searchResult(name: "main.rs", path: "/workspace/src/main.rs", extension: "rs", size: 1024, created: 1000, modified: 2000)
        let itemB = TestFixture.searchResult(name: "lib.rs", path: "/workspace/src/lib.rs", extension: "rs", size: 2048, created: 1000, modified: 2100)
        let itemC = TestFixture.searchResult(name: "readme.md", path: "/workspace/docs/readme.md", extension: "md", size: 512, created: 1000, modified: 2200)
        let testRawResults = [itemA, itemB, itemC]
        let testTree = ResultDirectoryTree.build(from: testRawResults)

        splitVC.updateSearchResults(rawResults: testRawResults, tree: testTree)
        XCTAssertEqual(splitVC.rawResults.count, 3)
        XCTAssertEqual(splitVC.sidebarTreeViewController.currentTree?.totalMatchCount, 3)
        XCTAssertEqual(splitVC.searchResultsViewController.results.count, 3)

        splitVC.sidebarDidSelectDirectory(path: "/workspace/src")
        XCTAssertEqual(splitVC.searchResultsViewController.results.count, 2)
        for res in splitVC.searchResultsViewController.results {
            XCTAssertTrue(res.path.hasPrefix("/workspace/src"))
        }

        splitVC.sidebarDidSelectDirectory(path: nil)
        XCTAssertEqual(splitVC.searchResultsViewController.results.count, 3)

        let mockDelegate = MockSplitDelegate()
        splitVC.delegate = mockDelegate

        splitVC.searchResultsDidSelect(result: itemA)
        XCTAssertEqual(mockDelegate.lastSelectedResult, itemA)
        XCTAssertEqual(mockDelegate.selectCallCount, 1)
        XCTAssertNil(splitVC.sidebarTreeViewController.selectedPath)

        splitVC.searchResultsViewController.selectFirstRow()
        XCTAssertEqual(splitVC.searchResultsViewController.selectedResult, itemA)

        splitVC.searchResultsDidDoubleClick(result: itemA)
        XCTAssertEqual(mockDelegate.lastDoubleClickedResult, itemA)
        XCTAssertEqual(mockDelegate.doubleClickCallCount, 1)

        splitVC.searchResultsDidRequestRebuild()
        XCTAssertEqual(mockDelegate.rebuildCallCount, 1)

        splitVC.searchResultsDidRequestFocusSearchField()
        XCTAssertEqual(mockDelegate.focusSearchFieldCallCount, 1)

        splitVC.searchResultsDidTrashItems(paths: [itemA.path])
        XCTAssertEqual(splitVC.rawResults.count, 2)
        XCTAssertFalse(splitVC.rawResults.contains(itemA))
        XCTAssertTrue(splitVC.rawResults.contains(itemB))
        XCTAssertTrue(splitVC.rawResults.contains(itemC))
        XCTAssertEqual(mockDelegate.trashCallCount, 1)
        XCTAssertEqual(mockDelegate.trashedRemainingCount, 2)
        XCTAssertTrue(mockDelegate.trashedPaths.contains(itemA.path))

        splitVC.updateSearchResults(rawResults: [], tree: nil)
        XCTAssertTrue(splitVC.rawResults.isEmpty)
        XCTAssertNil(splitVC.sidebarTreeViewController.currentTree)
        XCTAssertTrue(splitVC.searchResultsViewController.results.isEmpty)
        splitVC.onLanguageChanged()

        // 侧边栏折叠时的惰性目录树构建、展开补建与自动首行预选
        let lazySplitVC = SearchMainSplitViewController()
        _ = lazySplitVC.view
        let lazyDelegate = MockSplitDelegate()
        lazySplitVC.delegate = lazyDelegate

        XCTAssertTrue(lazySplitVC.isSidebarCollapsed)
        XCTAssertFalse(lazySplitVC.needsDirectoryTree)

        lazySplitVC.updateSearchResults(
            rawResults: testRawResults,
            tree: nil,
            deferTreeBuild: true,
            autoSelectFirstRow: true
        )
        XCTAssertNil(lazySplitVC.sidebarTreeViewController.currentTree)
        XCTAssertEqual(lazySplitVC.searchResultsViewController.selectedResult, itemA)
        XCTAssertEqual(lazyDelegate.lastFilteredCount, 3)

        lazySplitVC.toggleSidebar()
        XCTAssertFalse(lazySplitVC.isSidebarCollapsed)
        XCTAssertEqual(lazySplitVC.sidebarTreeViewController.currentTree?.totalMatchCount, 3)
    }

    func testUpdatingSearchResultsRecursionGuard() {
        let itemA = TestFixture.searchResult(name: "main.rs", path: "/workspace/src/main.rs", extension: "rs", size: 1024, created: 1000, modified: 2000)
        let itemB = TestFixture.searchResult(name: "lib.rs", path: "/workspace/src/lib.rs", extension: "rs", size: 2048, created: 1000, modified: 2100)
        let itemC = TestFixture.searchResult(name: "readme.md", path: "/workspace/docs/readme.md", extension: "md", size: 512, created: 1000, modified: 2200)
        let testRawResults = [itemA, itemB, itemC]
        let testTree = ResultDirectoryTree.build(from: testRawResults)

        let recursionSplitVC = SearchMainSplitViewController()
        _ = recursionSplitVC.view
        let recursionDelegate = IdleEmptyRecursionGuardDelegate()
        recursionDelegate.splitVC = recursionSplitVC
        recursionSplitVC.delegate = recursionDelegate

        recursionDelegate.enterIdleEmpty()
        XCTAssertEqual(recursionDelegate.enterIdleCallCount, 1)
        XCTAssertEqual(recursionDelegate.observedUpdatingFlags, [true])
        XCTAssertEqual(recursionSplitVC.searchResultsViewController.emptyState, .idleEmpty)

        recursionSplitVC.updateSearchResults(rawResults: testRawResults, tree: testTree)
        recursionSplitVC.searchResultsViewController.emptyState = .noMatches
        recursionSplitVC.sidebarDidSelectDirectory(path: "/workspace/src")
        recursionDelegate.enterIdleCallCount = 0
        recursionDelegate.observedUpdatingFlags.removeAll()
        recursionSplitVC.sidebarDidSelectDirectory(path: nil)
        XCTAssertEqual(recursionDelegate.enterIdleCallCount, 1)
        XCTAssertEqual(recursionDelegate.observedUpdatingFlags, [false, true])
        XCTAssertEqual(recursionSplitVC.searchResultsViewController.emptyState, .idleEmpty)
    }
}
