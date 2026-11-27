import SwiftUI
import AppKit

// MARK: - View Extension for macOS 14+ Toolbar Compatibility
private extension View {
    @ViewBuilder
    func removeSidebarToggleIfAvailable() -> some View {
        if #available(macOS 14.0, *) {
            self.toolbar(removing: .sidebarToggle)
        } else {
            self
        }
    }
}

// MARK: - Main Preferences View
struct PreferencesView: View {
    @ObservedObject var viewModel: PreferencesViewModel
    @ObservedObject private var updater = AppUpdater.shared

    private var sidebarItems: [SidebarItem] {
        [
            SidebarItem(id: "general", title: L("prefs.tab.general"), icon: "gearshape", iconColor: Color.gray),
            SidebarItem(id: "index", title: L("prefs.tab.index"), icon: "internaldrive", iconColor: .blue, hasBadge: viewModel.isIndexDirty),
            SidebarItem(id: "privacy", title: L("prefs.tab.privacy"), icon: "hand.raised.fill", iconColor: viewModel.hasFullDiskAccess ? .green : .orange),
            SidebarItem(id: "about", title: L("prefs.tab.about"), icon: "info.circle", iconColor: .orange, hasBadge: updater.hasNewVersion)
        ]
    }

    private var currentPageTitle: String {
        switch viewModel.selectedTab {
        case "index":
            return L("prefs.tab.index")
        case "privacy":
            return L("prefs.tab.privacy")
        case "about":
            return L("prefs.tab.about")
        default:
            return L("prefs.tab.general")
        }
    }

    var body: some View {
        NavigationSplitView(columnVisibility: .constant(.all)) {
            SidebarList(items: sidebarItems, selection: Binding(
                get: { viewModel.selectedTab },
                set: { viewModel.selectedTab = $0 ?? "general" }
            ))
        } detail: {
            detailPane
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .navigationSplitViewStyle(.balanced)
        .navigationTitle(currentPageTitle)
        .removeSidebarToggleIfAvailable()
        .frame(minWidth: 580, idealWidth: 620, minHeight: 480, idealHeight: 520)
        .onAppear {
            viewModel.updateFDAStatus()
            viewModel.refreshLaunchAtLoginStatus()
            viewModel.refreshCliStatus()
            viewModel.refreshCloudDrives()
        }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            viewModel.updateFDAStatus()
            viewModel.refreshCliStatus()
            viewModel.refreshCloudDrives()
        }
    }

    @ViewBuilder
    private var detailPane: some View {
        switch viewModel.selectedTab {
        case "index":
            PreferencesIndexPane(viewModel: viewModel)
        case "privacy":
            PreferencesPrivacyPane(viewModel: viewModel)
        case "about":
            PreferencesAboutPane()
        default:
            PreferencesGeneralPane(viewModel: viewModel)
        }
    }
}
