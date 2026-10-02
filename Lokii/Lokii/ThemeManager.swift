import AppKit

enum AppTheme: String, CaseIterable, Identifiable {
    case system = "system"
    case light = "light"
    case dark = "dark"

    var id: String { rawValue }

    var appearance: NSAppearance? {
        switch self {
        case .system:
            return nil
        case .light:
            return NSAppearance(named: .aqua)
        case .dark:
            return NSAppearance(named: .darkAqua)
        }
    }
}

final class ThemeManager: ObservableObject {
    static let shared = ThemeManager()

    private let userDefaultsKey = "appTheme"

    @Published private(set) var currentTheme: AppTheme

    private init() {
        let savedRaw = UserDefaults.standard.string(forKey: userDefaultsKey) ?? AppTheme.system.rawValue
        self.currentTheme = AppTheme(rawValue: savedRaw) ?? .system
    }

    func applyTheme(_ theme: AppTheme) {
        currentTheme = theme
        UserDefaults.standard.set(theme.rawValue, forKey: userDefaultsKey)
        applyAppearance(theme)
    }

    func applyCurrentTheme() {
        applyAppearance(currentTheme)
    }

    private func applyAppearance(_ theme: AppTheme) {
        DispatchQueue.main.async {
            NSApp.appearance = theme.appearance
        }
    }
}
