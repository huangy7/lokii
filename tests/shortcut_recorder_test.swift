import Cocoa

// MARK: - Standalone Test Harness for Shortcut Recorder Control (Keycap Pill)

func assertEqual<T: Equatable>(_ actual: T, _ expected: T, _ message: String = "", file: StaticString = #file, line: UInt = #line) {
    if actual != expected {
        fputs("断言失败: [\(actual)] != [\(expected)] - \(message) (\(file):\(line))\n", stderr)
        exit(1)
    }
}

func assertTrue(_ condition: Bool, _ message: String = "", file: StaticString = #file, line: UInt = #line) {
    if !condition {
        fputs("断言失败: 条件不满足 - \(message) (\(file):\(line))\n", stderr)
        exit(1)
    }
}

let recorderSourcePath = "Lokii/Lokii/KeyboardShortcuts/RecorderCocoa.swift"
guard let recorderSource = try? String(contentsOfFile: recorderSourcePath, encoding: .utf8) else {
    fputs("无法读取 \(recorderSourcePath) 源码文件\n", stderr)
    exit(1)
}

print("[1/5] 测试 RecorderCocoa 架构与反脆弱约束（彻底剔除 NSSearchField 与文本编辑机制）...")
do {
    assertTrue(!recorderSource.contains("class RecorderCocoa: NSSearchField"), "RecorderCocoa 严禁继承自 NSSearchField 搜索框")
    assertTrue(recorderSource.contains("class RecorderCocoa: NSControl") || recorderSource.contains("class RecorderCocoa: NSView"), "RecorderCocoa 必须继承自 NSControl 或 NSView 原生控件")
    assertTrue(!recorderSource.contains("NSSearchFieldCell"), "源码严禁出现 NSSearchFieldCell 依赖")
    assertTrue(!recorderSource.contains("searchButtonCell"), "源码严禁包含 searchButtonCell")
    assertTrue(!recorderSource.contains("cancelButtonCell"), "源码严禁包含 cancelButtonCell 伪装")
    assertTrue(!recorderSource.contains("controlTextDidChange"), "源码严禁包含 controlTextDidChange 文本编辑委托")
    assertTrue(!recorderSource.contains("controlTextDidEndEditing"), "源码严禁包含 controlTextDidEndEditing 文本编辑委托")
    assertTrue(!recorderSource.contains("hideCaret"), "源码严禁包含 hideCaret 障眼法调用")
    assertTrue(recorderSource.contains("override public var inputContext: NSTextInputContext? { nil }") || recorderSource.contains("override var inputContext: NSTextInputContext? { nil }"), "必须显式重写 inputContext 返回 nil 以彻底切断输入法 TSM 文本服务引擎")
}

print("[2/5] 测试方案 A 原生单体按钮结构与外部点击安全退出机制...")
do {
    assertTrue(!recorderSource.contains("class KeyCapView"), "方案 A 严禁使用多层嵌套 KeyCapView 独立小方框")
    assertTrue(recorderSource.contains("displayLabel"), "方案 A 必须使用一体化单体文本排版 displayLabel 渲染快捷键")
    assertTrue(recorderSource.contains("clearButton"), "必须具备明确的独立快捷键清除按钮 clearButton")
    assertTrue(recorderSource.contains("addLocalMonitorForEvents"), "必须挂载本地事件监视器以实现点击界面外部任意区域安全退出")
    assertTrue(recorderSource.contains("removeMonitor"), "必须包含严密的 removeMonitor 注销防线以杜绝内存泄漏与野指针崩溃")
}

print("[3/5] 测试实时修饰键动态跟踪 (flagsChanged) 约束...")
do {
    assertTrue(recorderSource.contains("flagsChanged"), "必须监听并处理 flagsChanged 事件以实现按住修饰键实时点亮")
    assertTrue(recorderSource.contains("currentModifiers"), "必须维护当前已按住修饰键状态 currentModifiers")
    assertTrue(recorderSource.contains("isRecording"), "必须维护录制中状态 isRecording")
}

print("[4/5] 测试按键拦截与状态守卫机制 (Escape / Delete / 纯字母阻隔) 约束...")
do {
    assertTrue(recorderSource.contains("kVK_Escape") || recorderSource.contains("53"), "必须拦截 Escape 键以实现取消录制并恢复原状")
    assertTrue(recorderSource.contains("kVK_Delete") || recorderSource.contains("51"), "必须拦截 Delete/Backspace 键以实现清空快捷键")
    assertTrue(recorderSource.contains("isRecording"), "录制状态管理必须完整")
}

print("[5/5] 测试多语言资源文件键值与 Space 英文规范完整性...")
do {
    let zhPath = "Lokii/Lokii/Resources/zh-Hans.lproj/Localizable.strings"
    let enPath = "Lokii/Lokii/Resources/en.lproj/Localizable.strings"

    guard let zhContent = try? String(contentsOfFile: zhPath, encoding: .utf8),
          let enContent = try? String(contentsOfFile: enPath, encoding: .utf8) else {
        fputs("无法读取多语言资源文件\n", stderr)
        exit(1)
    }

    let requiredKeys = [
        "record_shortcut",
        "press_shortcut",
        "shortcut.clear",
        "space_key",
    ]

    for key in requiredKeys {
        assertTrue(zhContent.contains("\"\(key)\""), "zh-Hans 必须包含 \(key)")
        assertTrue(enContent.contains("\"\(key)\""), "en 必须包含 \(key)")
    }

    assertTrue(zhContent.contains("\"space_key\" = \"Space\";"), "中文多语言中 space_key 必须规范为英文单词 Space")
    assertTrue(enContent.contains("\"space_key\" = \"Space\";"), "英文多语言中 space_key 必须为 Space")
}

print("测试全部通过：5/5 项断言成功。")
