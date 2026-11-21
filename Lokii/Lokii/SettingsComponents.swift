import SwiftUI

// MARK: - Sidebar Models & Icons
struct SidebarItem: Identifiable, Hashable {
    let id: String
    let title: String
    let icon: String
    var iconColor: Color = .accentColor
    var hasBadge: Bool = false
}

struct SidebarItemIcon: View {
    let item: SidebarItem

    var body: some View {
        Image(systemName: item.icon)
            .font(.system(size: 11, weight: .semibold))
            .foregroundStyle(.white)
            .frame(width: 20, height: 20)
            .background(item.iconColor)
            .clipShape(RoundedRectangle(cornerRadius: 5, style: .continuous))
    }
}

struct UpdateBadge: View {
    var body: some View {
        Circle()
            .fill(Color.red)
            .frame(width: 7, height: 7)
            .accessibilityHidden(true)
    }
}

// MARK: - Native Sidebar List
struct SidebarList: View {
    let items: [SidebarItem]
    @Binding var selection: String?

    var body: some View {
        List(selection: $selection) {
            ForEach(items) { item in
                Label {
                    HStack(spacing: 4) {
                        Text(item.title)
                            .lineLimit(1)
                        if item.hasBadge {
                            UpdateBadge()
                        }
                    }
                } icon: {
                    SidebarItemIcon(item: item)
                }
                .tag(item.id)
            }
        }
        .listStyle(.sidebar)
        .navigationSplitViewColumnWidth(min: 140, ideal: 156, max: 176)
    }
}

// MARK: - Native Grouped Form Helpers

/// 原生分组表单 Section 标题视图，支持左侧标题、可选副标题说明与右侧操作控件
struct SettingsSectionHeader<Trailing: View>: View {
    let title: String
    let description: String?
    let trailing: Trailing

    init(
        _ title: String,
        description: String? = nil,
        @ViewBuilder trailing: () -> Trailing
    ) {
        self.title = title
        self.description = description
        self.trailing = trailing()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(alignment: .firstTextBaseline) {
                Text(title)
                Spacer()
                trailing
            }
            if let description, !description.isEmpty {
                // Section header 在分组表单中默认继承粗体，副标题显式恢复常规字重与次级前景色
                Text(description)
                    .font(.system(size: 11))
                    .fontWeight(.regular)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }
}

extension SettingsSectionHeader where Trailing == EmptyView {
    init(_ title: String, description: String? = nil) {
        self.init(title, description: description, trailing: { EmptyView() })
    }
}

/// 原生分组表单行标签，承载主标题与可选多行说明文案
struct SettingsRowLabel: View {
    let title: String
    let description: String?

    init(_ title: String, description: String? = nil) {
        self.title = title
        self.description = description
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title)
            if let description, !description.isEmpty {
                Text(description)
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }
}

/// 原生分组表单开关行
///
/// 通过 LabeledContent 将 .controlSize(.small) 精确限定在右侧 Toggle 控件本身，
/// 避免连带缩小左侧标题与说明文字字号，同时确保在 NSHostingController 托管下与系统设置开关尺码一致。
struct SettingsToggleRow: View {
    let title: String
    let description: String?
    @Binding var isOn: Bool

    init(_ title: String, description: String? = nil, isOn: Binding<Bool>) {
        self.title = title
        self.description = description
        self._isOn = isOn
    }

    var body: some View {
        // 使用 HStack(.center) 避免 LabeledContent 在左侧多行说明下按首行基线对齐导致开关偏上
        HStack(alignment: .center, spacing: 14) {
            SettingsRowLabel(title, description: description)
            Spacer(minLength: 12)
            Toggle("", isOn: $isOn)
                .toggleStyle(.switch)
                .labelsHidden()
                .controlSize(.small)
                .accessibilityLabel(title)
        }
    }
}

/// 紧凑克制的系统风格分段控件
///
/// 相比 macOS 27 默认大药丸样式，高度锁定为 30pt、圆角适度、使用系统强调色高亮选中项，
/// 并依赖系统语义背景与前景色自动适配浅色与深色模式。
struct SettingsSegmentedControl<SelectionValue: Hashable>: View {
    struct Option: Identifiable {
        let value: SelectionValue
        let title: String

        var id: SelectionValue { value }
    }

    let options: [Option]
    @Binding var selection: SelectionValue

    @Namespace private var highlightNamespace
    @State private var hoveredValue: SelectionValue?

    var body: some View {
        HStack(spacing: 2) {
            ForEach(options) { option in
                let isSelected = selection == option.value
                let isHovered = hoveredValue == option.value && !isSelected

                Button {
                    withAnimation(.easeInOut(duration: 0.16)) {
                        selection = option.value
                    }
                } label: {
                    Text(option.title)
                        .font(.system(size: 12, weight: isSelected ? .medium : .regular))
                        .foregroundStyle(isSelected ? Color.white : Color.primary)
                        .lineLimit(1)
                        .padding(.horizontal, 11)
                        .frame(height: 25)
                        .background {
                            if isSelected {
                                RoundedRectangle(cornerRadius: 5.5, style: .continuous)
                                    .fill(Color.accentColor)
                                    .matchedGeometryEffect(id: "activeSegment", in: highlightNamespace)
                            } else if isHovered {
                                RoundedRectangle(cornerRadius: 5.5, style: .continuous)
                                    .fill(Color.primary.opacity(0.05))
                            }
                        }
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .onHover { hovering in
                    hoveredValue = hovering ? option.value : (hoveredValue == option.value ? nil : hoveredValue)
                }
                .accessibilityAddTraits(isSelected ? .isSelected : [])
            }
        }
        .padding(2.5)
        .frame(height: 30)
        .background(Color.primary.opacity(0.06))
        .clipShape(RoundedRectangle(cornerRadius: 7.5, style: .continuous))
    }
}

/// 一体化紧凑数字步进控件（数字输入与上下 Chevron 步进整合于同一容器）
///
/// 仅在回车或失焦时解析并钳制至合法区间，避免用户输入中间态反复触发配置保存或出现千分位格式干扰。
/// 默认使用系统控件背景与分割线描边，仅在获得焦点时呈现 macOS 焦点环。
struct SettingsNumberStepper: View {
    @Binding var value: Int
    let range: ClosedRange<Int>
    let step: Int
    var width: CGFloat = 100
    var unit: String? = nil

    @State private var text: String = ""
    @State private var isEditing = false
    @State private var hoveredStep: StepDirection?
    @FocusState private var isFocused: Bool

    private enum StepDirection {
        case up, down
    }

    private var canIncrement: Bool { value < range.upperBound }
    private var canDecrement: Bool { value > range.lowerBound }

    var body: some View {
        HStack(spacing: 6) {
            HStack(spacing: 4) {
                // 非编辑态使用纯文本渲染，避免 macOS 窗口打开或切页时自动将首焦点落入输入框
                if isEditing {
                    TextField("", text: $text)
                        .textFieldStyle(.plain)
                        .labelsHidden()
                        .multilineTextAlignment(.leading)
                        .font(.system(size: 13, weight: .medium).monospacedDigit())
                        .foregroundStyle(.primary)
                        .focused($isFocused)
                        .onSubmit {
                            finishEditing(commit: true)
                        }
                        .onExitCommand {
                            finishEditing(commit: false)
                        }
                        .onChange(of: isFocused) { focused in
                            if !focused && isEditing {
                                finishEditing(commit: true)
                            }
                        }
                } else {
                    Text("\(value)")
                        .font(.system(size: 13, weight: .medium).monospacedDigit())
                        .foregroundStyle(.primary)
                        .lineLimit(1)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }

                VStack(spacing: 1) {
                    stepButton(direction: .up, symbol: "chevron.up", enabled: canIncrement) {
                        stepBy(step)
                    }
                    stepButton(direction: .down, symbol: "chevron.down", enabled: canDecrement) {
                        stepBy(-step)
                    }
                }
                .fixedSize()
            }
            .padding(.leading, 10)
            .padding(.trailing, 7)
            .frame(width: width, height: 32)
            .background(Color(nsColor: .controlBackgroundColor))
            .clipShape(RoundedRectangle(cornerRadius: 7, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .strokeBorder(
                        isEditing ? Color.accentColor.opacity(0.85) : Color(nsColor: .separatorColor),
                        lineWidth: 1
                    )
            )
            .overlay(
                RoundedRectangle(cornerRadius: 8.5, style: .continuous)
                    .stroke(Color.accentColor.opacity(isEditing ? 0.30 : 0), lineWidth: 2.5)
                    .padding(-1.5)
            )
            .contentShape(RoundedRectangle(cornerRadius: 7, style: .continuous))
            .onTapGesture {
                beginEditing()
            }

            if let unit, !unit.isEmpty {
                Text(unit)
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .fixedSize()
            }
        }
        .onAppear {
            text = "\(value)"
        }
        .onChange(of: value) { newValue in
            let formatted = "\(newValue)"
            if !isEditing && text != formatted {
                text = formatted
            }
        }
    }

    @ViewBuilder
    private func stepButton(
        direction: StepDirection,
        symbol: String,
        enabled: Bool,
        action: @escaping () -> Void
    ) -> some View {
        let isHovered = hoveredStep == direction && enabled
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 8.5, weight: .semibold))
                .foregroundStyle(
                    !enabled
                        ? Color.secondary.opacity(0.30)
                        : (isHovered ? Color.primary : Color.secondary)
                )
                .frame(width: 18, height: 12)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .onHover { hovering in
            hoveredStep = hovering ? direction : (hoveredStep == direction ? nil : hoveredStep)
        }
    }

    private func beginEditing() {
        guard !isEditing else { return }
        text = "\(value)"
        isEditing = true
        DispatchQueue.main.async {
            isFocused = true
        }
    }

    private func finishEditing(commit: Bool) {
        if commit {
            commitText()
        } else {
            text = "\(value)"
        }
        isFocused = false
        isEditing = false
    }

    private func stepBy(_ delta: Int) {
        if isEditing {
            finishEditing(commit: true)
        }
        let next = min(max(value + delta, range.lowerBound), range.upperBound)
        if value != next {
            value = next
        }
        text = "\(next)"
    }

    private func commitText() {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if let parsed = Int(trimmed) {
            let clamped = min(max(parsed, range.lowerBound), range.upperBound)
            if value != clamped {
                value = clamped
            }
            text = "\(clamped)"
        } else {
            text = "\(value)"
        }
    }
}

// MARK: - FlowLayout (Wrapping HStack)
struct FlowLayout: Layout {
    var spacing: CGFloat = 6

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let maxWidth = proposal.width ?? .infinity
        var currentX: CGFloat = 0
        var currentY: CGFloat = 0
        var lineHeight: CGFloat = 0
        var usedWidth: CGFloat = 0

        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if currentX + size.width > maxWidth && currentX > 0 {
                usedWidth = max(usedWidth, currentX - spacing)
                currentX = 0
                currentY += lineHeight + spacing
                lineHeight = 0
            }
            lineHeight = max(lineHeight, size.height)
            currentX += size.width + spacing
        }
        if currentX > 0 {
            usedWidth = max(usedWidth, currentX - spacing)
        }

        // 当父容器未提供有限宽度建议时返回实际内容宽度，避免在分组表单测量阶段返回 .infinity
        let resolvedWidth = proposal.width.flatMap { $0.isFinite ? $0 : nil } ?? usedWidth
        return CGSize(width: resolvedWidth, height: currentY + lineHeight)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var currentX = bounds.minX
        var currentY = bounds.minY
        var lineHeight: CGFloat = 0

        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if currentX + size.width > bounds.maxX && currentX > bounds.minX {
                currentX = bounds.minX
                currentY += lineHeight + spacing
                lineHeight = 0
            }
            subview.place(at: CGPoint(x: currentX, y: currentY), proposal: ProposedViewSize(size))
            lineHeight = max(lineHeight, size.height)
            currentX += size.width + spacing
        }
    }
}

// MARK: - Tag Chip View
struct PillTagView: View {
    let title: String
    let onRemove: () -> Void

    @State private var isHovered = false

    var body: some View {
        HStack(spacing: 4) {
            Text(title)
                .font(.system(size: 11, design: .monospaced))
                .lineLimit(1)
            
            Button(action: onRemove) {
                Image(systemName: "xmark")
                    .font(.system(size: 9, weight: .bold))
                    .foregroundStyle(isHovered ? Color.red : Color.secondary)
            }
            .buttonStyle(.plain)
            .help(L("prefs.action.remove"))
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(Color.primary.opacity(isHovered ? 0.08 : 0.04))
        .clipShape(Capsule())
        .overlay(
            Capsule()
                .strokeBorder(isHovered ? Color.primary.opacity(0.18) : Color.primary.opacity(0.08), lineWidth: 1)
        )
        .onHover { hovering in
            isHovered = hovering
        }
    }
}

// MARK: - Add Tag Chip Button
struct AddPillButton: View {
    let title: String
    let action: () -> Void

    @State private var isHovered = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 3) {
                Image(systemName: "plus")
                    .font(.system(size: 9, weight: .semibold))
                Text(title)
                    .font(.system(size: 11, weight: .medium))
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(isHovered ? Color.accentColor.opacity(0.12) : Color.accentColor.opacity(0.06))
            .foregroundStyle(Color.accentColor)
            .clipShape(Capsule())
            .overlay(
                Capsule()
                    .strokeBorder(Color.accentColor.opacity(0.35), style: StrokeStyle(lineWidth: 1, dash: [3, 3]))
            )
        }
        .buttonStyle(.plain)
        .onHover { hovering in
            isHovered = hovering
        }
    }
}
