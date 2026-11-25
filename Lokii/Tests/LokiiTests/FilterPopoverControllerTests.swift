import Cocoa
import SwiftUI
import XCTest
@testable import LokiiAppLib

final class FilterPopoverControllerTests: XCTestCase {
    func testPopoverLifecycleAndQuerySync() {
        _ = NSApplication.shared

        let initial = FilterPopoverController()
        XCTAssertFalse(initial.isShown)
        XCTAssertEqual(initial.currentState.activeCount, 0)
        XCTAssertTrue(initial.currentState.isUnconstrained)

        let controller = FilterPopoverController(animates: false)
        let searchField = NSSearchField()
        searchField.stringValue = "query type:code size:>10m"

        controller.configure(
            searchField: searchField,
            onSearch: { _ in },
            onFilterStateChanged: { _ in }
        )

        let window = NSWindow(
            contentRect: NSRect(x: 300, y: 500, width: 600, height: 500),
            styleMask: [.titled, .closable],
            backing: .buffered,
            defer: false
        )
        window.isReleasedWhenClosed = false
        defer {
            controller.close()
            window.orderOut(nil)
            window.close()
        }
        let anchorButton = NSButton()
        anchorButton.frame = NSRect(x: 480, y: 440, width: 38, height: 22)
        window.contentView?.addSubview(anchorButton)
        window.orderFront(nil)

        controller.show(relativeTo: anchorButton)
        RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.05))
        XCTAssertTrue(controller.isShown)
        let btnScreen = window.convertToScreen(anchorButton.convert(anchorButton.bounds, to: nil))
        if let popWin = NSApp.windows.first(where: { $0 !== window && $0.isVisible && String(describing: type(of: $0)).contains("Popover") }) {
            XCTAssertLessThanOrEqual(abs(popWin.frame.maxY - btnScreen.minY), 2.0)
        }

        let currentState = controller.currentState
        XCTAssertTrue(currentState.selectedTypes.contains(.code))
        XCTAssertEqual(currentState.minSizeBytes, 10_000_000)
        XCTAssertEqual(currentState.activeCount, 2)

        controller.close()
        RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.1))
        XCTAssertFalse(controller.isShown)

        controller.toggle(relativeTo: anchorButton)
        RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.05))
        XCTAssertTrue(controller.isShown)
        controller.toggle(relativeTo: anchorButton)
        RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.1))
        XCTAssertFalse(controller.isShown)

        let detachedButton = NSButton()
        let unattachedController = FilterPopoverController(animates: false)
        unattachedController.show(relativeTo: detachedButton)
        XCTAssertFalse(unattachedController.isShown)

        window.contentView?.addSubview(searchField)
        window.makeKeyAndOrderFront(nil)
        controller.show(relativeTo: anchorButton)
        XCTAssertTrue(controller.isShown)
        controller.close()
        RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.1))
        XCTAssertFalse(controller.isShown)
        XCTAssertTrue(window.firstResponder === searchField || window.firstResponder === searchField.currentEditor())
    }

    func testExternalQuerySyncAndOptionFRouting() {
        let controller = FilterPopoverController()
        var capturedState: FilterState?
        let searchField = NSSearchField()

        controller.configure(
            searchField: searchField,
            onSearch: { _ in },
            onFilterStateChanged: { state in capturedState = state }
        )

        controller.syncStateFromQuery("document ext:pdf type:doc,img size:1m..50m")
        XCTAssertEqual(capturedState?.selectedTypes.contains(.document), true)
        XCTAssertEqual(capturedState?.selectedTypes.contains(.image), true)
        XCTAssertEqual(capturedState?.minSizeBytes, 1_000_000)
        XCTAssertEqual(capturedState?.maxSizeBytes, 50_000_000)
        XCTAssertEqual(capturedState?.activeCount, 2)
        XCTAssertFalse(controller.flushPendingStateIfNeeded())

        let window = LokiiWindow(
            contentRect: NSRect(x: 100, y: 100, width: 800, height: 600),
            styleMask: [.titled, .closable, .resizable],
            backing: .buffered,
            defer: false
        )

        var optionFCalled = false
        var commandFCalled = false
        window.onOptionF = { optionFCalled = true }
        window.onCommandF = { commandFCalled = true }

        let optionFEvent = NSEvent.keyEvent(
            with: .keyDown,
            location: .zero,
            modifierFlags: [.option],
            timestamp: 0,
            windowNumber: window.windowNumber,
            context: nil,
            characters: "ƒ",
            charactersIgnoringModifiers: "f",
            isARepeat: false,
            keyCode: 3
        )!
        XCTAssertTrue(window.performKeyEquivalent(with: optionFEvent))
        XCTAssertTrue(optionFCalled)

        optionFCalled = false
        let cmdOptionFEvent = NSEvent.keyEvent(
            with: .keyDown,
            location: .zero,
            modifierFlags: [.command, .option],
            timestamp: 0,
            windowNumber: window.windowNumber,
            context: nil,
            characters: "ƒ",
            charactersIgnoringModifiers: "f",
            isARepeat: false,
            keyCode: 3
        )!
        XCTAssertFalse(window.performKeyEquivalent(with: cmdOptionFEvent))
        XCTAssertFalse(optionFCalled)

        let cmdFEvent = NSEvent.keyEvent(
            with: .keyDown,
            location: .zero,
            modifierFlags: [.command],
            timestamp: 0,
            windowNumber: window.windowNumber,
            context: nil,
            characters: "f",
            charactersIgnoringModifiers: "f",
            isARepeat: false,
            keyCode: 3
        )!
        window.keyDown(with: cmdFEvent)
        XCTAssertTrue(commandFCalled)
    }

    func testSearchWindowIntegrationAndLocalizationInvariants() throws {
        let searchWindowSource = try TestFixture.readRepoFile("Lokii/Lokii/SearchWindow.swift")
        let controllerSource = try TestFixture.readRepoFile("Lokii/Lokii/FilterPopoverController.swift")
        let zhContent = try TestFixture.readRepoFile("Lokii/Lokii/Resources/zh-Hans.lproj/Localizable.strings")
        let enContent = try TestFixture.readRepoFile("Lokii/Lokii/Resources/en.lproj/Localizable.strings")

        XCTAssertTrue(searchWindowSource.contains("filterPopoverController"))
        XCTAssertTrue(searchWindowSource.contains("filterButton"))
        XCTAssertTrue(searchWindowSource.contains("window.onOptionF"))
        XCTAssertTrue(searchWindowSource.contains("filterButtonClicked"))
        XCTAssertTrue(searchWindowSource.contains("greaterThanOrEqualToConstant: 360"))
        XCTAssertTrue(searchWindowSource.contains("line.3.horizontal.decrease"))
        XCTAssertTrue(searchWindowSource.contains("filterButton.isBordered = false"))
        XCTAssertTrue(searchWindowSource.contains("if self.filterPopoverController.isShown"))
        XCTAssertTrue(searchWindowSource.contains("self.filterPopoverController.close()"))
        XCTAssertTrue(searchWindowSource.contains("self.loadDefaultResults()"))

        XCTAssertTrue(controllerSource.contains("guard view.window != nil else { return }"))
        XCTAssertTrue(controllerSource.contains("restoreSearchFieldFocus"))
        XCTAssertTrue(controllerSource.contains("popoverDidClose"))

        XCTAssertTrue(zhContent.contains("\"filter.button.tooltip\""))
        XCTAssertTrue(enContent.contains("\"filter.button.tooltip\""))
        XCTAssertTrue(zhContent.contains("\"filter.button.tooltip.active\""))
        XCTAssertTrue(enContent.contains("\"filter.button.tooltip.active\""))
    }
}
