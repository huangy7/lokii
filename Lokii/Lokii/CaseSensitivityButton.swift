import AppKit

/// 搜索框内联大小写敏感控制按钮。
/// 四态视觉：自动浅灰 / 自动微光 / 严格大写实心 / 忽略大写斜杠。
final class CaseSensitivityButton: InlinePillButton {
    override var buttonText: String { "Aa" }

    override func tooltipText(for state: VisualState) -> String {
        switch state {
        case .inactive:
            return L("search.caseSensitivity.auto")
        case .autoActive:
            return L("search.caseSensitivity.autoActive")
        case .locked:
            return L("search.caseSensitivity.sensitive")
        case .inactiveSlashed:
            return L("search.caseSensitivity.insensitive")
        }
    }
}
