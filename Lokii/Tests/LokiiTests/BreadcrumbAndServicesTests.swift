import Cocoa
import Quartz
import UniformTypeIdentifiers
import XCTest
@testable import LokiiAppLib

final class BreadcrumbAndServicesTests: XCTestCase {
    private final class MockBreadcrumbActionDelegate: BreadcrumbPathControlDelegate {
        var requestedURLs: [URL] = []

        func breadcrumbPathControl(_ control: BreadcrumbPathControl, menuFor url: URL) -> NSMenu? {
            requestedURLs.append(url)
            let menu = NSMenu(title: "MockBreadcrumbMenu")
            let item = NSMenuItem(title: url.lastPathComponent, action: nil, keyEquivalent: "")
            item.representedObject = url
            menu.addItem(item)
            return menu
        }
    }

    func testBreadcrumbAcceptsFirstMouseAndGeometryHitTesting() throws {
        let breadcrumbControl = BreadcrumbPathControl(frame: NSRect(x: 0, y: 0, width: 500, height: 28))
        breadcrumbControl.pathStyle = .standard
        XCTAssertTrue(breadcrumbControl.acceptsFirstMouse(for: nil))

        let dummyMouseEvent = NSEvent.mouseEvent(
            with: .rightMouseDown,
            location: NSPoint(x: 10, y: 10),
            modifierFlags: [],
            timestamp: 0,
            windowNumber: 0,
            context: nil,
            eventNumber: 0,
            clickCount: 1,
            pressure: 1.0
        )
        XCTAssertTrue(breadcrumbControl.acceptsFirstMouse(for: dummyMouseEvent))

        let testWindow = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 800, height: 600),
            styleMask: [.titled],
            backing: .buffered,
            defer: false
        )
        let pathControl = BreadcrumbPathControl(frame: NSRect(x: 0, y: 0, width: 600, height: 28))
        pathControl.pathStyle = .standard
        let mockDelegate = MockBreadcrumbActionDelegate()
        pathControl.actionDelegate = mockDelegate
        testWindow.contentView?.addSubview(pathControl)

        pathControl.url = URL(fileURLWithPath: "/workspace/src/Lokii")
        let pathCell = try XCTUnwrap(pathControl.cell as? NSPathCell)
        let components = pathCell.pathComponentCells
        XCTAssertGreaterThanOrEqual(components.count, 3)

        let targetComponent = components[components.count - 1]
        let targetRect = pathCell.rect(of: targetComponent, withFrame: pathControl.bounds, in: pathControl)
        XCTAssertTrue(targetRect.width > 0 && targetRect.height > 0)

        let hitPointInWindow = pathControl.convert(NSPoint(x: targetRect.midX, y: targetRect.midY), to: nil)
        let hitEvent = NSEvent.mouseEvent(
            with: .rightMouseDown,
            location: hitPointInWindow,
            modifierFlags: [],
            timestamp: 0,
            windowNumber: testWindow.windowNumber,
            context: nil,
            eventNumber: 1,
            clickCount: 1,
            pressure: 1.0
        )!
        XCTAssertNotNil(pathControl.menu(for: hitEvent))
        XCTAssertEqual(mockDelegate.requestedURLs.last?.path, targetComponent.url?.path)

        var maxX: CGFloat = 0
        for comp in components {
            maxX = max(maxX, pathCell.rect(of: comp, withFrame: pathControl.bounds, in: pathControl).maxX)
        }
        XCTAssertLessThan(maxX, 550)

        let blankEvent = NSEvent.mouseEvent(
            with: .rightMouseDown,
            location: pathControl.convert(NSPoint(x: maxX + 30, y: 14), to: nil),
            modifierFlags: [],
            timestamp: 0,
            windowNumber: testWindow.windowNumber,
            context: nil,
            eventNumber: 2,
            clickCount: 1,
            pressure: 1.0
        )!
        XCTAssertNil(pathControl.menu(for: blankEvent))

        let outsideEvent = NSEvent.mouseEvent(
            with: .rightMouseDown,
            location: pathControl.convert(NSPoint(x: -20, y: -20), to: nil),
            modifierFlags: [],
            timestamp: 0,
            windowNumber: testWindow.windowNumber,
            context: nil,
            eventNumber: 3,
            clickCount: 1,
            pressure: 1.0
        )!
        XCTAssertNil(pathControl.menu(for: outsideEvent))
    }

    func testServicesResponderChainAndContextualScopeLease() {
        let searchVC = SearchResultsViewController()
        _ = searchVC.view

        XCTAssertNil(searchVC.validRequestor(forSendType: .fileURL, returnType: nil))

        let itemA = TestFixture.searchResult(name: "alpha.rs", path: "/workspace/src/alpha.rs", extension: "rs", size: 1024, created: 100, modified: 200)
        let itemB = TestFixture.searchResult(name: "beta.md", path: "/workspace/docs/beta.md", extension: "md", size: 2048, created: 100, modified: 300)
        searchVC.displayResults([itemA, itemB], preserveSelection: false)
        searchVC.selectFirstRow()
        XCTAssertEqual(searchVC.selectedResults.count, 1)

        XCTAssertTrue((searchVC.validRequestor(forSendType: .fileURL, returnType: nil) as? SearchResultsViewController) === searchVC)
        XCTAssertTrue((searchVC.validRequestor(forSendType: .string, returnType: nil) as? SearchResultsViewController) === searchVC)

        let plainTextType = NSPasteboard.PasteboardType("public.plain-text")
        XCTAssertTrue((searchVC.validRequestor(forSendType: plainTextType, returnType: nil) as? SearchResultsViewController) === searchVC)

        let directoryType = NSPasteboard.PasteboardType("public.directory")
        XCTAssertNil(searchVC.validRequestor(forSendType: directoryType, returnType: nil))

        let filenamesType = NSPasteboard.PasteboardType("NSFilenamesPboardType")
        XCTAssertTrue((searchVC.validRequestor(forSendType: filenamesType, returnType: nil) as? SearchResultsViewController) === searchVC)

        let dirItem = TestFixture.searchResult(name: "src", path: "/workspace/src", isDir: true, created: 100, modified: 200)
        searchVC.displayResults([dirItem], preserveSelection: false)
        searchVC.selectFirstRow()
        XCTAssertTrue((searchVC.validRequestor(forSendType: directoryType, returnType: nil) as? SearchResultsViewController) === searchVC)

        searchVC.displayResults([itemA, itemB], preserveSelection: false)
        searchVC.selectFirstRow()
        XCTAssertNil(searchVC.validRequestor(forSendType: .pdf, returnType: nil))

        let testPboard = NSPasteboard(name: NSPasteboard.Name("LokiiTestServicesPasteboard.3.\(UUID().uuidString)"))
        defer { testPboard.releaseGlobally() }
        testPboard.clearContents()
        XCTAssertTrue(searchVC.writeSelection(to: testPboard, types: [.fileURL, .string, plainTextType, filenamesType]))

        let readURLs = testPboard.readObjects(forClasses: [NSURL.self], options: nil) as? [URL]
        XCTAssertEqual(readURLs?.map(\.path), ["/workspace/src/alpha.rs"])
        XCTAssertEqual(testPboard.string(forType: .string), "/workspace/src/alpha.rs")
        XCTAssertEqual(testPboard.string(forType: plainTextType), "/workspace/src/alpha.rs")
        XCTAssertEqual(testPboard.propertyList(forType: filenamesType) as? [String], ["/workspace/src/alpha.rs"])

        // Contextual Scope Lease 临时上下文载荷优先消费与防污染
        let dirB = URL(fileURLWithPath: "/workspace/src")
        let breadcrumbMenu = searchVC.breadcrumbPathControl(searchVC.pathBar, menuFor: dirB)
        XCTAssertNotNil(breadcrumbMenu)
        XCTAssertEqual(breadcrumbMenu?.allowsContextMenuPlugIns, true)
        XCTAssertNil(breadcrumbMenu?.items.first {
            $0.title == L("search.ctx.services") || $0.representedObject as? String == "search.ctx.services"
        })
        XCTAssertEqual(breadcrumbMenu?.items[0].action, NSSelectorFromString("breadcrumbOpenItem:"))
        XCTAssertEqual(breadcrumbMenu?.items[1].action, NSSelectorFromString("breadcrumbRevealInFinder:"))

        var breadcrumbRevealedURLs: [URL] = []
        searchVC.revealFilesHandler = { breadcrumbRevealedURLs = $0 }
        let revealMenuItem = breadcrumbMenu?.items.first { $0.action == NSSelectorFromString("breadcrumbRevealInFinder:") }
        XCTAssertNotNil(revealMenuItem)
        if let revealItem = revealMenuItem {
            _ = searchVC.perform(revealItem.action, with: revealItem)
        }
        XCTAssertEqual(breadcrumbRevealedURLs.map(\.path), ["/workspace/src"])

        var breadcrumbTerminalURLs: [URL] = []
        searchVC.openInTerminalHandler = { breadcrumbTerminalURLs = $0 }
        let terminalMenuItem = breadcrumbMenu?.items.first { $0.action == NSSelectorFromString("breadcrumbOpenInTerminal:") }
        XCTAssertNotNil(terminalMenuItem)
        if let terminalItem = terminalMenuItem {
            _ = searchVC.perform(terminalItem.action, with: terminalItem)
        }
        XCTAssertEqual(breadcrumbTerminalURLs.map(\.path), ["/workspace/src"])

        XCTAssertTrue((searchVC.validRequestor(forSendType: directoryType, returnType: nil) as? SearchResultsViewController) === searchVC)

        let testPboard4 = NSPasteboard(name: NSPasteboard.Name("LokiiTestServicesPasteboard.4.\(UUID().uuidString)"))
        defer { testPboard4.releaseGlobally() }
        testPboard4.clearContents()
        XCTAssertTrue(searchVC.writeSelection(to: testPboard4, types: [.fileURL]))
        let writtenUrlsLeased = testPboard4.readObjects(forClasses: [NSURL.self], options: nil) as? [URL]
        XCTAssertEqual(writtenUrlsLeased?.first?.path, "/workspace/src")

        if let menu = breadcrumbMenu, let menuDelegate = menu.delegate {
            menuDelegate.menuDidClose?(menu)
        } else {
            searchVC.pathBar.menuDelegate.menuDidClose(breadcrumbMenu ?? NSMenu())
        }

        testPboard4.clearContents()
        XCTAssertTrue(searchVC.writeSelection(to: testPboard4, types: [.fileURL]))
        let writtenUrlsRestored = testPboard4.readObjects(forClasses: [NSURL.self], options: nil) as? [URL]
        XCTAssertEqual(writtenUrlsRestored?.first?.path, "/workspace/src/alpha.rs")
    }

    func testBreadcrumbMenuDelegateAndSplitScopeFiltering() {
        _ = NSApplication.shared
        let priorServicesMenu = NSApp.servicesMenu
        let originalAppServicesMenu = NSMenu(title: "OriginalGlobalServicesMenu")
        NSApp.servicesMenu = originalAppServicesMenu
        defer { NSApp.servicesMenu = priorServicesMenu }

        let breadcrumbMenuDelegate = BreadcrumbMenuDelegate()
        let mockComponentCell = NSPathComponentCell()
        mockComponentCell.isHighlighted = true
        breadcrumbMenuDelegate.activeComponentCell = mockComponentCell

        let testContextMenu = NSMenu(title: "TestContextMenu")
        let servicesItem = NSMenuItem(title: "服务", action: nil, keyEquivalent: "")
        servicesItem.representedObject = "search.ctx.services"
        let contextServicesSubmenu = NSMenu(title: "ContextServicesSubmenu")
        servicesItem.submenu = contextServicesSubmenu
        testContextMenu.addItem(servicesItem)

        var onCloseTriggered = false
        breadcrumbMenuDelegate.onClose = { onCloseTriggered = true }

        breadcrumbMenuDelegate.menuWillOpen(testContextMenu)
        XCTAssertTrue(NSApp.servicesMenu === contextServicesSubmenu)

        breadcrumbMenuDelegate.menuDidClose(testContextMenu)
        XCTAssertTrue(NSApp.servicesMenu === originalAppServicesMenu)
        XCTAssertFalse(mockComponentCell.isHighlighted)
        XCTAssertNil(breadcrumbMenuDelegate.activeComponentCell)
        XCTAssertTrue(onCloseTriggered)

        let splitVC = SearchMainSplitViewController()
        _ = splitVC.view
        XCTAssertTrue(splitVC.isSidebarCollapsed)

        let allResults = [
            TestFixture.searchResult(name: "main.rs", path: "/workspace/lokii/src/main.rs", extension: "rs", size: 1024, created: 100, modified: 200),
            TestFixture.searchResult(name: "util.rs", path: "/workspace/lokii/src/util.rs", extension: "rs", size: 2048, created: 100, modified: 210),
            TestFixture.searchResult(name: "Cargo.toml", path: "/workspace/lokii/Cargo.toml", extension: "toml", size: 512, created: 100, modified: 220),
            TestFixture.searchResult(name: "notes.txt", path: "/other/docs/notes.txt", extension: "txt", size: 128, created: 100, modified: 230),
        ]
        splitVC.updateSearchResults(rawResults: allResults, tree: ResultDirectoryTree.build(from: allResults))
        XCTAssertEqual(splitVC.searchResultsViewController.results.count, 4)

        splitVC.searchResultsDidRequestScopeFilter(path: "/workspace/lokii/src")
        XCTAssertFalse(splitVC.isSidebarCollapsed)
        XCTAssertEqual(splitVC.searchResultsViewController.results.count, 2)
        XCTAssertTrue(splitVC.searchResultsViewController.results.allSatisfy { $0.path.hasPrefix("/workspace/lokii/src") })

        splitVC.searchResultsDidRequestScopeFilter(path: "/workspace/lokii")
        XCTAssertEqual(splitVC.searchResultsViewController.results.count, 3)
        XCTAssertTrue(splitVC.searchResultsViewController.results.allSatisfy { $0.path.hasPrefix("/workspace/lokii") })

        splitVC.filterByDirectory(path: "/other")
        XCTAssertEqual(splitVC.searchResultsViewController.results.count, 1)
        XCTAssertEqual(splitVC.searchResultsViewController.results.first?.name, "notes.txt")
    }
}
