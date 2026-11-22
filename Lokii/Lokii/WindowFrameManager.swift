import AppKit
import Foundation

extension Notification.Name {
    static let lokiiResetWindowSize = Notification.Name("LokiiResetWindowSizeNotification")
    static let lokiiWindowSizesDidChange = Notification.Name("LokiiWindowSizesDidChangeNotification")
}

/// 抽象显示器几何指标，便于主视窗布局与无头单元测试共享同一套坐标计算逻辑
struct ScreenMetrics: Equatable {
    let frame: NSRect
    let visibleFrame: NSRect

    init(frame: NSRect, visibleFrame: NSRect) {
        self.frame = frame
        self.visibleFrame = visibleFrame
    }

    init(screen: NSScreen) {
        self.frame = screen.frame
        self.visibleFrame = screen.visibleFrame
    }
}

/// 主搜索窗口按显示器分辨率自适应尺寸与出现位置解耦管理器。
///
/// 设计职责：
/// 1. 窗口尺寸按屏幕逻辑分辨率（"WxH"）独立持久化，在不同显示器上呼出时自动匹配该屏舒适尺寸；
/// 2. 手动拖拽跨屏完全交由系统原生窗口服务，仅在彻底松手且窗口宽高真实超出目标屏可见工作区时执行温和溢出收缩；
/// 3. 窗口出现位置独立于尺寸管理：关闭位置记忆时每次呼出回归居中偏上 5%，开启时保留在线坐标；
/// 4. 兼容旧版单一 `LokiiMainWindowFrame` 键的一次性平滑迁移，并对异常/越界几何值做安全钳制。
final class WindowFrameManager {
    static let defaultMinSize = NSSize(width: 780, height: 420)
    static let legacyFrameKey = "LokiiMainWindowFrame"
    static let sizesByScreenKey = "LokiiMainWindowSizesByScreen"
    static let originKey = "LokiiMainWindowOrigin"

    private let defaults: UserDefaults
    private let minSize: NSSize

    init(defaults: UserDefaults = .standard, minSize: NSSize = WindowFrameManager.defaultMinSize) {
        self.defaults = defaults
        self.minSize = minSize
    }

    /// 以显示器逻辑分辨率取整作为稳定槽位键，跨重启与接口切换保持一致
    static func screenSizeKey(for frame: NSRect) -> String {
        "\(Int(round(frame.width)))x\(Int(round(frame.height)))"
    }

    /// 基于目标屏幕可见工作区，按黄金视觉比例计算默认尺寸与居中偏上 5% 位置
    static func defaultFrame(on screen: ScreenMetrics) -> NSRect {
        let visible = screen.visibleFrame
        let rawWidth = min(max(visible.width * 0.60, 960), 1280)
        let finalWidth = min(rawWidth, visible.width)
        let rawHeight = min(max(visible.height * 0.62, 600), 860)
        let finalHeight = min(rawHeight, visible.height)
        return centeredFrame(
            preservingSize: NSSize(width: finalWidth, height: finalHeight),
            on: screen,
            minSize: defaultMinSize
        )
    }

    /// 在目标屏幕可见工作区内保留指定宽高并居中偏上 5%，同时执行上下界钳制
    static func centeredFrame(
        preservingSize size: NSSize,
        on screen: ScreenMetrics,
        minSize: NSSize = defaultMinSize
    ) -> NSRect {
        let visible = screen.visibleFrame
        let clampedSize = clampSize(size, toVisibleFrame: visible, minSize: minSize)
        let x = visible.origin.x + (visible.width - clampedSize.width) / 2.0
        let targetY = visible.origin.y + (visible.height - clampedSize.height) / 2.0 + (visible.height * 0.05)
        let y = min(max(targetY, visible.origin.y), visible.maxY - clampedSize.height)
        return NSRect(x: round(x), y: round(y), width: round(clampedSize.width), height: round(clampedSize.height))
    }

    private static func clampSize(_ size: NSSize, toVisibleFrame visible: NSRect, minSize: NSSize) -> NSSize {
        let maxW = max(visible.width, 1)
        let maxH = max(visible.height, 1)
        let effMinW = min(minSize.width, maxW)
        let effMinH = min(minSize.height, maxH)
        let w = min(max(size.width, effMinW), maxW)
        let h = min(max(size.height, effMinH), maxH)
        return NSSize(width: round(w), height: round(h))
    }

    private static func clampFrame(_ frame: NSRect, toVisibleFrame visible: NSRect, minSize: NSSize) -> NSRect {
        let clampedSize = clampSize(frame.size, toVisibleFrame: visible, minSize: minSize)
        let maxX = max(visible.origin.x, visible.maxX - clampedSize.width)
        let maxY = max(visible.origin.y, visible.maxY - clampedSize.height)
        let x = min(max(frame.origin.x, visible.origin.x), maxX)
        let y = min(max(frame.origin.y, visible.origin.y), maxY)
        return NSRect(x: round(x), y: round(y), width: clampedSize.width, height: clampedSize.height)
    }

    private static func intersectionArea(_ a: NSRect, _ b: NSRect) -> CGFloat {
        let inter = a.intersection(b)
        guard !inter.isNull, inter.width > 0, inter.height > 0 else { return 0 }
        return inter.width * inter.height
    }

    /// 按最大重叠面积选取窗口主体所在的显示器，避免窗口边缘探入相邻屏幕 1pt 即被拉偏
    static func bestScreen(for rect: NSRect, in screens: [ScreenMetrics]) -> ScreenMetrics? {
        if let byVisible = screens
            .map({ ($0, intersectionArea(rect, $0.visibleFrame)) })
            .filter({ $0.1 > 0 })
            .max(by: { $0.1 < $1.1 })?.0 {
            return byVisible
        }
        return screens
            .map({ ($0, intersectionArea(rect, $0.frame)) })
            .filter({ $0.1 > 0 })
            .max(by: { $0.1 < $1.1 })?.0
    }

    // MARK: - Persistence & Migration

    /// 将旧版合并存储的 `LokiiMainWindowFrame` 一次性迁移至按屏尺寸字典与独立位置键。
    /// 若当前无在线显示器（如合盖/休眠启动），暂保留旧键等待后续有屏时再安全迁移。
    func migrateLegacyFrameIfNeeded(
        allScreens: [ScreenMetrics] = [],
        currentScreen: ScreenMetrics?,
        rememberPosition: Bool
    ) {
        guard let legacyString = defaults.string(forKey: Self.legacyFrameKey) else { return }
        guard !legacyString.isEmpty else {
            defaults.removeObject(forKey: Self.legacyFrameKey)
            return
        }
        let legacyRect = NSRectFromString(legacyString)
        guard legacyRect.width > 0, legacyRect.height > 0 else {
            defaults.removeObject(forKey: Self.legacyFrameKey)
            return
        }

        guard let targetScreen = Self.bestScreen(for: legacyRect, in: allScreens) ?? currentScreen else {
            return
        }
        defaults.removeObject(forKey: Self.legacyFrameKey)

        var map = defaults.dictionary(forKey: Self.sizesByScreenKey) as? [String: String] ?? [:]
        let key = Self.screenSizeKey(for: targetScreen.frame)
        if map[key] == nil {
            let safeSize = Self.clampSize(legacyRect.size, toVisibleFrame: targetScreen.visibleFrame, minSize: minSize)
            map[key] = NSStringFromSize(safeSize)
            defaults.set(map, forKey: Self.sizesByScreenKey)
        }
        if rememberPosition, defaults.string(forKey: Self.originKey) == nil {
            defaults.set(NSStringFromPoint(legacyRect.origin), forKey: Self.originKey)
        }
    }

    var hasCustomSizes: Bool {
        guard let map = defaults.dictionary(forKey: Self.sizesByScreenKey) as? [String: String] else {
            return false
        }
        return !map.isEmpty
    }

    /// 解析目标屏幕的生效窗口尺寸：命中用户该分辨率记录则钳制返回，否则回退该屏黄金比例尺寸
    func resolveSize(for screen: ScreenMetrics) -> NSSize {
        let key = Self.screenSizeKey(for: screen.frame)
        if let map = defaults.dictionary(forKey: Self.sizesByScreenKey) as? [String: String],
           let raw = map[key], !raw.isEmpty {
            let parsed = NSSizeFromString(raw)
            if parsed.width > 0, parsed.height > 0 {
                return Self.clampSize(parsed, toVisibleFrame: screen.visibleFrame, minSize: minSize)
            }
        }
        return Self.defaultFrame(on: screen).size
    }

    func saveSize(_ size: NSSize, for screen: ScreenMetrics) {
        guard size.width > 0, size.height > 0 else { return }
        let clamped = Self.clampSize(size, toVisibleFrame: screen.visibleFrame, minSize: minSize)
        var map = defaults.dictionary(forKey: Self.sizesByScreenKey) as? [String: String] ?? [:]
        map[Self.screenSizeKey(for: screen.frame)] = NSStringFromSize(clamped)
        defaults.set(map, forKey: Self.sizesByScreenKey)
    }

    func saveOrigin(_ origin: NSPoint) {
        defaults.set(NSStringFromPoint(origin), forKey: Self.originKey)
    }

    func savedOrigin() -> NSPoint? {
        guard let raw = defaults.string(forKey: Self.originKey), !raw.isEmpty else { return nil }
        return NSPointFromString(raw)
    }

    func clearSavedOrigin() {
        defaults.removeObject(forKey: Self.originKey)
    }

    func clearAllCustomSizesAndOrigin() {
        defaults.removeObject(forKey: Self.sizesByScreenKey)
        defaults.removeObject(forKey: Self.originKey)
        defaults.removeObject(forKey: Self.legacyFrameKey)
    }

    // MARK: - Window Frame Calculation

    /// 启动初始化时的窗口几何计算
    func initialFrame(
        on primaryScreen: ScreenMetrics,
        allScreens: [ScreenMetrics],
        rememberPosition: Bool
    ) -> NSRect {
        if rememberPosition, let origin = savedOrigin() {
            let probeSize = resolveSize(for: primaryScreen)
            let probeRect = NSRect(origin: origin, size: probeSize)
            let candidateScreen = Self.bestScreen(for: probeRect, in: allScreens)
                ?? allScreens.first { $0.frame.contains(origin) }
            if let screen = candidateScreen {
                let size = resolveSize(for: screen)
                return Self.clampFrame(NSRect(origin: origin, size: size), toVisibleFrame: screen.visibleFrame, minSize: minSize)
            }
        }
        let size = resolveSize(for: primaryScreen)
        return Self.centeredFrame(preservingSize: size, on: primaryScreen, minSize: minSize)
    }

    /// 每次从隐藏态呼出时的目标窗口几何计算
    func frameOnShow(
        currentFrame: NSRect,
        targetScreen: ScreenMetrics,
        allScreens: [ScreenMetrics],
        rememberPosition: Bool
    ) -> NSRect {
        if rememberPosition {
            if let hostScreen = Self.bestScreen(for: currentFrame, in: allScreens) {
                return Self.clampFrame(currentFrame, toVisibleFrame: hostScreen.visibleFrame, minSize: minSize)
            }
            let size = resolveSize(for: targetScreen)
            return Self.centeredFrame(preservingSize: size, on: targetScreen, minSize: minSize)
        }
        let size = resolveSize(for: targetScreen)
        return Self.centeredFrame(preservingSize: size, on: targetScreen, minSize: minSize)
    }

    /// 判断跨屏溢出保护是否应继续延后：系统事件源左键仍处于按下态（含触控板三指拖移）时必须延后，松开后放行
    static func shouldDeferOverflowClamp(isLeftMouseButtonDown: Bool) -> Bool {
        isLeftMouseButtonDown
    }

    /// 跨屏拖拽彻底松手后的安全溢出保护：
    /// 仅当窗口宽度或高度真实超出目标屏可见工作区时，才将溢出维度按鼠标锚点温和收缩回可见区内；
    /// 若窗口可完整容纳于目标屏内，返回 `nil`，完全尊重原生拖拽落点，绝不抢夺或跳变。
    func clampedFrameForScreenOverflow(
        currentFrame: NSRect,
        newScreen: ScreenMetrics,
        anchorX: CGFloat? = nil
    ) -> NSRect? {
        let visible = newScreen.visibleFrame
        let widthOverflows = currentFrame.width > visible.width + 1.0
        let heightOverflows = currentFrame.height > visible.height + 1.0
        guard widthOverflows || heightOverflows else {
            return nil
        }

        let targetWidth = widthOverflows ? max(visible.width, 1) : currentFrame.width
        let targetHeight = heightOverflows ? max(visible.height, 1) : currentFrame.height
        let clampedSize = Self.clampSize(
            NSSize(width: targetWidth, height: targetHeight),
            toVisibleFrame: visible,
            minSize: minSize
        )

        // 宽度收缩时按鼠标在标题栏的水平相对位置比例锚定；未溢出的维度保持原落点不产生多余位移
        let finalX: CGFloat
        if widthOverflows {
            let ratio: CGFloat
            if let anchorX, currentFrame.width > 0, anchorX >= currentFrame.minX, anchorX <= currentFrame.maxX {
                ratio = (anchorX - currentFrame.minX) / currentFrame.width
            } else {
                ratio = 0.5
            }
            let proposedX = (anchorX ?? currentFrame.midX) - clampedSize.width * ratio
            let maxX = max(visible.origin.x, visible.maxX - clampedSize.width)
            finalX = min(max(proposedX, visible.origin.x), maxX)
        } else {
            finalX = currentFrame.origin.x
        }

        let finalY: CGFloat
        if heightOverflows {
            let proposedY = currentFrame.maxY - clampedSize.height
            let maxY = max(visible.origin.y, visible.maxY - clampedSize.height)
            finalY = min(max(proposedY, visible.origin.y), maxY)
        } else {
            finalY = currentFrame.origin.y
        }

        return NSRect(x: round(finalX), y: round(finalY), width: clampedSize.width, height: clampedSize.height)
    }
}
