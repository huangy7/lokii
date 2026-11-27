import Foundation
import AppKit
import Sparkle

@MainActor
final class AppUpdater: NSObject, ObservableObject, SPUUpdaterDelegate {
    static let shared = AppUpdater()

    @Published private(set) var isChecking = false
    @Published private(set) var hasNewVersion = false
    @Published private(set) var lastCheckTime: Date?
    @Published private(set) var errorMessage: String?

    private var controller: SPUStandardUpdaterController?
    private var observations: [NSKeyValueObservation] = []
    private var userInitiated = false

    var automaticallyChecksForUpdates: Bool {
        get {
            controller?.updater.automaticallyChecksForUpdates
                ?? (UserDefaults.standard.object(forKey: "SUEnableAutomaticChecks") as? Bool ?? true)
        }
        set {
            objectWillChange.send()
            if let updater = controller?.updater {
                updater.automaticallyChecksForUpdates = newValue
            } else {
                UserDefaults.standard.set(newValue, forKey: "SUEnableAutomaticChecks")
            }
        }
    }

    func start() {
        guard controller == nil else { return }
        let controller = SPUStandardUpdaterController(startingUpdater: false, updaterDelegate: self, userDriverDelegate: nil)
        self.controller = controller
        observations = [
            controller.updater.observe(\.sessionInProgress, options: [.initial, .new]) { [weak self] updater, _ in
                let inProgress = updater.sessionInProgress
                Task { @MainActor in self?.isChecking = inProgress }
            }
        ]
        do {
            try controller.updater.start()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// 发起更新检查；仅在确认能够实际触发检查时才执行 `beforePresenting` 回调并返回 `true`，
    /// 避免后台已有检查或更新器未就绪时提前升格应用激活策略导致程序坞图标空悬。
    @discardableResult
    func checkForUpdates(isUserInitiated: Bool = false, beforePresenting: (() -> Void)? = nil) -> Bool {
        guard isUserInitiated || automaticallyChecksForUpdates else { return false }
        start()
        guard let updater = controller?.updater, updater.canCheckForUpdates else { return false }
        if !isUserInitiated && updater.sessionInProgress {
            return false
        }
        if isUserInitiated {
            beforePresenting?()
        }
        userInitiated = isUserInitiated
        errorMessage = nil
        isChecking = true
        if isUserInitiated {
            updater.checkForUpdates()
        } else {
            updater.checkForUpdatesInBackground()
        }
        return true
    }

    func updaterShouldPromptForPermissionToCheck(forUpdates updater: SPUUpdater) -> Bool {
        false
    }

    func updater(_ updater: SPUUpdater, didFindValidUpdate item: SUAppcastItem) {
        hasNewVersion = true
        lastCheckTime = Date()
    }

    func updaterDidNotFindUpdate(_ updater: SPUUpdater) {
        hasNewVersion = false
        lastCheckTime = Date()
    }

    func updater(_ updater: SPUUpdater, didFinishUpdateCycleFor updateCheck: SPUUpdateCheck, error: Error?) {
        isChecking = false
        if userInitiated, let error, (error as NSError).code != SUError.noUpdateError.rawValue {
            errorMessage = error.localizedDescription
        }
        userInitiated = false
    }

    func allowedSystemProfileKeys(for updater: SPUUpdater) -> [String]? {
        []
    }

    func openDownloadPage() {
        guard let url = URL(string: "https://github.com/huangy7/lokii/releases/latest") else { return }
        NSWorkspace.shared.open(url)
    }
}
