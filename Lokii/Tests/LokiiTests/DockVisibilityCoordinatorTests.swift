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
}
