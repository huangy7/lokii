import Cocoa
import XCTest
@testable import LokiiAppLib

final class ToolbarTests: XCTestCase {
    func testToolbarIdentifiersAndLocalization() throws {
        let searchFieldId = NSToolbarItem.Identifier("lokii.toolbar.searchField")
        XCTAssertEqual(searchFieldId.rawValue, "lokii.toolbar.searchField")
        XCTAssertEqual(NSToolbarItem.Identifier.toggleSidebar, .toggleSidebar)
        XCTAssertEqual(NSToolbarItem.Identifier.sidebarTrackingSeparator, .sidebarTrackingSeparator)

        let zhContent = try TestFixture.readRepoFile("Lokii/Lokii/Resources/zh-Hans.lproj/Localizable.strings")
        let enContent = try TestFixture.readRepoFile("Lokii/Lokii/Resources/en.lproj/Localizable.strings")

        let requiredKeys = [
            "tray.preferences",
            "tray.rebuildIndex",
            "tray.checkUpdates",
            "tray.quit",
            "search.toggleSidebar",
            "search.matchedCount",
            "search.matchedCount.one",
            "search.statusWithMatches",
            "window.resetSize",
            "window.viewOptions",
            "menu.zoomIn",
            "menu.zoomOut",
            "menu.actualSize",
            "density.compact",
            "density.standard",
            "density.comfortable",
            "density.large",
            "menu.preferences",
            "search.history.title",
            "search.history.empty",
            "search.history.clear",
            "search.history.removeItem",
        ]

        for key in requiredKeys {
            XCTAssertTrue(zhContent.contains("\"\(key)\""), "zh-Hans 必须包含 \(key)")
            XCTAssertTrue(enContent.contains("\"\(key)\""), "en 必须包含 \(key)")
        }

        XCTAssertTrue(zhContent.contains("切换侧边栏"))
        XCTAssertTrue(zhContent.contains("已匹配 %@ 项"))
        XCTAssertTrue(zhContent.contains("恢复默认窗口大小"))
        XCTAssertTrue(zhContent.contains("视图选项"))

        XCTAssertTrue(enContent.contains("Toggle Sidebar"))
        XCTAssertTrue(enContent.contains("%@ matches"))
        XCTAssertTrue(enContent.contains("Reset Window Size"))
        XCTAssertTrue(enContent.contains("View Options"))

        XCTAssertFalse(zhContent.contains("search.toggleInspector"))
        XCTAssertFalse(enContent.contains("search.toggleInspector"))
        XCTAssertTrue(zhContent.contains("\"tray.preferences\" = \"偏好设置…\";"))
        XCTAssertTrue(zhContent.contains("\"tray.checkUpdates\" = \"检查更新…\";"))
        XCTAssertTrue(zhContent.contains("\"tray.quit\" = \"退出\";"))
        XCTAssertTrue(enContent.contains("\"tray.preferences\" = \"Preferences…\";"))
        XCTAssertTrue(enContent.contains("\"tray.checkUpdates\" = \"Check for Updates…\";"))
        XCTAssertTrue(enContent.contains("\"tray.quit\" = \"Quit\";"))
    }

    func testCompositeStatusFormatting() {
        let indexedOnlyZh = "已索引 355,967 个文件"
        let indexedOnlyEn = "355,967 files indexed"
        let template = "%1$@ | %2$@"

        XCTAssertEqual(
            String(format: template, String(format: "已匹配 %@ 项", "49"), indexedOnlyZh),
            "已匹配 49 项 | 已索引 355,967 个文件"
        )
        XCTAssertEqual(
            String(format: template, String(format: "%@ matches", "49"), indexedOnlyEn),
            "49 matches | 355,967 files indexed"
        )
        XCTAssertEqual(
            String(format: template, "已匹配 1 项", indexedOnlyZh),
            "已匹配 1 项 | 已索引 355,967 个文件"
        )
        XCTAssertEqual(
            String(format: template, "1 match", indexedOnlyEn),
            "1 match | 355,967 files indexed"
        )
    }

    func testToolbarLayoutAndSourceInvariants() throws {
        final class MockToolbarDelegate: NSObject, NSToolbarDelegate {
            let splitView: NSSplitView
            let searchField: NSSearchField

            init(splitView: NSSplitView, searchField: NSSearchField) {
                self.splitView = splitView
                self.searchField = searchField
            }

            func toolbar(
                _ toolbar: NSToolbar,
                itemForItemIdentifier itemIdentifier: NSToolbarItem.Identifier,
                willBeInsertedIntoToolbar flag: Bool
            ) -> NSToolbarItem? {
                switch itemIdentifier {
                case .toggleSidebar:
                    let item = NSToolbarItem(itemIdentifier: itemIdentifier)
                    item.label = "切换侧边栏"
                    item.paletteLabel = "切换侧边栏"
                    item.toolTip = "切换侧边栏"
                    return item

                case .sidebarTrackingSeparator:
                    return NSTrackingSeparatorToolbarItem(
                        identifier: itemIdentifier,
                        splitView: splitView,
                        dividerIndex: 0
                    )

                case .init("lokii.toolbar.searchField"):
                    let container = NSView()
                    container.translatesAutoresizingMaskIntoConstraints = false
                    searchField.translatesAutoresizingMaskIntoConstraints = false
                    container.addSubview(searchField)
                    NSLayoutConstraint.activate([
                        container.heightAnchor.constraint(equalToConstant: 28),
                        container.widthAnchor.constraint(greaterThanOrEqualToConstant: 320),
                        searchField.widthAnchor.constraint(greaterThanOrEqualToConstant: 320),
                        searchField.leadingAnchor.constraint(equalTo: container.leadingAnchor),
                        searchField.trailingAnchor.constraint(equalTo: container.trailingAnchor),
                        searchField.centerYAnchor.constraint(equalTo: container.centerYAnchor),
                        searchField.heightAnchor.constraint(equalToConstant: 28),
                    ])
                    let item = NSToolbarItem(itemIdentifier: itemIdentifier)
                    item.view = container
                    item.label = "搜索"
                    item.paletteLabel = "搜索"
                    item.isNavigational = true
                    return item

                case .init("lokii.toolbar.resetWindowSize"):
                    let button = NSButton()
                    button.image = NSImage(systemSymbolName: "arrow.down.right.and.arrow.up.left", accessibilityDescription: "恢复默认窗口大小")
                    button.bezelStyle = .toolbar
                    button.isBordered = false
                    let item = NSToolbarItem(itemIdentifier: itemIdentifier)
                    item.view = button
                    item.label = "恢复默认窗口大小"
                    item.paletteLabel = "恢复默认窗口大小"
                    return item

                default:
                    return nil
                }
            }

            func toolbarDefaultItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
                [.toggleSidebar, .sidebarTrackingSeparator, .init("lokii.toolbar.searchField"), .init("lokii.toolbar.resetWindowSize")]
            }

            func toolbarAllowedItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
                [.toggleSidebar, .sidebarTrackingSeparator, .init("lokii.toolbar.searchField"), .init("lokii.toolbar.resetWindowSize")]
            }
        }

        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 900, height: 500),
            styleMask: [.titled, .closable, .resizable],
            backing: .buffered,
            defer: false
        )
        let toolbar = NSToolbar(identifier: "LokiiMainToolbar")
        let splitView = NSSplitView()
        splitView.isVertical = true
        let searchField = NSSearchField()
        searchField.translatesAutoresizingMaskIntoConstraints = false

        let delegate = MockToolbarDelegate(splitView: splitView, searchField: searchField)
        toolbar.delegate = delegate
        window.toolbar = toolbar
        window.layoutIfNeeded()
        window.display()

        let expectedIds: [NSToolbarItem.Identifier] = [
            .toggleSidebar,
            .sidebarTrackingSeparator,
            .init("lokii.toolbar.searchField"),
            .init("lokii.toolbar.resetWindowSize"),
        ]
        XCTAssertEqual(delegate.toolbarDefaultItemIdentifiers(toolbar), expectedIds)
        XCTAssertEqual(delegate.toolbarAllowedItemIdentifiers(toolbar), expectedIds)

        let toggleItem = delegate.toolbar(toolbar, itemForItemIdentifier: .toggleSidebar, willBeInsertedIntoToolbar: false)
        XCTAssertEqual(toggleItem?.itemIdentifier, .toggleSidebar)

        let trackingItem = delegate.toolbar(toolbar, itemForItemIdentifier: .sidebarTrackingSeparator, willBeInsertedIntoToolbar: false) as? NSTrackingSeparatorToolbarItem
        XCTAssertNotNil(trackingItem)
        XCTAssertEqual(trackingItem?.dividerIndex, 0)
        XCTAssertEqual(trackingItem?.splitView, splitView)

        let searchItem = delegate.toolbar(toolbar, itemForItemIdentifier: .init("lokii.toolbar.searchField"), willBeInsertedIntoToolbar: false)
        XCTAssertEqual(searchItem?.isNavigational, true)
        XCTAssertEqual(searchItem?.view?.subviews.contains(searchField), true)
        XCTAssertEqual(searchField.constraints.first { $0.firstAttribute == .height }?.constant, 28)
        XCTAssertEqual(
            searchField.constraints.first { $0.firstAttribute == .width && $0.relation == .greaterThanOrEqual }?.constant,
            320
        )

        let resetItem = delegate.toolbar(toolbar, itemForItemIdentifier: .init("lokii.toolbar.resetWindowSize"), willBeInsertedIntoToolbar: false)
        XCTAssertTrue(resetItem?.view is NSButton)

        final class MockTarget: NSObject {
            var sidebarToggled = false
            @objc func toggleSidebar(_ sender: Any?) { sidebarToggled = true }
        }
        let target = MockTarget()
        let actionDispatched = NSApplication.shared.sendAction(
            #selector(MockTarget.toggleSidebar(_:)),
            to: target,
            from: NSToolbarItem(itemIdentifier: .toggleSidebar)
        )
        XCTAssertTrue(actionDispatched)
        XCTAssertTrue(target.sidebarToggled)

        let swSource = try TestFixture.readRepoFile("Lokii/Lokii/SearchWindow.swift")
        let rvSource = try TestFixture.readRepoFile("Lokii/Lokii/SearchResultsViewController.swift")

        XCTAssertFalse(swSource.contains("toggleInspector"))
        XCTAssertFalse(swSource.contains("toggleInfoPanel"))
        XCTAssertFalse(swSource.contains("toggleSidebarButton"))
        XCTAssertFalse(swSource.contains("searchBarItem"))
        XCTAssertTrue(swSource.contains("static let searchField = NSToolbarItem.Identifier(\"lokii.toolbar.searchField\")"))
        XCTAssertTrue(swSource.contains("static let resetWindowSize = NSToolbarItem.Identifier(\"lokii.toolbar.resetWindowSize\")"))
        XCTAssertTrue(swSource.contains(".toggleSidebar"))
        XCTAssertTrue(swSource.contains(".sidebarTrackingSeparator"))
        XCTAssertTrue(swSource.contains(".resetWindowSize"))
        XCTAssertTrue(swSource.contains("NSTrackingSeparatorToolbarItem"))
        XCTAssertTrue(swSource.contains("dividerIndex: 0"))
        XCTAssertTrue(swSource.contains("item.isNavigational = true"))
        XCTAssertTrue(swSource.contains("greaterThanOrEqualToConstant: 320"))
        XCTAssertTrue(swSource.contains("equalToConstant: 28"))
        XCTAssertFalse(swSource.contains("lessThanOrEqualToConstant"))
        XCTAssertFalse(swSource.contains(".flexibleSpace"))
        XCTAssertTrue(swSource.contains("textformat.size"))
        XCTAssertTrue(swSource.contains("buildViewOptionsMenu"))
        XCTAssertTrue(swSource.contains("openPreferencesAction"))
        XCTAssertTrue(swSource.contains("toolbar.displayMode = .iconOnly"))
        XCTAssertTrue(swSource.contains("allowsDisplayModeCustomization = false"))

        XCTAssertTrue(swSource.contains("static let maxRecentSearches = 8"))
        XCTAssertFalse(swSource.contains("searchModeSelected"))
        XCTAssertTrue(swSource.contains("L(\"search.history.empty\")"))
        XCTAssertTrue(swSource.contains("alternateItem.isAlternate = true"))
        XCTAssertTrue(swSource.contains("alternateItem.keyEquivalentModifierMask = [.option]"))
        XCTAssertTrue(rvSource.contains("func searchResultsDidConsumeResult()"))
    }
}
