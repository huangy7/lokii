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

// 测试纯大写判定（用于 0ms 前端即时本地推导）
func hasEffectiveUppercase(query: String) -> Bool {
    var terms: [String] = []
    for word in query.split(separator: " ") {
        let s = String(word)
        if s.starts(with: "in:") || s.starts(with: "type:") || s.starts(with: "ext:") || s.starts(with: "size:") {
            continue
        }
        if s.contains("/") {
            if s.hasSuffix("/") { continue }
            if let last = s.split(separator: "/").last, !last.isEmpty {
                terms.append(String(last))
            }
            continue
        }
        if s.starts(with: ".") && !s.starts(with: "..") {
            continue
        }
        terms.append(s)
    }
    return terms.contains { term in
        term.contains { $0.isUppercase }
    }
}

assert(hasEffectiveUppercase(query: "report") == false)
assert(hasEffectiveUppercase(query: "Report") == true)
assert(hasEffectiveUppercase(query: "in:/Users/Downloads report") == false)
assert(hasEffectiveUppercase(query: "ext:PNG report") == false)
assert(hasEffectiveUppercase(query: "ext:png Report") == true)
assert(hasEffectiveUppercase(query: "type:pdf /path/to/File") == true)
assert(hasEffectiveUppercase(query: "type:pdf /path/to/file") == false)
assert(hasEffectiveUppercase(query: ".git report") == false)
assert(hasEffectiveUppercase(query: ".GIT Report") == true)

print("Swift case sensitivity tests passed!")

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

assert(swSrc.contains("regexModeButton = RegexModeButton()"))
assert(swSrc.contains("regexModeButtonClicked"))
assert(swSrc.contains("updateRegexModeButtonState"))
assert(swSrc.contains("hasRegexSugar"))
assert(regexBtnSrc.contains("class RegexModeButton: InlinePillButton"))
assert(btnSrc.contains("acceptsFirstResponder: Bool { false }"))
assert(btnSrc.contains("canBecomeKeyView: Bool { false }"))
assert(btnSrc.contains("needsPanelToBecomeKey: Bool { false }"))
assert(btnSrc.contains("func pulse()"))
assert(zhSrc.contains("search.regex.inactive"))
assert(zhSrc.contains("search.regex.autoActive"))
assert(zhSrc.contains("search.regex.locked"))
assert(enSrc.contains("search.regex.inactive"))
assert(enSrc.contains("search.regex.autoActive"))
assert(enSrc.contains("search.regex.locked"))

print("Swift regex mode & syntax sugar tests passed!")
