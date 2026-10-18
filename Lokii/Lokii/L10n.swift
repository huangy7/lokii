import Foundation

// 运行时语言包：根据配置在进程内切换语言，绕开系统 AppleLanguages 机制。
// macOS 原生 .app 包在打包时将多语言资源放置于 Contents/Resources/*.lproj 中。
// 这里直接从 Bundle.main 内找到对应 .lproj bundle 加载。
enum L10n {
    static let languageChangedNotification = Notification.Name("LokiiLanguageChanged")
    private static var _bundle: Bundle?
    private static var configuredLanguage: String?

    private static var defaultBundle: Bundle {
        Bundle.main
    }

    static var bundle: Bundle {
        _bundle ?? defaultBundle
    }

    /// 在 app 启动最早期调用（applicationDidFinishLaunching 第一行）
    static func configure(language: String) {
        precondition(Thread.isMainThread)
        guard configuredLanguage != language else { return }
        _bundle = defaultBundle
        // SPM 打包时会把 lproj 目录名转为小写（zh-Hans → zh-hans）
        let candidates = [language, language.lowercased()]
        for name in candidates {
            if let url = defaultBundle.url(forResource: name, withExtension: "lproj"),
               let b = Bundle(url: url) {
                _bundle = b
                break
            }
        }
        configuredLanguage = language
        NotificationCenter.default.post(name: languageChangedNotification, object: nil)
    }
}

enum LocalizedText {
    case key(String)
    case formatted(String, String)
    case verbatim(String)

    var string: String {
        switch self {
        case .key(let key): return L(key)
        case .formatted(let key, let argument): return String(format: L(key), argument)
        case .verbatim(let text): return text
        }
    }
}

// 替代 NSLocalizedString(_:bundle:comment:) 的简写
@inline(__always)
func L(_ key: String, comment: String = "") -> String {
    NSLocalizedString(key, bundle: L10n.bundle, comment: comment)
}

// 文件计数千分位格式化；实例复用避免高频索引进度回调中反复构造
private let fileCountFormatter: NumberFormatter = {
    let formatter = NumberFormatter()
    formatter.numberStyle = .decimal
    return formatter
}()

func formatFileCount(_ count: UInt64) -> String {
    fileCountFormatter.string(from: NSNumber(value: count)) ?? "\(count)"
}
