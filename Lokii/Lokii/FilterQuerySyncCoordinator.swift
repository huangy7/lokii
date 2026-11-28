import Cocoa

// MARK: - 过滤状态与检索语法协调数据结构

/// 过滤浮层中的修改时间预设档位，对应内联 `mtime:` 语法词元。
public enum ModifiedTimePreset: String, CaseIterable, Identifiable, Hashable, Sendable {
    case last7Days = "<7d"
    case last30Days = "<30d"
    case olderThan90Days = ">90d"
    case olderThan180Days = ">180d"
    case olderThan1Year = ">1y"

    public var id: String { rawValue }

    public var queryToken: String { "mtime:\(rawValue)" }

    public var titleKey: String {
        switch self {
        case .last7Days: return "filter.modified.last7Days"
        case .last30Days: return "filter.modified.last30Days"
        case .olderThan90Days: return "filter.modified.olderThan90Days"
        case .olderThan180Days: return "filter.modified.olderThan180Days"
        case .olderThan1Year: return "filter.modified.olderThan1Year"
        }
    }
}

/// 修改时间过滤状态：预设胶囊或用户手写的自定义时间语法词元。
public enum ModifiedTimeFilter: Equatable, Hashable, Sendable {
    case preset(ModifiedTimePreset)
    /// 完整保留搜索框中手写的自定义时间词元（如 `["mtime:2025-01-01..2025-12-31"]`），
    /// 避免在浮层切换文件类型或大小时丢失原有时间表达式。
    case custom([String])
}

/// 表示用户在过滤浮层或搜索栏中激活的多维条件约束
public struct FilterState: Equatable, Hashable, Sendable {
    /// 选定的文件类型集合
    public var selectedTypes: Set<FfiFileType>

    /// 文件大小字节下限（包含）
    public var minSizeBytes: UInt64?

    /// 文件大小字节上限（包含）
    public var maxSizeBytes: UInt64?

    /// 修改时间过滤条件
    public var modifiedTime: ModifiedTimeFilter?

    /// 具体文件类型枚举列表（排除 All）
    public static let concreteFileTypes: [FfiFileType] = [
        .code, .document, .image, .media, .archive, .folder
    ]
    public static let concreteFileTypeSet: Set<FfiFileType> = Set(concreteFileTypes)

    public init(
        selectedTypes: Set<FfiFileType> = [],
        minSizeBytes: UInt64? = nil,
        maxSizeBytes: UInt64? = nil,
        modifiedTime: ModifiedTimeFilter? = nil
    ) {
        self.selectedTypes = selectedTypes
        self.minSizeBytes = minSizeBytes
        self.maxSizeBytes = maxSizeBytes
        self.modifiedTime = modifiedTime
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
        if modifiedTime != nil {
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
        modifiedTime = nil
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

    /// 当输入法正在组字时，暂存用户在浮层中触发的待应用过滤状态
    public private(set) var pendingState: FilterState?

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

        // 3. 修改时间语法: mtime:<7d, mtime:>90d 或用户自定义表达
        if let modifiedTime = state.modifiedTime {
            switch modifiedTime {
            case .preset(let preset):
                tokens.append(preset.queryToken)
            case .custom(let customTokens):
                for tok in customTokens {
                    let clean = tok.trimmingCharacters(in: .whitespaces)
                    if !clean.isEmpty {
                        tokens.append(clean)
                    }
                }
            }
        }

        return tokens.joined(separator: " ")
    }

    /// 查找成对双引号的闭合索引（忽略 `\"` 转义引号），未闭合返回 nil。
    private static func findClosingDoubleQuote(in query: String, from start: String.Index) -> String.Index? {
        var j = start
        while j < query.endIndex {
            if query[j] == "\\" {
                let next = query.index(after: j)
                j = next < query.endIndex ? query.index(after: next) : next
                continue
            }
            if query[j] == "\"" {
                return j
            }
            j = query.index(after: j)
        }
        return nil
    }

    /// 扫描查询串的原始词元区间及引号/否定保护标记，供反序列化与语法高亮共享词法状态机。
    private static func scanSyntaxTokens(in query: String) -> [(raw: String, nsRange: NSRange, isProtected: Bool)] {
        var results: [(raw: String, nsRange: NSRange, isProtected: Bool)] = []
        var i = query.startIndex

        while i < query.endIndex {
            while i < query.endIndex && query[i].isWhitespace {
                i = query.index(after: i)
            }
            if i >= query.endIndex {
                break
            }

            let tokenStart = i
            var hasPairedQuote = false
            var hasEscapedMeta = false
            var startsWithNegation = false

            if query[i] == "!" {
                startsWithNegation = true
                i = query.index(after: i)
            }

            while i < query.endIndex && !query[i].isWhitespace {
                if query[i] == "\"" {
                    let afterOpen = query.index(after: i)
                    if let close = findClosingDoubleQuote(in: query, from: afterOpen) {
                        hasPairedQuote = true
                        i = query.index(after: close)
                        continue
                    }
                    i = afterOpen
                    continue
                }
                if query[i] == "\\" {
                    let next = query.index(after: i)
                    if next < query.endIndex {
                        let nextChar = query[next]
                        if nextChar == "\"" || nextChar == "!" || nextChar == "|" {
                            hasEscapedMeta = true
                            i = query.index(after: next)
                            continue
                        }
                    }
                    i = next
                    continue
                }
                i = query.index(after: i)
            }

            let range = tokenStart..<i
            let raw = String(query[range])
            let isProtected = startsWithNegation || hasPairedQuote || hasEscapedMeta || raw.lowercased().hasPrefix("regex:")
            results.append((raw: raw, nsRange: NSRange(range, in: query), isProtected: isProtected))
        }

        return results
    }

    /// 从复合查询字符串中提取剥离内联语法后的纯检索关键字，并还原结构化 FilterState（保留双引号与否定保护词元原貌）
    public static func deserialize(query: String) -> (keyword: String, state: FilterState) {
        let trimmed = query.trimmingCharacters(in: .whitespaces)
        if trimmed.isEmpty {
            return ("", FilterState())
        }

        var remainingTerms: [String] = []
        var state = FilterState()
        var validModifiedTokens: [(raw: String, payload: String)] = []

        for token in scanSyntaxTokens(in: trimmed) {
            let term = token.raw
            if token.isProtected {
                remainingTerms.append(term)
                continue
            }

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

            // 3. mtime:<condition> / modified:<condition>
            if lower.hasPrefix("mtime:") || lower.hasPrefix("modified:") {
                let prefixLen = lower.hasPrefix("mtime:") ? 6 : 9
                let modPayload = String(term.dropFirst(prefixLen))
                if isValidModifiedTimePayload(modPayload) {
                    validModifiedTokens.append((raw: term, payload: modPayload))
                    continue
                }
                remainingTerms.append(term)
                continue
            }

            // 未命中任何语法的普通词项
            remainingTerms.append(term)
        }

        if validModifiedTokens.count == 1,
           let preset = modifiedTimePreset(forPayload: validModifiedTokens[0].payload) {
            state.modifiedTime = .preset(preset)
        } else if !validModifiedTokens.isEmpty {
            state.modifiedTime = .custom(validModifiedTokens.map(\.raw))
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

    // MARK: - 修改时间语法校验与预设映射

    private static let modifiedDurationUnits: Set<String> = [
        "min", "mins", "minute", "minutes",
        "h", "hr", "hrs", "hour", "hours",
        "d", "day", "days",
        "w", "wk", "wks", "week", "weeks",
        "m", "mo", "mos", "mon", "month", "months",
        "y", "yr", "yrs", "year", "years",
    ]

    private static func isValidRelativeDuration(_ raw: String) -> Bool {
        let s = raw.trimmingCharacters(in: .whitespaces)
        guard !s.isEmpty else { return false }
        guard let splitIdx = s.firstIndex(where: { !($0.isASCII && ($0.isNumber || $0 == ".")) }) else {
            return false
        }
        guard splitIdx > s.startIndex else { return false }
        let numStr = String(s[..<splitIdx])
        let unit = String(s[splitIdx...]).trimmingCharacters(in: .whitespaces).lowercased()
        guard modifiedDurationUnits.contains(unit) else { return false }
        if UInt64(numStr) != nil {
            return true
        }
        if let val = Double(numStr), !val.isNaN, !val.isInfinite, val >= 0 {
            return true
        }
        return false
    }

    private static func isValidCalendarDate(_ raw: String) -> Bool {
        let s = raw.trimmingCharacters(in: .whitespaces)
        let lower = s.lowercased()
        if lower == "today" || lower == "yesterday" {
            return true
        }
        let parts = s.split(separator: "-", omittingEmptySubsequences: false)
        switch parts.count {
        case 1:
            guard parts[0].count == 4, let year = Int(parts[0]), (1970...9999).contains(year) else {
                return false
            }
            return parts[0].allSatisfy { $0.isASCII && $0.isNumber }
        case 2:
            guard parts[0].count == 4, parts[1].count == 2,
                  parts[0].allSatisfy({ $0.isASCII && $0.isNumber }),
                  parts[1].allSatisfy({ $0.isASCII && $0.isNumber }),
                  let year = Int(parts[0]), (1970...9999).contains(year),
                  let month = Int(parts[1]), (1...12).contains(month) else {
                return false
            }
            return true
        case 3:
            guard parts[0].count == 4, parts[1].count == 2, parts[2].count == 2,
                  parts[0].allSatisfy({ $0.isASCII && $0.isNumber }),
                  parts[1].allSatisfy({ $0.isASCII && $0.isNumber }),
                  parts[2].allSatisfy({ $0.isASCII && $0.isNumber }),
                  let year = Int(parts[0]), (1970...9999).contains(year),
                  let month = Int(parts[1]), (1...12).contains(month),
                  let day = Int(parts[2]) else {
                return false
            }
            let maxDay: Int
            switch month {
            case 1, 3, 5, 7, 8, 10, 12: maxDay = 31
            case 4, 6, 9, 11: maxDay = 30
            case 2:
                let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
                maxDay = leap ? 29 : 28
            default:
                return false
            }
            return (1...maxDay).contains(day)
        default:
            return false
        }
    }

    public static func isValidModifiedTimePayload(_ payload: String) -> Bool {
        let trimmed = payload.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return false }

        if let dotRange = trimmed.range(of: "..") {
            let left = String(trimmed[..<dotRange.lowerBound]).trimmingCharacters(in: .whitespaces)
            let right = String(trimmed[dotRange.upperBound...]).trimmingCharacters(in: .whitespaces)
            guard !left.isEmpty || !right.isEmpty else { return false }
            let leftRel = left.isEmpty || isValidRelativeDuration(left)
            let rightRel = right.isEmpty || isValidRelativeDuration(right)
            if leftRel && rightRel {
                return true
            }
            let leftCal = left.isEmpty || isValidCalendarDate(left)
            let rightCal = right.isEmpty || isValidCalendarDate(right)
            return leftCal && rightCal
        }

        if trimmed.hasPrefix(">=") || trimmed.hasPrefix("<=") {
            let rest = String(trimmed.dropFirst(2))
            return isValidRelativeDuration(rest) || isValidCalendarDate(rest)
        }
        if trimmed.hasPrefix(">") || trimmed.hasPrefix("<") {
            let rest = String(trimmed.dropFirst(1))
            return isValidRelativeDuration(rest) || isValidCalendarDate(rest)
        }
        if trimmed.hasPrefix("=") {
            let rest = String(trimmed.dropFirst(1))
            return isValidCalendarDate(rest)
        }
        return isValidRelativeDuration(trimmed) || isValidCalendarDate(trimmed)
    }

    public static func modifiedTimePreset(forPayload payload: String) -> ModifiedTimePreset? {
        let lower = payload.trimmingCharacters(in: .whitespaces).lowercased()
        switch lower {
        case "<7d", "<=7d", "7d", "<7day", "<7days", "<1w", "<=1w", "1w", "<1wk", "<1week":
            return .last7Days
        case "<30d", "<=30d", "30d", "<30day", "<30days", "<1m", "<=1m", "1m", "<1mo", "<1month":
            return .last30Days
        case ">90d", ">=90d", ">90day", ">90days", ">3m", ">=3m", ">3mo", ">3mos", ">3month", ">3months":
            return .olderThan90Days
        case ">180d", ">=180d", ">180day", ">180days", ">6m", ">=6m", ">6mo", ">6mos", ">6month", ">6months":
            return .olderThan180Days
        case ">1y", ">=1y", ">1yr", ">1yrs", ">1year", ">1years", ">365d", ">=365d", ">12m", ">=12m", ">12mo", ">12month", ">12months":
            return .olderThan1Year
        default:
            return nil
        }
    }

    // MARK: - 语法高亮与限定词胶囊格式化

    /// 限定词语法：(type|size|ext|mtime|modified):(\S*)；高亮格式化与「是否需要触碰 textStorage」的判定共用一份，避免模式漂移
    private static let filterSyntaxRegex = try? NSRegularExpression(pattern: "(?i)\\b(type|size|ext|mtime|modified):([^\\s]*)")

    /// 判断给定的检索表达式中是否包含限定词语法（type: / size: / ext: / mtime: / modified:）
    public static func containsFilterSyntax(_ query: String) -> Bool {
        let fullRange = NSRange(location: 0, length: (query as NSString).length)
        return filterSyntaxRegex?.firstMatch(in: query, range: fullRange) != nil
    }

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

        guard let regex = filterSyntaxRegex else {
            return result
        }

        let pillBackground = NSColor(name: nil) { appearance in
            let isDark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
            return isDark
                ? NSColor.controlAccentColor.withAlphaComponent(0.24)
                : NSColor.controlAccentColor.withAlphaComponent(0.14)
        }

        for token in scanSyntaxTokens(in: query) where !token.isProtected {
            let tokenLocalRange = NSRange(location: 0, length: token.nsRange.length)
            guard let match = regex.firstMatch(in: token.raw, range: tokenLocalRange) else {
                continue
            }
            let localKeyRange = match.range(at: 1)
            let localValRange = match.range(at: 2)

            // 当已输入属性值时校验其语义合法性：非法的 type: / size: 词元在后端会退化为普通检索词，不渲染有效胶囊以免误导
            if localValRange.length > 0 {
                let nsRaw = token.raw as NSString
                let keyLower = nsRaw.substring(with: localKeyRange).lowercased()
                let valPayload = nsRaw.substring(with: localValRange)
                if keyLower == "type" {
                    let parts = valPayload
                        .split(separator: ",")
                        .map { $0.trimmingCharacters(in: .whitespaces) }
                        .filter { !$0.isEmpty }
                    guard !parts.isEmpty, parts.allSatisfy({ fileType(fromCategory: $0) != nil }) else {
                        continue
                    }
                } else if keyLower == "size" {
                    guard parseSizeToken(valPayload) != nil else {
                        continue
                    }
                } else if keyLower == "mtime" || keyLower == "modified" {
                    guard isValidModifiedTimePayload(valPayload) else {
                        continue
                    }
                }
            }

            let keyRange = NSRange(location: token.nsRange.location + localKeyRange.location, length: localKeyRange.length)
            let colonRange = NSRange(location: keyRange.location + keyRange.length, length: 1)
            let valRange = NSRange(location: token.nsRange.location + localValRange.location, length: localValRange.length)

            let prefixRange = NSRange(location: keyRange.location, length: keyRange.length + colonRange.length)

            // 键名与冒号（如 type:）：次要前景色
            result.addAttribute(.foregroundColor, value: NSColor.secondaryLabelColor, range: prefixRange)
            result.addAttribute(.font, value: baseFont, range: prefixRange)

            // 属性值（如 code, >10m, pdf）：强调前景色 + 半透明圆角背景胶囊
            if valRange.length > 0 {
                // 在冒号后增加 2pt 字距，使右侧圆角胶囊左缘不紧贴冒号
                result.addAttribute(.kern, value: 2.0, range: colonRange)

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
            // 输入法正在进行多字节（如中文拼音）组字，暂存待应用的过滤状态，避免破坏未提交的拼音流
            pendingState = state
            return
        }
        pendingState = nil

        let currentQuery = searchField.stringValue
        let (keyword, _) = Self.deserialize(query: currentQuery)
        let serialized = Self.serialize(keyword: keyword, state: state)
        let newQuery = (!serialized.isEmpty && !state.isUnconstrained) ? "\(serialized) " : serialized

        let baseFont = searchField.font ?? .systemFont(ofSize: 14)
        let formatted = Self.formatQuerySyntax(newQuery, baseFont: baseFont)
        searchField.attributedStringValue = formatted

        // 若当前处于编辑状态，同步更新字段编辑器中的文字属性并将光标推进至文本末尾（便于直接继续键入检索词）
        if let fieldEditor = fieldEditor, let storage = fieldEditor.textStorage {
            PillLayoutManager.ensureInstalled(on: fieldEditor)
            storage.beginEditing()
            storage.setAttributedString(formatted)
            storage.endEditing()
            let textLength = (newQuery as NSString).length
            let selection = NSRange(location: textLength, length: 0)
            fieldEditor.selectedRange = selection
            fieldEditor.scrollRangeToVisible(selection)
            // 重置输入态默认属性，防止后续中文/多字节输入法组字继承胶囊蓝色背景与字距
            fieldEditor.typingAttributes = [
                .font: baseFont,
                .foregroundColor: NSColor.labelColor,
            ]
        }

        performSearch(newQuery)
    }

    /// 检查并应用之前因输入法组字而暂存的未提交过滤状态
    @discardableResult
    public func flushPendingStateIfNeeded(
        searchField: NSSearchField,
        performSearch: (String) -> Void
    ) -> Bool {
        guard let state = pendingState else { return false }
        let fieldEditor = searchField.currentEditor() as? NSTextView
        if fieldEditor?.hasMarkedText() == true {
            return false
        }
        pendingState = nil
        updateQueryFromUI(state: state, searchField: searchField, performSearch: performSearch)
        return true
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
        let clipView = textView.superview as? NSClipView
        let savedOrigin = clipView?.bounds.origin
        let savedRange = textView.selectedRange
        container.replaceLayoutManager(PillLayoutManager())
        if let savedOrigin = savedOrigin {
            clipView?.scroll(to: savedOrigin)
        }
        textView.selectedRange = savedRange
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
