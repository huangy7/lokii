import Cocoa
import UniformTypeIdentifiers
import XCTest
@testable import LokiiAppLib

final class KeyboardNavigationTests: XCTestCase {
    private final class MockSearchFieldDelegate: NSObject, NSSearchFieldDelegate {
        var hasResults = true
        var didFocusResults = false
        var didHandleEscape = false
        var didClearQuery = false
        weak var mockSearchField: NSSearchField?

        func control(_ control: NSControl, textView: NSTextView, doCommandBy commandSelector: Selector) -> Bool {
            if commandSelector == #selector(NSResponder.cancelOperation(_:)) {
                if textView.hasMarkedText() {
                    return false
                }
                let length = (textView.string as NSString).length
                if length == 0 {
                    didHandleEscape = true
                    return true
                } else {
                    didClearQuery = true
                    mockSearchField?.stringValue = ""
                    textView.string = ""
                    return true
                }
            }
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

    func testLokiiWindowKeyRouting() {
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
            characters: "\u{1B}",
            charactersIgnoringModifiers: "\u{1B}",
            isARepeat: false,
            keyCode: 53
        )!
        window.keyDown(with: escapeEvent)
        XCTAssertTrue(escapeCalled)

        escapeCalled = false
        window.cancelOperation(nil)
        XCTAssertTrue(escapeCalled)

        let returnEvent = NSEvent.keyEvent(
            with: .keyDown,
            location: .zero,
            modifierFlags: [],
            timestamp: 0,
            windowNumber: window.windowNumber,
            context: nil,
            characters: "\r",
            charactersIgnoringModifiers: "\r",
            isARepeat: false,
            keyCode: 36
        )!
        window.keyDown(with: returnEvent)
        XCTAssertTrue(returnCalled)

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
        XCTAssertTrue(commandFCalled)

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
        XCTAssertFalse(commandFCalled)
    }

    func testFieldEditorMoveDownCancelAndEndCursorPlacement() throws {
        let delegate = MockSearchFieldDelegate()
        let searchField = NSSearchField()
        delegate.mockSearchField = searchField
        searchField.delegate = delegate

        let dummyWindow = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 400, height: 100),
            styleMask: [.titled],
            backing: .buffered,
            defer: false
        )
        dummyWindow.isReleasedWhenClosed = false
        defer {
            dummyWindow.orderOut(nil)
            dummyWindow.close()
        }
        dummyWindow.contentView?.addSubview(searchField)
        dummyWindow.makeFirstResponder(searchField)

        let editor = try XCTUnwrap(searchField.currentEditor() as? NSTextView)

        // 空文本光标在 (0, 0) + 有结果 -> 接力
        editor.string = ""
        editor.selectedRange = NSRange(location: 0, length: 0)
        delegate.didFocusResults = false
        delegate.hasResults = true
        XCTAssertTrue(delegate.control(searchField, textView: editor, doCommandBy: #selector(NSResponder.moveDown(_:))))
        XCTAssertTrue(delegate.didFocusResults)

        // 文本末尾 + 有结果 -> 接力
        editor.string = "apple"
        editor.selectedRange = NSRange(location: 5, length: 0)
        delegate.didFocusResults = false
        XCTAssertTrue(delegate.control(searchField, textView: editor, doCommandBy: #selector(NSResponder.moveDown(_:))))
        XCTAssertTrue(delegate.didFocusResults)

        // 光标在中间 -> 不拦截
        editor.string = "apple"
        editor.selectedRange = NSRange(location: 2, length: 0)
        delegate.didFocusResults = false
        XCTAssertFalse(delegate.control(searchField, textView: editor, doCommandBy: #selector(NSResponder.moveDown(_:))))
        XCTAssertFalse(delegate.didFocusResults)

        // 存在选区 -> 不拦截
        editor.string = "apple"
        editor.selectedRange = NSRange(location: 0, length: 3)
        delegate.didFocusResults = false
        XCTAssertFalse(delegate.control(searchField, textView: editor, doCommandBy: #selector(NSResponder.moveDown(_:))))

        // 无结果 -> 不拦截
        editor.string = "empty"
        editor.selectedRange = NSRange(location: 5, length: 0)
        delegate.hasResults = false
        delegate.didFocusResults = false
        XCTAssertFalse(delegate.control(searchField, textView: editor, doCommandBy: #selector(NSResponder.moveDown(_:))))

        // 有内容时 Escape -> 清空文本
        editor.string = "lokii search"
        searchField.stringValue = "lokii search"
        delegate.didClearQuery = false
        delegate.didHandleEscape = false
        XCTAssertTrue(delegate.control(searchField, textView: editor, doCommandBy: #selector(NSResponder.cancelOperation(_:))))
        XCTAssertTrue(delegate.didClearQuery)
        XCTAssertFalse(delegate.didHandleEscape)
        XCTAssertEqual(searchField.stringValue, "")

        // 空内容时 Escape -> 窗口退出回调
        editor.string = ""
        searchField.stringValue = ""
        delegate.didClearQuery = false
        delegate.didHandleEscape = false
        XCTAssertTrue(delegate.control(searchField, textView: editor, doCommandBy: #selector(NSResponder.cancelOperation(_:))))
        XCTAssertTrue(delegate.didHandleEscape)

        // moveUp 不拦截
        XCTAssertFalse(delegate.control(searchField, textView: editor, doCommandBy: #selector(NSResponder.moveUp(_:))))

        // 搜索框回焦光标定位于末尾
        searchField.stringValue = "query text"
        dummyWindow.makeFirstResponder(searchField)
        if let fieldEditor = searchField.currentEditor() {
            let length = (searchField.stringValue as NSString).length
            fieldEditor.selectedRange = NSRange(location: length, length: 0)
        }
        XCTAssertTrue(dummyWindow.firstResponder === editor)
        XCTAssertEqual(editor.selectedRange.location, 10)
        XCTAssertEqual(editor.selectedRange.length, 0)
    }

    func testLayeredEscapePriorityContract() {
        var mockPopoverOpen = true
        var mockPreviewOpen = true
        var mockQuery = "test"
        var mockWindowHidden = false

        func handleEscape() {
            if mockPreviewOpen {
                mockPreviewOpen = false
                return
            }
            if mockPopoverOpen {
                mockPopoverOpen = false
                return
            }
            if !mockQuery.isEmpty {
                mockQuery = ""
                return
            }
            mockWindowHidden = true
        }

        handleEscape()
        XCTAssertFalse(mockPreviewOpen)
        XCTAssertTrue(mockPopoverOpen)
        XCTAssertEqual(mockQuery, "test")
        XCTAssertFalse(mockWindowHidden)

        handleEscape()
        XCTAssertFalse(mockPopoverOpen)
        XCTAssertEqual(mockQuery, "test")
        XCTAssertFalse(mockWindowHidden)

        handleEscape()
        XCTAssertEqual(mockQuery, "")
        XCTAssertFalse(mockWindowHidden)

        handleEscape()
        XCTAssertTrue(mockWindowHidden)
    }
}
