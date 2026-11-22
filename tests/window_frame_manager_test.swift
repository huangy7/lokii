import Cocoa

// MARK: - Standalone Test Harness for WindowFrameManager

let managerPath = "Lokii/Lokii/WindowFrameManager.swift"
guard FileManager.default.fileExists(atPath: managerPath) else {
    fputs("tests/window_frame_manager_test.swift: error: 找不到 \(managerPath)\n", stderr)
    exit(1)
}

guard let managerSource = try? String(contentsOfFile: managerPath, encoding: .utf8) else {
    fputs("tests/window_frame_manager_test.swift: error: 无法读取 \(managerPath)\n", stderr)
    exit(1)
}

// 剥离源文件中的 import AppKit/Cocoa 以拼接为单文件脚本
let sanitizedSource = managerSource
    .replacingOccurrences(of: "import AppKit\n", with: "")
    .replacingOccurrences(of: "import Cocoa\n", with: "")
    .replacingOccurrences(of: "import Foundation\n", with: "")

let runnerSource = """
import AppKit
import Foundation

\(sanitizedSource)

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

func makeIsolatedDefaults() -> (UserDefaults, String) {
    let suite = "com.lokii.test.windowframe.\\(UUID().uuidString)"
    let defaults = UserDefaults(suiteName: suite)!
    defaults.removePersistentDomain(forName: suite)
    return (defaults, suite)
}

let largeScreen = ScreenMetrics(
    frame: NSRect(x: 0, y: 0, width: 2560, height: 1440),
    visibleFrame: NSRect(x: 0, y: 0, width: 2560, height: 1415)
)
let laptopScreen = ScreenMetrics(
    frame: NSRect(x: 2560, y: 0, width: 1512, height: 982),
    visibleFrame: NSRect(x: 2560, y: 0, width: 1512, height: 945)
)
let compactScreen = ScreenMetrics(
    frame: NSRect(x: 0, y: 0, width: 1024, height: 640),
    visibleFrame: NSRect(x: 0, y: 0, width: 1024, height: 615)
)

// [1/6] 默认几何尺寸遵循黄金比例且保持垂直居中偏上 5%
print("[1/6] 测试默认尺寸与居中偏上 5% 偏移...")
do {
    let fLarge = WindowFrameManager.defaultFrame(on: largeScreen)
    assertEqual(fLarge.width, 1280, "2560 宽屏应钳制为 1280")
    assertEqual(fLarge.height, 860, "1415 可见高应钳制为 860")
    let expectedLargeY = round((1415.0 - 860.0) / 2.0 + 1415.0 * 0.05)
    assertEqual(fLarge.origin.x, round((2560.0 - 1280.0) / 2.0), "大屏水平应居中")
    assertEqual(fLarge.origin.y, expectedLargeY, "大屏垂直应居中偏上 5%")

    let fLaptop = WindowFrameManager.defaultFrame(on: laptopScreen)
    assertEqual(fLaptop.width, 960, "1512 笔记本屏 60% 低于 960 应下限钳制为 960")
    assertEqual(fLaptop.height, 600, "945 可见高 62% 低于 600 应下限钳制为 600")
    let expectedLaptopY = round((945.0 - 600.0) / 2.0 + 945.0 * 0.05)
    assertEqual(fLaptop.origin.y, expectedLaptopY, "笔记本屏垂直应居中偏上 5%")
}

// [2/6] 按屏幕逻辑分辨率独立记忆尺寸，未调过的屏幕自动走黄金比例
print("[2/6] 测试按屏幕分辨率独立记忆尺寸与自适应回退...")
do {
    let (defaults, suite) = makeIsolatedDefaults()
    defer { defaults.removePersistentDomain(forName: suite) }
    let manager = WindowFrameManager(defaults: defaults)

    assertEqual(manager.hasCustomSizes, false, "初始应无自定义尺寸")
    manager.saveSize(NSSize(width: 1200, height: 800), for: largeScreen)
    assertEqual(manager.hasCustomSizes, true, "保存后应标记存在自定义尺寸")

    assertEqual(manager.resolveSize(for: largeScreen), NSSize(width: 1200, height: 800), "大屏应返回已保存尺寸")
    assertEqual(manager.resolveSize(for: laptopScreen), NSSize(width: 960, height: 600), "未调过的小屏应回退黄金比例而非沿用 1200x800")

    manager.saveSize(NSSize(width: 980, height: 620), for: laptopScreen)
    assertEqual(manager.resolveSize(for: laptopScreen), NSSize(width: 980, height: 620), "小屏应记住独立尺寸")
    assertEqual(manager.resolveSize(for: largeScreen), NSSize(width: 1200, height: 800), "切回大屏应保持 1200x800 不受影响")
}

// [3/6] 关闭位置记忆时，同屏拖动后下次呼出应回正且保留当前屏尺寸
print("[3/6] 测试关闭位置记忆时同屏拖动后呼出归位...")
do {
    let (defaults, suite) = makeIsolatedDefaults()
    defer { defaults.removePersistentDomain(forName: suite) }
    let manager = WindowFrameManager(defaults: defaults)
    manager.saveSize(NSSize(width: 1120, height: 700), for: largeScreen)

    let draggedFrame = NSRect(x: 80, y: 100, width: 1120, height: 700)
    let shownFrame = manager.frameOnShow(
        currentFrame: draggedFrame,
        targetScreen: largeScreen,
        allScreens: [largeScreen, laptopScreen],
        rememberPosition: false
    )
    let expected = WindowFrameManager.centeredFrame(
        preservingSize: NSSize(width: 1120, height: 700),
        on: largeScreen,
        minSize: WindowFrameManager.defaultMinSize
    )
    assertEqual(shownFrame, expected, "未开启位置记忆时同屏拖走后再次呼出必须回到居中偏上 5% 且保留记住的尺寸")
}

// [4/6] 开启位置记忆时恢复在线坐标，原外接屏断开越界时回退到鼠标所在屏
print("[4/6] 测试开启位置记忆与外接屏断开越界回退...")
do {
    let (defaults, suite) = makeIsolatedDefaults()
    defer { defaults.removePersistentDomain(forName: suite) }
    let manager = WindowFrameManager(defaults: defaults)
    manager.saveSize(NSSize(width: 1100, height: 680), for: largeScreen)
    manager.saveOrigin(NSPoint(x: 200, y: 300))

    let initial = manager.initialFrame(
        on: largeScreen,
        allScreens: [largeScreen, laptopScreen],
        rememberPosition: true
    )
    assertEqual(initial, NSRect(x: 200, y: 300, width: 1100, height: 680), "启动应恢复保存位置与对应屏幕尺寸")

    // 模拟原大屏断开，仅剩小屏（位于 x: 2560..4072），旧位置 (200, 300) 已脱离在线屏
    let fallbackShown = manager.frameOnShow(
        currentFrame: initial,
        targetScreen: laptopScreen,
        allScreens: [laptopScreen],
        rememberPosition: true
    )
    let expectedFallback = WindowFrameManager.centeredFrame(
        preservingSize: NSSize(width: 960, height: 600),
        on: laptopScreen,
        minSize: WindowFrameManager.defaultMinSize
    )
    assertEqual(fallbackShown, expectedFallback, "原屏幕断开越界后应迁移至目标在线屏居中偏上并应用目标屏尺寸")
}

// [5/8] 方案 A：手动拖跨屏仅对真实溢出目标屏可见区的维度做温和收缩，完整容纳时返回 nil 绝不抢手跳变
print("[5/8] 测试跨屏拖拽仅溢出收缩与未溢出零干预...")
do {
    let (defaults, suite) = makeIsolatedDefaults()
    defer { defaults.removePersistentDomain(forName: suite) }
    let manager = WindowFrameManager(defaults: defaults)
    manager.saveSize(NSSize(width: 1200, height: 800), for: largeScreen)

    // 1. 1200x800 完整容纳于 1512x945 笔记本屏，即使笔记本屏默认/记忆尺寸更小，也必须返回 nil 不抢手不跳变
    let fitsOnLaptop = NSRect(x: 2700, y: 100, width: 1200, height: 800)
    assertTrue(
        manager.clampedFrameForScreenOverflow(currentFrame: fitsOnLaptop, newScreen: laptopScreen, anchorX: 2900) == nil,
        "窗口未超出目标屏可见宽高时必须返回 nil，完全尊重用户手动拖拽落点"
    )

    // 2. 仅高度溢出（1100x1050 拖到 1512x945）：只收缩高度至 945，水平坐标与宽度纹丝不动
    let heightOverflow = NSRect(x: 2720, y: -40, width: 1100, height: 1050)
    let clampedHeight = manager.clampedFrameForScreenOverflow(currentFrame: heightOverflow, newScreen: laptopScreen, anchorX: 2950)
    assertTrue(clampedHeight != nil, "高度超出目标屏可见区时应触发安全收缩")
    if let clampedHeight {
        assertEqual(clampedHeight.width, 1100, "未溢出的宽度必须保持不变")
        assertEqual(clampedHeight.origin.x, 2720, "仅高度溢出时水平坐标不得偏移")
        assertEqual(clampedHeight.height, laptopScreen.visibleFrame.height, "溢出高度应收缩至目标屏可见高度")
        assertTrue(clampedHeight.minY >= laptopScreen.visibleFrame.minY && clampedHeight.maxY <= laptopScreen.visibleFrame.maxY, "垂直方向必须位于目标屏可见区内")
    }

    // 3. 仅宽度溢出（1700x760 拖到 1512x945）：只收缩宽度至 1512，高度保持 760 且顶边尽量贴近原落点
    let widthOverflow = NSRect(x: 2500, y: 120, width: 1700, height: 760)
    let clampedWidth = manager.clampedFrameForScreenOverflow(currentFrame: widthOverflow, newScreen: laptopScreen, anchorX: 2800)
    assertTrue(clampedWidth != nil, "宽度超出目标屏可见区时应触发安全收缩")
    if let clampedWidth {
        assertEqual(clampedWidth.width, laptopScreen.visibleFrame.width, "溢出宽度应收缩至目标屏可见宽度")
        assertEqual(clampedWidth.height, 760, "未溢出的高度必须保持不变")
        assertEqual(clampedWidth.origin.x, laptopScreen.visibleFrame.origin.x, "收缩至满宽时左缘应贴齐目标屏可见区")
        assertEqual(clampedWidth.origin.y, 120, "未溢出且在可见区内的垂直位置应保持不变")
    }

    // 4. 宽高双溢出（1800x1100 拖到 1512x945）：双维度收敛至可见区
    let bothOverflow = NSRect(x: 2400, y: -100, width: 1800, height: 1100)
    let clampedBoth = manager.clampedFrameForScreenOverflow(currentFrame: bothOverflow, newScreen: laptopScreen, anchorX: 2850)
    assertEqual(clampedBoth, laptopScreen.visibleFrame, "宽高均溢出时应收敛为目标屏可见工作区")
}

// [6/6] 旧键 LokiiMainWindowFrame 一次性迁移与畸形尺寸安全钳制 (Review Focus #1)
print("[6/6] 测试旧键迁移与畸形尺寸安全钳制...")
do {
    let (defaults, suite) = makeIsolatedDefaults()
    defer { defaults.removePersistentDomain(forName: suite) }
    defaults.set(NSStringFromRect(NSRect(x: 150, y: 250, width: 1180, height: 740)), forKey: "LokiiMainWindowFrame")

    let manager = WindowFrameManager(defaults: defaults)
    manager.migrateLegacyFrameIfNeeded(currentScreen: largeScreen, rememberPosition: true)

    assertTrue(defaults.string(forKey: "LokiiMainWindowFrame") == nil, "迁移后应移除旧 LokiiMainWindowFrame 键")
    assertEqual(manager.resolveSize(for: largeScreen), NSSize(width: 1180, height: 740), "旧尺寸应迁移至当前屏槽位")
    assertEqual(manager.savedOrigin(), NSPoint(x: 150, y: 250), "开启位置记忆时应迁移旧坐标")

    // 写入畸形尺寸（零宽高 & 超出小屏尺寸）并验证安全钳制
    let compactKey = WindowFrameManager.screenSizeKey(for: compactScreen.frame)
    defaults.set([compactKey: NSStringFromSize(NSSize(width: 0, height: 0))], forKey: WindowFrameManager.sizesByScreenKey)
    assertEqual(manager.resolveSize(for: compactScreen), WindowFrameManager.defaultFrame(on: compactScreen).size, "零宽高应回退默认尺寸")

    defaults.set([compactKey: NSStringFromSize(NSSize(width: 5000, height: 3000))], forKey: WindowFrameManager.sizesByScreenKey)
    assertEqual(manager.resolveSize(for: compactScreen), compactScreen.visibleFrame.size, "超大尺寸应钳制至目标屏可见工作区大小")

    defaults.set([compactKey: NSStringFromSize(NSSize(width: 300, height: 200))], forKey: WindowFrameManager.sizesByScreenKey)
    assertEqual(manager.resolveSize(for: compactScreen), WindowFrameManager.defaultMinSize, "过小尺寸应钳制至 minSize (780x420)")

    manager.clearAllCustomSizesAndOrigin()
    assertEqual(manager.hasCustomSizes, false, "清空后 hasCustomSizes 应为 false")
    assertTrue(manager.savedOrigin() == nil, "清空后 savedOrigin 应为 nil")
}

// [7/7] 多显示器最大重叠面积选屏与无屏延迟迁移保护 (Final Review I4)
print("[7/7] 测试多显示器最大重叠选屏与无屏延迟迁移...")
do {
    let (defaults, suite) = makeIsolatedDefaults()
    defer { defaults.removePersistentDomain(forName: suite) }
    let manager = WindowFrameManager(defaults: defaults)

    // 窗口主体在右侧笔记本屏 (x: 2560..4072)，仅左边缘有 10pt 探入左侧大屏 (x: 0..2560)
    let mostlyOnLaptop = NSRect(x: 2550, y: 150, width: 960, height: 600)
    let shown = manager.frameOnShow(
        currentFrame: mostlyOnLaptop,
        targetScreen: largeScreen,
        allScreens: [largeScreen, laptopScreen],
        rememberPosition: true
    )
    assertTrue(
        shown.minX >= laptopScreen.visibleFrame.minX && shown.maxX <= laptopScreen.visibleFrame.maxX,
        "窗口绝大部分位于副屏时，必须按最大重叠面积留在副屏而非被拖回主屏"
    )

    // 休眠无屏时不得删除旧 LokiiMainWindowFrame 键
    let legacyOnLaptop = NSRect(x: 2700, y: 180, width: 1000, height: 640)
    defaults.set(NSStringFromRect(legacyOnLaptop), forKey: WindowFrameManager.legacyFrameKey)
    manager.migrateLegacyFrameIfNeeded(allScreens: [], currentScreen: nil, rememberPosition: true)
    assertTrue(defaults.string(forKey: WindowFrameManager.legacyFrameKey) != nil, "无在线屏幕时不得提前删除旧键导致尺寸丢失")

    // 存在多屏时，旧键落在副屏上应迁移至副屏对应分辨率槽位，而非误写入主屏槽位
    manager.migrateLegacyFrameIfNeeded(allScreens: [largeScreen, laptopScreen], currentScreen: largeScreen, rememberPosition: true)
    assertTrue(defaults.string(forKey: WindowFrameManager.legacyFrameKey) == nil, "完成迁移后应删除旧键")
    assertEqual(manager.resolveSize(for: laptopScreen), NSSize(width: 1000, height: 640), "旧尺寸应迁移到旧窗口实际所在的副屏槽位")
    assertEqual(manager.resolveSize(for: largeScreen), WindowFrameManager.defaultFrame(on: largeScreen).size, "主屏槽位不应被副屏旧尺寸污染")
}

// [8/8] 系统级左键按下态（含触控板三指拖移）必须延后跨屏溢出收缩，松开后放行
print("[8/8] 测试跨屏拖拽左键未松开延后判定...")
do {
    assertEqual(WindowFrameManager.shouldDeferOverflowClamp(isLeftMouseButtonDown: true), true, "左键按住时必须延后跨屏溢出收缩")
    assertEqual(WindowFrameManager.shouldDeferOverflowClamp(isLeftMouseButtonDown: false), false, "左键松开时应放行跨屏溢出收缩")
}

print("WindowFrameManager 全部测试通过！")
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
