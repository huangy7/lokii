import AppKit

extension KeyboardShortcuts.Name {
    static let toggleWindow = Self("toggleWindow", default: .init(.space, modifiers: [.command, .shift]))
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    private var mainWindow: SearchWindow?
    private var statusItem: NSStatusItem?
    private let engine = SearchEngine()
    private var preferencesController: PreferencesWindowController?

    func applicationDidFinishLaunching(_ notification: Notification) {
        let config = engine.getConfig()

        // 在创建任何 UI 之前加载语言包
        L10n.configure(language: config?.general.language ?? "zh-Hans")

        // 应用存储的外观主题
        ThemeManager.shared.applyCurrentTheme()

        // 根据配置设置应用激活策略（是否常驻 Dock）
        NSApp.setActivationPolicy((config?.general.showInDock ?? true) ? .regular : .accessory)

        // 注册应用支持向系统“服务（Services）”导出的数据类型
        NSApp.registerServicesMenuSendTypes([
            .fileURL,
            NSPasteboard.PasteboardType("NSFilenamesPboardType"),
            .string,
            NSPasteboard.PasteboardType("public.plain-text"),
            NSPasteboard.PasteboardType("public.directory"),
            NSPasteboard.PasteboardType("public.folder"),
            NSPasteboard.PasteboardType("public.item"),
            NSPasteboard.PasteboardType("public.data"),
            NSPasteboard.PasteboardType("com.apple.resolvable"),
        ], returnTypes: [])

        setupStatusItem()
        setupMainMenu()
        NotificationCenter.default.addObserver(self, selector: #selector(onLanguageChanged),
                                               name: L10n.languageChangedNotification, object: nil)
        registerGlobalShortcut()

        mainWindow = SearchWindow(engine: engine)
        mainWindow?.showWindow(nil)
        NSApp.activate(ignoringOtherApps: true)
        engine.initialize()

        // 热启动直接就绪场景下，延迟 0.3 秒主动弹出授权提醒浮窗
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { [weak self] in
            self?.checkAndPromptFDA()
        }

        AppUpdater.shared.start()
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        mainWindow?.showWindow(nil)
        return true
    }

    func applicationDidBecomeActive(_ notification: Notification) {
        clearFDAPromptIfGranted()
    }

    func applicationDidResignActive(_ notification: Notification) {
        if FDAGuideWindowController.isPresenting {
            return
        }
        guard engine.getConfig()?.window.hideOnBlur ?? true else { return }
        mainWindow?.window?.orderOut(nil)
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
            let menu = NSMenu()

            let trayPrefsItem = NSMenuItem(title: L("menu.preferences"), action: #selector(openPreferences), keyEquivalent: "")
            trayPrefsItem.target = self
            menu.addItem(trayPrefsItem)

            let checkUpdatesItem = NSMenuItem(title: L("menu.checkUpdates"), action: #selector(checkForUpdates), keyEquivalent: "")
            checkUpdatesItem.target = self
            menu.addItem(checkUpdatesItem)

            menu.addItem(NSMenuItem.separator())

            let quitItem = NSMenuItem(title: L("menu.quit"), action: #selector(quitApp), keyEquivalent: "")
            quitItem.target = self
            menu.addItem(quitItem)

            menu.delegate = self
            statusItem?.menu = menu
            statusItem?.button?.performClick(nil)
        } else {
            toggleWindow()
        }
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
        windowMenuItem.submenu = windowMenu
        mainMenu.addItem(windowMenuItem)

        NSApp.mainMenu = mainMenu
    }

    // MARK: - Actions

    @objc private func toggleWindow() {
        if let window = mainWindow?.window, window.isVisible, NSApp.isActive {
            window.orderOut(nil)
        } else {
            mainWindow?.showWindow(nil)
            NSApp.activate(ignoringOtherApps: true)
        }
    }

    @objc private func showAboutPanel() {
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
        if preferencesController == nil {
            preferencesController = PreferencesWindowController(engine: engine)
        }
        preferencesController?.showWindow(nil)
        preferencesController?.window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    @MainActor
    @objc private func checkForUpdates() {
        NSApp.activate(ignoringOtherApps: true)
        AppUpdater.shared.checkForUpdates(isUserInitiated: true)
    }

    @objc private func quitApp() {
        NSApp.terminate(nil)
    }
}

// MARK: - NSMenuDelegate

extension AppDelegate: NSMenuDelegate {
    func menuDidClose(_ menu: NSMenu) {
        statusItem?.menu = nil  // Re-enable button action for next left-click
        statusItem?.button?.highlight(false)  // Fix highlight stuck bug (Pitfall #2)
    }
}
