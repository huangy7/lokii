import Foundation

// 运行时语言包：根据配置在进程内切换语言，绕开系统 AppleLanguages 机制。
// macOS 原生 .app 包在打包时将多语言资源放置于 Contents/Resources/*.lproj 中。
// 这里直接从 Bundle.main 内找到对应 .lproj bundle 加载。
enum L10n {
    private static var _bundle: Bundle?

    private static var defaultBundle: Bundle {
        Bundle.main
    }

    static var bundle: Bundle {
        _bundle ?? defaultBundle
    }

    /// 在 app 启动最早期调用（applicationDidFinishLaunching 第一行）
    static func configure(language: String) {
        // SPM 打包时会把 lproj 目录名转为小写（zh-Hans → zh-hans）
        let candidates = [language, language.lowercased()]
        for name in candidates {
            if let url = defaultBundle.url(forResource: name, withExtension: "lproj"),
               let b = Bundle(url: url) {
                _bundle = b
                return
            }
        }
        _bundle = defaultBundle
    }
}

// 替代 NSLocalizedString(_:bundle:comment:) 的简写
@inline(__always)
func L(_ key: String, comment: String = "") -> String {
    NSLocalizedString(key, bundle: L10n.bundle, comment: comment)
}
