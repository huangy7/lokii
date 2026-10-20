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

// MARK: - macOS System Settings Style Group
struct SettingsGroup<Content: View, HeaderTrailing: View>: View {
    let title: String?
    let description: String?
    let headerTrailing: HeaderTrailing
    @ViewBuilder var content: () -> Content

    init(
        _ title: String? = nil,
        description: String? = nil,
        @ViewBuilder trailing: () -> HeaderTrailing,
        @ViewBuilder content: @escaping () -> Content
    ) {
        self.title = title
        self.description = description
        self.headerTrailing = trailing()
        self.content = content
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if title != nil || description != nil {
                VStack(alignment: .leading, spacing: 2) {
                    HStack(alignment: .firstTextBaseline) {
                        if let title {
                            Text(title)
                                .font(.system(size: 11, weight: .medium))
                                .foregroundStyle(.secondary)
                        }
                        Spacer()
                        headerTrailing
                    }
                    if let description {
                        Text(description)
                            .font(.system(size: 11))
                            .foregroundStyle(.secondary.opacity(0.85))
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                .padding(.horizontal, 12)
            }
            VStack(spacing: 0) {
                content()
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color(nsColor: .controlBackgroundColor))
            .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .strokeBorder(Color.primary.opacity(0.06), lineWidth: 1)
            )
        }
    }
}

extension SettingsGroup where HeaderTrailing == EmptyView {
    init(
        _ title: String? = nil,
        description: String? = nil,
        @ViewBuilder content: @escaping () -> Content
    ) {
        self.init(title, description: description, trailing: { EmptyView() }, content: content)
    }
}

// MARK: - macOS System Settings Style Row
struct SettingsRow<Trailing: View>: View {
    let title: String
    let description: String?
    @ViewBuilder var trailing: () -> Trailing

    init(_ title: String, description: String? = nil, @ViewBuilder trailing: @escaping () -> Trailing) {
        self.title = title
        self.description = description
        self.trailing = trailing
    }

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.system(size: 13, weight: .semibold))
                if let description {
                    Text(description)
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 8)
            trailing()
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 9)
    }
}

extension SettingsRow where Trailing == EmptyView {
    init(_ title: String, description: String? = nil) {
        self.init(title, description: description, trailing: { EmptyView() })
    }
}

struct SettingsRowDivider: View {
    var body: some View {
        Divider()
            .padding(.leading, 12)
    }
}

// MARK: - FlowLayout (Wrapping HStack)
struct FlowLayout: Layout {
    var spacing: CGFloat = 6

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let width = proposal.width ?? .infinity
        var currentX: CGFloat = 0
        var currentY: CGFloat = 0
        var lineHeight: CGFloat = 0

        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if currentX + size.width > width && currentX > 0 {
                currentX = 0
                currentY += lineHeight + spacing
                lineHeight = 0
            }
            lineHeight = max(lineHeight, size.height)
            currentX += size.width + spacing
        }

        return CGSize(width: width, height: currentY + lineHeight)
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
            .help("移除规则")
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
