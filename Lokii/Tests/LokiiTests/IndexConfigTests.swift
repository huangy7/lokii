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
            "network_rescan_minutes": 15
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
        let legacyGeneralData = """
        {"language": "zh-Hans", "show_in_dock": true}
        """.data(using: .utf8)!
        let decodedLegacyGeneral = try decoder.decode(LokiiConfig.GeneralConfig.self, from: legacyGeneralData)
        XCTAssertTrue(decodedLegacyGeneral.hideResultsWhenEmpty)

        let disabledGeneralData = """
        {"language": "en", "show_in_dock": true, "hide_results_when_empty": false}
        """.data(using: .utf8)!
        let decodedDisabledGeneral = try decoder.decode(LokiiConfig.GeneralConfig.self, from: disabledGeneralData)
        XCTAssertFalse(decodedDisabledGeneral.hideResultsWhenEmpty)
        let encodedGeneral = String(data: try JSONEncoder().encode(decodedDisabledGeneral), encoding: .utf8) ?? ""
        XCTAssertTrue(encodedGeneral.contains("\"hide_results_when_empty\":false"))
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
            entryCount: 100
        )
        XCTAssertEqual(
            SettingsPathStatusResolver.resolve(path: "/Volumes/Media", volumeStatuses: [remountedVol], localAvailabilityCache: [:]),
            .volumeOnline(entryCount: 980, activePath: "/Volumes/Media-1", isRemounted: true, kind: "local_external")
        )
        XCTAssertEqual(
            SettingsPathStatusResolver.resolve(path: "/Volumes/Media-1", volumeStatuses: [remountedVol], localAvailabilityCache: [:]),
            .volumeOnline(entryCount: 980, activePath: "/Volumes/Media-1", isRemounted: true, kind: "local_external")
        )
        XCTAssertEqual(
            SettingsPathStatusResolver.resolve(path: "/Volumes/Work", volumeStatuses: [syncingVol], localAvailabilityCache: [:]),
            .volumeSyncing(kind: "local_external")
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
        updatedStatus.state = .syncing
        XCTAssertFalse(updatedStatus.hasSameLifecycle(as: disk2Slot))
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

    func testPrimaryRebuildAndDiskImageDetection() {
        let mountedVols = [MountedVolumeEntry(path: "/Volumes/NAS", displayName: "NAS", kind: "network")]
        XCTAssertFalse(
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
            )
        )
        XCTAssertFalse(
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
            )
        )
        XCTAssertTrue(
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
            )
        )
        XCTAssertTrue(
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
            )
        )
        XCTAssertTrue(
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
            )
        )

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
}
