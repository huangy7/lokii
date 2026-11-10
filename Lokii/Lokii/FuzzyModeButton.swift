import AppKit

/// 搜索框内联模糊跳字匹配模式控制按钮。
/// 视觉状态：幽灵浅灰（未激活）/ 强调色实心（锁定开启模糊跳字匹配）；等宽半角波浪符 `~` 居中排印。
final class FuzzyModeButton: InlinePillButton {
    override var buttonText: String { "~" }
    override var labelFont: NSFont { .monospacedSystemFont(ofSize: 13, weight: .semibold) }

    override func tooltipText(for state: VisualState) -> String {
        switch state {
        case .inactive, .inactiveSlashed, .autoActive:
            return L("search.fuzzy.inactive")
        case .locked:
            return L("search.fuzzy.locked")
        }
    }
}
