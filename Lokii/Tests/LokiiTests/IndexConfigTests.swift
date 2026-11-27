import Cocoa
import Darwin
import XCTest
@testable import LokiiAppLib

final class IndexConfigTests: XCTestCase {
    func testIndexConfigSerializationAndDefaults() throws {
        let decoder = JSONDecoder()

        let jsonWithExcludePaths = """
        {
            "index_paths": ["~", "/Volumes/Data"],
            "index_apps": true,
            "exclude": ["node_modules", ".git"],
            "exclude_paths": ["~/Library", "/System"],
            "include_hidden": false,
            "sensitive_names": [".ssh", ".vault"],
            "max_results": 200,
            "network_rescan_minutes": 15,
            "serial_scan_paths": ["/Volumes/ArchiveHDD"]
        }
        """.data(using: .utf8)!

        let config1 = try decoder.decode(LokiiConfig.IndexConfig.self, from: jsonWithExcludePaths)
        XCTAssertEqual(config1.indexPaths, ["~", "/Volumes/Data"])
        XCTAssertTrue(config1.indexApps)
        XCTAssertEqual(config1.exclude, ["node_modules", ".git"])
        XCTAssertEqual(config1.excludePaths, ["~/Library", "/System"])
        XCTAssertEqual(config1.sensitiveNames, [".ssh", ".vault"])
        XCTAssertEqual(config1.maxResults, 200)
        XCTAssertEqual(config1.networkRescanMinutes, 15)
        XCTAssertEqual(config1.serialScanPaths, ["/Volumes/ArchiveHDD"])

        let jsonWithoutExcludePaths = """
        {
            "index_paths": ["~"],
            "exclude": ["target"],
            "include_hidden": true,
            "max_results": 500
        }
        """.data(using: .utf8)!

        let config2 = try decoder.decode(LokiiConfig.IndexConfig.self, from: jsonWithoutExcludePaths)
        XCTAssertEqual(config2.indexPaths, ["~"])
        XCTAssertFalse(config2.indexApps)
        XCTAssertEqual(config2.exclude, ["target"])
        XCTAssertEqual(config2.excludePaths, LokiiConfig.IndexConfig.defaultExcludePaths)
        XCTAssertTrue(config2.includeHidden)
        XCTAssertTrue(config2.excludeSensitive)
        XCTAssertEqual(config2.sensitiveNames, LokiiConfig.IndexConfig.defaultSensitiveNames)
        XCTAssertEqual(config2.networkRescanMinutes, 30)

        let encodedData = try JSONEncoder().encode(config1)
        let encodedJSON = String(data: encodedData, encoding: .utf8)!
        XCTAssertTrue(encodedJSON.contains("\"exclude_paths\":"))
        XCTAssertTrue(encodedJSON.contains("\"network_rescan_minutes\":15"))
        XCTAssertTrue(encodedJSON.contains("\"serial_scan_paths\":"))
        XCTAssertEqual(config2.serialScanPaths, [])

        let emptyData = "{}".data(using: .utf8)!
        let decodedIndex = try decoder.decode(LokiiConfig.IndexConfig.self, from: emptyData)
        let defaultIndex = LokiiConfig.IndexConfig()
        XCTAssertEqual(decodedIndex.indexPaths, ["~"])
        XCTAssertEqual(defaultIndex.indexPaths, ["~"])
        XCTAssertEqual(decodedIndex.exclude, LokiiConfig.IndexConfig.defaultExclude)
        XCTAssertEqual(decodedIndex.exclude.count, 35)
        XCTAssertEqual(decodedIndex.excludePaths, LokiiConfig.IndexConfig.defaultExcludePaths)
        XCTAssertEqual(decodedIndex.excludePaths.count, 25)
        XCTAssertEqual(decodedIndex.sensitiveNames, LokiiConfig.IndexConfig.defaultSensitiveNames)
        XCTAssertEqual(decodedIndex.sensitiveNames.count, 12)
        XCTAssertFalse(decodedIndex.includeHidden)
        XCTAssertFalse(defaultIndex.includeHidden)
        XCTAssertEqual(decodedIndex.maxResults, 500)
        XCTAssertEqual(defaultIndex.maxResults, 500)
        XCTAssertTrue(decodedIndex.excludeSensitive)
        XCTAssertEqual(decodedIndex.networkRescanMinutes, 30)
        XCTAssertEqual(PreferencesViewModel.defaultIndexPaths, LokiiConfig.IndexConfig.defaultIndexPaths)
        XCTAssertEqual(PreferencesViewModel.defaultExcludePatterns, LokiiConfig.IndexConfig.defaultExclude)
        XCTAssertEqual(PreferencesViewModel.defaultExcludePaths, LokiiConfig.IndexConfig.defaultExcludePaths)
        XCTAssertEqual(PreferencesViewModel.defaultNetworkRescanMinutes, LokiiConfig.IndexConfig.defaultNetworkRescanMinutes)

        let decodedWindow = try decoder.decode(LokiiConfig.WindowConfig.self, from: emptyData)
        let defaultWindow = LokiiConfig.WindowConfig()
        XCTAssertTrue(decodedWindow.hideOnBlur)
        XCTAssertTrue(defaultWindow.hideOnBlur)
        XCTAssertFalse(decodedWindow.rememberPosition)
        XCTAssertFalse(defaultWindow.rememberPosition)

        XCTAssertTrue(LokiiConfig.GeneralConfig().hideResultsWhenEmpty)
        XCTAssertEqual(LokiiConfig.GeneralConfig().dockIconMode, .whenOpen)
        let legacyGeneralData = """
        {"language": "zh-Hans", "show_in_dock": true}
        """.data(using: .utf8)!
        let decodedLegacyGeneral = try decoder.decode(LokiiConfig.GeneralConfig.self, from: legacyGeneralData)
        XCTAssertTrue(decodedLegacyGeneral.hideResultsWhenEmpty)
        XCTAssertEqual(decodedLegacyGeneral.dockIconMode, .always)

        let disabledGeneralData = """
        {"language": "en", "dock_icon_mode": "when_open", "hide_results_when_empty": false}
        """.data(using: .utf8)!
        let decodedDisabledGeneral = try decoder.decode(LokiiConfig.GeneralConfig.self, from: disabledGeneralData)
        XCTAssertFalse(decodedDisabledGeneral.hideResultsWhenEmpty)
        XCTAssertEqual(decodedDisabledGeneral.dockIconMode, .whenOpen)
        let encodedGeneral = String(data: try JSONEncoder().encode(decodedDisabledGeneral), encoding: .utf8) ?? ""
        XCTAssertTrue(encodedGeneral.contains("\"hide_results_when_empty\":false"))
        XCTAssertTrue(encodedGeneral.contains("\"dock_icon_mode\":\"when_open\""))
        XCTAssertFalse(encodedGeneral.contains("show_in_dock"))
    }

    func testSettingsPathStatusResolverAndVolumeLifecycle() {
        let offlineNas = TestFixture.volumeStatus(
            configuredPath: "/Volumes/NAS",
            activePath: nil,
            displayName: "NAS",
            kind: "network",
            state: .offline,
            entryCount: 4200
        )
        XCTAssertEqual(
            SettingsPathStatusResolver.resolve(path: "/Volumes/NAS", volumeStatuses: [offlineNas], localAvailabilityCache: [:]),
            .volumeOffline(entryCount: 4200, kind: "network")
        )

        let missingLocal = "/tmp/nonexistent_lokii_dir"
        XCTAssertEqual(
            SettingsPathStatusResolver.resolve(path: missingLocal, volumeStatuses: [offlineNas], localAvailabilityCache: [:]),
            .localChecking
        )
        XCTAssertEqual(
            SettingsPathStatusResolver.resolve(path: missingLocal, volumeStatuses: [offlineNas], localAvailabilityCache: [missingLocal: false]),
            .localMissing
        )

        let localOk = "/tmp/existing_lokii_dir"
        XCTAssertEqual(
            SettingsPathStatusResolver.resolve(path: localOk, volumeStatuses: [offlineNas], localAvailabilityCache: [localOk: true]),
            .localAvailable
        )

        let remountedVol = TestFixture.volumeStatus(
            configuredPath: "/Volumes/Media",
            activePath: "/Volumes/Media-1",
            displayName: "Media",
            kind: "local_external",
            state: .online,
            entryCount: 980,
            isRemounted: true
        )
        let syncingVol = TestFixture.volumeStatus(
            configuredPath: "/Volumes/Work",
            activePath: "/Volumes/Work",
            displayName: "Work",
            kind: "local_external",
            state: .syncing,
            entryCount: 100,
            scannedCount: 450
        )
        let confirmVol = TestFixture.volumeStatus(
            volumeId: "vol-confirm",
            configuredPath: "/Volumes/Archive",
            activePath: "/Volumes/Archive-1",
            displayName: "Archive",
            kind: "local_external",
            state: .needsConfirmation,
            entryCount: 2500,
            isRemounted: true,
            hasCache: true,
            confirmReason: .filterWidened
        )
        XCTAssertEqual(
            SettingsPathStatusResolver.resolve(path: "/Volumes/Media", volumeStatuses: [remountedVol], localAvailabilityCache: [:]),
            .volumeOnline(entryCount: 980, activePath: "/Volumes/Media-1", isRemounted: true, kind: "local_external", supportsPersistentJournal: true)
        )
        XCTAssertEqual(
            SettingsPathStatusResolver.resolve(path: "/Volumes/Media-1", volumeStatuses: [remountedVol], localAvailabilityCache: [:]),
            .volumeOnline(entryCount: 980, activePath: "/Volumes/Media-1", isRemounted: true, kind: "local_external", supportsPersistentJournal: true)
        )
        XCTAssertEqual(
            SettingsPathStatusResolver.resolve(path: "/Volumes/Work", volumeStatuses: [syncingVol], localAvailabilityCache: [:]),
            .volumeSyncing(kind: "local_external", scannedCount: 450)
        )
        XCTAssertEqual(
            SettingsPathStatusResolver.resolve(path: "/Volumes/Archive", volumeStatuses: [confirmVol], localAvailabilityCache: [:]),
            .volumeNeedsConfirmation(
                entryCount: 2500,
                activePath: "/Volumes/Archive-1",
                isRemounted: true,
                kind: "local_external",
                confirmReason: .filterWidened
            )
        )

        let diskSlot = TestFixture.volumeStatus(configuredPath: "/Volumes/Disk/", activePath: nil, displayName: "Disk", kind: "local_external", state: .offline, entryCount: 50)
        let disk2Slot = TestFixture.volumeStatus(configuredPath: "/Volumes/Disk2", activePath: "/Volumes/Disk2", displayName: "Disk2", kind: "local_external", state: .online, entryCount: 120)
        let nestedSlot = TestFixture.volumeStatus(configuredPath: "/Volumes/Disk/Sub", activePath: "/Volumes/Disk/Sub", displayName: "DiskSub", kind: "local_external", state: .online, entryCount: 10)
        let homeSlot = TestFixture.volumeStatus(configuredPath: "~/ExternalLink", activePath: nil, displayName: "HomeMount", kind: "local_external", state: .offline, entryCount: 5)
        let allVols = [diskSlot, disk2Slot, nestedSlot, homeSlot]

        XCTAssertEqual(
            SettingsPathStatusResolver.resolve(path: "/Volumes/Disk2/file.txt", volumeStatuses: allVols, localAvailabilityCache: [:]),
            .volumeOnline(entryCount: 120, activePath: "/Volumes/Disk2", isRemounted: false, kind: "local_external")
        )
        XCTAssertEqual(
            SettingsPathStatusResolver.resolve(path: "/volumes/disk/sub/deep.log", volumeStatuses: allVols, localAvailabilityCache: [:]),
            .volumeOnline(entryCount: 10, activePath: "/Volumes/Disk/Sub", isRemounted: false, kind: "local_external")
        )
        XCTAssertEqual(
            SettingsPathStatusResolver.resolve(path: "/volumes/disk/other/doc.txt", volumeStatuses: allVols, localAvailabilityCache: [:]),
            .volumeOffline(entryCount: 50, kind: "local_external")
        )
        let expandedHomeTarget = ("~/ExternalLink/item" as NSString).expandingTildeInPath
        XCTAssertEqual(
            SettingsPathStatusResolver.resolve(path: expandedHomeTarget, volumeStatuses: allVols, localAvailabilityCache: [:]),
            .volumeOffline(entryCount: 5, kind: "local_external")
        )

        let uncachedOfflineSlot = TestFixture.volumeStatus(
            configuredPath: "/Volumes/Home1",
            activePath: nil,
            displayName: "Home1",
            kind: "network",
            state: .offline,
            entryCount: 0,
            lastSyncAt: nil
        )
        XCTAssertEqual(
            SettingsPathStatusResolver.resolve(path: "/Volumes/Home1", volumeStatuses: [uncachedOfflineSlot], localAvailabilityCache: [:]),
            .volumeUncachedOffline
        )
        XCTAssertEqual(
            SettingsPathStatusResolver.resolve(path: "/Volumes/ExtDrive", volumeStatuses: [], localAvailabilityCache: ["/Volumes/ExtDrive": false]),
            .volumeUncachedOffline
        )

        let mountedMissingSubpathSlot = TestFixture.volumeStatus(
            configuredPath: "/Volumes/NAS/MissingSub",
            activePath: "/Volumes/NAS/MissingSub",
            displayName: "NAS",
            kind: "network",
            state: .missing,
            entryCount: 0
        )
        XCTAssertEqual(
            SettingsPathStatusResolver.resolve(path: "/Volumes/NAS/MissingSub", volumeStatuses: [mountedMissingSubpathSlot], localAvailabilityCache: [:]),
            .localMissing
        )
        XCTAssertTrue(SettingsPathStatusResolver.isUnderVolumesNamespace("/Volumes/ExtDrive/Sub"))
        XCTAssertFalse(SettingsPathStatusResolver.isUnderVolumesNamespace("/Volumes"))

        let conflictHome = NSHomeDirectory()
        XCTAssertTrue(SettingsPathStatusResolver.containsSamePath(["~/Library/"], conflictHome + "/library"))
        XCTAssertFalse(SettingsPathStatusResolver.containsSamePath(["~/Library"], "~/Library/xxx"))
        XCTAssertFalse(SettingsPathStatusResolver.containsSamePath(["~/Library/xxx"], "~/Library"))
        XCTAssertFalse(SettingsPathStatusResolver.containsSamePath(["~/Library", "/opt"], "~/LibraryX"))

        var updatedStatus = disk2Slot
        updatedStatus.entryCount += 1
        XCTAssertNotEqual(updatedStatus, disk2Slot)
        XCTAssertTrue(updatedStatus.hasSameLifecycle(as: disk2Slot))
        updatedStatus.scannedCount = 12_345
        XCTAssertNotEqual(updatedStatus, disk2Slot)
        XCTAssertTrue(
            updatedStatus.hasSameLifecycle(as: disk2Slot),
            "150ms 节流推送的 scannedCount 变化不得触发 volumeStatusesChangedNotification 生命周期变更"
        )
        updatedStatus.state = .syncing
        XCTAssertFalse(updatedStatus.hasSameLifecycle(as: disk2Slot))
        updatedStatus = disk2Slot
        updatedStatus.state = .needsConfirmation
        updatedStatus.confirmReason = .filterWidened
        var upgradedReason = updatedStatus
        upgradedReason.confirmReason = .journalInvalidated
        XCTAssertFalse(
            upgradedReason.hasSameLifecycle(as: updatedStatus),
            "confirmReason 变化必须判定为生命周期变更"
        )
        updatedStatus = disk2Slot
        updatedStatus.activePath = "/Volumes/Disk2-1"
        XCTAssertFalse(updatedStatus.hasSameLifecycle(as: disk2Slot))
        updatedStatus = disk2Slot
        updatedStatus.lastSyncAt = 123
        XCTAssertFalse(updatedStatus.hasSameLifecycle(as: disk2Slot))

        let batchMatcher = SettingsPathStatusResolver.VolumeStatusMatcher(volumeStatuses: allVols)
        XCTAssertEqual(batchMatcher.match(path: "/Volumes/Disk2/file.txt")?.displayName, "Disk2")
        XCTAssertEqual(batchMatcher.match(path: "/volumes/disk/sub/deep.log")?.displayName, "DiskSub")
        XCTAssertEqual(batchMatcher.match(path: expandedHomeTarget)?.displayName, "HomeMount")
        XCTAssertNil(batchMatcher.match(path: "/Users/local/file.txt"))
    }

    func testPendingSaveRequiresRebuildDelegatesToRustAndSingleDriverContract() throws {
        let engine = SearchEngine()
        let viewModel = PreferencesViewModel(engine: engine)
        let saved = try XCTUnwrap(engine.getConfig())

        // 1. 未修改时无需重建
        XCTAssertFalse(viewModel.pendingSaveRequiresRebuild)

        // 2. 纯收窄（新增排除路径）-> Rust 返回 PruneOnly -> pendingSaveRequiresRebuild == false
        viewModel.excludePaths.append("/tmp/__lokii_swift_test_narrow__")
        XCTAssertEqual(engine.previewPrimaryScopeAction(viewModel.draftIndexConfig(base: saved)), .pruneOnly)
        XCTAssertFalse(viewModel.pendingSaveRequiresRebuild)
        viewModel.discardIndexChanges()

        // 3. 新增未覆盖且真实存在的本地根 -> Rust 返回 IncrementalUpdate -> pendingSaveRequiresRebuild == false
        // 注意：Foundation 的 URL.resolvingSymlinksInPath() 会把 /private/var 规约回 /var；
        // 这里显式保留 /private/var 前缀，确保无论本机配置索引 "~" 还是 "/"（默认排除 /private），该目录都未被旧范围覆盖。
        let rawTempDir = FileManager.default.temporaryDirectory.path
        let privateTempBase = rawTempDir.hasPrefix("/var/") ? "/private" + rawTempDir : rawTempDir
        let addedRootPath = (privateTempBase as NSString).appendingPathComponent("lokii-preview-root-\(UUID().uuidString)")
        let addedRootURL = URL(fileURLWithPath: addedRootPath, isDirectory: true)
        try FileManager.default.createDirectory(at: addedRootURL, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: addedRootURL) }
        viewModel.indexPaths.append(addedRootPath)
        XCTAssertEqual(engine.previewPrimaryScopeAction(viewModel.draftIndexConfig(base: saved)), .incrementalUpdate)
        XCTAssertFalse(viewModel.pendingSaveRequiresRebuild)
        viewModel.discardIndexChanges()

        // 4. 宽化（开启 includeHidden，或移除已有排除规则/关闭 excludeSensitive）-> FullRebuild -> true
        if !viewModel.includeHidden {
            viewModel.includeHidden = true
        } else if !viewModel.excludePatterns.isEmpty {
            viewModel.excludePatterns.removeFirst()
        } else if viewModel.excludeSensitive {
            viewModel.excludeSensitive = false
        }
        XCTAssertEqual(engine.previewPrimaryScopeAction(viewModel.draftIndexConfig(base: saved)), .fullRebuild)
        XCTAssertTrue(viewModel.pendingSaveRequiresRebuild)
        viewModel.discardIndexChanges()

        // 5. 验证 startWatcher 专用 handler：source == "rebuild"，同时保留 onVolumeChanged 闭包并派发重建通知
        let volumeExp = expectation(description: "watcher handler preserves onVolumeChanged callback")
        volumeExp.expectedFulfillmentCount = 2 // onVolumeStatusesChanged + onIndexReady
        let progressExp = expectation(description: "watcher handler posts rebuild progress")
        let readyExp = expectation(description: "watcher handler posts rebuild ready")

        let watcherHandler = SearchEngine.makeWatcherEventHandler {
            volumeExp.fulfill()
        }
        XCTAssertEqual(watcherHandler.source, "rebuild")

        let progressObs = NotificationCenter.default.addObserver(
            forName: SearchEngine.indexProgressNotification,
            object: nil,
            queue: .main
        ) { note in
            if note.userInfo?["source"] as? String == "rebuild",
               note.userInfo?["scannedFiles"] as? UInt64 == 321 {
                progressExp.fulfill()
            }
        }
        let readyObs = NotificationCenter.default.addObserver(
            forName: SearchEngine.indexReadyNotification,
            object: nil,
            queue: .main
        ) { note in
            if note.userInfo?["source"] as? String == "rebuild",
               note.userInfo?["total"] as? UInt64 == 654 {
                readyExp.fulfill()
            }
        }
        defer {
            NotificationCenter.default.removeObserver(progressObs)
            NotificationCenter.default.removeObserver(readyObs)
        }

        watcherHandler.onVolumeStatusesChanged()
        watcherHandler.onIndexProgress(phase: "rebuilding", scannedFiles: 321)
        watcherHandler.onIndexReady(total: 654)
        wait(for: [volumeExp, progressExp, readyExp], timeout: 2.0)

        // 6. 静态契约：saveIndexChanges 单一路径仅调 updateConfig；startWatcher/startConfigWatcher 分离 handler
        let vmSource = try TestFixture.readRepoFile("Lokii/Lokii/Preferences/PreferencesViewModel.swift")
        let saveStart = try XCTUnwrap(vmSource.range(of: "func saveIndexChanges() -> Bool {"))
        let discardStart = try XCTUnwrap(vmSource.range(of: "func discardIndexChanges() {"))
        let saveBody = String(vmSource[saveStart.lowerBound..<discardStart.lowerBound])
        XCTAssertTrue(saveBody.contains("engine.updateConfig("))
        XCTAssertFalse(saveBody.contains("engine.rebuildIndex("), "saveIndexChanges 不得额外调用 rebuildIndex 造成双扫")
        XCTAssertFalse(saveBody.contains("engine.rebuildPrimaryIndex("), "saveIndexChanges 不得额外调用 rebuildPrimaryIndex 造成双扫")

        let engineSource = try TestFixture.readRepoFile("Lokii/Lokii/SearchEngine.swift")
        XCTAssertTrue(engineSource.contains("core.startWatcher(eventHandler: watcherHandler)"))
        XCTAssertTrue(engineSource.contains("core.startConfigWatcher(eventHandler: initHandler)"))
    }

    func testVolumeBannerResolverStateMachineAndCopy() throws {
        let zhURL = TestFixture.repoFileURL("Lokii/Lokii/Resources/zh-Hans.lproj/Localizable.strings")
        let enURL = TestFixture.repoFileURL("Lokii/Lokii/Resources/en.lproj/Localizable.strings")
        let zhDict = try XCTUnwrap(NSDictionary(contentsOf: zhURL) as? [String: String])
        let enDict = try XCTUnwrap(NSDictionary(contentsOf: enURL) as? [String: String])
        let localizeZh: (String) -> String = { key in zhDict[key] ?? key }
        let localizeEn: (String) -> String = { key in enDict[key] ?? key }

        let requiredKeys = [
            "volume.banner.syncing.single",
            "volume.banner.syncing.singleNoCount",
            "volume.banner.syncing.multi",
            "volume.banner.syncing.multiNoCount",
            "volume.banner.confirm.initialScan",
            "volume.banner.confirm.filterWidened",
            "volume.banner.confirm.journalInvalidated",
            "volume.banner.confirm.schemaUpgrade",
            "volume.banner.confirm.multi",
            "volume.banner.action.updateNow",
            "volume.banner.action.cancel",
            "volume.banner.action.manage",
            "volume.banner.action.later",
            "prefs.volume.status.scanning",
            "prefs.volume.status.syncingWithCount",
            "prefs.volume.status.needsConfirmation",
            "prefs.volume.updateIndex",
            "prefs.volume.cancelScan",
            "prefs.volume.nonPersistentJournalHint",
            "rebuild.scopeConfirm.title",
            "rebuild.scopeConfirm.message",
            "rebuild.scopeConfirm.primaryOnly",
            "rebuild.scopeConfirm.all",
            "rebuild.scopeConfirm.cancel",
        ]
        for key in requiredKeys {
            XCTAssertNotNil(zhDict[key], "zh-Hans 缺少本地化 key: \(key)")
            XCTAssertNotNil(enDict[key], "en 缺少本地化 key: \(key)")
        }

        let onlineVol = TestFixture.volumeStatus(
            volumeId: "vol-online",
            configuredPath: "/Volumes/Online",
            activePath: "/Volumes/Online",
            displayName: "OnlineDisk",
            kind: "local_external",
            state: .online,
            entryCount: 100
        )
        let pendingA = TestFixture.volumeStatus(
            volumeId: "vol-a",
            configuredPath: "/Volumes/Archive",
            activePath: "/Volumes/Archive",
            displayName: "Archive22TB",
            kind: "local_external",
            state: .needsConfirmation,
            entryCount: 5000,
            hasCache: true,
            confirmReason: .filterWidened
        )
        let pendingB = TestFixture.volumeStatus(
            volumeId: "vol-b",
            configuredPath: "/Volumes/Backup",
            activePath: "/Volumes/Backup",
            displayName: "BackupDrive",
            kind: "local_external",
            state: .needsConfirmation,
            entryCount: 0,
            hasCache: false,
            confirmReason: .initialScan
        )
        let syncingA = TestFixture.volumeStatus(
            volumeId: "vol-a",
            configuredPath: "/Volumes/Archive",
            activePath: "/Volumes/Archive",
            displayName: "Archive22TB",
            kind: "local_external",
            state: .syncing,
            entryCount: 5000,
            hasCache: true,
            scannedCount: 12_345
        )
        let syncingB = TestFixture.volumeStatus(
            volumeId: "vol-b",
            configuredPath: "/Volumes/Backup",
            activePath: "/Volumes/Backup",
            displayName: "BackupDrive",
            kind: "local_external",
            state: .syncing,
            entryCount: 0,
            hasCache: false,
            scannedCount: 555
        )

        // 无待确认且无扫描中 -> 隐藏
        XCTAssertEqual(VolumeBannerResolver.resolve(statuses: [onlineVol], dismissedVolumeIds: []), .hidden)

        // 优先级 1：syncing 优先于 needsConfirmation，且正确汇总扫描数与待确认标记
        let syncingWithPending = VolumeBannerResolver.resolve(
            statuses: [onlineVol, pendingB, syncingA],
            dismissedVolumeIds: []
        )
        XCTAssertEqual(
            syncingWithPending,
            .syncing(volumes: [syncingA], totalScanned: 12_345, hasPendingConfirmations: true)
        )
        let singleSyncMsg = VolumeBannerResolver.message(for: syncingWithPending, localize: localizeZh)
        XCTAssertTrue(singleSyncMsg.contains("Archive22TB"))
        XCTAssertTrue(singleSyncMsg.contains(formatFileCount(12_345)))
        XCTAssertTrue(VolumeBannerResolver.message(for: syncingWithPending, localize: localizeEn).contains("Archive22TB"))

        let multiSync = VolumeBannerResolver.resolve(statuses: [syncingA, syncingB], dismissedVolumeIds: [])
        XCTAssertEqual(
            multiSync,
            .syncing(volumes: [syncingA, syncingB], totalScanned: 12_900, hasPendingConfirmations: false)
        )
        let multiSyncMsg = VolumeBannerResolver.message(for: multiSync, localize: localizeZh)
        XCTAssertTrue(multiSyncMsg.contains("Archive22TB"))
        XCTAssertTrue(multiSyncMsg.contains("2"))
        XCTAssertTrue(multiSyncMsg.contains(formatFileCount(12_900)))

        // 优先级 2：needsConfirmation 单卷覆盖全部四种原因
        for reason: FfiVolumeConfirmReason in [.initialScan, .filterWidened, .journalInvalidated, .schemaUpgrade] {
            var v = pendingA
            v.confirmReason = reason
            let state = VolumeBannerResolver.resolve(statuses: [v], dismissedVolumeIds: [])
            XCTAssertEqual(state, .needsConfirmation(volumes: [v]))
            let msgZh = VolumeBannerResolver.message(for: state, localize: localizeZh)
            let msgEn = VolumeBannerResolver.message(for: state, localize: localizeEn)
            XCTAssertTrue(msgZh.contains("Archive22TB"), "zh 原因 \(reason) 文案应包含卷名称: \(msgZh)")
            XCTAssertTrue(msgEn.contains("Archive22TB"), "en 原因 \(reason) 文案应包含卷名称: \(msgEn)")
        }

        // 点「稍后再说」按 volumeId 集合静默收起
        XCTAssertEqual(
            VolumeBannerResolver.resolve(statuses: [pendingA], dismissedVolumeIds: ["vol-a"]),
            .hidden
        )

        // 新增另一块待确认卷时横幅重新弹出，并展示多卷合并文案
        let reShown = VolumeBannerResolver.resolve(
            statuses: [pendingA, pendingB],
            dismissedVolumeIds: ["vol-a"]
        )
        XCTAssertEqual(reShown, .needsConfirmation(volumes: [pendingA, pendingB]))
        let multiConfirmMsg = VolumeBannerResolver.message(for: reShown, localize: localizeZh)
        XCTAssertTrue(multiConfirmMsg.contains("Archive22TB"))
        XCTAssertTrue(multiConfirmMsg.contains("2"))

        // 取消扫描后状态从 syncing 回退到 needsConfirmation(initialScan)
        let postCancel = VolumeBannerResolver.resolve(statuses: [pendingB], dismissedVolumeIds: [])
        XCTAssertEqual(postCancel, .needsConfirmation(volumes: [pendingB]))
        XCTAssertTrue(VolumeBannerResolver.message(for: postCancel, localize: localizeZh).contains("BackupDrive"))
    }

    func testManualRebuildScopeDecision() {
        let offline = TestFixture.volumeStatus(
            volumeId: "v-off",
            configuredPath: "/Volumes/Off",
            displayName: "Off",
            kind: "local_external",
            state: .offline,
            entryCount: 100
        )
        let missing = TestFixture.volumeStatus(
            volumeId: "v-miss",
            configuredPath: "/Volumes/Miss",
            displayName: "Miss",
            kind: "network",
            state: .missing
        )
        let syncing = TestFixture.volumeStatus(
            volumeId: "v-sync",
            configuredPath: "/Volumes/Sync",
            displayName: "Sync",
            kind: "local_external",
            state: .syncing
        )
        let online = TestFixture.volumeStatus(
            volumeId: "v-on",
            configuredPath: "/Volumes/On",
            displayName: "OnDrive",
            kind: "local_external",
            state: .online,
            entryCount: 500
        )
        let confirm = TestFixture.volumeStatus(
            volumeId: "v-conf",
            configuredPath: "/Volumes/Conf",
            displayName: "ConfDrive",
            kind: "local_external",
            state: .needsConfirmation,
            entryCount: 800,
            confirmReason: .filterWidened
        )

        // 无在线/待确认外置卷时跳过弹窗，直接走仅重建本地索引回调
        XCTAssertFalse(ManualRebuildCoordinator.requiresScopeConfirmation(for: []))
        XCTAssertFalse(ManualRebuildCoordinator.requiresScopeConfirmation(for: [offline, missing, syncing]))
        var primaryOnlyCalled = false
        var rebuildAllCalled = false
        ManualRebuildCoordinator.requestRebuild(
            window: nil,
            statuses: [offline, missing, syncing],
            onPrimaryOnly: { primaryOnlyCalled = true },
            onRebuildAll: { rebuildAllCalled = true }
        )
        XCTAssertTrue(primaryOnlyCalled)
        XCTAssertFalse(rebuildAllCalled)

        // 存在 .online 或 .needsConfirmation 外部卷时需弹出双模式确认框
        XCTAssertTrue(ManualRebuildCoordinator.requiresScopeConfirmation(for: [offline, online]))
        XCTAssertTrue(ManualRebuildCoordinator.requiresScopeConfirmation(for: [offline, confirm]))
        XCTAssertEqual(
            ManualRebuildCoordinator.activeExternalVolumes(in: [offline, missing, syncing, online, confirm]).map(\.volumeId),
            ["v-on", "v-conf"]
        )
    }

    func testAppIndexConflictedMatchesExactRootsOnly() {
        let defaultExcludes = LokiiConfig.IndexConfig.defaultExcludePaths
        XCTAssertTrue(defaultExcludes.contains("/System"), "默认排除路径应包含 /System")

        // 默认排除 /System 与名称排除 Applications 均不误报冲突
        XCTAssertFalse(
            PreferencesViewModel.isAppIndexConflicted(indexApps: true, excludePaths: defaultExcludes)
        )
        XCTAssertFalse(
            PreferencesViewModel.isAppIndexConflicted(indexApps: false, excludePaths: ["/Applications"])
        )
        XCTAssertFalse(
            PreferencesViewModel.isAppIndexConflicted(indexApps: true, excludePaths: ["/Applications/Utilities"])
        )

        // 仅精确同路径命中自动应用根（大小写无关、忽略尾随斜杠）时才返回 true
        XCTAssertTrue(
            PreferencesViewModel.isAppIndexConflicted(indexApps: true, excludePaths: defaultExcludes + ["/Applications"])
        )
        XCTAssertTrue(
            PreferencesViewModel.isAppIndexConflicted(indexApps: true, excludePaths: ["/applications/"])
        )
        XCTAssertTrue(
            PreferencesViewModel.isAppIndexConflicted(indexApps: true, excludePaths: ["/System/Applications/"])
        )

        // ViewModel 实例层面确认 excludePatterns 含 "Applications" 不会误报冲突
        let engine = SearchEngine()
        let vm = PreferencesViewModel(engine: engine)
        vm.indexApps = true
        vm.excludePaths = defaultExcludes
        vm.excludePatterns = ["Applications"]
        XCTAssertFalse(vm.appIndexConflicted)
        vm.excludePaths.append("/Applications")
        XCTAssertTrue(vm.appIndexConflicted)
    }

    func testDiskImageDetection() {

        XCTAssertTrue(
            SettingsPathStatusResolver.isDiskImageVolume(
                ancestorIOKitClasses: ["AppleDiskImageDevice", "AppleDiskImagesController"],
                deviceModel: "Disk Image",
                deviceProtocol: "Virtual Interface"
            )
        )
        XCTAssertTrue(
            SettingsPathStatusResolver.isDiskImageVolume(
                ancestorIOKitClasses: ["AppleAPFSVolume", "AppleAPFSContainer", "AppleAPFSMedia", "AppleDiskImageDevice", "AppleDiskImagesController"],
                deviceModel: "Disk Image",
                deviceProtocol: "Disk Image"
            )
        )
        XCTAssertTrue(
            SettingsPathStatusResolver.isDiskImageVolume(
                ancestorIOKitClasses: ["IOHDIXHDDriveOutKernel", "IOHDIXController"],
                deviceModel: nil,
                deviceProtocol: nil
            )
        )
        XCTAssertFalse(
            SettingsPathStatusResolver.isDiskImageVolume(
                ancestorIOKitClasses: ["IOSCSIPeripheralDeviceNub", "IOUSBMassStorageInterfaceNub", "IOUSBHostDevice"],
                deviceModel: "SanDisk Ultra",
                deviceProtocol: "USB"
            )
        )
        XCTAssertFalse(
            SettingsPathStatusResolver.isDiskImageVolume(
                ancestorIOKitClasses: ["AppleAPFSVolume", "AppleAPFSContainer", "IOUSBMassStorageDriver", "AppleUSBXHCI"],
                deviceModel: "Disk Image",
                deviceProtocol: "Virtual Interface"
            )
        )
        XCTAssertFalse(
            SettingsPathStatusResolver.isDiskImageVolume(
                ancestorIOKitClasses: ["AppleAPFSVolume", "AppleAPFSContainer", "IONVMeController", "IOThunderboltSwitch"],
                deviceModel: "Samsung SSD 980 PRO",
                deviceProtocol: "PCI-Express"
            )
        )
        XCTAssertFalse(
            SettingsPathStatusResolver.isDiskImageVolume(
                ancestorIOKitClasses: ["AppleSDXCBlockStorageDevice"],
                deviceModel: "SD Card Reader",
                deviceProtocol: "Secure Digital"
            )
        )
        XCTAssertTrue(
            SettingsPathStatusResolver.isDiskImageVolume(
                ancestorIOKitClasses: [],
                deviceModel: "Disk Image",
                deviceProtocol: "Virtual Interface"
            )
        )
        XCTAssertFalse(
            SettingsPathStatusResolver.isDiskImageVolume(
                ancestorIOKitClasses: [],
                deviceModel: "External SSD",
                deviceProtocol: "Virtual Interface"
            )
        )

        let dmgEntry = MountedVolumeEntry(path: "/Volumes/Lokii", displayName: "Lokii", kind: "local_external", isDiskImage: true)
        XCTAssertTrue(dmgEntry.isDiskImage)
        let usbEntry = MountedVolumeEntry(path: "/Volumes/MyUSB", displayName: "MyUSB", kind: "local_external")
        XCTAssertFalse(usbEntry.isDiskImage)
    }

    func testSingleInstanceGuardLockLifecycle() throws {
        let lockTmpDir = FileManager.default.temporaryDirectory.appendingPathComponent("lokii-lock-test-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: lockTmpDir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: lockTmpDir) }
        let testLockPath = lockTmpDir.appendingPathComponent("lokii.lock").path

        var heldGuard: SingleInstanceGuard?
        switch SingleInstanceGuard.tryAcquire(at: testLockPath) {
        case .acquired(let g):
            let fdFlags = Darwin.fcntl(g.fileDescriptor, F_GETFD)
            XCTAssertNotEqual(fdFlags & FD_CLOEXEC, 0, "单实例锁文件描述符必须设置 FD_CLOEXEC")
            heldGuard = g
        case .alreadyHeld, .ioError:
            XCTFail("首次获取测试单实例锁应成功")
        }

        withExtendedLifetime(heldGuard) {
            switch SingleInstanceGuard.tryAcquire(at: testLockPath) {
            case .alreadyHeld(let holderPid):
                XCTAssertEqual(holderPid, Darwin.getpid())
            case .acquired, .ioError:
                XCTFail("持有单实例锁期间二次 tryAcquire 必须返回 alreadyHeld")
            }
        }

        heldGuard = nil
        switch SingleInstanceGuard.tryAcquire(at: testLockPath) {
        case .acquired(let reacquired):
            _ = reacquired
        case .alreadyHeld, .ioError:
            XCTFail("释放锁句柄后应可重新获取单实例锁")
        }

        XCTAssertTrue(
            SingleInstanceGuard.matchesExistingInstance(
                candidateBundleId: "com.lokii.app",
                candidateName: "Lokii",
                currentBundleId: "com.lokii.app"
            )
        )
        XCTAssertFalse(
            SingleInstanceGuard.matchesExistingInstance(
                candidateBundleId: "com.apple.Safari",
                candidateName: "Safari",
                currentBundleId: "com.lokii.app"
            )
        )
        XCTAssertTrue(
            SingleInstanceGuard.matchesExistingInstance(
                candidateBundleId: nil,
                candidateName: "Lokii",
                currentBundleId: nil
            )
        )
        XCTAssertFalse(
            SingleInstanceGuard.matchesExistingInstance(
                candidateBundleId: nil,
                candidateName: "OtherTool",
                currentBundleId: nil
            )
        )
    }

    func testStaticWiringInvariants() throws {
        let entryMainSource = try TestFixture.readRepoFile("Lokii/LokiiEntry/main.swift")
        XCTAssertTrue(entryMainSource.contains("LokiiApplication.main()"))

        let appDelegateSource = try TestFixture.readRepoFile("Lokii/Lokii/AppDelegate.swift")
        XCTAssertTrue(appDelegateSource.contains("private lazy var engine = SearchEngine()"))
        let lockRange = try XCTUnwrap(appDelegateSource.range(of: "SingleInstanceGuard.acquireOrActivateExistingAndExit()"))
        let delegateInitRange = try XCTUnwrap(appDelegateSource.range(of: "let delegate = AppDelegate()"))
        XCTAssertTrue(lockRange.lowerBound < delegateInitRange.lowerBound, "单实例锁必须在创建 AppDelegate 前执行")

        let indexPaneSource = try TestFixture.readRepoFile("Lokii/Lokii/Preferences/PreferencesIndexPane.swift")
        let rowStart = try XCTUnwrap(indexPaneSource.range(of: "private struct SettingsPathRow: View {"))
        let viewStart = try XCTUnwrap(indexPaneSource.range(of: "// MARK: - Native SwiftUI Input Sheet"))
        let rowBody = String(indexPaneSource[rowStart.lowerBound..<viewStart.lowerBound])
        XCTAssertFalse(rowBody.contains("FileManager.default.fileExists"))

        let managedRowStart = try XCTUnwrap(indexPaneSource.range(of: "private struct ManagedVolumeRow: View {"))
        let paneStart = try XCTUnwrap(indexPaneSource.range(of: "// MARK: - Index Preferences Pane"))
        let managedRowBody = String(indexPaneSource[managedRowStart.lowerBound..<paneStart.lowerBound])
        XCTAssertFalse(managedRowBody.contains("prefs.volume.status.offlineNoCount"))

        let searchEngineSource = try TestFixture.readRepoFile("Lokii/Lokii/SearchEngine.swift")
        for requiredSymbol in [
            "peekStartupIsColdStart",
            "didMountNotification",
            "willUnmountNotification",
            "didUnmountNotification",
            "func onVolumeStatusesChanged()",
            "volumeEventQueue",
            "statusRefreshQueue",
            "statusSequence",
            "catch let FfiError.IoError(message)",
        ] {
            XCTAssertTrue(searchEngineSource.contains(requiredSymbol), "SearchEngine.swift 缺少 \(requiredSymbol)")
        }

        for requiredPrefsSymbol in [
            "prefs.volume.clearCacheConfirm.title",
            "prefs.volume.clearCacheConfirm.message",
            "prefs.volume.clearCacheConfirm.confirm",
            "prefs.volume.state.offlineUncached",
            "prefs.index.group.volumes",
            "prefs.volume.includeOffline",
        ] {
            XCTAssertTrue(indexPaneSource.contains(requiredPrefsSymbol), "PreferencesIndexPane.swift 缺少 \(requiredPrefsSymbol)")
        }

        let preferencesSource = try TestFixture.readRepoFile("Lokii/Lokii/Preferences/PreferencesViewModel.swift")
        for requiredViewModelSymbol in [
            "import DiskArbitration",
            "import IOKit",
            "probeIsDiskImage",
            "where !mounted.isDiskImage",
        ] {
            XCTAssertTrue(preferencesSource.contains(requiredViewModelSymbol), "PreferencesViewModel.swift 缺少 \(requiredViewModelSymbol)")
        }

        let searchWindowSource = try TestFixture.readRepoFile("Lokii/Lokii/SearchWindow.swift")
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
            "!window.isOnActiveSpace",
        ] {
            XCTAssertTrue(searchWindowSource.contains(requiredWindowSymbol), "SearchWindow.swift 缺少 \(requiredWindowSymbol)")
        }
        for forbiddenWindowSymbol in [
            "NSEvent.pressedMouseButtons",
            "titlebarAppearsTransparent = true",
            "collectionBehavior = [.moveToActiveSpace]",
        ] {
            XCTAssertFalse(searchWindowSource.contains(forbiddenWindowSymbol), "SearchWindow.swift 不得包含 \(forbiddenWindowSymbol)")
        }

        let resultsVCSource = try TestFixture.readRepoFile("Lokii/Lokii/SearchResultsViewController.swift")
        for requiredVCSymbol in [
            "func setOfflineItems(",
            "requireAllOnline",
            "areAllContextualTargetsOnline",
            "func refreshOfflineStates()",
        ] {
            XCTAssertTrue(resultsVCSource.contains(requiredVCSymbol), "SearchResultsViewController.swift 缺少 \(requiredVCSymbol)")
        }
    }

    func testCachedPendingSaveRequiresRebuildAndWatcherErrorResetsRebuilding() {
        let engine = SearchEngine()
        let vm = PreferencesViewModel(engine: engine)
        XCTAssertFalse(vm.isIndexDirty)
        XCTAssertFalse(vm.pendingSaveRequiresRebuild)

        // 宽化草稿（开启隐藏文件或移除排除目录名）应立即刷新缓存的 pendingSaveRequiresRebuild
        if !vm.savedIncludeHidden {
            vm.includeHidden = true
            XCTAssertTrue(vm.pendingSaveRequiresRebuild)
        } else if !vm.savedExcludePatterns.isEmpty {
            vm.excludePatterns.removeLast()
            XCTAssertTrue(vm.pendingSaveRequiresRebuild)
        }

        vm.discardIndexChanges()
        XCTAssertFalse(vm.isIndexDirty)
        XCTAssertFalse(vm.pendingSaveRequiresRebuild)

        // 重建错误通知必须复位 isRebuildingIndex 并透传错误信息，非 rebuild 源不误清
        vm.isRebuildingIndex = true
        vm.rebuildScannedCount = 4200
        vm.indexSaveErrorMessage = nil

        NotificationCenter.default.post(
            name: SearchEngine.watcherErrorNotification,
            object: nil,
            userInfo: ["source": "initialize", "message": "ignore me"]
        )
        RunLoop.main.run(until: Date().addingTimeInterval(0.05))
        XCTAssertTrue(vm.isRebuildingIndex)

        NotificationCenter.default.post(
            name: SearchEngine.watcherErrorNotification,
            object: nil,
            userInfo: ["source": "rebuild", "message": "full reconciliation failed: limit"]
        )
        RunLoop.main.run(until: Date().addingTimeInterval(0.05))
        XCTAssertFalse(vm.isRebuildingIndex)
        XCTAssertEqual(vm.rebuildScannedCount, 0)
        XCTAssertEqual(vm.indexSaveErrorMessage, "full reconciliation failed: limit")
    }

    func testEnglishMultiVolumeBannerCopyUsesInclusiveCount() throws {
        let enPath = TestFixture.repoFileURL("Lokii/Lokii/Resources/en.lproj/Localizable.strings")
        let enDict = try XCTUnwrap(NSDictionary(contentsOf: enPath) as? [String: String])
        let localize: (String) -> String = { key in enDict[key] ?? key }

        let d1 = TestFixture.volumeStatus(
            volumeId: "v1",
            configuredPath: "/Volumes/Disk1",
            displayName: "Disk1",
            kind: "local_external",
            state: .syncing,
            scannedCount: 500
        )
        let d2 = TestFixture.volumeStatus(
            volumeId: "v2",
            configuredPath: "/Volumes/Disk2",
            displayName: "Disk2",
            kind: "local_external",
            state: .syncing,
            scannedCount: 700
        )
        let syncingMsg = VolumeBannerResolver.message(
            for: .syncing(volumes: [d1, d2], totalScanned: 1200, hasPendingConfirmations: false),
            localize: localize
        )
        XCTAssertTrue(syncingMsg.contains("2 external volumes including \"Disk1\""), "unexpected: \(syncingMsg)")
        XCTAssertFalse(syncingMsg.contains("and 2 external volumes"))

        let syncingNoCountMsg = VolumeBannerResolver.message(
            for: .syncing(volumes: [d1, d2], totalScanned: 0, hasPendingConfirmations: false),
            localize: localize
        )
        XCTAssertTrue(syncingNoCountMsg.contains("2 external volumes including \"Disk1\""), "unexpected: \(syncingNoCountMsg)")
        XCTAssertFalse(syncingNoCountMsg.contains("and 2 external volumes"))

        let confirmMsg = VolumeBannerResolver.message(
            for: .needsConfirmation(volumes: [d1, d2]),
            localize: localize
        )
        XCTAssertTrue(confirmMsg.contains("2 external volumes including \"Disk1\""), "unexpected: \(confirmMsg)")
        XCTAssertFalse(confirmMsg.contains("and 2 external volumes"))
    }
}
