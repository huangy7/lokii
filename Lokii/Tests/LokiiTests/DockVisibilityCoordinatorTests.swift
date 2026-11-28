import AppKit
import XCTest
@testable import LokiiAppLib

final class DockVisibilityCoordinatorTests: XCTestCase {
    func testTargetPolicyMatrix() {
        XCTAssertEqual(DockVisibilityCoordinator.targetPolicy(for: .always, hasOpenUserWindow: false), .regular)
        XCTAssertEqual(DockVisibilityCoordinator.targetPolicy(for: .always, hasOpenUserWindow: true), .regular)

        XCTAssertEqual(DockVisibilityCoordinator.targetPolicy(for: .whenOpen, hasOpenUserWindow: true), .regular)
        XCTAssertEqual(DockVisibilityCoordinator.targetPolicy(for: .whenOpen, hasOpenUserWindow: false), .accessory)

        XCTAssertEqual(DockVisibilityCoordinator.targetPolicy(for: .never, hasOpenUserWindow: false), .accessory)
        XCTAssertEqual(DockVisibilityCoordinator.targetPolicy(for: .never, hasOpenUserWindow: true), .accessory)
    }

    func testWindowSnapshotFiltering() {
        let normalVisible = DockWindowSnapshot(className: "LokiiWindow", level: .normal, isVisible: true)
        let floatingPinned = DockWindowSnapshot(className: "LokiiWindow", level: .floating, isVisible: true)
        let modalDialog = DockWindowSnapshot(className: "NSOpenPanel", level: .modalPanel, isVisible: true)
        let miniaturized = DockWindowSnapshot(
            className: "PreferencesWindow",
            level: .normal,
            isVisible: false,
            isMiniaturized: true
        )

        XCTAssertTrue(normalVisible.countsAsOpenUserWindow)
        XCTAssertTrue(floatingPinned.countsAsOpenUserWindow)
        XCTAssertTrue(modalDialog.countsAsOpenUserWindow)
        XCTAssertTrue(miniaturized.countsAsOpenUserWindow)

        let hiddenWindow = DockWindowSnapshot(className: "LokiiWindow", level: .normal, isVisible: false)
        let statusBar = DockWindowSnapshot(
            className: "NSStatusBarWindow",
            level: .statusBar,
            isVisible: true,
            hasTitledStyle: false,
            isExcludedAuxiliary: true
        )
        let popover = DockWindowSnapshot(
            className: "_NSPopoverWindow",
            level: .floating,
            isVisible: true,
            hasTitledStyle: true,
            isExcludedAuxiliary: DockWindowSnapshot.isExcludedClassName("_NSPopoverWindow")
        )
        let tooltip = DockWindowSnapshot(
            className: "NSToolTipWindow",
            level: .floating,
            isVisible: true,
            hasTitledStyle: true,
            isExcludedAuxiliary: DockWindowSnapshot.isExcludedClassName("NSToolTipWindow")
        )
        let menuWindow = DockWindowSnapshot(
            className: "NSMenuWindow",
            level: .popUpMenu,
            isVisible: true,
            hasTitledStyle: true,
            isExcludedAuxiliary: DockWindowSnapshot.isExcludedClassName("NSMenuWindow")
        )
        let quickLook = DockWindowSnapshot(
            className: "QLPreviewPanel",
            level: .floating,
            isVisible: true,
            hasTitledStyle: true,
            isExcludedAuxiliary: true
        )
        let untitledHelper = DockWindowSnapshot(
            className: "NSWindow",
            level: .normal,
            isVisible: true,
            hasTitledStyle: false
        )

        XCTAssertFalse(hiddenWindow.countsAsOpenUserWindow)
        XCTAssertFalse(statusBar.countsAsOpenUserWindow)
        XCTAssertFalse(popover.countsAsOpenUserWindow)
        XCTAssertFalse(tooltip.countsAsOpenUserWindow)
        XCTAssertFalse(menuWindow.countsAsOpenUserWindow)
        XCTAssertFalse(quickLook.countsAsOpenUserWindow)
        XCTAssertFalse(untitledHelper.countsAsOpenUserWindow)
        XCTAssertFalse(
            DockVisibilityCoordinator.hasOpenUserWindow(in: [
                hiddenWindow, statusBar, popover, tooltip, menuWindow, quickLook, untitledHelper,
            ])
        )
    }

    func testPrepareForWindowPresentationUpgradesWhenOpenAndIsIdempotent() {
        var currentPolicy: NSApplication.ActivationPolicy = .accessory
        var appliedPolicies: [NSApplication.ActivationPolicy] = []

        let coordinator = DockVisibilityCoordinator(
            initialMode: .whenOpen,
            observeWindowNotifications: false,
            policyReader: { currentPolicy },
            policySetter: { next in
                currentPolicy = next
                appliedPolicies.append(next)
                return true
            },
            windowSnapshotProvider: { [] },
            activeWindowRestorerBuilder: { nil },
            asyncScheduler: { $0() }
        )

        XCTAssertTrue(coordinator.prepareForWindowPresentation())
        XCTAssertEqual(currentPolicy, .regular)
        XCTAssertEqual(appliedPolicies, [.regular])

        // 幂等守卫：已是 .regular 时不再触发重复调用
        XCTAssertFalse(coordinator.prepareForWindowPresentation())
        XCTAssertEqual(appliedPolicies, [.regular])

        // .never 模式下呈现窗口不升格为 .regular
        coordinator.updateMode(.never)
        XCTAssertEqual(currentPolicy, .accessory)
        appliedPolicies.removeAll()
        XCTAssertFalse(coordinator.prepareForWindowPresentation())
        XCTAssertEqual(currentPolicy, .accessory)
        XCTAssertTrue(appliedPolicies.isEmpty)
    }

    func testSchedulePolicySyncCoalescesBurstsAndRespectsRemainingOpenWindows() {
        var currentPolicy: NSApplication.ActivationPolicy = .regular
        var appliedPolicies: [NSApplication.ActivationPolicy] = []
        var snapshots: [DockWindowSnapshot] = [
            DockWindowSnapshot(className: "PreferencesWindow", level: .normal, isVisible: true),
        ]
        var scheduledBlocks: [() -> Void] = []
        var restorerInvocations = 0

        let coordinator = DockVisibilityCoordinator(
            initialMode: .whenOpen,
            observeWindowNotifications: false,
            policyReader: { currentPolicy },
            policySetter: { next in
                currentPolicy = next
                appliedPolicies.append(next)
                return true
            },
            windowSnapshotProvider: { snapshots },
            activeWindowRestorerBuilder: {
                { restorerInvocations += 1 }
            },
            asyncScheduler: { block in
                scheduledBlocks.append(block)
            }
        )

        // 同一回合多次触发合并为一次异步评估
        coordinator.schedulePolicySync()
        coordinator.schedulePolicySync()
        coordinator.schedulePolicySync()
        XCTAssertEqual(scheduledBlocks.count, 1)

        // 设置窗仍打开，保持 .regular
        scheduledBlocks.removeFirst()()
        XCTAssertEqual(currentPolicy, .regular)
        XCTAssertTrue(appliedPolicies.isEmpty)

        // 所有窗口关闭后，自动回落 .accessory 且不主动抢夺焦点
        snapshots = [
            DockWindowSnapshot(className: "NSStatusBarWindow", level: .statusBar, isVisible: true, hasTitledStyle: false, isExcludedAuxiliary: true),
        ]
        coordinator.schedulePolicySync()
        XCTAssertEqual(scheduledBlocks.count, 1)
        scheduledBlocks.removeFirst()()
        XCTAssertEqual(currentPolicy, .accessory)
        XCTAssertEqual(appliedPolicies, [.accessory])
        XCTAssertEqual(restorerInvocations, 0)
    }

    func testUpdateModeNeverPreservesFrontWindowWhenDroppingFromRegular() {
        var currentPolicy: NSApplication.ActivationPolicy = .regular
        var appliedPolicies: [NSApplication.ActivationPolicy] = []
        var restorerBuiltBeforePolicyChange = false
        var restorerInvocations = 0

        let coordinator = DockVisibilityCoordinator(
            initialMode: .whenOpen,
            observeWindowNotifications: false,
            policyReader: { currentPolicy },
            policySetter: { next in
                currentPolicy = next
                appliedPolicies.append(next)
                return true
            },
            windowSnapshotProvider: {
                [DockWindowSnapshot(className: "PreferencesWindow", level: .normal, isVisible: true)]
            },
            activeWindowRestorerBuilder: {
                // 必须在策略切换前捕获前台窗口，验证调用时仍为 .regular
                restorerBuiltBeforePolicyChange = (currentPolicy == .regular)
                return { restorerInvocations += 1 }
            },
            asyncScheduler: { $0() }
        )

        coordinator.updateMode(.never)
        XCTAssertEqual(currentPolicy, .accessory)
        XCTAssertEqual(appliedPolicies, [.accessory])
        XCTAssertTrue(restorerBuiltBeforePolicyChange)
        XCTAssertEqual(restorerInvocations, 1)

        // 切回 .always 不触发 accessory 焦点恢复器
        coordinator.updateMode(.always)
        XCTAssertEqual(currentPolicy, .regular)
        XCTAssertEqual(restorerInvocations, 1)
    }

    func testWillCloseNotificationExcludesClosingWindowAndHidesDock() {
        let center = NotificationCenter()
        let closingWindow = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 200, height: 120),
            styleMask: [.titled, .closable],
            backing: .buffered,
            defer: true
        )
        let closingId = ObjectIdentifier(closingWindow)

        var currentPolicy: NSApplication.ActivationPolicy = .regular
        var scheduledBlocks: [() -> Void] = []

        let coordinator = DockVisibilityCoordinator(
            initialMode: .whenOpen,
            notificationCenter: center,
            observeWindowNotifications: true,
            policyReader: { currentPolicy },
            policySetter: { next in
                currentPolicy = next
                return true
            },
            windowSnapshotProvider: {
                // 模拟 willClose 触发瞬间窗口快照仍为可见
                [DockWindowSnapshot(identifier: closingId, className: "PreferencesWindow", level: .normal, isVisible: true)]
            },
            activeWindowRestorerBuilder: { nil },
            asyncScheduler: { block in
                scheduledBlocks.append(block)
            }
        )
        _ = coordinator

        center.post(name: NSWindow.willCloseNotification, object: closingWindow)
        XCTAssertEqual(scheduledBlocks.count, 1)
        scheduledBlocks.removeFirst()()
        XCTAssertEqual(currentPolicy, .accessory)
    }

    func testBeginTerminationBlocksFurtherPolicyTransitions() {
        var currentPolicy: NSApplication.ActivationPolicy = .regular
        var appliedPolicies: [NSApplication.ActivationPolicy] = []

        let coordinator = DockVisibilityCoordinator(
            initialMode: .whenOpen,
            observeWindowNotifications: false,
            policyReader: { currentPolicy },
            policySetter: { next in
                currentPolicy = next
                appliedPolicies.append(next)
                return true
            },
            windowSnapshotProvider: { [] },
            activeWindowRestorerBuilder: { nil },
            asyncScheduler: { $0() }
        )

        coordinator.beginTermination()
        XCTAssertFalse(coordinator.prepareForWindowPresentation())
        coordinator.schedulePolicySync()
        coordinator.syncPolicyNow()
        coordinator.updateMode(.never)

        XCTAssertEqual(currentPolicy, .regular)
        XCTAssertTrue(appliedPolicies.isEmpty)
    }

    func testGeneralConfigDockIconModeMigrationAndDefaults() throws {
        let decoder = JSONDecoder()

        XCTAssertEqual(LokiiConfig.GeneralConfig().dockIconMode, .whenOpen)
        let emptyDecoded = try decoder.decode(LokiiConfig.GeneralConfig.self, from: Data("{}".utf8))
        XCTAssertEqual(emptyDecoded.dockIconMode, .whenOpen)

        let legacyTrue = try decoder.decode(
            LokiiConfig.GeneralConfig.self,
            from: Data(#"{"language":"zh-Hans","show_in_dock":true}"#.utf8)
        )
        XCTAssertEqual(legacyTrue.dockIconMode, .always)

        let legacyFalse = try decoder.decode(
            LokiiConfig.GeneralConfig.self,
            from: Data(#"{"language":"en","show_in_dock":false}"#.utf8)
        )
        XCTAssertEqual(legacyFalse.dockIconMode, .never)

        let dualKeys = try decoder.decode(
            LokiiConfig.GeneralConfig.self,
            from: Data(#"{"dock_icon_mode":"when_open","show_in_dock":false}"#.utf8)
        )
        XCTAssertEqual(dualKeys.dockIconMode, .whenOpen)

        let encoded = String(data: try JSONEncoder().encode(dualKeys), encoding: .utf8) ?? ""
        XCTAssertTrue(encoded.contains("\"dock_icon_mode\":\"when_open\""))
        XCTAssertFalse(encoded.contains("show_in_dock"))
    }

    func testUpdateModeNeverSuppressesBlurHideDuringPolicyTransition() {
        var currentPolicy: NSApplication.ActivationPolicy = .regular
        var preservingDuringSetter = false
        var scheduledBlocks: [() -> Void] = []

        var coordinator: DockVisibilityCoordinator!
        coordinator = DockVisibilityCoordinator(
            initialMode: .whenOpen,
            observeWindowNotifications: false,
            policyReader: { currentPolicy },
            policySetter: { next in
                preservingDuringSetter = coordinator.isPreservingActiveWindow
                currentPolicy = next
                return true
            },
            windowSnapshotProvider: {
                [DockWindowSnapshot(className: "PreferencesWindow", level: .normal, isVisible: true)]
            },
            activeWindowRestorerBuilder: {
                { }
            },
            asyncScheduler: { block in
                scheduledBlocks.append(block)
            }
        )

        XCTAssertFalse(coordinator.isPreservingActiveWindow)
        coordinator.updateMode(.never)
        XCTAssertTrue(preservingDuringSetter, "切换至 .accessory 同步期间必须抑制失焦隐藏")
        XCTAssertTrue(coordinator.isPreservingActiveWindow, "下一轮主线程复焦完成前应继续抑制失焦隐藏")
        XCTAssertEqual(scheduledBlocks.count, 1)

        scheduledBlocks.removeFirst()()
        XCTAssertFalse(coordinator.isPreservingActiveWindow)

        // 若系统拒绝切换策略，不得遗留常驻抑制标记
        currentPolicy = .regular
        let failingCoordinator = DockVisibilityCoordinator(
            initialMode: .whenOpen,
            observeWindowNotifications: false,
            policyReader: { currentPolicy },
            policySetter: { _ in false },
            windowSnapshotProvider: {
                [DockWindowSnapshot(className: "PreferencesWindow", level: .normal, isVisible: true)]
            },
            activeWindowRestorerBuilder: { { } },
            asyncScheduler: { block in
                scheduledBlocks.append(block)
            }
        )
        failingCoordinator.updateMode(.never)
        XCTAssertFalse(failingCoordinator.isPreservingActiveWindow)
    }

    func testPrepareForWindowPresentationInvalidatesStaleWillCloseSync() {
        let center = NotificationCenter()
        let reusedWindow = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 200, height: 120),
            styleMask: [.titled, .closable],
            backing: .buffered,
            defer: true
        )
        let reusedId = ObjectIdentifier(reusedWindow)

        var currentPolicy: NSApplication.ActivationPolicy = .regular
        var snapshots: [DockWindowSnapshot] = [
            DockWindowSnapshot(identifier: reusedId, className: "PreferencesWindow", level: .normal, isVisible: true),
        ]
        var scheduledBlocks: [() -> Void] = []

        let coordinator = DockVisibilityCoordinator(
            initialMode: .whenOpen,
            notificationCenter: center,
            observeWindowNotifications: true,
            policyReader: { currentPolicy },
            policySetter: { next in
                currentPolicy = next
                return true
            },
            windowSnapshotProvider: { snapshots },
            activeWindowRestorerBuilder: { nil },
            asyncScheduler: { block in
                scheduledBlocks.append(block)
            }
        )

        // 1. 复用窗口触发关闭，排入旧异步回落任务（排除 reusedId）
        center.post(name: NSWindow.willCloseNotification, object: reusedWindow)
        XCTAssertEqual(scheduledBlocks.count, 1)

        // 2. 旧任务尚未落地前立刻重新呈现窗口：应作废旧异步同步并清空排除集
        _ = coordinator.prepareForWindowPresentation()

        // 3. 旧异步回调执行后，不得把刚重开的窗口排除掉并误降级为 .accessory
        scheduledBlocks.removeFirst()()
        XCTAssertEqual(currentPolicy, .regular)

        // 4. 随后真正关闭全部窗口并重新排入同步，仍应正常回落为 .accessory
        snapshots = []
        coordinator.schedulePolicySync()
        XCTAssertEqual(scheduledBlocks.count, 1)
        scheduledBlocks.removeFirst()()
        XCTAssertEqual(currentPolicy, .accessory)
    }

    func testSearchWindowReopenDuringFadeOutRestoresOpacity() throws {
        _ = NSApplication.shared
        let searchWindow = SearchWindow(engine: SearchEngine())
        let window = try XCTUnwrap(searchWindow.window)
        defer { window.orderOut(nil) }

        var didHideCount = 0
        searchWindow.onDidHide = {
            didHideCount += 1
        }

        searchWindow.showWindow(nil)
        RunLoop.main.run(until: Date().addingTimeInterval(0.05))
        XCTAssertTrue(window.isVisible)

        // 触发 0.15s 淡出动画后立刻重新唤起，验证旧收起回调被取消且窗口透明度恢复为 1.0
        searchWindow.hideWindow(animated: true)
        RunLoop.main.run(until: Date().addingTimeInterval(0.02))
        searchWindow.showWindow(nil)
        RunLoop.main.run(until: Date().addingTimeInterval(0.25))

        XCTAssertTrue(window.isVisible)
        XCTAssertEqual(window.alphaValue, 1.0, accuracy: 0.01)
        XCTAssertEqual(didHideCount, 0)
    }

    @MainActor
    func testCheckForUpdatesSkipsBeforePresentingWhenUpdaterCannotCheck() {
        var beforePresentingCalled = false
        let started = AppUpdater.shared.checkForUpdates(isUserInitiated: true) {
            beforePresentingCalled = true
        }
        XCTAssertFalse(started)
        XCTAssertFalse(beforePresentingCalled)
    }

    func testRapidToggleBurstsDebounceDemotionAndPreventPolicyThrashing() {
        var currentPolicy: NSApplication.ActivationPolicy = .accessory
        var appliedPolicies: [NSApplication.ActivationPolicy] = []
        var hasOpen = false
        var now: TimeInterval = 10.0
        var immediateTasks: [() -> Void] = []
        var delayedTasks: [(delay: TimeInterval, block: () -> Void)] = []

        let coordinator = DockVisibilityCoordinator(
            initialMode: .whenOpen,
            rapidToggleThreshold: 0.45,
            demotionDelay: 0.55,
            clock: { now },
            observeWindowNotifications: false,
            policyReader: { currentPolicy },
            policySetter: { next in
                currentPolicy = next
                appliedPolicies.append(next)
                return true
            },
            windowSnapshotProvider: {
                hasOpen
                    ? [DockWindowSnapshot(className: "LokiiWindow", level: .normal, isVisible: true)]
                    : []
            },
            activeWindowRestorerBuilder: { nil },
            asyncScheduler: { block in
                immediateTasks.append(block)
            },
            delayedScheduler: { delay, block in
                delayedTasks.append((delay, block))
            }
        )

        // 1. 正常使用路径：开窗停留超过 rapidToggleThreshold (0.45s) 后关闭 -> 0ms 即时回落，不走延迟定时器
        hasOpen = true
        _ = coordinator.prepareForWindowPresentation()
        now += 1.0
        hasOpen = false
        coordinator.schedulePolicySync()
        XCTAssertEqual(immediateTasks.count, 1)
        XCTAssertTrue(delayedTasks.isEmpty)
        immediateTasks.removeFirst()()
        XCTAssertEqual(currentPolicy, .accessory)
        XCTAssertEqual(appliedPolicies, [.regular, .accessory])

        appliedPolicies.removeAll()
        // 越过阈值窗口后开始连续快点测试
        now += 1.0

        // 2. 高频连点路径：连续 10 次间隔 0.10s (< 0.45s) 快速开合 -> 仅第 1 次升格，期间全走 0.55s 防抖冷却
        for _ in 0..<10 {
            hasOpen = true
            _ = coordinator.prepareForWindowPresentation()
            now += 0.10
            hasOpen = false
            coordinator.schedulePolicySync()
        }

        XCTAssertEqual(currentPolicy, .regular)
        XCTAssertEqual(appliedPolicies, [.regular])
        XCTAssertTrue(immediateTasks.isEmpty)
        XCTAssertEqual(delayedTasks.count, 10)
        XCTAssertTrue(delayedTasks.allSatisfy { $0.delay == 0.55 })

        // 前 9 个陈旧延迟任务已被后续 prepareForWindowPresentation 作废，执行时不得降格
        for i in 0..<9 {
            delayedTasks[i].block()
        }
        XCTAssertEqual(currentPolicy, .regular)
        XCTAssertEqual(appliedPolicies, [.regular])

        // 仅最后一次关窗的延迟任务生效并平稳回落为 .accessory
        delayedTasks[9].block()
        XCTAssertEqual(currentPolicy, .accessory)
        XCTAssertEqual(appliedPolicies, [.regular, .accessory])
    }

    func testStatusItemResignToggleGuardSuppressesImmediateReopenAndAllowsNextClick() {
        var guardState = StatusItemResignToggleGuard(suppressionWindow: 0.25)

        // 无前置失焦隐藏记录时不拦截
        XCTAssertFalse(guardState.consumeRecentResignHide(at: 100.0))

        // 同一点击先触发失焦隐藏，随后 10ms 内触发托盘点击 -> 抑制误重开并自动消费记录
        guardState.recordResignAutoHide(at: 100.0)
        XCTAssertTrue(guardState.consumeRecentResignHide(at: 100.01))
        // 已消费后，紧接着的下一击允许正常打开
        XCTAssertFalse(guardState.consumeRecentResignHide(at: 100.02))

        // 超过抑制时间窗后过期失效
        guardState.recordResignAutoHide(at: 200.0)
        XCTAssertFalse(guardState.consumeRecentResignHide(at: 200.30))

        // clear 后立即失效
        guardState.recordResignAutoHide(at: 300.0)
        guardState.clear()
        XCTAssertFalse(guardState.consumeRecentResignHide(at: 300.01))
    }
}
