import Cocoa
import SwiftUI

// MARK: - Standalone Test Harness for FilterPopoverView

let coordinatorPath = "Lokii/Lokii/FilterQuerySyncCoordinator.swift"
let popoverViewPath = "Lokii/Lokii/FilterPopoverView.swift"

guard FileManager.default.fileExists(atPath: coordinatorPath) else {
    fputs("tests/filter_popover_view_test.swift: error: 找不到 \(coordinatorPath)\n", stderr)
    exit(1)
}

guard FileManager.default.fileExists(atPath: popoverViewPath) else {
    fputs("tests/filter_popover_view_test.swift: error: 找不到 \(popoverViewPath)\n", stderr)
    exit(1)
}

guard let coordinatorSource = try? String(contentsOfFile: coordinatorPath, encoding: .utf8),
      let popoverViewSource = try? String(contentsOfFile: popoverViewPath, encoding: .utf8) else {
    fputs("tests/filter_popover_view_test.swift: error: 无法读取源文件\n", stderr)
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

// 清理源文件中重复的 import，避免在拼接代码中产生警告
let cleanCoordinatorSource = coordinatorSource
    .replacingOccurrences(of: "import Cocoa", with: "")

let cleanPopoverViewSource = popoverViewSource
    .replacingOccurrences(of: "import SwiftUI", with: "")
    .replacingOccurrences(of: "import AppKit", with: "")

let runnerSource = """
import Cocoa
import SwiftUI

\(mockFfiSource)

\(cleanCoordinatorSource)

\(cleanPopoverViewSource)

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

print("[1/6] 测试 SizeUnit 大小单位定义、换算乘数与 calculateBytes 数值计算...")
do {
    assertEqual(SizeUnit.bytes.multiplier, 1, "Bytes 乘数应为 1")
    assertEqual(SizeUnit.kb.multiplier, 1_000, "KB 乘数应为 1,000")
    assertEqual(SizeUnit.mb.multiplier, 1_000_000, "MB 乘数应为 1,000,000")
    assertEqual(SizeUnit.gb.multiplier, 1_000_000_000, "GB 乘数应为 1,000,000,000")

    // 整数与单位换算
    assertEqual(FilterPopoverView.calculateBytes(text: "10", unit: .mb), 10_000_000, "10 MB 换算应为 10,000,000")
    assertEqual(FilterPopoverView.calculateBytes(text: "500", unit: .kb), 500_000, "500 KB 换算应为 500,000")
    assertEqual(FilterPopoverView.calculateBytes(text: "2", unit: .gb), 2_000_000_000, "2 GB 换算应为 2,000,000,000")
    assertEqual(FilterPopoverView.calculateBytes(text: "1024", unit: .bytes), 1024, "1024 Bytes 换算应为 1024")

    // 小数与单位换算
    assertEqual(FilterPopoverView.calculateBytes(text: "1.5", unit: .gb), 1_500_000_000, "1.5 GB 换算应为 1,500,000,000")
    assertEqual(FilterPopoverView.calculateBytes(text: "0.5", unit: .mb), 500_000, "0.5 MB 换算应为 500,000")

    // 容错与边界值
    assertTrue(FilterPopoverView.calculateBytes(text: "", unit: .mb) == nil, "空文本应返回 nil")
    assertTrue(FilterPopoverView.calculateBytes(text: "   ", unit: .mb) == nil, "空白文本应返回 nil")
    assertTrue(FilterPopoverView.calculateBytes(text: "abc", unit: .mb) == nil, "非法字符应返回 nil")
    assertTrue(FilterPopoverView.calculateBytes(text: "-10", unit: .mb) == nil, "负数应返回 nil")

    // sanitizeNumberString ASCII 纯数字过滤强化测试
    assertEqual(FilterPopoverView.sanitizeNumberString("123.45"), "123.45", "标准正数应保持原样")
    assertEqual(FilterPopoverView.sanitizeNumberString("1.2.3.4"), "1.234", "多个小数点应仅保留第一个")
    assertEqual(FilterPopoverView.sanitizeNumberString(" 10 MB "), "10", "字母与空格应被彻底剥离")
    assertEqual(FilterPopoverView.sanitizeNumberString("①②²½٣4"), "4", "非 ASCII Unicode 数字字符应被过滤，仅保留 ASCII 0-9")
    assertEqual(FilterPopoverView.sanitizeNumberString(".5"), ".5", "首位小数点应被保留")
}

print("[2/6] 测试 decomposeBytes 字节数逆向呈现解构...")
do {
    let (t1, u1) = FilterPopoverView.decomposeBytes(10_000_000)
    assertEqual(t1, "10")
    assertEqual(u1, .mb)

    let (t2, u2) = FilterPopoverView.decomposeBytes(500_000)
    assertEqual(t2, "500")
    assertEqual(u2, .kb)

    let (t3, u3) = FilterPopoverView.decomposeBytes(2_000_000_000)
    assertEqual(t3, "2")
    assertEqual(u3, .gb)

    let (t4, u4) = FilterPopoverView.decomposeBytes(1_500_000_000)
    assertEqual(t4, "1.5")
    assertEqual(u4, .gb)

    let (t5, u5) = FilterPopoverView.decomposeBytes(1024)
    assertEqual(t5, "1024")
    assertEqual(u5, .bytes)

    let (t6, u6) = FilterPopoverView.decomposeBytes(0)
    assertEqual(t6, "0")
    assertEqual(u6, .bytes)
}

print("[3/6] 测试 FilterPopoverView 初始化与默认状态流转...")
do {
    var state = FilterState()
    var stateChangedCount = 0

    let binding = Binding(get: { state }, set: { state = $0 })
    let view = FilterPopoverView(
        state: binding,
        onStateChanged: { _ in
            stateChangedCount += 1
        }
    )

    assertTrue(view.isAllSelected, "初始无约束状态下 isAllSelected 应为 true")
    assertEqual(FilterPopoverView.fileTypeItems.count, 6, "应提供 6 种具体文件类型")

    // 校验 6 种具体类型与顺序，以及全部采用 SF Symbols 矢量图标（不含任何 Emoji）
    let expectedTypes: [FfiFileType] = [.code, .document, .image, .media, .archive, .folder]
    assertEqual(FilterPopoverView.fileTypeItems.map { $0.type }, expectedTypes, "文件类型列表及顺序应正确")
    for item in FilterPopoverView.fileTypeItems {
        assertTrue(!item.symbolName.isEmpty, "每个文件类型项必须配置非空 SF Symbol 名称")
        assertTrue(NSImage(systemSymbolName: item.symbolName, accessibilityDescription: nil) != nil, "SF Symbol \\(item.symbolName) 必须在系统中有效存在")
    }
}

print("[4/6] 测试文件类型勾选联动与全部（All）总控逻辑...")
do {
    var state = FilterState()
    var lastState: FilterState?
    let binding = Binding(get: { state }, set: { state = $0 })
    let view = FilterPopoverView(state: binding, onStateChanged: { lastState = $0 })

    // 1. 勾选单个类型：代码 (.code)
    view.toggleFileType(.code)
    assertTrue(state.selectedTypes.contains(.code), "勾选后应包含 .code")
    assertEqual(state.selectedTypes.count, 1, "应只选中 1 个类型")
    assertTrue(!view.isAllSelected, "选中特定类型时 isAllSelected 应为 false")
    assertEqual(lastState?.selectedTypes, [.code], "回调通知状态应与 state 一致")

    // 2. 增加勾选：文档 (.document)
    view.toggleFileType(.document)
    assertTrue(state.selectedTypes.contains(.code) && state.selectedTypes.contains(.document), "应同时选中 code 与 document")
    assertEqual(state.selectedTypes.count, 2)
    assertTrue(!view.isAllSelected)

    // 3. 反选取消代码：仅剩文档
    view.toggleFileType(.code)
    assertTrue(!state.selectedTypes.contains(.code), "取消后不应包含 .code")
    assertTrue(state.selectedTypes.contains(.document), "应仍然包含 .document")
    assertTrue(!view.isAllSelected)

    // 4. 取消文档：选定为空集合，恢复完全放行
    view.toggleFileType(.document)
    assertTrue(state.selectedTypes.isEmpty, "全部取消后 selectedTypes 应为空")
    assertTrue(view.isAllSelected, "类型为空时 isAllSelected 应恢复为 true")

    // 5. 选中部分后点击全部 (toggleAllTypes) 重置
    view.toggleFileType(.image)
    assertTrue(!view.isAllSelected)
    view.toggleAllTypes()
    assertTrue(state.selectedTypes.isEmpty, "点击全部后应重置为空集合")
    assertTrue(view.isAllSelected, "isAllSelected 应为 true")

    // 6. 勾选满 6 个具体类型时，isAllSelected 判定应为 true
    for t in FilterState.concreteFileTypes {
        view.toggleFileType(t)
    }
    assertTrue(view.isAllSelected, "满选 6 种类型时 isAllSelected 应为 true")
}

print("[5/6] 测试检索行为开关、全部清除（clearAll）与关闭回调...")
do {
    var state = FilterState()
    var lastState: FilterState?
    var dismissTriggered = false
    let binding = Binding(get: { state }, set: { state = $0 })
    let view = FilterPopoverView(
        state: binding,
        onStateChanged: { lastState = $0 },
        onDismiss: { dismissTriggered = true }
    )

    // 修改大小约束
    state.minSizeBytes = 10_000_000
    state.maxSizeBytes = 100_000_000
    state.selectedTypes = [.code]
    assertEqual(state.activeCount, 2, "2 个维度均有约束")

    // 测试全部清除
    view.clearAll()
    assertTrue(state.isUnconstrained, "执行 clearAll 后应恢复为完全无约束状态")
    assertEqual(state.activeCount, 0, "activeCount 应归零")
    assertTrue(state.selectedTypes.isEmpty, "selectedTypes 应清空")
    assertTrue(state.minSizeBytes == nil, "minSizeBytes 应为 nil")
    assertTrue(state.maxSizeBytes == nil, "maxSizeBytes 应为 nil")
    assertEqual(lastState, state, "clearAll 应调用 onStateChanged")

    // 测试关闭回调
    view.onDismiss?()
    assertTrue(dismissTriggered, "onDismiss 回调应成功触发")
}

print("[6/6] 测试本地化资源完整性与多语言键值对齐...")
do {
    let zhPath = "Lokii/Lokii/Resources/zh-Hans.lproj/Localizable.strings"
    let enPath = "Lokii/Lokii/Resources/en.lproj/Localizable.strings"

    guard let zhContent = try? String(contentsOfFile: zhPath, encoding: .utf8),
          let enContent = try? String(contentsOfFile: enPath, encoding: .utf8) else {
        fputs("无法读取多语言资源文件\\n", stderr)
        exit(1)
    }

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
        "filter.behavior.title",
        "filter.behavior.path",
        "filter.behavior.fuzzy",
        "filter.done"
    ]

    for key in requiredFilterKeys {
        assertTrue(zhContent.contains("\\"\\(key)\\""), "zh-Hans 必须包含 \\(key)")
        assertTrue(enContent.contains("\\"\\(key)\\""), "en 必须包含 \\(key)")
    }

    assertTrue(zhContent.contains("过滤条件"), "zh-Hans 应包含“过滤条件”")
    assertTrue(enContent.contains("Filters"), "en 应包含“Filters”")
    assertTrue(zhContent.contains("全部清除"), "zh-Hans 应包含“全部清除”")
    assertTrue(enContent.contains("Clear All"), "en 应包含“Clear All”")
    assertTrue(zhContent.contains("大小下限大于上限，区间互斥"), "zh-Hans 应包含区间倒置弱提示")
    assertTrue(enContent.contains("Minimum size exceeds maximum size"), "en 应包含区间倒置弱提示")
}

print("[7/8] 测试输入防抖、立即生效与清空编辑态隔离保护...")
do {
    var state = FilterState()
    var stateChangedCount = 0
    let binding = Binding(get: { state }, set: { state = $0 })
    let view = FilterPopoverView(
        state: binding,
        onStateChanged: { _ in
            stateChangedCount += 1
        }
    )

    // 1. 开启下限（Checkbox 零延迟即时响应）
    view.handleMinToggle(true)
    assertEqual(state.minSizeBytes, 10_000_000, "勾选后应立即设置为默认 10MB")
    assertEqual(stateChangedCount, 1, "Checkbox 勾选应立即触发 1 次回调")
    assertTrue(view.isMinEnabled, "isMinEnabled 应为 true")

    // 2. 模拟用户键盘连续输入（输入防抖：不立即写 state）
    view.handleMinTextChange("2")
    view.handleMinTextChange("20")
    assertEqual(state.minSizeBytes, 10_000_000, "防抖期间 state 暂不发生变更")
    assertEqual(stateChangedCount, 1, "防抖期间不应触发额外回调")

    // 3. 用户回车或完成立即 flush 生效
    view.flushDebouncedChanges()
    assertEqual(state.minSizeBytes, 20_000_000, "flush 后应写入 20MB")
    assertEqual(stateChangedCount, 2, "flush 后应触发回调")
    assertTrue(view.isMinEnabled, "isMinEnabled 仍保持开启")

    // 4. [CRITICAL 缺陷修复验证] 用户清空输入框文本（临时编辑态/中间态）
    view.handleMinTextChange("")
    view.flushDebouncedChanges()
    // 文本为空时计算出的字节数为 nil
    assertTrue(state.minSizeBytes == nil, "文本清空后 minSizeBytes 应置为 nil")
    assertTrue(view.isMinEnabled, "重点守卫：文本清空时 Checkbox 绝不能被取消勾选，控件不得被 disabled！")

    // 5. 用户继续输入有效数字
    view.handleMinTextChange("50")
    view.flushDebouncedChanges()
    assertEqual(state.minSizeBytes, 50_000_000, "继续输入后应正常恢复 50MB")
    assertTrue(view.isMinEnabled, "isMinEnabled 仍保持为 true")

    // 6. 用户主动取消勾选 Checkbox（零延迟即时禁用）
    view.handleMinToggle(false)
    assertTrue(state.minSizeBytes == nil, "取消勾选后应重置为 nil")
    assertTrue(!view.isMinEnabled, "主动取消勾选后 isMinEnabled 应为 false")
}

print("[8/8] 测试区间倒置（isSizeRangeInverted）判定逻辑...")
do {
    // 1. 初始空状态无倒置
    var s1 = FilterState()
    let b1 = Binding(get: { s1 }, set: { s1 = $0 })
    let v1 = FilterPopoverView(state: b1)
    assertTrue(!v1.isSizeRangeInverted, "未启用大小过滤时不应判定为倒置")

    // 2. 正常区间：下限 10MB，上限 100MB
    var s2 = FilterState(minSizeBytes: 10_000_000, maxSizeBytes: 100_000_000)
    let b2 = Binding(get: { s2 }, set: { s2 = $0 })
    let v2 = FilterPopoverView(state: b2)
    assertTrue(!v2.isSizeRangeInverted, "下限小于上限时不应倒置")

    // 3. 倒置区间：下限 10MB，上限 500KB (10,000,000 > 500,000)
    var s3 = FilterState(minSizeBytes: 10_000_000, maxSizeBytes: 500_000)
    let b3 = Binding(get: { s3 }, set: { s3 = $0 })
    let v3 = FilterPopoverView(state: b3)
    assertTrue(v3.isSizeRangeInverted, "下限大于上限时应判定为倒置")

    // 4. 单边约束不应判定为倒置
    var s4 = FilterState(minSizeBytes: 10_000_000, maxSizeBytes: nil)
    let b4 = Binding(get: { s4 }, set: { s4 = $0 })
    let v4 = FilterPopoverView(state: b4)
    assertTrue(!v4.isSizeRangeInverted, "仅有单边约束时不应倒置")
}

print("FilterPopoverView 全部测试通过！")
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

if runProcess.terminationStatus != 0 {
    exit(runProcess.terminationStatus)
}
