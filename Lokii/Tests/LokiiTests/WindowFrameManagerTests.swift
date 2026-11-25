import AppKit
import Foundation
import XCTest
@testable import LokiiAppLib

final class WindowFrameManagerTests: XCTestCase {
    private let largeScreen = ScreenMetrics(
        frame: NSRect(x: 0, y: 0, width: 2560, height: 1440),
        visibleFrame: NSRect(x: 0, y: 0, width: 2560, height: 1415)
    )
    private let laptopScreen = ScreenMetrics(
        frame: NSRect(x: 2560, y: 0, width: 1512, height: 982),
        visibleFrame: NSRect(x: 2560, y: 0, width: 1512, height: 945)
    )
    private let compactScreen = ScreenMetrics(
        frame: NSRect(x: 0, y: 0, width: 1024, height: 640),
        visibleFrame: NSRect(x: 0, y: 0, width: 1024, height: 615)
    )

    private func makeIsolatedDefaults() -> (UserDefaults, String) {
        let suite = "com.lokii.test.windowframe.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defaults.removePersistentDomain(forName: suite)
        return (defaults, suite)
    }

    func testDefaultFrameAndVerticalOffset() {
        let fLarge = WindowFrameManager.defaultFrame(on: largeScreen)
        XCTAssertEqual(fLarge.width, 1280)
        XCTAssertEqual(fLarge.height, 860)
        let expectedLargeY = round((1415.0 - 860.0) / 2.0 + 1415.0 * 0.05)
        XCTAssertEqual(fLarge.origin.x, round((2560.0 - 1280.0) / 2.0))
        XCTAssertEqual(fLarge.origin.y, expectedLargeY)

        let fLaptop = WindowFrameManager.defaultFrame(on: laptopScreen)
        XCTAssertEqual(fLaptop.width, 960)
        XCTAssertEqual(fLaptop.height, 600)
        let expectedLaptopY = round((945.0 - 600.0) / 2.0 + 945.0 * 0.05)
        XCTAssertEqual(fLaptop.origin.y, expectedLaptopY)
    }

    func testPerScreenRememberedSizesAndFallback() {
        let (defaults, suite) = makeIsolatedDefaults()
        defer { defaults.removePersistentDomain(forName: suite) }
        let manager = WindowFrameManager(defaults: defaults)

        XCTAssertFalse(manager.hasCustomSizes)
        manager.saveSize(NSSize(width: 1200, height: 800), for: largeScreen)
        XCTAssertTrue(manager.hasCustomSizes)

        XCTAssertEqual(manager.resolveSize(for: largeScreen), NSSize(width: 1200, height: 800))
        XCTAssertEqual(manager.resolveSize(for: laptopScreen), NSSize(width: 960, height: 600))

        manager.saveSize(NSSize(width: 980, height: 620), for: laptopScreen)
        XCTAssertEqual(manager.resolveSize(for: laptopScreen), NSSize(width: 980, height: 620))
        XCTAssertEqual(manager.resolveSize(for: largeScreen), NSSize(width: 1200, height: 800))
    }

    func testRecenterWhenRememberPositionDisabled() {
        let (defaults, suite) = makeIsolatedDefaults()
        defer { defaults.removePersistentDomain(forName: suite) }
        let manager = WindowFrameManager(defaults: defaults)
        manager.saveSize(NSSize(width: 1120, height: 700), for: largeScreen)

        let draggedFrame = NSRect(x: 80, y: 100, width: 1120, height: 700)
        let shownFrame = manager.frameOnShow(
            currentFrame: draggedFrame,
            targetScreen: largeScreen,
            allScreens: [largeScreen, laptopScreen],
            rememberPosition: false
        )
        let expected = WindowFrameManager.centeredFrame(
            preservingSize: NSSize(width: 1120, height: 700),
            on: largeScreen,
            minSize: WindowFrameManager.defaultMinSize
        )
        XCTAssertEqual(shownFrame, expected)
    }

    func testRememberPositionAndDisconnectedMonitorFallback() {
        let (defaults, suite) = makeIsolatedDefaults()
        defer { defaults.removePersistentDomain(forName: suite) }
        let manager = WindowFrameManager(defaults: defaults)
        manager.saveSize(NSSize(width: 1100, height: 680), for: largeScreen)
        manager.saveOrigin(NSPoint(x: 200, y: 300))

        let initial = manager.initialFrame(
            on: largeScreen,
            allScreens: [largeScreen, laptopScreen],
            rememberPosition: true
        )
        XCTAssertEqual(initial, NSRect(x: 200, y: 300, width: 1100, height: 680))

        let fallbackShown = manager.frameOnShow(
            currentFrame: initial,
            targetScreen: laptopScreen,
            allScreens: [laptopScreen],
            rememberPosition: true
        )
        let expectedFallback = WindowFrameManager.centeredFrame(
            preservingSize: NSSize(width: 960, height: 600),
            on: laptopScreen,
            minSize: WindowFrameManager.defaultMinSize
        )
        XCTAssertEqual(fallbackShown, expectedFallback)
    }

    func testCrossScreenOverflowClamping() {
        let (defaults, suite) = makeIsolatedDefaults()
        defer { defaults.removePersistentDomain(forName: suite) }
        let manager = WindowFrameManager(defaults: defaults)
        manager.saveSize(NSSize(width: 1200, height: 800), for: largeScreen)

        let fitsOnLaptop = NSRect(x: 2700, y: 100, width: 1200, height: 800)
        XCTAssertNil(manager.clampedFrameForScreenOverflow(currentFrame: fitsOnLaptop, newScreen: laptopScreen, anchorX: 2900))

        let heightOverflow = NSRect(x: 2720, y: -40, width: 1100, height: 1050)
        let clampedHeight = manager.clampedFrameForScreenOverflow(currentFrame: heightOverflow, newScreen: laptopScreen, anchorX: 2950)
        XCTAssertNotNil(clampedHeight)
        if let clampedHeight {
            XCTAssertEqual(clampedHeight.width, 1100)
            XCTAssertEqual(clampedHeight.origin.x, 2720)
            XCTAssertEqual(clampedHeight.height, laptopScreen.visibleFrame.height)
            XCTAssertTrue(clampedHeight.minY >= laptopScreen.visibleFrame.minY && clampedHeight.maxY <= laptopScreen.visibleFrame.maxY)
        }

        let widthOverflow = NSRect(x: 2500, y: 120, width: 1700, height: 760)
        let clampedWidth = manager.clampedFrameForScreenOverflow(currentFrame: widthOverflow, newScreen: laptopScreen, anchorX: 2800)
        XCTAssertNotNil(clampedWidth)
        if let clampedWidth {
            XCTAssertEqual(clampedWidth.width, laptopScreen.visibleFrame.width)
            XCTAssertEqual(clampedWidth.height, 760)
            XCTAssertEqual(clampedWidth.origin.x, laptopScreen.visibleFrame.origin.x)
            XCTAssertEqual(clampedWidth.origin.y, 120)
        }

        let bothOverflow = NSRect(x: 2400, y: -100, width: 1800, height: 1100)
        let clampedBoth = manager.clampedFrameForScreenOverflow(currentFrame: bothOverflow, newScreen: laptopScreen, anchorX: 2850)
        XCTAssertEqual(clampedBoth, laptopScreen.visibleFrame)
    }

    func testLegacyMigrationAndClamping() {
        let (defaults, suite) = makeIsolatedDefaults()
        defer { defaults.removePersistentDomain(forName: suite) }
        defaults.set(NSStringFromRect(NSRect(x: 150, y: 250, width: 1180, height: 740)), forKey: "LokiiMainWindowFrame")

        let manager = WindowFrameManager(defaults: defaults)
        manager.migrateLegacyFrameIfNeeded(currentScreen: largeScreen, rememberPosition: true)

        XCTAssertNil(defaults.string(forKey: "LokiiMainWindowFrame"))
        XCTAssertEqual(manager.resolveSize(for: largeScreen), NSSize(width: 1180, height: 740))
        XCTAssertEqual(manager.savedOrigin(), NSPoint(x: 150, y: 250))

        let compactKey = WindowFrameManager.screenSizeKey(for: compactScreen.frame)
        defaults.set([compactKey: NSStringFromSize(NSSize(width: 0, height: 0))], forKey: WindowFrameManager.sizesByScreenKey)
        XCTAssertEqual(manager.resolveSize(for: compactScreen), WindowFrameManager.defaultFrame(on: compactScreen).size)

        defaults.set([compactKey: NSStringFromSize(NSSize(width: 5000, height: 3000))], forKey: WindowFrameManager.sizesByScreenKey)
        XCTAssertEqual(manager.resolveSize(for: compactScreen), compactScreen.visibleFrame.size)

        defaults.set([compactKey: NSStringFromSize(NSSize(width: 300, height: 200))], forKey: WindowFrameManager.sizesByScreenKey)
        XCTAssertEqual(manager.resolveSize(for: compactScreen), WindowFrameManager.defaultMinSize)

        manager.clearAllCustomSizesAndOrigin()
        XCTAssertFalse(manager.hasCustomSizes)
        XCTAssertNil(manager.savedOrigin())
    }

    func testMaxOverlapScreenSelectionAndDeferredMigration() {
        let (defaults, suite) = makeIsolatedDefaults()
        defer { defaults.removePersistentDomain(forName: suite) }
        let manager = WindowFrameManager(defaults: defaults)

        let mostlyOnLaptop = NSRect(x: 2550, y: 150, width: 960, height: 600)
        let shown = manager.frameOnShow(
            currentFrame: mostlyOnLaptop,
            targetScreen: largeScreen,
            allScreens: [largeScreen, laptopScreen],
            rememberPosition: true
        )
        XCTAssertTrue(shown.minX >= laptopScreen.visibleFrame.minX && shown.maxX <= laptopScreen.visibleFrame.maxX)

        let legacyOnLaptop = NSRect(x: 2700, y: 180, width: 1000, height: 640)
        defaults.set(NSStringFromRect(legacyOnLaptop), forKey: WindowFrameManager.legacyFrameKey)
        manager.migrateLegacyFrameIfNeeded(allScreens: [], currentScreen: nil, rememberPosition: true)
        XCTAssertNotNil(defaults.string(forKey: WindowFrameManager.legacyFrameKey))

        manager.migrateLegacyFrameIfNeeded(allScreens: [largeScreen, laptopScreen], currentScreen: largeScreen, rememberPosition: true)
        XCTAssertNil(defaults.string(forKey: WindowFrameManager.legacyFrameKey))
        XCTAssertEqual(manager.resolveSize(for: laptopScreen), NSSize(width: 1000, height: 640))
        XCTAssertEqual(manager.resolveSize(for: largeScreen), WindowFrameManager.defaultFrame(on: largeScreen).size)
    }

    func testOverflowClampDeferralWhileMouseDown() {
        XCTAssertTrue(WindowFrameManager.shouldDeferOverflowClamp(isLeftMouseButtonDown: true))
        XCTAssertFalse(WindowFrameManager.shouldDeferOverflowClamp(isLeftMouseButtonDown: false))
    }
}
