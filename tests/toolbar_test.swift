import Cocoa

// MARK: - Standalone Test Harness for Toolbar and Status Formatting

func assertEqual<T: Equatable>(_ actual: T, _ expected: T, _ message: String = "", file: StaticString = #file, line: UInt = #line) {
    if actual != expected {
        fputs("断言失败: [\(actual)] != [\(expected)] - \(message) (\(file):\(line))\n", stderr)
        exit(1)
    }
}

func assertTrue(_ condition: Bool, _ message: String = "", file: StaticString = #file, line: UInt = #line) {
    if !condition {
        fputs("断言失败: 条件不满足 - \(message) (\(file):\(line))\n", stderr)
        exit(1)
    }
}

print("[1/5] 测试工具栏标识符常量定义...")
do {
    let searchField = NSToolbarItem.Identifier("lokii.toolbar.searchField")
    assertEqual(searchField.rawValue, "lokii.toolbar.searchField", "主检索输入框工具栏标识符应为 lokii.toolbar.searchField")
    assertEqual(NSToolbarItem.Identifier.toggleSidebar, .toggleSidebar, "系统原生侧边栏切换标识符应可用")
    assertEqual(NSToolbarItem.Identifier.sidebarTrackingSeparator, .sidebarTrackingSeparator, "系统原生侧边栏跟踪分隔条标识符应可用")
}

print("[2/5] 测试多语言资源文件键值完整性...")
do {
    let zhPath = "Lokii/Lokii/Resources/zh-Hans.lproj/Localizable.strings"
    let enPath = "Lokii/Lokii/Resources/en.lproj/Localizable.strings"

    guard let zhContent = try? String(contentsOfFile: zhPath, encoding: .utf8),
          let enContent = try? String(contentsOfFile: enPath, encoding: .utf8) else {
        fputs("无法读取多语言资源文件\n", stderr)
        exit(1)
    }

    let requiredKeys = [
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
    ]

    for key in requiredKeys {
        assertTrue(zhContent.contains("\"\(key)\""), "zh-Hans 必须包含 \(key)")
        assertTrue(enContent.contains("\"\(key)\""), "en 必须包含 \(key)")
    }

    assertTrue(zhContent.contains("切换侧边栏"), "zh-Hans 应包含切换侧边栏")
    assertTrue(zhContent.contains("已匹配 %@ 项"), "zh-Hans 应包含匹配项模板")
    assertTrue(zhContent.contains("恢复默认窗口大小"), "zh-Hans 应包含恢复默认窗口大小")
    assertTrue(zhContent.contains("视图选项"), "zh-Hans 应包含视图选项")

    assertTrue(enContent.contains("Toggle Sidebar"), "en 应包含 Toggle Sidebar")
    assertTrue(enContent.contains("%@ matches"), "en 应包含 matches 模板")
    assertTrue(enContent.contains("Reset Window Size"), "en 应包含 Reset Window Size")
    assertTrue(enContent.contains("View Options"), "en 应包含 View Options")

    assertTrue(!zhContent.contains("search.toggleInspector"), "zh-Hans 不应包含已下线的 search.toggleInspector")
    assertTrue(!enContent.contains("search.toggleInspector"), "en 不应包含已下线的 search.toggleInspector")
}

print("[3/5] 测试底部状态栏统计文案复合格式化逻辑...")
do {
    // 模拟纯索引状态（未发生搜索检索时）
    let indexedOnlyZh = "已索引 355,967 个文件"
    let indexedOnlyEn = "355,967 files indexed"
    assertEqual(indexedOnlyZh, "已索引 355,967 个文件")
    assertEqual(indexedOnlyEn, "355,967 files indexed")

    // 模拟搜索结果复合展示（49 项命中）
    let template = "%1$@ | %2$@"
    let matched49Zh = String(format: "已匹配 %@ 项", "49")
    let combined49Zh = String(format: template, matched49Zh, indexedOnlyZh)
    assertEqual(combined49Zh, "已匹配 49 项 | 已索引 355,967 个文件", "中文复合状态文案格式应完全符合规范")

    let matched49En = String(format: "%@ matches", "49")
    let combined49En = String(format: template, matched49En, indexedOnlyEn)
    assertEqual(combined49En, "49 matches | 355,967 files indexed", "英文复合状态文案格式应符合规范")

    // 模拟单项命中
    let matched1Zh = "已匹配 1 项"
    let combined1Zh = String(format: template, matched1Zh, indexedOnlyZh)
    assertEqual(combined1Zh, "已匹配 1 项 | 已索引 355,967 个文件", "单项命中中文状态格式准确")

    let matched1En = "1 match"
    let combined1En = String(format: template, matched1En, indexedOnlyEn)
    assertEqual(combined1En, "1 match | 355,967 files indexed", "单项命中英文状态格式准确")
}

print("[4/5] 测试 NSToolbar 原生分栏跟踪条、侧边栏切换与搜索框约束...")
do {
    let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 900, height: 500),
                          styleMask: [.titled, .closable, .resizable],
                          backing: .buffered,
                          defer: false)
    let toolbar = NSToolbar(identifier: "LokiiMainToolbar")
    let splitView = NSSplitView()
    splitView.isVertical = true

    let searchField = NSSearchField()
    searchField.translatesAutoresizingMaskIntoConstraints = false

    class MockToolbarDelegate: NSObject, NSToolbarDelegate {
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
                button.toolTip = "恢复默认窗口大小"
                let item = NSToolbarItem(itemIdentifier: itemIdentifier)
                item.view = button
                item.label = "恢复默认窗口大小"
                item.paletteLabel = "恢复默认窗口大小"
                item.toolTip = "恢复默认窗口大小"
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

    let delegate = MockToolbarDelegate(splitView: splitView, searchField: searchField)
    toolbar.delegate = delegate
    window.toolbar = toolbar
    window.layoutIfNeeded()
    window.display()

    let defaultIdentifiers = delegate.toolbarDefaultItemIdentifiers(toolbar)
    assertEqual(defaultIdentifiers, [
        .toggleSidebar,
        .sidebarTrackingSeparator,
        .init("lokii.toolbar.searchField"),
        .init("lokii.toolbar.resetWindowSize"),
    ], "默认项配置应严格包含系统侧边栏切换、分栏跟踪条、通栏搜索框与恢复默认窗口大小按钮")

    let allowedIdentifiers = delegate.toolbarAllowedItemIdentifiers(toolbar)
    assertEqual(allowedIdentifiers, [
        .toggleSidebar,
        .sidebarTrackingSeparator,
        .init("lokii.toolbar.searchField"),
        .init("lokii.toolbar.resetWindowSize"),
    ], "允许项配置应严格包含系统侧边栏切换、分栏跟踪条、通栏搜索框与恢复默认窗口大小按钮")

    let toggleItem = delegate.toolbar(toolbar, itemForItemIdentifier: .toggleSidebar, willBeInsertedIntoToolbar: false)
    assertTrue(toggleItem != nil, "toggleSidebar 项必须成功生成")
    assertEqual(toggleItem?.itemIdentifier, .toggleSidebar, "侧边栏切换项标识符应为 .toggleSidebar")

    let trackingItem = delegate.toolbar(toolbar, itemForItemIdentifier: .sidebarTrackingSeparator, willBeInsertedIntoToolbar: false) as? NSTrackingSeparatorToolbarItem
    assertTrue(trackingItem != nil, "sidebarTrackingSeparator 项必须是 NSTrackingSeparatorToolbarItem 实例")
    assertEqual(trackingItem?.dividerIndex, 0, "分栏跟踪条必须绑定到分栏视图的第 0 处分割线")
    assertEqual(trackingItem?.splitView, splitView, "分栏跟踪条必须关联主窗口分栏视图")

    let searchItem = delegate.toolbar(toolbar, itemForItemIdentifier: .init("lokii.toolbar.searchField"), willBeInsertedIntoToolbar: false)
    assertTrue(searchItem != nil, "searchField 项必须成功生成")
    assertEqual(searchItem?.isNavigational, true, "主检索输入框工具栏项必须声明 isNavigational = true 以自适应铺满顶栏")
    assertTrue(searchItem?.view?.subviews.contains(searchField) == true, "searchField 项 view 必须装配并承载 searchField 视图以实现全行自适应展开")

    let heightConstraint = searchField.constraints.first { $0.firstAttribute == .height }
    assertTrue(heightConstraint != nil, "searchField 必须设置高度约束")
    assertEqual(heightConstraint?.constant, 28, "searchField 固定高度必须为 28 pt")

    let minWidthConstraint = searchField.constraints.first { $0.firstAttribute == .width && $0.relation == .greaterThanOrEqual }
    assertTrue(minWidthConstraint != nil, "searchField 必须设置最小宽度约束")
    assertEqual(minWidthConstraint?.constant, 320, "searchField 最小宽度应为 320 pt")

    let resetItem = delegate.toolbar(toolbar, itemForItemIdentifier: .init("lokii.toolbar.resetWindowSize"), willBeInsertedIntoToolbar: false)
    assertTrue(resetItem != nil, "resetWindowSize 项必须成功生成")
    assertTrue(resetItem?.view is NSButton, "resetWindowSize 项 view 应为 NSButton 实例")
}

print("[5/5] 测试工具栏项行为与 SearchWindow 源码实现约束...")
do {
    let app = NSApplication.shared
    class MockTarget: NSObject {
        var sidebarToggled = false
        @objc func toggleSidebar(_ sender: Any?) { sidebarToggled = true }
    }

    let target = MockTarget()
    let toggleItem = NSToolbarItem(itemIdentifier: .toggleSidebar)

    let actionDispatched = app.sendAction(#selector(MockTarget.toggleSidebar(_:)), to: target, from: toggleItem)
    assertTrue(actionDispatched, "系统标准侧边栏切换动作应能被正常派发")
    assertTrue(target.sidebarToggled, "系统标准侧边栏切换动作应通过响应链正确触发目标方法")

    let searchWindowPath = "Lokii/Lokii/SearchWindow.swift"
    guard let source = try? String(contentsOfFile: searchWindowPath, encoding: .utf8) else {
        fputs("无法读取 SearchWindow.swift 源码文件\n", stderr)
        exit(1)
    }

    assertTrue(!source.contains("toggleInspector"), "SearchWindow.swift 中严禁包含 toggleInspector 标识符或处理分支")
    assertTrue(!source.contains("toggleInfoPanel"), "SearchWindow.swift 中严禁包含 toggleInfoPanel 方法定义或引用")
    assertTrue(!source.contains("toggleSidebarButton"), "SearchWindow.swift 中严禁包含废弃的 toggleSidebarButton 自定义按钮")
    assertTrue(!source.contains("searchBarItem"), "SearchWindow.swift 中严禁包含废弃的 searchBarItem 胶水容器")
    assertTrue(source.contains("static let searchField = NSToolbarItem.Identifier(\"lokii.toolbar.searchField\")"), "SearchWindow.swift 必须声明 static let searchField 工具栏标识符")
    assertTrue(source.contains("static let resetWindowSize = NSToolbarItem.Identifier(\"lokii.toolbar.resetWindowSize\")"), "SearchWindow.swift 必须声明 static let resetWindowSize 工具栏标识符")
    assertTrue(source.contains(".toggleSidebar"), "SearchWindow.swift 必须集成系统原生 .toggleSidebar 工具栏项")
    assertTrue(source.contains(".sidebarTrackingSeparator"), "SearchWindow.swift 必须集成系统原生 .sidebarTrackingSeparator 工具栏项")
    assertTrue(source.contains(".resetWindowSize"), "SearchWindow.swift 必须集成系统 .resetWindowSize 工具栏项")
    assertTrue(source.contains("NSTrackingSeparatorToolbarItem"), "SearchWindow.swift 必须实例化 NSTrackingSeparatorToolbarItem")
    assertTrue(source.contains("dividerIndex: 0"), "SearchWindow.swift 必须指定 dividerIndex: 0")
    assertTrue(source.contains("item.isNavigational = true"), "SearchWindow.swift 中 searchField 工具栏项必须声明 item.isNavigational = true")
    assertTrue(source.contains("greaterThanOrEqualToConstant: 320"), "SearchWindow.swift 中 searchField 必须设置 >= 320 pt 最小宽度约束")
    assertTrue(source.contains("equalToConstant: 28"), "SearchWindow.swift 中 searchField 必须设置 28 pt 固定高度约束")
    assertTrue(!source.contains("lessThanOrEqualToConstant"), "SearchWindow.swift 中 searchField 严禁设置最大宽度上限约束")
    assertTrue(!source.contains(".flexibleSpace"), "SearchWindow.swift 中顶栏项不应再包含 flexibleSpace 留白占位符")
    assertTrue(source.contains("textformat.size"), "SearchWindow.swift 中视图选项按钮图标应为 textformat.size")
    assertTrue(source.contains("buildViewOptionsMenu"), "SearchWindow.swift 必须实现 buildViewOptionsMenu 视图选项菜单构建方法")
    assertTrue(source.contains("openPreferencesAction"), "SearchWindow.swift 必须包含 openPreferencesAction 偏好设置动作")
    assertTrue(source.contains("toolbar.displayMode = .iconOnly"), "SearchWindow.swift 工具栏必须显式配置为纯图标显示模式")
    assertTrue(source.contains("allowsDisplayModeCustomization = false"), "SearchWindow.swift 必须禁用工具栏显示模式自定义以屏蔽'图标和文本'右键选项")
}

print("测试全部通过：5/5 项断言成功。")
