import AppKit
import Foundation

/// 搜索窗口呈现形态：完整双栏标准窗口或无标题栏用完即走的悬浮搜索条
enum WindowPresentationMode: String, CaseIterable, Equatable {
    case standardWindow = "standard"
    case floatingBar = "floating_bar"
}

/// 两种窗口形态之间热切换时透传的检索上下文，确保查询词、匹配模式与大小写策略无缝延续
struct SearchHandoffState: Equatable {
    let query: String
    let mode: FfiSearchMode
    let caseSensitivity: FfiCaseSensitivity
}

/// 窗口呈现模式持久化存储与变更广播中心
final class WindowPresentationModeStore {
    static let shared = WindowPresentationModeStore()
    static let userDefaultsKey = "LokiiWindowPresentationMode"
    static let didChangeNotification = Notification.Name("LokiiWindowPresentationModeDidChange")

    private let defaults: UserDefaults
    private let notificationCenter: NotificationCenter

    init(
        defaults: UserDefaults = .standard,
        notificationCenter: NotificationCenter = .default
    ) {
        self.defaults = defaults
        self.notificationCenter = notificationCenter
    }

    var currentMode: WindowPresentationMode {
        get {
            guard let raw = defaults.string(forKey: Self.userDefaultsKey),
                  let mode = WindowPresentationMode(rawValue: raw) else {
                return .standardWindow
            }
            return mode
        }
        set {
            let previous = currentMode
            defaults.set(newValue.rawValue, forKey: Self.userDefaultsKey)
            if previous != newValue {
                notificationCenter.post(
                    name: Self.didChangeNotification,
                    object: self,
                    userInfo: ["mode": newValue.rawValue]
                )
            }
        }
    }
}

/// 悬浮搜索条左上角（minX, maxY）坐标持久化仓库：以顶边锚点记录位置，避免展开与收折改变面板高度时产生偏移
final class FloatingBarPositionStore {
    static let shared = FloatingBarPositionStore()
    static let topLeftOriginKey = "LokiiFloatingBarTopLeftOrigin"

    private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    func savedTopLeft() -> NSPoint? {
        guard let raw = defaults.string(forKey: Self.topLeftOriginKey), !raw.isEmpty else {
            return nil
        }
        return NSPointFromString(raw)
    }

    func saveTopLeft(_ point: NSPoint) {
        let rounded = NSPoint(x: round(point.x), y: round(point.y))
        defaults.set(NSStringFromPoint(rounded), forKey: Self.topLeftOriginKey)
    }

    func clearTopLeft() {
        defaults.removeObject(forKey: Self.topLeftOriginKey)
    }
}

/// 悬浮搜索条纯几何布局计算器：负责液态玻璃圆角形态、收折/展开高度、托盘下方停靠与顶部锚定边界钳制
struct FloatingSearchBarLayout {
    static let defaultWidth: CGFloat = 580
    static let inputBarHeight: CGFloat = 58
    static let dividerHeight: CGFloat = 1
    static let filterStripHeight: CGFloat = 38
    static let filterOnlyBottomPadding: CGFloat = 6
    static let bodyTopPadding: CGFloat = 6
    static let bodyBottomPadding: CGFloat = 8
    static let rowHeight: CGFloat = 54
    static let maxVisibleRows: Int = 7
    static let emptyContentHeight: CGFloat = 76
    static let collapsedCornerRadius: CGFloat = 29
    static let expandedCornerRadius: CGFloat = 24
    static let trayAnchorGap: CGFloat = 8
    static let horizontalScreenMargin: CGFloat = 16
    static let bottomScreenMargin: CGFloat = 14
    static let topAnchorRatio: CGFloat = 0.22

    static func expandedChromeHeight(filterBarExpanded: Bool = false) -> CGFloat {
        let filterChrome = filterBarExpanded ? filterStripHeight : 0
        return inputBarHeight + dividerHeight + filterChrome + bodyTopPadding + bodyBottomPadding
    }

    static var expandedChromeHeight: CGFloat {
        expandedChromeHeight(filterBarExpanded: false)
    }

    enum ExpansionState: Equatable {
        case collapsed
        case results(count: Int)
        case message
    }

    static func cornerRadius(for state: ExpansionState, filterBarExpanded: Bool = false) -> CGFloat {
        if state == .collapsed && !filterBarExpanded {
            return collapsedCornerRadius
        }
        return expandedCornerRadius
    }

    static func panelWidth(for screen: ScreenMetrics) -> CGFloat {
        round(min(defaultWidth, max(320, screen.visibleFrame.width - horizontalScreenMargin * 2)))
    }

    static func visibleRowCount(
        for resultCount: Int,
        maxVisibleHeight: CGFloat,
        filterBarExpanded: Bool = false
    ) -> Int {
        let chrome = expandedChromeHeight(filterBarExpanded: filterBarExpanded)
        let maxPanelHeight = max(chrome + rowHeight, floor(maxVisibleHeight * 0.72))
        let maxFittingRows = max(1, Int(floor((maxPanelHeight - chrome) / rowHeight)))
        return min(max(resultCount, 1), min(maxVisibleRows, maxFittingRows))
    }

    static func targetHeight(
        for state: ExpansionState,
        maxVisibleHeight: CGFloat,
        filterBarExpanded: Bool = false
    ) -> CGFloat {
        switch state {
        case .collapsed:
            if filterBarExpanded {
                return inputBarHeight + dividerHeight + filterStripHeight + filterOnlyBottomPadding
            }
            return inputBarHeight
        case .results(let count):
            let chrome = expandedChromeHeight(filterBarExpanded: filterBarExpanded)
            let rows = visibleRowCount(
                for: count,
                maxVisibleHeight: maxVisibleHeight,
                filterBarExpanded: filterBarExpanded
            )
            return round(chrome + CGFloat(rows) * rowHeight)
        case .message:
            let chrome = expandedChromeHeight(filterBarExpanded: filterBarExpanded)
            let maxPanelHeight = max(chrome + emptyContentHeight, floor(maxVisibleHeight * 0.72))
            return round(min(chrome + emptyContentHeight, maxPanelHeight))
        }
    }

    /// 将当前纵向滚动偏移吸附到最近的整行边界，避免停留在半行裁切位置
    static func snappedScrollOffsetY(
        currentY: CGFloat,
        contentHeight: CGFloat,
        viewportHeight: CGFloat,
        rowHeight: CGFloat = FloatingSearchBarLayout.rowHeight
    ) -> CGFloat {
        let maxOffset = max(0, contentHeight - viewportHeight)
        guard maxOffset > 0, rowHeight > 0 else { return 0 }
        let clamped = min(max(currentY, 0), maxOffset)
        let snapped = round(clamped / rowHeight) * rowHeight
        return min(max(snapped, 0), maxOffset)
    }

    static func initialFrame(
        on screen: ScreenMetrics,
        state: ExpansionState,
        filterBarExpanded: Bool = false
    ) -> NSRect {
        let visible = screen.visibleFrame
        let width = panelWidth(for: screen)
        let height = targetHeight(for: state, maxVisibleHeight: visible.height, filterBarExpanded: filterBarExpanded)
        let x = round(visible.minX + (visible.width - width) / 2.0)
        let topY = visible.maxY - round(visible.height * topAnchorRatio)
        let minY = visible.minY + bottomScreenMargin
        let maxY = max(minY, visible.maxY - height)
        let y = round(max(minY, min(topY - height, maxY)))
        return NSRect(x: x, y: y, width: width, height: height)
    }

    /// 停靠在状态栏托盘图标正下方，居中对齐锚点并钳制在目标屏幕安全边距内
    static func anchoredFrame(
        below anchorRect: NSRect,
        state: ExpansionState,
        on screen: ScreenMetrics,
        filterBarExpanded: Bool = false
    ) -> NSRect {
        let visible = screen.visibleFrame
        let width = panelWidth(for: screen)
        let height = targetHeight(for: state, maxVisibleHeight: visible.height, filterBarExpanded: filterBarExpanded)
        let minX = visible.minX + horizontalScreenMargin
        let maxX = max(minX, visible.maxX - width - horizontalScreenMargin)
        let rawX = anchorRect.midX - width / 2.0
        let x = round(min(max(rawX, minX), maxX))

        let topY = min(anchorRect.minY - trayAnchorGap, visible.maxY - trayAnchorGap)
        let minY = visible.minY + bottomScreenMargin
        let maxY = max(minY, visible.maxY - height)
        let y = round(min(max(topY - height, minY), maxY))
        return NSRect(x: x, y: y, width: width, height: height)
    }

    /// 基于记住的左上角（minX, maxY）恢复悬浮条窗口，确保高度随收折/展开变化时顶边严丝合缝
    static func frame(
        fromTopLeft topLeft: NSPoint,
        state: ExpansionState,
        on screen: ScreenMetrics,
        filterBarExpanded: Bool = false
    ) -> NSRect {
        let visible = screen.visibleFrame
        let width = panelWidth(for: screen)
        let height = targetHeight(for: state, maxVisibleHeight: visible.height, filterBarExpanded: filterBarExpanded)
        let maxX = max(visible.minX, visible.maxX - width)
        let x = round(min(max(topLeft.x, visible.minX), maxX))

        let topY = min(topLeft.y, visible.maxY)
        let minY = visible.minY + bottomScreenMargin
        let maxY = max(minY, visible.maxY - height)
        let y = round(min(max(topY - height, minY), maxY))
        return NSRect(x: x, y: y, width: width, height: height)
    }

    /// 统一呼出位置决策：记住位置优先 -> 托盘锚点停靠 -> 目标屏幕中上居中
    static func resolveShowFrame(
        state: ExpansionState,
        rememberPosition: Bool,
        savedTopLeft: NSPoint?,
        anchorRect: NSRect?,
        targetScreen: ScreenMetrics,
        allScreens: [ScreenMetrics],
        filterBarExpanded: Bool = false
    ) -> NSRect {
        if rememberPosition, let savedTopLeft {
            let probeWidth = defaultWidth
            let probeRect = NSRect(x: savedTopLeft.x, y: savedTopLeft.y - inputBarHeight, width: probeWidth, height: inputBarHeight)
            let hostScreen = allScreens.first { $0.frame.contains(savedTopLeft) }
                ?? WindowFrameManager.bestScreen(for: probeRect, in: allScreens)
            if let hostScreen {
                return frame(fromTopLeft: savedTopLeft, state: state, on: hostScreen, filterBarExpanded: filterBarExpanded)
            }
        }
        if let anchorRect {
            let anchorMid = NSPoint(x: anchorRect.midX, y: anchorRect.midY)
            let anchorScreen = allScreens.first { $0.frame.contains(anchorMid) } ?? targetScreen
            return anchoredFrame(below: anchorRect, state: state, on: anchorScreen, filterBarExpanded: filterBarExpanded)
        }
        return initialFrame(on: targetScreen, state: state, filterBarExpanded: filterBarExpanded)
    }

    /// 展开与收折时保持搜索条顶边（maxY）静止，仅向下伸缩高度并在触及屏幕底边时温和上移
    static func resizedFrame(
        preservingTopOf currentFrame: NSRect,
        newHeight: CGFloat,
        on screen: ScreenMetrics
    ) -> NSRect {
        let visible = screen.visibleFrame
        let maxWidth = panelWidth(for: screen)
        let width = round(min(max(currentFrame.width, 320), maxWidth))
        let topY = currentFrame.maxY
        let rawY = topY - newHeight
        let minY = visible.minY + bottomScreenMargin
        let maxAllowedY = max(minY, visible.maxY - newHeight)
        let y = round(max(minY, min(rawY, maxAllowedY)))
        let maxAllowedX = max(visible.minX, visible.maxX - width)
        let x = round(min(max(currentFrame.origin.x, visible.minX), maxAllowedX))
        return NSRect(x: x, y: y, width: width, height: round(newHeight))
    }
}
