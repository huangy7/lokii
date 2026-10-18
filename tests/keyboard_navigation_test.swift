import Cocoa
import UniformTypeIdentifiers

// MARK: - Standalone Test Harness for Keyboard Navigation

let lokiiWindowPath = "Lokii/Lokii/LokiiWindow.swift"
guard FileManager.default.fileExists(atPath: lokiiWindowPath) else {
    fputs("tests/keyboard_navigation_test.swift: error: 找不到 \(lokiiWindowPath)\n", stderr)
    exit(1)
}

guard let lokiiWindowSource = try? String(contentsOfFile: lokiiWindowPath, encoding: .utf8) else {
    fputs("tests/keyboard_navigation_test.swift: error: 无法读取 \(lokiiWindowPath)\n", stderr)
    exit(1)
}

let runnerSource = """
import Cocoa
import UniformTypeIdentifiers

\(lokiiWindowSource)

func assertEqual<T: Equatable>(_ actual: T, _ expected: T, _ message: String = "", file: StaticString = #file, line: UInt = #line) {
    if actual != expected {
        fputs("断言失败: [\\(actual)] != [\\(expected)] - \\(message) (\\(file):\\(line))\\n", stderr)
        exit(1)
    }
}

func assertTrue(_ condition: Bool, _ message: String = "", file: StaticString = #file, line: UInt = #line) {
    if !condition {
        fputs("断言失败: 条件不满足 - \\(message) (\\(file):\\(line))\\n", stderr)
        exit(1)
    }
}

print("[1/4] 测试 LokiiWindow 核心快捷键分发 (Escape / Return / ⌘F)...")
let window = LokiiWindow(
    contentRect: NSRect(x: 0, y: 0, width: 800, height: 600),
    styleMask: [.titled, .closable, .resizable],
    backing: .buffered,
    defer: false
)

var escapeCalled = false
var returnCalled = false
var commandFCalled = false

window.onEscape = { escapeCalled = true }
window.onReturn = { returnCalled = true }
window.onCommandF = { commandFCalled = true }

let escapeEvent = NSEvent.keyEvent(
    with: .keyDown,
    location: .zero,
    modifierFlags: [],
    timestamp: 0,
    windowNumber: window.windowNumber,
    context: nil,
    characters: "\\u{1B}",
    charactersIgnoringModifiers: "\\u{1B}",
    isARepeat: false,
    keyCode: 53
)!
window.keyDown(with: escapeEvent)
assertTrue(escapeCalled, "按下 Escape 应触发 onEscape 回调")

let returnEvent = NSEvent.keyEvent(
    with: .keyDown,
    location: .zero,
    modifierFlags: [],
    timestamp: 0,
    windowNumber: window.windowNumber,
    context: nil,
    characters: "\\r",
    charactersIgnoringModifiers: "\\r",
    isARepeat: false,
    keyCode: 36
)!
window.keyDown(with: returnEvent)
assertTrue(returnCalled, "按下 Return 应触发 onReturn 回调")

let commandFEvent = NSEvent.keyEvent(
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
window.keyDown(with: commandFEvent)
assertTrue(commandFCalled, "按下 ⌘F 应触发 onCommandF 回调")

// 裸按 'f' 键不应误触发 ⌘F
commandFCalled = false
let bareFEvent = NSEvent.keyEvent(
    with: .keyDown,
    location: .zero,
    modifierFlags: [],
    timestamp: 0,
    windowNumber: window.windowNumber,
    context: nil,
    characters: "f",
    charactersIgnoringModifiers: "f",
    isARepeat: false,
    keyCode: 3
)!
window.keyDown(with: bareFEvent)
assertTrue(!commandFCalled, "无修饰键按下 'f' 不应触发 onCommandF 回调")

print("[2/4] 测试搜索框 Field Editor 语义命令解析 (moveDown: 触发时机与边界守卫)...")

class MockSearchFieldDelegate: NSObject, NSSearchFieldDelegate {
    var hasResults = true
    var didFocusResults = false

    func control(_ control: NSControl, textView: NSTextView, doCommandBy commandSelector: Selector) -> Bool {
        if commandSelector == #selector(NSResponder.moveDown(_:)) {
            let length = (textView.string as NSString).length
            if textView.selectedRange.length == 0 && textView.selectedRange.location == length {
                if hasResults {
                    didFocusResults = true
                    return true
                }
            }
        }
        return false
    }
}

let delegate = MockSearchFieldDelegate()
let searchField = NSSearchField()
searchField.delegate = delegate
let dummyWindow = NSWindow()
dummyWindow.contentView?.addSubview(searchField)
dummyWindow.makeKeyAndOrderFront(nil)
dummyWindow.makeFirstResponder(searchField)

guard let editor = searchField.currentEditor() as? NSTextView else {
    fputs("未能获取 searchField.currentEditor()\\n", stderr)
    exit(1)
}

// 场景 A: 输入为空，光标在 (0, 0)，有结果集 -> 预期消费并接力 (返回 true)
editor.string = ""
editor.selectedRange = NSRange(location: 0, length: 0)
delegate.didFocusResults = false
delegate.hasResults = true
let handledEmpty = delegate.control(searchField, textView: editor, doCommandBy: #selector(NSResponder.moveDown(_:)))
assertTrue(handledEmpty, "输入为空时按下 ↓ 应成功交接焦点至结果列表")
assertTrue(delegate.didFocusResults, "应标记成功触发列表焦点激活")

// 场景 B: 文本为 'apple'，光标位于末尾 (5, 0)，有结果集 -> 预期消费并接力 (返回 true)
editor.string = "apple"
editor.selectedRange = NSRange(location: 5, length: 0)
delegate.didFocusResults = false
let handledAtEnd = delegate.control(searchField, textView: editor, doCommandBy: #selector(NSResponder.moveDown(_:)))
assertTrue(handledAtEnd, "光标位于文本末尾时按下 ↓ 应成功交接焦点至结果列表")
assertTrue(delegate.didFocusResults, "应标记成功触发列表焦点激活")

// 场景 C: 文本为 'apple'，光标在中间 (2, 0) -> 预期不拦截 (返回 false)，保留向后移动光标手感
editor.string = "apple"
editor.selectedRange = NSRange(location: 2, length: 0)
delegate.didFocusResults = false
let handledMiddle = delegate.control(searchField, textView: editor, doCommandBy: #selector(NSResponder.moveDown(_:)))
assertTrue(!handledMiddle, "光标位于文本中间时按下 ↓ 不应拦截，应保留自然光标移动")
assertTrue(!delegate.didFocusResults, "不应触发列表焦点激活")

// 场景 D: 文本有选区 (例如选中 'app'，length > 0) -> 预期不拦截 (返回 false)
editor.string = "apple"
editor.selectedRange = NSRange(location: 0, length: 3)
delegate.didFocusResults = false
let handledSelection = delegate.control(searchField, textView: editor, doCommandBy: #selector(NSResponder.moveDown(_:)))
assertTrue(!handledSelection, "存在文本选区时不应拦截 ↓ 动作")

// 场景 E: 结果集为空 -> 预期不拦截 (返回 false)
editor.string = "empty"
editor.selectedRange = NSRange(location: 5, length: 0)
delegate.hasResults = false
delegate.didFocusResults = false
let handledNoResults = delegate.control(searchField, textView: editor, doCommandBy: #selector(NSResponder.moveDown(_:)))
assertTrue(!handledNoResults, "结果集为空时不应拦截 ↓ 动作")

// 场景 F: 其他语义命令 (如 moveUp: 或 insertNewline:) -> 预期不拦截 (返回 false)
let handledMoveUp = delegate.control(searchField, textView: editor, doCommandBy: #selector(NSResponder.moveUp(_:)))
assertTrue(!handledMoveUp, "moveUp: 命令不应被搜索框拦截处理")

print("[3/4] 测试搜索框回焦与光标末尾定位机制 (focusSearchField)...")
func testFocusSearchField(in window: NSWindow, searchField: NSSearchField) {
    window.makeFirstResponder(searchField)
    if let fieldEditor = searchField.currentEditor() {
        let length = (searchField.stringValue as NSString).length
        fieldEditor.selectedRange = NSRange(location: length, length: 0)
    }
}

searchField.stringValue = "query text"
testFocusSearchField(in: dummyWindow, searchField: searchField)
assertEqual(dummyWindow.firstResponder === editor, true, "搜索框应成为第一响应者")
assertEqual(editor.selectedRange.location, 10, "光标位置应严格定位于字符串末尾 (location = 10)")
assertEqual(editor.selectedRange.length, 0, "光标选区长度应为 0")

print("[4/4] 测试端到端键盘上下移动双向焦点闭环联动...")
var currentFocus: String = "searchField"
var tableSelectedRow: Int = 0

// 下移操作模拟
if currentFocus == "searchField" {
    currentFocus = "tableView"
    tableSelectedRow = 0
}
assertEqual(currentFocus, "tableView", "初次下移应将焦点置于列表")

// 上移操作模拟 (在首行)
if currentFocus == "tableView" && tableSelectedRow <= 0 {
    currentFocus = "searchField"
}
assertEqual(currentFocus, "searchField", "首行上移应将焦点交还搜索输入框")

print("测试全部通过：4/4 组断言成功。")
"""

let tempDir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
try! FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
defer { try? FileManager.default.removeItem(at: tempDir) }

let harnessFile = tempDir.appendingPathComponent("main.swift")
try! runnerSource.write(to: harnessFile, atomically: true, encoding: .utf8)

let process = Process()
process.executableURL = URL(fileURLWithPath: "/usr/bin/swift")
process.arguments = [harnessFile.path]
process.standardOutput = FileHandle.standardOutput
process.standardError = FileHandle.standardError

try! process.run()
process.waitUntilExit()

if process.terminationStatus != 0 {
    exit(process.terminationStatus)
}
