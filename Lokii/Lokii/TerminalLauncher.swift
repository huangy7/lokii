import AppKit
import UniformTypeIdentifiers

/// 终端应用元数据描述模型
struct TerminalAppItem: Identifiable {
    /// 唯一标识：Bundle Identifier（如 "com.googlecode.iterm2"）或自定应用程序物理路径
    let id: String
    /// 本地化显示名（如 "Terminal", "iTerm"）
    let name: String
    /// 应用程序包物理路径 URL
    let appURL: URL
}

/// 终端应用调度与探活中介者
final class TerminalLauncher {
    /// 系统自带终端固定标识符，作为全局默认配置与终极兜底
    static let systemTerminalBundleId = "com.apple.Terminal"
    /// 系统自带终端的固定安装路径，用于应用索引不可用时的物理兜底
    static let systemTerminalURL = URL(fileURLWithPath: "/System/Applications/Utilities/Terminal.app")
    /// 批量唤起终端视窗的安全上限，避免多选大量条目时引发并发风暴与系统卡顿
    static let maxBatchOpenCount = 5

    /// 主流终端候选池（按知名度与开发者使用频率排序）
    static let presetTerminals: [(name: String, bundleId: String)] = [
        ("Terminal", "com.apple.Terminal"),
        ("iTerm2", "com.googlecode.iterm2"),
        ("Ghostty", "com.mitchellh.ghostty"),
        ("Warp", "dev.warp.Warp-GDK"),
        ("Alacritty", "io.alacritty"),
        ("Kitty", "net.kovidgoyal.kitty"),
        ("WezTerm", "com.github.wez.wezterm"),
        ("Hyper", "co.zeit.hyper")
    ]

    /// 动态探测本机实际已安装的终端列表，并按需追加生效中的自定义终端项
    static func installedTerminals(selectedId: String? = nil) -> [TerminalAppItem] {
        var items: [TerminalAppItem] = []

        for preset in presetTerminals {
            if let appURL = NSWorkspace.shared.urlForApplication(withBundleIdentifier: preset.bundleId) {
                let displayName = FileManager.default.displayName(atPath: appURL.path)
                items.append(TerminalAppItem(
                    id: preset.bundleId,
                    name: displayName.isEmpty ? preset.name : displayName,
                    appURL: appURL
                ))
            }
        }

        // 边界守卫与兜底保障：系统终端必须始终在列表中可用
        if !items.contains(where: { $0.id == systemTerminalBundleId }) {
            let fallbackURL = resolveApplicationURL(for: systemTerminalBundleId).appURL
            items.insert(TerminalAppItem(
                id: systemTerminalBundleId,
                name: "Terminal",
                appURL: fallbackURL
            ), at: 0)
        }

        // 若用户配置的自选终端不在预设列表中，验证物理存在性后作为自定义项呈现
        if let current = selectedId, !current.isEmpty,
           !items.contains(where: { $0.id == current }),
           let customItem = resolveCustomAppItem(for: current) {
            items.append(customItem)
        }

        return items
    }

    /// 解析自定终端应用元数据
    static func resolveCustomAppItem(for identifier: String) -> TerminalAppItem? {
        let appURL: URL?
        if identifier.hasPrefix("/") {
            appURL = URL(fileURLWithPath: identifier)
        } else {
            appURL = NSWorkspace.shared.urlForApplication(withBundleIdentifier: identifier)
        }

        guard let resolvedURL = appURL,
              FileManager.default.fileExists(atPath: resolvedURL.path) else {
            return nil
        }

        let displayName = FileManager.default.displayName(atPath: resolvedURL.path)
        return TerminalAppItem(
            id: identifier,
            name: displayName,
            appURL: resolvedURL
        )
    }

    /// 通过系统文件选择面板选取第三方自定终端应用包（.app）
    static func chooseCustomTerminal(window: NSWindow? = nil, completion: @escaping (TerminalAppItem?) -> Void) {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.directoryURL = URL(fileURLWithPath: "/Applications")
        if #available(macOS 11.0, *) {
            panel.allowedContentTypes = [UTType.application, UTType.applicationBundle]
        } else {
            panel.allowedFileTypes = ["app"]
        }

        let handleSelection: (NSApplication.ModalResponse) -> Void = { response in
            guard response == .OK, let selectedURL = panel.url else {
                completion(nil)
                return
            }

            // 优先读取应用 Info.plist 中声明的 Bundle ID，规避路径重命名导致的失效；无 ID 时回退至绝对路径
            let bundleId = Bundle(url: selectedURL)?.bundleIdentifier ?? selectedURL.path
            let displayName = FileManager.default.displayName(atPath: selectedURL.path)
            let item = TerminalAppItem(
                id: bundleId,
                name: displayName,
                appURL: selectedURL
            )
            completion(item)
        }

        if let parentWindow = window {
            panel.beginSheetModal(for: parentWindow, completionHandler: handleSelection)
        } else {
            let response = panel.runModal()
            handleSelection(response)
        }
    }

    /// 解析指定标识符对应的应用程序物理 URL，具备自愈降级回退能力
    static func resolveApplicationURL(for identifier: String?) -> (appURL: URL, isFallback: Bool) {
        let systemFallback = NSWorkspace.shared.urlForApplication(withBundleIdentifier: systemTerminalBundleId)
            ?? systemTerminalURL

        guard let rawId = identifier, !rawId.isEmpty else {
            return (systemFallback, false)
        }

        if rawId.hasPrefix("/") {
            let pathURL = URL(fileURLWithPath: rawId)
            if FileManager.default.fileExists(atPath: pathURL.path) {
                return (pathURL, false)
            }
        } else if let appURL = NSWorkspace.shared.urlForApplication(withBundleIdentifier: rawId) {
            return (appURL, false)
        }

        return (systemFallback, true)
    }

    /// 目标路径守卫：若是普通文件，则向上推导至其所在父级目录
    static func directoryURL(for url: URL) -> URL {
        var isDir: ObjCBool = false
        if FileManager.default.fileExists(atPath: url.path, isDirectory: &isDir), isDir.boolValue {
            return url
        }
        return url.deletingLastPathComponent()
    }

    /// 按原始顺序去重出目标对应的目录列表，并可提前截断至指定上限
    static func uniqueDirectories(from urls: [URL], limit: Int = .max) -> [URL] {
        var seenPaths = Set<String>()
        var directories: [URL] = []
        for url in urls where directories.count < limit {
            let directory = directoryURL(for: url)
            if seenPaths.insert(directory.path).inserted {
                directories.append(directory)
            }
        }
        return directories
    }

    /// 在指定目录唤起终端应用，支持零权限派发与运行时异常自动降级
    static func open(at targetURL: URL, preferredIdentifier: String? = nil) {
        let directoryURL = directoryURL(for: targetURL)
        let effectiveId = preferredIdentifier ?? systemTerminalBundleId
        let (primaryAppURL, _) = resolveApplicationURL(for: effectiveId)

        let config = NSWorkspace.OpenConfiguration()
        config.activates = true

        NSWorkspace.shared.open([directoryURL], withApplicationAt: primaryAppURL, configuration: config) { _, error in
            if let error = error {
                NSLog("Terminal launch error with \(primaryAppURL.path): \(error.localizedDescription). Falling back to system Terminal.")
                let (fallbackURL, _) = resolveApplicationURL(for: systemTerminalBundleId)
                if fallbackURL.path != primaryAppURL.path {
                    // 运行时自愈二次重试：当第三方终端拒收目录或崩溃报错时，以系统自带终端重新派发
                    NSWorkspace.shared.open([directoryURL], withApplicationAt: fallbackURL, configuration: config)
                }
            }
        }
    }

    /// 批量唤起多个条目对应的终端，具备目录去重与最大并发限流保护
    static func openBatch(at targetURLs: [URL], preferredIdentifier: String? = nil) {
        // 默认终端标识与并发上限均在批次内解析一次，避免每个目录重复回读配置
        let effectiveId = preferredIdentifier ?? systemTerminalBundleId
        for directory in uniqueDirectories(from: targetURLs, limit: maxBatchOpenCount) {
            open(at: directory, preferredIdentifier: effectiveId)
        }
    }
}
