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
        // 在创建任何 UI 之前加载语言包
        let lang = engine.getConfig()?.general.language ?? "zh-Hans"
        L10n.configure(language: lang)

        // 应用存储的外观主题
        ThemeManager.shared.applyCurrentTheme()

        // LSUIElement apps must explicitly set activation policy to accept keyboard input
        NSApp.setActivationPolicy(.accessory)

        setupStatusItem()
        setupMainMenu()
        registerGlobalShortcut()

        mainWindow = SearchWindow(engine: engine)
        mainWindow?.showWindow(nil)
        NSApp.activate(ignoringOtherApps: true)
        checkAndPromptFDA()

        engine.initialize()

        // 后台静默检查更新（延迟 3 秒，避免阻塞冷启动性能）
        DispatchQueue.main.asyncAfter(deadline: .now() + 3.0) {
            Task { @MainActor in
                AppUpdater.shared.checkForUpdates(isUserInitiated: false)
            }
        }
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        mainWindow?.showWindow(nil)
        return true
    }

    func applicationDidBecomeActive(_ notification: Notification) {
        guard engine.hasFullDiskAccess else { return }
        UserDefaults.standard.removeObject(forKey: "FDAPromptDismissed")
        FDAGuideWindowController.closeIfOpen()
    }

    func applicationDidResignActive(_ notification: Notification) {
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

    private func checkAndPromptFDA() {
        if engine.hasFullDiskAccess {
            UserDefaults.standard.removeObject(forKey: "FDAPromptDismissed")
            FDAGuideWindowController.closeIfOpen()
            return
        }

        guard !UserDefaults.standard.bool(forKey: "FDAPromptDismissed") else { return }
        FDAGuideWindowController.show()
    }

    // MARK: - Main Menu

    private func setupMainMenu() {
        let mainMenu = NSMenu()

        // App menu
        let appMenuItem = NSMenuItem()
        let appMenu = NSMenu()

        // About Lokii — custom About panel with version, copyright, description
        let aboutItem = NSMenuItem(title: L("menu.about"), action: #selector(showAboutPanel), keyEquivalent: "")
        aboutItem.target = self
        appMenu.addItem(aboutItem)

        let checkUpdatesItem = NSMenuItem(title: L("menu.checkUpdates"), action: #selector(checkForUpdates), keyEquivalent: "")
        checkUpdatesItem.target = self
        appMenu.addItem(checkUpdatesItem)

        appMenu.addItem(NSMenuItem.separator())

        // Preferences... with Cmd+,
        let prefsItem = NSMenuItem(title: L("menu.preferences"), action: #selector(openPreferences), keyEquivalent: ",")
        prefsItem.target = self
        appMenu.addItem(prefsItem)

        appMenu.addItem(NSMenuItem.separator())

        // Quit Lokii with Cmd+Q
        appMenu.addItem(withTitle: L("menu.quit"), action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")

        appMenuItem.submenu = appMenu
        mainMenu.addItem(appMenuItem)

        // Edit menu (for copy/paste in search field)
        let editMenuItem = NSMenuItem()
        let editMenu = NSMenu(title: L("menu.edit"))
        editMenu.addItem(withTitle: L("menu.cut"), action: #selector(NSText.cut(_:)), keyEquivalent: "x")
        editMenu.addItem(withTitle: L("menu.copy"), action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        editMenu.addItem(withTitle: L("menu.paste"), action: #selector(NSText.paste(_:)), keyEquivalent: "v")
        editMenu.addItem(withTitle: L("menu.selectAll"), action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
        editMenuItem.submenu = editMenu
        mainMenu.addItem(editMenuItem)

        // Window menu
        let windowMenuItem = NSMenuItem()
        let windowMenu = NSMenu(title: L("menu.window"))
        windowMenu.addItem(withTitle: L("menu.close"), action: #selector(NSWindow.performClose(_:)), keyEquivalent: "w")
        windowMenu.addItem(withTitle: L("menu.minimize"), action: #selector(NSWindow.performMiniaturize(_:)), keyEquivalent: "m")
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

    @objc private func openPreferences() {
        if preferencesController == nil {
            preferencesController = PreferencesWindowController(engine: engine)
        }
        preferencesController?.showWindow(nil)
        preferencesController?.window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    @MainActor
    @objc private func checkForUpdates() {
        if preferencesController == nil {
            preferencesController = PreferencesWindowController(engine: engine)
        }
        preferencesController?.showWindow(tab: "about")
        preferencesController?.window?.makeKeyAndOrderFront(nil)
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
