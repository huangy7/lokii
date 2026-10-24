import Cocoa

// MARK: - 过滤状态与检索语法协调数据结构

/// 表示用户在过滤浮层或搜索栏中激活的多维条件约束
public struct FilterState: Equatable, Hashable, Sendable {
    /// 选定的文件类型集合
    public var selectedTypes: Set<FfiFileType>

    /// 文件大小字节下限（包含）
    public var minSizeBytes: UInt64?

    /// 文件大小字节上限（包含）
    public var maxSizeBytes: UInt64?

    /// 具体文件类型枚举列表（排除 All）
    public static let concreteFileTypes: [FfiFileType] = [
        .code, .document, .image, .media, .archive, .folder
    ]
    public static let concreteFileTypeSet: Set<FfiFileType> = Set(concreteFileTypes)

    public init(
        selectedTypes: Set<FfiFileType> = [],
        minSizeBytes: UInt64? = nil,
        maxSizeBytes: UInt64? = nil
    ) {
        self.selectedTypes = selectedTypes
        self.minSizeBytes = minSizeBytes
        self.maxSizeBytes = maxSizeBytes
    }

    /// 判断文件类型维度是否具备实质性过滤约束
    /// 当为空、显式包含 .all、或覆盖全量具体类型时，均视为无类型约束
    public var hasTypeConstraint: Bool {
        if selectedTypes.isEmpty || selectedTypes.contains(.all) {
            return false
        }
        return !Self.concreteFileTypeSet.isSubset(of: selectedTypes)
    }

    /// 激活的过滤条件维度数量（供工具栏过滤胶囊按钮显示角标数值）
    public var activeCount: Int {
        var count = 0
        if hasTypeConstraint {
            count += 1
        }
        if minSizeBytes != nil || maxSizeBytes != nil {
            count += 1
        }
        return count
    }

    /// 是否处于没有任何条件约束的完全放行状态
    public var isUnconstrained: Bool {
        return activeCount == 0
    }

    /// 重置所有过滤条件为初始未受限状态
    public mutating func clearAll() {
        selectedTypes.removeAll()
        minSizeBytes = nil
        maxSizeBytes = nil
    }
}

// MARK: - 过滤语法双向协调器

/// 负责 AppKit 搜索输入框文本语法与 SwiftUI 过滤气泡浮层状态之间的无损双向映射与重入同步。
/// 核心保障：
/// 1. 单向重入锁（Re-entrancy Lock）：切断 UI 状态与文本编辑器之间的死循环震荡；
/// 2. 输入法安全区（IME Composition Guard）：在中文或多字节组字未提交时暂缓覆写输入框；
/// 3. 光标与选择区恢复：变更过滤标记后尽可能维持用户当前正在编辑的光标偏移量。
public final class FilterQuerySyncCoordinator {
    /// 同步守卫锁，防止双向联动中的递归重入
    private var isSynchronizing = false

    public init() {}

    // MARK: - 静态语法序列化与反序列化

    /// 将纯关键字与过滤状态拼接为规范的内联查询字符串
    public static func serialize(keyword: String, state: FilterState) -> String {
        var tokens: [String] = []

        let trimmedKeyword = keyword.trimmingCharacters(in: .whitespaces)
        if !trimmedKeyword.isEmpty {
            tokens.append(trimmedKeyword)
        }

        // 1. 文件类型语法: type:code,doc
        if state.hasTypeConstraint {
            let sortedTypes = FilterState.concreteFileTypes.filter { state.selectedTypes.contains($0) }
            let categoryNames = sortedTypes.compactMap { category(forType: $0) }
            if !categoryNames.isEmpty {
                tokens.append("type:\(categoryNames.joined(separator: ","))")
            }
        }

        // 2. 文件大小语法: size:>10m, size:<500k, size:10m..50m
        if let minBytes = state.minSizeBytes, let maxBytes = state.maxSizeBytes {
            tokens.append("size:\(formatBytes(minBytes))..\(formatBytes(maxBytes))")
        } else if let minBytes = state.minSizeBytes {
            tokens.append("size:>\(formatBytes(minBytes))")
        } else if let maxBytes = state.maxSizeBytes {
            tokens.append("size:<\(formatBytes(maxBytes))")
        }

        return tokens.joined(separator: " ")
    }

    /// 从复合查询字符串中提取剥离内联语法后的纯检索关键字，并还原结构化 FilterState
    public static func deserialize(query: String) -> (keyword: String, state: FilterState) {
        let trimmed = query.trimmingCharacters(in: .whitespaces)
        if trimmed.isEmpty {
            return ("", FilterState())
        }

        var remainingTerms: [String] = []
        var state = FilterState()

        let rawTerms = trimmed.components(separatedBy: .whitespaces).filter { !$0.isEmpty }

        for term in rawTerms {
            let lower = term.lowercased()

            // 1. type:<category>（支持逗号切分的复合分类，如 type:code,doc）
            if lower.hasPrefix("type:") {
                let catPayload = String(term.dropFirst(5))
                let parts = catPayload.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }

                if !parts.isEmpty {
                    let parsedTypes = parts.map { fileType(fromCategory: $0) }
                    if parsedTypes.allSatisfy({ $0 != nil }) {
                        for ft in parsedTypes.compactMap({ $0 }) {
                            if ft == .all {
                                state.selectedTypes.removeAll()
                            } else {
                                state.selectedTypes.insert(ft)
                            }
                        }
                        continue
                    }
                }
                // 未知分类保留为纯检索词
                remainingTerms.append(term)
                continue
            }

            // 2. size:<condition>（支持 size:>10m, size:<500k, size:10m..50m 等）
            if lower.hasPrefix("size:") {
                let sizePayload = String(term.dropFirst(5))
                if let (minB, maxB) = parseSizeToken(sizePayload) {
                    if let minB = minB {
                        state.minSizeBytes = state.minSizeBytes.map { max($0, minB) } ?? minB
                    }
                    if let maxB = maxB {
                        state.maxSizeBytes = state.maxSizeBytes.map { min($0, maxB) } ?? maxB
                    }
                    continue
                }
                remainingTerms.append(term)
                continue
            }

            // 未命中任何语法的普通词项
            remainingTerms.append(term)
        }

        let keyword = remainingTerms.joined(separator: " ")
        return (keyword, state)
    }

    // MARK: - 辅助分类与数值解析器

    /// 根据分类简写或别名解析 FfiFileType
    public static func fileType(fromCategory category: String) -> FfiFileType? {
        let lower = category.trimmingCharacters(in: .whitespaces).lowercased()
        switch lower {
        case "all":
            return .all
        case "doc", "document":
            return .document
        case "code", "dev":
            return .code
        case "img", "image", "pic":
            return .image
        case "media", "video", "audio":
            return .media
        case "archive", "zip":
            return .archive
        case "folder", "dir":
            return .folder
        default:
            return nil
        }
    }

    /// 将 FfiFileType 映射为规范内联分类缩写
    public static func category(forType type: FfiFileType) -> String? {
        switch type {
        case .all:
            return nil
        case .code:
            return "code"
        case .document:
            return "doc"
        case .image:
            return "img"
        case .media:
            return "media"
        case .archive:
            return "archive"
        case .folder:
            return "folder"
        }
    }

    /// 解析十进制容量单位并换算为标准字节数（支持 b/k/kb/m/mb/g/gb/t/tb）
    public static func parseSizeBytes(_ raw: String) -> UInt64? {
        let s = raw.trimmingCharacters(in: .whitespaces).lowercased()
        if s.isEmpty { return nil }

        let multiplier: UInt64
        let numStr: String
        if s.hasSuffix("tb") {
            multiplier = 1_000_000_000_000
            numStr = String(s.dropLast(2))
        } else if s.hasSuffix("gb") {
            multiplier = 1_000_000_000
            numStr = String(s.dropLast(2))
        } else if s.hasSuffix("mb") {
            multiplier = 1_000_000
            numStr = String(s.dropLast(2))
        } else if s.hasSuffix("kb") {
            multiplier = 1_000
            numStr = String(s.dropLast(2))
        } else if s.hasSuffix("t") {
            multiplier = 1_000_000_000_000
            numStr = String(s.dropLast(1))
        } else if s.hasSuffix("g") {
            multiplier = 1_000_000_000
            numStr = String(s.dropLast(1))
        } else if s.hasSuffix("m") {
            multiplier = 1_000_000
            numStr = String(s.dropLast(1))
        } else if s.hasSuffix("k") {
            multiplier = 1_000
            numStr = String(s.dropLast(1))
        } else if s.hasSuffix("b") {
            multiplier = 1
            numStr = String(s.dropLast(1))
        } else {
            multiplier = 1
            numStr = s
        }

        let trimmedNum = numStr.trimmingCharacters(in: .whitespaces)
        if trimmedNum.isEmpty { return nil }

        if let intVal = UInt64(trimmedNum) {
            let (res, overflow) = intVal.multipliedReportingOverflow(by: multiplier)
            return overflow ? UInt64.max : res
        }

        if let doubleVal = Double(trimmedNum) {
            if doubleVal.isNaN || doubleVal.isInfinite || doubleVal < 0 {
                return nil
            }
            let res = doubleVal * Double(multiplier)
            if res >= Double(UInt64.max) {
                return UInt64.max
            }
            return UInt64(res.rounded())
        }

        return nil
    }

    /// 解析大小表达式负载（支持双端区间、大于/小于号以及等值数值）
    public static func parseSizeToken(_ payload: String) -> (min: UInt64?, max: UInt64?)? {
        let payload = payload.trimmingCharacters(in: .whitespaces)
        if payload.isEmpty { return nil }

        // 1. 双点区间语法：min..max
        if let dotRange = payload.range(of: "..") {
            let left = String(payload[..<dotRange.lowerBound]).trimmingCharacters(in: .whitespaces)
            let right = String(payload[dotRange.upperBound...]).trimmingCharacters(in: .whitespaces)
            if left.isEmpty && right.isEmpty { return nil }

            let minBytes: UInt64?
            if !left.isEmpty {
                guard let val = parseSizeBytes(left) else { return nil }
                minBytes = val
            } else {
                minBytes = nil
            }

            let maxBytes: UInt64?
            if !right.isEmpty {
                guard let val = parseSizeBytes(right) else { return nil }
                maxBytes = val
            } else {
                maxBytes = nil
            }
            return (min: minBytes, max: maxBytes)
        }

        // 2. 下限前缀：>= 或 >
        if payload.hasPrefix(">=") {
            let rest = String(payload.dropFirst(2))
            guard let val = parseSizeBytes(rest) else { return nil }
            return (min: val, max: nil)
        }
        if payload.hasPrefix(">") {
            let rest = String(payload.dropFirst(1))
            guard let val = parseSizeBytes(rest) else { return nil }
            return (min: val, max: nil)
        }

        // 3. 上限前缀：<= 或 <
        if payload.hasPrefix("<=") {
            let rest = String(payload.dropFirst(2))
            guard let val = parseSizeBytes(rest) else { return nil }
            return (min: nil, max: val)
        }
        if payload.hasPrefix("<") {
            let rest = String(payload.dropFirst(1))
            guard let val = parseSizeBytes(rest) else { return nil }
            return (min: nil, max: val)
        }

        // 4. 等值前缀：= 或直接数值匹配
        if payload.hasPrefix("=") {
            let rest = String(payload.dropFirst(1))
            guard let val = parseSizeBytes(rest) else { return nil }
            return (min: val, max: val)
        }

        if let val = parseSizeBytes(payload) {
            return (min: val, max: val)
        }

        return nil
    }

    /// 将字节数格式化为最适配的十进制字符串（如 10m, 500k, 1g 等）
    public static func formatBytes(_ bytes: UInt64) -> String {
        let tb: UInt64 = 1_000_000_000_000
        let gb: UInt64 = 1_000_000_000
        let mb: UInt64 = 1_000_000
        let kb: UInt64 = 1_000

        let posixLocale = Locale(identifier: "en_US_POSIX")
        if bytes >= tb && bytes % tb == 0 {
            return "\(bytes / tb)t"
        } else if bytes >= tb && bytes % (tb / 10) == 0 {
            let val = Double(bytes) / Double(tb)
            return String(format: "%.1ft", locale: posixLocale, val)
        } else if bytes >= gb && bytes % gb == 0 {
            return "\(bytes / gb)g"
        } else if bytes >= gb && bytes % (gb / 10) == 0 {
            let val = Double(bytes) / Double(gb)
            return String(format: "%.1fg", locale: posixLocale, val)
        } else if bytes >= mb && bytes % mb == 0 {
            return "\(bytes / mb)m"
        } else if bytes >= mb && bytes % (mb / 10) == 0 {
            let val = Double(bytes) / Double(mb)
            return String(format: "%.1fm", locale: posixLocale, val)
        } else if bytes >= kb && bytes % kb == 0 {
            return "\(bytes / kb)k"
        } else if bytes >= kb && bytes % (kb / 10) == 0 {
            let val = Double(bytes) / Double(kb)
            return String(format: "%.1fk", locale: posixLocale, val)
        } else {
            return "\(bytes)b"
        }
    }

    // MARK: - 语法高亮与限定词胶囊格式化

    /// 将检索字符串格式化为具备语法高亮与属性胶囊标记的带属性字符串。
    /// - 语法键名（type: / size: / ext:）：采用次要文本色，弱化视觉噪音；
    /// - 属性值（如 code, >10m, pdf）：采用强调前景色与微透明强调衬底胶囊样式，并在冒号处施加微字距以留出圆角呼吸空间；
    /// - 普通检索词项：保持默认前景色与排版。
    public static func formatQuerySyntax(
        _ query: String,
        baseFont: NSFont = .systemFont(ofSize: 14)
    ) -> NSAttributedString {
        let result = NSMutableAttributedString(string: query)
        let fullRange = NSRange(location: 0, length: (query as NSString).length)
        guard fullRange.length > 0 else { return result }

        // 默认全局样式
        result.setAttributes([
            .font: baseFont,
            .foregroundColor: NSColor.labelColor
        ], range: fullRange)

        // 匹配限定词语法：(type|size|ext):(\S*)
        let pattern = "(?i)\\b(type|size|ext):([^\\s]*)"
        guard let regex = try? NSRegularExpression(pattern: pattern) else {
            return result
        }

        let matches = regex.matches(in: query, range: fullRange)
        for match in matches {
            let keyRange = match.range(at: 1)
            let colonRange = NSRange(location: keyRange.location + keyRange.length, length: 1)
            let valRange = match.range(at: 2)

            let prefixRange = NSRange(location: keyRange.location, length: keyRange.length + colonRange.length)

            // 键名与冒号（如 type:）：次要前景色
            result.addAttribute(.foregroundColor, value: NSColor.secondaryLabelColor, range: prefixRange)
            result.addAttribute(.font, value: baseFont, range: prefixRange)

            // 属性值（如 code, >10m, pdf）：强调前景色 + 半透明圆角背景胶囊
            if valRange.length > 0 {
                // 在冒号后增加 2pt 字距，使右侧圆角胶囊左缘不紧贴冒号
                result.addAttribute(.kern, value: 2.0, range: colonRange)

                let pillBackground = NSColor(name: nil) { appearance in
                    let isDark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
                    return isDark
                        ? NSColor.controlAccentColor.withAlphaComponent(0.24)
                        : NSColor.controlAccentColor.withAlphaComponent(0.14)
                }
                result.addAttribute(.foregroundColor, value: NSColor.controlAccentColor, range: valRange)
                result.addAttribute(.backgroundColor, value: pillBackground, range: valRange)
                result.addAttribute(.font, value: NSFont.systemFont(ofSize: baseFont.pointSize, weight: .medium), range: valRange)
            }
        }

        return result
    }

    // MARK: - 状态与输入框双向联动

    /// UI 侧变动驱动搜索框文本更新与即时检索触发
    /// 具备重入锁拦截与输入法组字守卫，并在过滤条件激活时自动追加尾随空格且将光标推进至末尾
    public func updateQueryFromUI(
        state: FilterState,
        searchField: NSSearchField,
        performSearch: (String) -> Void
    ) {
        guard !isSynchronizing else { return }
        isSynchronizing = true
        defer { isSynchronizing = false }

        // 检查当前输入框活跃的字段编辑器
        let fieldEditor = searchField.currentEditor() as? NSTextView
        if fieldEditor?.hasMarkedText() == true {
            // 输入法正在进行多字节（如中文拼音）组字，暂缓覆写，避免破坏未提交的拼音流
            return
        }

        let currentQuery = searchField.stringValue
        let (keyword, _) = Self.deserialize(query: currentQuery)
        let serialized = Self.serialize(keyword: keyword, state: state)
        let newQuery = (!serialized.isEmpty && !state.isUnconstrained) ? "\(serialized) " : serialized

        let formatted = Self.formatQuerySyntax(newQuery, baseFont: searchField.font ?? .systemFont(ofSize: 14))
        searchField.attributedStringValue = formatted

        // 若当前处于编辑状态，同步更新字段编辑器中的文字属性并将光标推进至文本末尾（便于直接继续键入检索词）
        if let fieldEditor = fieldEditor, let storage = fieldEditor.textStorage {
            PillLayoutManager.ensureInstalled(on: fieldEditor)
            storage.beginEditing()
            storage.setAttributedString(formatted)
            storage.endEditing()
            let textLength = (newQuery as NSString).length
            fieldEditor.selectedRange = NSRange(location: textLength, length: 0)
        }

        performSearch(newQuery)
    }

    /// 搜索框键盘键入变动逆向驱动浮层 UI 状态回填
    public func updateUIFromQuery(
        query: String,
        updateState: (FilterState) -> Void
    ) {
        guard !isSynchronizing else { return }
        isSynchronizing = true
        defer { isSynchronizing = false }

        let (_, state) = Self.deserialize(query: query)
        updateState(state)
    }
}

// MARK: - 语法胶囊圆角背景排版管理器

/// 自定义排版管理器，将搜索框内限定词属性值的背景色块绘制为平滑圆角胶囊，同时保留原生文本选区矩形行为
public final class PillLayoutManager: NSLayoutManager {
    public static func ensureInstalled(on textView: NSTextView) {
        guard let container = textView.textContainer,
              !(textView.layoutManager is PillLayoutManager) else {
            return
        }
        container.replaceLayoutManager(PillLayoutManager())
    }

    public override func fillBackgroundRectArray(
        _ rectArray: UnsafePointer<NSRect>,
        count rectCount: Int,
        forCharacterRange charRange: NSRange,
        color: NSColor
    ) {
        // 系统文本选区高亮保持默认矩形绘制，仅对语法限定词胶囊背景做圆角化处理
        if color == NSColor.selectedTextBackgroundColor {
            super.fillBackgroundRectArray(rectArray, count: rectCount, forCharacterRange: charRange, color: color)
            return
        }
        color.setFill()
        for i in 0..<rectCount {
            let rect = rectArray[i].insetBy(dx: -2.0, dy: 0.5)
            let path = NSBezierPath(roundedRect: rect, xRadius: 4.0, yRadius: 4.0)
            path.fill()
        }
    }
}
