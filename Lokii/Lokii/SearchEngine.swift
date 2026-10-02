import Foundation

// MARK: - Config Model (mirrors Rust AppConfig JSON)

struct LokiiConfig: Codable {
    var general: GeneralConfig
    var index: IndexConfig
    var window: WindowConfig

    struct GeneralConfig: Codable {
        var language: String

        enum CodingKeys: String, CodingKey {
            case language
        }
    }

    struct IndexConfig: Codable {
        var indexPaths: [String]
        var exclude: [String]
        var includeHidden: Bool
        var maxResults: Int

        enum CodingKeys: String, CodingKey {
            case indexPaths = "index_paths"
            case exclude
            case includeHidden = "include_hidden"
            case maxResults = "max_results"
        }
    }

    struct WindowConfig: Codable {
        var hideOnBlur: Bool
        var rememberPosition: Bool

        enum CodingKeys: String, CodingKey {
            case hideOnBlur = "hide_on_blur"
            case rememberPosition = "remember_position"
        }
    }
}

/// Wraps LokiiCore (Rust via UniFFI) with a Swift-friendly async interface.
final class SearchEngine: @unchecked Sendable {
    private let core: LokiiCore
    private let queue = DispatchQueue(label: "com.lokii.engine", qos: .userInitiated)

    /// Posted on main thread when index progress updates.
    static let indexProgressNotification = Notification.Name("LokiiIndexProgress")
    /// Posted on main thread when index is ready. userInfo: ["total": UInt64]
    static let indexReadyNotification = Notification.Name("LokiiIndexReady")
    /// Posted on main thread when index is updated. userInfo: ["total": UInt64]
    static let indexUpdatedNotification = Notification.Name("LokiiIndexUpdated")
    /// Posted on main thread after initialize() determines startup mode. userInfo: ["isColdStart": Bool]
    static let startupModeNotification = Notification.Name("LokiiStartupMode")
    /// Posted on main thread when config.toml is reloaded.
    static let configReloadedNotification = Notification.Name("LokiiConfigReloaded")

    init() {
        core = LokiiCore()
    }

    /// Initialize the index on a background thread. Fires notifications for progress.
    ///
    /// Notification ordering: `startupModeNotification` is dispatched to the main
    /// thread **before** `core.initialize()` begins.  Because `initialize()` is a
    /// blocking call, the Rust-side `on_index_ready` callback dispatches its own
    /// `indexReadyNotification` to the main queue *during* that call.  By posting
    /// the startup-mode notification first, the main-queue ordering is guaranteed:
    ///   1. startupModeNotification  →  UI decides cold-start vs hot-start
    ///   2. indexReadyNotification   →  UI dismisses the correct indicator
    func initialize() {
        let handler = NotificationEventHandler()
        queue.async { [core] in
            // Peek at cache existence to determine startup mode BEFORE the
            // blocking initialize() call, so we can post the mode notification
            // to the main queue first.
            let configDir = FileManager.default.homeDirectoryForCurrentUser
                .appendingPathComponent(".config/lokii")
            let cacheExists = FileManager.default.fileExists(
                atPath: configDir.appendingPathComponent("index.cache").path
            )
            DispatchQueue.main.async {
                NotificationCenter.default.post(
                    name: SearchEngine.startupModeNotification,
                    object: nil,
                    userInfo: ["isColdStart": !cacheExists]
                )
            }

            let mode = core.initialize(eventHandler: handler)
            switch mode {
            case let .hotStart(cachedCount):
                print("[Lokii] Hot start: \(cachedCount) cached entries")
            case let .coldStart(scannedCount):
                print("[Lokii] Cold start: scanned \(scannedCount) entries")
            }
            core.startWatcher(eventHandler: handler)
            core.startConfigWatcher(eventHandler: handler)
        }
    }

    /// Search synchronously (fast — sub-millisecond for in-memory index).
    func search(query: String, mode: FfiSearchMode = .auto) -> FfiSearchResponse {
        core.search(query: query, mode: mode)
    }

    /// Get current index stats.
    var stats: FfiIndexStats {
        core.getStats()
    }

    /// Check if a background scan is currently running.
    var isScanning: Bool {
        core.isScanning()
    }

    /// Rebuild index from scratch on background thread.
    func rebuildIndex() {
        let handler = NotificationEventHandler(source: "rebuild")
        DispatchQueue.global(qos: .userInitiated).async { [core] in
            _ = core.rebuildIndex(eventHandler: handler)
        }
    }

    /// Check Full Disk Access permission.
    var hasFullDiskAccess: Bool {
        core.checkFda()
    }

    /// Open file with default app.
    func openFile(path: String) {
        _ = core.openFile(path: path)
    }

    /// Reveal file in Finder.
    func revealInFinder(path: String) {
        _ = core.revealInFinder(path: path)
    }

    /// Read current config as a decoded Swift struct.
    func getConfig() -> LokiiConfig? {
        let json = core.getConfigJson()
        guard let data = json.data(using: .utf8) else { return nil }
        return try? JSONDecoder().decode(LokiiConfig.self, from: data)
    }

    /// Encode and write config back to disk via FFI.
    func updateConfig(_ config: LokiiConfig) -> Bool {
        guard let data = try? JSONEncoder().encode(config),
              let json = String(data: data, encoding: .utf8) else { return false }
        return core.updateConfigFromJson(json: json)
    }
}

// MARK: - Event Handler (Rust → Swift callbacks)

private final class NotificationEventHandler: EventHandler {
    /// Source tag included in notifications so the UI can distinguish
    /// events from initialize vs rebuild.
    let source: String

    init(source: String = "initialize") {
        self.source = source
    }

    func onIndexProgress(phase: String, scannedFiles: UInt64) {
        DispatchQueue.main.async { [source] in
            NotificationCenter.default.post(
                name: SearchEngine.indexProgressNotification,
                object: nil,
                userInfo: ["phase": phase, "scannedFiles": scannedFiles, "source": source]
            )
        }
    }

    func onIndexReady(total: UInt64) {
        DispatchQueue.main.async { [source] in
            NotificationCenter.default.post(
                name: SearchEngine.indexReadyNotification,
                object: nil,
                userInfo: ["total": total, "source": source]
            )
        }
    }

    func onIndexUpdated(newTotal: UInt64) {
        DispatchQueue.main.async { [source] in
            NotificationCenter.default.post(
                name: SearchEngine.indexUpdatedNotification,
                object: nil,
                userInfo: ["total": newTotal, "source": source]
            )
        }
    }

    func onWatcherError(msg: String) {
        print("[Lokii] Watcher error: \(msg)")
    }

    func onConfigReloaded() {
        DispatchQueue.main.async {
            NotificationCenter.default.post(name: SearchEngine.configReloadedNotification, object: nil)
        }
        print("[Lokii] Config reloaded")
    }
}
