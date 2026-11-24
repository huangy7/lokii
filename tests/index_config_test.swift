import Cocoa

// MARK: - Standalone Test Harness for LokiiConfig.IndexConfig

let searchEnginePath = "Lokii/Lokii/SearchEngine.swift"
guard FileManager.default.fileExists(atPath: searchEnginePath) else {
    fputs("tests/index_config_test.swift: error: 找不到 \(searchEnginePath)\n", stderr)
    exit(1)
}

guard let searchEngineSource = try? String(contentsOfFile: searchEnginePath, encoding: .utf8) else {
    fputs("tests/index_config_test.swift: error: 无法读取 \(searchEnginePath)\n", stderr)
    exit(1)
}

guard let startRange = searchEngineSource.range(of: "struct LokiiConfig: Codable {"),
      let endRange = searchEngineSource.range(of: "final class SearchEngine") else {
    fputs("tests/index_config_test.swift: error: 未能在 \(searchEnginePath) 中定位 LokiiConfig\n", stderr)
    exit(1)
}

let configSection = searchEngineSource[startRange.lowerBound..<endRange.lowerBound]
guard let lastBrace = configSection.range(of: "}", options: .backwards) else {
    fputs("tests/index_config_test.swift: error: 未能在 LokiiConfig 片段中找到闭合括号\n", stderr)
    exit(1)
}
let lokiiConfigCode = String(configSection[..<lastBrace.upperBound])

guard let lifecycleStart = searchEngineSource.range(of: "private extension FfiVolumeStatus {"),
      let lifecycleEnd = searchEngineSource.range(of: "// MARK: - Event Handler") else {
    fputs("tests/index_config_test.swift: error: 未能在 \(searchEnginePath) 中定位卷生命周期比较方法\n", stderr)
    exit(1)
}
let lifecycleCode = String(searchEngineSource[lifecycleStart.lowerBound..<lifecycleEnd.lowerBound])

let preferencesPath = "Lokii/Lokii/Preferences/PreferencesViewModel.swift"
guard let preferencesSource = try? String(contentsOfFile: preferencesPath, encoding: .utf8),
      let resolverStart = preferencesSource.range(of: "enum SettingsPathAvailabilityState: Equatable {"),
      let resolverEnd = preferencesSource.range(of: "// MARK: - Preferences ViewModel") else {
    fputs("tests/index_config_test.swift: error: 未能在 \(preferencesPath) 中定位 SettingsPathStatusResolver\n", stderr)
    exit(1)
}
let pathResolverCode = String(preferencesSource[resolverStart.lowerBound..<resolverEnd.lowerBound])

guard let appDelegateSource = try? String(contentsOfFile: "Lokii/Lokii/AppDelegate.swift", encoding: .utf8),
      let guardStart = appDelegateSource.range(of: "@_silgen_name(\"flock\")"),
      let delegateStart = appDelegateSource.range(of: "final class AppDelegate:") else {
    fputs("tests/index_config_test.swift: error: 未能在 AppDelegate.swift 中定位 SingleInstanceGuard\n", stderr)
    exit(1)
}
let singleInstanceGuardCode = String(appDelegateSource[guardStart.lowerBound..<delegateStart.lowerBound])

guard let mainSwiftSource = try? String(contentsOfFile: "Lokii/Lokii/main.swift", encoding: .utf8),
      mainSwiftSource.contains("SingleInstanceGuard.acquireOrActivateExistingAndExit()") else {
    fputs("tests/index_config_test.swift: error: main.swift 必须在创建 AppDelegate 前调用 SingleInstanceGuard.acquireOrActivateExistingAndExit()\n", stderr)
    exit(1)
}

guard appDelegateSource.contains("private lazy var engine = SearchEngine()") else {
    fputs("tests/index_config_test.swift: error: AppDelegate.engine 必须为 lazy，避免重复实例初始化 core\n", stderr)
    exit(1)
}

let mockVolumeTypesCode = """
public enum FfiVolumeState: Equatable, Hashable {
    case online
    case syncing
    case offline
    case missing
}

public struct FfiVolumeStatus: Equatable, Hashable {
    public var volumeId: String
    public var configuredPath: String
    public var activePath: String?
    public var displayName: String
    public var kind: String
    public var state: FfiVolumeState
    public var entryCount: UInt64
    public var lastSyncAt: UInt64?
    public var isRemounted: Bool

    public init(
        volumeId: String = "vol-1",
        configuredPath: String,
        activePath: String? = nil,
        displayName: String = "Disk",
        kind: String = "network",
        state: FfiVolumeState,
        entryCount: UInt64 = 0,
        lastSyncAt: UInt64? = nil,
        isRemounted: Bool = false
    ) {
        self.volumeId = volumeId
        self.configuredPath = configuredPath
        self.activePath = activePath
        self.displayName = displayName
        self.kind = kind
        self.state = state
        self.entryCount = entryCount
        self.lastSyncAt = lastSyncAt
        self.isRemounted = isRemounted
    }
}
"""

let runnerSource = """
import Cocoa
import Darwin

\(singleInstanceGuardCode)
\(lokiiConfigCode)
\(mockVolumeTypesCode)
\(lifecycleCode)
\(pathResolverCode)

func assertEqual<T: Equatable>(_ actual: T, _ expected: T, _ message: String = "", file: StaticString = #file, line: UInt = #line) {
    if actual != expected {
        fputs("断言失败: [\\(actual)] != [\\(expected)] - \\(message) (\\(file):\\(line))\\n", stderr)
        exit(1)
    }
}

func assertTrue(_ condition: Bool, _ message: String = "", file: StaticString = #file, line: UInt = #line) {
    if !condition {
        fputs("断言失败: 条件不满足 - \\(message) (\\(file):\\(line))\\n", stderr)
        exit(1)
    }
}

do {
    print("[1/3] 测试 IndexConfig 具备 exclude_paths 时的正常反序列化...")
    let jsonWithExcludePaths = \"\"\"
    {
        "index_paths": ["~", "/Volumes/Data"],
        "index_apps": true,
        "exclude": ["node_modules", ".git"],
        "exclude_paths": ["~/Library", "/System"],
        "include_hidden": false,
        "sensitive_names": [".ssh", ".vault"],
        "max_results": 200,
        "network_rescan_minutes": 15
    }
    \"\"\".data(using: .utf8)!

    let decoder = JSONDecoder()
    let config1 = try decoder.decode(LokiiConfig.IndexConfig.self, from: jsonWithExcludePaths)
    assertEqual(config1.indexPaths, ["~", "/Volumes/Data"], "indexPaths 解析匹配")
    assertTrue(config1.indexApps, "index_apps 解析匹配")
    assertEqual(config1.exclude, ["node_modules", ".git"], "exclude 解析匹配")
    assertEqual(config1.excludePaths, ["~/Library", "/System"], "excludePaths 解析匹配")
    assertEqual(config1.sensitiveNames, [".ssh", ".vault"], "sensitiveNames 解析匹配")
    assertEqual(config1.maxResults, 200, "maxResults 解析匹配")
    assertEqual(config1.networkRescanMinutes, 15, "networkRescanMinutes 解析匹配")

    print("[2/3] 测试 IndexConfig 缺少 exclude_paths 时的默认回退...")
    let jsonWithoutExcludePaths = \"\"\"
    {
        "index_paths": ["~"],
        "exclude": ["target"],
        "include_hidden": true,
        "max_results": 500
    }
    \"\"\".data(using: .utf8)!

    let config2 = try decoder.decode(LokiiConfig.IndexConfig.self, from: jsonWithoutExcludePaths)
    assertEqual(config2.indexPaths, ["~"], "indexPaths 正常")
    assertTrue(!config2.indexApps, "缺少 index_apps 时默认关闭")
    assertEqual(config2.exclude, ["target"], "exclude 正常")
    assertEqual(config2.excludePaths, LokiiConfig.IndexConfig.defaultExcludePaths, "缺少 exclude_paths 时应回退至默认排除路径列表")
    assertTrue(config2.includeHidden, "includeHidden 为 true")
    assertTrue(config2.excludeSensitive, "缺少 exclude_sensitive 时默认保护凭据目录")
    assertEqual(config2.sensitiveNames, LokiiConfig.IndexConfig.defaultSensitiveNames, "缺少 sensitive_names 时应回退至默认敏感路径列表")
    assertEqual(config2.networkRescanMinutes, 30, "缺少 network_rescan_minutes 时回退为 30 分钟")

    print("[3/3] 测试 IndexConfig 序列化能正确输出 exclude_paths 字段...")
    let encoder = JSONEncoder()
    let encodedData = try encoder.encode(config1)
    let encodedJSON = String(data: encodedData, encoding: .utf8)!
    assertTrue(encodedJSON.contains("\\"exclude_paths\\":"), "JSON 中应包含 exclude_paths 字段")
    assertTrue(encodedJSON.contains("\\"network_rescan_minutes\\":15"), "JSON 中应保留 network_rescan_minutes")

    print("[4/6] 测试卷离线与本地缺失目录的四态区分解析 (SettingsPathStatusResolver)...")
    let offlineNas = FfiVolumeStatus(
        configuredPath: "/Volumes/NAS",
        activePath: nil,
        displayName: "NAS",
        kind: "network",
        state: .offline,
        entryCount: 4200
    )
    let resolvedNas = SettingsPathStatusResolver.resolve(
        path: "/Volumes/NAS",
        volumeStatuses: [offlineNas],
        localAvailabilityCache: [:]
    )
    assertEqual(resolvedNas, .volumeOffline(entryCount: 4200, kind: "network"), "离线卷应归类为 volumeOffline 而非本地未找到")

    let missingLocal = "/tmp/nonexistent_lokii_dir"
    let resolvedUnprobed = SettingsPathStatusResolver.resolve(
        path: missingLocal,
        volumeStatuses: [offlineNas],
        localAvailabilityCache: [:]
    )
    assertEqual(resolvedUnprobed, .localChecking, "未完成后台探测的纯本地路径应先呈现 localChecking")

    let resolvedMissing = SettingsPathStatusResolver.resolve(
        path: missingLocal,
        volumeStatuses: [offlineNas],
        localAvailabilityCache: [missingLocal: false]
    )
    assertEqual(resolvedMissing, .localMissing, "本地探测失败路径应呈现 localMissing")

    let localOk = "/tmp/existing_lokii_dir"
    let resolvedOk = SettingsPathStatusResolver.resolve(
        path: localOk,
        volumeStatuses: [offlineNas],
        localAvailabilityCache: [localOk: true]
    )
    assertEqual(resolvedOk, .localAvailable, "本地探测存在路径应呈现 localAvailable")

    print("[5/6] 测试重挂载路径映射与同步中卷状态解析...")
    let remountedVol = FfiVolumeStatus(
        configuredPath: "/Volumes/Media",
        activePath: "/Volumes/Media-1",
        displayName: "Media",
        kind: "local_external",
        state: .online,
        entryCount: 980,
        isRemounted: true
    )
    let syncingVol = FfiVolumeStatus(
        configuredPath: "/Volumes/Work",
        activePath: "/Volumes/Work",
        displayName: "Work",
        kind: "local_external",
        state: .syncing,
        entryCount: 100
    )
    assertEqual(
        SettingsPathStatusResolver.resolve(path: "/Volumes/Media", volumeStatuses: [remountedVol], localAvailabilityCache: [:]),
        .volumeOnline(entryCount: 980, activePath: "/Volumes/Media-1", isRemounted: true, kind: "local_external"),
        "重挂载卷按配置路径应返回包含新挂载点的在线状态"
    )
    assertEqual(
        SettingsPathStatusResolver.resolve(path: "/Volumes/Media-1", volumeStatuses: [remountedVol], localAvailabilityCache: [:]),
        .volumeOnline(entryCount: 980, activePath: "/Volumes/Media-1", isRemounted: true, kind: "local_external"),
        "重挂载卷按活动挂载路径也应命中对应在线状态"
    )
    assertEqual(
        SettingsPathStatusResolver.resolve(path: "/Volumes/Work", volumeStatuses: [syncingVol], localAvailabilityCache: [:]),
        .volumeSyncing(kind: "local_external"),
        "同步中卷应返回 volumeSyncing"
    )

    print("[6/7] 测试路径段边界、大小写不敏感、尾部斜杠、波浪号展开与深度优先匹配...")
    let diskSlot = FfiVolumeStatus(configuredPath: "/Volumes/Disk/", activePath: nil, displayName: "Disk", kind: "local_external", state: .offline, entryCount: 50)
    let disk2Slot = FfiVolumeStatus(configuredPath: "/Volumes/Disk2", activePath: "/Volumes/Disk2", displayName: "Disk2", kind: "local_external", state: .online, entryCount: 120)
    let nestedSlot = FfiVolumeStatus(configuredPath: "/Volumes/Disk/Sub", activePath: "/Volumes/Disk/Sub", displayName: "DiskSub", kind: "local_external", state: .online, entryCount: 10)
    let homeSlot = FfiVolumeStatus(configuredPath: "~/ExternalLink", activePath: nil, displayName: "HomeMount", kind: "local_external", state: .offline, entryCount: 5)
    let allVols = [diskSlot, disk2Slot, nestedSlot, homeSlot]

    assertEqual(
        SettingsPathStatusResolver.resolve(path: "/Volumes/Disk2/file.txt", volumeStatuses: allVols, localAvailabilityCache: [:]),
        .volumeOnline(entryCount: 120, activePath: "/Volumes/Disk2", isRemounted: false, kind: "local_external"),
        "/Volumes/Disk 绝不可误匹配 /Volumes/Disk2/file.txt"
    )
    assertEqual(
        SettingsPathStatusResolver.resolve(path: "/volumes/disk/sub/deep.log", volumeStatuses: allVols, localAvailabilityCache: [:]),
        .volumeOnline(entryCount: 10, activePath: "/Volumes/Disk/Sub", isRemounted: false, kind: "local_external"),
        "大小写不同且存在嵌套配置时应命中深度优先子路径"
    )
    assertEqual(
        SettingsPathStatusResolver.resolve(path: "/volumes/disk/other/doc.txt", volumeStatuses: allVols, localAvailabilityCache: [:]),
        .volumeOffline(entryCount: 50, kind: "local_external"),
        "尾部斜杠应归一化并按目录边界命中离线卷"
    )
    let expandedHomeTarget = ("~/ExternalLink/item" as NSString).expandingTildeInPath
    assertEqual(
        SettingsPathStatusResolver.resolve(path: expandedHomeTarget, volumeStatuses: allVols, localAvailabilityCache: [:]),
        .volumeOffline(entryCount: 5, kind: "local_external"),
        "~ 前缀路径应与展开后的绝对路径一致匹配"
    )

    // 校验离线占位槽位（如手动删除 volumes/ 目录或缓存缺失，state == .offline 但 entryCount == 0）必须归类为未缓存离线，不可误报“索引已保留”
    let uncachedOfflineSlot = FfiVolumeStatus(
        configuredPath: "/Volumes/Home1",
        activePath: nil,
        displayName: "Home1",
        kind: "network",
        state: .offline,
        entryCount: 0,
        lastSyncAt: nil
    )
    assertEqual(
        SettingsPathStatusResolver.resolve(
            path: "/Volumes/Home1",
            volumeStatuses: [uncachedOfflineSlot],
            localAvailabilityCache: [:]
        ),
        .volumeUncachedOffline,
        "entryCount == 0 的离线占位卷应解析为 volumeUncachedOffline，绝不可误报为已保留缓存"
    )

    // 校验清除卷缓存后（volumeStatuses 为空或未命中）的 /Volumes/<Name> 路径保持离线未索引状态而非误判为 localMissing
    assertEqual(
        SettingsPathStatusResolver.resolve(
            path: "/Volumes/ExtDrive",
            volumeStatuses: [],
            localAvailabilityCache: ["/Volumes/ExtDrive": false]
        ),
        .volumeUncachedOffline,
        "清除缓存后未挂载的 /Volumes/ExtDrive 应解析为 volumeUncachedOffline，绝不可退化为 localMissing"
    )
    let mountedMissingSubpathSlot = FfiVolumeStatus(
        configuredPath: "/Volumes/NAS/MissingSub",
        activePath: "/Volumes/NAS/MissingSub",
        displayName: "NAS",
        kind: "network",
        state: .missing,
        entryCount: 0
    )
    assertEqual(
        SettingsPathStatusResolver.resolve(
            path: "/Volumes/NAS/MissingSub",
            volumeStatuses: [mountedMissingSubpathSlot],
            localAvailabilityCache: [:]
        ),
        .localMissing,
        "已挂载卷（activePath 非空）的子路径缺失（state == .missing）应解析为 localMissing 而非 volumeUncachedOffline"
    )
    assertEqual(
        SettingsPathStatusResolver.isUnderVolumesNamespace("/Volumes/ExtDrive/Sub"),
        true,
        "/Volumes/ExtDrive/Sub 应识别为挂载卷命名空间路径"
    )
    assertEqual(
        SettingsPathStatusResolver.isUnderVolumesNamespace("/Volumes"),
        false,
        "裸 /Volumes 容器目录不应被视为具体挂载卷"
    )
    let conflictHome = NSHomeDirectory()
    assertEqual(
        SettingsPathStatusResolver.containsSamePath(["~/Library/"], conflictHome + "/library"),
        true,
        "波浪号展开、尾部斜杠与大小写差异后的同一路径应判定为相同"
    )
    assertEqual(
        SettingsPathStatusResolver.containsSamePath(["~/Library"], "~/Library/xxx"),
        false,
        "子路径属于合法穿透，不应判定为同路径冲突"
    )
    assertEqual(
        SettingsPathStatusResolver.containsSamePath(["~/Library/xxx"], "~/Library"),
        false,
        "祖先路径不应判定为同路径冲突"
    )
    assertEqual(
        SettingsPathStatusResolver.containsSamePath(["~/Library", "/opt"], "~/LibraryX"),
        false,
        "仅前缀相同的兄弟路径不应误判"
    )

    print("[8/8] 测试空 JSON 与默认初始化回退值与 Rust AppConfig::default() 一致...")
    let emptyData = "{}".data(using: .utf8)!
    let decodedIndex = try decoder.decode(LokiiConfig.IndexConfig.self, from: emptyData)
    let defaultIndex = LokiiConfig.IndexConfig()
    assertEqual(decodedIndex.indexPaths, ["~"], "IndexConfig decode indexPaths 应默认 [~]")
    assertEqual(defaultIndex.indexPaths, ["~"], "IndexConfig init indexPaths 应默认 [~]")
    assertEqual(decodedIndex.exclude, LokiiConfig.IndexConfig.defaultExclude, "IndexConfig decode exclude 应匹配默认列表")
    assertEqual(decodedIndex.exclude.count, 35, "IndexConfig defaultExclude 应包含 35 项")
    assertEqual(decodedIndex.excludePaths, LokiiConfig.IndexConfig.defaultExcludePaths, "IndexConfig decode excludePaths 应匹配默认列表")
    assertEqual(decodedIndex.excludePaths.count, 25, "IndexConfig defaultExcludePaths 应包含 25 项")
    assertEqual(decodedIndex.sensitiveNames, LokiiConfig.IndexConfig.defaultSensitiveNames, "IndexConfig decode sensitiveNames 应匹配默认列表")
    assertEqual(decodedIndex.sensitiveNames.count, 12, "IndexConfig defaultSensitiveNames 应包含 12 项")
    assertEqual(decodedIndex.includeHidden, false, "IndexConfig decode includeHidden 应默认 false")
    assertEqual(defaultIndex.includeHidden, false, "IndexConfig init includeHidden 应默认 false")
    assertEqual(decodedIndex.maxResults, 500, "IndexConfig decode maxResults 应默认 500")
    assertEqual(defaultIndex.maxResults, 500, "IndexConfig init maxResults 应默认 500")
    assertEqual(decodedIndex.excludeSensitive, true, "IndexConfig decode excludeSensitive 应默认 true")
    assertEqual(decodedIndex.networkRescanMinutes, 30, "IndexConfig decode networkRescanMinutes 应默认 30")

    let decodedWindow = try decoder.decode(LokiiConfig.WindowConfig.self, from: emptyData)
    let defaultWindow = LokiiConfig.WindowConfig()
    assertEqual(decodedWindow.hideOnBlur, true, "WindowConfig decode hideOnBlur 应默认 true")
    assertEqual(defaultWindow.hideOnBlur, true, "WindowConfig init hideOnBlur 应默认 true")
    assertEqual(decodedWindow.rememberPosition, false, "WindowConfig decode rememberPosition 应默认 false")
    assertEqual(defaultWindow.rememberPosition, false, "WindowConfig init rememberPosition 应默认 false")
    var updatedStatus = disk2Slot
    updatedStatus.entryCount += 1
    assertEqual(updatedStatus == disk2Slot, false, "计数变化仍应更新卷状态快照")
    assertEqual(updatedStatus.hasSameLifecycle(as: disk2Slot), true, "只更新计数时不应触发重搜")
    updatedStatus.state = .syncing
    assertEqual(updatedStatus.hasSameLifecycle(as: disk2Slot), false, "开始扫描应触发生命周期通知")
    updatedStatus = disk2Slot
    updatedStatus.activePath = "/Volumes/Disk2-1"
    assertEqual(updatedStatus.hasSameLifecycle(as: disk2Slot), false, "重挂载路径变化应触发生命周期通知")
    updatedStatus = disk2Slot
    updatedStatus.lastSyncAt = 123
    assertEqual(updatedStatus.hasSameLifecycle(as: disk2Slot), false, "扫描完成应触发生命周期通知")

    let batchMatcher = SettingsPathStatusResolver.VolumeStatusMatcher(volumeStatuses: allVols)
    assertEqual(batchMatcher.match(path: "/Volumes/Disk2/file.txt")?.displayName, "Disk2", "预归一化批处理匹配器应按边界精确命中 Disk2")
    assertEqual(batchMatcher.match(path: "/volumes/disk/sub/deep.log")?.displayName, "DiskSub", "预归一化批处理匹配器应深度优先命中嵌套卷")
    assertEqual(batchMatcher.match(path: expandedHomeTarget)?.displayName, "HomeMount", "预归一化批处理匹配器应兼容 ~ 展开路径")
    assertTrue(batchMatcher.match(path: "/Users/local/file.txt") == nil, "非卷本地路径应返回 nil")

    // GeneralConfig.hideResultsWhenEmpty 缺省兼容与往返编解码
    assertEqual(LokiiConfig.GeneralConfig().hideResultsWhenEmpty, true, "GeneralConfig 默认构造时 hideResultsWhenEmpty 应默认开启")
    let legacyGeneralData = \"\"\"
    {"language": "zh-Hans", "show_in_dock": true}
    \"\"\".data(using: .utf8)!
    let decodedLegacyGeneral = try JSONDecoder().decode(LokiiConfig.GeneralConfig.self, from: legacyGeneralData)
    assertEqual(decodedLegacyGeneral.hideResultsWhenEmpty, true, "旧版配置缺失 hide_results_when_empty 时应默认回退为 true")

    let disabledGeneralData = \"\"\"
    {"language": "en", "show_in_dock": true, "hide_results_when_empty": false}
    \"\"\".data(using: .utf8)!
    let decodedDisabledGeneral = try JSONDecoder().decode(LokiiConfig.GeneralConfig.self, from: disabledGeneralData)
    assertEqual(decodedDisabledGeneral.hideResultsWhenEmpty, false, "hide_results_when_empty = false 应准确解码")
    let encodedGeneral = String(data: try JSONEncoder().encode(decodedDisabledGeneral), encoding: .utf8) ?? ""
    assertTrue(encodedGeneral.contains("\\"hide_results_when_empty\\":false"), "编码后的 JSON 应保留 hide_results_when_empty 键")

    // 主索引重建判定：仅修改卷开关/网络重扫间隔/追加排除路径时不触发主索引重建；修改本地目录或全局扫描选项时触发 (Review Focus #5)
    let mountedVols = [MountedVolumeEntry(path: "/Volumes/NAS", displayName: "NAS", kind: "network")]
    assertEqual(
        SettingsPathStatusResolver.requiresPrimaryRebuild(
            draftIndexPaths: ["~", "/Volumes/NAS"],
            savedIndexPaths: ["~"],
            draftIndexApps: false,
            savedIndexApps: false,
            draftIncludeHidden: false,
            savedIncludeHidden: false,
            draftExcludeSensitive: true,
            savedExcludeSensitive: true,
            draftExcludePatterns: ["node_modules"],
            savedExcludePatterns: ["node_modules"],
            draftExcludePaths: ["/System"],
            savedExcludePaths: ["/System"],
            mountedVolumes: mountedVols,
            volumeStatuses: []
        ),
        false,
        "仅勾选已挂载外置卷/网络卷时不得触发主索引重建"
    )
    assertEqual(
        SettingsPathStatusResolver.requiresPrimaryRebuild(
            draftIndexPaths: ["~"],
            savedIndexPaths: ["~"],
            draftIndexApps: false,
            savedIndexApps: false,
            draftIncludeHidden: false,
            savedIncludeHidden: false,
            draftExcludeSensitive: true,
            savedExcludeSensitive: true,
            draftExcludePatterns: ["node_modules"],
            savedExcludePatterns: ["node_modules"],
            draftExcludePaths: ["/System", "~/Downloads"],
            savedExcludePaths: ["/System"],
            mountedVolumes: mountedVols,
            volumeStatuses: []
        ),
        false,
        "仅追加新的排除路径（或仅改 networkRescanMinutes）时不得触发主索引重建"
    )
    assertEqual(
        SettingsPathStatusResolver.requiresPrimaryRebuild(
            draftIndexPaths: ["~", "/opt/work"],
            savedIndexPaths: ["~"],
            draftIndexApps: false,
            savedIndexApps: false,
            draftIncludeHidden: false,
            savedIncludeHidden: false,
            draftExcludeSensitive: true,
            savedExcludeSensitive: true,
            draftExcludePatterns: ["node_modules"],
            savedExcludePatterns: ["node_modules"],
            draftExcludePaths: ["/System"],
            savedExcludePaths: ["/System"],
            mountedVolumes: mountedVols,
            volumeStatuses: []
        ),
        true,
        "新增本地索引目录必须判定为需要主索引重建"
    )
    assertEqual(
        SettingsPathStatusResolver.requiresPrimaryRebuild(
            draftIndexPaths: ["~"],
            savedIndexPaths: ["~"],
            draftIndexApps: false,
            savedIndexApps: false,
            draftIncludeHidden: true,
            savedIncludeHidden: false,
            draftExcludeSensitive: true,
            savedExcludeSensitive: true,
            draftExcludePatterns: ["node_modules"],
            savedExcludePatterns: ["node_modules"],
            draftExcludePaths: ["/System"],
            savedExcludePaths: ["/System"],
            mountedVolumes: mountedVols,
            volumeStatuses: []
        ),
        true,
        "切换包含隐藏文件必须判定为需要主索引重建"
    )
    assertEqual(
        SettingsPathStatusResolver.requiresPrimaryRebuild(
            draftIndexPaths: ["~"],
            savedIndexPaths: ["~"],
            draftIndexApps: false,
            savedIndexApps: false,
            draftIncludeHidden: false,
            savedIncludeHidden: false,
            draftExcludeSensitive: true,
            savedExcludeSensitive: true,
            draftExcludePatterns: ["node_modules"],
            savedExcludePatterns: ["node_modules"],
            draftExcludePaths: [],
            savedExcludePaths: ["/System"],
            mountedVolumes: mountedVols,
            volumeStatuses: []
        ),
        true,
        "移除已有排除路径必须判定为需要主索引重建"
    )

    print("[7/7] 测试磁盘映像（DMG/ISO）精确识别与物理外置设备（U 盘/SSD/SD 卡）零误杀...")
    assertEqual(
        SettingsPathStatusResolver.isDiskImageVolume(
            ancestorIOKitClasses: ["AppleDiskImageDevice", "AppleDiskImagesController"],
            deviceModel: "Disk Image",
            deviceProtocol: "Virtual Interface"
        ),
        true,
        "DiskImages2 挂载的 HFS+ DMG 必须识别为磁盘映像"
    )
    assertEqual(
        SettingsPathStatusResolver.isDiskImageVolume(
            ancestorIOKitClasses: ["AppleAPFSVolume", "AppleAPFSContainer", "AppleAPFSMedia", "AppleDiskImageDevice", "AppleDiskImagesController"],
            deviceModel: "Disk Image",
            deviceProtocol: "Disk Image"
        ),
        true,
        "APFS 合成容器格式的 DMG 必须识别为磁盘映像"
    )
    assertEqual(
        SettingsPathStatusResolver.isDiskImageVolume(
            ancestorIOKitClasses: ["IOHDIXHDDriveOutKernel", "IOHDIXController"],
            deviceModel: nil,
            deviceProtocol: nil
        ),
        true,
        "传统 IOHDIX 驱动挂载的 DMG / SparseImage 必须识别为磁盘映像"
    )
    assertEqual(
        SettingsPathStatusResolver.isDiskImageVolume(
            ancestorIOKitClasses: ["IOSCSIPeripheralDeviceNub", "IOUSBMassStorageInterfaceNub", "IOUSBHostDevice"],
            deviceModel: "SanDisk Ultra",
            deviceProtocol: "USB"
        ),
        false,
        "物理 USB 闪存盘绝不能误判为磁盘映像"
    )
    assertEqual(
        SettingsPathStatusResolver.isDiskImageVolume(
            ancestorIOKitClasses: ["AppleAPFSVolume", "AppleAPFSContainer", "IOUSBMassStorageDriver", "AppleUSBXHCI"],
            deviceModel: "Disk Image",
            deviceProtocol: "Virtual Interface"
        ),
        false,
        "即使外接 USB APFS 硬盘盒固件误报 Virtual Interface / Disk Image，命中物理 USB 总线祖先时也绝不能误杀"
    )
    assertEqual(
        SettingsPathStatusResolver.isDiskImageVolume(
            ancestorIOKitClasses: ["AppleAPFSVolume", "AppleAPFSContainer", "IONVMeController", "IOThunderboltSwitch"],
            deviceModel: "Samsung SSD 980 PRO",
            deviceProtocol: "PCI-Express"
        ),
        false,
        "雷电 NVMe 外置固态硬盘绝不能误判为磁盘映像"
    )
    assertEqual(
        SettingsPathStatusResolver.isDiskImageVolume(
            ancestorIOKitClasses: ["AppleSDXCBlockStorageDevice"],
            deviceModel: "SD Card Reader",
            deviceProtocol: "Secure Digital"
        ),
        false,
        "内置 SD 卡槽存储卡绝不能误判为磁盘映像"
    )
    assertEqual(
        SettingsPathStatusResolver.isDiskImageVolume(
            ancestorIOKitClasses: [],
            deviceModel: "Disk Image",
            deviceProtocol: "Virtual Interface"
        ),
        true,
        "IOKit 祖先链为空时，明确标明 Disk Image 型号应回退判定为磁盘映像"
    )
    assertEqual(
        SettingsPathStatusResolver.isDiskImageVolume(
            ancestorIOKitClasses: [],
            deviceModel: "External SSD",
            deviceProtocol: "Virtual Interface"
        ),
        false,
        "仅报 Virtual Interface 但未标明 Disk Image 时应保守放行，避免误伤第三方外接盘"
    )

    let dmgEntry = MountedVolumeEntry(path: "/Volumes/Lokii", displayName: "Lokii", kind: "local_external", isDiskImage: true)
    assertEqual(dmgEntry.isDiskImage, true, "MountedVolumeEntry 应支持记录 isDiskImage 标志")
    let usbEntry = MountedVolumeEntry(path: "/Volumes/MyUSB", displayName: "MyUSB", kind: "local_external")
    assertEqual(usbEntry.isDiskImage, false, "MountedVolumeEntry 默认 isDiskImage 应为 false")

    let lockTmpDir = FileManager.default.temporaryDirectory.appendingPathComponent("lokii-lock-test-\\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: lockTmpDir, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: lockTmpDir) }
    let testLockPath = lockTmpDir.appendingPathComponent("lokii.lock").path

    var heldGuard: SingleInstanceGuard?
    switch SingleInstanceGuard.tryAcquire(at: testLockPath) {
    case .acquired(let g):
        let fdFlags = Darwin.fcntl(g.fileDescriptor, F_GETFD)
        assertTrue((fdFlags & FD_CLOEXEC) != 0, "单实例锁文件描述符必须设置 FD_CLOEXEC")
        heldGuard = g
    case .alreadyHeld, .ioError:
        fputs("断言失败: 首次获取测试单实例锁应成功\\n", stderr)
        exit(1)
    }

    withExtendedLifetime(heldGuard) {
        switch SingleInstanceGuard.tryAcquire(at: testLockPath) {
        case .alreadyHeld(let holderPid):
            assertEqual(holderPid, Darwin.getpid(), "重复获取单实例锁应返回当前持锁进程 PID")
        case .acquired, .ioError:
            fputs("断言失败: 持有单实例锁期间二次 tryAcquire 必须返回 alreadyHeld\\n", stderr)
            exit(1)
        }
    }

    heldGuard = nil
    switch SingleInstanceGuard.tryAcquire(at: testLockPath) {
    case .acquired(let reacquired):
        _ = reacquired
    case .alreadyHeld, .ioError:
        fputs("断言失败: 释放锁句柄后应可重新获取单实例锁\\n", stderr)
        exit(1)
    }

    assertEqual(
        SingleInstanceGuard.matchesExistingInstance(
            candidateBundleId: "com.lokii.app",
            candidateName: "Lokii",
            currentBundleId: "com.lokii.app"
        ),
        true,
        "相同 bundleId 的持锁进程应允许唤醒"
    )
    assertEqual(
        SingleInstanceGuard.matchesExistingInstance(
            candidateBundleId: "com.apple.Safari",
            candidateName: "Safari",
            currentBundleId: "com.lokii.app"
        ),
        false,
        "被系统复用给无关应用的 PID 不得被误唤醒"
    )
    assertEqual(
        SingleInstanceGuard.matchesExistingInstance(
            candidateBundleId: nil,
            candidateName: "Lokii",
            currentBundleId: nil
        ),
        true,
        "开发态无 bundleId 时仅允许同名 Lokii 进程"
    )
    assertEqual(
        SingleInstanceGuard.matchesExistingInstance(
            candidateBundleId: nil,
            candidateName: "OtherTool",
            currentBundleId: nil
        ),
        false,
        "开发态无 bundleId 时不得唤醒其他同名缺失进程"
    )

    print("IndexConfig 编解码、卷状态解析与通知测试全部通过！")
} catch {
    fputs("测试执行抛出异常: \\(error)\\n", stderr)
    exit(1)
}
"""

let indexPanePath = "Lokii/Lokii/Preferences/PreferencesIndexPane.swift"
guard let indexPaneSource = try? String(contentsOfFile: indexPanePath, encoding: .utf8),
      let rowStart = indexPaneSource.range(of: "private struct SettingsPathRow: View {"),
      let viewStart = indexPaneSource.range(of: "// MARK: - Native SwiftUI Input Sheet") else {
    fputs("tests/index_config_test.swift: error: 无法定位 SettingsPathRow 代码片段\n", stderr)
    exit(1)
}
let rowBody = String(indexPaneSource[rowStart.lowerBound..<viewStart.lowerBound])
guard !rowBody.contains("FileManager.default.fileExists") else {
    fputs("断言失败: SettingsPathRow 视图渲染内禁止同步调用 FileManager.default.fileExists\n", stderr)
    exit(1)
}
guard let managedRowStart = indexPaneSource.range(of: "private struct ManagedVolumeRow: View {"),
      let paneStart = indexPaneSource.range(of: "// MARK: - Index Preferences Pane") else {
    fputs("tests/index_config_test.swift: error: 无法定位 ManagedVolumeRow 代码片段\n", stderr)
    exit(1)
}
let managedRowBody = String(indexPaneSource[managedRowStart.lowerBound..<paneStart.lowerBound])
guard !managedRowBody.contains("prefs.volume.status.offlineNoCount") else {
    fputs("断言失败: ManagedVolumeRow 在 entryCount == 0 时必须展示 offlineUncached 而非 offlineNoCount\n", stderr)
    exit(1)
}

for requiredSymbol in [
    "peekStartupIsColdStart",
    "didMountNotification",
    "willUnmountNotification",
    "didUnmountNotification",
    "func onVolumeStatusesChanged()",
    "volumeEventQueue",
    "statusRefreshQueue",
    "statusSequence",
    "catch let FfiError.IoError(message)"
] {
    guard searchEngineSource.contains(requiredSymbol) else {
        fputs("断言失败: SearchEngine.swift 缺少 \(requiredSymbol)\n", stderr)
        exit(1)
    }
}
for requiredPrefsSymbol in [
    "prefs.volume.clearCacheConfirm.title",
    "prefs.volume.clearCacheConfirm.message",
    "prefs.volume.clearCacheConfirm.confirm",
    "prefs.volume.state.offlineUncached",
    "prefs.index.group.volumes",
    "prefs.volume.includeOffline"
] {
    guard indexPaneSource.contains(requiredPrefsSymbol) else {
        fputs("断言失败: PreferencesIndexPane.swift 缺少 \(requiredPrefsSymbol)\n", stderr)
        exit(1)
    }
}
for requiredViewModelSymbol in [
    "import DiskArbitration",
    "import IOKit",
    "probeIsDiskImage",
    "where !mounted.isDiskImage"
] {
    guard preferencesSource.contains(requiredViewModelSymbol) else {
        fputs("断言失败: PreferencesViewModel.swift 缺少 \(requiredViewModelSymbol)\n", stderr)
        exit(1)
    }
}

guard let searchWindowSource = try? String(contentsOfFile: "Lokii/Lokii/SearchWindow.swift", encoding: .utf8) else {
    fputs("tests/index_config_test.swift: error: 无法读取 SearchWindow.swift\n", stderr)
    exit(1)
}
for requiredWindowSymbol in [
    "isPathOfflineHandler",
    "volumeStatusesChangedNotification",
    "openResultWithFeedback",
    "refreshOfflineStates",
    "FilterQuerySyncCoordinator.deserialize(query: searchField.stringValue)",
    "!window.isZoomed",
    "styleMask.contains(.fullScreen)",
    "isApplyingIdleEmptyState",
    "CGEventSource.buttonState(.combinedSessionState, button: .left)",
    "clampedFrameForScreenOverflow",
    "resetScreenOverflowDebounce",
    "!window.isOnActiveSpace"
] {
    guard searchWindowSource.contains(requiredWindowSymbol) else {
        fputs("断言失败: SearchWindow.swift 缺少 \(requiredWindowSymbol)\n", stderr)
        exit(1)
    }
}
for forbiddenWindowSymbol in [
    "NSEvent.pressedMouseButtons",
    "titlebarAppearsTransparent = true",
    "collectionBehavior = [.moveToActiveSpace]"
] {
    guard !searchWindowSource.contains(forbiddenWindowSymbol) else {
        fputs("断言失败: SearchWindow.swift 不得包含 \(forbiddenWindowSymbol)（会触发跨屏拖拽冲突）\n", stderr)
        exit(1)
    }
}

guard let resultsVCSource = try? String(contentsOfFile: "Lokii/Lokii/SearchResultsViewController.swift", encoding: .utf8) else {
    fputs("tests/index_config_test.swift: error: 无法读取 SearchResultsViewController.swift\n", stderr)
    exit(1)
}
for requiredVCSymbol in [
    "func setOfflineItems(",
    "requireAllOnline",
    "areAllContextualTargetsOnline",
    "func refreshOfflineStates()"
] {
    guard resultsVCSource.contains(requiredVCSymbol) else {
        fputs("断言失败: SearchResultsViewController.swift 缺少 \(requiredVCSymbol)\n", stderr)
        exit(1)
    }
}
print("[7/7] 静态守卫检查通过：SettingsPathRow 无同步 fileExists，SearchEngine/SearchWindow/BreadcrumbPathControl 接线完整。")

let tempDir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
try! FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
defer { try? FileManager.default.removeItem(at: tempDir) }

let harnessFile = tempDir.appendingPathComponent("main.swift")
try! runnerSource.write(to: harnessFile, atomically: true, encoding: .utf8)

let process = Process()
process.executableURL = URL(fileURLWithPath: "/usr/bin/swift")
process.arguments = [harnessFile.path]
process.standardOutput = FileHandle.standardOutput
process.standardError = FileHandle.standardError

try! process.run()
process.waitUntilExit()

if process.terminationStatus != 0 {
    exit(process.terminationStatus)
}
