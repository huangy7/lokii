import AppKit
import Darwin

extension KeyboardShortcuts.Name {
    static let toggleWindow = Self("toggleWindow", default: .init(.space, modifiers: [.command, .shift]))
}

@_silgen_name("flock")
private func darwinFlock(_ fd: Int32, _ operation: Int32) -> Int32

/// Enforces a single live Lokii instance via a non-blocking exclusive `flock` lockfile
/// before `LokiiCore`, watchers, or the query server initialize.
final class SingleInstanceGuard {
    enum AcquireResult {
        case acquired(SingleInstanceGuard)
        case alreadyHeld(holderPid: pid_t)
        case ioError(Int32)
    }

    private static var activeGuard: SingleInstanceGuard?

    private let fd: Int32

    private init(fd: Int32) {
        self.fd = fd
    }

    deinit {
        if fd >= 0 {
            Darwin.close(fd)
        }
    }

    var fileDescriptor: Int32 { fd }

    static var defaultLockPath: String {
        (NSHomeDirectory() as NSString).appendingPathComponent(".config/lokii/lokii.lock")
    }

    static func tryAcquire(at lockPath: String) -> AcquireResult {
        let parentDir = (lockPath as NSString).deletingLastPathComponent
        if !parentDir.isEmpty {
            try? FileManager.default.createDirectory(
                atPath: parentDir,
                withIntermediateDirectories: true,
                attributes: nil
            )
        }

        // Open with O_CLOEXEC so child processes never inherit the lock fd and keep
        // subsequent Lokii launches blocked after the parent exits.
        let fd = Darwin.open(lockPath, O_CREAT | O_RDWR | O_CLOEXEC, S_IRUSR | S_IWUSR)
        guard fd >= 0 else {
            return .ioError(Darwin.errno)
        }

        var lockRc: Int32 = -1
        repeat {
            lockRc = darwinFlock(fd, LOCK_EX | LOCK_NB)
        } while lockRc != 0 && Darwin.errno == EINTR

        if lockRc == 0 {
            Darwin.ftruncate(fd, 0)
            Darwin.lseek(fd, 0, SEEK_SET)
            let pidLine = "\(Darwin.getpid())\n"
            _ = pidLine.withCString { ptr in
                Darwin.write(fd, ptr, strlen(ptr))
            }
            return .acquired(SingleInstanceGuard(fd: fd))
        }

        let lockErr = Darwin.errno
        if lockErr == EWOULDBLOCK || lockErr == EAGAIN {
            var buffer = [CChar](repeating: 0, count: 32)
            let bytesRead = Darwin.pread(fd, &buffer, buffer.count - 1, 0)
            var holderPid: pid_t = 0
            if bytesRead > 0 {
                let text = String(cString: buffer).trimmingCharacters(in: .whitespacesAndNewlines)
                if let parsed = pid_t(text), parsed > 0 {
                    holderPid = parsed
                }
            }
            Darwin.close(fd)
            return .alreadyHeld(holderPid: holderPid)
        }

        Darwin.close(fd)
        return .ioError(lockErr)
    }

    static func acquireOrActivateExistingAndExit() {
        switch tryAcquire(at: defaultLockPath) {
        case .acquired(let guardHandle):
            activeGuard = guardHandle
        case .alreadyHeld(let holderPid):
            activateExistingInstance(holderPid: holderPid)
            Darwin.exit(0)
        case .ioError(let code):
            NSLog("[Lokii] Single-instance lock open failed (errno=%d); continuing startup", code)
        }
    }

    /// Verifies that a PID read from the lockfile actually belongs to a Lokii instance
    /// so a recycled PID read in a narrow race window never activates an unrelated app.
    static func matchesExistingInstance(
        candidateBundleId: String?,
        candidateName: String?,
        currentBundleId: String?,
        fallbackBundleId: String = "com.lokii.app"
    ) -> Bool {
        let expectedBundleId = currentBundleId ?? fallbackBundleId
        if let candidateBundleId {
            return candidateBundleId == expectedBundleId
        }
        return currentBundleId == nil && candidateName == "Lokii"
    }

    private static func activateExistingInstance(holderPid: pid_t) {
        let selfPid = Darwin.getpid()
        let currentBundleId = Bundle.main.bundleIdentifier
        let bundleId = currentBundleId ?? "com.lokii.app"
        let candidateByPid: NSRunningApplication? = {
            guard holderPid > 0, holderPid != selfPid else { return nil }
            guard let app = NSRunningApplication(processIdentifier: holderPid),
                  !app.isTerminated,
                  matchesExistingInstance(
                      candidateBundleId: app.bundleIdentifier,
                      candidateName: app.localizedName,
                      currentBundleId: currentBundleId
                  ) else {
                return nil
            }
            return app
        }()
        let existingApp = candidateByPid
            ?? NSRunningApplication.runningApplications(withBundleIdentifier: bundleId)
                .first(where: { $0.processIdentifier != selfPid && !$0.isTerminated })

        guard let existingApp else { return }
        existingApp.unhide()
        if #available(macOS 14.0, *) {
            existingApp.activate(options: [.activateAllWindows])
        } else {
            existingApp.activate(options: [.activateAllWindows, .activateIgnoringOtherApps])
        }

        // Reopen through LaunchServices so an accessory-mode instance whose window is
        // hidden by hideOnBlur receives applicationShouldHandleReopen and shows itself.
        if let bundleURL = existingApp.bundleURL {
            let config = NSWorkspace.OpenConfiguration()
            config.activates = true
            config.createsNewApplicationInstance = false
            let delivered = DispatchSemaphore(value: 0)
            NSWorkspace.shared.openApplication(at: bundleURL, configuration: config) { _, _ in
                delivered.signal()
            }
            _ = delivered.wait(timeout: .now() + 0.5)
        }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    private var mainWindow: SearchWindow?
    private var statusItem: NSStatusItem?
    private lazy var engine = SearchEngine()
    private var preferencesController: PreferencesWindowController?
    private var dockCoordinator: DockVisibilityCoordinator?

    func applicationDidFinishLaunching(_ notification: Notification) {
        let config = engine.getConfig()

        // 在创建任何 UI 之前加载语言包
        L10n.configure(language: config?.general.language ?? "zh-Hans")

        // 应用存储的外观主题
        ThemeManager.shared.applyCurrentTheme()

        // 初始化程序坞显示策略协调器并同步初始状态
        let dockIconMode = config?.general.dockIconMode ?? .whenOpen
        let coordinator = DockVisibilityCoordinator(initialMode: dockIconMode)
        dockCoordinator = coordinator
        coordinator.syncPolicyNow()

        FDAGuideWindowController.onWillShow = { [weak self] in
            self?.dockCoordinator?.prepareForWindowPresentation()
        }

        // 注册应用支持向系统“服务（Services）”导出的数据类型
        NSApp.registerServicesMenuSendTypes(Array(ServicesPasteboard.allTypes), returnTypes: [])

        setupStatusItem()
        setupMainMenu()
        NotificationCenter.default.addObserver(self, selector: #selector(onLanguageChanged),
                                               name: L10n.languageChangedNotification, object: nil)
        NotificationCenter.default.addObserver(self, selector: #selector(onConfigReloaded),
                                               name: SearchEngine.configReloadedNotification, object: nil)
        registerGlobalShortcut()

        mainWindow = SearchWindow(engine: engine)
        mainWindow?.onWillShow = { [weak self] in
            self?.dockCoordinator?.prepareForWindowPresentation() ?? false
        }
        mainWindow?.onDidHide = { [weak self] in
            self?.dockCoordinator?.schedulePolicySync()
        }
        mainWindow?.showWindow(nil)
        NSApp.activate(ignoringOtherApps: true)
        engine.initialize()

        // 热启动直接就绪场景下，延迟 0.3 秒主动弹出授权提醒浮窗
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { [weak self] in
            self?.checkAndPromptFDA()
        }

        AppUpdater.shared.start()
    }

    func applicationWillTerminate(_ notification: Notification) {
        dockCoordinator?.beginTermination()
        // 存下索引与回放检查点，下次启动只需回放此后的变更（数百万条约数百毫秒）。
        engine.persistIndex()
    }

    @objc private func onConfigReloaded() {
        let latestMode = engine.getConfig()?.general.dockIconMode ?? .whenOpen
        dockCoordinator?.updateMode(latestMode)
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        mainWindow?.showWindow(nil)
        return true
    }

    func applicationDidBecomeActive(_ notification: Notification) {
        clearFDAPromptIfGranted()
    }

    func applicationDidResignActive(_ notification: Notification) {
        if dockCoordinator?.isPreservingActiveWindow == true {
            return
        }
        if FDAGuideWindowController.isPresenting {
            return
        }
        if mainWindow?.isPinned == true {
            return
        }
        guard engine.getConfig()?.window.hideOnBlur ?? true else { return }
        mainWindow?.hideWindow(animated: false)
    }

    // MARK: - Global Shortcut

    private func registerGlobalShortcut() {
        KeyboardShortcuts.onKeyUp(for: .toggleWindow) { [weak self] in
            self?.toggleWindow()
        }
    }

    // MARK: - Status Item (Tray Icon)

    private func setupStatusItem() {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        if let button = statusItem?.button {
            button.image = NSImage(systemSymbolName: "magnifyingglass", accessibilityDescription: "Lokii")
            button.action = #selector(statusItemClicked)
            button.target = self
            // Use leftMouseDown (not leftMouseUp) to avoid highlight stuck bug (Pitfall #2)
            button.sendAction(on: [.leftMouseDown, .rightMouseUp])
        }
        // Do NOT set statusItem?.menu here — it would block left-click action
    }

    @objc private func statusItemClicked() {
        guard let event = NSApp.currentEvent else { return }
        if event.type == .rightMouseUp {
            let menu = buildTrayMenu()
            menu.delegate = self
            statusItem?.menu = menu
            statusItem?.button?.performClick(nil)
        } else {
            toggleWindow()
        }
    }

    private func buildTrayMenu() -> NSMenu {
        let menu = NSMenu()
        let iconConfig = NSImage.SymbolConfiguration(pointSize: 13, weight: .regular)

        func makeItem(key: String, symbol: String, action: Selector) -> NSMenuItem {
            let item = NSMenuItem(title: L(key), action: action, keyEquivalent: "")
            item.target = self
            if let image = NSImage(systemSymbolName: symbol, accessibilityDescription: nil)?
                .withSymbolConfiguration(iconConfig) {
                image.isTemplate = true
                item.image = image
            }
            return item
        }

        menu.addItem(makeItem(key: "tray.preferences", symbol: "gearshape", action: #selector(openPreferences)))

        let trayRebuildItem = makeItem(key: "tray.rebuildIndex", symbol: "arrow.clockwise", action: #selector(rebuildIndex))
        trayRebuildItem.isEnabled = mainWindow?.canTriggerRebuild ?? false
        menu.addItem(trayRebuildItem)

        menu.addItem(makeItem(key: "tray.checkUpdates", symbol: "arrow.down.circle", action: #selector(checkForUpdates)))

        menu.addItem(NSMenuItem.separator())

        menu.addItem(makeItem(key: "tray.quit", symbol: "power", action: #selector(quitApp)))
        return menu
    }

    // MARK: - Full Disk Access

    func checkAndPromptFDA() {
        guard !clearFDAPromptIfGranted() else { return }
        guard !FDAGuideWindowController.isPromptDismissed else { return }
        // 若主窗口处于冷启动全屏建库遮罩状态，暂不弹出，待索引就绪遮罩收起后再弹出
        if mainWindow?.isOverlayVisible == true {
            return
        }
        FDAGuideWindowController.show()
    }

    /// 权限已授予时清除“不再提示”标记并收起引导窗；返回当前是否已具备完整磁盘访问权限
    @discardableResult
    private func clearFDAPromptIfGranted() -> Bool {
        guard engine.hasFullDiskAccess else { return false }
        FDAGuideWindowController.clearPromptDismissed()
        FDAGuideWindowController.closeIfOpen()
        return true
    }

    // MARK: - Main Menu

    @objc private func onLanguageChanged() {
        NSApp.mainMenu?.refreshLocalizedTitles()
        statusItem?.menu?.refreshLocalizedTitles()
    }

    private func setupMainMenu() {
        let mainMenu = NSMenu()

        // App menu
        let appMenuItem = NSMenuItem()
        let appMenu = NSMenu()

        // About Lokii — custom About panel with version, copyright, description
        let aboutItem = NSMenuItem(localizedKey: "menu.about", action: #selector(showAboutPanel), keyEquivalent: "")
        aboutItem.target = self
        appMenu.addItem(aboutItem)

        let checkUpdatesItem = NSMenuItem(localizedKey: "menu.checkUpdates", action: #selector(checkForUpdates), keyEquivalent: "")
        checkUpdatesItem.target = self
        appMenu.addItem(checkUpdatesItem)

        appMenu.addItem(NSMenuItem.separator())

        // Preferences... with Cmd+,
        let prefsItem = NSMenuItem(localizedKey: "menu.preferences", action: #selector(openPreferences), keyEquivalent: ",")
        prefsItem.target = self
        appMenu.addItem(prefsItem)

        // Rebuild Index with Cmd+Shift+R
        let rebuildItem = NSMenuItem(localizedKey: "menu.rebuildIndex", action: #selector(rebuildIndex), keyEquivalent: "r")
        rebuildItem.keyEquivalentModifierMask = [.command, .shift]
        rebuildItem.target = self
        appMenu.addItem(rebuildItem)

        appMenu.addItem(NSMenuItem.separator())

        // Services menu
        let servicesMenuItem = NSMenuItem(localizedKey: "menu.services", action: nil, keyEquivalent: "")
        let servicesMenu = NSMenu(title: L("menu.services"))
        servicesMenuItem.submenu = servicesMenu
        appMenu.addItem(servicesMenuItem)
        NSApp.servicesMenu = servicesMenu

        appMenu.addItem(NSMenuItem.separator())

        // Quit Lokii with Cmd+Q
        appMenu.addLocalizedItem("menu.quit", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")

        appMenuItem.submenu = appMenu
        mainMenu.addItem(appMenuItem)

        // Edit menu (for copy/paste in search field)
        let editMenuItem = NSMenuItem()
        editMenuItem.representedObject = "menu.edit"
        editMenuItem.title = L("menu.edit")
        let editMenu = NSMenu(title: L("menu.edit"))
        editMenu.addLocalizedItem("menu.cut", action: #selector(NSText.cut(_:)), keyEquivalent: "x")
        editMenu.addLocalizedItem("menu.copy", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        editMenu.addLocalizedItem("menu.paste", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
        editMenu.addLocalizedItem("menu.selectAll", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
        editMenuItem.submenu = editMenu
        mainMenu.addItem(editMenuItem)

        // Window menu
        let windowMenuItem = NSMenuItem()
        windowMenuItem.representedObject = "menu.window"
        windowMenuItem.title = L("menu.window")
        let windowMenu = NSMenu(title: L("menu.window"))
        windowMenu.addLocalizedItem("menu.close", action: #selector(NSWindow.performClose(_:)), keyEquivalent: "w")
        windowMenu.addLocalizedItem("menu.minimize", action: #selector(NSWindow.performMiniaturize(_:)), keyEquivalent: "m")
        windowMenu.addItem(NSMenuItem.separator())
        let resetSizeItem = NSMenuItem(localizedKey: "menu.resetWindowSize", action: #selector(resetWindowSize), keyEquivalent: "")
        resetSizeItem.target = self
        windowMenu.addItem(resetSizeItem)
        windowMenuItem.submenu = windowMenu
        mainMenu.addItem(windowMenuItem)

        NSApp.mainMenu = mainMenu
    }

    // MARK: - Actions

    @objc private func toggleWindow() {
        // 以窗口是否实际可见判定收起：点击状态栏图标不会激活应用，依赖 NSApp.isActive 会在失焦后误判为显示
        if let window = mainWindow?.window, window.isVisible, window.occlusionState.contains(.visible) {
            mainWindow?.hideWindow(animated: false)
        } else {
            mainWindow?.showWindow(nil)
            NSApp.activate(ignoringOtherApps: true)
        }
    }

    @objc private func showAboutPanel() {
        dockCoordinator?.prepareForWindowPresentation()
        NSApp.activate(ignoringOtherApps: true)
        let options: [NSApplication.AboutPanelOptionKey: Any] = [
            .applicationIcon: NSApp.applicationIconImage as Any,
            .applicationName: "Lokii",
            .applicationVersion: Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "",
            .version: Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "",
            NSApplication.AboutPanelOptionKey(rawValue: "Copyright"): "\u{00A9} 2026 Lokii",
            .credits: NSAttributedString(
                string: L("about.tagline"),
                attributes: [.font: NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)]
            )
        ]
        NSApp.orderFrontStandardAboutPanel(options: options)
    }

    @objc func openPreferences() {
        showPreferences()
    }

    @objc private func rebuildIndex() {
        mainWindow?.triggerRebuildIndex()
    }

    @objc private func resetWindowSize() {
        mainWindow?.resetToDefaultWindowFrame()
    }

    /// 从搜索结果空态直达索引页
    func openIndexPreferences() {
        showPreferences(tab: "index")
    }

    private func showPreferences(tab: String? = nil) {
        dockCoordinator?.prepareForWindowPresentation()
        if preferencesController == nil {
            preferencesController = PreferencesWindowController(engine: engine)
        }
        if let tab {
            preferencesController?.selectTab(tab)
        }
        preferencesController?.showWindow(nil)
        preferencesController?.window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    @MainActor
    @objc private func checkForUpdates() {
        AppUpdater.shared.checkForUpdates(isUserInitiated: true) { [weak self] in
            self?.dockCoordinator?.prepareForWindowPresentation()
            NSApp.activate(ignoringOtherApps: true)
        }
    }

    @objc private func quitApp() {
        NSApp.terminate(nil)
    }
}

extension AppDelegate: NSMenuItemValidation {
    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        if menuItem.action == #selector(rebuildIndex) {
            return mainWindow?.canTriggerRebuild ?? false
        }
        return true
    }
}

// MARK: - NSMenuDelegate

extension AppDelegate: NSMenuDelegate {
    func menuDidClose(_ menu: NSMenu) {
        statusItem?.menu = nil  // Re-enable button action for next left-click
        statusItem?.button?.highlight(false)  // Fix highlight stuck bug (Pitfall #2)
    }
}

/// 应用启动入口，供可执行包装层调用以保证单实例锁在初始化任何核心服务前生效。
public enum LokiiApplication {
    public static func main() {
        SingleInstanceGuard.acquireOrActivateExistingAndExit()

        let app = NSApplication.shared
        let delegate = AppDelegate()
        app.delegate = delegate
        app.run()
    }
}
