import Foundation
import XCTest
@testable import LokiiAppLib

final class CaseSensitivityTests: XCTestCase {
    func testSmartCaseHasEffectiveUppercase() {
        XCTAssertFalse(SearchWindow.hasEffectiveUppercase(query: "report"))
        XCTAssertTrue(SearchWindow.hasEffectiveUppercase(query: "Report"))
        XCTAssertFalse(SearchWindow.hasEffectiveUppercase(query: "in:/Users/Downloads report"))
        XCTAssertFalse(SearchWindow.hasEffectiveUppercase(query: "ext:PNG report"))
        XCTAssertTrue(SearchWindow.hasEffectiveUppercase(query: "ext:png Report"))
        XCTAssertTrue(SearchWindow.hasEffectiveUppercase(query: "type:pdf /path/to/File"))
        XCTAssertFalse(SearchWindow.hasEffectiveUppercase(query: "type:pdf /path/to/file"))
        XCTAssertFalse(SearchWindow.hasEffectiveUppercase(query: ".git report"))
        XCTAssertTrue(SearchWindow.hasEffectiveUppercase(query: ".GIT Report"))

        // 引号保护、否定词忽略与正则转义兼容断言
        XCTAssertFalse(SearchWindow.hasEffectiveUppercase(query: "!Test"))
        XCTAssertFalse(SearchWindow.hasEffectiveUppercase(query: "!\"Test\""))
        XCTAssertTrue(SearchWindow.hasEffectiveUppercase(query: "\"Test\""))
        XCTAssertFalse(SearchWindow.hasEffectiveUppercase(query: "main !Test"))
        XCTAssertTrue(SearchWindow.hasEffectiveUppercase(query: "Main !Test"))
        XCTAssertTrue(SearchWindow.hasEffectiveUppercase(query: "\"type:PDF\""))
        XCTAssertTrue(SearchWindow.hasEffectiveUppercase(query: "\"in:Dir\""))
        XCTAssertTrue(SearchWindow.hasEffectiveUppercase(query: "\"foo/Bar\""))
        XCTAssertFalse(SearchWindow.hasEffectiveUppercase(query: "regex:\\D+ !Skip"))
        XCTAssertTrue(SearchWindow.hasEffectiveUppercase(query: "regex:^Main !skip"))
        XCTAssertFalse(SearchWindow.hasEffectiveUppercase(query: "/[A-Z]+/i"))
        XCTAssertTrue(SearchWindow.hasEffectiveUppercase(query: "/[A-Z]+/"))
        XCTAssertTrue(SearchWindow.hasEffectiveUppercase(query: "foo | Bar"))
        XCTAssertFalse(SearchWindow.hasEffectiveUppercase(query: "foo | bar"))
        XCTAssertFalse(SearchWindow.hasEffectiveUppercase(query: "\\D+\\S+", mode: .regex))
        XCTAssertTrue(SearchWindow.hasEffectiveUppercase(query: "Main\\d+", mode: .regex))
    }

    func testRegexSugarDetection() {
        XCTAssertFalse(SearchWindow.hasRegexSugar(query: ""))
        XCTAssertFalse(SearchWindow.hasRegexSugar(query: "/"))
        XCTAssertFalse(SearchWindow.hasRegexSugar(query: "//"))
        XCTAssertFalse(SearchWindow.hasRegexSugar(query: "  /pattern/  "))
        XCTAssertTrue(SearchWindow.hasRegexSugar(query: "/pattern/i"))
        XCTAssertTrue(SearchWindow.hasRegexSugar(query: "/[a-z]+/"))
        XCTAssertTrue(SearchWindow.hasRegexSugar(query: "/[a-z]+/i"))
        XCTAssertTrue(SearchWindow.hasRegexSugar(query: "/[a-z]+/I"))
        XCTAssertTrue(SearchWindow.hasRegexSugar(query: "regex:foo"))
        XCTAssertFalse(SearchWindow.hasRegexSugar(query: "\"regex:foo\""))
        XCTAssertTrue(SearchWindow.hasRegexSugar(query: "  /[a-z]+/  "))
        XCTAssertTrue(SearchWindow.hasRegexSugar(query: "/foo\\/bar/"))
        XCTAssertTrue(SearchWindow.hasRegexSugar(query: "in:src /^test_.*\\.rs/"))
        XCTAssertFalse(SearchWindow.hasRegexSugar(query: "/unclosed"))
        XCTAssertFalse(SearchWindow.hasRegexSugar(query: "/escaped\\/"))
        XCTAssertFalse(SearchWindow.hasRegexSugar(query: "/usr/local/bin/"))
        XCTAssertFalse(SearchWindow.hasRegexSugar(query: "plain text"))
    }

    func testModePillButtonsAndLocalizationInvariants() throws {
        let pillButton = InlinePillButton()
        XCTAssertFalse(pillButton.acceptsFirstResponder)
        XCTAssertFalse(pillButton.canBecomeKeyView)
        XCTAssertFalse(pillButton.needsPanelToBecomeKey)

        let regexButton: InlinePillButton = RegexModeButton()
        XCTAssertFalse(regexButton.acceptsFirstResponder)

        let swSrc = try TestFixture.readRepoFile("Lokii/Lokii/SearchWindow.swift")
        let btnSrc = try TestFixture.readRepoFile("Lokii/Lokii/InlinePillButton.swift")
        let zhSrc = try TestFixture.readRepoFile("Lokii/Lokii/Resources/zh-Hans.lproj/Localizable.strings")
        let enSrc = try TestFixture.readRepoFile("Lokii/Lokii/Resources/en.lproj/Localizable.strings")

        XCTAssertTrue(swSrc.contains("fuzzyModeButton = FuzzyModeButton()"))
        XCTAssertTrue(swSrc.contains("fuzzyModeButtonClicked"))
        XCTAssertTrue(swSrc.contains("updateFuzzyModeButtonState"))
        XCTAssertTrue(swSrc.contains("regexModeButton = RegexModeButton()"))
        XCTAssertTrue(swSrc.contains("regexModeButtonClicked"))
        XCTAssertTrue(swSrc.contains("updateRegexModeButtonState"))
        XCTAssertTrue(swSrc.contains("hasRegexSugar"))
        XCTAssertTrue(btnSrc.contains("func pulse()"))

        for key in [
            "search.fuzzy.inactive",
            "search.fuzzy.locked",
            "search.regex.inactive",
            "search.regex.autoActive",
            "search.regex.locked",
        ] {
            XCTAssertTrue(zhSrc.contains(key), "zh-Hans 缺少 \(key)")
            XCTAssertTrue(enSrc.contains(key), "en 缺少 \(key)")
        }
    }
}
