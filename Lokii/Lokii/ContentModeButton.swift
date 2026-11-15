import AppKit

/// 搜索框内联全文内容检索模式开关胶囊。
/// 三态视觉与焦点封锁行为与同栏 `FuzzyModeButton` / `RegexModeButton` 保持一致：
/// - `inactive`：纯文件名瞬时检索路径
/// - `autoActive`：用户在查询串中显式输入了 `content:` / `grep:` / `c:` / `cregex:` / `sym:`
/// - `locked`：强制开启全词内容检索模式（所有非过滤词元同时匹配正文与符号定义）
final class ContentModeButton: InlinePillButton {
    override var buttonText: String { "IN" }
    override var labelFont: NSFont { .monospacedSystemFont(ofSize: 10.5, weight: .bold) }

    override func tooltipText(for state: VisualState) -> String {
        switch state {
        case .inactive, .inactiveSlashed:
            return L("search.content.inactive")
        case .autoActive:
            return L("search.content.autoActive")
        case .locked:
            return L("search.content.locked")
        }
    }
}
