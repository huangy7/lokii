import Foundation

// MARK: - Config Model (mirrors Rust AppConfig JSON)

struct LokiiConfig: Codable {
    var general: GeneralConfig
    var index: IndexConfig
    var window: WindowConfig
    var content: ContentConfig

    init(
        general: GeneralConfig = GeneralConfig(),
        index: IndexConfig = IndexConfig(),
        window: WindowConfig = WindowConfig(),
        content: ContentConfig = ContentConfig()
    ) {
        self.general = general
        self.index = index
        self.window = window
        self.content = content
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.general = try container.decodeIfPresent(GeneralConfig.self, forKey: .general) ?? GeneralConfig()
        self.index = try container.decodeIfPresent(IndexConfig.self, forKey: .index) ?? IndexConfig()
        self.window = try container.decodeIfPresent(WindowConfig.self, forKey: .window) ?? WindowConfig()
        self.content = try container.decodeIfPresent(ContentConfig.self, forKey: .content) ?? ContentConfig()
    }

    enum CodingKeys: String, CodingKey {
        case general, index, window, content
    }

    struct ContentConfig: Codable, Equatable {
        var enabled: Bool
        var indexOffice: Bool
        var indexPdf: Bool
        var indexImages: Bool
        var respectGitignore: Bool
        var maxLruMb: Int

        enum CodingKeys: String, CodingKey {
            case enabled
            case indexOffice = "index_office"
            case indexPdf = "index_pdf"
            case indexImages = "index_images"
            case respectGitignore = "respect_gitignore"
            case maxLruMb = "max_lru_mb"
        }

        init(
            enabled: Bool = true,
            indexOffice: Bool = true,
            indexPdf: Bool = true,
            indexImages: Bool = false,
            respectGitignore: Bool = true,
            maxLruMb: Int = 32
        ) {
            self.enabled = enabled
            self.indexOffice = indexOffice
            self.indexPdf = indexPdf
            self.indexImages = indexImages
            self.respectGitignore = respectGitignore
            self.maxLruMb = maxLruMb
        }

        init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            self.enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? true
            self.indexOffice = try container.decodeIfPresent(Bool.self, forKey: .indexOffice) ?? true
            self.indexPdf = try container.decodeIfPresent(Bool.self, forKey: .indexPdf) ?? true
            self.indexImages = try container.decodeIfPresent(Bool.self, forKey: .indexImages) ?? false
            self.respectGitignore = try container.decodeIfPresent(Bool.self, forKey: .respectGitignore) ?? true
            self.maxLruMb = try container.decodeIfPresent(Int.self, forKey: .maxLruMb) ?? 32
        }
    }

    struct GeneralConfig: Codable {
        var language: String
        var showInDock: Bool
        var terminalBundleId: String

        enum CodingKeys: String, CodingKey {
            case language
            case showInDock = "show_in_dock"
            case terminalBundleId = "terminal_bundle_id"
        }

        init(language: String = "zh-Hans", showInDock: Bool = true, terminalBundleId: String = "com.apple.Terminal") {
            self.language = language
            self.showInDock = showInDock
            self.terminalBundleId = terminalBundleId
        }

        init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            self.language = try container.decodeIfPresent(String.self, forKey: .language) ?? "zh-Hans"
            self.showInDock = try container.decodeIfPresent(Bool.self, forKey: .showInDock) ?? true
            self.terminalBundleId = try container.decodeIfPresent(String.self, forKey: .terminalBundleId) ?? "com.apple.Terminal"
        }
    }

    struct IndexConfig: Codable {
        var indexPaths: [String]
        var exclude: [String]
        var excludePaths: [String]
        var includeHidden: Bool
        var maxResults: Int
        var networkRescanMinutes: Int

        enum CodingKeys: String, CodingKey {
            case indexPaths = "index_paths"
            case exclude
            case excludePaths = "exclude_paths"
            case includeHidden = "include_hidden"
            case maxResults = "max_results"
            case networkRescanMinutes = "network_rescan_minutes"
        }

        init(
            indexPaths: [String] = [],
            exclude: [String] = [],
            excludePaths: [String] = [],
            includeHidden: Bool = false,
            maxResults: Int = 1000,
            networkRescanMinutes: Int = 5
        ) {
            self.indexPaths = indexPaths
            self.exclude = exclude
            self.excludePaths = excludePaths
            self.includeHidden = includeHidden
            self.maxResults = maxResults
            self.networkRescanMinutes = networkRescanMinutes
        }

        init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            self.indexPaths = try container.decodeIfPresent([String].self, forKey: .indexPaths) ?? []
            self.exclude = try container.decodeIfPresent([String].self, forKey: .exclude) ?? []
            self.excludePaths = try container.decodeIfPresent([String].self, forKey: .excludePaths) ?? []
            self.includeHidden = try container.decodeIfPresent(Bool.self, forKey: .includeHidden) ?? false
            self.maxResults = try container.decodeIfPresent(Int.self, forKey: .maxResults) ?? 1000
            self.networkRescanMinutes = try container.decodeIfPresent(Int.self, forKey: .networkRescanMinutes) ?? 5
        }
    }

    struct WindowConfig: Codable {
        var hideOnBlur: Bool
        var rememberPosition: Bool

        enum CodingKeys: String, CodingKey {
            case hideOnBlur = "hide_on_blur"
            case rememberPosition = "remember_position"
        }

        init(hideOnBlur: Bool = false, rememberPosition: Bool = false) {
            self.hideOnBlur = hideOnBlur
            self.rememberPosition = rememberPosition
        }

        init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            self.hideOnBlur = try container.decodeIfPresent(Bool.self, forKey: .hideOnBlur) ?? false
            self.rememberPosition = try container.decodeIfPresent(Bool.self, forKey: .rememberPosition) ?? false
        }
    }
}

/// Wraps LokiiCore (Rust via UniFFI) with a Swift-friendly async interface.
final class SearchEngine: @unchecked Sendable {
    private let core: LokiiCore
    private let queue = DispatchQueue(label: "com.lokii.engine", qos: .userInitiated)
    private let richDocWorker: RichDocumentExtractionCoordinator

    /// Posted on main thread when index progress updates.
    static let indexProgressNotification = Notification.Name("LokiiIndexProgress")
    /// Posted on main thread when index is ready. userInfo: ["total": UInt64]
    static let indexReadyNotification = Notification.Name("LokiiIndexReady")
    /// Posted on main thread when index is updated. userInfo: ["total": UInt64]
    static let indexUpdatedNotification = Notification.Name("LokiiIndexUpdated")
    /// Posted on main thread when content index live count updates. userInfo: ["liveDocs": UInt64]
    static let contentIndexUpdatedNotification = Notification.Name("LokiiContentIndexUpdated")
    /// Posted on main thread after initialize() determines startup mode. userInfo: ["isColdStart": Bool]
    static let startupModeNotification = Notification.Name("LokiiStartupMode")
    static let initializationFailedNotification = Notification.Name("LokiiInitializationFailed")
    /// Posted on main thread when config.toml is reloaded.
    static let configReloadedNotification = Notification.Name("LokiiConfigReloaded")

    init() {
        let core = LokiiCore()
        self.core = core
        self.richDocWorker = RichDocumentExtractionCoordinator(core: core)
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
        let handler = NotificationEventHandler(source: "initialize", engine: self)
        queue.async { [core] in
            // Bind the CLI socket before the blocking initialize(): during a cold-start scan the
            // CLI then gets empty results instead of "cannot connect", and a failed init no longer
            // leaves the socket missing forever.
            core.startQueryServer()

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
            case let .failed(message):
                DispatchQueue.main.async {
                    NotificationCenter.default.post(
                        name: SearchEngine.initializationFailedNotification,
                        object: nil,
                        userInfo: ["message": message]
                    )
                }
                return
            }
            core.startWatcher(eventHandler: handler)
            core.startConfigWatcher(eventHandler: handler)
        }
    }

    /// Search synchronously (fast — sub-millisecond for in-memory index).
    func search(
        query: String,
        mode: FfiSearchMode = .auto,
        caseSensitivity: FfiCaseSensitivity = .auto
    ) -> FfiSearchResponse {
        core.search(query: query, mode: mode, caseSensitivity: caseSensitivity)
    }

    /// 快速判断是否含显式正文检索指令（`content:` / `grep:` / `c:` / `cregex:` / `sym:`）。
    func hasContentDirective(query: String) -> Bool {
        core.hasContentDirective(query: query)
    }

    /// 执行受时间预算约束的混合内容检索（支持无锁热取消）。
    func searchContent(
        query: String,
        caseSensitivity: FfiCaseSensitivity = .auto,
        forceContent: Bool = false,
        forceRegex: Bool = false,
        forceSymbol: Bool = false,
        maxResults: UInt32 = 200,
        budgetMs: UInt32 = 250
    ) -> FfiContentSearchResponse {
        core.searchContent(
            query: query,
            caseSensitivity: caseSensitivity,
            forceContent: forceContent,
            forceRegex: forceRegex,
            forceSymbol: forceSymbol,
            maxResults: maxResults,
            budgetMs: budgetMs
        )
    }

    /// 原子取消进行中的上一轮内容检证。
    func cancelContentSearch() {
        core.cancelContentSearch()
    }

    /// 针对单个命中路径提取全量匹配行切片（供预览展开或详细视图使用）。
    func getContentSnippets(
        path: String,
        query: String,
        caseSensitivity: FfiCaseSensitivity = .auto,
        forceContent: Bool = false,
        forceRegex: Bool = false,
        forceSymbol: Bool = false,
        maxMatches: UInt32 = 16
    ) -> [FfiContentLineMatch] {
        core.getContentSnippets(
            path: path,
            query: query,
            caseSensitivity: caseSensitivity,
            forceContent: forceContent,
            forceRegex: forceRegex,
            forceSymbol: forceSymbol,
            maxMatches: maxMatches
        )
    }

    /// 获取内容索引实时统计。
    var contentStats: FfiContentStats {
        core.getContentStats()
    }

    /// 清空内容解压缓存与增量片段统计。
    func clearContentCache() {
        core.clearContentCache()
    }

    /// 手动触发一次全量内容索引差异同步与富文档补扫。
    func triggerContentResync() {
        core.triggerContentResync()
        scheduleRichDocumentExtraction(delaySeconds: 1.0)
    }

    func scheduleRichDocumentExtraction(delaySeconds: Double = 1.5) {
        richDocWorker.schedule(delaySeconds: delaySeconds, configProvider: { [weak self] in
            self?.getConfig()?.content
        })
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
        let handler = NotificationEventHandler(source: "rebuild", engine: self)
        DispatchQueue.global(qos: .userInitiated).async { [core] in
            _ = core.rebuildIndex(eventHandler: handler)
        }
    }

    /// Save the index with its replay checkpoint (blocking; call on quit).
    func persistIndex() {
        core.persistIndex()
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
        let success = core.updateConfigFromJson(json: json)
        if success {
            DispatchQueue.main.async { [weak self] in
                NotificationCenter.default.post(name: Self.configReloadedNotification, object: nil)
                self?.scheduleRichDocumentExtraction(delaySeconds: 1.0)
            }
        }
        return success
    }
}

// MARK: - Isolated Rich Document & OCR Worker

/// 低优先级富文档与图片 OCR 隔离调度器：
/// 将 PDFKit 与 Apple Vision 识别完全隔离在独立辅助子进程 `LokiiContentHelper` 中，
/// 配合双层超时看门狗与可疑文件隔离清单，确保畸形文档卡死或崩溃不影响主界面内存与响应。
private final class RichDocumentExtractionCoordinator: @unchecked Sendable {
    private struct HelperOutput: Decodable {
        let ok: Bool
        let pages: UInt32
        let text: String?
        let error: String?
    }

    private let core: LokiiCore
    private let queue = DispatchQueue(label: "com.lokii.content.richworker", qos: .utility)
    private let stateLock = NSLock()
    private var scheduledWork: DispatchWorkItem?
    private var isProcessing = false
    private var suspectPaths: Set<String>
    private let suspectFileURL: URL

    init(core: LokiiCore) {
        self.core = core
        let configDir = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".config/lokii")
        self.suspectFileURL = configDir.appendingPathComponent("content_suspects.txt")
        if let raw = try? String(contentsOf: suspectFileURL, encoding: .utf8) {
            self.suspectPaths = Set(raw.split(separator: "\n").map(String.init))
        } else {
            self.suspectPaths = []
        }
    }

    func schedule(delaySeconds: Double, configProvider: @escaping () -> LokiiConfig.ContentConfig?) {
        queue.async { [weak self] in
            guard let self else { return }
            self.scheduledWork?.cancel()
            let item = DispatchWorkItem { [weak self] in
                self?.runBatch(configProvider: configProvider)
            }
            self.scheduledWork = item
            self.queue.asyncAfter(deadline: .now() + delaySeconds, execute: item)
        }
    }

    private func runBatch(configProvider: @escaping () -> LokiiConfig.ContentConfig?) {
        stateLock.lock()
        if isProcessing {
            stateLock.unlock()
            return
        }
        isProcessing = true
        stateLock.unlock()

        defer {
            stateLock.lock()
            isProcessing = false
            stateLock.unlock()
        }

        guard let cfg = configProvider(),
              cfg.enabled,
              cfg.indexPdf || cfg.indexImages,
              let helperURL = resolveHelperURL() else {
            return
        }

        let batchSize: UInt32 = 16
        let candidates = core.listRichDocumentCandidates(
            includePdf: cfg.indexPdf,
            includeImages: cfg.indexImages,
            maxItems: batchSize
        )
        guard !candidates.isEmpty else { return }

        var processedAny = false
        for path in candidates {
            if isSuspect(path) { continue }
            guard let attrs = try? FileManager.default.attributesOfItem(atPath: path),
                  let size = (attrs[.size] as? NSNumber)?.uint64Value else {
                continue
            }
            let mtime = Int64(((attrs[.modificationDate] as? Date)?.timeIntervalSince1970) ?? 0)
            let ext = (path as NSString).pathExtension.lowercased()
            let isPdf = (ext == "pdf")
            let modeArg = isPdf ? "--pdf" : "--image"
            let sourceKind: FfiContentSourceKind = isPdf ? .pdf : .image

            guard let output = runHelper(helperURL: helperURL, modeArg: modeArg, path: path),
                  output.ok else {
                markSuspect(path)
                continue
            }

            let text = output.text ?? ""
            if core.ingestRichDocument(
                path: path,
                sourceKind: sourceKind,
                mtimeSecs: mtime,
                sizeBytes: size,
                decodedText: text,
                auxMeta: output.pages
            ) {
                processedAny = true
            }
        }

        if processedAny && candidates.count == Int(batchSize) {
            schedule(delaySeconds: 0.3, configProvider: configProvider)
        }
    }

    private func runHelper(helperURL: URL, modeArg: String, path: String) -> HelperOutput? {
        let process = Process()
        process.executableURL = helperURL
        process.arguments = [modeArg, path]
        process.qualityOfService = .utility

        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = FileHandle.nullDevice

        do {
            try process.run()
        } catch {
            return nil
        }

        // 双重看门狗：辅助进程自身 alarm(90)，宿主额外设置 95s 终止兜底避免僵尸管道挂死。
        let watchdog = DispatchSource.makeTimerSource(queue: DispatchQueue.global(qos: .utility))
        watchdog.schedule(deadline: .now() + 95)
        watchdog.setEventHandler {
            if process.isRunning {
                process.terminate()
            }
        }
        watchdog.resume()

        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        watchdog.cancel()

        guard process.terminationReason == .exit && process.terminationStatus == 0 else {
            return nil
        }
        return try? JSONDecoder().decode(HelperOutput.self, from: data)
    }

    private func resolveHelperURL() -> URL? {
        let fm = FileManager.default
        let bundled = Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/LokiiContentHelper")
        if fm.isExecutableFile(atPath: bundled.path) {
            return bundled
        }
        if let sibling = Bundle.main.executableURL?.deletingLastPathComponent().appendingPathComponent("LokiiContentHelper"),
           fm.isExecutableFile(atPath: sibling.path) {
            return sibling
        }
        return nil
    }

    private func isSuspect(_ path: String) -> Bool {
        stateLock.lock()
        defer { stateLock.unlock() }
        return suspectPaths.contains(path)
    }

    private func markSuspect(_ path: String) {
        stateLock.lock()
        let inserted = suspectPaths.insert(path).inserted
        let all = suspectPaths
        stateLock.unlock()
        guard inserted else { return }
        let dir = suspectFileURL.deletingLastPathComponent()
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let body = all.sorted().joined(separator: "\n") + "\n"
        try? body.write(to: suspectFileURL, atomically: true, encoding: .utf8)
    }
}

// MARK: - Event Handler (Rust → Swift callbacks)

private final class NotificationEventHandler: EventHandler, @unchecked Sendable {
    /// Source tag included in notifications so the UI can distinguish
    /// events from initialize vs rebuild.
    let source: String
    weak var engine: SearchEngine?

    init(source: String = "initialize", engine: SearchEngine? = nil) {
        self.source = source
        self.engine = engine
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
        DispatchQueue.main.async { [source, weak engine] in
            NotificationCenter.default.post(
                name: SearchEngine.indexReadyNotification,
                object: nil,
                userInfo: ["total": total, "source": source]
            )
            engine?.scheduleRichDocumentExtraction(delaySeconds: 2.0)
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

    func onContentIndexUpdated(liveDocs: UInt64) {
        DispatchQueue.main.async { [weak engine] in
            NotificationCenter.default.post(
                name: SearchEngine.contentIndexUpdatedNotification,
                object: nil,
                userInfo: ["liveDocs": liveDocs]
            )
            engine?.scheduleRichDocumentExtraction(delaySeconds: 1.5)
        }
    }

    func onWatcherError(msg: String) {
        print("[Lokii] Watcher error: \(msg)")
    }

    func onConfigReloaded() {
        DispatchQueue.main.async { [weak engine] in
            NotificationCenter.default.post(name: SearchEngine.configReloadedNotification, object: nil)
            engine?.scheduleRichDocumentExtraction(delaySeconds: 1.0)
        }
        print("[Lokii] Config reloaded")
    }
}
