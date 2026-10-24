import SwiftUI
import AppKit

// MARK: - 文件大小单位定义

/// 文件大小换算单位，遵循 macOS 访达十进制标准（1KB = 1000 Bytes, 1MB = 1,000,000 Bytes, 1GB = 1,000,000,000 Bytes）
public enum SizeUnit: String, CaseIterable, Identifiable, Sendable {
    case mb = "MB"
    case kb = "KB"
    case gb = "GB"
    case bytes = "Bytes"

    public var id: String { rawValue }

    public var multiplier: UInt64 {
        switch self {
        case .bytes: return 1
        case .kb: return 1_000
        case .mb: return 1_000_000
        case .gb: return 1_000_000_000
        }
    }
}

// MARK: - 文件类型展示项

public struct FileTypeItem: Identifiable, Sendable {
    public let type: FfiFileType
    public let symbolName: String
    public let key: String
    public let defaultName: String

    public var id: FfiFileType { type }
}

// MARK: - 本地编辑状态协同容器

final class FilterPopoverLocalState {
    var isMinEnabled: Bool? = nil
    var minText: String? = nil
    var minUnit: SizeUnit? = nil

    var isMaxEnabled: Bool? = nil
    var maxSizeText: String? = nil
    var maxSizeUnit: SizeUnit? = nil

    var isUserEditingMin: Bool = false
    var isUserEditingMax: Bool = false
    var minDebounceTask: Task<Void, Never>? = nil
    var maxDebounceTask: Task<Void, Never>? = nil
    var isInternalUpdate: Bool = false
    var lastSyncedMinBytes: UInt64? = nil
    var lastSyncedMaxBytes: UInt64? = nil
}

// MARK: - 声明式过滤气泡浮层视图

/// 高信息密度、键盘优先的原生搜索过滤气泡浮层视图。
/// 核心特性：
/// 1. 与 `FilterState` 及主搜索栏保持双向实时绑定；
/// 2. 支持多文件类型并集复选与“全部”总控联动；
/// 3. 支持大小阈值与区间约束的安全数值输入与单位换算；
/// 4. 隔离本地编辑态与防抖提交，防止击键中间态意外失焦；
/// 5. 补齐无障碍 VoiceOver 辅助功能标签与区间倒置弱提示。
public struct FilterPopoverView: View {
    @Environment(\.colorScheme) private var colorScheme
    @Binding public var state: FilterState
    public var onStateChanged: ((FilterState) -> Void)?
    public var onDismiss: (() -> Void)?

    private let localBox = FilterPopoverLocalState()

    private var isDark: Bool {
        colorScheme == .dark
    }

    /// 气泡面板自适应高不透明度底色，阻断跨窗口边缘时的半透明透底割裂感
    private var surfaceBackground: Color {
        isDark
            ? Color(red: 0.14, green: 0.145, blue: 0.165).opacity(0.96)
            : Color(red: 0.965, green: 0.968, blue: 0.978).opacity(0.96)
    }

    /// 内部卡片层级底色：浅色为明亮白，深色为微亮板岩色（避免深色下死黑块对比失衡）
    private var cardBackground: Color {
        isDark
            ? Color.white.opacity(0.055)
            : Color.white
    }

    /// 内部卡片微细描边色
    private var cardBorder: Color {
        isDark
            ? Color.white.opacity(0.09)
            : Color.black.opacity(0.07)
    }

    // 内部本地大小输入状态（结合状态箱保障独立宿主与视图树生命周期双向一致）
    @State private var isMinEnabledState: Bool = false
    public var isMinEnabled: Bool {
        get { localBox.isMinEnabled ?? isMinEnabledState }
        nonmutating set {
            localBox.isMinEnabled = newValue
            isMinEnabledState = newValue
        }
    }

    @State private var minTextState: String = "10"
    public var minText: String {
        get { localBox.minText ?? minTextState }
        nonmutating set {
            localBox.minText = newValue
            minTextState = newValue
        }
    }

    @State private var minUnitState: SizeUnit = .mb
    public var minUnit: SizeUnit {
        get { localBox.minUnit ?? minUnitState }
        nonmutating set {
            localBox.minUnit = newValue
            minUnitState = newValue
        }
    }

    @State private var isMaxEnabledState: Bool = false
    public var isMaxEnabled: Bool {
        get { localBox.isMaxEnabled ?? isMaxEnabledState }
        nonmutating set {
            localBox.isMaxEnabled = newValue
            isMaxEnabledState = newValue
        }
    }

    @State private var maxSizeTextState: String = "500"
    public var maxSizeText: String {
        get { localBox.maxSizeText ?? maxSizeTextState }
        nonmutating set {
            localBox.maxSizeText = newValue
            maxSizeTextState = newValue
        }
    }

    @State private var maxSizeUnitState: SizeUnit = .kb
    public var maxSizeUnit: SizeUnit {
        get { localBox.maxSizeUnit ?? maxSizeUnitState }
        nonmutating set {
            localBox.maxSizeUnit = newValue
            maxSizeUnitState = newValue
        }
    }

    public static let fileTypeItems: [FileTypeItem] = [
        FileTypeItem(type: .code, symbolName: "chevron.left.forwardslash.chevron.right", key: "filter.type.code", defaultName: "代码"),
        FileTypeItem(type: .document, symbolName: "doc.text", key: "filter.type.document", defaultName: "文档"),
        FileTypeItem(type: .image, symbolName: "photo", key: "filter.type.image", defaultName: "图片"),
        FileTypeItem(type: .media, symbolName: "play.rectangle", key: "filter.type.media", defaultName: "多媒体"),
        FileTypeItem(type: .archive, symbolName: "archivebox", key: "filter.type.archive", defaultName: "压缩包"),
        FileTypeItem(type: .folder, symbolName: "folder", key: "filter.type.folder", defaultName: "文件夹")
    ]

    public init(
        state: Binding<FilterState>,
        onStateChanged: ((FilterState) -> Void)? = nil,
        onDismiss: (() -> Void)? = nil
    ) {
        self._state = state
        self.onStateChanged = onStateChanged
        self.onDismiss = onDismiss

        if let minBytes = state.wrappedValue.minSizeBytes {
            let (t, u) = Self.decomposeBytes(minBytes)
            _isMinEnabledState = State(initialValue: true)
            _minTextState = State(initialValue: t)
            _minUnitState = State(initialValue: u)
            localBox.isMinEnabled = true
            localBox.minText = t
            localBox.minUnit = u
            localBox.lastSyncedMinBytes = minBytes
        }
        if let maxBytes = state.wrappedValue.maxSizeBytes {
            let (t, u) = Self.decomposeBytes(maxBytes)
            _isMaxEnabledState = State(initialValue: true)
            _maxSizeTextState = State(initialValue: t)
            _maxSizeUnitState = State(initialValue: u)
            localBox.isMaxEnabled = true
            localBox.maxSizeText = t
            localBox.maxSizeUnit = u
            localBox.lastSyncedMaxBytes = maxBytes
        }
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            topBarSection
            Divider().opacity(isDark ? 0.4 : 0.6)
            fileTypesSection
            Divider().opacity(isDark ? 0.4 : 0.6)
            fileSizeSection
            bottomBarSection
        }
        .padding(16)
        .frame(width: 320)
        .background(surfaceBackground)
        .onAppear {
            syncLocalStateFromExternal()
        }
        .onStateChange(of: state) {
            if localBox.isInternalUpdate {
                localBox.isInternalUpdate = false
                return
            }
            syncLocalStateFromExternal()
        }
        .onDisappear {
            flushDebouncedChanges()
        }
    }

    // MARK: - 顶栏：标题与清除全部

    private var topBarSection: some View {
        HStack(alignment: .center) {
            Text(L("filter.title"))
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(Color.primary)

            Spacer()

            Button(L("filter.clearAll")) {
                clearAll()
            }
            .buttonStyle(.plain)
            .font(.system(size: 11.5, weight: .medium))
            .foregroundStyle(state.isUnconstrained ? Color.secondary.opacity(0.4) : Color.accentColor)
            .disabled(state.isUnconstrained)
        }
    }

    // MARK: - 文件类型分类网格

    private var fileTypesSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .center) {
                Text(L("filter.type.title"))
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(.secondary)

                Spacer()

                Button(action: {
                    toggleAllTypes()
                }) {
                    HStack(spacing: 4) {
                        if isAllSelected {
                            Image(systemName: "checkmark")
                                .font(.system(size: 9, weight: .bold))
                        }
                        Text(L("filter.all"))
                            .font(.system(size: 11, weight: .medium))
                    }
                    .padding(.horizontal, 8)
                    .padding(.vertical, 3)
                    .foregroundStyle(isAllSelected ? Color.accentColor : Color.secondary)
                    .background(
                        Capsule()
                            .fill(isAllSelected
                                  ? Color.accentColor.opacity(isDark ? 0.22 : 0.12)
                                  : (isDark ? Color.white.opacity(0.06) : Color.black.opacity(0.04)))
                    )
                }
                .buttonStyle(.plain)
            }

            LazyVGrid(columns: [GridItem(.flexible(), spacing: 8), GridItem(.flexible(), spacing: 8)], spacing: 8) {
                ForEach(Self.fileTypeItems) { item in
                    let isSelected = state.selectedTypes.contains(item.type)
                    Button(action: {
                        toggleFileType(item.type)
                    }) {
                        HStack(spacing: 8) {
                            Image(systemName: item.symbolName)
                                .font(.system(size: 12, weight: .medium))
                                .foregroundStyle(isSelected ? Color.accentColor : Color.secondary)
                                .frame(width: 16, alignment: .center)

                            Text(L(item.key))
                                .font(.system(size: 12, weight: isSelected ? .medium : .regular))
                                .foregroundStyle(isSelected ? (isDark ? Color.white : Color.accentColor) : Color.primary.opacity(0.9))
                                .lineLimit(1)

                            Spacer(minLength: 4)

                            checkboxIndicator(isChecked: isSelected)
                        }
                        .padding(.horizontal, 10)
                        .padding(.vertical, 7.5)
                        .background(
                            RoundedRectangle(cornerRadius: 8, style: .continuous)
                                .fill(isSelected
                                      ? Color.accentColor.opacity(isDark ? 0.20 : 0.10)
                                      : cardBackground)
                        )
                        .overlay(
                            RoundedRectangle(cornerRadius: 8, style: .continuous)
                                .strokeBorder(
                                    isSelected
                                        ? Color.accentColor.opacity(isDark ? 0.55 : 0.40)
                                        : cardBorder,
                                    lineWidth: 1
                                )
                        )
                        .contentShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                    }
                    .buttonStyle(.plain)
                }
            }
        }
    }

    // MARK: - 文件大小阈值区间

    private var fileSizeSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L("filter.size.title"))
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(.secondary)

            VStack(spacing: 8) {
                // 大于下限
                HStack(spacing: 8) {
                    Button(action: {
                        handleMinToggle(!isMinEnabled)
                    }) {
                        HStack(spacing: 7) {
                            checkboxIndicator(isChecked: isMinEnabled)
                            Text(L("filter.size.over"))
                                .font(.system(size: 12, weight: isMinEnabled ? .medium : .regular))
                                .foregroundStyle(isMinEnabled ? Color.primary : Color.secondary)
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)

                    Spacer()

                    sizeValueInput(
                        text: Binding(
                            get: { minText },
                            set: { handleMinTextChange($0) }
                        ),
                        isEnabled: isMinEnabled,
                        accessibilityKey: "filter.size.over"
                    )

                    sizeUnitSelector(
                        unit: minUnit,
                        isEnabled: isMinEnabled,
                        accessibilityKey: "filter.size.over",
                        onSelect: { handleMinUnitChange($0) }
                    )
                }

                // 小于上限
                HStack(spacing: 8) {
                    Button(action: {
                        handleMaxToggle(!isMaxEnabled)
                    }) {
                        HStack(spacing: 7) {
                            checkboxIndicator(isChecked: isMaxEnabled)
                            Text(L("filter.size.under"))
                                .font(.system(size: 12, weight: isMaxEnabled ? .medium : .regular))
                                .foregroundStyle(isMaxEnabled ? Color.primary : Color.secondary)
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)

                    Spacer()

                    sizeValueInput(
                        text: Binding(
                            get: { maxSizeText },
                            set: { handleMaxTextChange($0) }
                        ),
                        isEnabled: isMaxEnabled,
                        accessibilityKey: "filter.size.under"
                    )

                    sizeUnitSelector(
                        unit: maxSizeUnit,
                        isEnabled: isMaxEnabled,
                        accessibilityKey: "filter.size.under",
                        onSelect: { handleMaxUnitChange($0) }
                    )
                }
            }
            .padding(10)
            .background(
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .fill(cardBackground)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .strokeBorder(cardBorder, lineWidth: 1)
            )

            // 区间倒置视觉弱提示
            if isSizeRangeInverted {
                HStack(spacing: 4) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .font(.system(size: 11))
                        .foregroundStyle(Color.orange)
                    Text(L("filter.size.invalidRange"))
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                    Spacer()
                }
                .padding(.top, 2)
            }
        }
    }

    // MARK: - 底栏：完成关闭

    private var bottomBarSection: some View {
        HStack {
            Spacer()
            Button(L("filter.done")) {
                flushDebouncedChanges()
                onDismiss?()
            }
            .keyboardShortcut(.defaultAction)
            .buttonStyle(.borderedProminent)
            .controlSize(.small)
        }
        .padding(.top, 2)
    }

    // MARK: - 子组件构建辅助

    @ViewBuilder
    private func checkboxIndicator(isChecked: Bool) -> some View {
        ZStack {
            RoundedRectangle(cornerRadius: 4, style: .continuous)
                .fill(isChecked ? Color.accentColor : (isDark ? Color.white.opacity(0.04) : Color.clear))
            RoundedRectangle(cornerRadius: 4, style: .continuous)
                .strokeBorder(
                    isChecked
                        ? Color.accentColor
                        : (isDark ? Color.white.opacity(0.20) : Color.black.opacity(0.18)),
                    lineWidth: 1.2
                )
            if isChecked {
                Image(systemName: "checkmark")
                    .font(.system(size: 8.5, weight: .bold))
                    .foregroundStyle(Color.white)
            }
        }
        .frame(width: 14, height: 14)
    }

    @ViewBuilder
    private func sizeValueInput(text: Binding<String>, isEnabled: Bool, accessibilityKey: String) -> some View {
        TextField("", text: text)
            .textFieldStyle(.plain)
            .font(.system(size: 11.5, weight: .medium, design: .monospaced))
            .foregroundStyle(isEnabled ? Color.primary : Color.secondary.opacity(0.5))
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .frame(width: 68)
            .background(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(isDark
                          ? Color.white.opacity(isEnabled ? 0.08 : 0.03)
                          : (isEnabled ? Color(red: 0.96, green: 0.965, blue: 0.975) : Color.black.opacity(0.025)))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .strokeBorder(
                        isDark
                            ? Color.white.opacity(isEnabled ? 0.16 : 0.06)
                            : Color.black.opacity(isEnabled ? 0.12 : 0.05),
                        lineWidth: 1
                    )
            )
            .disabled(!isEnabled)
            .accessibilityLabel(Text(accessibilityKey))
            .onSubmit {
                flushDebouncedChanges()
            }
    }

    @ViewBuilder
    private func sizeUnitSelector(
        unit: SizeUnit,
        isEnabled: Bool,
        accessibilityKey: String,
        onSelect: @escaping (SizeUnit) -> Void
    ) -> some View {
        Menu {
            ForEach(SizeUnit.allCases) { candidate in
                Button(candidate.rawValue) {
                    onSelect(candidate)
                }
            }
        } label: {
            HStack(spacing: 4) {
                Text(unit.rawValue)
                    .font(.system(size: 11.5, weight: .medium))
                Spacer(minLength: 2)
                Image(systemName: "chevron.up.chevron.down")
                    .font(.system(size: 8.5, weight: .semibold))
                    .foregroundStyle(.secondary)
            }
            .foregroundStyle(isEnabled ? Color.primary : Color.secondary.opacity(0.5))
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .frame(width: 72)
            .background(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(isDark
                          ? Color.white.opacity(isEnabled ? 0.08 : 0.03)
                          : (isEnabled ? Color(red: 0.96, green: 0.965, blue: 0.975) : Color.black.opacity(0.025)))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .strokeBorder(
                        isDark
                            ? Color.white.opacity(isEnabled ? 0.16 : 0.06)
                            : Color.black.opacity(isEnabled ? 0.12 : 0.05),
                        lineWidth: 1
                    )
            )
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        .disabled(!isEnabled)
        .accessibilityLabel(Text(accessibilityKey))
    }

    // MARK: - 状态流转与逻辑辅助方法

    /// 是否处于全部类型均允许状态（空集合、包含 .all 或包含全部具体类别）
    public var isAllSelected: Bool {
        !state.hasTypeConstraint
    }

    /// 是否处于大小区间倒置状态（即下限大于上限）
    public var isSizeRangeInverted: Bool {
        if isMinEnabled && isMaxEnabled,
           let minB = Self.calculateBytes(text: minText, unit: minUnit),
           let maxB = Self.calculateBytes(text: maxSizeText, unit: maxSizeUnit) {
            return minB > maxB
        }
        if let minB = state.minSizeBytes, let maxB = state.maxSizeBytes {
            return minB > maxB
        }
        return false
    }

    /// 处理下限大小文本输入变化，启动 250ms 防抖
    public func handleMinTextChange(_ newText: String) {
        let cleaned = Self.sanitizeNumberString(newText)
        minText = cleaned
        guard isMinEnabled else { return }
        localBox.isUserEditingMin = true
        localBox.minDebounceTask?.cancel()
        localBox.minDebounceTask = Task { @MainActor in
            do {
                try await Task.sleep(nanoseconds: 250_000_000)
            } catch {
                return
            }
            guard !Task.isCancelled else { return }
            flushMinSize()
        }
    }

    /// 立即同步下限大小至 FilterState
    public func flushMinSize() {
        localBox.minDebounceTask?.cancel()
        localBox.minDebounceTask = nil
        localBox.isUserEditingMin = false
        guard isMinEnabled else {
            if state.minSizeBytes != nil {
                localBox.isInternalUpdate = true
                state.minSizeBytes = nil
                onStateChanged?(state)
            }
            return
        }
        let bytes = Self.calculateBytes(text: minText, unit: minUnit)
        if state.minSizeBytes != bytes {
            localBox.isInternalUpdate = true
            state.minSizeBytes = bytes
            localBox.lastSyncedMinBytes = bytes
            onStateChanged?(state)
        }
    }

    /// 切换下限大小开关（即时响应，零延迟）
    public func handleMinToggle(_ enabled: Bool) {
        localBox.minDebounceTask?.cancel()
        localBox.minDebounceTask = nil
        localBox.isUserEditingMin = false
        isMinEnabled = enabled
        localBox.isInternalUpdate = true
        if enabled {
            if let bytes = Self.calculateBytes(text: minText, unit: minUnit) {
                state.minSizeBytes = bytes
                localBox.lastSyncedMinBytes = bytes
            } else {
                minText = "10"
                minUnit = .mb
                let defaultBytes = Self.calculateBytes(text: "10", unit: .mb)
                state.minSizeBytes = defaultBytes
                localBox.lastSyncedMinBytes = defaultBytes
            }
        } else {
            state.minSizeBytes = nil
            localBox.lastSyncedMinBytes = nil
        }
        onStateChanged?(state)
    }

    /// 切换下限单位（立即生效）
    public func handleMinUnitChange(_ newUnit: SizeUnit) {
        minUnit = newUnit
        if isMinEnabled {
            flushMinSize()
        }
    }

    /// 处理上限大小文本输入变化，启动 250ms 防抖
    public func handleMaxTextChange(_ newText: String) {
        let cleaned = Self.sanitizeNumberString(newText)
        maxSizeText = cleaned
        guard isMaxEnabled else { return }
        localBox.isUserEditingMax = true
        localBox.maxDebounceTask?.cancel()
        localBox.maxDebounceTask = Task { @MainActor in
            do {
                try await Task.sleep(nanoseconds: 250_000_000)
            } catch {
                return
            }
            guard !Task.isCancelled else { return }
            flushMaxSize()
        }
    }

    /// 立即同步上限大小至 FilterState
    public func flushMaxSize() {
        localBox.maxDebounceTask?.cancel()
        localBox.maxDebounceTask = nil
        localBox.isUserEditingMax = false
        guard isMaxEnabled else {
            if state.maxSizeBytes != nil {
                localBox.isInternalUpdate = true
                state.maxSizeBytes = nil
                onStateChanged?(state)
            }
            return
        }
        let bytes = Self.calculateBytes(text: maxSizeText, unit: maxSizeUnit)
        if state.maxSizeBytes != bytes {
            localBox.isInternalUpdate = true
            state.maxSizeBytes = bytes
            localBox.lastSyncedMaxBytes = bytes
            onStateChanged?(state)
        }
    }

    /// 切换上限大小开关（即时响应，零延迟）
    public func handleMaxToggle(_ enabled: Bool) {
        localBox.maxDebounceTask?.cancel()
        localBox.maxDebounceTask = nil
        localBox.isUserEditingMax = false
        isMaxEnabled = enabled
        localBox.isInternalUpdate = true
        if enabled {
            if let bytes = Self.calculateBytes(text: maxSizeText, unit: maxSizeUnit) {
                state.maxSizeBytes = bytes
                localBox.lastSyncedMaxBytes = bytes
            } else {
                maxSizeText = "500"
                maxSizeUnit = .kb
                let defaultBytes = Self.calculateBytes(text: "500", unit: .kb)
                state.maxSizeBytes = defaultBytes
                localBox.lastSyncedMaxBytes = defaultBytes
            }
        } else {
            state.maxSizeBytes = nil
            localBox.lastSyncedMaxBytes = nil
        }
        onStateChanged?(state)
    }

    /// 切换上限单位（立即生效）
    public func handleMaxUnitChange(_ newUnit: SizeUnit) {
        maxSizeUnit = newUnit
        if isMaxEnabled {
            flushMaxSize()
        }
    }

    /// 立即生效所有未提交的输入防抖变更
    public func flushDebouncedChanges() {
        flushMinSize()
        flushMaxSize()
    }

    /// 切换具体文件类型的选择状态
    public func toggleFileType(_ type: FfiFileType) {
        localBox.isInternalUpdate = true
        if state.selectedTypes.contains(type) {
            state.selectedTypes.remove(type)
        } else {
            state.selectedTypes.insert(type)
        }
        onStateChanged?(state)
    }

    /// 切换全部文件类型总控开关
    public func toggleAllTypes() {
        localBox.isInternalUpdate = true
        // 无论当前是完全放行还是局部选定，点击「全部」均重置为无类型约束
        state.selectedTypes.removeAll()
        onStateChanged?(state)
    }

    /// 清空所有激活的过滤条件
    public func clearAll() {
        localBox.minDebounceTask?.cancel()
        localBox.minDebounceTask = nil
        localBox.maxDebounceTask?.cancel()
        localBox.maxDebounceTask = nil
        localBox.isUserEditingMin = false
        localBox.isUserEditingMax = false
        localBox.isInternalUpdate = true
        state.clearAll()
        isMinEnabled = false
        isMaxEnabled = false
        localBox.lastSyncedMinBytes = nil
        localBox.lastSyncedMaxBytes = nil
        onStateChanged?(state)
    }

    /// 从外部 state 同步本地编辑状态
    private func syncLocalStateFromExternal() {
        if !localBox.isUserEditingMin {
            if let minBytes = state.minSizeBytes {
                isMinEnabled = true
                localBox.lastSyncedMinBytes = minBytes
                if Self.calculateBytes(text: minText, unit: minUnit) != minBytes {
                    let (t, u) = Self.decomposeBytes(minBytes)
                    minText = t
                    minUnit = u
                }
            } else if localBox.lastSyncedMinBytes != nil && !minText.isEmpty {
                isMinEnabled = false
                localBox.lastSyncedMinBytes = nil
            }
        }

        if !localBox.isUserEditingMax {
            if let maxBytes = state.maxSizeBytes {
                isMaxEnabled = true
                localBox.lastSyncedMaxBytes = maxBytes
                if Self.calculateBytes(text: maxSizeText, unit: maxSizeUnit) != maxBytes {
                    let (t, u) = Self.decomposeBytes(maxBytes)
                    maxSizeText = t
                    maxSizeUnit = u
                }
            } else if localBox.lastSyncedMaxBytes != nil && !maxSizeText.isEmpty {
                isMaxEnabled = false
                localBox.lastSyncedMaxBytes = nil
            }
        }
    }

    /// 安全校验数字文本，仅保留有效正数格式（仅允许 ASCII 0-9 数字与最多一个小数点）
    public static func sanitizeNumberString(_ raw: String) -> String {
        var seenDot = false
        var cleaned = ""
        for ch in raw {
            if ch.isASCII && ch.isNumber {
                cleaned.append(ch)
            } else if ch == "." && !seenDot {
                seenDot = true
                cleaned.append(ch)
            }
        }
        return cleaned
    }

    /// 根据用户输入文本与选定单位换算为精确字节数
    public static func calculateBytes(text: String, unit: SizeUnit) -> UInt64? {
        let trimmed = text.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return nil }

        if let intVal = UInt64(trimmed) {
            let (res, overflow) = intVal.multipliedReportingOverflow(by: unit.multiplier)
            return overflow ? UInt64.max : res
        }

        if let doubleVal = Double(trimmed), !doubleVal.isNaN, !doubleVal.isInfinite, doubleVal >= 0 {
            let res = doubleVal * Double(unit.multiplier)
            if res >= Double(UInt64.max) {
                return UInt64.max
            }
            return UInt64(res.rounded())
        }

        return nil
    }

    /// 将字节数逆向解构为最适宜呈现的数字文本与单位
    public static func decomposeBytes(_ bytes: UInt64) -> (text: String, unit: SizeUnit) {
        let posixLocale = Locale(identifier: "en_US_POSIX")
        if bytes >= 1_000_000_000 && bytes % 1_000_000_000 == 0 {
            return ("\(bytes / 1_000_000_000)", .gb)
        } else if bytes >= 1_000_000_000 && bytes % 100_000_000 == 0 {
            return (String(format: "%.1f", locale: posixLocale, Double(bytes) / 1_000_000_000), .gb)
        } else if bytes >= 1_000_000 && bytes % 1_000_000 == 0 {
            return ("\(bytes / 1_000_000)", .mb)
        } else if bytes >= 1_000_000 && bytes % 100_000 == 0 {
            return (String(format: "%.1f", locale: posixLocale, Double(bytes) / 1_000_000), .mb)
        } else if bytes >= 1_000 && bytes % 1_000 == 0 {
            return ("\(bytes / 1_000)", .kb)
        } else if bytes >= 1_000 && bytes % 100 == 0 {
            return (String(format: "%.1f", locale: posixLocale, Double(bytes) / 1_000), .kb)
        } else {
            return ("\(bytes)", .bytes)
        }
    }
}

// MARK: - 跨版本兼容扩展

private extension View {
    @ViewBuilder
    func onStateChange<V: Equatable>(of value: V, action: @escaping () -> Void) -> some View {
        if #available(macOS 14.0, *) {
            self.onChange(of: value) {
                action()
            }
        } else {
            self.onChange(of: value) { _ in
                action()
            }
        }
    }
}
