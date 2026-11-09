import AppKit

/// 搜索框内联正则表达式模式控制按钮。
/// 三态视觉：幽灵浅灰 / 柔和微光 / 强调色实心；等宽半角字型消解点号与星号不对齐瑕疵。
final class RegexModeButton: InlinePillButton {
    override var buttonText: String { ".*" }
    override var labelFont: NSFont { .monospacedSystemFont(ofSize: 11, weight: .semibold) }

    override func tooltipText(for state: VisualState) -> String {
        switch state {
        case .inactive, .inactiveSlashed:
            return L("search.regex.inactive")
        case .autoActive:
            return L("search.regex.autoActive")
        case .locked:
            return L("search.regex.locked")
        }
    }
}
