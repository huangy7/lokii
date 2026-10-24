import Cocoa
import SwiftUI

// MARK: - Standalone Test Harness for FilterPopoverController

let coordinatorPath = "Lokii/Lokii/FilterQuerySyncCoordinator.swift"
let popoverViewPath = "Lokii/Lokii/FilterPopoverView.swift"
let controllerPath = "Lokii/Lokii/FilterPopoverController.swift"
let lokiiWindowPath = "Lokii/Lokii/LokiiWindow.swift"
let searchWindowPath = "Lokii/Lokii/SearchWindow.swift"

guard FileManager.default.fileExists(atPath: coordinatorPath),
      FileManager.default.fileExists(atPath: popoverViewPath),
      FileManager.default.fileExists(atPath: controllerPath),
      FileManager.default.fileExists(atPath: lokiiWindowPath),
      FileManager.default.fileExists(atPath: searchWindowPath) else {
    fputs("tests/filter_popover_controller_test.swift: error: 找不到源文件\n", stderr)
    exit(1)
}

guard let coordinatorSource = try? String(contentsOfFile: coordinatorPath, encoding: .utf8),
      let popoverViewSource = try? String(contentsOfFile: popoverViewPath, encoding: .utf8),
      let controllerSource = try? String(contentsOfFile: controllerPath, encoding: .utf8),
      let lokiiWindowSource = try? String(contentsOfFile: lokiiWindowPath, encoding: .utf8),
      let searchWindowSource = try? String(contentsOfFile: searchWindowPath, encoding: .utf8) else {
    fputs("tests/filter_popover_controller_test.swift: error: 无法读取源文件内容\n", stderr)
    exit(1)
}

let mockFfiSource = """
public enum FfiFileType: String, CaseIterable, Hashable, Sendable {
    case all
    case document
    case code
    case image
    case media
    case archive
    case folder
}

func L(_ key: String, comment: String = "") -> String {
    NSLocalizedString(key, comment: comment)
}
"""

let cleanCoordinatorSource = coordinatorSource
    .replacingOccurrences(of: "import Cocoa", with: "")

let cleanPopoverViewSource = popoverViewSource
    .replacingOccurrences(of: "import SwiftUI", with: "")
    .replacingOccurrences(of: "import AppKit", with: "")

let cleanControllerSource = controllerSource
    .replacingOccurrences(of: "import SwiftUI", with: "")
    .replacingOccurrences(of: "import AppKit", with: "")

let cleanLokiiWindowSource = lokiiWindowSource
    .replacingOccurrences(of: "import AppKit", with: "")

let runnerSource = """
import Cocoa
import SwiftUI

\(mockFfiSource)

\(cleanCoordinatorSource)

\(cleanPopoverViewSource)

\(cleanControllerSource)

\(cleanLokiiWindowSource)

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

// 确保主运行循环已初始化以支持 AppKit 窗口与浮层装配
_ = NSApplication.shared

print("[1/5] 测试 FilterPopoverController 初始化与初始属性契约...")
do {
    let controller = FilterPopoverController()
    assertEqual(controller.isShown, false, "初始化后浮层应处于隐藏未展示状态")
    assertEqual(controller.currentState.activeCount, 0, "初始过滤状态应完全无约束")
    assertTrue(controller.currentState.isUnconstrained, "初始过滤状态应判定为 isUnconstrained")
}

print("[2/5] 测试 FilterPopoverController 配置与双向语法回填联动...")
do {
    let controller = FilterPopoverController(animates: false)
    let searchField = NSSearchField()
    searchField.stringValue = "query type:code size:>10m"

    var lastSearchedQuery: String? = nil
    var lastStateChange: FilterState? = nil

    controller.configure(
        searchField: searchField,
        onSearch: { query in
            lastSearchedQuery = query
        },
        onFilterStateChanged: { state in
            lastStateChange = state
        }
    )

    // 创建测试宿主窗口与锚定按钮（放置在窗口顶部区域，确保下方空间充足）
    let window = NSWindow(contentRect: NSRect(x: 300, y: 500, width: 600, height: 500),
                          styleMask: [.titled, .closable],
                          backing: .buffered,
                          defer: false)
    let anchorButton = NSButton()
    anchorButton.frame = NSRect(x: 480, y: 440, width: 38, height: 22)
    window.contentView?.addSubview(anchorButton)
    window.orderFront(nil)

    // 弹出浮层：应从 searchField 反向回填状态，且首次弹出时浮层顶边紧贴按钮底边（不向上顶穿工具栏）
    controller.show(relativeTo: anchorButton)
    RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.05))
    assertEqual(controller.isShown, true, "show() 后浮层应处于展示状态")
    let btnScreen = window.convertToScreen(anchorButton.convert(anchorButton.bounds, to: nil))
    if let popWin = NSApp.windows.first(where: { $0 !== window && $0.isVisible && String(describing: type(of: $0)).contains("Popover") }) {
        assertTrue(abs(popWin.frame.maxY - btnScreen.minY) <= 2.0, "首次弹出时浮层顶边应紧贴按钮底边，不得因未预测量高度而向上漂移")
    }

    let currentState = controller.currentState
    assertTrue(currentState.selectedTypes.contains(.code), "逆向解析回填应识别 .code 类型")
    assertEqual(currentState.minSizeBytes, 10_000_000, "逆向解析回填应识别 10MB 下限字节数")
    assertEqual(currentState.activeCount, 2, "激活过滤维度数量应准确为 2")

    // 关闭浮层
    controller.close()
    RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.1))
    assertEqual(controller.isShown, false, "close() 后浮层应关闭")

    // toggle 动作
    controller.toggle(relativeTo: anchorButton)
    RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.05))
    assertEqual(controller.isShown, true, "toggle() 在关闭状态下应执行展开")
    controller.toggle(relativeTo: anchorButton)
    RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.1))
    assertEqual(controller.isShown, false, "toggle() 在展开状态下应执行收起")

    // 测试边界防护：当锚定视图无宿主 window 时，show() 必须安全退出并不弹出浮层
    let detachedButton = NSButton()
    let unattachedController = FilterPopoverController(animates: false)
    unattachedController.show(relativeTo: detachedButton)
    assertEqual(unattachedController.isShown, false, "当锚定视图无 window 时应触发边界守护不展示浮层")

    // 测试焦点归还机制：浮层关闭后主动交接焦点至 searchField
    window.contentView?.addSubview(searchField)
    window.makeKeyAndOrderFront(nil)
    controller.show(relativeTo: anchorButton)
    assertEqual(controller.isShown, true, "show() 展示浮层以供焦点恢复测试")
    controller.close()
    RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.1))
    assertEqual(controller.isShown, false, "浮层收拢完毕")
    assertTrue(window.firstResponder === searchField || window.firstResponder === searchField.currentEditor(), "浮层关闭后应将焦点归还给 searchField")
}

print("[3/5] 测试外部输入框变化驱动浮层实时同步 (syncStateFromQuery)...")
do {
    let controller = FilterPopoverController()
    var capturedState: FilterState? = nil
    let searchField = NSSearchField()

    controller.configure(
        searchField: searchField,
        onSearch: { _ in },
        onFilterStateChanged: { state in
            capturedState = state
        }
    )

    controller.syncStateFromQuery("document ext:pdf type:doc,img size:1m..50m")
    assertEqual(capturedState?.selectedTypes.contains(.document), true, "文档类型应被同步识别")
    assertEqual(capturedState?.selectedTypes.contains(.image), true, "图片类型应被同步识别")
    assertEqual(capturedState?.minSizeBytes, 1_000_000, "1MB 下限应被同步识别")
    assertEqual(capturedState?.maxSizeBytes, 50_000_000, "50MB 上限应被同步识别")
    assertEqual(capturedState?.activeCount, 2, "激活过滤维度数量应准确计算为 2 (类型、大小)")

    // 测试未有暂存状态时，flushPendingStateIfNeeded 应返回 false
    let flushedWithoutPending = controller.flushPendingStateIfNeeded()
    assertEqual(flushedWithoutPending, false, "无暂存状态时 flushPendingStateIfNeeded 应返回 false")
}

print("[4/5] 测试 LokiiWindow 键盘事件路由与 ⌥F (Option+F) 快捷键拦截...")
do {
    let window = LokiiWindow(
        contentRect: NSRect(x: 100, y: 100, width: 800, height: 600),
        styleMask: [.titled, .closable, .resizable],
        backing: .buffered,
        defer: false
    )

    var optionFCalled = false
    var commandFCalled = false
    var escapeCalled = false

    window.onOptionF = { optionFCalled = true }
    window.onCommandF = { commandFCalled = true }
    window.onEscape = { escapeCalled = true }

    // 1. 模拟 Option+F 按键 (performKeyEquivalent 优先级高于普通文本编辑)
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

    let handledKeyEquivalent = window.performKeyEquivalent(with: optionFEvent)
    assertTrue(handledKeyEquivalent, "⌥F 应被 performKeyEquivalent 成功消费并返回 true")
    assertTrue(optionFCalled, "⌥F 应触发 onOptionF 回调")

    // 2. 模拟 Command+Option+F 复合修饰键：不应误触发纯 ⌥F
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
    let handledCmdOption = window.performKeyEquivalent(with: cmdOptionFEvent)
    assertEqual(handledCmdOption, false, "带有 ⌘ 的复合修饰键不应被 onOptionF 消费")
    assertEqual(optionFCalled, false, "带有 ⌘ 的复合修饰键不应触发 onOptionF 回调")

    // 3. 模拟普通 ⌘F 正常生效
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
    assertTrue(commandFCalled, "⌘F 快捷键正常路由至 onCommandF")
}

print("FilterPopoverController 运行时装配与逻辑测试全部通过！")
"""

let tempDir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
try! FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
defer { try? FileManager.default.removeItem(at: tempDir) }

let harnessFile = tempDir.appendingPathComponent("main.swift")
try! runnerSource.write(to: harnessFile, atomically: true, encoding: .utf8)

let binFile = tempDir.appendingPathComponent("runner")
let compileProcess = Process()
compileProcess.executableURL = URL(fileURLWithPath: "/usr/bin/swiftc")
compileProcess.arguments = ["-suppress-warnings", harnessFile.path, "-o", binFile.path]
compileProcess.standardOutput = FileHandle.standardOutput
compileProcess.standardError = FileHandle.standardError

try! compileProcess.run()
compileProcess.waitUntilExit()

guard compileProcess.terminationStatus == 0 else {
    exit(compileProcess.terminationStatus)
}

let runProcess = Process()
runProcess.executableURL = binFile
runProcess.standardOutput = FileHandle.standardOutput
runProcess.standardError = FileHandle.standardError

try! runProcess.run()
runProcess.waitUntilExit()

guard runProcess.terminationStatus == 0 else {
    exit(runProcess.terminationStatus)
}

print("[5/5] 测试 SearchWindow 胶囊按钮挂载、布局约束与多语言完整性...")
func assertSearchWindowTrue(_ condition: Bool, _ message: String) {
    if !condition {
        fputs("断言失败: 条件不满足 - \(message)\n", stderr)
        exit(1)
    }
}

assertSearchWindowTrue(searchWindowSource.contains("filterPopoverController"), "SearchWindow 必须集成 filterPopoverController")
assertSearchWindowTrue(searchWindowSource.contains("filterButton"), "SearchWindow 必须添加 filterButton 过滤胶囊按钮")
assertSearchWindowTrue(searchWindowSource.contains("window.onOptionF"), "SearchWindow 必须注册 window.onOptionF 快捷键")
assertSearchWindowTrue(searchWindowSource.contains("filterButtonClicked"), "SearchWindow 必须响应 filterButtonClicked 按钮动作")
assertSearchWindowTrue(searchWindowSource.contains("greaterThanOrEqualToConstant: 360"), "searchContainer 最小宽度需扩充至 >= 360 pt")
assertSearchWindowTrue(searchWindowSource.contains("line.3.horizontal.decrease"), "filterButton 图标必须使用 line.3.horizontal.decrease")
assertSearchWindowTrue(searchWindowSource.contains("filterButton.isBordered = false"), "filterButton 必须采用扁平化无外边框样式")
assertSearchWindowTrue(searchWindowSource.contains("if self.filterPopoverController.isShown"), "SearchWindow Esc 按键必须优先检查浮层展开状态")
assertSearchWindowTrue(searchWindowSource.contains("self.filterPopoverController.close()"), "SearchWindow Esc 按键在浮层展开时必须优先收起浮层")
assertSearchWindowTrue(searchWindowSource.contains("self.loadDefaultResults()"), "SearchWindow 空查询回调时必须调用 loadDefaultResults 回退默认结果")
assertSearchWindowTrue(controllerSource.contains("guard view.window != nil else { return }"), "FilterPopoverController 必须包含 view.window 边界防护")
assertSearchWindowTrue(controllerSource.contains("restoreSearchFieldFocus"), "FilterPopoverController 必须包含 restoreSearchFieldFocus 焦点归还")
assertSearchWindowTrue(controllerSource.contains("popoverDidClose"), "FilterPopoverController 必须实现 popoverDidClose 代理方法")

let zhPath = "Lokii/Lokii/Resources/zh-Hans.lproj/Localizable.strings"
let enPath = "Lokii/Lokii/Resources/en.lproj/Localizable.strings"

guard let zhContent = try? String(contentsOfFile: zhPath, encoding: .utf8),
      let enContent = try? String(contentsOfFile: enPath, encoding: .utf8) else {
    fputs("无法读取多语言资源文件\n", stderr)
    exit(1)
}

assertSearchWindowTrue(zhContent.contains("\"filter.button.tooltip\""), "zh-Hans 必须包含 filter.button.tooltip")
assertSearchWindowTrue(enContent.contains("\"filter.button.tooltip\""), "en 必须包含 filter.button.tooltip")
assertSearchWindowTrue(zhContent.contains("\"filter.button.tooltip.active\""), "zh-Hans 必须包含 filter.button.tooltip.active")
assertSearchWindowTrue(enContent.contains("\"filter.button.tooltip.active\""), "en 必须包含 filter.button.tooltip.active")

print("FilterPopoverController 全部测试通过！")
