import AppKit

extension NSMenuItem {
    convenience init(localizedKey: String, action: Selector?, keyEquivalent: String) {
        self.init(title: L(localizedKey), action: action, keyEquivalent: keyEquivalent)
        representedObject = localizedKey
    }
}

extension NSMenu {
    @discardableResult
    func addLocalizedItem(_ key: String, action: Selector?, keyEquivalent: String) -> NSMenuItem {
        let item = NSMenuItem(localizedKey: key, action: action, keyEquivalent: keyEquivalent)
        addItem(item)
        return item
    }

    func refreshLocalizedTitles() {
        for item in items {
            if let key = item.representedObject as? String {
                item.title = L(key)
                item.submenu?.title = item.title
            }
            item.submenu?.refreshLocalizedTitles()
        }
    }
}
