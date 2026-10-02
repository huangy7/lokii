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
struct SettingsGroup<Content: View>: View {
    let title: String?
    @ViewBuilder var content: () -> Content

    init(_ title: String? = nil, @ViewBuilder content: @escaping () -> Content) {
        self.title = title
        self.content = content
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let title {
                Text(title)
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
                    .padding(.leading, 12)
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
