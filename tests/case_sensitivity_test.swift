import Foundation

// 测试三态流转逻辑
enum CaseMode { case auto, sensitive, insensitive }

func nextMode(_ mode: CaseMode) -> CaseMode {
    switch mode {
    case .auto: return .sensitive
    case .sensitive: return .insensitive
    case .insensitive: return .auto
    }
}

assert(nextMode(.auto) == .sensitive)
assert(nextMode(.sensitive) == .insensitive)
assert(nextMode(.insensitive) == .auto)

// 从 SearchWindow.swift 实时提取并测试真正的 hasEffectiveUppercase 实现
let searchWindowSrcPath = "Lokii/Lokii/SearchWindow.swift"
guard let searchWindowSrc = try? String(contentsOfFile: searchWindowSrcPath, encoding: .utf8),
      let fnStart = searchWindowSrc.range(of: "    static func hasEffectiveUppercase(query: String) -> Bool {"),
      let fnEnd = searchWindowSrc.range(of: "\n    // MARK: - Fuzzy & Regex Mode Actions & State", range: fnStart.upperBound..<searchWindowSrc.endIndex) else {
    fputs("tests/case_sensitivity_test.swift: error: 无法从 SearchWindow.swift 提取 hasEffectiveUppercase\n", stderr)
    exit(1)
}
let extractedFn = String(searchWindowSrc[fnStart.lowerBound..<fnEnd.lowerBound])
    .replacingOccurrences(of: "    static func hasEffectiveUppercase", with: "func hasEffectiveUppercase")

let runnerScript = """
import Foundation

\(extractedFn)

assert(hasEffectiveUppercase(query: "report") == false)
assert(hasEffectiveUppercase(query: "Report") == true)
assert(hasEffectiveUppercase(query: "in:/Users/Downloads report") == false)
assert(hasEffectiveUppercase(query: "ext:PNG report") == false)
assert(hasEffectiveUppercase(query: "ext:png Report") == true)
assert(hasEffectiveUppercase(query: "type:pdf /path/to/File") == true)
assert(hasEffectiveUppercase(query: "type:pdf /path/to/file") == false)
assert(hasEffectiveUppercase(query: ".git report") == false)
assert(hasEffectiveUppercase(query: ".GIT Report") == true)

// 引号保护、否定词忽略与正则转义兼容断言
assert(hasEffectiveUppercase(query: "!Test") == false, "!Test negated token must not trigger smart case")
assert(hasEffectiveUppercase(query: "!\\"Test\\"") == false, "negated quoted phrase must not trigger smart case")
assert(hasEffectiveUppercase(query: "\\"Test\\"") == true, "positive quoted phrase must preserve uppercase")
assert(hasEffectiveUppercase(query: "main !Test") == false, "negative uppercase alongside lowercase positive stays insensitive")
assert(hasEffectiveUppercase(query: "Main !Test") == true, "positive uppercase still triggers smart case")
assert(hasEffectiveUppercase(query: "\\"type:PDF\\"") == true, "quoted filter-like text stays literal and triggers smart case")
assert(hasEffectiveUppercase(query: "\\"in:Dir\\"") == true, "quoted in: prefix stays literal and triggers smart case")
assert(hasEffectiveUppercase(query: "\\"foo/Bar\\"") == true, "quoted slash stays literal")
assert(hasEffectiveUppercase(query: "regex:\\\\D+ !Skip") == false, "regex escapes and negation must not trigger smart case")
assert(hasEffectiveUppercase(query: "regex:^Main !skip") == true, "regex literal uppercase triggers smart case")

print("Swift case sensitivity tests passed!")
"""

let tmpCaseDir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
try! FileManager.default.createDirectory(at: tmpCaseDir, withIntermediateDirectories: true)
defer { try? FileManager.default.removeItem(at: tmpCaseDir) }
let tmpCaseMain = tmpCaseDir.appendingPathComponent("main.swift")
try! runnerScript.write(to: tmpCaseMain, atomically: true, encoding: .utf8)
let caseProc = Process()
caseProc.executableURL = URL(fileURLWithPath: "/usr/bin/swift")
caseProc.arguments = [tmpCaseMain.path]
caseProc.standardOutput = FileHandle.standardOutput
caseProc.standardError = FileHandle.standardError
try! caseProc.run()
caseProc.waitUntilExit()
if caseProc.terminationStatus != 0 {
    exit(caseProc.terminationStatus)
}

// MARK: - Regex Mode & Syntax Sugar Tests

enum RegexVisualState: Equatable {
    case inactive
    case autoActive
    case locked
}

func hasRegexSugar(query: String) -> Bool {
    let trimmed = query.trimmingCharacters(in: .whitespaces)
    if trimmed.hasPrefix("regex:") { return true }
    guard trimmed.hasPrefix("/"), trimmed.count >= 2 else { return false }
    // Check if closed by unescaped slash
    let chars = Array(trimmed)
    var i = chars.count - 1
    // Check for trailing 'i' or 'I'
    if i >= 1 && (chars[i] == "i" || chars[i] == "I") {
        i -= 1
    }
    guard i >= 1 && chars[i] == "/" else { return false }
    var backslashCount = 0
    var j = i - 1
    while j >= 0 && chars[j] == "\\" {
        backslashCount += 1
        j -= 1
    }
    guard backslashCount % 2 == 0 else { return false }

    // Intermediate unescaped slashes indicate a directory path rather than a regex pattern
    var k = 1
    var intermediateBackslashes = 0
    while k < i {
        if chars[k] == "/" && intermediateBackslashes % 2 == 0 {
            return false
        }
        if chars[k] == "\\" {
            intermediateBackslashes += 1
        } else {
            intermediateBackslashes = 0
        }
        k += 1
    }
    return true
}

assert(hasRegexSugar(query: "") == false)
assert(hasRegexSugar(query: "/") == false)
assert(hasRegexSugar(query: "//") == true)
assert(hasRegexSugar(query: "/[a-z]+/") == true)
assert(hasRegexSugar(query: "/[a-z]+/i") == true)
assert(hasRegexSugar(query: "/[a-z]+/I") == true)
assert(hasRegexSugar(query: "regex:foo") == true)
assert(hasRegexSugar(query: "  /pattern/  ") == true)
assert(hasRegexSugar(query: "/foo\\/bar/") == true)
assert(hasRegexSugar(query: "/unclosed") == false)
assert(hasRegexSugar(query: "/escaped\\/") == false)
assert(hasRegexSugar(query: "/usr/local/bin/") == false)
assert(hasRegexSugar(query: "plain text") == false)

// Verify SearchWindow.swift and Localizable.strings source invariants
let searchWindowPath = "Lokii/Lokii/SearchWindow.swift"
let pillButtonPath = "Lokii/Lokii/InlinePillButton.swift"
let regexButtonPath = "Lokii/Lokii/RegexModeButton.swift"
let zhPath = "Lokii/Lokii/Resources/zh-Hans.lproj/Localizable.strings"
let enPath = "Lokii/Lokii/Resources/en.lproj/Localizable.strings"

guard let swSrc = try? String(contentsOfFile: searchWindowPath, encoding: .utf8),
      let btnSrc = try? String(contentsOfFile: pillButtonPath, encoding: .utf8),
      let regexBtnSrc = try? String(contentsOfFile: regexButtonPath, encoding: .utf8),
      let zhSrc = try? String(contentsOfFile: zhPath, encoding: .utf8),
      let enSrc = try? String(contentsOfFile: enPath, encoding: .utf8) else {
    fputs("无法读取源文件或多语言资源\n", stderr)
    exit(1)
}

assert(swSrc.contains("fuzzyModeButton = FuzzyModeButton()"))
assert(swSrc.contains("fuzzyModeButtonClicked"))
assert(swSrc.contains("updateFuzzyModeButtonState"))
assert(swSrc.contains("regexModeButton = RegexModeButton()"))
assert(swSrc.contains("regexModeButtonClicked"))
assert(swSrc.contains("updateRegexModeButtonState"))
assert(swSrc.contains("hasRegexSugar"))
assert(regexBtnSrc.contains("class RegexModeButton: InlinePillButton"))
assert(btnSrc.contains("acceptsFirstResponder: Bool { false }"))
assert(btnSrc.contains("canBecomeKeyView: Bool { false }"))
assert(btnSrc.contains("needsPanelToBecomeKey: Bool { false }"))
assert(btnSrc.contains("func pulse()"))
assert(zhSrc.contains("search.fuzzy.inactive"))
assert(zhSrc.contains("search.fuzzy.locked"))
assert(enSrc.contains("search.fuzzy.inactive"))
assert(enSrc.contains("search.fuzzy.locked"))
assert(zhSrc.contains("search.regex.inactive"))
assert(zhSrc.contains("search.regex.autoActive"))
assert(zhSrc.contains("search.regex.locked"))
assert(enSrc.contains("search.regex.inactive"))
assert(enSrc.contains("search.regex.autoActive"))
assert(enSrc.contains("search.regex.locked"))

print("Swift regex & fuzzy mode tests passed!")
