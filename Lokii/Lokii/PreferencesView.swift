import SwiftUI
import AppKit

// MARK: - Preferences ViewModel
class PreferencesViewModel: ObservableObject {
    let engine: SearchEngine
    
    @Published var showInDock: Bool = true {
        didSet {
            saveGeneralConfig()
        }
    }
    
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
            guard language != oldValue else { return }
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
    @Published var excludePaths: [String] = []
    @Published var includeHidden: Bool = false
    
    // Saved configurations for diffing
    @Published var savedIndexPaths: [String] = []
    @Published var savedExcludePatterns: [String] = []
    @Published var savedExcludePaths: [String] = []
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
        self.showInDock = config.general.showInDock
        self.maxResults = config.index.maxResults
        self.language = config.general.language
        self.theme = ThemeManager.shared.currentTheme
        
        self.indexPaths = config.index.indexPaths
        self.excludePatterns = config.index.exclude
        self.excludePaths = config.index.excludePaths
        self.includeHidden = config.index.includeHidden
        
        self.savedIndexPaths = config.index.indexPaths
        self.savedExcludePatterns = config.index.exclude
        self.savedExcludePaths = config.index.excludePaths
        self.savedIncludeHidden = config.index.includeHidden
        isLoading = false
    }
    
    private func saveGeneralConfig() {
        guard !isLoading else { return }
        _ = engine.updateConfig(LokiiConfig(
            general: .init(language: language, showInDock: showInDock),
            index: .init(
                indexPaths: savedIndexPaths,
                exclude: savedExcludePatterns,
                excludePaths: savedExcludePaths,
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
        excludePaths != savedExcludePaths ||
        includeHidden != savedIncludeHidden
    }
    
    func saveIndexChanges() -> Bool {
        guard var config = engine.getConfig() else { return false }
        config.index.indexPaths = indexPaths
        config.index.exclude = excludePatterns
        config.index.excludePaths = excludePaths
        config.index.includeHidden = includeHidden
        
        let success = engine.updateConfig(config)
        if success {
            savedIndexPaths = indexPaths
            savedExcludePatterns = excludePatterns
            savedExcludePaths = excludePaths
            savedIncludeHidden = includeHidden
        }
        return success
    }
    
    func discardIndexChanges() {
        indexPaths = savedIndexPaths
        excludePatterns = savedExcludePatterns
        excludePaths = savedExcludePaths
        includeHidden = savedIncludeHidden
    }
    
    func restoreIndexDefaults() {
        indexPaths = Self.defaultIndexPaths
        excludePatterns = Self.defaultExcludePatterns
        excludePaths = Self.defaultExcludePaths
        includeHidden = false
    }

    /// 索引配置的出厂默认值，与 Rust 侧 IndexConfig::default 保持一致。
    /// 重置入口与“当前是否处于默认态”判定共用此定义，避免两处字面量各自漂移。
    static let defaultIndexPaths = ["~"]
    static let defaultExcludePatterns = ["node_modules", ".git", ".DS_Store", "target", ".Trash", ".hg", ".svn", "build", "dist", ".next", ".nuxt", ".cache", ".venv", "venv", "__pycache__", "DerivedData", "Pods", "Carthage", "Contents", "logs", "tmp", "temp", "coverage", ".idea", ".vscode", "vendor", "out", "bin", "obj", ".mypy_cache", ".pytest_cache", ".ruff_cache", ".tox", "xcshareddata", "xcuserdata"]
    static let defaultExcludePaths = [
        "/System", "/Library", "/private", "/bin", "/sbin", "/usr", "/dev",
        "/Volumes", "/opt", "~/Library", "~/.Trash", "~/.cache", "~/.npm",
        "~/.pnpm-store", "~/.yarn", "~/.cargo/registry", "~/.cargo/git",
        "~/.rustup", "~/.gradle/caches", "~/.m2/repository", "~/.bun",
        "~/.deno", "~/.nvm", "~/.orbstack", "~/.docker"
    ]

    var customExcludePaths: [String] {
        excludePaths.filter { !Self.defaultExcludePaths.contains($0) }
    }

    var presetExcludePaths: [String] {
        excludePaths.filter { Self.defaultExcludePaths.contains($0) }
    }

    var customExcludePatterns: [String] {
        excludePatterns.filter { !Self.defaultExcludePatterns.contains($0) }
    }

    var presetExcludePatterns: [String] {
        excludePatterns.filter { Self.defaultExcludePatterns.contains($0) }
    }

    var isDefaultIndexState: Bool {
        indexPaths == Self.defaultIndexPaths &&
        excludePatterns == Self.defaultExcludePatterns &&
        excludePaths == Self.defaultExcludePaths &&
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

// MARK: - Interactive Path Row with Subtle Hover-Fade Delete Action & Existence Guard
private struct SettingsPathRow: View {
    let icon: String
    let path: String
    let onRemove: () -> Void

    @State private var isHovered = false

    private var isPathAvailable: Bool {
        var isDir: ObjCBool = false
        let expanded = (path as NSString).expandingTildeInPath
        return FileManager.default.fileExists(atPath: expanded, isDirectory: &isDir) && isDir.boolValue
    }

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: icon)
                .foregroundStyle(isPathAvailable ? Color.accentColor : Color.secondary)
                .font(.system(size: 12))
            Text(path)
                .font(.system(size: 11, design: .monospaced))
                .lineLimit(1)
                .truncationMode(.middle)
            
            if !isPathAvailable {
                HStack(spacing: 3) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .font(.system(size: 9))
                        .foregroundStyle(.orange)
                    Text(L("prefs.index.pathNotFound"))
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary)
                }
                .help(L("prefs.index.pathNotFound.help"))
            }

            Spacer()
            Button(action: onRemove) {
                Image(systemName: "xmark.circle.fill")
                    .foregroundStyle(isHovered ? Color.red.opacity(0.85) : Color.secondary.opacity(0.35))
                    .font(.system(size: 13))
            }
            .buttonStyle(.plain)
            .opacity(isHovered ? 1.0 : 0.25)
            .animation(.easeInOut(duration: 0.15), value: isHovered)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 7)
        .background(isHovered ? Color.primary.opacity(0.035) : Color.clear)
        .contentShape(Rectangle())
        .onHover { hovering in
            isHovered = hovering
        }
    }
}

// MARK: - Main Preferences View
struct PreferencesView: View {
    @ObservedObject var viewModel: PreferencesViewModel
    var window: NSWindow? = nil
    @ObservedObject private var updater = AppUpdater.shared
    @State private var isPresetsExpanded = false
    @State private var isPatternPresetsExpanded = false
    
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
            .onAppear {
                viewModel.updateFDAStatus()
            }
            .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
                viewModel.updateFDAStatus()
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
            SettingsRow(L("prefs.showInDock"), description: L("prefs.showInDock.description")) {
                Toggle("", isOn: $viewModel.showInDock)
                    .toggleStyle(.switch)
                    .labelsHidden()
            }
            SettingsRowDivider()
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
            SettingsRow(L("prefs.language")) {
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
                    // Group 1: 索引目录 (Index Paths)
                    SettingsGroup(
                        L("prefs.index.group.paths"),
                        trailing: {
                            Menu {
                                Button {
                                    addIndexPathViaPanel()
                                } label: {
                                    Label(L("prefs.index.excludePaths.chooseFolder"), systemImage: "folder.badge.plus")
                                }
                                Button {
                                    presentIndexPathInputSheet()
                                } label: {
                                    Label(L("prefs.index.excludePaths.manualInput"), systemImage: "character.cursor.ibeam")
                                }
                            } label: {
                                HStack(spacing: 4) {
                                    Image(systemName: "plus")
                                        .font(.system(size: 10, weight: .bold))
                                    Text(L("prefs.index.addPath"))
                                        .font(.system(size: 11, weight: .medium))
                                }
                            }
                            .menuStyle(.borderedButton)
                            .menuIndicator(.hidden)
                            .controlSize(.small)
                        }
                    ) {
                        VStack(spacing: 0) {
                            if viewModel.indexPaths.isEmpty {
                                Text(L("prefs.indexPaths.empty"))
                                    .font(.system(size: 11))
                                    .foregroundStyle(.secondary)
                                    .frame(maxWidth: .infinity, alignment: .center)
                                    .padding(.vertical, 12)
                            } else {
                                ForEach(Array(viewModel.indexPaths.enumerated()), id: \.element) { idx, path in
                                    if idx > 0 {
                                        SettingsRowDivider()
                                    }
                                    SettingsPathRow(icon: "folder.fill", path: path) {
                                        viewModel.indexPaths.removeAll(where: { $0 == path })
                                    }
                                }
                            }
                        }
                    }

                    // Group 2: 排除路径 (Exclude Paths)
                    SettingsGroup(
                        L("prefs.index.group.excludePaths"),
                        description: L("prefs.index.group.excludePaths.description"),
                        trailing: {
                            Menu {
                                Button {
                                    addExcludePathViaPanel()
                                } label: {
                                    Label(L("prefs.index.excludePaths.chooseFolder"), systemImage: "folder.badge.plus")
                                }
                                Button {
                                    presentExcludePathInputSheet()
                                } label: {
                                    Label(L("prefs.index.excludePaths.manualInput"), systemImage: "character.cursor.ibeam")
                                }
                            } label: {
                                HStack(spacing: 4) {
                                    Image(systemName: "plus")
                                        .font(.system(size: 10, weight: .bold))
                                    Text(L("prefs.index.addExcludePath"))
                                        .font(.system(size: 11, weight: .medium))
                                }
                            }
                            .menuStyle(.borderedButton)
                            .menuIndicator(.hidden)
                            .controlSize(.small)
                        }
                    ) {
                        VStack(spacing: 0) {
                            // 自定义排除路径
                            if viewModel.customExcludePaths.isEmpty {
                                HStack(spacing: 6) {
                                    Image(systemName: "folder")
                                        .foregroundStyle(.secondary.opacity(0.6))
                                        .font(.system(size: 12))
                                    Text(L("prefs.index.excludePaths.empty"))
                                        .font(.system(size: 11))
                                        .foregroundStyle(.secondary)
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.horizontal, 12)
                                .padding(.vertical, 8)
                            } else {
                                ForEach(Array(viewModel.customExcludePaths.enumerated()), id: \.element) { idx, path in
                                    if idx > 0 {
                                        SettingsRowDivider()
                                    }
                                    SettingsPathRow(icon: "folder.badge.minus", path: path) {
                                        viewModel.excludePaths.removeAll(where: { $0 == path })
                                    }
                                }
                            }

                            // 系统与运行环境默认排除预设 (折叠展示)
                            if !viewModel.presetExcludePaths.isEmpty {
                                SettingsRowDivider()

                                DisclosureGroup(isExpanded: $isPresetsExpanded) {
                                    FlowLayout(spacing: 6) {
                                        ForEach(viewModel.presetExcludePaths, id: \.self) { path in
                                            PillTagView(title: path) {
                                                viewModel.excludePaths.removeAll(where: { $0 == path })
                                            }
                                        }
                                    }
                                    .padding(.top, 6)
                                    .padding(.bottom, 4)
                                } label: {
                                    HStack(spacing: 6) {
                                        Image(systemName: "shield.lefthalf.filled")
                                            .foregroundStyle(Color.accentColor)
                                            .font(.system(size: 11))
                                        Text(String(format: L("prefs.index.excludePaths.systemPresets.count"), viewModel.presetExcludePaths.count))
                                            .font(.system(size: 11, weight: .medium))
                                            .foregroundStyle(.secondary)
                                        Spacer()
                                    }
                                }
                                .padding(.horizontal, 12)
                                .padding(.vertical, 8)
                            }
                        }
                    }

                    // Group 3: 排除名称规则 (Exclude Patterns - Pill Tags)
                    SettingsGroup(
                        L("prefs.index.group.excludes"),
                        description: L("prefs.excludePatterns.description"),
                        trailing: {
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
                                        .font(.system(size: 10, weight: .bold))
                                    Text(L("prefs.index.excludePatterns.add"))
                                        .font(.system(size: 11, weight: .medium))
                                }
                            }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                        }
                    ) {
                        VStack(spacing: 0) {
                            // 自定义排除规则
                            if viewModel.customExcludePatterns.isEmpty {
                                HStack(spacing: 6) {
                                    Image(systemName: "tag")
                                        .foregroundStyle(.secondary.opacity(0.6))
                                        .font(.system(size: 12))
                                    Text(L("prefs.index.excludePatterns.empty"))
                                        .font(.system(size: 11))
                                        .foregroundStyle(.secondary)
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.horizontal, 12)
                                .padding(.vertical, 8)
                            } else {
                                FlowLayout(spacing: 6) {
                                    ForEach(viewModel.customExcludePatterns, id: \.self) { pattern in
                                        PillTagView(title: pattern) {
                                            viewModel.excludePatterns.removeAll(where: { $0 == pattern })
                                        }
                                    }
                                    AddPillButton(title: L("prefs.index.excludePatterns.add")) {
                                        presentExcludePatternSheet(
                                            title: L("prefs.excludePatterns.editor.addTitle"),
                                            initialValue: ""
                                        ) { newValue in
                                            guard let newValue = newValue else { return }
                                            if !viewModel.excludePatterns.contains(newValue) {
                                                viewModel.excludePatterns.append(newValue)
                                            }
                                        }
                                    }
                                }
                                .padding(.horizontal, 12)
                                .padding(.vertical, 8)
                            }

                            // 开发与构建缓存默认排除预设 (折叠展示)
                            if !viewModel.presetExcludePatterns.isEmpty {
                                SettingsRowDivider()

                                DisclosureGroup(isExpanded: $isPatternPresetsExpanded) {
                                    FlowLayout(spacing: 6) {
                                        ForEach(viewModel.presetExcludePatterns, id: \.self) { pattern in
                                            PillTagView(title: pattern) {
                                                viewModel.excludePatterns.removeAll(where: { $0 == pattern })
                                            }
                                        }
                                    }
                                    .padding(.top, 6)
                                    .padding(.bottom, 4)
                                } label: {
                                    HStack(spacing: 6) {
                                        Image(systemName: "shield.lefthalf.filled")
                                            .foregroundStyle(Color.accentColor)
                                            .font(.system(size: 11))
                                        Text(String(format: L("prefs.index.excludePatterns.systemPresets.count"), viewModel.presetExcludePatterns.count))
                                            .font(.system(size: 11, weight: .medium))
                                            .foregroundStyle(.secondary)
                                        Spacer()
                                    }
                                }
                                .padding(.horizontal, 12)
                                .padding(.vertical, 8)
                            }
                        }
                    }

                    // Group 4: 扫描选项
                    SettingsGroup(L("prefs.index.group.options")) {
                        SettingsRow(L("prefs.includeHidden"), description: L("prefs.includeHidden.description")) {
                            Toggle("", isOn: $viewModel.includeHidden)
                                .toggleStyle(.switch)
                                .labelsHidden()
                        }
                    }
                }
                .padding(.horizontal, 20)
                .padding(.top, 20)
                .padding(.bottom, 16)
            }
            
            // Fixed Bottom Action Bar
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
            SettingsRow(L("prefs.about.update.automatic")) {
                Toggle("", isOn: Binding(
                    get: { updater.automaticallyChecksForUpdates },
                    set: { updater.automaticallyChecksForUpdates = $0 }
                ))
                .labelsHidden()
                .toggleStyle(.switch)
                .controlSize(.small)
                .accessibilityLabel(L("prefs.about.update.automatic"))
            }
            SettingsRowDivider()
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
                        Text(L(updater.hasNewVersion ? "prefs.about.update.found" : "prefs.about.update.latest"))
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
            if updater.errorMessage != nil {
                SettingsRow(L("prefs.about.update.manualDownload")) {
                    Button(L("prefs.about.update.download")) {
                        updater.openDownloadPage()
                    }
                    .controlSize(.small)
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
            .padding(.vertical, 10)
            .background(Color(nsColor: .windowBackgroundColor))
        }
    }
    
    // MARK: - Helpers
    private var currentWindow: NSWindow? {
        if let window = window { return window }
        return NSApp.windows.first(where: { $0.frameAutosaveName == "LokiiPreferencesWindow" }) ?? NSApp.keyWindow
    }

    private func normalizePathString(_ raw: String) -> String {
        var path = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if path.hasPrefix("~/") {
            // Keep tilde
        } else if path == NSHomeDirectory() {
            path = "~"
        } else if path.hasPrefix(NSHomeDirectory() + "/") {
            path = path.replacingOccurrences(of: NSHomeDirectory(), with: "~")
        }
        // Remove trailing slashes if longer than 1 character
        while path.count > 1 && path.hasSuffix("/") {
            path.removeLast()
        }
        return path
    }

    private func addExcludePathViaPanel() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = true
        
        if let targetWindow = currentWindow {
            panel.beginSheetModal(for: targetWindow) { response in
                guard response == .OK else { return }
                for url in panel.urls {
                    let normalized = normalizePathString(url.path)
                    if !viewModel.excludePaths.contains(normalized) {
                        viewModel.excludePaths.append(normalized)
                    }
                }
            }
        }
    }

    private func checkPathExistenceAndConfirm(path: String, onConfirmed: @escaping () -> Void) {
        var isDir: ObjCBool = false
        let expanded = (path as NSString).expandingTildeInPath
        let exists = FileManager.default.fileExists(atPath: expanded, isDirectory: &isDir) && isDir.boolValue
        if exists {
            onConfirmed()
            return
        }

        let confirmAlert = NSAlert()
        confirmAlert.messageText = L("prefs.index.pathConfirm.title")
        confirmAlert.informativeText = String(format: L("prefs.index.pathConfirm.message"), path)
        let confirmBtn = confirmAlert.addButton(withTitle: L("prefs.index.pathConfirm.addAnyway"))
        confirmBtn.keyEquivalent = "\r"
        confirmAlert.addButton(withTitle: L("prefs.index.pathConfirm.cancel"))

        if let targetWindow = currentWindow {
            confirmAlert.beginSheetModal(for: targetWindow) { resp in
                if resp == .alertFirstButtonReturn {
                    onConfirmed()
                }
            }
        } else {
            if confirmAlert.runModal() == .alertFirstButtonReturn {
                onConfirmed()
            }
        }
    }

    private func presentExcludePathInputSheet() {
        let alert = NSAlert()
        alert.messageText = L("prefs.index.excludePaths.inputTitle")
        alert.informativeText = L("prefs.index.excludePaths.inputMessage")

        let confirmButton = alert.addButton(withTitle: L("prefs.excludePatterns.editor.confirm"))
        confirmButton.keyEquivalent = "\r"
        alert.addButton(withTitle: L("prefs.excludePatterns.editor.cancel"))

        let field = NSTextField(string: "")
        field.placeholderString = L("prefs.index.excludePaths.inputPlaceholder")
        field.frame = NSRect(x: 0, y: 0, width: 300, height: 24)
        alert.accessoryView = field

        if let targetWindow = currentWindow {
            alert.beginSheetModal(for: targetWindow) { response in
                guard response == .alertFirstButtonReturn else { return }
                let normalized = normalizePathString(field.stringValue)
                guard !normalized.isEmpty else { return }
                checkPathExistenceAndConfirm(path: normalized) {
                    if !viewModel.excludePaths.contains(normalized) {
                        viewModel.excludePaths.append(normalized)
                    }
                }
            }
            DispatchQueue.main.async {
                field.window?.makeFirstResponder(field)
            }
        }
    }

    private func addIndexPath() {
        addIndexPathViaPanel()
    }

    private func addIndexPathViaPanel() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = true
        
        if let targetWindow = currentWindow {
            panel.beginSheetModal(for: targetWindow) { response in
                guard response == .OK else { return }
                for url in panel.urls {
                    let normalized = normalizePathString(url.path)
                    if !viewModel.indexPaths.contains(normalized) {
                        viewModel.indexPaths.append(normalized)
                    }
                }
            }
        }
    }

    private func presentIndexPathInputSheet() {
        let alert = NSAlert()
        alert.messageText = L("prefs.index.indexPaths.inputTitle")
        alert.informativeText = L("prefs.index.indexPaths.inputMessage")

        let confirmButton = alert.addButton(withTitle: L("prefs.excludePatterns.editor.confirm"))
        confirmButton.keyEquivalent = "\r"
        alert.addButton(withTitle: L("prefs.excludePatterns.editor.cancel"))

        let field = NSTextField(string: "")
        field.placeholderString = L("prefs.index.indexPaths.inputPlaceholder")
        field.frame = NSRect(x: 0, y: 0, width: 300, height: 24)
        alert.accessoryView = field

        if let targetWindow = currentWindow {
            alert.beginSheetModal(for: targetWindow) { response in
                guard response == .alertFirstButtonReturn else { return }
                let normalized = normalizePathString(field.stringValue)
                guard !normalized.isEmpty else { return }
                checkPathExistenceAndConfirm(path: normalized) {
                    if !viewModel.indexPaths.contains(normalized) {
                        viewModel.indexPaths.append(normalized)
                    }
                }
            }
            DispatchQueue.main.async {
                field.window?.makeFirstResponder(field)
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
