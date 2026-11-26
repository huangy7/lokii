import SwiftUI
import AppKit
import Combine
import DiskArbitration
import IOKit
import ServiceManagement

// MARK: - CLI Tool Install State
enum CliInstallState: Equatable {
    case notInstalled
    case installing
    case installed
    case failed(message: String, detail: String)
}

// MARK: - Path & Volume Availability State

/// Resolved display status for a configured index or exclude path.
enum SettingsPathAvailabilityState: Equatable {
    case volumeOnline(
        entryCount: UInt64,
        activePath: String?,
        isRemounted: Bool,
        kind: String,
        supportsPersistentJournal: Bool = true
    )
    case volumeSyncing(kind: String, scannedCount: UInt64 = 0)
    case volumeNeedsConfirmation(
        entryCount: UInt64,
        activePath: String?,
        isRemounted: Bool,
        kind: String,
        confirmReason: FfiVolumeConfirmReason?
    )
    case volumeOffline(entryCount: UInt64, kind: String)
    case volumeUncachedOffline
    case localAvailable
    case localChecking
    case localMissing
}

/// Pure status resolver mapping a path against cached volume statuses and background local probes.
enum SettingsPathStatusResolver {
    static func normalizePath(_ path: String) -> String {
        let expanded = path.hasPrefix("~") ? (path as NSString).expandingTildeInPath : path
        guard expanded.count > 1 else { return expanded }
        var trimmed = expanded
        while trimmed.count > 1 && trimmed.hasSuffix("/") {
            trimmed.removeLast()
        }
        return trimmed
    }

    /// 规范化后大小写无关地判断 `paths` 中是否有与 `path` 相同的路径（祖先/子孙不算）。
    static func containsSamePath(_ paths: [String], _ path: String) -> Bool {
        let key = normalizePath(path).lowercased()
        return paths.contains { normalizePath($0).lowercased() == key }
    }

    static func isUnderVolumesNamespace(_ path: String) -> Bool {
        let normalized = normalizePath(path)
        let prefix = "/volumes/"
        guard normalized.lowercased().hasPrefix(prefix) else { return false }
        let remainder = normalized.dropFirst(prefix.count)
        return !remainder.isEmpty && !remainder.allSatisfy { $0 == "/" }
    }

    static func resolve(
        path: String,
        volumeStatuses: [FfiVolumeStatus],
        localAvailabilityCache: [String: Bool]
    ) -> SettingsPathAvailabilityState {
        let normalizedTarget = normalizePath(path)

        if let matched = matchVolumeStatus(normalizedTarget: normalizedTarget, volumeStatuses: volumeStatuses) {
            switch matched.state {
            case .online:
                return .volumeOnline(
                    entryCount: matched.entryCount,
                    activePath: matched.activePath,
                    isRemounted: matched.isRemounted,
                    kind: matched.kind,
                    supportsPersistentJournal: matched.supportsPersistentJournal
                )
            case .syncing:
                return .volumeSyncing(kind: matched.kind, scannedCount: matched.scannedCount)
            case .needsConfirmation:
                return .volumeNeedsConfirmation(
                    entryCount: matched.entryCount,
                    activePath: matched.activePath,
                    isRemounted: matched.isRemounted,
                    kind: matched.kind,
                    confirmReason: matched.confirmReason
                )
            case .offline:
                // 仅当内存与磁盘缓存确有条目时才视作“索引已保留”；PendingPath 或缓存已删除（entryCount == 0）归类为未缓存离线
                if matched.entryCount == 0 {
                    return .volumeUncachedOffline
                }
                return .volumeOffline(entryCount: matched.entryCount, kind: matched.kind)
            case .missing:
                if let activePath = matched.activePath, !activePath.isEmpty {
                    return .localMissing
                }
                return isUnderVolumesNamespace(normalizedTarget) ? .volumeUncachedOffline : .localMissing
            }
        }

        if isUnderVolumesNamespace(normalizedTarget) {
            return .volumeUncachedOffline
        }

        if let cached = localAvailabilityCache[normalizedTarget] ?? localAvailabilityCache[path] {
            return cached ? .localAvailable : .localMissing
        }
        return .localChecking
    }

    static func matchVolumeStatus(
        forPath path: String,
        in volumeStatuses: [FfiVolumeStatus]
    ) -> FfiVolumeStatus? {
        guard !volumeStatuses.isEmpty else { return nil }
        return matchVolumeStatus(
            normalizedTarget: normalizePath(path),
            volumeStatuses: volumeStatuses
        )
    }

    static func matchVolumeStatus(
        normalizedTarget: String,
        volumeStatuses: [FfiVolumeStatus]
    ) -> FfiVolumeStatus? {
        var bestMatch: (length: Int, status: FfiVolumeStatus)?
        for status in volumeStatuses {
            var roots = [normalizePath(status.configuredPath)]
            if let activePath = status.activePath, !activePath.isEmpty {
                roots.append(normalizePath(activePath))
            }
            for root in roots where !root.isEmpty {
                if isPath(normalizedTarget, insideOrEqual: root) {
                    if bestMatch == nil || root.count > bestMatch!.length {
                        bestMatch = (root.count, status)
                    }
                }
            }
        }
        return bestMatch?.status
    }

    /// 预归一化卷状态匹配器：在高频批处理循环外一次性完成卷路径规范化与小写化，消除逐条结果重复展开开销
    struct VolumeStatusMatcher {
        private struct Entry {
            let rootLower: String
            let rootWithSlashLower: String
            let length: Int
            let status: FfiVolumeStatus
        }

        private let entries: [Entry]

        init(volumeStatuses: [FfiVolumeStatus]) {
            var built: [Entry] = []
            built.reserveCapacity(volumeStatuses.count * 2)
            for status in volumeStatuses {
                var roots = [SettingsPathStatusResolver.normalizePath(status.configuredPath)]
                if let activePath = status.activePath, !activePath.isEmpty {
                    roots.append(SettingsPathStatusResolver.normalizePath(activePath))
                }
                for root in roots where !root.isEmpty {
                    let lower = root.lowercased()
                    let withSlash = lower == "/" ? "/" : lower + "/"
                    built.append(Entry(
                        rootLower: lower,
                        rootWithSlashLower: withSlash,
                        length: root.count,
                        status: status
                    ))
                }
            }
            self.entries = built
        }

        var isEmpty: Bool { entries.isEmpty }

        func match(path: String) -> FfiVolumeStatus? {
            guard !entries.isEmpty else { return nil }
            let normalized = SettingsPathStatusResolver.normalizePath(path)
            let targetLower = normalized.lowercased()
            var bestMatch: (length: Int, status: FfiVolumeStatus)?
            for entry in entries {
                let matched: Bool
                if entry.rootLower == "/" {
                    matched = targetLower.hasPrefix("/")
                } else {
                    matched = targetLower == entry.rootLower || targetLower.hasPrefix(entry.rootWithSlashLower)
                }
                if matched, bestMatch == nil || entry.length > bestMatch!.length {
                    bestMatch = (entry.length, entry.status)
                }
            }
            return bestMatch?.status
        }
    }

    static func isPath(_ target: String, insideOrEqual root: String) -> Bool {
        if root == "/" {
            return target.hasPrefix("/")
        }
        if target.caseInsensitiveCompare(root) == .orderedSame {
            return true
        }
        return target.lowercased().hasPrefix(root.lowercased() + "/")
    }

    /// 判定路径是否为 `/Volumes/<卷名>` 形式的整卷根挂载点（不含更深层子目录）
    static func isVolumeRootPath(_ path: String) -> Bool {
        let normalized = normalizePath(path)
        let components = (normalized as NSString).pathComponents
        return components.count == 3
            && components[0] == "/"
            && components[1].caseInsensitiveCompare("Volumes") == .orderedSame
            && !components[2].isEmpty
    }

    /// 根据内核 IOKit 驱动祖先链与 DiskArbitration 设备元数据，精确判定挂载卷是否为磁盘映像（DMG / ISO / SparseImage）。
    ///
    /// 判定优先级：
    /// 1. 祖先链命中磁盘映像内核驱动（DiskImages2 的 AppleDiskImageDevice/AppleDiskImagesController
    ///    或传统 IOHDIXHDDrive/IOHDIXController）直接判为 true；
    /// 2. 祖先链命中真实物理总线（USB / Thunderbolt / NVMe / SATA / SCSI / SD / FireWire）直接判为 false，
    ///    杜绝外接 APFS 硬盘盒固件误报 Virtual Interface 时被误杀；
    /// 3. 仅在 IOKit 祖先链为空时保守回退至 DiskArbitration 显式声明的 `Disk Image` 型号或协议。
    static func isDiskImageVolume(
        ancestorIOKitClasses: [String],
        deviceModel: String?,
        deviceProtocol: String?
    ) -> Bool {
        let diskImageClassMarkers = [
            "AppleDiskImageDevice",
            "AppleDiskImagesController",
            "IOHDIXHDDrive",
            "IOHDIXController",
        ]
        if ancestorIOKitClasses.contains(where: { cls in
            diskImageClassMarkers.contains { cls == $0 || cls.hasPrefix($0) }
        }) {
            return true
        }

        let physicalBusClassMarkers = [
            "IOUSB",
            "AppleUSB",
            "IOThunderbolt",
            "AppleThunderbolt",
            "IONVMe",
            "IOEmbeddedNVMe",
            "IOAHCI",
            "IOSCSI",
            "AppleSDXC",
            "IOSDHost",
            "IOFireWire",
        ]
        if ancestorIOKitClasses.contains(where: { cls in
            physicalBusClassMarkers.contains { cls.hasPrefix($0) }
        }) {
            return false
        }

        let model = deviceModel?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() ?? ""
        let proto = deviceProtocol?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() ?? ""
        return model == "disk image" || proto == "disk image"
    }
}

/// 后台从内核挂载表探测到的当前在线外部卷或网络卷摘要
struct MountedVolumeEntry: Equatable {
    let path: String
    let displayName: String
    let kind: String
    let isDiskImage: Bool

    init(path: String, displayName: String, kind: String, isDiskImage: Bool = false) {
        self.path = path
        self.displayName = displayName
        self.kind = kind
        self.isDiskImage = isDiskImage
    }
}

/// 设置页「外置磁盘与网络卷」分组的统一展示条目（融合在线挂载卷、后端卷状态与用户草稿配置）
struct ManagedVolumeItem: Identifiable, Equatable {
    let id: String
    let configuredPath: String
    let activePath: String?
    let displayName: String
    let kind: String
    let isIndexed: Bool
    let isSavedIndexed: Bool
    let isMounted: Bool
    let status: FfiVolumeStatus?
}

// MARK: - Preferences ViewModel
class PreferencesViewModel: ObservableObject {
    let engine: SearchEngine

    @Published var showInDock: Bool = true {
        didSet {
            saveGeneralConfig()
        }
    }

    @Published var hideOnBlur: Bool = true {
        didSet {
            saveGeneralConfig()
        }
    }

    @Published var rememberPosition: Bool = false {
        didSet {
            saveGeneralConfig()
        }
    }

    @Published var hasCustomWindowSizes: Bool = WindowFrameManager().hasCustomSizes
    private var windowSizeObserver: NSObjectProtocol?

    func refreshWindowSizeState() {
        hasCustomWindowSizes = WindowFrameManager().hasCustomSizes
    }

    func resetWindowSizes() {
        NotificationCenter.default.post(name: .lokiiResetWindowSize, object: nil)
        refreshWindowSizeState()
    }

    @Published var maxResults: Int = 500 {
        didSet {
            saveGeneralConfig()
        }
    }

    @Published var hideResultsWhenEmpty: Bool = true {
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
    @Published var indexPaths: [String] = LokiiConfig.IndexConfig.defaultIndexPaths {
        didSet {
            guard !isLoading else { return }
            refreshLocalPathChecks()
            recomputePendingSaveRequiresRebuild()
        }
    }
    @Published var indexApps: Bool = false {
        didSet {
            guard !isLoading else { return }
            recomputePendingSaveRequiresRebuild()
        }
    }
    @Published var excludePatterns: [String] = LokiiConfig.IndexConfig.defaultExclude {
        didSet {
            guard !isLoading else { return }
            recomputePendingSaveRequiresRebuild()
        }
    }
    @Published var excludePaths: [String] = LokiiConfig.IndexConfig.defaultExcludePaths {
        didSet {
            guard !isLoading else { return }
            refreshLocalPathChecks()
            recomputePendingSaveRequiresRebuild()
        }
    }
    @Published var includeHidden: Bool = false {
        didSet {
            guard !isLoading else { return }
            recomputePendingSaveRequiresRebuild()
        }
    }
    @Published var excludeSensitive: Bool = true {
        didSet {
            guard !isLoading else { return }
            recomputePendingSaveRequiresRebuild()
        }
    }
    @Published var networkRescanMinutes: Int = 30 {
        didSet {
            guard !isLoading else { return }
            recomputePendingSaveRequiresRebuild()
        }
    }

    /// 内存缓存的外部卷/网络卷状态快照，由检索引擎卷生命周期通知驱动更新
    @Published var volumeStatuses: [FfiVolumeStatus] = []
    /// 后台探测到的本机当前已挂载外部卷/网络卷列表
    @Published var mountedVolumes: [MountedVolumeEntry] = []
    /// 搜索结果是否包含已断开外部卷/网络卷的离线缓存条目（即时生效，独立于索引草稿）
    @Published var includeOfflineResults: Bool = true {
        didSet {
            guard !isLoading, includeOfflineResults != oldValue else { return }
            engine.includeOfflineResults = includeOfflineResults
        }
    }
    /// 非卷本地目录的后台存在性探测缓存（键为规范化路径）
    @Published var localPathAvailability: [String: Bool] = [:]
    /// 最近一次卷重扫或缓存清理失败提示
    @Published var volumeActionErrorMessage: String? = nil
    /// 索引配置保存失败提示
    @Published var indexSaveErrorMessage: String? = nil
    /// 后台主索引重建进行中标记与实时扫描计数，用于设置页底部闭环反馈
    @Published var isRebuildingIndex: Bool = false
    @Published var rebuildScannedCount: UInt64 = 0
    /// 缓存当前索引草稿保存是否触发全量重建：仅在草稿/已存配置/卷挂载生命周期/FDA 变更时重算，避免 SwiftUI 高频重绘反复跨 FFI。
    @Published private(set) var pendingSaveRequiresRebuild: Bool = false

    private var volumeStatusObserver: NSObjectProtocol?
    private var indexProgressObserver: NSObjectProtocol?
    private var indexReadyObserver: NSObjectProtocol?
    private var watcherErrorObserver: NSObjectProtocol?
    private var volumeStatusSubscription: AnyCancellable?
    private var workspaceVolumeObservers: [NSObjectProtocol] = []
    private let pathProbeQueue = DispatchQueue(label: "com.lokii.prefs.pathProbe", qos: .utility)
    private let mountedVolumeProbeQueue = DispatchQueue(label: "com.lokii.prefs.mountedVolumes", qos: .utility)
    private var localProbeGeneration: UInt64 = 0
    private var cliProbeGeneration: UInt64 = 0

    // 已持久化的索引配置，用于脏状态比对与放弃更改恢复
    @Published var savedIndexPaths: [String] = LokiiConfig.IndexConfig.defaultIndexPaths
    @Published var savedIndexApps: Bool = false
    @Published var savedExcludePatterns: [String] = LokiiConfig.IndexConfig.defaultExclude
    @Published var savedExcludePaths: [String] = LokiiConfig.IndexConfig.defaultExcludePaths
    @Published var savedIncludeHidden: Bool = false
    @Published var savedExcludeSensitive: Bool = true
    private var sensitiveNames: [String] = LokiiConfig.IndexConfig.defaultSensitiveNames
    private var serialScanPaths: [String] = LokiiConfig.IndexConfig.defaultSerialScanPaths
    @Published var savedNetworkRescanMinutes: Int = 30

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
        self.volumeStatuses = engine.getVolumeStatuses()
        self.includeOfflineResults = engine.includeOfflineResults
        loadConfig()
        updateFDAStatus()
        refreshLaunchAtLoginStatus()
        refreshCliStatus()
        volumeStatusSubscription = engine.$volumeStatuses.dropFirst().sink { [weak self] statuses in
            self?.volumeStatuses = statuses
        }
        volumeStatusObserver = NotificationCenter.default.addObserver(
            forName: SearchEngine.volumeStatusesChangedNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            self?.refreshLocalPathChecks()
            self?.refreshMountedVolumes()
            self?.recomputePendingSaveRequiresRebuild()
        }
        indexProgressObserver = NotificationCenter.default.addObserver(
            forName: SearchEngine.indexProgressNotification,
            object: nil,
            queue: .main
        ) { [weak self] note in
            guard let self else { return }
            let source = note.userInfo?["source"] as? String ?? ""
            guard source == "rebuild" || self.isRebuildingIndex else { return }
            let count = note.userInfo?["scannedFiles"] as? UInt64 ?? 0
            self.isRebuildingIndex = true
            self.rebuildScannedCount = count
        }
        indexReadyObserver = NotificationCenter.default.addObserver(
            forName: SearchEngine.indexReadyNotification,
            object: nil,
            queue: .main
        ) { [weak self] note in
            guard let self else { return }
            let source = note.userInfo?["source"] as? String ?? ""
            if source == "rebuild" || self.isRebuildingIndex {
                self.isRebuildingIndex = false
                self.rebuildScannedCount = 0
            }
        }
        watcherErrorObserver = NotificationCenter.default.addObserver(
            forName: SearchEngine.watcherErrorNotification,
            object: nil,
            queue: .main
        ) { [weak self] note in
            guard let self else { return }
            let source = note.userInfo?["source"] as? String ?? ""
            guard source == "rebuild" && self.isRebuildingIndex else { return }
            self.isRebuildingIndex = false
            self.rebuildScannedCount = 0
            let msg = (note.userInfo?["message"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines)
            if let msg, !msg.isEmpty {
                self.indexSaveErrorMessage = msg
            } else {
                self.indexSaveErrorMessage = L("prefs.index.saveFailed")
            }
        }
        windowSizeObserver = NotificationCenter.default.addObserver(
            forName: .lokiiWindowSizesDidChange,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            self?.refreshWindowSizeState()
        }
        let wsCenter = NSWorkspace.shared.notificationCenter
        for name in [NSWorkspace.didMountNotification, NSWorkspace.didUnmountNotification] {
            let obs = wsCenter.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                self?.refreshMountedVolumes()
            }
            workspaceVolumeObservers.append(obs)
        }
    }

    deinit {
        if let observer = volumeStatusObserver {
            NotificationCenter.default.removeObserver(observer)
        }
        if let observer = indexProgressObserver {
            NotificationCenter.default.removeObserver(observer)
        }
        if let observer = indexReadyObserver {
            NotificationCenter.default.removeObserver(observer)
        }
        if let observer = watcherErrorObserver {
            NotificationCenter.default.removeObserver(observer)
        }
        if let observer = windowSizeObserver {
            NotificationCenter.default.removeObserver(observer)
        }
        let wsCenter = NSWorkspace.shared.notificationCenter
        for observer in workspaceVolumeObservers {
            wsCenter.removeObserver(observer)
        }
    }

    /// 供「索引路径」分组展示的目录列表：过滤掉已由「外置磁盘与网络卷」分组统一托管的整卷根路径，避免重复展示
    var nonVolumeIndexPaths: [String] {
        indexPaths.filter { !SettingsPathStatusResolver.isVolumeRootPath($0) }
    }

    /// 供「外置磁盘与网络卷」分组展示的统一卷清单（合并本机已挂载卷、后端追踪卷与草稿中的整卷路径）
    var managedVolumes: [ManagedVolumeItem] {
        var items: [ManagedVolumeItem] = []
        var coveredMountedKeys: Set<String> = []
        var seenConfiguredKeys: Set<String> = []

        let indexedSet = Set(indexPaths.map { SettingsPathStatusResolver.normalizePath($0).lowercased() })
        let savedIndexedSet = Set(savedIndexPaths.map { SettingsPathStatusResolver.normalizePath($0).lowercased() })
        let mountedByKey = Dictionary(
            mountedVolumes.map { ($0.path.lowercased(), $0) },
            uniquingKeysWith: { first, _ in first }
        )

        // 1. 先纳入后端已知的外部卷状态（涵盖在线、重挂载漂移与离线保留缓存的卷）
        for status in volumeStatuses {
            let normalizedConfig = SettingsPathStatusResolver.normalizePath(status.configuredPath)
            guard SettingsPathStatusResolver.isVolumeRootPath(normalizedConfig) else { continue }
            let key = normalizedConfig.lowercased()
            guard seenConfiguredKeys.insert(key).inserted else { continue }

            let normalizedActive = status.activePath.map(SettingsPathStatusResolver.normalizePath)
            let activeKey = normalizedActive?.lowercased()
            let mountedMatch = activeKey.flatMap { mountedByKey[$0] } ?? mountedByKey[key]
            if let mountedMatch {
                coveredMountedKeys.insert(mountedMatch.path.lowercased())
            }
            if let activeKey {
                coveredMountedKeys.insert(activeKey)
            }
            coveredMountedKeys.insert(key)

            items.append(ManagedVolumeItem(
                id: key,
                configuredPath: normalizedConfig,
                activePath: normalizedActive,
                displayName: status.displayName.isEmpty ? (normalizedConfig as NSString).lastPathComponent : status.displayName,
                kind: status.kind,
                isIndexed: indexedSet.contains(key),
                isSavedIndexed: savedIndexedSet.contains(key),
                isMounted: mountedMatch != nil
                    || status.state == .online
                    || status.state == .syncing
                    || status.state == .needsConfirmation,
                status: status
            ))
        }

        // 2. 纳入草稿 indexPaths 中尚未被后端追踪的整卷路径（例如手动输入了一个离线卷路径）
        for rawPath in indexPaths {
            let normalized = SettingsPathStatusResolver.normalizePath(rawPath)
            guard SettingsPathStatusResolver.isVolumeRootPath(normalized) else { continue }
            let key = normalized.lowercased()
            guard seenConfiguredKeys.insert(key).inserted else { continue }

            let mountedMatch = mountedByKey[key]
            if let mountedMatch {
                coveredMountedKeys.insert(mountedMatch.path.lowercased())
            }
            items.append(ManagedVolumeItem(
                id: key,
                configuredPath: normalized,
                activePath: mountedMatch?.path,
                displayName: mountedMatch?.displayName ?? (normalized as NSString).lastPathComponent,
                kind: mountedMatch?.kind ?? "local_external",
                isIndexed: true,
                isSavedIndexed: savedIndexedSet.contains(key),
                isMounted: mountedMatch != nil,
                status: nil
            ))
        }

        // 3. 纳入本机当前已挂载、尚未加入索引的真实外置磁盘或网络卷（自动过滤临时挂载的 DMG/ISO 磁盘映像）
        for mounted in mountedVolumes where !mounted.isDiskImage {
            let key = mounted.path.lowercased()
            guard !coveredMountedKeys.contains(key),
                  seenConfiguredKeys.insert(key).inserted else { continue }
            items.append(ManagedVolumeItem(
                id: key,
                configuredPath: mounted.path,
                activePath: mounted.path,
                displayName: mounted.displayName,
                kind: mounted.kind,
                isIndexed: indexedSet.contains(key),
                isSavedIndexed: savedIndexedSet.contains(key),
                isMounted: true,
                status: nil
            ))
        }

        return items.sorted { lhs, rhs in
            if lhs.isIndexed != rhs.isIndexed {
                return lhs.isIndexed && !rhs.isIndexed
            }
            if lhs.isMounted != rhs.isMounted {
                return lhs.isMounted && !rhs.isMounted
            }
            return lhs.displayName.localizedStandardCompare(rhs.displayName) == .orderedAscending
        }
    }

    func setVolumeIndexed(_ volume: ManagedVolumeItem, enabled: Bool) {
        let targetKey = SettingsPathStatusResolver.normalizePath(volume.configuredPath).lowercased()
        if enabled {
            if !indexPaths.contains(where: { SettingsPathStatusResolver.normalizePath($0).lowercased() == targetKey }) {
                indexPaths.append(volume.configuredPath)
            }
        } else {
            indexPaths.removeAll { SettingsPathStatusResolver.normalizePath($0).lowercased() == targetKey }
        }
    }

    /// 在独立后台串行队列查询系统已挂载的外部卷与网络卷，与本地路径探活队列隔离且绝不在主线程访问挂载点元数据
    func refreshMountedVolumes() {
        mountedVolumeProbeQueue.async { [weak self] in
            let keys: [URLResourceKey] = [
                .volumeNameKey,
                .volumeIsLocalKey,
                .volumeIsRootFileSystemKey
            ]
            let urls = FileManager.default.mountedVolumeURLs(
                includingResourceValuesForKeys: keys,
                options: [.skipHiddenVolumes]
            ) ?? []

            let daSession = DASessionCreate(kCFAllocatorDefault)
            var entries: [MountedVolumeEntry] = []
            var seenKeys: Set<String> = []
            for url in urls {
                let normalized = SettingsPathStatusResolver.normalizePath(url.path)
                guard SettingsPathStatusResolver.isVolumeRootPath(normalized) else { continue }
                let lowerKey = normalized.lowercased()
                guard seenKeys.insert(lowerKey).inserted else { continue }
                let values = try? url.resourceValues(forKeys: Set(keys))
                if values?.volumeIsRootFileSystem == true {
                    continue
                }
                let isLocal = values?.volumeIsLocal ?? true
                let kind = isLocal ? "local_external" : "network"
                let isDiskImage = isLocal ? Self.probeIsDiskImage(volumeURL: url, session: daSession) : false
                let name = values?.volumeName ?? (normalized as NSString).lastPathComponent
                entries.append(MountedVolumeEntry(path: normalized, displayName: name, kind: kind, isDiskImage: isDiskImage))
            }

            DispatchQueue.main.async {
                guard let self else { return }
                if self.mountedVolumes != entries {
                    self.mountedVolumes = entries
                }
            }
        }
    }

    /// 通过 DiskArbitration + IOKit 驱动祖先链在后台零磁盘 I/O 判定挂载点是否为磁盘映像（DMG / ISO / SparseImage）
    private static func probeIsDiskImage(volumeURL: URL, session: DASession?) -> Bool {
        guard let session,
              let disk = DADiskCreateFromVolumePath(kCFAllocatorDefault, session, volumeURL as CFURL) else {
            return false
        }

        var ancestorClasses: [String] = []
        var media = DADiskCopyIOMedia(disk)
        if media == 0, let wholeDisk = DADiskCopyWholeDisk(disk) {
            media = DADiskCopyIOMedia(wholeDisk)
        }
        if media != 0 {
            defer { IOObjectRelease(media) }
            var iterator: io_iterator_t = 0
            let options = IOOptionBits(kIORegistryIterateRecursively | kIORegistryIterateParents)
            if IORegistryEntryCreateIterator(media, kIOServicePlane, options, &iterator) == KERN_SUCCESS {
                defer { IOObjectRelease(iterator) }
                var entry = IOIteratorNext(iterator)
                while entry != 0 {
                    var className = [CChar](repeating: 0, count: 128)
                    if IOObjectGetClass(entry, &className) == KERN_SUCCESS {
                        ancestorClasses.append(String(cString: className))
                    }
                    IOObjectRelease(entry)
                    entry = IOIteratorNext(iterator)
                }
            }
        }

        let desc = DADiskCopyDescription(disk) as? [String: Any]
        let deviceModel = desc?[kDADiskDescriptionDeviceModelKey as String] as? String
        let deviceProtocol = desc?[kDADiskDescriptionDeviceProtocolKey as String] as? String

        return SettingsPathStatusResolver.isDiskImageVolume(
            ancestorIOKitClasses: ancestorClasses,
            deviceModel: deviceModel,
            deviceProtocol: deviceProtocol
        )
    }

    /// 精确匹配已保存索引路径对应的后端卷槽位，用于安全绑定行级「重新扫描 / 清除缓存」动作
    func exactSavedVolumeStatus(for path: String) -> FfiVolumeStatus? {
        let normalized = SettingsPathStatusResolver.normalizePath(path)
        let isSaved = savedIndexPaths.contains {
            SettingsPathStatusResolver.normalizePath($0).caseInsensitiveCompare(normalized) == .orderedSame
        }
        guard isSaved else { return nil }
        return volumeStatuses.first { status in
            if SettingsPathStatusResolver.normalizePath(status.configuredPath)
                .caseInsensitiveCompare(normalized) == .orderedSame {
                return true
            }
            if let active = status.activePath, !active.isEmpty,
               SettingsPathStatusResolver.normalizePath(active)
                .caseInsensitiveCompare(normalized) == .orderedSame {
                return true
            }
            return false
        }
    }

    func availabilityState(for path: String) -> SettingsPathAvailabilityState {
        SettingsPathStatusResolver.resolve(
            path: path,
            volumeStatuses: volumeStatuses,
            localAvailabilityCache: localPathAvailability
        )
    }

    func rescanVolume(path: String) {
        volumeActionErrorMessage = nil
        engine.rescanVolume(pathOrId: path) { [weak self] outcome in
            guard let self, !outcome.succeeded else { return }
            self.volumeActionErrorMessage = outcome.errorMessage ?? L("prefs.volume.actionFailed")
        }
    }

    func confirmVolumeScan(path: String) {
        volumeActionErrorMessage = nil
        engine.confirmVolumeScan(pathOrId: path) { [weak self] outcome in
            guard let self, !outcome.succeeded else { return }
            self.volumeActionErrorMessage = outcome.errorMessage ?? L("prefs.volume.actionFailed")
        }
    }

    func cancelVolumeScan(path: String) {
        volumeActionErrorMessage = nil
        engine.cancelVolumeScan(pathOrId: path) { [weak self] outcome in
            guard let self, !outcome.succeeded else { return }
            self.volumeActionErrorMessage = outcome.errorMessage ?? L("prefs.volume.actionFailed")
        }
    }

    func clearVolumeCache(path: String) {
        volumeActionErrorMessage = nil
        engine.clearVolumeCache(pathOrId: path) { [weak self] outcome in
            guard let self, !outcome.succeeded else { return }
            self.volumeActionErrorMessage = outcome.errorMessage ?? L("prefs.volume.actionFailed")
        }
    }

    /// 后台探测非卷本地目录存在性，避免失联挂载点阻塞 SwiftUI 布局线程
    func refreshLocalPathChecks() {
        let currentVolumes = volumeStatuses
        let candidatePaths = Set(indexPaths + excludePaths)
        let nonVolumeTargets = candidatePaths.compactMap { raw -> String? in
            let normalized = SettingsPathStatusResolver.normalizePath(raw)
            if SettingsPathStatusResolver.isUnderVolumesNamespace(normalized) {
                return nil
            }
            let isTrackedVolume = SettingsPathStatusResolver.matchVolumeStatus(
                normalizedTarget: normalized,
                volumeStatuses: currentVolumes
            ) != nil
            return isTrackedVolume ? nil : normalized
        }

        let validLocalKeySet = Set(nonVolumeTargets)
        for existingKey in Array(localPathAvailability.keys) where !validLocalKeySet.contains(existingKey) {
            localPathAvailability.removeValue(forKey: existingKey)
        }

        guard !nonVolumeTargets.isEmpty else { return }

        localProbeGeneration &+= 1
        let generation = localProbeGeneration

        pathProbeQueue.async { [weak self] in
            var results: [String: Bool] = [:]
            let fm = FileManager.default
            for normalized in nonVolumeTargets {
                var isDir: ObjCBool = false
                let exists = fm.fileExists(atPath: normalized, isDirectory: &isDir) && isDir.boolValue
                results[normalized] = exists
            }
            DispatchQueue.main.async {
                guard let self, self.localProbeGeneration == generation else { return }
                let currentValidKeys = Set((self.indexPaths + self.excludePaths).map(SettingsPathStatusResolver.normalizePath))
                for (path, available) in results where currentValidKeys.contains(path) {
                    self.localPathAvailability[path] = available
                }
            }
        }
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
        recomputePendingSaveRequiresRebuild()
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

    /// 后台异步校验并刷新 /usr/local/bin/lokii 软链接状态，避免激活设置窗口时在主线程同步执行磁盘探测
    func refreshCliStatus() {
        // 安装/卸载进行中时不让被动窗口激活轮询覆盖状态
        if case .installing = cliState {
            return
        }

        cliProbeGeneration &+= 1
        let generation = cliProbeGeneration
        let target = cliTarget
        let source = cliSource

        pathProbeQueue.async { [weak self] in
            let fm = FileManager.default
            var isInstalled = false
            if let attrs = try? fm.attributesOfItem(atPath: target),
               attrs[.type] as? FileAttributeType == .typeSymbolicLink,
               let dest = try? fm.destinationOfSymbolicLink(atPath: target) {
                let isOurs = dest == source || dest.hasSuffix("/Contents/Helpers/lokii")
                isInstalled = isOurs && fm.fileExists(atPath: target)
            }

            DispatchQueue.main.async {
                guard let self, self.cliProbeGeneration == generation else { return }
                if case .installing = self.cliState {
                    return
                }
                if isInstalled {
                    self.cliState = .installed
                } else {
                    if case .failed = self.cliState {
                        // 保留失败信息供用户重试或查看日志
                    } else {
                        self.cliState = .notInstalled
                    }
                }
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
        self.hideResultsWhenEmpty = config.general.hideResultsWhenEmpty
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
        self.serialScanPaths = config.index.serialScanPaths
        self.savedNetworkRescanMinutes = config.index.networkRescanMinutes
        self.includeOfflineResults = engine.includeOfflineResults
        isLoading = false
        recomputePendingSaveRequiresRebuild()
        refreshAvailableTerminals()
        volumeStatuses = engine.getVolumeStatuses()
        engine.scheduleVolumeStatusesRefresh()
        refreshLocalPathChecks()
        refreshMountedVolumes()
    }

    private func saveGeneralConfig() {
        guard !isLoading else { return }
        _ = engine.updateConfig(LokiiConfig(
            general: .init(
                language: language,
                showInDock: showInDock,
                terminalBundleId: terminalBundleId,
                hideResultsWhenEmpty: hideResultsWhenEmpty
            ),
            index: .init(
                indexPaths: savedIndexPaths,
                indexApps: savedIndexApps,
                exclude: savedExcludePatterns,
                excludePaths: savedExcludePaths,
                includeHidden: savedIncludeHidden,
                excludeSensitive: savedExcludeSensitive,
                sensitiveNames: sensitiveNames,
                maxResults: maxResults,
                networkRescanMinutes: savedNetworkRescanMinutes,
                serialScanPaths: serialScanPaths
            ),
            window: .init(
                hideOnBlur: hideOnBlur,
                rememberPosition: rememberPosition
            )
        ))
    }

    /// 按当前草稿拼装完整配置对象，供 `previewPrimaryScopeAction` 与保存路径共用。
    func draftIndexConfig(base: LokiiConfig? = nil) -> LokiiConfig {
        var config = base ?? LokiiConfig(
            general: .init(
                language: language,
                showInDock: showInDock,
                terminalBundleId: terminalBundleId,
                hideResultsWhenEmpty: hideResultsWhenEmpty
            ),
            index: .init(
                indexPaths: indexPaths,
                indexApps: indexApps,
                exclude: excludePatterns,
                excludePaths: excludePaths,
                includeHidden: includeHidden,
                excludeSensitive: excludeSensitive,
                sensitiveNames: sensitiveNames,
                maxResults: maxResults,
                networkRescanMinutes: networkRescanMinutes,
                serialScanPaths: serialScanPaths
            ),
            window: .init(
                hideOnBlur: hideOnBlur,
                rememberPosition: rememberPosition
            )
        )
        config.index.indexPaths = indexPaths
        config.index.indexApps = indexApps
        config.index.exclude = excludePatterns
        config.index.excludePaths = excludePaths
        config.index.includeHidden = includeHidden
        config.index.excludeSensitive = excludeSensitive
        config.index.networkRescanMinutes = networkRescanMinutes
        return config
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

    /// 重算当前草稿保存是否需要主索引全量重建（直接委托 Rust `preview_primary_scope_action` 单一真相源），
    /// 供草稿变更、保存、放弃、恢复默认、FDA 与卷挂载变化时按需刷新缓存，避免在 SwiftUI body 内高频调用 FFI。
    private func recomputePendingSaveRequiresRebuild() {
        guard !isLoading else { return }
        let next = isIndexDirty
            ? (engine.previewPrimaryScopeAction(draftIndexConfig()) == .fullRebuild)
            : false
        if pendingSaveRequiresRebuild != next {
            pendingSaveRequiresRebuild = next
        }
    }

    func triggerManualRebuild(window: NSWindow? = NSApp.keyWindow) {
        guard !isIndexDirty, !isRebuildingIndex, !engine.isScanning else { return }
        ManualRebuildCoordinator.requestRebuild(
            window: window,
            statuses: engine.getVolumeStatuses(),
            onPrimaryOnly: { [weak self] in
                self?.startManualRebuild(includeExternal: false)
            },
            onRebuildAll: { [weak self] in
                self?.startManualRebuild(includeExternal: true)
            }
        )
    }

    private func startManualRebuild(includeExternal: Bool) {
        guard !isIndexDirty, !isRebuildingIndex, !engine.isScanning else { return }
        indexSaveErrorMessage = nil
        isRebuildingIndex = true
        rebuildScannedCount = 0
        if includeExternal {
            engine.rebuildIndex()
        } else {
            engine.rebuildPrimaryIndex()
        }
    }

    func saveIndexChanges() -> Bool {
        indexSaveErrorMessage = nil
        guard let currentConfig = engine.getConfig() else {
            indexSaveErrorMessage = L("prefs.index.saveFailed")
            return false
        }
        let draftConfig = draftIndexConfig(base: currentConfig)
        let needsRebuild = engine.previewPrimaryScopeAction(draftConfig) == .fullRebuild

        // 单一路径驱动：无论是否宽化均只调用 updateConfig，由 Rust watcher 唯一负责裁剪/增量补扫/主索引重建，
        // 绝不在此重复调用 rebuildIndex 或 rebuildPrimaryIndex，彻底消除双扫竞态与外置卷连坐。
        let success = engine.updateConfig(draftConfig)
        if success {
            savedIndexPaths = indexPaths
            savedIndexApps = indexApps
            savedExcludePatterns = excludePatterns
            savedExcludePaths = excludePaths
            savedIncludeHidden = includeHidden
            savedExcludeSensitive = excludeSensitive
            savedNetworkRescanMinutes = networkRescanMinutes
            recomputePendingSaveRequiresRebuild()
            if needsRebuild {
                isRebuildingIndex = true
                rebuildScannedCount = 0
            }
        } else {
            indexSaveErrorMessage = L("prefs.index.saveFailed")
        }
        return success
    }

    func discardIndexChanges() {
        let wasLoading = isLoading
        isLoading = true
        indexPaths = savedIndexPaths
        indexApps = savedIndexApps
        excludePatterns = savedExcludePatterns
        excludePaths = savedExcludePaths
        includeHidden = savedIncludeHidden
        excludeSensitive = savedExcludeSensitive
        networkRescanMinutes = savedNetworkRescanMinutes
        isLoading = wasLoading
        refreshLocalPathChecks()
        recomputePendingSaveRequiresRebuild()
    }

    func restoreIndexDefaults() {
        let defaults = LokiiConfig.defaultConfig.index
        let wasLoading = isLoading
        isLoading = true
        indexPaths = defaults.indexPaths
        indexApps = defaults.indexApps
        excludePatterns = defaults.exclude
        excludePaths = defaults.excludePaths
        includeHidden = defaults.includeHidden
        excludeSensitive = defaults.excludeSensitive
        networkRescanMinutes = defaults.networkRescanMinutes
        isLoading = wasLoading
        refreshLocalPathChecks()
        recomputePendingSaveRequiresRebuild()
    }

    /// 开关打开时内核追加的系统应用索引根；此处仅用于提示与排除规则的冲突。
    static let appIndexRoots = ["/Applications", "/System/Applications", "/System/Cryptexes/App/System/Applications"]

    /// 开关已打开且存在自动应用根被排除路径精确同路径（大小写无关、忽略尾随 `/`）排除时返回 true，
    /// 与后端同深度排除优先规则保持一致（默认 `/System` 排除与名称规则不会挡住系统应用根）。
    var appIndexConflicted: Bool {
        Self.isAppIndexConflicted(indexApps: indexApps, excludePaths: excludePaths)
    }

    static func isAppIndexConflicted(indexApps: Bool, excludePaths: [String]) -> Bool {
        guard indexApps else { return false }
        return appIndexRoots.contains { root in
            SettingsPathStatusResolver.containsSamePath(excludePaths, root)
        }
    }

    /// 索引配置的出厂默认值，直接透传自 Rust `AppConfig::default()` 单一来源。
    static var defaultIndexPaths: [String] { LokiiConfig.IndexConfig.defaultIndexPaths }
    static var defaultNetworkRescanMinutes: Int { LokiiConfig.IndexConfig.defaultNetworkRescanMinutes }
    static var defaultExcludePatterns: [String] { LokiiConfig.IndexConfig.defaultExclude }
    static var defaultExcludePaths: [String] { LokiiConfig.IndexConfig.defaultExcludePaths }

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
        let defaults = LokiiConfig.defaultConfig.index
        return indexPaths == defaults.indexPaths &&
            indexApps == defaults.indexApps &&
            excludePatterns == defaults.exclude &&
            excludePaths == defaults.excludePaths &&
            includeHidden == defaults.includeHidden &&
            excludeSensitive == defaults.excludeSensitive &&
            networkRescanMinutes == defaults.networkRescanMinutes
    }
}
