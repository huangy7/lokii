import SwiftUI
import AppKit
import ServiceManagement

// MARK: - CLI Tool Install State
enum CliInstallState: Equatable {
    case notInstalled
    case installing
    case installed
    case failed(message: String, detail: String)
}

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
            guard theme != oldValue, !isLoading else { return }
            ThemeManager.shared.applyTheme(theme)
        }
    }

    @Published var terminalBundleId: String = TerminalLauncher.systemTerminalBundleId {
        didSet {
            guard terminalBundleId != oldValue, !isLoading else { return }
            saveGeneralConfig()
            refreshAvailableTerminals()
        }
    }

    @Published var availableTerminals: [TerminalAppItem] = []

    /// 命令行工具安装状态（未安装 / 安装中 / 已安装 / 安装失败）
    @Published var cliState: CliInstallState = .notInstalled

    let cliTarget = "/usr/local/bin/lokii"
    var cliSource: String {
        Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/lokii").path
    }

    /// 软链接目标是否属于 Lokii；刷新、安装、卸载共用，避免误碰用户自己的软链接
    private func isOurCliLink(_ dest: String) -> Bool {
        dest == cliSource || dest.hasSuffix("/Contents/Helpers/lokii")
    }

    /// 失败后的“重试”重做刚才失败的动作，而不是一律执行安装
    private var lastCliActionWasUninstall = false

    func retryCommandLineTool() {
        if lastCliActionWasUninstall {
            uninstallCommandLineTool()
        } else {
            installCommandLineTool()
        }
    }

    /// 当前生效的终端项，供菜单标签与图标统一取值
    var selectedTerminal: TerminalAppItem? {
        availableTerminals.first { $0.id == terminalBundleId }
    }

    // 索引配置草稿态
    @Published var indexPaths: [String] = []
    @Published var indexApps: Bool = false
    @Published var excludePatterns: [String] = []
    @Published var excludePaths: [String] = []
    @Published var includeHidden: Bool = false
    @Published var excludeSensitive: Bool = true
    @Published var networkRescanMinutes: Int = 5

    // 已持久化的索引配置，用于脏状态比对与放弃更改恢复
    @Published var savedIndexPaths: [String] = []
    @Published var savedIndexApps: Bool = false
    @Published var savedExcludePatterns: [String] = []
    @Published var savedExcludePaths: [String] = []
    @Published var savedIncludeHidden: Bool = false
    @Published var savedExcludeSensitive: Bool = true
    private var sensitiveNames: [String] = []
    @Published var savedNetworkRescanMinutes: Int = 5

    @Published var hasFullDiskAccess: Bool = false
    @Published var selectedTab: String = "general"
    @Published var launchAtLogin: Bool = false {
        didSet {
            guard !isLoading, launchAtLogin != oldValue else { return }
            setLaunchAtLogin(enabled: launchAtLogin)
        }
    }

    private var isLoading = false

    init(engine: SearchEngine) {
        self.engine = engine
        loadConfig()
        updateFDAStatus()
        refreshLaunchAtLoginStatus()
        refreshCliStatus()
    }

    func refreshLaunchAtLoginStatus() {
        if #available(macOS 13.0, *) {
            let wasLoading = isLoading
            isLoading = true
            defer { isLoading = wasLoading }
            self.launchAtLogin = SMAppService.mainApp.status == .enabled
        }
    }

    private func setLaunchAtLogin(enabled: Bool) {
        if #available(macOS 13.0, *) {
            do {
                if enabled {
                    if SMAppService.mainApp.status == .enabled { return }
                    try SMAppService.mainApp.register()
                } else {
                    if SMAppService.mainApp.status == .notFound { return }
                    try SMAppService.mainApp.unregister()
                }
            } catch {
                NSLog("Failed to update SMAppService launch at login: \(error.localizedDescription)")
                refreshLaunchAtLoginStatus()
            }
        }
    }

    func updateFDAStatus() {
        self.hasFullDiskAccess = engine.hasFullDiskAccess
    }

    func refreshAvailableTerminals() {
        self.availableTerminals = TerminalLauncher.installedTerminals(selectedId: terminalBundleId)
    }

    func chooseCustomTerminal(window: NSWindow? = nil) {
        TerminalLauncher.chooseCustomTerminal(window: window) { [weak self] item in
            guard let self = self, let item = item else { return }
            self.terminalBundleId = item.id
        }
    }

    /// 校验并刷新 /usr/local/bin/lokii 软链接状态
    func refreshCliStatus() {
        let fm = FileManager.default
        var isInstalled = false
        if let attrs = try? fm.attributesOfItem(atPath: cliTarget),
           attrs[.type] as? FileAttributeType == .typeSymbolicLink,
           let dest = try? fm.destinationOfSymbolicLink(atPath: cliTarget) {
            isInstalled = isOurCliLink(dest) && fm.fileExists(atPath: cliTarget)
        }

        // 安装/卸载进行中时不让被动窗口激活轮询覆盖状态
        if case .installing = cliState {
            return
        }

        if isInstalled {
            self.cliState = .installed
        } else {
            if case .failed = cliState {
                // 保留失败信息供用户重试或查看日志
            } else {
                self.cliState = .notInstalled
            }
        }
    }

    /// 安装 App 内置的 `lokii` CLI 软链接到 /usr/local/bin
    func installCommandLineTool() {
        guard cliState != .installing else { return }
        lastCliActionWasUninstall = false
        cliState = .installing

        let source = cliSource
        let target = cliTarget
        let fm = FileManager.default

        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            guard let self = self else { return }

            guard fm.isExecutableFile(atPath: source) else {
                DispatchQueue.main.async {
                    self.cliState = .failed(
                        message: L("prefs.cli.error.notFound"),
                        detail: String(format: L("prefs.cli.missing"), source)
                    )
                }
                return
            }

            // 不覆盖用户自己放在同名位置的真实文件或指向别处的软链接
            if let attrs = try? fm.attributesOfItem(atPath: target) {
                let dest = try? fm.destinationOfSymbolicLink(atPath: target)
                let isOurs = attrs[.type] as? FileAttributeType == .typeSymbolicLink
                    && dest.map(self.isOurCliLink) == true
                if !isOurs {
                    DispatchQueue.main.async {
                        self.cliState = .failed(
                            message: L("prefs.cli.error.conflict"),
                            detail: String(format: L("prefs.cli.conflict"), target)
                        )
                    }
                    return
                }
            }

            do {
                try fm.createDirectory(atPath: "/usr/local/bin", withIntermediateDirectories: true)
                try? fm.removeItem(atPath: target)
                try fm.createSymbolicLink(atPath: target, withDestinationPath: source)
                DispatchQueue.main.async {
                    self.cliState = .installed
                }
            } catch {
                let escaped = source.replacingOccurrences(of: "\\", with: "\\\\")
                    .replacingOccurrences(of: "\"", with: "\\\"")
                let script = "do shell script \"mkdir -p /usr/local/bin && ln -sfh \" & quoted form of \"\(escaped)\" & \" \(target)\" with administrator privileges"
                var errorInfo: NSDictionary?
                NSAppleScript(source: script)?.executeAndReturnError(&errorInfo)

                DispatchQueue.main.async {
                    if let errorInfo = errorInfo {
                        let errNum = errorInfo[NSAppleScript.errorNumber] as? Int
                        if errNum == -128 {
                            self.cliState = .notInstalled
                        } else {
                            let errDetail = errorInfo[NSAppleScript.errorMessage] as? String ?? "Unknown error"
                            self.cliState = .failed(
                                message: L("prefs.cli.error.permission"),
                                detail: errDetail
                            )
                        }
                    } else {
                        self.cliState = .installed
                    }
                }
            }
        }
    }

    /// 移除 /usr/local/bin/lokii 软链接；严格校验目标确实指向当前应用的 Helpers/lokii，避免误删用户文件
    func uninstallCommandLineTool() {
        guard cliState != .installing else { return }
        lastCliActionWasUninstall = true
        cliState = .installing
        let target = cliTarget
        let fm = FileManager.default

        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            guard let self = self else { return }

            guard let attrs = try? fm.attributesOfItem(atPath: target) else {
                DispatchQueue.main.async { self.cliState = .notInstalled }
                return
            }

            guard attrs[.type] as? FileAttributeType == .typeSymbolicLink else {
                DispatchQueue.main.async {
                    self.cliState = .failed(
                        message: L("prefs.cli.error.conflict"),
                        detail: String(format: L("prefs.cli.conflict"), target)
                    )
                }
                return
            }

            guard let dest = try? fm.destinationOfSymbolicLink(atPath: target) else {
                DispatchQueue.main.async {
                    self.cliState = .failed(
                        message: L("prefs.cli.error.uninstallFailed"),
                        detail: String(format: L("prefs.cli.readLinkFailed"), target)
                    )
                }
                return
            }

            guard self.isOurCliLink(dest) else {
                DispatchQueue.main.async {
                    self.cliState = .failed(
                        message: L("prefs.cli.error.notOurSymlink"),
                        detail: String(format: L("prefs.cli.notOurSymlink"), dest)
                    )
                }
                return
            }

            do {
                try fm.removeItem(atPath: target)
                DispatchQueue.main.async {
                    self.cliState = .notInstalled
                }
            } catch {
                let script = "do shell script \"rm -f \(target)\" with administrator privileges"
                var errorInfo: NSDictionary?
                NSAppleScript(source: script)?.executeAndReturnError(&errorInfo)

                DispatchQueue.main.async {
                    if let errorInfo = errorInfo {
                        let errNum = errorInfo[NSAppleScript.errorNumber] as? Int
                        if errNum == -128 {
                            self.cliState = .installed
                        } else {
                            let errDetail = errorInfo[NSAppleScript.errorMessage] as? String ?? "Unknown error"
                            self.cliState = .failed(
                                message: L("prefs.cli.error.uninstallFailed"),
                                detail: errDetail
                            )
                        }
                    } else {
                        self.cliState = .notInstalled
                    }
                }
            }
        }
    }

    func loadConfig() {
        guard let config = engine.getConfig() else { return }

        isLoading = true
        self.hideOnBlur = config.window.hideOnBlur
        self.rememberPosition = config.window.rememberPosition
        self.showInDock = config.general.showInDock
        self.maxResults = config.index.maxResults
        self.language = config.general.language
        self.terminalBundleId = config.general.terminalBundleId
        self.theme = ThemeManager.shared.currentTheme

        self.indexPaths = config.index.indexPaths
        self.indexApps = config.index.indexApps
        self.excludePatterns = config.index.exclude
        self.excludePaths = config.index.excludePaths
        self.includeHidden = config.index.includeHidden
        self.excludeSensitive = config.index.excludeSensitive
        self.networkRescanMinutes = config.index.networkRescanMinutes

        self.savedIndexPaths = config.index.indexPaths
        self.savedIndexApps = config.index.indexApps
        self.savedExcludePatterns = config.index.exclude
        self.savedExcludePaths = config.index.excludePaths
        self.savedIncludeHidden = config.index.includeHidden
        self.savedExcludeSensitive = config.index.excludeSensitive
        self.sensitiveNames = config.index.sensitiveNames
        self.savedNetworkRescanMinutes = config.index.networkRescanMinutes
        isLoading = false
        refreshAvailableTerminals()
    }

    private func saveGeneralConfig() {
        guard !isLoading else { return }
        _ = engine.updateConfig(LokiiConfig(
            general: .init(language: language, showInDock: showInDock, terminalBundleId: terminalBundleId),
            index: .init(
                indexPaths: savedIndexPaths,
                indexApps: savedIndexApps,
                exclude: savedExcludePatterns,
                excludePaths: savedExcludePaths,
                includeHidden: savedIncludeHidden,
                excludeSensitive: savedExcludeSensitive,
                sensitiveNames: sensitiveNames,
                maxResults: maxResults,
                networkRescanMinutes: savedNetworkRescanMinutes
            ),
            window: .init(
                hideOnBlur: hideOnBlur,
                rememberPosition: rememberPosition
            )
        ))
    }

    var isIndexDirty: Bool {
        indexPaths != savedIndexPaths ||
        indexApps != savedIndexApps ||
        excludePatterns != savedExcludePatterns ||
        excludePaths != savedExcludePaths ||
        includeHidden != savedIncludeHidden ||
        excludeSensitive != savedExcludeSensitive ||
        networkRescanMinutes != savedNetworkRescanMinutes
    }

    func saveIndexChanges() -> Bool {
        guard var config = engine.getConfig() else { return false }
        // 判定是否需要触发后台全量重建：
        // 仅追加新的排除路径（内核可就地毫秒级剔除）或仅调整网络卷定时重扫间隔时，无需重建本地全量索引；
        // 其余任何范围扩大或规则放宽变更均需触发后台重建以重新录入文件。
        let onlyNonRebuildChanges = (indexPaths == savedIndexPaths) &&
                                    (indexApps == savedIndexApps) &&
                                    (includeHidden == savedIncludeHidden) &&
                                    (excludeSensitive == savedExcludeSensitive) &&
                                    (excludePatterns == savedExcludePatterns) &&
                                    (savedExcludePaths.allSatisfy { excludePaths.contains($0) })
        let needsRebuild = isIndexDirty && !onlyNonRebuildChanges

        config.index.indexPaths = indexPaths
        config.index.indexApps = indexApps
        config.index.exclude = excludePatterns
        config.index.excludePaths = excludePaths
        config.index.includeHidden = includeHidden
        config.index.excludeSensitive = excludeSensitive
        config.index.networkRescanMinutes = networkRescanMinutes

        let success = engine.updateConfig(config)
        if success {
            savedIndexPaths = indexPaths
            savedIndexApps = indexApps
            savedExcludePatterns = excludePatterns
            savedExcludePaths = excludePaths
            savedIncludeHidden = includeHidden
            savedExcludeSensitive = excludeSensitive
            savedNetworkRescanMinutes = networkRescanMinutes
            if needsRebuild {
                engine.rebuildIndex()
            }
        }
        return success
    }

    func discardIndexChanges() {
        indexPaths = savedIndexPaths
        indexApps = savedIndexApps
        excludePatterns = savedExcludePatterns
        excludePaths = savedExcludePaths
        includeHidden = savedIncludeHidden
        excludeSensitive = savedExcludeSensitive
        networkRescanMinutes = savedNetworkRescanMinutes
    }

    func restoreIndexDefaults() {
        indexPaths = Self.defaultIndexPaths
        indexApps = false
        excludePatterns = Self.defaultExcludePatterns
        excludePaths = Self.defaultExcludePaths
        includeHidden = false
        excludeSensitive = true
        networkRescanMinutes = Self.defaultNetworkRescanMinutes
    }

    /// 开关打开时内核追加的系统应用索引根；此处仅用于提示与排除规则的冲突。
    static let appIndexRoots = ["/Applications", "/System/Applications", "/System/Cryptexes/App/System/Applications"]

    /// 开关已打开，但相关目录被排除路径或排除名单挡住，实际不会生效。
    var appIndexConflicted: Bool {
        guard indexApps else { return false }
        if excludePatterns.contains("Applications") { return true }
        return Self.appIndexRoots.contains { root in
            excludePaths.contains { raw in
                let path = raw.hasSuffix("/") ? String(raw.dropLast()) : raw
                return root == path || root.hasPrefix(path + "/")
            }
        }
    }

    /// 索引配置的出厂默认值，与 Rust 侧 IndexConfig::default 保持一致。
    /// 重置入口与“当前是否处于默认态”判定共用此定义，避免两处字面量各自漂移。
    static let defaultIndexPaths = ["~"]
    static let defaultNetworkRescanMinutes = 5
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
        !indexApps &&
        excludePatterns == Self.defaultExcludePatterns &&
        excludePaths == Self.defaultExcludePaths &&
        !includeHidden &&
        excludeSensitive &&
        networkRescanMinutes == Self.defaultNetworkRescanMinutes
    }
}
