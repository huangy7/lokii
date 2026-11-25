import Cocoa
import XCTest
@testable import LokiiAppLib

final class ShortcutRecorderTests: XCTestCase {
    func testRecorderCocoaInvariantsAndLocalization() throws {
        let recorderSource = try TestFixture.readRepoFile("Lokii/Lokii/KeyboardShortcuts/RecorderCocoa.swift")

        XCTAssertFalse(recorderSource.contains("class RecorderCocoa: NSSearchField"))
        XCTAssertTrue(
            recorderSource.contains("class RecorderCocoa: NSControl")
                || recorderSource.contains("class RecorderCocoa: NSView")
        )
        XCTAssertFalse(recorderSource.contains("NSSearchFieldCell"))
        XCTAssertFalse(recorderSource.contains("searchButtonCell"))
        XCTAssertFalse(recorderSource.contains("cancelButtonCell"))
        XCTAssertFalse(recorderSource.contains("controlTextDidChange"))
        XCTAssertFalse(recorderSource.contains("controlTextDidEndEditing"))
        XCTAssertFalse(recorderSource.contains("hideCaret"))
        XCTAssertTrue(
            recorderSource.contains("override public var inputContext: NSTextInputContext? { nil }")
                || recorderSource.contains("override var inputContext: NSTextInputContext? { nil }")
        )

        XCTAssertFalse(recorderSource.contains("class KeyCapView"))
        XCTAssertTrue(recorderSource.contains("displayLabel"))
        XCTAssertTrue(recorderSource.contains("clearButton"))
        XCTAssertTrue(recorderSource.contains("addLocalMonitorForEvents"))
        XCTAssertTrue(recorderSource.contains("removeMonitor"))

        XCTAssertTrue(recorderSource.contains("flagsChanged"))
        XCTAssertTrue(recorderSource.contains("currentModifiers"))
        XCTAssertTrue(recorderSource.contains("isRecording"))
        XCTAssertTrue(recorderSource.contains("KeyboardShortcuts.isEnabled = false"))

        XCTAssertTrue(recorderSource.contains("kVK_Escape") || recorderSource.contains("53"))
        XCTAssertTrue(recorderSource.contains("kVK_Delete") || recorderSource.contains("51"))

        let zhContent = try TestFixture.readRepoFile("Lokii/Lokii/Resources/zh-Hans.lproj/Localizable.strings")
        let enContent = try TestFixture.readRepoFile("Lokii/Lokii/Resources/en.lproj/Localizable.strings")

        for key in ["record_shortcut", "press_shortcut", "shortcut.clear", "space_key"] {
            XCTAssertTrue(zhContent.contains("\"\(key)\""), "zh-Hans 必须包含 \(key)")
            XCTAssertTrue(enContent.contains("\"\(key)\""), "en 必须包含 \(key)")
        }

        XCTAssertTrue(zhContent.contains("\"space_key\" = \"Space\";"))
        XCTAssertTrue(enContent.contains("\"space_key\" = \"Space\";"))
    }
}
