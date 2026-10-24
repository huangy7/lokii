import Cocoa

// MARK: - Standalone Test Harness for FilterQuerySyncCoordinator

let coordinatorPath = "Lokii/Lokii/FilterQuerySyncCoordinator.swift"
guard FileManager.default.fileExists(atPath: coordinatorPath) else {
    fputs("tests/filter_query_sync_coordinator_test.swift: error: 找不到 \(coordinatorPath)\n", stderr)
    exit(1)
}

guard let coordinatorSource = try? String(contentsOfFile: coordinatorPath, encoding: .utf8) else {
    fputs("tests/filter_query_sync_coordinator_test.swift: error: 无法读取 \(coordinatorPath)\n", stderr)
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
"""

let runnerSource = """
import Cocoa

\(mockFfiSource)

\(coordinatorSource)

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

print("[1/5] 测试 FilterState 基础属性、activeCount 统计与 clearAll 重置逻辑...")
do {
    var state = FilterState()
    assertTrue(state.isUnconstrained, "默认 FilterState 应为无约束状态")
    assertEqual(state.activeCount, 0, "默认 FilterState activeCount 应为 0")

    state.selectedTypes = [.code, .document]
    assertTrue(!state.isUnconstrained, "添加文件类型约束后不应为无约束状态")
    assertEqual(state.activeCount, 1, "仅设置文件类型时 activeCount 应为 1")

    state.minSizeBytes = 10_000_000
    assertEqual(state.activeCount, 2, "设置类型与大小约束时 activeCount 应为 2")

    state.clearAll()
    assertTrue(state.isUnconstrained, "执行 clearAll 后应恢复为无约束状态")
    assertEqual(state.activeCount, 0, "执行 clearAll 后 activeCount 应归零")
    assertTrue(state.selectedTypes.isEmpty, "执行 clearAll 后 selectedTypes 应为空")
    assertTrue(state.minSizeBytes == nil, "执行 clearAll 后 minSizeBytes 应为 nil")
    assertTrue(state.maxSizeBytes == nil, "执行 clearAll 后 maxSizeBytes 应为 nil")

    // 测试 .all 单独选定或全量选定不计为有效过滤约束
    state.selectedTypes = [.all]
    assertTrue(state.isUnconstrained, "selectedTypes 为 [.all] 时视为无约束")
    assertEqual(state.activeCount, 0, "selectedTypes 为 [.all] 时 activeCount 应为 0")

    let allConcrete: Set<FfiFileType> = [.document, .code, .image, .media, .archive, .folder]
    state.selectedTypes = allConcrete
    assertTrue(state.isUnconstrained, "全选具体类型等价于无约束")
    assertEqual(state.activeCount, 0, "全选具体类型时 activeCount 应为 0")
}

print("[2/5] 测试 FilterQuerySyncCoordinator.serialize 状态序列化...")
do {
    var state = FilterState()
    state.selectedTypes = [.code, .document]
    state.minSizeBytes = 10_000_000

    let result1 = FilterQuerySyncCoordinator.serialize(keyword: "lokii", state: state)
    assertTrue(result1.contains("lokii"), "序列化应保留原始检索词 lokii")
    assertTrue(result1.contains("type:code,doc"), "序列化应正确输出规范分类缩写 type:code,doc")
    assertTrue(result1.contains("size:>10m"), "序列化应输出十进制单位格式 size:>10m")

    // 空关键字序列化
    let resultEmptyKeyword = FilterQuerySyncCoordinator.serialize(keyword: "", state: state)
    assertEqual(resultEmptyKeyword, "type:code,doc size:>10m", "空关键字序列化不应包含首尾多余空格")

    // 区间大小序列化
    var intervalState = FilterState()
    intervalState.minSizeBytes = 10_000_000
    intervalState.maxSizeBytes = 50_000_000
    let resultInterval = FilterQuerySyncCoordinator.serialize(keyword: "archive", state: intervalState)
    assertEqual(resultInterval, "archive size:10m..50m", "双端大小闭区间应序列化为 size:min..max")

    // 仅上限大小序列化
    var maxOnlyState = FilterState()
    maxOnlyState.maxSizeBytes = 500_000
    let resultMaxOnly = FilterQuerySyncCoordinator.serialize(keyword: "doc", state: maxOnlyState)
    assertEqual(resultMaxOnly, "doc size:<500k", "仅上限大小应序列化为 size:<500k")

    // 无约束状态序列化
    let unconstrainedState = FilterState()
    let resultPlain = FilterQuerySyncCoordinator.serialize(keyword: "hello world", state: unconstrainedState)
    assertEqual(resultPlain, "hello world", "无约束状态序列化应保持关键字原样")
}

print("[3/5] 测试 FilterQuerySyncCoordinator.deserialize 语法反序列化...")
do {
    let (keyword1, state1) = FilterQuerySyncCoordinator.deserialize(query: "myproject type:code,doc size:>10m")
    assertEqual(keyword1, "myproject", "应正确剔除内联语法提取纯关键字")
    assertEqual(state1.selectedTypes, [.code, .document], "应正确回填多选文件类型")
    assertEqual(state1.minSizeBytes, 10_000_000, "应正确解析 size:>10m 字节下限")
    assertTrue(state1.maxSizeBytes == nil, "单边下限时 maxSizeBytes 应为 nil")

    // 双端区间语法解析
    let (keywordInterval, stateInterval) = FilterQuerySyncCoordinator.deserialize(query: "size:10m..50m lokii")
    assertEqual(keywordInterval, "lokii", "双端区间语法后置关键字应正确提取")
    assertEqual(stateInterval.minSizeBytes, 10_000_000, "应正确解析 10m 下限")
    assertEqual(stateInterval.maxSizeBytes, 50_000_000, "应正确解析 50m 上限")

    // 单边上限语法解析
    let (keywordMax, stateMax) = FilterQuerySyncCoordinator.deserialize(query: "report size:<500k")
    assertEqual(keywordMax, "report", "应提取纯关键字 report")
    assertTrue(stateMax.minSizeBytes == nil, "单边上限时 minSizeBytes 应为 nil")
    assertEqual(stateMax.maxSizeBytes, 500_000, "应正确解析 500k 上限")

    // 大小写不敏感解析
    let (_, stateUpper) = FilterQuerySyncCoordinator.deserialize(query: "TYPE:CODE,DOC SIZE:>10M")
    assertEqual(stateUpper.selectedTypes, [.code, .document], "语法标记解析应不区分大小写")
    assertEqual(stateUpper.minSizeBytes, 10_000_000, "大小单位解析应不区分大小写")
}

print("[4/5] 测试 FilterQuerySyncCoordinator 语法解析边界条件...")
do {
    // 空查询
    let (kwEmpty, stateEmpty) = FilterQuerySyncCoordinator.deserialize(query: "")
    assertEqual(kwEmpty, "", "空查询关键字应为空")
    assertTrue(stateEmpty.isUnconstrained, "空查询状态应无约束")

    // 纯空格查询
    let (kwSpaces, stateSpaces) = FilterQuerySyncCoordinator.deserialize(query: "    ")
    assertEqual(kwSpaces, "", "纯空格查询关键字应为空")
    assertTrue(stateSpaces.isUnconstrained, "纯空格查询状态应无约束")

    // 纯语法无关键字
    let (kwPureSyntax, statePureSyntax) = FilterQuerySyncCoordinator.deserialize(query: "type:code size:>10m")
    assertEqual(kwPureSyntax, "", "纯语法查询分离关键字应为空")
    assertEqual(statePureSyntax.selectedTypes, [.code], "纯语法查询应正常提取类型")
    assertEqual(statePureSyntax.minSizeBytes, 10_000_000, "纯语法查询应正常提取大小")

    // 无任何语法的普通多词查询
    let (kwPlain, statePlain) = FilterQuerySyncCoordinator.deserialize(query: "swift build release tool")
    assertEqual(kwPlain, "swift build release tool", "普通多词查询关键字应保持完整")
    assertTrue(statePlain.isUnconstrained, "普通多词查询状态应无约束")

    // 未知或非法分类保留为普通关键字
    let (kwInvalidType, stateInvalidType) = FilterQuerySyncCoordinator.deserialize(query: "type:unknowncategory mycode")
    assertEqual(kwInvalidType, "type:unknowncategory mycode", "未知分类语法应退化为普通检索词")
    assertTrue(stateInvalidType.selectedTypes.isEmpty, "未知分类不应污染 selectedTypes")

    // 非法大小数值保留为普通关键字
    let (kwInvalidSize, stateInvalidSize) = FilterQuerySyncCoordinator.deserialize(query: "size:notanumber report")
    assertEqual(kwInvalidSize, "size:notanumber report", "非法大小语法应退化为普通检索词")
    assertTrue(stateInvalidSize.minSizeBytes == nil, "非法大小不应污染 minSizeBytes")
}

print("[5/5] 测试 FilterQuerySyncCoordinator 重入锁与输入法保护...")
do {
    let coordinator = FilterQuerySyncCoordinator()
    let searchField = NSSearchField()
    searchField.stringValue = "initial query"

    // 1. 常规 UI 触发搜索框更新（激活过滤条件时自动追加尾随空格，方便用户继续输入关键词）
    var searchPerformedWith: String?
    var state = FilterState()
    state.selectedTypes = [.code]
    coordinator.updateQueryFromUI(state: state, searchField: searchField) { newQuery in
        searchPerformedWith = newQuery
    }
    assertEqual(searchField.stringValue, "initial query type:code ", "updateQueryFromUI 应保留已有输入词、追加类型语法并附上尾随空格")
    assertEqual(searchPerformedWith, "initial query type:code ", "应触发执行检索回调并传入合成后的 query")

    // 2. 模拟重入防死循环：在 updateQueryFromUI 内部触发 updateUIFromQuery 不应引发无限循环或重复更新
    var loopTriggerCount = 0
    coordinator.updateQueryFromUI(state: state, searchField: searchField) { _ in
        // 模拟外部响应通知反向调用 updateUIFromQuery
        coordinator.updateUIFromQuery(query: "nested query") { _ in
            loopTriggerCount += 1
        }
    }
    assertEqual(loopTriggerCount, 0, "正在同步中（isSynchronizing = true）时应拦截反向重入调用")

    // 3. updateUIFromQuery 正常触发
    var receivedState: FilterState?
    coordinator.updateUIFromQuery(query: "lokii type:media size:<1g") { s in
        receivedState = s
    }
    assertTrue(receivedState != nil, "updateUIFromQuery 应成功回调解析所得状态")
    assertEqual(receivedState?.selectedTypes, [.media], "反向更新应还原 selectedTypes")
    assertEqual(receivedState?.maxSizeBytes, 1_000_000_000, "反向更新应还原 maxSizeBytes")

    // 4. 输入法组字（hasMarkedText）保护测试
    class MockTextViewWithMarkedText: NSTextView {
        var mockMarked: Bool = true
        override func hasMarkedText() -> Bool {
            return mockMarked
        }
    }

    class MockSearchField: NSSearchField {
        let mockEditor = MockTextViewWithMarkedText()
        override func currentEditor() -> NSText? {
            return mockEditor
        }
    }

    let markedSearchField = MockSearchField()
    markedSearchField.stringValue = "拼音输入zhong"
    var markedSearchTriggered = false

    var newState = FilterState()
    newState.selectedTypes = [.document]
    coordinator.updateQueryFromUI(state: newState, searchField: markedSearchField) { _ in
        markedSearchTriggered = true
    }

    assertEqual(markedSearchTriggered, false, "输入法组字中（hasMarkedText() == true）时不应触发检索")
    assertEqual(markedSearchField.stringValue, "拼音输入zhong", "输入法组字中时不应覆写输入框文本")

    // 模拟组字完成提交后允许更新，且光标自动推进至末尾空格后，同时挂载 PillLayoutManager
    markedSearchField.mockEditor.mockMarked = false
    coordinator.updateQueryFromUI(state: newState, searchField: markedSearchField) { _ in
        markedSearchTriggered = true
    }
    assertEqual(markedSearchTriggered, true, "输入法组字完成后应允许更新并触发检索")
    assertEqual(markedSearchField.stringValue, "拼音输入zhong type:doc ", "组字完成后应成功同步语法并追加尾随空格")
    assertEqual(markedSearchField.mockEditor.selectedRange.location, ("拼音输入zhong type:doc " as NSString).length, "勾选后光标必须自动推进至文本末尾空格后")
    assertEqual(markedSearchField.mockEditor.selectedRange.length, 0, "光标不应处于选中状态")
    assertTrue(markedSearchField.mockEditor.layoutManager is PillLayoutManager, "字段编辑器应自动挂载 PillLayoutManager 以绘制圆角胶囊背景")
}

print("[6/6] 测试 formatQuerySyntax 语法高亮与限定词胶囊格式化...")
do {
    // 1. 空字符串
    let emptyResult = FilterQuerySyncCoordinator.formatQuerySyntax("")
    assertEqual(emptyResult.length, 0, "空字符串应返回空属性字符串")

    // 2. 复合限定词语法
    let query = "readme type:code,doc size:>10m ext:swift"
    let attrStr = FilterQuerySyncCoordinator.formatQuerySyntax(query)
    assertEqual(attrStr.string, query, "格式化后的文本内容必须与原始查询完全一致")

    let nsQuery = query as NSString

    // 校验普通检索词 readme
    let readmeRange = nsQuery.range(of: "readme")
    let readmeAttrs = attrStr.attributes(at: readmeRange.location, effectiveRange: nil)
    assertTrue(readmeAttrs[.backgroundColor] == nil, "普通检索词不应带有背景衬底")

    // 校验 type: 键名前缀次要文本色
    let typeKeyRange = nsQuery.range(of: "type:")
    let typeKeyAttrs = attrStr.attributes(at: typeKeyRange.location, effectiveRange: nil)
    assertEqual(typeKeyAttrs[.foregroundColor] as? NSColor, NSColor.secondaryLabelColor, "限定词前缀必须使用次要文本色弱化视觉噪音")

    // 校验 code,doc 属性值高亮与半透明衬底
    let typeValRange = nsQuery.range(of: "code,doc")
    let typeValAttrs = attrStr.attributes(at: typeValRange.location, effectiveRange: nil)
    assertEqual(typeValAttrs[.foregroundColor] as? NSColor, NSColor.controlAccentColor, "限定词属性值必须使用系统强调前景色")
    assertTrue(typeValAttrs[.backgroundColor] != nil, "限定词属性值必须应用背景胶囊衬底")

    // 校验 size: 与 >10m
    let sizeKeyRange = nsQuery.range(of: "size:")
    let sizeValRange = nsQuery.range(of: ">10m")
    assertEqual(attrStr.attributes(at: sizeKeyRange.location, effectiveRange: nil)[.foregroundColor] as? NSColor, NSColor.secondaryLabelColor, "size: 必须使用次要文本色")
    assertEqual(attrStr.attributes(at: sizeValRange.location, effectiveRange: nil)[.foregroundColor] as? NSColor, NSColor.controlAccentColor, ">10m 必须使用强调色")
    assertTrue(attrStr.attributes(at: sizeValRange.location, effectiveRange: nil)[.backgroundColor] != nil, ">10m 必须应用背景胶囊衬底")

    // 校验 ext: 与 swift
    let extKeyRange = nsQuery.range(of: "ext:")
    let extValRange = nsQuery.range(of: "swift")
    assertEqual(attrStr.attributes(at: extKeyRange.location, effectiveRange: nil)[.foregroundColor] as? NSColor, NSColor.secondaryLabelColor, "ext: 必须使用次要文本色")
    assertEqual(attrStr.attributes(at: extValRange.location, effectiveRange: nil)[.foregroundColor] as? NSColor, NSColor.controlAccentColor, "swift 必须使用强调色")
    assertTrue(attrStr.attributes(at: extValRange.location, effectiveRange: nil)[.backgroundColor] != nil, "swift 必须应用背景胶囊衬底")

    // 3. 正在输入的未闭合限定词（如 type: 暂无值）
    let incompleteQuery = "type:"
    let incompleteAttr = FilterQuerySyncCoordinator.formatQuerySyntax(incompleteQuery)
    assertEqual(incompleteAttr.attributes(at: 0, effectiveRange: nil)[.foregroundColor] as? NSColor, NSColor.secondaryLabelColor, "未键入值的限定词前缀应立即高亮为次要色")

    // 4. 大小写不敏感支持
    let upperQuery = "TYPE:CODE"
    let upperAttr = FilterQuerySyncCoordinator.formatQuerySyntax(upperQuery)
    let codeRange = (upperQuery as NSString).range(of: "CODE")
    assertEqual(upperAttr.attributes(at: codeRange.location, effectiveRange: nil)[.foregroundColor] as? NSColor, NSColor.controlAccentColor, "大写限定词值亦应正确高亮")
}

print("FilterQuerySyncCoordinator 全部测试通过！")
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
