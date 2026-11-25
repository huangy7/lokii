import Cocoa
import Quartz
import UniformTypeIdentifiers
import XCTest
@testable import LokiiAppLib

final class SearchResultsViewControllerTests: XCTestCase {
    private final class MockDelegate: SearchResultsViewControllerDelegate {
        var selectedResult: FfiSearchResult?
        var selectCallCount = 0
        var doubleClickedResult: FfiSearchResult?
        var doubleClickCallCount = 0
        var rebuildCallCount = 0
        var focusSearchFieldCallCount = 0
        var trashedPaths: Set<String> = []
        var trashCallCount = 0

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

        func searchResultsDidRequestFocusSearchField() {
            focusSearchFieldCallCount += 1
        }

        func searchResultsDidTrashItems(paths: Set<String>) {
            trashedPaths = paths
            trashCallCount += 1
        }
    }

    override func setUp() {
        super.setUp()
        UserDefaults.standard.removeObject(forKey: "NSTableView Columns SearchResultsTable")
        UserDefaults.standard.removeObject(forKey: SearchResultsViewController.hiddenColumnsDefaultsKey)
    }

    override func tearDown() {
        UserDefaults.standard.removeObject(forKey: "NSTableView Columns SearchResultsTable")
        UserDefaults.standard.removeObject(forKey: SearchResultsViewController.hiddenColumnsDefaultsKey)
        super.tearDown()
    }

    private func waitMainUntil(timeout: TimeInterval = 2.0, _ predicate: () -> Bool) {
        let deadline = Date().addingTimeInterval(timeout)
        while !predicate() && Date() < deadline {
            CFRunLoopRunInMode(.defaultMode, 0.02, true)
        }
    }

    func testEmptyStatesHierarchyRenderingSortingSelectionAndClipboard() throws {
        let controller = SearchResultsViewController()
        _ = controller.view

        XCTAssertTrue(controller.emptyStateContainer.isHidden)
        controller.displayResults([], preserveSelection: false)
        XCTAssertFalse(controller.emptyStateContainer.isHidden)
        XCTAssertEqual(controller.emptyTitleLabel.stringValue, L("search.empty.title"))
        XCTAssertEqual(controller.emptySubtitleLabel.stringValue, L("search.empty.subtitle"))
        XCTAssertTrue(controller.emptyRuleRow.isHidden)

        controller.emptyState = .excluded(rule: "Contents")
        XCTAssertEqual(controller.emptyTitleLabel.stringValue, L("search.empty.excluded.title"))
        XCTAssertEqual(controller.emptyRuleNameLabel.stringValue, "Contents")
        XCTAssertFalse(controller.emptyRuleRow.isHidden)
        let manageRulesButton = try XCTUnwrap(controller.emptyStateContainer.arrangedSubviews.compactMap { $0 as? NSButton }.first)
        XCTAssertFalse(manageRulesButton.isHidden)
        var manageRulesCallCount = 0
        controller.onManageIndexRules = { manageRulesCallCount += 1 }
        manageRulesButton.sendAction(manageRulesButton.action!, to: manageRulesButton.target)
        XCTAssertEqual(manageRulesCallCount, 1)
        controller.emptyTitleLabel.stringValue = "旧语言标题"
        controller.onLanguageChanged()
        XCTAssertEqual(controller.emptyTitleLabel.stringValue, L("search.empty.excluded.title"))
        XCTAssertEqual(controller.emptyRuleNameLabel.stringValue, "Contents")

        controller.emptyState = .indexing
        XCTAssertEqual(controller.emptyTitleLabel.stringValue, L("search.empty.indexing.title"))
        XCTAssertEqual(controller.emptySubtitleLabel.stringValue, L("search.empty.indexing.subtitle"))
        XCTAssertTrue(controller.emptyRuleRow.isHidden && manageRulesButton.isHidden)
        controller.emptyState = .noMatches

        XCTAssertTrue(controller.view.subviews.contains(controller.tableScrollView))
        XCTAssertTrue(controller.view.subviews.contains(controller.separator))
        XCTAssertTrue(controller.view.subviews.contains(controller.bottomBar))
        XCTAssertFalse(controller.tableScrollView.automaticallyAdjustsContentInsets)
        XCTAssertEqual(controller.tableScrollView.contentInsets.top, 0)
        XCTAssertEqual(controller.tableScrollView.contentInsets.bottom, 0)
        let topConstraint = controller.view.constraints.first {
            $0.firstItem as? NSScrollView == controller.tableScrollView && $0.firstAttribute == .top
        }
        XCTAssertTrue(topConstraint?.secondItem is NSLayoutGuide)
        XCTAssertEqual(controller.tableView.numberOfColumns, 6)
        XCTAssertEqual(
            controller.tableView.tableColumns.filter { !$0.isHidden }.map(\.identifier),
            [.columnName, .columnPath, .columnModified, .columnSize]
        )
        XCTAssertTrue(controller.tableView.autosaveTableColumns)
        XCTAssertEqual(controller.tableView.autosaveName, "SearchResultsTable")
        XCTAssertTrue(controller.tableView.usesAlternatingRowBackgroundColors)
        XCTAssertEqual(controller.tableView.rowHeight, 24)

        XCTAssertTrue(controller.bottomBar.subviews.contains(controller.pathBar))
        XCTAssertTrue(controller.bottomBar.subviews.contains(controller.statusLabel))
        XCTAssertTrue(controller.bottomBar.subviews.contains(controller.inlineProgressIndicator))
        XCTAssertTrue(controller.bottomBar.subviews.contains(controller.rebuildContainer))
        XCTAssertTrue(controller.rebuildContainer.subviews.contains(controller.rebuildButton))
        XCTAssertEqual(controller.bottomBar.constraints.first { $0.firstAttribute == .height }?.constant, 28)

        let sampleItems = [
            TestFixture.searchResult(name: "alpha.rs", path: "/tmp/src/alpha.rs", extension: "rs", size: 2048, created: 100, modified: 300, matchPositions: [FfiMatchRange(start: 0, end: 5)]),
            TestFixture.searchResult(name: "beta.md", path: "/tmp/docs/beta.md", extension: "md", size: 1024, created: 100, modified: 200),
            TestFixture.searchResult(name: "gamma_dir", path: "/tmp/gamma_dir", isDir: true, size: 0, created: 100, modified: 100),
        ]

        controller.displayResults(sampleItems, preserveSelection: false)
        XCTAssertTrue(controller.emptyStateContainer.isHidden)
        XCTAssertEqual(controller.results.count, 3)
        XCTAssertEqual(controller.numberOfRows(in: controller.tableView), 3)

        let nameCol = controller.tableView.tableColumns.first { $0.identifier == .columnName }
        let pathCol = controller.tableView.tableColumns.first { $0.identifier == .columnPath }
        let sizeCol = controller.tableView.tableColumns.first { $0.identifier == .columnSize }

        let nameCellView = controller.tableView(controller.tableView, viewFor: nameCol, row: 0)
        XCTAssertTrue(nameCellView is SearchNameTableCellView)
        XCTAssertEqual(nameCellView?.toolTip, "alpha.rs")
        XCTAssertEqual((nameCellView as? SearchNameTableCellView)?.textField?.toolTip, "alpha.rs")

        let nameCellViewNoMatch = controller.tableView(controller.tableView, viewFor: nameCol, row: 1) as? SearchNameTableCellView
        XCTAssertNotNil(nameCellViewNoMatch)
        XCTAssertEqual(nameCellViewNoMatch?.textField?.attributedStringValue.string, "beta.md")
        XCTAssertEqual(nameCellViewNoMatch?.toolTip, "beta.md")

        let pathCellView = controller.tableView(controller.tableView, viewFor: pathCol, row: 0) as? NSTableCellView
        XCTAssertEqual(pathCellView?.textField?.stringValue, "/tmp/src")
        XCTAssertEqual(pathCellView?.toolTip, "/tmp/src/alpha.rs")
        XCTAssertEqual(pathCellView?.textField?.toolTip, "/tmp/src/alpha.rs")

        let sizeCellView = controller.tableView(controller.tableView, viewFor: sizeCol, row: 2) as? NSTableCellView
        XCTAssertEqual(sizeCellView?.textField?.stringValue, "--")

        // 排序联动
        controller.tableView.sortDescriptors = [NSSortDescriptor(key: "name", ascending: false)]
        controller.tableView(controller.tableView, sortDescriptorsDidChange: [])
        XCTAssertEqual(controller.results[0].name, "gamma_dir")
        XCTAssertEqual(controller.results[2].name, "alpha.rs")

        controller.tableView.sortDescriptors = [NSSortDescriptor(key: "size", ascending: false)]
        controller.tableView(controller.tableView, sortDescriptorsDidChange: [])
        XCTAssertEqual(controller.results[0].name, "alpha.rs")
        XCTAssertEqual(controller.results[2].name, "gamma_dir")

        // 保留选区智能恢复
        controller.selectFirstRow()
        let selectedItem = controller.results[1]
        let targetIndex = try XCTUnwrap(controller.results.firstIndex(of: selectedItem))
        controller.tableView.selectRowIndexes(IndexSet(integer: targetIndex), byExtendingSelection: false)
        XCTAssertEqual(controller.selectedResult?.path, selectedItem.path)

        controller.displayResults(Array(sampleItems.reversed()), preserveSelection: true)
        XCTAssertEqual(controller.selectedResult?.path, selectedItem.path)

        controller.displayResults([sampleItems[0]], preserveSelection: true)
        XCTAssertNil(controller.selectedResult)

        // 代理与剪贴板/右键菜单
        let mockDelegate = MockDelegate()
        controller.delegate = mockDelegate
        controller.displayResults(sampleItems, preserveSelection: false)
        controller.selectFirstRow()
        XCTAssertEqual(controller.selectedResult?.name, "alpha.rs")
        XCTAssertEqual(mockDelegate.selectedResult?.name, "alpha.rs")

        controller.openSelected()
        XCTAssertEqual(mockDelegate.doubleClickedResult?.name, "alpha.rs")

        controller.copyFile()
        let copiedUrls = NSPasteboard.general.readObjects(forClasses: [NSURL.self], options: nil) as? [URL]
        XCTAssertEqual(copiedUrls?.first?.path, "/tmp/src/alpha.rs")
        XCTAssertEqual(NSPasteboard.general.string(forType: .string), "/tmp/src/alpha.rs")

        controller.copyPath()
        XCTAssertEqual(NSPasteboard.general.string(forType: .string), "/tmp/src/alpha.rs")

        controller.copyName()
        XCTAssertEqual(NSPasteboard.general.string(forType: .string), "alpha.rs")

        let menu = try XCTUnwrap(controller.tableView.menu)
        XCTAssertEqual(menu.items.count, 15)
        XCTAssertEqual(menu.items[0].action, #selector(controller.openSelected))
        XCTAssertTrue(menu.items[1].hasSubmenu)
        XCTAssertNotNil(menu.items[1].submenu?.delegate)
        XCTAssertEqual(menu.items[2].action, #selector(controller.quickLookSelected))
        XCTAssertEqual(menu.items[2].keyEquivalent, " ")
        XCTAssertTrue(menu.items[2].keyEquivalentModifierMask.isEmpty)
        XCTAssertEqual(menu.items[3].action, #selector(controller.revealSelected))
        XCTAssertEqual(menu.items[4].action, #selector(controller.filterByDirectorySelected))
        XCTAssertEqual(menu.items[5].action, #selector(controller.openInTerminalSelected))
        XCTAssertEqual(menu.items[6].action, #selector(controller.showInfoSelected))
        XCTAssertEqual(menu.items[6].keyEquivalent, "i")
        XCTAssertTrue(menu.items[7].isSeparatorItem)
        XCTAssertEqual(menu.items[8].action, #selector(controller.copyFile))
        XCTAssertEqual(menu.items[8].keyEquivalent, "c")
        XCTAssertEqual(menu.items[9].action, #selector(controller.copyPath))
        XCTAssertEqual(menu.items[10].action, #selector(controller.copyName))
        XCTAssertTrue(menu.items[11].isSeparatorItem)
        XCTAssertEqual(menu.items[12].action, #selector(controller.excludeDirectorySelected))
        XCTAssertTrue(menu.items[13].isSeparatorItem)
        XCTAssertEqual(menu.items[14].action, #selector(controller.moveToTrashSelected))
        XCTAssertEqual(menu.items[14].keyEquivalent, String(format: "%c", NSDeleteCharacter))
        XCTAssertEqual(menu.items[14].keyEquivalentModifierMask, [.command])
        XCTAssertTrue(menu.allowsContextMenuPlugIns)

        for item in menu.items where !item.isSeparatorItem && !item.hasSubmenu {
            XCTAssertTrue(controller.validateMenuItem(item))
        }

        // Terminal & Reveal 单选/多选 + 路径栏点击
        var interceptedTerminalURLs: [URL] = []
        controller.openInTerminalHandler = { interceptedTerminalURLs = $0 }
        controller.selectFirstRow()
        controller.openInTerminalSelected()
        XCTAssertEqual(interceptedTerminalURLs.map(\.path), ["/tmp/src/alpha.rs"])

        controller.tableView.selectRowIndexes(IndexSet([0, 1]), byExtendingSelection: false)
        controller.openInTerminalSelected()
        XCTAssertEqual(interceptedTerminalURLs.map(\.path), ["/tmp/src/alpha.rs", "/tmp/docs/beta.md"])

        var interceptedRevealURLs: [URL] = []
        controller.revealFilesHandler = { interceptedRevealURLs = $0 }
        controller.selectFirstRow()
        controller.revealSelected()
        XCTAssertEqual(interceptedRevealURLs.map(\.path), ["/tmp/src/alpha.rs"])

        controller.tableView.selectRowIndexes(IndexSet([0, 1]), byExtendingSelection: false)
        controller.revealSelected()
        XCTAssertEqual(interceptedRevealURLs.map(\.path), ["/tmp/src/alpha.rs", "/tmp/docs/beta.md"])

        controller.selectFirstRow()
        controller.updateBottomPath(with: "/tmp/src/alpha.rs")
        interceptedRevealURLs.removeAll()
        if let pathBarAction = controller.pathBar.action {
            _ = controller.pathBar.target?.perform(pathBarAction, with: controller.pathBar)
        }
        XCTAssertEqual(interceptedRevealURLs.map(\.path), ["/tmp/src/alpha.rs"])

        // ⌘⌫ 等效与 moveToTrashSelected + 自愈
        let deleteEvent = NSEvent.keyEvent(
            with: .keyDown,
            location: .zero,
            modifierFlags: [.command],
            timestamp: 0,
            windowNumber: 0,
            context: nil,
            characters: "\u{7f}",
            charactersIgnoringModifiers: "\u{7f}",
            isARepeat: false,
            keyCode: 51
        )!
        XCTAssertTrue(controller.tableView.performKeyEquivalent(with: deleteEvent))

        var interceptedTrashURLs: [URL] = []
        controller.trashItemHandler = { url in
            interceptedTrashURLs.append(url)
            return url
        }
        controller.confirmTrashHandler = { _, completion in completion(true) }

        let initialCount = controller.results.count
        controller.selectFirstRow()
        controller.moveToTrashSelected()
        waitMainUntil { mockDelegate.trashCallCount == 1 }
        XCTAssertEqual(mockDelegate.trashCallCount, 1)
        XCTAssertTrue(mockDelegate.trashedPaths.contains("/tmp/src/alpha.rs"))
        XCTAssertEqual(interceptedTrashURLs.first?.path, "/tmp/src/alpha.rs")
        XCTAssertEqual(controller.results.count, initialCount - 1)
        XCTAssertEqual(controller.selectedResult?.name, "beta.md")

        let countBeforeStale = controller.results.count
        controller.trashItemHandler = { _ in
            throw NSError(domain: NSCocoaErrorDomain, code: NSFileNoSuchFileError, userInfo: nil)
        }
        controller.moveToTrashSelected()
        waitMainUntil { mockDelegate.trashCallCount == 2 }
        XCTAssertEqual(mockDelegate.trashCallCount, 2)
        XCTAssertEqual(controller.results.count, countBeforeStale - 1)
    }

    func testQuickLookStatusBarKeyboardNavDensityFDAAndColumnCustomization() throws {
        let controller = SearchResultsViewController()
        _ = controller.view

        let sampleItems = [
            TestFixture.searchResult(name: "alpha.rs", path: "/tmp/src/alpha.rs", extension: "rs", size: 2048, created: 100, modified: 300),
            TestFixture.searchResult(name: "beta.md", path: "/tmp/docs/beta.md", extension: "md", size: 1024, created: 100, modified: 200),
            TestFixture.searchResult(name: "gamma_dir", path: "/tmp/gamma_dir", isDir: true, size: 0, created: 100, modified: 100),
        ]
        let menu = try XCTUnwrap(controller.tableView.menu)

        controller.displayResults([], preserveSelection: false)
        XCTAssertFalse(controller.acceptsPreviewPanelControl(nil))
        XCTAssertEqual(controller.numberOfPreviewItems(in: nil), 0)
        for item in menu.items where !item.isSeparatorItem {
            XCTAssertFalse(controller.validateMenuItem(item))
        }

        controller.displayResults(sampleItems, preserveSelection: false)
        controller.selectFirstRow()
        XCTAssertTrue(controller.acceptsPreviewPanelControl(nil))
        XCTAssertEqual(controller.numberOfPreviewItems(in: nil), 1)
        let previewItem = controller.previewPanel(nil, previewItemAt: 0)
        XCTAssertEqual((previewItem as? NSURL)?.path, "/tmp/src/alpha.rs")
        XCTAssertEqual(controller.previewPanel(nil, sourceFrameOnScreenFor: previewItem), NSRect.zero)

        // 底栏状态与临时气泡
        controller.updateBottomPath(with: "/tmp/src/alpha.rs")
        XCTAssertEqual(controller.pathBar.url?.path, "/tmp/src/alpha.rs")
        XCTAssertEqual(controller.pathBar.toolTip, "/tmp/src/alpha.rs")
        controller.updateBottomPath(with: nil)
        XCTAssertNil(controller.pathBar.url)
        XCTAssertNil(controller.pathBar.toolTip)

        controller.updateStatusText("已匹配 10 项")
        XCTAssertEqual(controller.statusLabel.stringValue, "已匹配 10 项")
        controller.updateStatusText("已索引 100 个文件")
        XCTAssertEqual(controller.statusLabel.stringValue, "已索引 100 个文件")

        controller.notifyBackgroundIndexingActive()
        XCTAssertEqual(controller.statusLabel.stringValue, L("search.backgroundRefresh"))
        XCTAssertEqual(controller.statusLabel.textColor, NSColor.controlAccentColor)
        XCTAssertFalse(controller.statusFeedbackCapsule.isHidden)
        XCTAssertTrue(controller.statusFeedbackCapsule.isSpinning)
        XCTAssertEqual(controller.statusFeedbackCapsule.label.stringValue, L("search.backgroundRefresh"))

        controller.updateStatusText("已索引 200 个文件")
        XCTAssertEqual(controller.statusLabel.stringValue, L("search.backgroundRefresh"))

        controller.cancelTemporaryStatus()
        XCTAssertEqual(controller.statusLabel.stringValue, "已索引 200 个文件")
        XCTAssertEqual(controller.statusLabel.textColor, NSColor.secondaryLabelColor)
        XCTAssertTrue(controller.statusFeedbackCapsule.isHidden)
        XCTAssertFalse(controller.statusFeedbackCapsule.isSpinning)

        controller.setInlineProgressVisible(true)
        XCTAssertFalse(controller.inlineProgressIndicator.isHidden)
        controller.setInlineProgressVisible(false)
        XCTAssertTrue(controller.inlineProgressIndicator.isHidden)

        controller.setRebuildEnabled(true)
        XCTAssertTrue(controller.rebuildButton.isEnabled)
        controller.setRebuildSpinning(true)
        XCTAssertNotNil(controller.rebuildButton.layer?.animation(forKey: "spin"))
        controller.setRebuildSpinning(false)
        XCTAssertNil(controller.rebuildButton.layer?.animation(forKey: "spin"))

        let mockRebuildDelegate = MockDelegate()
        controller.delegate = mockRebuildDelegate
        controller.rebuildButton.sendAction(controller.rebuildButton.action!, to: controller.rebuildButton.target)
        XCTAssertEqual(mockRebuildDelegate.rebuildCallCount, 1)

        // 键盘导航与首行上移回调
        let navDelegate = MockDelegate()
        controller.delegate = navDelegate
        controller.displayResults([], preserveSelection: false)
        XCTAssertFalse(controller.focusResults())

        controller.displayResults(sampleItems, preserveSelection: false)
        XCTAssertTrue(controller.focusResults(row: 1))
        XCTAssertEqual(controller.tableView.selectedRow, 1)

        let upArrowEvent = NSEvent.keyEvent(
            with: .keyDown, location: .zero, modifierFlags: [], timestamp: 0, windowNumber: 0,
            context: nil, characters: "\u{F700}", charactersIgnoringModifiers: "\u{F700}", isARepeat: false, keyCode: 126
        )!
        controller.tableView.keyDown(with: upArrowEvent)
        XCTAssertEqual(controller.tableView.selectedRow, 0)
        XCTAssertEqual(navDelegate.focusSearchFieldCallCount, 0)

        controller.tableView.keyDown(with: upArrowEvent)
        XCTAssertEqual(controller.tableView.selectedRow, 0)
        XCTAssertEqual(navDelegate.focusSearchFieldCallCount, 1)

        let cmdUpArrowEvent = NSEvent.keyEvent(
            with: .keyDown, location: .zero, modifierFlags: [.command], timestamp: 0, windowNumber: 0,
            context: nil, characters: "\u{F700}", charactersIgnoringModifiers: "\u{F700}", isARepeat: false, keyCode: 126
        )!
        controller.tableView.keyDown(with: cmdUpArrowEvent)
        XCTAssertEqual(navDelegate.focusSearchFieldCallCount, 1)

        let shiftUpArrowEvent = NSEvent.keyEvent(
            with: .keyDown, location: .zero, modifierFlags: [.shift], timestamp: 0, windowNumber: 0,
            context: nil, characters: "\u{F700}", charactersIgnoringModifiers: "\u{F700}", isARepeat: false, keyCode: 126
        )!
        controller.tableView.keyDown(with: shiftUpArrowEvent)
        XCTAssertEqual(navDelegate.focusSearchFieldCallCount, 1)

        // 密度模型
        controller.applyFontSize(CGFloat(TableDensity.compact.rawValue))
        XCTAssertEqual(controller.currentFontSize, 11)
        XCTAssertEqual(controller.tableView.rowHeight, 21)
        XCTAssertEqual(controller.currentIconSize, 14)

        controller.applyFontSize(CGFloat(TableDensity.comfortable.rawValue))
        XCTAssertEqual(controller.currentFontSize, 15)
        XCTAssertEqual(controller.tableView.rowHeight, 27)
        XCTAssertEqual(controller.currentIconSize, 18)

        controller.applyFontSize(CGFloat(TableDensity.large.rawValue))
        XCTAssertEqual(controller.currentFontSize, 17)
        XCTAssertEqual(controller.tableView.rowHeight, 30)
        XCTAssertEqual(controller.currentIconSize, 20)

        controller.applyFontSize(CGFloat(TableDensity.standard.rawValue))
        XCTAssertEqual(controller.currentFontSize, 13)
        XCTAssertEqual(controller.tableView.rowHeight, 24)
        XCTAssertEqual(controller.currentIconSize, 16)

        // FDA 与路径栏互斥
        controller.updateFDAStatus(hasFDA: false)
        controller.updateBottomPath(with: nil)
        XCTAssertFalse(controller.fdaHintButton.isHidden)
        XCTAssertTrue(controller.pathBar.isHidden)

        controller.updateBottomPath(with: "/System/Library/CoreServices/Finder.app")
        XCTAssertFalse(controller.pathBar.isHidden)
        XCTAssertTrue(controller.fdaHintButton.isHidden)

        controller.updateBottomPath(with: nil)
        XCTAssertFalse(controller.fdaHintButton.isHidden)
        XCTAssertTrue(controller.pathBar.isHidden)

        controller.updateFDAStatus(hasFDA: true)
        XCTAssertTrue(controller.fdaHintButton.isHidden)
        XCTAssertFalse(controller.pathBar.isHidden)

        var fdaGuideTriggered = false
        controller.onOpenFDAGuide = { fdaGuideTriggered = true }
        if let action = controller.fdaHintButton.action {
            _ = controller.fdaHintButton.target?.perform(action, with: controller.fdaHintButton)
        }
        XCTAssertTrue(fdaGuideTriggered)

        // 列拖拽重排与末列对齐
        let modifiedCol = try XCTUnwrap(controller.tableView.tableColumns.first { $0.identifier == .columnModified })
        let testSizeCol = try XCTUnwrap(controller.tableView.tableColumns.first { $0.identifier == .columnSize })
        let testPathCol = try XCTUnwrap(controller.tableView.tableColumns.first { $0.identifier == .columnPath })

        XCTAssertEqual(controller.tableView.tableColumns.last(where: { !$0.isHidden })?.identifier, .columnSize)
        XCTAssertEqual(testSizeCol.headerCell.alignment, .right)
        XCTAssertEqual((controller.tableView(controller.tableView, viewFor: testSizeCol, row: 0) as? NSTableCellView)?.textField?.alignment, .right)
        XCTAssertEqual(modifiedCol.headerCell.alignment, .left)
        XCTAssertEqual((controller.tableView(controller.tableView, viewFor: modifiedCol, row: 0) as? NSTableCellView)?.textField?.alignment, .left)

        let oldSizeIndex = controller.tableView.column(withIdentifier: .columnSize)
        controller.tableView.moveColumn(oldSizeIndex, toColumn: 1)
        controller.tableViewColumnDidMove(Notification(
            name: NSTableView.columnDidMoveNotification,
            object: controller.tableView,
            userInfo: ["NSOldColumn": oldSizeIndex, "NSNewColumn": 1]
        ))
        XCTAssertEqual(controller.tableView.tableColumns.last(where: { !$0.isHidden })?.identifier, .columnModified)
        XCTAssertEqual(testSizeCol.headerCell.alignment, .left)
        XCTAssertEqual((controller.tableView(controller.tableView, viewFor: testSizeCol, row: 0) as? NSTableCellView)?.textField?.alignment, .left)
        XCTAssertEqual(modifiedCol.headerCell.alignment, .right)
        XCTAssertEqual((controller.tableView(controller.tableView, viewFor: modifiedCol, row: 0) as? NSTableCellView)?.textField?.alignment, .right)

        let oldPathIndex = controller.tableView.column(withIdentifier: .columnPath)
        let targetLastIndex = controller.tableView.tableColumns.count - 1
        controller.tableView.moveColumn(oldPathIndex, toColumn: targetLastIndex)
        controller.tableViewColumnDidMove(Notification(
            name: NSTableView.columnDidMoveNotification,
            object: controller.tableView,
            userInfo: ["NSOldColumn": oldPathIndex, "NSNewColumn": targetLastIndex]
        ))
        XCTAssertEqual(controller.tableView.tableColumns.last(where: { !$0.isHidden })?.identifier, .columnPath)
        XCTAssertEqual(testPathCol.headerCell.alignment, .left)
        XCTAssertEqual((controller.tableView(controller.tableView, viewFor: testPathCol, row: 0) as? NSTableCellView)?.textField?.alignment, .left)
        XCTAssertEqual(modifiedCol.headerCell.alignment, .left)

        // 自定义列显隐、持久化与创建日期/扩展名列渲染排序
        controller.resetColumnsToDefault()
        XCTAssertEqual(
            controller.tableView.tableColumns.filter { !$0.isHidden }.map(\.identifier),
            [.columnName, .columnPath, .columnModified, .columnSize]
        )

        controller.setColumnHidden(true, for: .columnName)
        XCTAssertEqual(controller.tableView.tableColumns.first { $0.identifier == .columnName }?.isHidden, false)

        controller.tableView.sortDescriptors = [NSSortDescriptor(key: "size", ascending: false)]
        controller.setColumnHidden(false, for: .columnCreated)
        controller.setColumnHidden(false, for: .columnExtension)
        controller.setColumnHidden(true, for: .columnSize)
        XCTAssertTrue(controller.tableView.sortDescriptors.isEmpty)
        let savedHidden = Set(UserDefaults.standard.stringArray(forKey: SearchResultsViewController.hiddenColumnsDefaultsKey) ?? [])
        XCTAssertEqual(savedHidden, Set([NSUserInterfaceItemIdentifier.columnSize.rawValue]))

        controller.setColumnHidden(true, for: .columnExtension)
        let createdCol = try XCTUnwrap(controller.tableView.tableColumns.first { $0.identifier == .columnCreated })
        let extensionCol = try XCTUnwrap(controller.tableView.tableColumns.first { $0.identifier == .columnExtension })
        XCTAssertEqual(createdCol.headerCell.alignment, .right)

        controller.setColumnHidden(false, for: .columnExtension)
        let colItems = [
            TestFixture.searchResult(name: "folder_a", path: "/tmp/folder_a", isDir: true, size: 0, created: 0, modified: 100),
            TestFixture.searchResult(name: "report.pdf", path: "/tmp/report.pdf", extension: "pdf", size: 100, created: 1_700_000_200, modified: 200),
            TestFixture.searchResult(name: "README", path: "/tmp/README", extension: "", size: 50, created: 1_700_000_100, modified: 150),
            TestFixture.searchResult(name: "app.Swift", path: "/tmp/app.Swift", extension: "Swift", size: 300, created: 1_700_000_300, modified: 300),
        ]
        controller.displayResults(colItems, preserveSelection: false)
        XCTAssertEqual((controller.tableView(controller.tableView, viewFor: createdCol, row: 0) as? NSTableCellView)?.textField?.stringValue, "--")
        let validCreatedText = (controller.tableView(controller.tableView, viewFor: createdCol, row: 1) as? NSTableCellView)?.textField?.stringValue ?? ""
        XCTAssertTrue(validCreatedText != "--" && !validCreatedText.isEmpty)
        XCTAssertEqual((controller.tableView(controller.tableView, viewFor: extensionCol, row: 0) as? NSTableCellView)?.textField?.stringValue, "--")
        XCTAssertEqual((controller.tableView(controller.tableView, viewFor: extensionCol, row: 1) as? NSTableCellView)?.textField?.stringValue, "pdf")

        controller.tableView.sortDescriptors = [NSSortDescriptor(key: "created", ascending: true)]
        controller.tableView(controller.tableView, sortDescriptorsDidChange: [])
        XCTAssertEqual(controller.results.map(\.created), [0, 1_700_000_100, 1_700_000_200, 1_700_000_300])

        controller.tableView.sortDescriptors = [NSSortDescriptor(key: "extension", ascending: true)]
        controller.tableView(controller.tableView, sortDescriptorsDidChange: [])
        XCTAssertEqual(Array(controller.results.prefix(2).map(\.name)), ["report.pdf", "app.Swift"])

        controller.tableView.sortDescriptors = [NSSortDescriptor(key: "extension", ascending: false)]
        controller.tableView(controller.tableView, sortDescriptorsDidChange: [])
        XCTAssertEqual(Array(controller.results.prefix(2).map(\.name)), ["app.Swift", "report.pdf"])

        XCTAssertTrue(controller.savedHeaderView === controller.tableView.headerView)
        let headerMenu = try XCTUnwrap(controller.tableView.headerView?.menu)
        headerMenu.delegate?.menuNeedsUpdate?(headerMenu)
        XCTAssertEqual(headerMenu.items.count, 8)
        XCTAssertEqual(headerMenu.items[0].state, .on)
        XCTAssertFalse(headerMenu.items[0].isEnabled)
    }

    func testOfflineBlockingDynamicDegradationAndIdleEmpty() throws {
        let controller = SearchResultsViewController()
        _ = controller.view

        let testNameCol = try XCTUnwrap(controller.tableView.tableColumns.first { $0.identifier == .columnName })
        let testPathCol = try XCTUnwrap(controller.tableView.tableColumns.first { $0.identifier == .columnPath })

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

        let offlineItem = TestFixture.searchResult(
            name: "budget.xlsx",
            path: "/Volumes/OfflineNAS/finance/budget.xlsx",
            extension: "xlsx",
            size: 4096,
            created: 1000,
            modified: 2000,
            isOffline: true,
            volumeName: "OfflineNAS"
        )
        let onlineItem = TestFixture.searchResult(
            name: "notes.txt",
            path: "/tmp/local_notes.txt",
            extension: "txt",
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
        XCTAssertFalse(workspaceIconCalls.contains(offlineItem.path))
        XCTAssertLessThan(offlineNameCell?.textField?.alphaValue ?? 1.0, 0.9)
        XCTAssertTrue(offlinePathCell?.textField?.stringValue.contains(L("search.result.offline")) == true)

        let onlineNameCell = controller.tableView(controller.tableView, viewFor: testNameCol, row: 1) as? SearchNameTableCellView
        let onlinePathCell = controller.tableView(controller.tableView, viewFor: testPathCol, row: 1) as? SearchPathTableCellView
        XCTAssertEqual(onlineNameCell?.textField?.alphaValue, 1.0)
        XCTAssertFalse(onlinePathCell?.textField?.stringValue.contains(L("search.result.offline")) == true)

        XCTAssertNil(controller.tableView(controller.tableView, pasteboardWriterForRow: 0))
        XCTAssertNotNil(controller.tableView(controller.tableView, pasteboardWriterForRow: 1))

        controller.tableView.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
        XCTAssertFalse(controller.acceptsPreviewPanelControl(QLPreviewPanel.shared()))
        XCTAssertEqual(controller.numberOfPreviewItems(in: QLPreviewPanel.shared()), 0)

        controller.openSelected()
        controller.revealSelected()
        controller.openInTerminalSelected()
        controller.quickLookSelected()
        controller.showInfoSelected()
        controller.copyFile()
        controller.moveToTrashSelected()

        XCTAssertEqual(offlineDelegate.doubleClickCallCount, 0)
        XCTAssertEqual(offlineRevealCalls, 0)
        XCTAssertEqual(offlineTerminalCalls, 0)
        XCTAssertEqual(offlineTrashCalls, 0)
        XCTAssertEqual(controller.statusLabel.stringValue, L("search.offline.actionBlocked"))

        XCTAssertFalse(controller.validateMenuItem(NSMenuItem(title: "Open", action: #selector(SearchResultsViewController.openSelected(_:)), keyEquivalent: "")))
        XCTAssertFalse(controller.validateMenuItem(NSMenuItem(title: "Reveal", action: #selector(SearchResultsViewController.revealSelected(_:)), keyEquivalent: "")))
        XCTAssertFalse(controller.validateMenuItem(NSMenuItem(title: "Copy File", action: #selector(SearchResultsViewController.copyFile(_:)), keyEquivalent: "")))
        XCTAssertFalse(controller.validateMenuItem(NSMenuItem(title: "Trash", action: #selector(SearchResultsViewController.moveToTrashSelected(_:)), keyEquivalent: "")))
        XCTAssertTrue(controller.validateMenuItem(NSMenuItem(title: "Copy Path", action: #selector(SearchResultsViewController.copyPath(_:)), keyEquivalent: "")))
        XCTAssertTrue(controller.validateMenuItem(NSMenuItem(title: "Copy Name", action: #selector(SearchResultsViewController.copyName(_:)), keyEquivalent: "")))

        let offlineBreadcrumbReveal = NSMenuItem(title: "Reveal", action: NSSelectorFromString("breadcrumbRevealInFinder:"), keyEquivalent: "")
        offlineBreadcrumbReveal.representedObject = URL(fileURLWithPath: "/Volumes/OfflineNAS/finance")
        XCTAssertFalse(controller.validateMenuItem(offlineBreadcrumbReveal))

        XCTAssertNil(controller.validRequestor(forSendType: NSPasteboard.PasteboardType("NSFilenamesPboardType"), returnType: nil))

        controller.copyPath()
        XCTAssertEqual(NSPasteboard.general.string(forType: .string), offlineItem.path)
        controller.copyName()
        XCTAssertEqual(NSPasteboard.general.string(forType: .string), offlineItem.name)
        controller.cancelTemporaryStatus()
        XCTAssertEqual(controller.statusLabel.stringValue, "2 items")

        controller.updateBottomPath(with: offlineItem.path)
        XCTAssertTrue(controller.pathBar.isOfflinePathMode)
        XCTAssertNil(controller.pathBar.url)

        let rootBreadcrumbReveal = NSMenuItem(title: "Reveal Root", action: NSSelectorFromString("breadcrumbRevealInFinder:"), keyEquivalent: "")
        rootBreadcrumbReveal.representedObject = URL(fileURLWithPath: "/")
        XCTAssertFalse(controller.validateMenuItem(rootBreadcrumbReveal))
        XCTAssertFalse(controller.isPathOffline("/"))
        XCTAssertFalse(controller.isPathOffline("/Volumes"))

        controller.tableView.selectRowIndexes(IndexSet(integer: 1), byExtendingSelection: false)
        controller.updateBottomPath(with: onlineItem.path)
        XCTAssertFalse(controller.pathBar.isOfflinePathMode)
        XCTAssertTrue(controller.validateMenuItem(rootBreadcrumbReveal))

        var excludedPaths: [String] = []
        controller.onExcludeDirectory = { excludedPaths.append($0) }
        let offlineDirItem = TestFixture.searchResult(name: "docs", path: "/Volumes/OfflineNAS/docs", isDir: true, created: 1000, modified: 2000, isOffline: true, volumeName: "OfflineNAS")
        let offlineFileInRoot = TestFixture.searchResult(name: "readme.md", path: "/Volumes/OfflineNAS/readme.md", extension: "md", size: 512, created: 1000, modified: 2000, isOffline: true, volumeName: "OfflineNAS")
        controller.displayResults([offlineDirItem, offlineFileInRoot, onlineItem], preserveSelection: false)
        controller.tableView.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
        controller.excludeDirectorySelected()
        XCTAssertEqual(excludedPaths.last, "/Volumes/OfflineNAS/docs")

        controller.tableView.selectRowIndexes(IndexSet(integer: 1), byExtendingSelection: false)
        controller.excludeDirectorySelected()
        XCTAssertEqual(excludedPaths.last, "/Volumes/OfflineNAS")

        // 混合选区全链路拦截
        controller.displayResults([offlineItem, onlineItem], preserveSelection: false)
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

        XCTAssertEqual(offlineDelegate.doubleClickCallCount, 0)
        XCTAssertEqual(offlineRevealCalls, 0)
        XCTAssertEqual(offlineTerminalCalls, 0)
        XCTAssertEqual(offlineTrashCalls, 0)
        XCTAssertEqual(controller.statusLabel.stringValue, L("search.offline.actionBlocked"))
        XCTAssertFalse(controller.acceptsPreviewPanelControl(QLPreviewPanel.shared()))
        XCTAssertEqual(controller.numberOfPreviewItems(in: QLPreviewPanel.shared()), 0)
        XCTAssertFalse(controller.validateMenuItem(NSMenuItem(title: "Open", action: #selector(SearchResultsViewController.openSelected(_:)), keyEquivalent: "")))
        XCTAssertFalse(controller.validateMenuItem(NSMenuItem(title: "Terminal", action: #selector(SearchResultsViewController.openInTerminalSelected(_:)), keyEquivalent: "")))
        XCTAssertFalse(controller.validateMenuItem(NSMenuItem(title: "Open With", action: #selector(SearchResultsViewController.openWithApp(_:)), keyEquivalent: "")))
        controller.cancelTemporaryStatus()

        // 动态离线降级与即时重绘
        workspaceIconCalls.removeAll()
        var dynamicOfflineEnabled = false
        controller.isPathOfflineHandler = { path in
            dynamicOfflineEnabled && path.hasPrefix("/Volumes/DisconnectedDisk")
        }
        let dynamicallyOffline = TestFixture.searchResult(name: "archive.tar", path: "/Volumes/DisconnectedDisk/archive.tar", extension: "tar", size: 512, created: 1000, modified: 2000)
        controller.displayResults([dynamicallyOffline], preserveSelection: false)
        controller.tableView.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
        XCTAssertNotNil(controller.tableView(controller.tableView, pasteboardWriterForRow: 0))

        dynamicOfflineEnabled = true
        controller.refreshOfflineStates()
        let refreshedPathCell = controller.tableView(controller.tableView, viewFor: testPathCol, row: 0) as? SearchPathTableCellView
        XCTAssertTrue(refreshedPathCell?.textField?.stringValue.contains(L("search.result.offline")) == true)
        XCTAssertFalse(workspaceIconCalls.contains(dynamicallyOffline.path))
        XCTAssertNil(controller.tableView(controller.tableView, pasteboardWriterForRow: 0))
        XCTAssertTrue(controller.pathBar.isOfflinePathMode)

        // 自动预选首行、纯内存图标、O(1) 底栏更新、文本编辑态 ⌘⌫ 保护
        workspaceIconCalls.removeAll()
        controller.isPathOfflineHandler = nil
        let autoSelectDelegate = MockDelegate()
        controller.delegate = autoSelectDelegate
        let localCodeItem = TestFixture.searchResult(name: "engine.swift", path: "/tmp/project/engine.swift", extension: "swift", size: 2048, created: 1000, modified: 2000)
        let localDocItem = TestFixture.searchResult(name: "guide.md", path: "/tmp/project/guide.md", extension: "md", size: 512, created: 1000, modified: 1900)
        controller.displayResults([localCodeItem, localDocItem], preserveSelection: false, autoSelectFirstRow: true)
        XCTAssertEqual(controller.tableView.selectedRow, 0)
        XCTAssertEqual(autoSelectDelegate.selectedResult?.path, localCodeItem.path)

        _ = controller.tableView(controller.tableView, viewFor: testNameCol, row: 0)
        XCTAssertFalse(workspaceIconCalls.contains(localCodeItem.path))

        controller.updateBottomPath(for: localCodeItem)
        XCTAssertEqual(controller.pathBar.url?.path, localCodeItem.path)
        XCTAssertEqual(controller.pathBar.alphaValue, 1.0)
        controller.updateBottomPath(for: offlineItem)
        XCTAssertTrue(controller.pathBar.isOfflinePathMode)
        XCTAssertNil(controller.pathBar.url)
        XCTAssertLessThan(controller.pathBar.alphaValue, 1.0)
        XCTAssertEqual(controller.pathBar.pathItems.map(\.title), ["OfflineNAS", "finance", "budget.xlsx"])
        XCTAssertEqual(
            controller.pathBar.offlineComponentURLs.map(\.path),
            ["/Volumes/OfflineNAS", "/Volumes/OfflineNAS/finance", "/Volumes/OfflineNAS/finance/budget.xlsx"]
        )
        XCTAssertTrue(controller.pathBar.pathItems.allSatisfy { ($0.image?.isTemplate == false) && (($0.image?.size.width ?? 0) > 0) })
        controller.updateBottomPath(for: localCodeItem)
        XCTAssertEqual(controller.pathBar.alphaValue, 1.0)

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
        let deleteEvent = NSEvent.keyEvent(
            with: .keyDown, location: .zero, modifierFlags: [.command], timestamp: 0, windowNumber: 0,
            context: nil, characters: "\u{7f}", charactersIgnoringModifiers: "\u{7f}", isARepeat: false, keyCode: 51
        )!
        XCTAssertFalse(controller.tableView.performKeyEquivalent(with: deleteEvent))

        // idleEmpty 判定、语法速查卡片、最近搜索胶囊与无结果行动卡片
        XCTAssertTrue(SearchResultsViewController.shouldShowIdleEmptyState(hideWhenEmpty: true, query: "   ", isFilterUnconstrained: true, hasActiveDirectory: false, isIndexingIdle: true))
        XCTAssertFalse(SearchResultsViewController.shouldShowIdleEmptyState(hideWhenEmpty: false, query: "", isFilterUnconstrained: true, hasActiveDirectory: false, isIndexingIdle: true))
        XCTAssertFalse(SearchResultsViewController.shouldShowIdleEmptyState(hideWhenEmpty: true, query: "", isFilterUnconstrained: false, hasActiveDirectory: false, isIndexingIdle: true))
        XCTAssertFalse(SearchResultsViewController.shouldShowIdleEmptyState(hideWhenEmpty: true, query: "", isFilterUnconstrained: true, hasActiveDirectory: true, isIndexingIdle: true))

        controller.displayResults([], preserveSelection: false)
        controller.emptyState = .idleEmpty
        XCTAssertFalse(controller.emptyStateContainer.isHidden)
        XCTAssertEqual(controller.emptyTitleLabel.stringValue, L("search.empty.idle.title"))
        XCTAssertFalse(controller.emptySubtitleLabel.isHidden)
        XCTAssertEqual(controller.emptySubtitleLabel.stringValue, L("search.empty.idle.subtitle"))
        XCTAssertFalse(controller.emptySyntaxSection.isHidden)
        XCTAssertEqual(controller.syntaxTipButtons.count, 6)
        XCTAssertTrue(controller.emptyRecentSection.isHidden)
        XCTAssertNil(controller.tableView.headerView)

        controller.updateRecentSearches(["项目计划.pdf", "设计稿.sketch", "会议记录.docx", "q4_budget.xlsx", "架构图.png", "第六条应被截断"])
        XCTAssertFalse(controller.emptyRecentSection.isHidden)
        XCTAssertEqual(controller.recentSearchChips.count, 5)
        var selectedRecent: String?
        var clearRecentCount = 0
        controller.onSelectRecentSearch = { selectedRecent = $0 }
        controller.onClearRecentSearches = { clearRecentCount += 1 }
        controller.recentSearchChips[1].simulateClick()
        XCTAssertEqual(selectedRecent, "设计稿.sketch")
        selectedRecent = nil
        XCTAssertTrue(controller.recentSearchChips[0].accessibilityPerformPress())
        XCTAssertEqual(selectedRecent, "项目计划.pdf")
        controller.triggerClearRecentSearchesForTesting()
        XCTAssertEqual(clearRecentCount, 1)
        controller.updateRecentSearches([])
        XCTAssertTrue(controller.emptyRecentSection.isHidden)

        var selectedExample: String?
        controller.onSelectSyntaxExample = { selectedExample = $0 }
        let extTipButton = try XCTUnwrap(controller.syntaxTipButtons.first { $0.syntaxExample == "ext:pdf,docx" })
        extTipButton.simulateClick()
        XCTAssertEqual(selectedExample, "ext:pdf,docx")
        selectedExample = nil
        XCTAssertTrue(extTipButton.accessibilityPerformPress())
        XCTAssertEqual(selectedExample, "ext:pdf,docx")

        controller.updateEmptyContext(query: "dsafas", isFuzzyActive: false)
        controller.emptyState = .noMatches
        XCTAssertTrue(controller.emptySyntaxSection.isHidden)
        XCTAssertTrue(controller.emptyRecentSection.isHidden)
        XCTAssertTrue(controller.tableView.headerView === controller.savedHeaderView)
        XCTAssertNotNil(controller.tableView.headerView?.menu)
        XCTAssertEqual(controller.emptySubtitleLabel.stringValue, String(format: L("search.empty.subtitleWithQuery"), "dsafas"))
        XCTAssertFalse(controller.emptyNoMatchActionsGrid.isHidden)
        XCTAssertEqual(controller.noMatchActionCards.count, 4)

        var focusSearchCalls = 0
        var enableFuzzyCalls = 0
        var openFilterCalls = 0
        var openIndexCalls = 0
        controller.onFocusSearchField = { focusSearchCalls += 1 }
        controller.onEnableFuzzyMode = { enableFuzzyCalls += 1 }
        controller.onOpenFilterPopover = { openFilterCalls += 1 }
        controller.onManageIndexRules = { openIndexCalls += 1 }

        controller.noMatchActionCards[0].simulateClick()
        controller.noMatchActionCards[1].simulateClick()
        controller.noMatchActionCards[2].simulateClick()
        controller.noMatchActionCards[3].simulateClick()
        XCTAssertEqual(focusSearchCalls, 1)
        XCTAssertEqual(enableFuzzyCalls, 1)
        XCTAssertEqual(openFilterCalls, 1)
        XCTAssertEqual(openIndexCalls, 1)

        controller.updateEmptyContext(query: "dsafas", isFuzzyActive: true)
        XCTAssertFalse(controller.noMatchActionCards[1].isCardEnabled)
        XCTAssertFalse(controller.noMatchActionCards[1].accessibilityPerformPress())
        controller.noMatchActionCards[1].simulateClick()
        XCTAssertEqual(enableFuzzyCalls, 1)
    }

    func testShowInfoErrorHandlingAndEntitlementInvariants() throws {
        let controller = SearchResultsViewController()
        _ = controller.view

        let infoTmpFile = "/tmp/lokii-getinfo-test-\(UUID().uuidString).txt"
        FileManager.default.createFile(atPath: infoTmpFile, contents: Data("ok".utf8))
        defer { try? FileManager.default.removeItem(atPath: infoTmpFile) }

        let missingResult = TestFixture.searchResult(
            name: "missing.txt",
            path: "/tmp/lokii-getinfo-missing-\(UUID().uuidString).txt",
            extension: "txt",
            size: 1
        )
        controller.displayResults([missingResult], preserveSelection: false)
        controller.tableView.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
        XCTAssertEqual(controller.selectedResults.count, 1)
        controller.showInfoSelected(nil)
        waitMainUntil { controller.statusLabel.stringValue == L("search.getInfo.notFound") }
        XCTAssertEqual(controller.statusLabel.stringValue, L("search.getInfo.notFound"))

        let existingResult = TestFixture.searchResult(name: "present.txt", path: infoTmpFile, extension: "txt", size: 2)
        controller.displayResults([existingResult], preserveSelection: false)
        controller.tableView.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)

        var automationPromptCount = 0
        var scriptExecutorCalls = 0
        controller.automationPermissionDeniedHandler = { automationPromptCount += 1 }

        controller.cancelTemporaryStatus()
        controller.statusLabel.stringValue = ""
        controller.appleScriptExecutor = { _ in
            DispatchQueue.main.async { scriptExecutorCalls += 1 }
            return .init(succeeded: false, errorNumber: -128, errorMessage: "User canceled.")
        }
        controller.showInfoSelected(nil)
        waitMainUntil { scriptExecutorCalls == 1 }
        CFRunLoopRunInMode(.defaultMode, 0.05, true)
        XCTAssertEqual(scriptExecutorCalls, 1)
        XCTAssertEqual(automationPromptCount, 0)
        XCTAssertEqual(controller.statusLabel.stringValue, "")

        controller.appleScriptExecutor = { _ in
            .init(succeeded: false, errorNumber: -1743, errorMessage: "Not authorized to send Apple events to Finder.")
        }
        controller.showInfoSelected(nil)
        waitMainUntil { automationPromptCount == 1 && controller.statusLabel.stringValue == L("search.getInfo.automationDenied.title") }
        XCTAssertEqual(automationPromptCount, 1)
        XCTAssertEqual(controller.statusLabel.stringValue, L("search.getInfo.automationDenied.title"))
        XCTAssertTrue(SearchResultsViewController.automationPrivacySettingsURLs.allSatisfy { $0.contains("Privacy_Automation") })

        controller.cancelTemporaryStatus()
        controller.statusLabel.stringValue = ""
        controller.appleScriptExecutor = { _ in
            .init(succeeded: false, errorNumber: -1000, errorMessage: nil)
        }
        controller.showInfoSelected(nil)
        waitMainUntil { controller.statusLabel.stringValue == L("search.getInfo.failed") }
        XCTAssertEqual(controller.statusLabel.stringValue, L("search.getInfo.failed"))

        controller.cancelTemporaryStatus()
        controller.statusLabel.stringValue = ""
        controller.appleScriptExecutor = { _ in
            .init(succeeded: false, errorNumber: -1000, errorMessage: "  \n\t ")
        }
        controller.showInfoSelected(nil)
        waitMainUntil { controller.statusLabel.stringValue == L("search.getInfo.failed") }
        XCTAssertEqual(controller.statusLabel.stringValue, L("search.getInfo.failed"))

        let searchResultsSource = try TestFixture.readRepoFile("Lokii/Lokii/SearchResultsViewController.swift")
        XCTAssertTrue(
            searchResultsSource.contains("NSApp.activate(ignoringOtherApps: true)")
                && !searchResultsSource.contains("NSApp.activate()")
        )

        let projectYmlSource = try TestFixture.readRepoFile("Lokii/project.yml")
        XCTAssertTrue(projectYmlSource.contains("com.apple.security.automation.apple-events: true"))
    }
}
