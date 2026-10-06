import SwiftUI
import AppKit

// MARK: - Preferences ViewModel
class PreferencesViewModel: ObservableObject {
    let engine: SearchEngine
    
    @Published var hideOnBlur: Bool = false {
        didSet {
            saveGeneralConfig()
        }
    }
    
    @Published var rememberPosition: Bool = false {
        didSet {
            saveGeneralConfig()
        }
    }
    
    @Published var maxResults: Int = 100 {
        didSet {
            saveGeneralConfig()
        }
    }
    
    @Published var language: String = "zh-Hans" {
        didSet {
            saveGeneralConfig()
            L10n.configure(language: language)
        }
    }
    
    @Published var theme: AppTheme = ThemeManager.shared.currentTheme {
        didSet {
            ThemeManager.shared.applyTheme(theme)
        }
    }
    
    // Draft states for index configurations
    @Published var indexPaths: [String] = []
    @Published var excludePatterns: [String] = []
    @Published var includeHidden: Bool = false
    
    // Saved configurations for diffing
    @Published var savedIndexPaths: [String] = []
    @Published var savedExcludePatterns: [String] = []
    @Published var savedIncludeHidden: Bool = false
    
    @Published var hasFullDiskAccess: Bool = false
    @Published var selectedTab: String = "general"
    
    private var isLoading = false
    
    init(engine: SearchEngine) {
        self.engine = engine
        loadConfig()
        updateFDAStatus()
    }
    
    func updateFDAStatus() {
        self.hasFullDiskAccess = engine.hasFullDiskAccess
    }
    
    func loadConfig() {
        guard let config = engine.getConfig() else { return }
        
        isLoading = true
        self.hideOnBlur = config.window.hideOnBlur
        self.rememberPosition = config.window.rememberPosition
        self.maxResults = config.index.maxResults
        self.language = config.general.language
        self.theme = ThemeManager.shared.currentTheme
        
        self.indexPaths = config.index.indexPaths
        self.excludePatterns = config.index.exclude
        self.includeHidden = config.index.includeHidden
        
        self.savedIndexPaths = config.index.indexPaths
        self.savedExcludePatterns = config.index.exclude
        self.savedIncludeHidden = config.index.includeHidden
        isLoading = false
    }
    
    private func saveGeneralConfig() {
        guard !isLoading else { return }
        _ = engine.updateConfig(LokiiConfig(
            general: .init(language: language),
            index: .init(
                indexPaths: savedIndexPaths,
                exclude: savedExcludePatterns,
                includeHidden: savedIncludeHidden,
                maxResults: maxResults
            ),
            window: .init(
                hideOnBlur: hideOnBlur,
                rememberPosition: rememberPosition
            )
        ))
    }
    
    var isIndexDirty: Bool {
        indexPaths != savedIndexPaths ||
        excludePatterns != savedExcludePatterns ||
        includeHidden != savedIncludeHidden
    }
    
    func saveIndexChanges() -> Bool {
        guard var config = engine.getConfig() else { return false }
        config.index.indexPaths = indexPaths
        config.index.exclude = excludePatterns
        config.index.includeHidden = includeHidden
        
        let success = engine.updateConfig(config)
        if success {
            savedIndexPaths = indexPaths
            savedExcludePatterns = excludePatterns
            savedIncludeHidden = includeHidden
        }
        return success
    }
    
    func discardIndexChanges() {
        indexPaths = savedIndexPaths
        excludePatterns = savedExcludePatterns
        includeHidden = savedIncludeHidden
    }
    
    func restoreIndexDefaults() {
        indexPaths = ["~"]
        excludePatterns = ["node_modules", ".git", ".DS_Store", "target", ".Trash", ".hg", ".svn", "build", "dist", ".next", ".nuxt", ".cache", ".venv", "venv", "__pycache__", "DerivedData", "Pods", "Carthage", "Contents", "logs", "tmp", "temp", "coverage", ".idea", ".vscode", "vendor", "out", "bin", "obj", ".mypy_cache", ".pytest_cache", ".ruff_cache", ".tox", "xcshareddata", "xcuserdata"]
        includeHidden = false
    }
    
    var isDefaultIndexState: Bool {
        indexPaths == ["~"] &&
        excludePatterns == ["node_modules", ".git", ".DS_Store", "target", ".Trash", ".hg", ".svn", "build", "dist", ".next", ".nuxt", ".cache", ".venv", "venv", "__pycache__", "DerivedData", "Pods", "Carthage", "Contents", "logs", "tmp", "temp", "coverage", ".idea", ".vscode", "vendor", "out", "bin", "obj", ".mypy_cache", ".pytest_cache", ".ruff_cache", ".tox", "xcshareddata", "xcuserdata"] &&
        !includeHidden
    }
}

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
    var window: NSWindow? = nil
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
    
    @ViewBuilder
    private var splitView: some View {
        NavigationSplitView(columnVisibility: .constant(.all)) {
            SidebarList(items: sidebarItems, selection: Binding(
                get: { viewModel.selectedTab },
                set: { viewModel.selectedTab = $0 ?? "general" }
            ))
        } detail: {
            switch viewModel.selectedTab {
            case "index":
                indexPane
            default:
                ScrollView(showsIndicators: false) {
                    VStack(spacing: 20) {
                        detailPane
                    }
                    .padding(.horizontal, 20)
                    .padding(.vertical, 20)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .navigationSplitViewStyle(.balanced)
        .navigationTitle(currentPageTitle)
        .removeSidebarToggleIfAvailable()
    }
    
    var body: some View {
        splitView
            .frame(minWidth: 580, idealWidth: 620, minHeight: 480, idealHeight: 520)
            .id(viewModel.language) // Force full layout redraw on language changes
            .onAppear {
                viewModel.updateFDAStatus()
                // Observe window active updates to check Full Disk Access status in real-time
                NotificationCenter.default.addObserver(
                    forName: NSApplication.didBecomeActiveNotification,
                    object: nil,
                    queue: .main
                ) { _ in
                    viewModel.updateFDAStatus()
                }
            }
    }
    
    // MARK: - Detail Pane Dispatcher
    @ViewBuilder
    private var detailPane: some View {
        switch viewModel.selectedTab {
        case "privacy":
            privacyPane
        case "about":
            aboutPane
        default:
            generalPane
        }
    }
    
    // MARK: - General Pane
    @ViewBuilder
    private var generalPane: some View {
        SettingsGroup(L("prefs.section.behavior")) {
            SettingsRow(L("prefs.hideOnBlur"), description: L("prefs.hideOnBlur.description")) {
                Toggle("", isOn: $viewModel.hideOnBlur)
                    .toggleStyle(.switch)
                    .labelsHidden()
            }
            SettingsRowDivider()
            SettingsRow(L("prefs.rememberPosition"), description: L("prefs.rememberPosition.description")) {
                Toggle("", isOn: $viewModel.rememberPosition)
                    .toggleStyle(.switch)
                    .labelsHidden()
            }
        }
        
        SettingsGroup(L("prefs.section.search")) {
            SettingsRow(L("prefs.maxResults"), description: L("prefs.maxResults.description")) {
                HStack(spacing: 8) {
                    Text("\(viewModel.maxResults)")
                        .font(.system(size: 12, weight: .semibold, design: .monospaced))
                    Stepper("", value: $viewModel.maxResults, in: 50...5000, step: 50)
                        .labelsHidden()
                }
            }
            SettingsRowDivider()
            SettingsRow(L("prefs.globalShortcut"), description: L("prefs.globalShortcut.description")) {
                KeyboardShortcuts.Recorder(for: .toggleWindow)
            }
        }
        
        SettingsGroup(L("prefs.section.appearance")) {
            SettingsRow(L("prefs.appearance"), description: L("prefs.appearance.description")) {
                Picker("", selection: $viewModel.theme) {
                    Text(L("prefs.appearance.system")).tag(AppTheme.system)
                    Text(L("prefs.appearance.light")).tag(AppTheme.light)
                    Text(L("prefs.appearance.dark")).tag(AppTheme.dark)
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .frame(width: 200)
            }
        }
        
        SettingsGroup(L("prefs.section.language")) {
            SettingsRow(L("prefs.language"), description: L("prefs.language.restartHint")) {
                Picker("", selection: $viewModel.language) {
                    Text(L("prefs.language.zh-Hans")).tag("zh-Hans")
                    Text(L("prefs.language.en")).tag("en")
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .frame(width: 140)
            }
        }
    }
    
    // MARK: - Index Pane
    @ViewBuilder
    private var indexPane: some View {
        VStack(spacing: 0) {
            ScrollView(showsIndicators: false) {
                VStack(spacing: 20) {
                    // Group 1: Index Paths
                    SettingsGroup(L("prefs.index.group.paths")) {
                        VStack(spacing: 8) {
                            if viewModel.indexPaths.isEmpty {
                                Text(L("prefs.indexPaths.empty"))
                                    .font(.system(size: 11))
                                    .foregroundStyle(.secondary)
                                    .frame(maxWidth: .infinity, alignment: .center)
                                    .padding(.vertical, 8)
                            } else {
                                VStack(alignment: .leading, spacing: 4) {
                                    ForEach(viewModel.indexPaths, id: \.self) { path in
                                        HStack {
                                            Text(path)
                                                .font(.system(size: 11, design: .monospaced))
                                                .lineLimit(1)
                                                .truncationMode(.middle)
                                            Spacer()
                                            Button {
                                                if let index = viewModel.indexPaths.firstIndex(of: path) {
                                                    viewModel.indexPaths.remove(at: index)
                                                }
                                            } label: {
                                                Image(systemName: "minus.circle.fill")
                                                    .foregroundStyle(.red.opacity(0.8))
                                                    .font(.system(size: 12))
                                            }
                                            .buttonStyle(.plain)
                                        }
                                        .padding(.horizontal, 8)
                                        .padding(.vertical, 5)
                                        .background(Color.primary.opacity(0.04))
                                        .cornerRadius(6)
                                    }
                                }
                            }
                            
                            Button {
                                addIndexPath()
                            } label: {
                                HStack(spacing: 4) {
                                    Image(systemName: "plus")
                                    Text(L("prefs.index.addPath"))
                                }
                                .font(.system(size: 11, weight: .medium))
                                .frame(maxWidth: .infinity)
                                .padding(.vertical, 6)
                                .background(Color.accentColor.opacity(0.1))
                                .cornerRadius(6)
                                .foregroundStyle(Color.accentColor)
                            }
                            .buttonStyle(.plain)
                        }
                        .padding(12)
                    }
                    
                    // Group 2: Exclude Patterns
                    SettingsGroup(L("prefs.index.group.excludes")) {
                        VStack(spacing: 8) {
                            if viewModel.excludePatterns.isEmpty {
                                Text(L("prefs.excludePatterns.empty"))
                                    .font(.system(size: 11))
                                    .foregroundStyle(.secondary)
                                    .frame(maxWidth: .infinity, alignment: .center)
                                    .padding(.vertical, 8)
                            } else {
                                VStack(alignment: .leading, spacing: 4) {
                                    ForEach(viewModel.excludePatterns, id: \.self) { pattern in
                                        HStack {
                                            Text(pattern)
                                                .font(.system(size: 11, design: .monospaced))
                                            Spacer()
                                            HStack(spacing: 8) {
                                                Button {
                                                    presentExcludePatternSheet(
                                                        title: L("prefs.excludePatterns.editor.editTitle"),
                                                        initialValue: pattern
                                                    ) { newValue in
                                                        guard let newValue = newValue else { return }
                                                        if let idx = viewModel.excludePatterns.firstIndex(of: pattern) {
                                                            viewModel.excludePatterns[idx] = newValue
                                                        }
                                                    }
                                                } label: {
                                                    Image(systemName: "pencil")
                                                        .foregroundStyle(.secondary)
                                                        .font(.system(size: 11))
                                                }
                                                .buttonStyle(.plain)
                                                
                                                Button {
                                                    if let index = viewModel.excludePatterns.firstIndex(of: pattern) {
                                                        viewModel.excludePatterns.remove(at: index)
                                                    }
                                                } label: {
                                                    Image(systemName: "minus.circle.fill")
                                                        .foregroundStyle(.red.opacity(0.8))
                                                        .font(.system(size: 12))
                                                }
                                                .buttonStyle(.plain)
                                            }
                                        }
                                        .padding(.horizontal, 8)
                                        .padding(.vertical, 5)
                                        .background(Color.primary.opacity(0.04))
                                        .cornerRadius(6)
                                    }
                                }
                            }
                            
                            Button {
                                presentExcludePatternSheet(
                                    title: L("prefs.excludePatterns.editor.addTitle"),
                                    initialValue: ""
                                ) { newValue in
                                    guard let newValue = newValue else { return }
                                    if !viewModel.excludePatterns.contains(newValue) {
                                        viewModel.excludePatterns.append(newValue)
                                    }
                                }
                            } label: {
                                HStack(spacing: 4) {
                                    Image(systemName: "plus")
                                    Text(L("prefs.index.addExclude"))
                                }
                                .font(.system(size: 11, weight: .medium))
                                .frame(maxWidth: .infinity)
                                .padding(.vertical, 6)
                                .background(Color.accentColor.opacity(0.1))
                                .cornerRadius(6)
                                .foregroundStyle(Color.accentColor)
                            }
                            .buttonStyle(.plain)
                        }
                        .padding(12)
                    }
                    
                    // Group 3: Scanning Options
                    SettingsGroup(L("prefs.index.group.options")) {
                        SettingsRow(L("prefs.includeHidden"), description: L("prefs.includeHidden.description")) {
                            Toggle("", isOn: $viewModel.includeHidden)
                                .toggleStyle(.switch)
                                .labelsHidden()
                        }
                    }
                }
                .padding(20)
            }
            
            // Sticky Bottom Action Bar
            indexActionBar
        }
    }
    
    // MARK: - Privacy Pane
    @ViewBuilder
    private var privacyPane: some View {
        SettingsGroup(L("prefs.privacy.fullDiskAccess")) {
            SettingsRow(
                viewModel.hasFullDiskAccess ? L("prefs.privacy.status.granted") : L("prefs.privacy.status.missing")
            ) {
                HStack(spacing: 6) {
                    Circle()
                        .fill(viewModel.hasFullDiskAccess ? Color.green : Color.orange)
                        .frame(width: 8, height: 8)
                    Text(viewModel.hasFullDiskAccess ? L("prefs.privacy.badge.granted") : L("prefs.privacy.badge.missing"))
                        .font(.system(size: 11, weight: .medium))
                        .foregroundStyle(viewModel.hasFullDiskAccess ? .green : .orange)
                }
            }
            
            SettingsRowDivider()
            
            VStack(alignment: .leading, spacing: 12) {
                Text(viewModel.hasFullDiskAccess ? L("prefs.privacy.status.granted.description") : L("prefs.privacy.status.missing.description"))
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                
                if !viewModel.hasFullDiskAccess {
                    HStack(spacing: 12) {
                        Button {
                            FDAGuideWindowController.show()
                        } label: {
                            Text(L("prefs.privacy.viewGuide"))
                                .font(.system(size: 12, weight: .medium))
                                .frame(maxWidth: .infinity)
                                .padding(.vertical, 6)
                                .background(Color.primary.opacity(0.06))
                                .cornerRadius(6)
                        }
                        .buttonStyle(.plain)
                        
                        Button {
                            FDAGuideWindowController.openSystemSettingsPage()
                        } label: {
                            Text(L("prefs.privacy.openSettings"))
                                .font(.system(size: 12, weight: .medium))
                                .frame(maxWidth: .infinity)
                                .padding(.vertical, 6)
                                .background(Color.accentColor)
                                .foregroundColor(.white)
                                .cornerRadius(6)
                        }
                        .buttonStyle(.plain)
                    }
                } else {
                    Text(L("prefs.privacy.rebuildHint"))
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .padding(12)
        }
    }
    
    // MARK: - About Pane
    @ViewBuilder
    private var aboutPane: some View {
        // Header
        VStack(spacing: 8) {
            if let appIcon = NSImage(named: "AppIcon") ?? NSApp.applicationIconImage {
                Image(nsImage: appIcon)
                    .resizable()
                    .scaledToFit()
                    .frame(width: 80, height: 80)
            } else {
                Image(systemName: "magnifyingglass.circle.fill")
                    .resizable()
                    .scaledToFit()
                    .frame(width: 80, height: 80)
                    .foregroundStyle(Color.accentColor)
                    .clipShape(RoundedRectangle(cornerRadius: 18, style: .continuous))
            }
            
            Text("Lokii")
                .font(.title2)
                .bold()
            
            let version = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? ""
            if !version.isEmpty {
                Text("v\(version)")
                    .font(.system(size: 12, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
            
            Text(L("about.tagline"))
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 12)
        
        // Update Section
        SettingsGroup(L("prefs.about.update.section")) {
            if updater.hasNewVersion, let release = updater.latestRelease {
                VStack(alignment: .leading, spacing: 10) {
                    HStack(alignment: .center, spacing: 10) {
                        Image(systemName: "arrow.down.circle.fill")
                            .font(.system(size: 20))
                            .foregroundStyle(.green)
                        
                        VStack(alignment: .leading, spacing: 2) {
                            Text(String(format: L("prefs.about.update.available"), release.tagName))
                                .font(.system(size: 13, weight: .semibold))
                            if let body = release.body, !body.isEmpty {
                                Text(body.prefix(120) + (body.count > 120 ? "..." : ""))
                                    .font(.system(size: 11))
                                    .foregroundStyle(.secondary)
                                    .lineLimit(2)
                            }
                        }
                        
                        Spacer()
                        
                        Button(L("prefs.about.update.download")) {
                            updater.openDownloadPage()
                        }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.small)
                    }
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
            } else {
                SettingsRow(L("prefs.about.update.status")) {
                    HStack(spacing: 8) {
                        if updater.isChecking {
                            ProgressView()
                                .controlSize(.small)
                            Text(L("prefs.about.update.checking"))
                                .font(.system(size: 11))
                                .foregroundStyle(.secondary)
                        } else if let error = updater.errorMessage {
                            Text(error)
                                .font(.system(size: 11))
                                .foregroundStyle(.red)
                                .lineLimit(1)
                        } else if updater.lastCheckTime != nil {
                            Text(L("prefs.about.update.latest"))
                                .font(.system(size: 11))
                                .foregroundStyle(.secondary)
                        }
                        
                        Button(L("prefs.about.update.checkNow")) {
                            updater.checkForUpdates(isUserInitiated: true)
                        }
                        .disabled(updater.isChecking)
                        .controlSize(.small)
                    }
                }
            }
        }
        
        // Info Group
        SettingsGroup {
            SettingsRow(L("prefs.about.architecture")) {
                Text(L("prefs.about.architecture.value"))
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
            }
            SettingsRowDivider()
            SettingsRow(L("prefs.about.license")) {
                Text(L("prefs.about.license.value"))
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
            }
            SettingsRowDivider()
            SettingsRow(L("prefs.about.source")) {
                Button(L("prefs.about.source.action")) {
                    if let url = URL(string: "https://github.com/huangy7/lokii") {
                        NSWorkspace.shared.open(url)
                    }
                }
                .buttonStyle(.link)
            }
        }
    }
    
    // MARK: - Bottom Action Bar (Index Settings only)
    private var indexActionBar: some View {
        VStack(spacing: 0) {
            Divider()
            HStack(spacing: 12) {
                if viewModel.isIndexDirty {
                    HStack(spacing: 4) {
                        Image(systemName: "exclamationmark.circle.fill")
                            .foregroundStyle(.orange)
                            .font(.system(size: 11))
                        Text(L("prefs.index.unsaved"))
                            .font(.system(size: 11))
                            .foregroundStyle(.secondary)
                    }
                }
                Spacer()
                
                Button {
                    viewModel.restoreIndexDefaults()
                } label: {
                    Text(L("prefs.resetToDefaults"))
                        .font(.system(size: 12))
                        .padding(.horizontal, 12)
                        .padding(.vertical, 5)
                        .background(Color.primary.opacity(0.06))
                        .cornerRadius(6)
                }
                .buttonStyle(.plain)
                .disabled(!viewModel.isIndexDirty && viewModel.isDefaultIndexState)
                
                Button {
                    _ = viewModel.saveIndexChanges()
                } label: {
                    Text(L("prefs.save"))
                        .font(.system(size: 12, weight: .semibold))
                        .padding(.horizontal, 12)
                        .padding(.vertical, 5)
                        .background(viewModel.isIndexDirty ? Color.accentColor : Color.primary.opacity(0.05))
                        .foregroundColor(viewModel.isIndexDirty ? .white : .secondary)
                        .cornerRadius(6)
                }
                .buttonStyle(.plain)
                .disabled(!viewModel.isIndexDirty)
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 12)
            .background(Color(nsColor: .windowBackgroundColor).opacity(0.3))
        }
    }
    
    // MARK: - Helpers
    private var currentWindow: NSWindow? {
        if let window = window { return window }
        return NSApp.windows.first(where: { $0.frameAutosaveName == "LokiiPreferencesWindow" }) ?? NSApp.keyWindow
    }

    private func addIndexPath() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = true
        
        if let targetWindow = currentWindow {
            panel.beginSheetModal(for: targetWindow) { response in
                guard response == .OK else { return }
                for url in panel.urls {
                    var path = url.path
                    path = path.replacingOccurrences(of: NSHomeDirectory(), with: "~")
                    if !viewModel.indexPaths.contains(path) {
                        viewModel.indexPaths.append(path)
                    }
                }
            }
        }
    }
    
    private func presentExcludePatternSheet(
        title: String,
        initialValue: String,
        completion: @escaping (String?) -> Void
    ) {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = L("prefs.excludePatterns.editor.message")

        let confirmButton = alert.addButton(withTitle: L("prefs.excludePatterns.editor.confirm"))
        confirmButton.keyEquivalent = "\r"
        alert.addButton(withTitle: L("prefs.excludePatterns.editor.cancel"))

        let field = NSTextField(string: initialValue)
        field.placeholderString = L("prefs.excludePatterns.editor.placeholder")
        field.frame = NSRect(x: 0, y: 0, width: 280, height: 24)
        alert.accessoryView = field

        if let targetWindow = currentWindow {
            alert.beginSheetModal(for: targetWindow) { response in
                guard response == .alertFirstButtonReturn else {
                    completion(nil)
                    return
                }

                let trimmed = field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
                completion(trimmed.isEmpty ? nil : trimmed)
            }
            
            DispatchQueue.main.async {
                field.window?.makeFirstResponder(field)
                field.selectText(nil)
            }
        } else {
            completion(nil)
        }
    }
}
