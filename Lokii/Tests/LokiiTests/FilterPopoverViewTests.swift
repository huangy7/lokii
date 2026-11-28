import Cocoa
import SwiftUI
import XCTest
@testable import LokiiAppLib

final class FilterPopoverViewTests: XCTestCase {
    func testSizeUnitCalculationAndSanitization() {
        XCTAssertEqual(SizeUnit.bytes.multiplier, 1)
        XCTAssertEqual(SizeUnit.kb.multiplier, 1_000)
        XCTAssertEqual(SizeUnit.mb.multiplier, 1_000_000)
        XCTAssertEqual(SizeUnit.gb.multiplier, 1_000_000_000)

        XCTAssertEqual(FilterPopoverView.calculateBytes(text: "10", unit: .mb), 10_000_000)
        XCTAssertEqual(FilterPopoverView.calculateBytes(text: "500", unit: .kb), 500_000)
        XCTAssertEqual(FilterPopoverView.calculateBytes(text: "2", unit: .gb), 2_000_000_000)
        XCTAssertEqual(FilterPopoverView.calculateBytes(text: "1024", unit: .bytes), 1024)
        XCTAssertEqual(FilterPopoverView.calculateBytes(text: "1.5", unit: .gb), 1_500_000_000)
        XCTAssertEqual(FilterPopoverView.calculateBytes(text: "0.5", unit: .mb), 500_000)

        XCTAssertNil(FilterPopoverView.calculateBytes(text: "", unit: .mb))
        XCTAssertNil(FilterPopoverView.calculateBytes(text: "   ", unit: .mb))
        XCTAssertNil(FilterPopoverView.calculateBytes(text: "abc", unit: .mb))
        XCTAssertNil(FilterPopoverView.calculateBytes(text: "-10", unit: .mb))

        XCTAssertEqual(FilterPopoverView.sanitizeNumberString("123.45"), "123.45")
        XCTAssertEqual(FilterPopoverView.sanitizeNumberString("1.2.3.4"), "1.234")
        XCTAssertEqual(FilterPopoverView.sanitizeNumberString(" 10 MB "), "10")
        XCTAssertEqual(FilterPopoverView.sanitizeNumberString("①②²½٣4"), "4")
        XCTAssertEqual(FilterPopoverView.sanitizeNumberString(".5"), ".5")

        let (t1, u1) = FilterPopoverView.decomposeBytes(10_000_000)
        XCTAssertEqual(t1, "10")
        XCTAssertEqual(u1, .mb)

        let (t2, u2) = FilterPopoverView.decomposeBytes(500_000)
        XCTAssertEqual(t2, "500")
        XCTAssertEqual(u2, .kb)

        let (t3, u3) = FilterPopoverView.decomposeBytes(2_000_000_000)
        XCTAssertEqual(t3, "2")
        XCTAssertEqual(u3, .gb)

        let (t4, u4) = FilterPopoverView.decomposeBytes(1_500_000_000)
        XCTAssertEqual(t4, "1.5")
        XCTAssertEqual(u4, .gb)

        let (t5, u5) = FilterPopoverView.decomposeBytes(1024)
        XCTAssertEqual(t5, "1024")
        XCTAssertEqual(u5, .bytes)

        let (t6, u6) = FilterPopoverView.decomposeBytes(0)
        XCTAssertEqual(t6, "0")
        XCTAssertEqual(u6, .bytes)
    }

    func testFileTypeSelectionClearAndDismiss() {
        var state = FilterState()
        var lastState: FilterState?
        var dismissTriggered = false
        let binding = Binding(get: { state }, set: { state = $0 })
        let view = FilterPopoverView(
            state: binding,
            onStateChanged: { lastState = $0 },
            onDismiss: { dismissTriggered = true }
        )

        XCTAssertTrue(view.isAllSelected)
        XCTAssertEqual(FilterPopoverView.fileTypeItems.count, 6)
        XCTAssertEqual(
            FilterPopoverView.fileTypeItems.map { $0.type },
            [.code, .document, .image, .media, .archive, .folder]
        )
        for item in FilterPopoverView.fileTypeItems {
            XCTAssertFalse(item.symbolName.isEmpty)
            XCTAssertNotNil(NSImage(systemSymbolName: item.symbolName, accessibilityDescription: nil))
        }

        view.toggleFileType(.code)
        XCTAssertEqual(state.selectedTypes, [.code])
        XCTAssertFalse(view.isAllSelected)
        XCTAssertEqual(lastState?.selectedTypes, [.code])

        view.toggleFileType(.document)
        XCTAssertEqual(state.selectedTypes, [.code, .document])
        XCTAssertFalse(view.isAllSelected)

        view.toggleFileType(.code)
        XCTAssertEqual(state.selectedTypes, [.document])
        XCTAssertFalse(view.isAllSelected)

        view.toggleFileType(.document)
        XCTAssertTrue(state.selectedTypes.isEmpty)
        XCTAssertTrue(view.isAllSelected)

        view.toggleFileType(.image)
        XCTAssertFalse(view.isAllSelected)
        view.toggleAllTypes()
        XCTAssertTrue(state.selectedTypes.isEmpty)
        XCTAssertTrue(view.isAllSelected)

        for t in FilterState.concreteFileTypes {
            view.toggleFileType(t)
        }
        XCTAssertTrue(view.isAllSelected)

        state.minSizeBytes = 10_000_000
        state.maxSizeBytes = 100_000_000
        state.selectedTypes = [.code]
        XCTAssertEqual(state.activeCount, 2)

        view.clearAll()
        XCTAssertTrue(state.isUnconstrained)
        XCTAssertEqual(state.activeCount, 0)
        XCTAssertTrue(state.selectedTypes.isEmpty)
        XCTAssertNil(state.minSizeBytes)
        XCTAssertNil(state.maxSizeBytes)
        XCTAssertEqual(lastState, state)

        view.onDismiss?()
        XCTAssertTrue(dismissTriggered)
    }

    func testDebounceEditingIsolationAndInvertedRange() {
        var state = FilterState()
        var stateChangedCount = 0
        let binding = Binding(get: { state }, set: { state = $0 })
        let view = FilterPopoverView(
            state: binding,
            onStateChanged: { _ in stateChangedCount += 1 }
        )

        view.handleMinToggle(true)
        XCTAssertEqual(state.minSizeBytes, 10_000_000)
        XCTAssertEqual(stateChangedCount, 1)
        XCTAssertTrue(view.isMinEnabled)

        view.handleMinTextChange("2")
        view.handleMinTextChange("20")
        XCTAssertEqual(state.minSizeBytes, 10_000_000)
        XCTAssertEqual(stateChangedCount, 1)

        view.flushDebouncedChanges()
        XCTAssertEqual(state.minSizeBytes, 20_000_000)
        XCTAssertEqual(stateChangedCount, 2)
        XCTAssertTrue(view.isMinEnabled)

        view.handleMinTextChange("")
        view.flushDebouncedChanges()
        XCTAssertNil(state.minSizeBytes)
        XCTAssertTrue(view.isMinEnabled)

        view.handleMinTextChange("50")
        view.flushDebouncedChanges()
        XCTAssertEqual(state.minSizeBytes, 50_000_000)
        XCTAssertTrue(view.isMinEnabled)

        view.handleMinToggle(false)
        XCTAssertNil(state.minSizeBytes)
        XCTAssertFalse(view.isMinEnabled)

        var s1 = FilterState()
        XCTAssertFalse(FilterPopoverView(state: Binding(get: { s1 }, set: { s1 = $0 })).isSizeRangeInverted)

        var s2 = FilterState(minSizeBytes: 10_000_000, maxSizeBytes: 100_000_000)
        XCTAssertFalse(FilterPopoverView(state: Binding(get: { s2 }, set: { s2 = $0 })).isSizeRangeInverted)

        var s3 = FilterState(minSizeBytes: 10_000_000, maxSizeBytes: 500_000)
        XCTAssertTrue(FilterPopoverView(state: Binding(get: { s3 }, set: { s3 = $0 })).isSizeRangeInverted)

        var s4 = FilterState(minSizeBytes: 10_000_000, maxSizeBytes: nil)
        XCTAssertFalse(FilterPopoverView(state: Binding(get: { s4 }, set: { s4 = $0 })).isSizeRangeInverted)
    }

    func testLocalizationKeys() throws {
        let zhContent = try TestFixture.readRepoFile("Lokii/Lokii/Resources/zh-Hans.lproj/Localizable.strings")
        let enContent = try TestFixture.readRepoFile("Lokii/Lokii/Resources/en.lproj/Localizable.strings")

        let requiredFilterKeys = [
            "filter.title",
            "filter.clearAll",
            "filter.all",
            "filter.type.title",
            "filter.type.code",
            "filter.type.document",
            "filter.type.image",
            "filter.type.media",
            "filter.type.archive",
            "filter.type.folder",
            "filter.size.title",
            "filter.size.over",
            "filter.size.under",
            "filter.size.invalidRange",
            "filter.done",
        ]

        for key in requiredFilterKeys {
            XCTAssertTrue(zhContent.contains("\"\(key)\""), "zh-Hans 必须包含 \(key)")
            XCTAssertTrue(enContent.contains("\"\(key)\""), "en 必须包含 \(key)")
        }

        XCTAssertTrue(zhContent.contains("过滤条件"))
        XCTAssertTrue(enContent.contains("Filters"))
        XCTAssertTrue(zhContent.contains("全部清除"))
        XCTAssertTrue(enContent.contains("Clear All"))
        XCTAssertTrue(zhContent.contains("大小下限大于上限，区间互斥"))
        XCTAssertTrue(enContent.contains("Minimum size exceeds maximum size"))

        let newKeys = [
            "filter.modified.title",
            "filter.modified.any",
            "filter.modified.last7Days",
            "filter.modified.last30Days",
            "filter.modified.olderThan90Days",
            "filter.modified.olderThan180Days",
            "filter.modified.olderThan1Year",
            "filter.modified.custom",
            "search.selection.summary",
            "search.selection.summaryWithFolders",
            "search.selection.summaryWithFoldersTooltip",
            "search.selection.foldersOnly",
            "search.trash.dataless.singleTitle",
            "search.trash.dataless.multiOneTitle",
            "search.trash.dataless.multiTitle",
            "search.trash.dataless.message",
            "search.trash.dataless.confirmButton",
        ]
        for key in newKeys {
            XCTAssertTrue(zhContent.contains("\"\(key)\""), "zh-Hans 必须包含 \(key)")
            XCTAssertTrue(enContent.contains("\"\(key)\""), "en 必须包含 \(key)")
        }
    }

    func testModifiedTimePresetSelectionToggleAndClearAll() {
        var state = FilterState()
        var changeCount = 0
        let binding = Binding(get: { state }, set: { state = $0 })
        let view = FilterPopoverView(
            state: binding,
            onStateChanged: { _ in changeCount += 1 }
        )

        view.selectModifiedTimePreset(.last7Days)
        XCTAssertEqual(state.modifiedTime, .preset(.last7Days))
        XCTAssertEqual(changeCount, 1)

        // 再次点击同一预设应切换回不限
        view.selectModifiedTimePreset(.last7Days)
        XCTAssertNil(state.modifiedTime)
        XCTAssertEqual(changeCount, 2)

        view.selectModifiedTimePreset(.olderThan1Year)
        XCTAssertEqual(state.modifiedTime, .preset(.olderThan1Year))
        view.selectModifiedTimePreset(nil)
        XCTAssertNil(state.modifiedTime)

        state.modifiedTime = .custom(["mtime:2025-01-01..2025-03-31"])
        state.selectedTypes = [.media]
        state.minSizeBytes = 1_000_000_000
        view.clearAll()
        XCTAssertNil(state.modifiedTime)
        XCTAssertTrue(state.isUnconstrained)
    }
}
