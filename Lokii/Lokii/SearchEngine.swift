import AppKit
import Combine
import Foundation

// MARK: - Config Model (mirrors Rust AppConfig JSON)

struct LokiiConfig: Codable {
    var general: GeneralConfig
    var index: IndexConfig
    var window: WindowConfig

    struct GeneralConfig: Codable {
        var language: String
        var showInDock: Bool
        var terminalBundleId: String
        var hideResultsWhenEmpty: Bool

        enum CodingKeys: String, CodingKey {
            case language
            case showInDock = "show_in_dock"
            case terminalBundleId = "terminal_bundle_id"
            case hideResultsWhenEmpty = "hide_results_when_empty"
        }

        init(
            language: String = "zh-Hans",
            showInDock: Bool = true,
            terminalBundleId: String = "com.apple.Terminal",
            hideResultsWhenEmpty: Bool = true
        ) {
            self.language = language
            self.showInDock = showInDock
            self.terminalBundleId = terminalBundleId
            self.hideResultsWhenEmpty = hideResultsWhenEmpty
        }

        init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            self.language = try container.decodeIfPresent(String.self, forKey: .language) ?? "zh-Hans"
            self.showInDock = try container.decodeIfPresent(Bool.self, forKey: .showInDock) ?? true
            self.terminalBundleId = try container.decodeIfPresent(String.self, forKey: .terminalBundleId) ?? "com.apple.Terminal"
            self.hideResultsWhenEmpty = try container.decodeIfPresent(Bool.self, forKey: .hideResultsWhenEmpty) ?? true
        }
    }

    struct IndexConfig: Codable {
        var indexPaths: [String]
        var indexApps: Bool
        var exclude: [String]
        var excludePaths: [String]
        var includeHidden: Bool
        var excludeSensitive: Bool
        var sensitiveNames: [String]
        var maxResults: Int
        var networkRescanMinutes: Int

        enum CodingKeys: String, CodingKey {
            case indexPaths = "index_paths"
            case indexApps = "index_apps"
            case exclude
            case excludePaths = "exclude_paths"
            case includeHidden = "include_hidden"
            case excludeSensitive = "exclude_sensitive"
            case sensitiveNames = "sensitive_names"
            case maxResults = "max_results"
            case networkRescanMinutes = "network_rescan_minutes"
        }

        static let defaultIndexPaths: [String] = ["~"]
        static let defaultExclude: [String] = [
            "node_modules", ".git", ".DS_Store", "target", ".Trash", ".hg", ".svn",
            "build", "dist", ".next", ".nuxt", ".cache", ".venv", "venv", "__pycache__",
            "DerivedData", "Pods", "Carthage", "Contents", "logs", "tmp", "temp",
            "coverage", ".idea", ".vscode", "vendor", "out", "bin", "obj",
            ".mypy_cache", ".pytest_cache", ".ruff_cache", ".tox", "xcshareddata", "xcuserdata",
        ]
        static let defaultExcludePaths: [String] = [
            "/System", "/Library", "/private", "/bin", "/sbin", "/usr", "/dev",
            "/Volumes", "/opt", "~/Library", "~/.Trash", "~/.cache", "~/.npm",
            "~/.pnpm-store", "~/.yarn", "~/.cargo/registry", "~/.cargo/git", "~/.rustup",
            "~/.gradle/caches", "~/.m2/repository", "~/.bun", "~/.deno", "~/.nvm",
            "~/.orbstack", "~/.docker",
        ]
        static let defaultSensitiveNames: [String] = [
            ".ssh", ".gnupg", ".aws", ".kube", ".docker", ".password-store",
            ".netrc", ".git-credentials", ".npmrc", ".pypirc", ".authinfo", ".msmtprc",
        ]

        init(
            indexPaths: [String] = IndexConfig.defaultIndexPaths,
            indexApps: Bool = false,
            exclude: [String] = IndexConfig.defaultExclude,
            excludePaths: [String] = IndexConfig.defaultExcludePaths,
            includeHidden: Bool = false,
            excludeSensitive: Bool = true,
            sensitiveNames: [String] = IndexConfig.defaultSensitiveNames,
            maxResults: Int = 500,
            networkRescanMinutes: Int = 30
        ) {
            self.indexPaths = indexPaths
            self.indexApps = indexApps
            self.exclude = exclude
            self.excludePaths = excludePaths
            self.includeHidden = includeHidden
            self.excludeSensitive = excludeSensitive
            self.sensitiveNames = sensitiveNames
            self.maxResults = maxResults
            self.networkRescanMinutes = networkRescanMinutes
        }

        init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            self.indexPaths = try container.decodeIfPresent([String].self, forKey: .indexPaths) ?? IndexConfig.defaultIndexPaths
            self.indexApps = try container.decodeIfPresent(Bool.self, forKey: .indexApps) ?? false
            self.exclude = try container.decodeIfPresent([String].self, forKey: .exclude) ?? IndexConfig.defaultExclude
            self.excludePaths = try container.decodeIfPresent([String].self, forKey: .excludePaths) ?? IndexConfig.defaultExcludePaths
            self.includeHidden = try container.decodeIfPresent(Bool.self, forKey: .includeHidden) ?? false
            self.excludeSensitive = try container.decodeIfPresent(Bool.self, forKey: .excludeSensitive) ?? true
            self.sensitiveNames = try container.decodeIfPresent([String].self, forKey: .sensitiveNames) ?? IndexConfig.defaultSensitiveNames
            self.maxResults = try container.decodeIfPresent(Int.self, forKey: .maxResults) ?? 500
            self.networkRescanMinutes = try container.decodeIfPresent(Int.self, forKey: .networkRescanMinutes) ?? 30
        }
    }

    struct WindowConfig: Codable {
        var hideOnBlur: Bool
        var rememberPosition: Bool

        enum CodingKeys: String, CodingKey {
            case hideOnBlur = "hide_on_blur"
            case rememberPosition = "remember_position"
        }

        init(
            hideOnBlur: Bool = true,
            rememberPosition: Bool = false
        ) {
            self.hideOnBlur = hideOnBlur
            self.rememberPosition = rememberPosition
        }

        init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            self.hideOnBlur = try container.decodeIfPresent(Bool.self, forKey: .hideOnBlur) ?? true
            self.rememberPosition = try container.decodeIfPresent(Bool.self, forKey: .rememberPosition) ?? false
        }
    }
}

/// Result of attempting a filesystem action (open / reveal) through the core engine.
struct FileActionOutcome {
    let succeeded: Bool
    let errorMessage: String?
}

/// Wraps LokiiCore (Rust via UniFFI) with a Swift-friendly async interface.
final class SearchEngine: ObservableObject, @unchecked Sendable {
    private let core: LokiiCore
    private let queue = DispatchQueue(label: "com.lokii.engine", qos: .userInitiated)
    private let volumeEventQueue = DispatchQueue(label: "com.lokii.volumeEvents", qos: .userInitiated)
    private let statusRefreshQueue = DispatchQueue(label: "com.lokii.engine.volumeStatus", qos: .userInitiated)
    private let statusLock = NSLock()
    private var cachedVolumeStatuses: [FfiVolumeStatus] = []
    /// 缓存是否存在离线或缺失卷槽位，所有卷均在线或未配置外部卷时可让 isPathOffline O(1) 零分配返回
    private var cachedHasOfflineStatus = false
    /// statusLock 保护的离线结果开关镜像，供后台检索线程安全读取，避免跨线程读写 @Published 产生数据竞争
    private var cachedIncludeOfflineResults = true
    /// 合并高频卷状态刷新时记录是否出现过生命周期变化，避免亚秒级 .online -> .syncing -> .online 漏发通知
    private var pendingLifecycleNotification = false
    private var statusSequence: UInt64 = 0
    private var workspaceObservers: [NSObjectProtocol] = []

    private static let includeOfflineDefaultsKey = "lokii.includeOfflineResults"

    /// Posted on main thread when index progress updates.
    static let indexProgressNotification = Notification.Name("LokiiIndexProgress")
    /// Posted on main thread when index is ready. userInfo: ["total": UInt64]
    static let indexReadyNotification = Notification.Name("LokiiIndexReady")
    /// Posted on main thread when index is updated. userInfo: ["total": UInt64]
    static let indexUpdatedNotification = Notification.Name("LokiiIndexUpdated")
    /// Posted on main thread after initialize() determines startup mode. userInfo: ["isColdStart": Bool]
    static let startupModeNotification = Notification.Name("LokiiStartupMode")
    static let initializationFailedNotification = Notification.Name("LokiiInitializationFailed")
    /// Posted on main thread when config.toml is reloaded.
    static let configReloadedNotification = Notification.Name("LokiiConfigReloaded")
    /// Posted on main thread when mounted/offline volume statuses change.
    static let volumeStatusesChangedNotification = Notification.Name("LokiiVolumeStatusesChanged")

    /// Latest volume statuses snapshot published on the main queue for SwiftUI views.
    @Published private(set) var volumeStatuses: [FfiVolumeStatus] = []

    /// Whether searches should include indexed entries from disconnected volumes.
    @Published var includeOfflineResults: Bool {
        didSet {
            guard oldValue != includeOfflineResults else { return }
            statusLock.lock()
            cachedIncludeOfflineResults = includeOfflineResults
            statusLock.unlock()
            UserDefaults.standard.set(includeOfflineResults, forKey: Self.includeOfflineDefaultsKey)
            // 通知主搜索窗口立即按新偏好重刷结果列表
            if Thread.isMainThread {
                NotificationCenter.default.post(name: Self.volumeStatusesChangedNotification, object: nil)
            } else {
                DispatchQueue.main.async {
                    NotificationCenter.default.post(name: Self.volumeStatusesChangedNotification, object: nil)
                }
            }
        }
    }

    init() {
        core = LokiiCore()
        let initialIncludeOffline: Bool
        if let stored = UserDefaults.standard.object(forKey: Self.includeOfflineDefaultsKey) as? Bool {
            initialIncludeOffline = stored
        } else {
            initialIncludeOffline = true
        }
        includeOfflineResults = initialIncludeOffline
        cachedIncludeOfflineResults = initialIncludeOffline
    }

    deinit {
        let workspaceCenter = NSWorkspace.shared.notificationCenter
        for observer in workspaceObservers {
            workspaceCenter.removeObserver(observer)
        }
        workspaceObservers.removeAll()
    }

    /// Initialize the index on a background thread. Fires notifications for progress.
    ///
    /// Notification ordering: `startupModeNotification` is dispatched to the main
    /// thread **before** `core.initialize()` begins.  Because `initialize()` is a
    /// blocking call, the Rust-side `on_index_ready` callback dispatches its own
    /// `indexReadyNotification` to the main queue *during* that call.  By posting
    /// the startup-mode notification first, the main-queue ordering is guaranteed:
    ///   1. startupModeNotification  ->  UI decides cold-start vs hot-start
    ///   2. indexReadyNotification   ->  UI dismisses the correct indicator
    func initialize() {
        startWorkspaceMonitoring()
        let handler = NotificationEventHandler { [weak self] in
            self?.scheduleVolumeStatusesRefresh()
        }
        queue.async { [weak self, core] in
            // Bind the CLI socket before the blocking initialize(): during a cold-start scan the
            // CLI then gets empty results instead of "cannot connect", and a failed init no longer
            // leaves the socket missing forever.
            core.startQueryServer()

            // Query startup mode via FFI before the blocking initialize() call so cold-start
            // covers both missing primary cache and uninitialized configured volume caches.
            let isColdStart = core.peekStartupIsColdStart()
            DispatchQueue.main.async {
                NotificationCenter.default.post(
                    name: SearchEngine.startupModeNotification,
                    object: nil,
                    userInfo: ["isColdStart": isColdStart]
                )
            }

            let mode = core.initialize(eventHandler: handler)
            switch mode {
            case let .hotStart(cachedCount):
                print("[Lokii] Hot start: \(cachedCount) cached entries")
            case let .coldStart(scannedCount):
                print("[Lokii] Cold start: scanned \(scannedCount) entries")
            case let .failed(message):
                DispatchQueue.main.async {
                    NotificationCenter.default.post(
                        name: SearchEngine.initializationFailedNotification,
                        object: nil,
                        userInfo: ["message": message]
                    )
                }
                return
            }
            core.startWatcher(eventHandler: handler)
            core.startConfigWatcher(eventHandler: handler)
            self?.refreshVolumeStatusesSync()
        }
    }

    /// Register NSWorkspace mount/unmount notifications and forward them to the Rust engine.
    private func startWorkspaceMonitoring() {
        guard workspaceObservers.isEmpty else { return }
        let workspaceCenter = NSWorkspace.shared.notificationCenter

        let didMount = workspaceCenter.addObserver(
            forName: NSWorkspace.didMountNotification,
            object: nil,
            queue: nil
        ) { [weak self] note in
            guard let path = (note.userInfo?[NSWorkspace.volumeURLUserInfoKey] as? URL)?.path else { return }
            self?.volumeEventQueue.async { [weak self] in
                guard let self else { return }
                self.core.notifyVolumeMounted(mountPath: path)
                self.refreshVolumeStatusesSync()
            }
        }

        let willUnmount = workspaceCenter.addObserver(
            forName: NSWorkspace.willUnmountNotification,
            object: nil,
            queue: nil
        ) { [weak self] note in
            guard let path = (note.userInfo?[NSWorkspace.volumeURLUserInfoKey] as? URL)?.path else { return }
            self?.volumeEventQueue.async { [weak self] in
                guard let self else { return }
                self.core.notifyVolumeWillUnmount(mountPath: path)
                self.refreshVolumeStatusesSync()
            }
        }

        let didUnmount = workspaceCenter.addObserver(
            forName: NSWorkspace.didUnmountNotification,
            object: nil,
            queue: nil
        ) { [weak self] note in
            guard let path = (note.userInfo?[NSWorkspace.volumeURLUserInfoKey] as? URL)?.path else { return }
            self?.volumeEventQueue.async { [weak self] in
                guard let self else { return }
                self.core.notifyVolumeDidUnmount(mountPath: path)
                self.refreshVolumeStatusesSync()
            }
        }

        workspaceObservers = [didMount, willUnmount, didUnmount]
    }

    /// Schedule an off-main-thread refresh of the cached volume status snapshot.
    func scheduleVolumeStatusesRefresh() {
        statusRefreshQueue.async { [weak self] in
            self?.refreshVolumeStatusesOnSerialQueue()
        }
    }

    private func refreshVolumeStatusesSync() {
        statusRefreshQueue.sync { [weak self] in
            self?.refreshVolumeStatusesOnSerialQueue()
        }
    }

    private func refreshVolumeStatusesOnSerialQueue() {
        let latest = core.getVolumeStatuses()
        statusLock.lock()
        guard statusSequence == 0 || latest != cachedVolumeStatuses else {
            statusLock.unlock()
            return
        }
        let stepLifecycleChanged = statusSequence == 0 || !cachedVolumeStatuses.elementsEqual(latest) {
            $0.hasSameLifecycle(as: $1)
        }
        if stepLifecycleChanged {
            pendingLifecycleNotification = true
        }
        statusSequence &+= 1
        let seq = statusSequence
        cachedVolumeStatuses = latest
        cachedHasOfflineStatus = latest.contains { $0.state == .offline || $0.state == .missing }
        statusLock.unlock()

        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            self.statusLock.lock()
            let isLatest = (seq == self.statusSequence)
            let shouldPostNotification = isLatest && self.pendingLifecycleNotification
            if isLatest {
                self.pendingLifecycleNotification = false
            }
            self.statusLock.unlock()
            guard isLatest else { return }
            let changed = latest != self.volumeStatuses
            if changed {
                self.volumeStatuses = latest
            }
            if shouldPostNotification {
                NotificationCenter.default.post(
                    name: SearchEngine.volumeStatusesChangedNotification,
                    object: self
                )
            }
        }
    }

    /// Thread-safe snapshot of the last known volume statuses (strictly in-memory, zero FFI/disk I/O on caller thread).
    func getVolumeStatuses() -> [FfiVolumeStatus] {
        statusLock.lock()
        let copy = cachedVolumeStatuses
        let needsRefresh = (statusSequence == 0)
        statusLock.unlock()
        if needsRefresh {
            scheduleVolumeStatusesRefresh()
        }
        return copy
    }

    /// Return the most specific configured/active volume status matching `path`, without touching disk.
    func volumeStatus(forPath path: String) -> FfiVolumeStatus? {
        Self.matchVolumeStatus(forPath: path, in: getVolumeStatuses())
    }

    static func matchVolumeStatus(forPath path: String, in statuses: [FfiVolumeStatus]) -> FfiVolumeStatus? {
        SettingsPathStatusResolver.matchVolumeStatus(forPath: path, in: statuses)
    }

    /// Return true when `path` belongs to a disconnected or missing volume slot (zero FFI/disk I/O on caller thread).
    func isPathOffline(_ path: String) -> Bool {
        statusLock.lock()
        let needsRefresh = (statusSequence == 0)
        let hasOffline = cachedHasOfflineStatus
        let statuses = hasOffline ? cachedVolumeStatuses : []
        statusLock.unlock()

        if needsRefresh {
            scheduleVolumeStatusesRefresh()
        }
        guard hasOffline,
              let status = Self.matchVolumeStatus(forPath: path, in: statuses) else {
            return false
        }
        return status.state == .offline || status.state == .missing
    }

    /// Trigger an asynchronous rescan for a volume identified by configured path or volumeId.
    func rescanVolume(pathOrId: String, completion: ((FileActionOutcome) -> Void)? = nil) {
        volumeEventQueue.async { [weak self] in
            guard let self else { return }
            let outcome: FileActionOutcome
            do {
                let started = try self.core.rescanVolume(pathOrId: pathOrId)
                outcome = FileActionOutcome(
                    succeeded: started,
                    errorMessage: started ? nil : L("prefs.volume.actionFailed")
                )
            } catch let FfiError.IoError(message) {
                outcome = FileActionOutcome(succeeded: false, errorMessage: message)
            } catch {
                outcome = FileActionOutcome(succeeded: false, errorMessage: error.localizedDescription)
            }
            self.refreshVolumeStatusesSync()
            if let completion {
                DispatchQueue.main.async { completion(outcome) }
            }
        }
    }

    /// Clear cached index entries for an offline or unmounted volume slot.
    func clearVolumeCache(pathOrId: String, completion: ((FileActionOutcome) -> Void)? = nil) {
        volumeEventQueue.async { [weak self] in
            guard let self else { return }
            let outcome: FileActionOutcome
            do {
                let cleared = try self.core.clearVolumeCache(pathOrId: pathOrId)
                outcome = FileActionOutcome(
                    succeeded: cleared,
                    errorMessage: cleared ? nil : L("prefs.volume.actionFailed")
                )
            } catch let FfiError.IoError(message) {
                outcome = FileActionOutcome(succeeded: false, errorMessage: message)
            } catch {
                outcome = FileActionOutcome(succeeded: false, errorMessage: error.localizedDescription)
            }
            self.refreshVolumeStatusesSync()
            if let completion {
                DispatchQueue.main.async { completion(outcome) }
            }
        }
    }

    /// Search synchronously (fast — sub-millisecond for in-memory index).
    func search(
        query: String,
        mode: FfiSearchMode = .auto,
        caseSensitivity: FfiCaseSensitivity = .auto,
        includeOffline: Bool? = nil
    ) -> FfiSearchResponse {
        let shouldIncludeOffline: Bool
        if let includeOffline {
            shouldIncludeOffline = includeOffline
        } else {
            statusLock.lock()
            shouldIncludeOffline = cachedIncludeOfflineResults
            statusLock.unlock()
        }
        return core.searchWithOptions(
            query: query,
            mode: mode,
            caseSensitivity: caseSensitivity,
            includeOffline: shouldIncludeOffline
        )
    }

    /// Get current index stats.
    var stats: FfiIndexStats {
        core.getStats()
    }

    /// Check if a background scan is currently running.
    var isScanning: Bool {
        core.isScanning()
    }

    /// Rebuild index from scratch on background thread.
    func rebuildIndex() {
        let handler = NotificationEventHandler(source: "rebuild") { [weak self] in
            self?.scheduleVolumeStatusesRefresh()
        }
        DispatchQueue.global(qos: .userInitiated).async { [weak self, core] in
            let completed = core.rebuildIndex(eventHandler: handler)
            // 若后端因并发扫描冲突或扫描失败提前返回 false（不会触发 on_index_ready），
            // 主动广播重建结束通知，防止主视窗与偏好设置页永久卡在 rebuilding 状态。
            if !completed {
                let total = core.getStats().totalCount
                DispatchQueue.main.async {
                    NotificationCenter.default.post(
                        name: SearchEngine.indexReadyNotification,
                        object: nil,
                        userInfo: ["total": total, "source": "rebuild"]
                    )
                }
            }
            self?.scheduleVolumeStatusesRefresh()
        }
    }

    /// Save the index with its replay checkpoint (blocking; call on quit).
    func persistIndex() {
        core.persistIndex()
    }

    /// Check Full Disk Access permission.
    var hasFullDiskAccess: Bool {
        core.checkFda()
    }

    /// Open file off the main thread so hung network mounts or `open` subprocess execution never freeze UI.
    func openFileAsync(path: String, completion: ((FileActionOutcome) -> Void)? = nil) {
        volumeEventQueue.async { [weak self] in
            guard let self else { return }
            let outcome = self.openFile(path: path)
            if let completion {
                DispatchQueue.main.async { completion(outcome) }
            }
        }
    }

    /// Open file with default app, surfacing any volume offline or I/O error message from Rust.
    @discardableResult
    func openFile(path: String) -> FileActionOutcome {
        do {
            let opened = try core.openFile(path: path)
            return FileActionOutcome(
                succeeded: opened,
                errorMessage: opened ? nil : L("search.offline.actionBlocked")
            )
        } catch let FfiError.IoError(message) {
            return FileActionOutcome(succeeded: false, errorMessage: message)
        } catch {
            return FileActionOutcome(succeeded: false, errorMessage: error.localizedDescription)
        }
    }

    /// Reveal file in Finder, surfacing any volume offline or I/O error message from Rust.
    @discardableResult
    func revealInFinder(path: String) -> FileActionOutcome {
        do {
            let revealed = try core.revealInFinder(path: path)
            return FileActionOutcome(
                succeeded: revealed,
                errorMessage: revealed ? nil : L("search.offline.actionBlocked")
            )
        } catch let FfiError.IoError(message) {
            return FileActionOutcome(succeeded: false, errorMessage: message)
        } catch {
            return FileActionOutcome(succeeded: false, errorMessage: error.localizedDescription)
        }
    }

    /// Read current config as a decoded Swift struct.
    func getConfig() -> LokiiConfig? {
        let json = core.getConfigJson()
        guard let data = json.data(using: .utf8) else { return nil }
        return try? JSONDecoder().decode(LokiiConfig.self, from: data)
    }

    /// Encode and write config back to disk via FFI.
    func updateConfig(_ config: LokiiConfig) -> Bool {
        guard let data = try? JSONEncoder().encode(config),
              let json = String(data: data, encoding: .utf8) else { return false }
        let success = core.updateConfigFromJson(json: json)
        if success {
            scheduleVolumeStatusesRefresh()
            DispatchQueue.main.async {
                NotificationCenter.default.post(name: Self.configReloadedNotification, object: nil)
            }
        }
        return success
    }
}

private extension FfiVolumeStatus {
    func hasSameLifecycle(as other: Self) -> Bool {
        var status = self
        status.entryCount = other.entryCount
        return status == other
    }
}

// MARK: - Event Handler (Rust -> Swift callbacks)

private final class NotificationEventHandler: EventHandler, @unchecked Sendable {
    /// Source tag included in notifications so the UI can distinguish
    /// events from initialize vs rebuild.
    let source: String
    private let volumeChangedCallback: (@Sendable () -> Void)?

    init(source: String = "initialize", onVolumeChanged: (@Sendable () -> Void)? = nil) {
        self.source = source
        self.volumeChangedCallback = onVolumeChanged
    }

    func onIndexProgress(phase: String, scannedFiles: UInt64) {
        DispatchQueue.main.async { [source] in
            NotificationCenter.default.post(
                name: SearchEngine.indexProgressNotification,
                object: nil,
                userInfo: ["phase": phase, "scannedFiles": scannedFiles, "source": source]
            )
        }
    }

    func onIndexReady(total: UInt64) {
        volumeChangedCallback?()
        DispatchQueue.main.async { [source] in
            NotificationCenter.default.post(
                name: SearchEngine.indexReadyNotification,
                object: nil,
                userInfo: ["total": total, "source": source]
            )
        }
    }

    func onIndexUpdated(newTotal: UInt64) {
        DispatchQueue.main.async { [source] in
            NotificationCenter.default.post(
                name: SearchEngine.indexUpdatedNotification,
                object: nil,
                userInfo: ["total": newTotal, "source": source]
            )
        }
    }

    func onWatcherError(msg: String) {
        print("[Lokii] Watcher error: \(msg)")
    }

    func onConfigReloaded() {
        volumeChangedCallback?()
        DispatchQueue.main.async {
            NotificationCenter.default.post(name: SearchEngine.configReloadedNotification, object: nil)
        }
        print("[Lokii] Config reloaded")
    }

    func onVolumeStatusesChanged() {
        volumeChangedCallback?()
    }
}
