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

    print("[2/3] 测试 IndexConfig 缺少 exclude_paths 时的优雅回退（默认空数组）...")
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
    assertEqual(config2.excludePaths, [], "缺少 exclude_paths 时回退为空数组")
    assertTrue(config2.includeHidden, "includeHidden 为 true")
    assertTrue(config2.excludeSensitive, "缺少 exclude_sensitive 时默认保护凭据目录")
    assertEqual(config2.sensitiveNames, [], "缺少 sensitive_names 时回退为空数组")
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

guard let searchWindowSource = try? String(contentsOfFile: "Lokii/Lokii/SearchWindow.swift", encoding: .utf8) else {
    fputs("tests/index_config_test.swift: error: 无法读取 SearchWindow.swift\n", stderr)
    exit(1)
}
for requiredWindowSymbol in [
    "isPathOfflineHandler",
    "volumeStatusesChangedNotification",
    "openResultWithFeedback",
    "refreshOfflineStates"
] {
    guard searchWindowSource.contains(requiredWindowSymbol) else {
        fputs("断言失败: SearchWindow.swift 缺少 \(requiredWindowSymbol)\n", stderr)
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
