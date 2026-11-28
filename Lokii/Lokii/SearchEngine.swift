import AppKit
import Combine
import Foundation

// MARK: - Config Model (mirrors Rust AppConfig JSON)

struct LokiiConfig: Codable {
    enum DockIconMode: String, Codable, CaseIterable, Hashable {
        case always
        case whenOpen = "when_open"
        case never

        init(ffi: FfiDockIconMode) {
            switch ffi {
            case .always: self = .always
            case .whenOpen: self = .whenOpen
            case .never: self = .never
            }
        }
    }

    var general: GeneralConfig
    var index: IndexConfig
    var window: WindowConfig

    /// 出厂默认配置，通过强类型 UniFFI Record 从 Rust `AppConfig::default()` 获取，确保编译期字段校验与单一来源。
    static let defaultConfig: LokiiConfig = {
        let raw = defaultAppConfig()
        return LokiiConfig(
            general: GeneralConfig(
                language: raw.general.language,
                dockIconMode: DockIconMode(ffi: raw.general.dockIconMode),
                terminalBundleId: raw.general.terminalBundleId,
                hideResultsWhenEmpty: raw.general.hideResultsWhenEmpty
            ),
            index: IndexConfig(
                indexPaths: raw.index.indexPaths,
                indexApps: raw.index.indexApps,
                exclude: raw.index.exclude,
                excludePaths: raw.index.excludePaths,
                includeHidden: raw.index.includeHidden,
                excludeSensitive: raw.index.excludeSensitive,
                sensitiveNames: raw.index.sensitiveNames,
                maxResults: Int(raw.index.maxResults),
                networkRescanEnabled: raw.index.networkRescanEnabled,
                networkRescanMinutes: Int(raw.index.networkRescanMinutes),
                serialScanPaths: raw.index.serialScanPaths
            ),
            window: WindowConfig(
                hideOnBlur: raw.window.hideOnBlur,
                rememberPosition: raw.window.rememberPosition
            )
        )
    }()

    struct GeneralConfig: Codable {
        var language: String
        var dockIconMode: DockIconMode
        var terminalBundleId: String
        var hideResultsWhenEmpty: Bool

        enum CodingKeys: String, CodingKey {
            case language
            case dockIconMode = "dock_icon_mode"
            case terminalBundleId = "terminal_bundle_id"
            case hideResultsWhenEmpty = "hide_results_when_empty"
        }

        private enum LegacyCodingKeys: String, CodingKey {
            case showInDock = "show_in_dock"
        }

        init(
            language: String? = nil,
            dockIconMode: DockIconMode? = nil,
            terminalBundleId: String? = nil,
            hideResultsWhenEmpty: Bool? = nil
        ) {
            if let language, let dockIconMode, let terminalBundleId, let hideResultsWhenEmpty {
                self.language = language
                self.dockIconMode = dockIconMode
                self.terminalBundleId = terminalBundleId
                self.hideResultsWhenEmpty = hideResultsWhenEmpty
            } else {
                let defaults = LokiiConfig.defaultConfig.general
                self.language = language ?? defaults.language
                self.dockIconMode = dockIconMode ?? defaults.dockIconMode
                self.terminalBundleId = terminalBundleId ?? defaults.terminalBundleId
                self.hideResultsWhenEmpty = hideResultsWhenEmpty ?? defaults.hideResultsWhenEmpty
            }
        }

        init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            let legacyContainer = try decoder.container(keyedBy: LegacyCodingKeys.self)
            let defaults = LokiiConfig.defaultConfig.general
            self.language = try container.decodeIfPresent(String.self, forKey: .language) ?? defaults.language
            if let explicitMode = try container.decodeIfPresent(DockIconMode.self, forKey: .dockIconMode) {
                self.dockIconMode = explicitMode
            } else if let legacyShowInDock = try legacyContainer.decodeIfPresent(Bool.self, forKey: .showInDock) {
                self.dockIconMode = legacyShowInDock ? .always : .never
            } else {
                self.dockIconMode = defaults.dockIconMode
            }
            self.terminalBundleId = try container.decodeIfPresent(String.self, forKey: .terminalBundleId) ?? defaults.terminalBundleId
            self.hideResultsWhenEmpty = try container.decodeIfPresent(Bool.self, forKey: .hideResultsWhenEmpty) ?? defaults.hideResultsWhenEmpty
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
        var networkRescanEnabled: Bool
        var networkRescanMinutes: Int
        var serialScanPaths: [String]

        enum CodingKeys: String, CodingKey {
            case indexPaths = "index_paths"
            case indexApps = "index_apps"
            case exclude
            case excludePaths = "exclude_paths"
            case includeHidden = "include_hidden"
            case excludeSensitive = "exclude_sensitive"
            case sensitiveNames = "sensitive_names"
            case maxResults = "max_results"
            case networkRescanEnabled = "network_rescan_enabled"
            case networkRescanMinutes = "network_rescan_minutes"
            case serialScanPaths = "serial_scan_paths"
        }

        static var defaultIndexPaths: [String] { LokiiConfig.defaultConfig.index.indexPaths }
        static var defaultExclude: [String] { LokiiConfig.defaultConfig.index.exclude }
        static var defaultExcludePaths: [String] { LokiiConfig.defaultConfig.index.excludePaths }
        static var defaultSensitiveNames: [String] { LokiiConfig.defaultConfig.index.sensitiveNames }
        static var defaultMaxResults: Int { LokiiConfig.defaultConfig.index.maxResults }
        static var defaultNetworkRescanEnabled: Bool { LokiiConfig.defaultConfig.index.networkRescanEnabled }
        static var defaultNetworkRescanMinutes: Int { LokiiConfig.defaultConfig.index.networkRescanMinutes }
        static var defaultSerialScanPaths: [String] { LokiiConfig.defaultConfig.index.serialScanPaths }

        init(
            indexPaths: [String]? = nil,
            indexApps: Bool? = nil,
            exclude: [String]? = nil,
            excludePaths: [String]? = nil,
            includeHidden: Bool? = nil,
            excludeSensitive: Bool? = nil,
            sensitiveNames: [String]? = nil,
            maxResults: Int? = nil,
            networkRescanEnabled: Bool? = nil,
            networkRescanMinutes: Int? = nil,
            serialScanPaths: [String]? = nil
        ) {
            if let indexPaths, let indexApps, let exclude, let excludePaths,
               let includeHidden, let excludeSensitive, let sensitiveNames,
               let maxResults, let networkRescanEnabled, let networkRescanMinutes, let serialScanPaths {
                self.indexPaths = indexPaths
                self.indexApps = indexApps
                self.exclude = exclude
                self.excludePaths = excludePaths
                self.includeHidden = includeHidden
                self.excludeSensitive = excludeSensitive
                self.sensitiveNames = sensitiveNames
                self.maxResults = maxResults
                self.networkRescanEnabled = networkRescanEnabled
                self.networkRescanMinutes = networkRescanMinutes
                self.serialScanPaths = serialScanPaths
            } else {
                let defaults = LokiiConfig.defaultConfig.index
                self.indexPaths = indexPaths ?? defaults.indexPaths
                self.indexApps = indexApps ?? defaults.indexApps
                self.exclude = exclude ?? defaults.exclude
                self.excludePaths = excludePaths ?? defaults.excludePaths
                self.includeHidden = includeHidden ?? defaults.includeHidden
                self.excludeSensitive = excludeSensitive ?? defaults.excludeSensitive
                self.sensitiveNames = sensitiveNames ?? defaults.sensitiveNames
                self.maxResults = maxResults ?? defaults.maxResults
                self.networkRescanEnabled = networkRescanEnabled ?? defaults.networkRescanEnabled
                self.networkRescanMinutes = networkRescanMinutes ?? defaults.networkRescanMinutes
                self.serialScanPaths = serialScanPaths ?? defaults.serialScanPaths
            }
        }

        init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            let defaults = LokiiConfig.defaultConfig.index
            self.indexPaths = try container.decodeIfPresent([String].self, forKey: .indexPaths) ?? defaults.indexPaths
            self.indexApps = try container.decodeIfPresent(Bool.self, forKey: .indexApps) ?? defaults.indexApps
            self.exclude = try container.decodeIfPresent([String].self, forKey: .exclude) ?? defaults.exclude
            self.excludePaths = try container.decodeIfPresent([String].self, forKey: .excludePaths) ?? defaults.excludePaths
            self.includeHidden = try container.decodeIfPresent(Bool.self, forKey: .includeHidden) ?? defaults.includeHidden
            self.excludeSensitive = try container.decodeIfPresent(Bool.self, forKey: .excludeSensitive) ?? defaults.excludeSensitive
            self.sensitiveNames = try container.decodeIfPresent([String].self, forKey: .sensitiveNames) ?? defaults.sensitiveNames
            self.maxResults = try container.decodeIfPresent(Int.self, forKey: .maxResults) ?? defaults.maxResults
            let rawNetworkRescanEnabled = try container.decodeIfPresent(Bool.self, forKey: .networkRescanEnabled)
            let rawNetworkRescanMinutes = try container.decodeIfPresent(Int.self, forKey: .networkRescanMinutes)
            if let rawNetworkRescanMinutes, rawNetworkRescanMinutes <= 0 {
                self.networkRescanEnabled = false
                self.networkRescanMinutes = defaults.networkRescanMinutes
            } else {
                self.networkRescanEnabled = rawNetworkRescanEnabled ?? defaults.networkRescanEnabled
                self.networkRescanMinutes = rawNetworkRescanMinutes ?? defaults.networkRescanMinutes
            }
            self.serialScanPaths = try container.decodeIfPresent([String].self, forKey: .serialScanPaths) ?? defaults.serialScanPaths
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
            hideOnBlur: Bool? = nil,
            rememberPosition: Bool? = nil
        ) {
            if let hideOnBlur, let rememberPosition {
                self.hideOnBlur = hideOnBlur
                self.rememberPosition = rememberPosition
            } else {
                let defaults = LokiiConfig.defaultConfig.window
                self.hideOnBlur = hideOnBlur ?? defaults.hideOnBlur
                self.rememberPosition = rememberPosition ?? defaults.rememberPosition
            }
        }

        init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            let defaults = LokiiConfig.defaultConfig.window
            self.hideOnBlur = try container.decodeIfPresent(Bool.self, forKey: .hideOnBlur) ?? defaults.hideOnBlur
            self.rememberPosition = try container.decodeIfPresent(Bool.self, forKey: .rememberPosition) ?? defaults.rememberPosition
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
    /// Posted on main thread when watcher or primary reconciliation fails. userInfo: ["message": String, "source": String]
    static let watcherErrorNotification = Notification.Name("LokiiWatcherError")
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
        let initHandler = NotificationEventHandler(source: "initialize") { [weak self] in
            self?.scheduleVolumeStatusesRefresh()
        }
        // watcher 负责配置宽化后的唯一全量重建与后台卷监控，需使用 source = "rebuild" 驱动进度与刷表，
        // 同时保留 onVolumeChanged 闭包以刷新顶部横幅与设置页卷行。
        let watcherHandler = Self.makeWatcherEventHandler { [weak self] in
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

            let mode = core.initialize(eventHandler: initHandler)
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
            core.startWatcher(eventHandler: watcherHandler)
            core.startConfigWatcher(eventHandler: initHandler)
            self?.refreshVolumeStatusesSync()
        }
    }

    static func makeWatcherEventHandler(
        onVolumeChanged: (@Sendable () -> Void)? = nil
    ) -> NotificationEventHandler {
        NotificationEventHandler(source: "rebuild", onVolumeChanged: onVolumeChanged)
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

    /// Confirm and start scanning a single volume waiting in `NeedsConfirmation`.
    func confirmVolumeScan(pathOrId: String, completion: ((FileActionOutcome) -> Void)? = nil) {
        volumeEventQueue.async { [weak self] in
            guard let self else { return }
            let outcome: FileActionOutcome
            do {
                let started = try self.core.confirmVolumeScan(pathOrId: pathOrId)
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

    /// Confirm and start scanning all currently mounted volumes waiting in `NeedsConfirmation`.
    func confirmAllPendingVolumeScans(completion: ((UInt32) -> Void)? = nil) {
        volumeEventQueue.async { [weak self] in
            guard let self else { return }
            let started = self.core.confirmAllPendingVolumeScans()
            self.refreshVolumeStatusesSync()
            if let completion {
                DispatchQueue.main.async { completion(started) }
            }
        }
    }

    /// Cancel an active scan for a volume while preserving its prior cached entries.
    func cancelVolumeScan(pathOrId: String, completion: ((FileActionOutcome) -> Void)? = nil) {
        volumeEventQueue.async { [weak self] in
            guard let self else { return }
            let outcome: FileActionOutcome
            do {
                let cancelled = try self.core.cancelVolumeScan(pathOrId: pathOrId)
                outcome = FileActionOutcome(
                    succeeded: cancelled,
                    errorMessage: cancelled ? nil : L("prefs.volume.actionFailed")
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
        includeOffline: Bool? = nil,
        sort: FfiSearchSort = .relevance
    ) -> FfiSearchResponse {
        let shouldIncludeOffline: Bool
        if let includeOffline {
            shouldIncludeOffline = includeOffline
        } else {
            statusLock.lock()
            shouldIncludeOffline = cachedIncludeOfflineResults
            statusLock.unlock()
        }
        return core.searchWithSort(
            query: query,
            mode: mode,
            caseSensitivity: caseSensitivity,
            includeOffline: shouldIncludeOffline,
            sort: sort
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

    /// Rebuild both the local primary index and all online mounted external/network volumes.
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

    /// Rebuild only the local primary index without triggering any external/network volume rescan.
    func rebuildPrimaryIndex() {
        let handler = NotificationEventHandler(source: "rebuild") { [weak self] in
            self?.scheduleVolumeStatusesRefresh()
        }
        DispatchQueue.global(qos: .userInitiated).async { [weak self, core] in
            let completed = core.rebuildPrimaryIndex(eventHandler: handler)
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

    /// Preview what primary index scope action a draft configuration would trigger via Rust scope_diff.
    func previewPrimaryScopeAction(_ draftConfig: LokiiConfig) -> FfiPrimaryScopeAction? {
        guard let data = try? JSONEncoder().encode(draftConfig),
              let json = String(data: data, encoding: .utf8) else { return nil }
        return try? core.previewPrimaryScopeAction(draftConfigJson: json)
    }

    /// Diagnose why a path is or is not indexed under the provided draft configuration.
    func diagnosePath(_ path: String, draftConfig: LokiiConfig) -> FfiPathDiagnosis? {
        guard let data = try? JSONEncoder().encode(draftConfig),
              let json = String(data: data, encoding: .utf8) else { return nil }
        return try? core.diagnosePath(path: path, draftConfigJson: json)
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

extension FfiVolumeStatus {
    /// 忽略高频跳动的条目计数与实时扫描进度，避免 150ms 节流进度触发重搜或 IOKit 挂载表轮询。
    func hasSameLifecycle(as other: Self) -> Bool {
        var status = self
        status.entryCount = other.entryCount
        status.scannedCount = other.scannedCount
        return status == other
    }
}

/// 协调主窗口底栏、全局菜单/快捷键与设置页三处手动「重建索引」入口的作用域决策与确认弹窗。
enum ManualRebuildCoordinator {
    /// 返回当前在线或待确认的外置卷/网络卷（全量重建时会连带重扫的对象）。
    static func activeExternalVolumes(in statuses: [FfiVolumeStatus]) -> [FfiVolumeStatus] {
        statuses.filter { $0.state == .online || $0.state == .needsConfirmation }
    }

    /// 仅当存在在线或待确认的外部卷时弹出双模式作用域确认框；否则静默仅重建本地索引。
    static func requiresScopeConfirmation(for statuses: [FfiVolumeStatus]) -> Bool {
        !activeExternalVolumes(in: statuses).isEmpty
    }

    static func requestRebuild(
        window: NSWindow?,
        statuses: [FfiVolumeStatus],
        onPrimaryOnly: @escaping () -> Void,
        onRebuildAll: @escaping () -> Void
    ) {
        let active = activeExternalVolumes(in: statuses)
        guard !active.isEmpty else {
            onPrimaryOnly()
            return
        }

        let names = active.map { status -> String in
            if !status.displayName.isEmpty { return status.displayName }
            return (status.configuredPath as NSString).lastPathComponent
        }.joined(separator: "、")

        let alert = NSAlert()
        alert.alertStyle = .informational
        alert.messageText = L("rebuild.scopeConfirm.title")
        alert.informativeText = String(format: L("rebuild.scopeConfirm.message"), names)
        alert.addButton(withTitle: L("rebuild.scopeConfirm.primaryOnly"))
        alert.addButton(withTitle: L("rebuild.scopeConfirm.all"))
        alert.addButton(withTitle: L("rebuild.scopeConfirm.cancel"))

        let handleResponse: (NSApplication.ModalResponse) -> Void = { response in
            switch response {
            case .alertFirstButtonReturn:
                onPrimaryOnly()
            case .alertSecondButtonReturn:
                onRebuildAll()
            default:
                break
            }
        }

        if let window, window.isVisible {
            alert.beginSheetModal(for: window, completionHandler: handleResponse)
        } else {
            NSApp.activate(ignoringOtherApps: true)
            handleResponse(alert.runModal())
        }
    }
}

/// 主窗口顶部非阻塞外置卷状态横幅的展示状态模型。
enum VolumeBannerState: Equatable {
    case hidden
    case syncing(volumes: [FfiVolumeStatus], totalScanned: UInt64, hasPendingConfirmations: Bool)
    case needsConfirmation(volumes: [FfiVolumeStatus])
}

/// 纯函数状态解析器：按「扫描中 > 待确认 > 隐藏」优先级与会话级收起集合推导横幅状态与展示文案。
enum VolumeBannerResolver {
    static func resolve(
        statuses: [FfiVolumeStatus],
        dismissedVolumeIds: Set<String>
    ) -> VolumeBannerState {
        let syncing = statuses.filter { $0.state == .syncing }
        let pending = statuses.filter { $0.state == .needsConfirmation }

        if !syncing.isEmpty {
            let totalScanned = syncing.reduce(UInt64(0)) { $0 &+ $1.scannedCount }
            return .syncing(
                volumes: syncing,
                totalScanned: totalScanned,
                hasPendingConfirmations: !pending.isEmpty
            )
        }

        if !pending.isEmpty {
            let hasUndismissed = pending.contains { !dismissedVolumeIds.contains($0.volumeId) }
            if hasUndismissed {
                return .needsConfirmation(volumes: pending)
            }
        }

        return .hidden
    }

    static func displayName(for status: FfiVolumeStatus) -> String {
        if !status.displayName.isEmpty {
            return status.displayName
        }
        return (status.configuredPath as NSString).lastPathComponent
    }

    static func message(
        for state: VolumeBannerState,
        localize: (String) -> String = { L($0) }
    ) -> String {
        switch state {
        case .hidden:
            return ""
        case let .syncing(volumes, totalScanned, _):
            guard let first = volumes.first else { return "" }
            let name = displayName(for: first)
            if volumes.count == 1 {
                if totalScanned > 0 {
                    return String(format: localize("volume.banner.syncing.single"), name, formatFileCount(totalScanned))
                }
                return String(format: localize("volume.banner.syncing.singleNoCount"), name)
            } else {
                if totalScanned > 0 {
                    return String(format: localize("volume.banner.syncing.multi"), name, volumes.count, formatFileCount(totalScanned))
                }
                return String(format: localize("volume.banner.syncing.multiNoCount"), name, volumes.count)
            }
        case let .needsConfirmation(volumes):
            guard let first = volumes.first else { return "" }
            let name = displayName(for: first)
            if volumes.count == 1 {
                switch first.confirmReason ?? .initialScan {
                case .initialScan:
                    return String(format: localize("volume.banner.confirm.initialScan"), name)
                case .filterWidened:
                    return String(format: localize("volume.banner.confirm.filterWidened"), name)
                case .journalInvalidated:
                    return String(format: localize("volume.banner.confirm.journalInvalidated"), name)
                case .schemaUpgrade:
                    return String(format: localize("volume.banner.confirm.schemaUpgrade"), name)
                }
            } else {
                return String(format: localize("volume.banner.confirm.multi"), name, volumes.count)
            }
        }
    }
}

// MARK: - Event Handler (Rust -> Swift callbacks)

final class NotificationEventHandler: EventHandler, @unchecked Sendable {
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
        DispatchQueue.main.async { [source] in
            NotificationCenter.default.post(
                name: SearchEngine.watcherErrorNotification,
                object: nil,
                userInfo: ["message": msg, "source": source]
            )
        }
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
