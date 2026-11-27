import AppKit
import XCTest
@testable import LokiiAppLib

final class FloatingSearchBarTests: XCTestCase {
    private let standardScreen = ScreenMetrics(
        frame: NSRect(x: 0, y: 0, width: 1512, height: 982),
        visibleFrame: NSRect(x: 0, y: 58, width: 1512, height: 900)
    )

    private let secondaryScreen = ScreenMetrics(
        frame: NSRect(x: 1512, y: 0, width: 1920, height: 1080),
        visibleFrame: NSRect(x: 1512, y: 0, width: 1920, height: 1055)
    )

    private let narrowScreen = ScreenMetrics(
        frame: NSRect(x: 0, y: 0, width: 420, height: 500),
        visibleFrame: NSRect(x: 0, y: 40, width: 420, height: 430)
    )

    func testWindowPresentationModeStorePersistenceAndNotifications() {
        let suiteName = "FloatingSearchBarTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let center = NotificationCenter()
        let store = WindowPresentationModeStore(defaults: defaults, notificationCenter: center)

        XCTAssertEqual(store.currentMode, .standardWindow)

        var receivedCount = 0
        let observer = center.addObserver(
            forName: WindowPresentationModeStore.didChangeNotification,
            object: store,
            queue: nil
        ) { _ in
            receivedCount += 1
        }
        defer { center.removeObserver(observer) }

        store.currentMode = .standardWindow
        XCTAssertEqual(receivedCount, 0)

        store.currentMode = .floatingBar
        XCTAssertEqual(store.currentMode, .floatingBar)
        XCTAssertEqual(receivedCount, 1)

        store.currentMode = .floatingBar
        XCTAssertEqual(receivedCount, 1)

        let reloaded = WindowPresentationModeStore(defaults: defaults, notificationCenter: center)
        XCTAssertEqual(reloaded.currentMode, .floatingBar)
    }

    func testFloatingBarPositionStoreSaveLoadAndClear() {
        let suiteName = "FloatingBarPositionStoreTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let store = FloatingBarPositionStore(defaults: defaults)
        XCTAssertNil(store.savedTopLeft())

        store.saveTopLeft(NSPoint(x: 312.4, y: 745.6))
        XCTAssertEqual(store.savedTopLeft(), NSPoint(x: 312, y: 746))

        let reloaded = FloatingBarPositionStore(defaults: defaults)
        XCTAssertEqual(reloaded.savedTopLeft(), NSPoint(x: 312, y: 746))

        store.clearTopLeft()
        XCTAssertNil(store.savedTopLeft())
    }

    func testFloatingSearchBarLayoutHeightCornerRadiusAndTopAnchoredResize() {
        XCTAssertEqual(FloatingSearchBarLayout.expandedChromeHeight, 73)
        XCTAssertEqual(FloatingSearchBarLayout.cornerRadius(for: .collapsed), 29)
        XCTAssertEqual(FloatingSearchBarLayout.cornerRadius(for: .results(count: 3)), 24)
        XCTAssertEqual(FloatingSearchBarLayout.cornerRadius(for: .message), 24)

        let collapsedHeight = FloatingSearchBarLayout.targetHeight(
            for: .collapsed,
            maxVisibleHeight: standardScreen.visibleFrame.height
        )
        XCTAssertEqual(collapsedHeight, 58)

        let oneResultHeight = FloatingSearchBarLayout.targetHeight(
            for: .results(count: 1),
            maxVisibleHeight: standardScreen.visibleFrame.height
        )
        XCTAssertEqual(oneResultHeight, 73 + 54)

        let fourResultHeight = FloatingSearchBarLayout.targetHeight(
            for: .results(count: 4),
            maxVisibleHeight: standardScreen.visibleFrame.height
        )
        XCTAssertEqual(fourResultHeight, 73 + 4 * 54)

        let sevenResultHeight = FloatingSearchBarLayout.targetHeight(
            for: .results(count: 7),
            maxVisibleHeight: standardScreen.visibleFrame.height
        )
        let fiftyResultHeight = FloatingSearchBarLayout.targetHeight(
            for: .results(count: 50),
            maxVisibleHeight: standardScreen.visibleFrame.height
        )
        XCTAssertEqual(sevenResultHeight, 73 + 7 * 54)
        XCTAssertEqual(fiftyResultHeight, sevenResultHeight)

        // 短屏幕应向下取整显示完整整数行，绝不在首屏露出半行
        XCTAssertEqual(
            FloatingSearchBarLayout.visibleRowCount(for: 20, maxVisibleHeight: narrowScreen.visibleFrame.height),
            4
        )
        XCTAssertEqual(
            FloatingSearchBarLayout.targetHeight(for: .results(count: 20), maxVisibleHeight: narrowScreen.visibleFrame.height),
            73 + 4 * 54
        )

        let messageHeight = FloatingSearchBarLayout.targetHeight(
            for: .message,
            maxVisibleHeight: standardScreen.visibleFrame.height
        )
        XCTAssertEqual(messageHeight, 73 + 76)

        // 展开内联玻璃筛选条时的高度与整数行对齐（单细分隔线）
        XCTAssertEqual(FloatingSearchBarLayout.expandedChromeHeight(filterBarExpanded: true), 111)
        XCTAssertEqual(FloatingSearchBarLayout.cornerRadius(for: .collapsed, filterBarExpanded: true), 24)
        XCTAssertEqual(
            FloatingSearchBarLayout.targetHeight(
                for: .collapsed,
                maxVisibleHeight: standardScreen.visibleFrame.height,
                filterBarExpanded: true
            ),
            103
        )
        XCTAssertEqual(
            FloatingSearchBarLayout.targetHeight(
                for: .results(count: 1),
                maxVisibleHeight: standardScreen.visibleFrame.height,
                filterBarExpanded: true
            ),
            111 + 54
        )
        XCTAssertEqual(
            FloatingSearchBarLayout.targetHeight(
                for: .results(count: 7),
                maxVisibleHeight: standardScreen.visibleFrame.height,
                filterBarExpanded: true
            ),
            111 + 7 * 54
        )
        XCTAssertEqual(
            FloatingSearchBarLayout.visibleRowCount(
                for: 20,
                maxVisibleHeight: narrowScreen.visibleFrame.height,
                filterBarExpanded: true
            ),
            3
        )

        let initial = FloatingSearchBarLayout.initialFrame(on: standardScreen, state: .collapsed)
        XCTAssertEqual(initial.width, 580)
        XCTAssertEqual(initial.height, 58)
        XCTAssertEqual(initial.midX, standardScreen.visibleFrame.midX, accuracy: 1)

        let expanded = FloatingSearchBarLayout.resizedFrame(
            preservingTopOf: initial,
            newHeight: sevenResultHeight,
            on: standardScreen
        )
        XCTAssertEqual(expanded.maxY, initial.maxY, accuracy: 1)
        XCTAssertEqual(expanded.height, sevenResultHeight)

        // 贴近屏幕底部时应防止底边溢出可见区域
        let nearBottom = NSRect(x: 200, y: 120, width: 580, height: 58)
        let clampedBottom = FloatingSearchBarLayout.resizedFrame(
            preservingTopOf: nearBottom,
            newHeight: sevenResultHeight,
            on: standardScreen
        )
        XCTAssertGreaterThanOrEqual(
            clampedBottom.minY,
            standardScreen.visibleFrame.minY + FloatingSearchBarLayout.bottomScreenMargin
        )

        // 窄屏宽度应自适应收缩
        let narrowWidth = FloatingSearchBarLayout.panelWidth(for: narrowScreen)
        XCTAssertEqual(narrowWidth, 388)
    }

    func testSnappedScrollOffsetYAlignsToRowBoundaries() {
        let rowHeight: CGFloat = 54
        let viewportHeight = rowHeight * 7

        // 不足一页时不产生滚动偏移
        XCTAssertEqual(
            FloatingSearchBarLayout.snappedScrollOffsetY(
                currentY: 18,
                contentHeight: rowHeight * 3,
                viewportHeight: viewportHeight
            ),
            0
        )

        // 半行偏移就近吸附至整行边界
        XCTAssertEqual(
            FloatingSearchBarLayout.snappedScrollOffsetY(
                currentY: 20,
                contentHeight: rowHeight * 12,
                viewportHeight: viewportHeight
            ),
            0
        )
        XCTAssertEqual(
            FloatingSearchBarLayout.snappedScrollOffsetY(
                currentY: 32,
                contentHeight: rowHeight * 12,
                viewportHeight: viewportHeight
            ),
            54
        )

        // 底部边界钳制
        XCTAssertEqual(
            FloatingSearchBarLayout.snappedScrollOffsetY(
                currentY: 999,
                contentHeight: rowHeight * 12,
                viewportHeight: viewportHeight
            ),
            rowHeight * 5
        )
    }

    func testResolveShowFramePositionMemoryAndTrayAnchoring() {
        let screens = [standardScreen, secondaryScreen]
        let trayAnchorNearRight = NSRect(x: 1420, y: 958, width: 28, height: 24)
        let savedTopLeft = NSPoint(x: 180, y: 720)

        // 1. 开启记住位置且有合法记录时，优先恢复上次顶边与左缘坐标
        let rememberedCollapsed = FloatingSearchBarLayout.resolveShowFrame(
            state: .collapsed,
            rememberPosition: true,
            savedTopLeft: savedTopLeft,
            anchorRect: trayAnchorNearRight,
            targetScreen: standardScreen,
            allScreens: screens
        )
        XCTAssertEqual(rememberedCollapsed.minX, 180)
        XCTAssertEqual(rememberedCollapsed.maxY, 720)
        XCTAssertEqual(rememberedCollapsed.height, 58)

        let rememberedExpanded = FloatingSearchBarLayout.resolveShowFrame(
            state: .results(count: 3),
            rememberPosition: true,
            savedTopLeft: savedTopLeft,
            anchorRect: trayAnchorNearRight,
            targetScreen: standardScreen,
            allScreens: screens
        )
        XCTAssertEqual(rememberedExpanded.minX, 180)
        XCTAssertEqual(rememberedExpanded.maxY, 720)

        // 2. 开启记住位置但记录落在已断开显示器时，回退至托盘锚点停靠
        let disconnectedSaved = NSPoint(x: -1400, y: 800)
        let fallbackToTray = FloatingSearchBarLayout.resolveShowFrame(
            state: .collapsed,
            rememberPosition: true,
            savedTopLeft: disconnectedSaved,
            anchorRect: trayAnchorNearRight,
            targetScreen: standardScreen,
            allScreens: screens
        )
        XCTAssertEqual(
            fallbackToTray.maxX,
            standardScreen.visibleFrame.maxX - FloatingSearchBarLayout.horizontalScreenMargin
        )
        XCTAssertEqual(
            fallbackToTray.maxY,
            trayAnchorNearRight.minY - FloatingSearchBarLayout.trayAnchorGap
        )

        // 3. 关闭记住位置时忽略已保存坐标，点击托盘直接贴在托盘图标下方
        let trayDropped = FloatingSearchBarLayout.resolveShowFrame(
            state: .collapsed,
            rememberPosition: false,
            savedTopLeft: savedTopLeft,
            anchorRect: trayAnchorNearRight,
            targetScreen: standardScreen,
            allScreens: screens
        )
        XCTAssertEqual(
            trayDropped.maxY,
            trayAnchorNearRight.minY - FloatingSearchBarLayout.trayAnchorGap
        )
        XCTAssertLessThanOrEqual(
            trayDropped.maxX,
            standardScreen.visibleFrame.maxX - FloatingSearchBarLayout.horizontalScreenMargin
        )

        // 4. 快捷键唤起（无托盘锚点且未记住位置）回归中上居中
        let hotkeyDefault = FloatingSearchBarLayout.resolveShowFrame(
            state: .collapsed,
            rememberPosition: false,
            savedTopLeft: savedTopLeft,
            anchorRect: nil,
            targetScreen: standardScreen,
            allScreens: screens
        )
        XCTAssertEqual(
            hotkeyDefault,
            FloatingSearchBarLayout.initialFrame(on: standardScreen, state: .collapsed)
        )
    }

    func testFloatingSearchPanelIsBorderlessFloatingAndExcludedFromDockCount() {
        let panel = FloatingSearchPanel(contentRect: NSRect(x: 100, y: 100, width: 580, height: 58))
        XCTAssertFalse(panel.styleMask.contains(.titled))
        XCTAssertFalse(panel.styleMask.contains(.fullSizeContentView))
        XCTAssertTrue(panel.styleMask.contains(.borderless))
        XCTAssertEqual(panel.styleMask, [.borderless])
        XCTAssertTrue(panel.canBecomeKey)
        XCTAssertTrue(panel.canBecomeMain)
        XCTAssertEqual(panel.level, .floating)

        let snapshot = DockWindowSnapshot(
            className: NSStringFromClass(type(of: panel)),
            level: panel.level,
            isVisible: true,
            isMiniaturized: false,
            hasTitledStyle: panel.styleMask.contains(.titled),
            isExcludedAuxiliary: false
        )
        XCTAssertFalse(snapshot.countsAsOpenUserWindow)
    }

    func testFloatingSearchResultCellViewTwoLineAndSelectionHintConfiguration() {
        let home = NSHomeDirectory()
        let fileResult = TestFixture.searchResult(
            name: "ReportFinal.pdf",
            path: "\(home)/Documents/Finance/ReportFinal.pdf",
            extension: "pdf",
            isDir: false,
            size: 2048,
            modified: 1_762_609_500,
            matchPositions: [FfiMatchRange(start: 0, end: 6)],
            parentMatchPositions: [FfiMatchRange(start: UInt32((home + "/Documents/").utf8.count), end: UInt32((home + "/Documents/Finance").utf8.count))]
        )

        let cell = FloatingSearchResultCellView(frame: NSRect(x: 0, y: 0, width: 540, height: 54))
        cell.configure(result: fileResult, icon: nil, isOffline: false)
        cell.setRowSelected(false)
        cell.layoutSubtreeIfNeeded()

        XCTAssertEqual(cell.nameLabel.stringValue, "ReportFinal.pdf")
        XCTAssertTrue(cell.pathLabel.stringValue.contains("Finance"))
        XCTAssertTrue(cell.pathLabel.stringValue.contains(" · "))
        XCTAssertFalse(cell.isRowSelected)
        XCTAssertEqual(cell.nameLabel.alphaValue, 1.0, accuracy: 0.001)
        // 验证文件名横跨完整内容区宽度，不被次行内容拽窄截断
        XCTAssertGreaterThan(cell.nameLabel.frame.width, 400)

        // 选中行切入强调态高对比白色字样
        cell.setRowSelected(true)
        XCTAssertTrue(cell.isRowSelected)
        XCTAssertEqual(
            cell.nameLabel.attributedStringValue.attribute(.foregroundColor, at: 0, effectiveRange: nil) as? NSColor,
            .white
        )
        XCTAssertEqual(
            cell.nameLabel.attributedStringValue.attribute(.foregroundColor, at: 7, effectiveRange: nil) as? NSColor,
            NSColor.white.withAlphaComponent(0.94)
        )

        // 离线条目附加离线标识并整体降透明
        let dirOfflineResult = TestFixture.searchResult(
            name: "Archive",
            path: "/Volumes/NAS/Archive",
            isDir: true,
            size: 0,
            isOffline: true
        )
        cell.configure(result: dirOfflineResult, icon: nil, isOffline: true)
        cell.setRowSelected(true)

        XCTAssertEqual(cell.nameLabel.stringValue, "Archive")
        XCTAssertTrue(cell.pathLabel.stringValue.contains("NAS"))
        XCTAssertTrue(cell.pathLabel.stringValue.contains(L("search.result.offline")))
        XCTAssertEqual(cell.iconView.alphaValue, 0.55, accuracy: 0.001)
    }

    func testFloatingInlineFilterBarAndGlassEmptyState() {
        let filterBar = FloatingInlineFilterBarView(frame: NSRect(x: 0, y: 0, width: 580, height: 38))
        filterBar.layoutSubtreeIfNeeded()

        let pillButtons = filterBar.allFilterPillButtonsForTesting
        let sizeButton = filterBar.sizeFilterButtonForTesting
        XCTAssertEqual(pillButtons.count, 7)
        XCTAssertEqual(pillButtons[0].visualStyle, .neutralActive)
        for typeButton in pillButtons.dropFirst() {
            XCTAssertEqual(typeButton.visualStyle, .inactive)
        }
        XCTAssertEqual(sizeButton.visualStyle, .inactive)

        // 验证初始状态视觉样式，并在中英文真实文案下校验全部 7 颗筛选胶囊与右侧大小按钮均完整可见且不重叠
        let localizedCases: [([String], String)] = [
            (["全部", "代码", "文档", "图片", "多媒体", "压缩包", "文件夹"], "大小"),
            (["All", "Code", "Documents", "Images", "Media", "Archives", "Folders"], "Size"),
        ]
        for (chipTitles, sizeTitle) in localizedCases {
            for (idx, title) in chipTitles.enumerated() {
                pillButtons[idx].title = title
            }
            sizeButton.title = sizeTitle
            filterBar.layoutSubtreeIfNeeded()

            let sizeFrameInBar = sizeButton.convert(sizeButton.bounds, to: filterBar)
            for button in pillButtons + [sizeButton] {
                XCTAssertFalse(button.isHidden)
                let frameInBar = button.convert(button.bounds, to: filterBar)
                XCTAssertGreaterThan(frameInBar.width, 20)
                XCTAssertGreaterThanOrEqual(frameInBar.width, button.intrinsicContentSize.width - 0.5)
                XCTAssertGreaterThanOrEqual(frameInBar.height, 24)
                XCTAssertGreaterThanOrEqual(frameInBar.minX, 0)
                XCTAssertLessThanOrEqual(frameInBar.maxX, filterBar.bounds.width)
            }
            if let lastPillFrame = pillButtons.last.map({ $0.convert($0.bounds, to: filterBar) }) {
                XCTAssertLessThanOrEqual(lastPillFrame.maxX, sizeFrameInBar.minX - 4)
            }
        }
        filterBar.refreshLocalization()

        var lastEmitted: FilterState?
        filterBar.onStateChanged = { lastEmitted = $0 }

        XCTAssertTrue(filterBar.state.isUnconstrained)

        filterBar.toggleType(.code)
        XCTAssertEqual(lastEmitted?.selectedTypes, [.code])
        XCTAssertEqual(pillButtons[0].visualStyle, .inactive)
        XCTAssertEqual(pillButtons[1].visualStyle, .accentActive)

        filterBar.toggleType(.document)
        XCTAssertEqual(lastEmitted?.selectedTypes, [.code, .document])

        filterBar.selectAllTypes()
        XCTAssertEqual(lastEmitted?.selectedTypes, [])
        XCTAssertEqual(pillButtons[0].visualStyle, .neutralActive)

        // 选中全部 6 种具体类型时自动规约为无类型约束
        for type in FilterState.concreteFileTypes {
            filterBar.toggleType(type)
        }
        XCTAssertFalse(filterBar.state.hasTypeConstraint)

        filterBar.applySizePreset(minBytes: 10_000_000, maxBytes: nil)
        XCTAssertEqual(lastEmitted?.minSizeBytes, 10_000_000)
        XCTAssertNil(lastEmitted?.maxSizeBytes)
        XCTAssertEqual(sizeButton.visualStyle, .accentActive)
        XCTAssertTrue(FloatingInlineFilterBarView.sizeButtonTitle(for: filterBar.state).hasPrefix("> "))
        XCTAssertEqual(FloatingInlineFilterBarView.sizeButtonTitle(for: FilterState()), L("floatingBar.filter.size"))

        filterBar.clearAllFilters()
        XCTAssertTrue(filterBar.state.isUnconstrained)
        XCTAssertEqual(sizeButton.visualStyle, .inactive)

        // 横向玻璃空态视图：去除了圆形底衬，右侧强调按钮与双行提示清晰对齐
        let emptyView = FloatingGlassEmptyStateView(frame: NSRect(x: 0, y: 0, width: 580, height: 76))
        var actionFired = false
        emptyView.apply(.init(
            symbolName: "magnifyingglass",
            title: "没有找到匹配项",
            subtitle: "当前搜索条件没有匹配的文件，试试调整关键词或筛选条件。",
            actionTitle: "清除筛选条件",
            onAction: { actionFired = true }
        ))
        emptyView.layoutSubtreeIfNeeded()

        XCTAssertTrue(emptyView.iconView.superview === emptyView)
        XCTAssertEqual(emptyView.titleLabel.stringValue, "没有找到匹配项")
        XCTAssertFalse(emptyView.subtitleLabel.isHidden)
        XCTAssertFalse(emptyView.actionButton.isHidden)

        let titleFrame = emptyView.titleLabel.convert(emptyView.titleLabel.bounds, to: emptyView)
        let subtitleFrame = emptyView.subtitleLabel.convert(emptyView.subtitleLabel.bounds, to: emptyView)
        let actionFrame = emptyView.actionButton.convert(emptyView.actionButton.bounds, to: emptyView)
        XCTAssertGreaterThan(titleFrame.width, 60)
        XCTAssertGreaterThan(subtitleFrame.width, 100)
        XCTAssertLessThanOrEqual(subtitleFrame.maxX, actionFrame.minX - 12)
        XCTAssertEqual(actionFrame.maxX, emptyView.bounds.width - 20, accuracy: 1.5)

        emptyView.triggerActionForTesting()
        XCTAssertTrue(actionFired)

        emptyView.apply(.init(
            symbolName: "magnifyingglass",
            title: "没有找到匹配项",
            subtitle: "当前搜索条件没有匹配的文件，试试调整关键词或筛选条件。",
            actionTitle: nil,
            onAction: nil
        ))
        emptyView.layoutSubtreeIfNeeded()
        XCTAssertTrue(emptyView.actionButton.isHidden)
        let expandedSubtitleFrame = emptyView.subtitleLabel.convert(emptyView.subtitleLabel.bounds, to: emptyView)
        XCTAssertGreaterThan(expandedSubtitleFrame.maxX, actionFrame.minX)
    }

    func testFloatingBarLocalizationKeysParity() throws {
        let zhContent = try TestFixture.readRepoFile("Lokii/Lokii/Resources/zh-Hans.lproj/Localizable.strings")
        let enContent = try TestFixture.readRepoFile("Lokii/Lokii/Resources/en.lproj/Localizable.strings")

        let requiredKeys = [
            "menu.switchToFloatingBar",
            "menu.switchToStandardWindow",
            "window.switchToFloatingBar",
            "tray.switchToFloatingBar",
            "tray.switchToStandardWindow",
            "prefs.windowMode",
            "prefs.windowMode.standard",
            "prefs.windowMode.floatingBar",
            "prefs.windowMode.description.standard",
            "prefs.windowMode.description.floatingBar",
            "floatingBar.placeholder",
            "floatingBar.expandToStandard",
            "floatingBar.clear",
            "floatingBar.noMatches",
            "floatingBar.enableFuzzy",
            "floatingBar.clearFilters",
            "floatingBar.noMatches.filteredHint",
            "floatingBar.noMatches.fuzzyHint",
            "floatingBar.noMatches.genericHint",
            "floatingBar.filter.size",
            "floatingBar.filter.sizeAny",
            "floatingBar.filter.sizeOver1MB",
            "floatingBar.filter.sizeOver10MB",
            "floatingBar.filter.sizeOver100MB",
            "floatingBar.filter.sizeUnder1MB",
            "floatingBar.ctx.openInStandard",
            "search.ctx.open",
            "search.ctx.revealInFinder",
            "search.ctx.openInTerminal",
            "search.ctx.quickLook",
            "search.ctx.copyPath",
            "search.ctx.copyName",
            "search.empty.excluded.title",
            "search.empty.excluded.tag",
            "search.empty.manageRules",
            "search.error.syntax",
        ]

        for key in requiredKeys {
            XCTAssertTrue(zhContent.contains("\"\(key)\""), "zh-Hans 必须包含 \(key)")
            XCTAssertTrue(enContent.contains("\"\(key)\""), "en 必须包含 \(key)")
        }

        // 自动扫描悬浮条全部源码中使用的多语言 Key，防止再次漏配或拼写错误
        let auditedFiles = [
            "Lokii/Lokii/FloatingSearchWindowController.swift",
            "Lokii/Lokii/FloatingBarInlineViews.swift",
            "Lokii/Lokii/FloatingSearchResultCellView.swift",
            "Lokii/Lokii/FloatingSearchPanel.swift",
            "Lokii/Lokii/WindowPresentationMode.swift",
        ]
        let keyRegex = try NSRegularExpression(pattern: #"(?:L\(|localizedKey:\s*|titleKey:\s*)"([^"]+)""#)
        for file in auditedFiles {
            let source = try TestFixture.readRepoFile(file)
            let range = NSRange(source.startIndex..<source.endIndex, in: source)
            for match in keyRegex.matches(in: source, range: range) {
                if let keyRange = Range(match.range(at: 1), in: source) {
                    let key = String(source[keyRange])
                    XCTAssertTrue(zhContent.contains("\"\(key)\""), "\(file) 使用的 Key 缺失于 zh-Hans: \(key)")
                    XCTAssertTrue(enContent.contains("\"\(key)\""), "\(file) 使用的 Key 缺失于 en: \(key)")
                }
            }
        }

        XCTAssertTrue(zhContent.contains("没有找到匹配项"))
        XCTAssertTrue(zhContent.contains("当前搜索条件没有匹配的文件，试试调整关键词或筛选条件。"))
        XCTAssertTrue(zhContent.contains("清除筛选条件"))

        let controllerSource = try TestFixture.readRepoFile("Lokii/Lokii/FloatingSearchWindowController.swift")
        XCTAssertFalse(controllerSource.contains("\"search.ctx.reveal\""))
        XCTAssertFalse(controllerSource.contains("\"search.empty.excludedTitle\""))
        XCTAssertFalse(controllerSource.contains("FilterPopoverController"))
        XCTAssertTrue(controllerSource.contains("glassHostView.layer?.cornerCurve = .continuous"))
        XCTAssertTrue(controllerSource.contains("glassHostView.layer?.masksToBounds = true"))
        XCTAssertFalse(controllerSource.contains("\"line.3.horizontal.decrease\",\n                title: L(\"floatingBar.noMatches\")"))
    }
}
