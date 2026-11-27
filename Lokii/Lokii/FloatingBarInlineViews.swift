import AppKit

/// 悬浮搜索条自适应超细分隔线：以浅色/深色极低对比度细线区分输入区与下方内容区，避免厚重边框感
final class FloatingBarHairlineDividerView: NSView {
    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        wantsLayer = true
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsDisplay = true
    }

    override func draw(_ dirtyRect: NSRect) {
        let isDark = effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        let lineColor = isDark
            ? NSColor.white.withAlphaComponent(0.10)
            : NSColor.black.withAlphaComponent(0.06)
        lineColor.setFill()
        let lineHeight: CGFloat = 0.5
        let y = floor((bounds.height - lineHeight) * 0.5)
        NSRect(x: 0, y: max(0, y), width: bounds.width, height: lineHeight).fill()
    }
}

/// 悬浮搜索条专属全圆角玻璃胶囊按钮：承载内联筛选标签与横向空态快捷动作，随深浅外观与激活状态自适应渲染
final class FloatingGlassPillButton: NSButton {
    enum VisualStyle: Equatable {
        case inactive
        case neutralActive
        case accentActive
        case prominentAction
    }

    var visualStyle: VisualStyle = .inactive {
        didSet {
            if oldValue != visualStyle {
                invalidateIntrinsicContentSize()
                updateAppearance()
            }
        }
    }

    var trailingChevron: Bool = false {
        didSet {
            if oldValue != trailingChevron {
                updateChevronVisibility()
                invalidateIntrinsicContentSize()
            }
        }
    }

    private var isHovered: Bool = false {
        didSet {
            if oldValue != isHovered {
                needsDisplay = true
            }
        }
    }

    private var trackingAreaRef: NSTrackingArea?
    private let textLabel = NSTextField(labelWithString: "")
    private let chevronView = NSImageView()
    private let contentStack = NSStackView()

    override var title: String {
        didSet {
            textLabel.stringValue = title
            invalidateIntrinsicContentSize()
            updateAppearance()
        }
    }

    override var isHighlighted: Bool {
        didSet {
            if oldValue != isHighlighted {
                needsDisplay = true
            }
        }
    }

    override var intrinsicContentSize: NSSize {
        let labelSize = textLabel.intrinsicContentSize
        let hPad: CGFloat = (visualStyle == .prominentAction) ? 14 : 9
        let chevronExtra: CGFloat = trailingChevron ? 11 : 0
        let height: CGFloat = (visualStyle == .prominentAction) ? 28 : 25
        return NSSize(width: ceil(labelSize.width) + hPad * 2 + chevronExtra, height: height)
    }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        setupViews()
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        setupViews()
    }

    private func setupViews() {
        isBordered = false
        focusRingType = .none
        wantsLayer = true
        setContentHuggingPriority(.required, for: .horizontal)
        setContentCompressionResistancePriority(.required, for: .horizontal)

        textLabel.font = .systemFont(ofSize: 11.5, weight: .medium)
        textLabel.alignment = .center
        textLabel.lineBreakMode = .byTruncatingTail
        textLabel.isEditable = false
        textLabel.isBezeled = false
        textLabel.drawsBackground = false

        let chevronConfig = NSImage.SymbolConfiguration(pointSize: 9, weight: .semibold)
        chevronView.image = NSImage(systemSymbolName: "chevron.down", accessibilityDescription: nil)?
            .withSymbolConfiguration(chevronConfig)
        chevronView.imageScaling = .scaleNone
        chevronView.isHidden = true

        contentStack.translatesAutoresizingMaskIntoConstraints = false
        contentStack.orientation = .horizontal
        contentStack.alignment = .centerY
        contentStack.spacing = 4
        contentStack.detachesHiddenViews = true
        contentStack.addArrangedSubview(textLabel)
        contentStack.addArrangedSubview(chevronView)
        addSubview(contentStack)

        NSLayoutConstraint.activate([
            contentStack.centerXAnchor.constraint(equalTo: centerXAnchor),
            contentStack.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])

        updateAppearance()
    }

    override func hitTest(_ point: NSPoint) -> NSView? {
        bounds.contains(convert(point, from: superview)) ? self : nil
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        updateAppearance()
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let existing = trackingAreaRef {
            removeTrackingArea(existing)
        }
        let area = NSTrackingArea(
            rect: bounds,
            options: [.mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect],
            owner: self,
            userInfo: nil
        )
        addTrackingArea(area)
        trackingAreaRef = area
    }

    override func resetCursorRects() {
        super.resetCursorRects()
        addCursorRect(bounds, cursor: .pointingHand)
    }

    override func mouseEntered(with event: NSEvent) {
        super.mouseEntered(with: event)
        isHovered = true
    }

    override func mouseExited(with event: NSEvent) {
        super.mouseExited(with: event)
        isHovered = false
    }

    private func updateChevronVisibility() {
        chevronView.isHidden = !trailingChevron
    }

    private func updateAppearance() {
        let fg: NSColor
        let weight: NSFont.Weight
        switch visualStyle {
        case .inactive:
            fg = NSColor.labelColor.withAlphaComponent(0.78)
            weight = .medium
        case .neutralActive:
            fg = .labelColor
            weight = .semibold
        case .accentActive:
            fg = .white
            weight = .semibold
        case .prominentAction:
            fg = .controlAccentColor
            weight = .semibold
        }
        let fontSize: CGFloat = (visualStyle == .prominentAction) ? 12 : 11.5
        textLabel.font = .systemFont(ofSize: fontSize, weight: weight)
        textLabel.textColor = fg
        chevronView.contentTintColor = fg
        needsDisplay = true
    }

    override func draw(_ dirtyRect: NSRect) {
        let pillRect = bounds.insetBy(dx: 0.5, dy: 0.5)
        guard pillRect.width > 0, pillRect.height > 0 else { return }
        let radius = pillRect.height / 2.0
        let path = NSBezierPath(roundedRect: pillRect, xRadius: radius, yRadius: radius)
        let isDark = effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua

        switch visualStyle {
        case .accentActive:
            let fill = isHighlighted
                ? (NSColor.controlAccentColor.blended(withFraction: 0.14, of: .black) ?? .controlAccentColor)
                : NSColor.controlAccentColor
            fill.setFill()
            path.fill()
        case .neutralActive:
            let fillColor: NSColor
            let borderColor: NSColor
            if isDark {
                let alpha: CGFloat = isHighlighted ? 0.25 : (isHovered ? 0.22 : 0.18)
                fillColor = NSColor.white.withAlphaComponent(alpha)
                borderColor = NSColor.white.withAlphaComponent(0.20)
            } else {
                let alpha: CGFloat = isHighlighted ? 0.15 : (isHovered ? 0.12 : 0.09)
                fillColor = NSColor.black.withAlphaComponent(alpha)
                borderColor = NSColor.black.withAlphaComponent(0.10)
            }
            fillColor.setFill()
            path.fill()
            borderColor.setStroke()
            path.lineWidth = 0.75
            path.stroke()
        case .prominentAction:
            let fillAlpha: CGFloat = isHighlighted ? 0.24 : (isHovered ? 0.19 : 0.13)
            NSColor.controlAccentColor.withAlphaComponent(fillAlpha).setFill()
            path.fill()
            NSColor.controlAccentColor.withAlphaComponent(isHovered ? 0.42 : 0.28).setStroke()
            path.lineWidth = 0.75
            path.stroke()
        case .inactive:
            let fillColor: NSColor
            let borderColor: NSColor
            if isDark {
                let alpha: CGFloat = isHighlighted ? 0.16 : (isHovered ? 0.12 : 0.07)
                fillColor = NSColor.white.withAlphaComponent(alpha)
                borderColor = NSColor.white.withAlphaComponent(isHovered ? 0.18 : 0.10)
            } else {
                let alpha: CGFloat = isHighlighted ? 0.105 : (isHovered ? 0.075 : 0.038)
                fillColor = NSColor.black.withAlphaComponent(alpha)
                borderColor = NSColor.black.withAlphaComponent(isHovered ? 0.11 : 0.055)
            }
            fillColor.setFill()
            path.fill()
            borderColor.setStroke()
            path.lineWidth = 0.75
            path.stroke()
        }
    }
}

/// 悬浮搜索条内嵌玻璃筛选条：左侧单排展示文件类型胶囊（支持多选），右侧提供文件体积快捷下拉菜单
final class FloatingInlineFilterBarView: NSView {
    struct SizePreset {
        let titleKey: String
        let minBytes: UInt64?
        let maxBytes: UInt64?
    }

    static let sizePresets: [SizePreset] = [
        SizePreset(titleKey: "floatingBar.filter.sizeOver1MB", minBytes: 1_000_000, maxBytes: nil),
        SizePreset(titleKey: "floatingBar.filter.sizeOver10MB", minBytes: 10_000_000, maxBytes: nil),
        SizePreset(titleKey: "floatingBar.filter.sizeOver100MB", minBytes: 100_000_000, maxBytes: nil),
        SizePreset(titleKey: "floatingBar.filter.sizeUnder1MB", minBytes: nil, maxBytes: 1_000_000),
    ]

    private static let typeTitleKeys: [(type: FfiFileType, titleKey: String)] = [
        (.code, "filter.type.code"),
        (.document, "filter.type.document"),
        (.image, "filter.type.image"),
        (.media, "filter.type.media"),
        (.archive, "filter.type.archive"),
        (.folder, "filter.type.folder"),
    ]

    private(set) var state = FilterState()
    var onStateChanged: ((FilterState) -> Void)?

    private let chipsStack = NSStackView()
    private let allButton = FloatingGlassPillButton()
    private var typeButtons: [(type: FfiFileType, button: FloatingGlassPillButton)] = []
    private let sizeMenuButton = FloatingGlassPillButton()

    var allFilterPillButtonsForTesting: [FloatingGlassPillButton] {
        [allButton] + typeButtons.map(\.button)
    }

    var sizeFilterButtonForTesting: FloatingGlassPillButton {
        sizeMenuButton
    }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        setupViews()
        refreshLocalization()
        syncVisualState()
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        setupViews()
        refreshLocalization()
        syncVisualState()
    }

    static func sizeButtonTitle(for state: FilterState) -> String {
        switch (state.minSizeBytes, state.maxSizeBytes) {
        case (nil, nil):
            return L("floatingBar.filter.size")
        case (let min?, nil):
            return "> \(SearchResultsViewController.formatCompactFileSize(min))"
        case (nil, let max?):
            return "< \(SearchResultsViewController.formatCompactFileSize(max))"
        case (let min?, let max?):
            return "\(SearchResultsViewController.formatCompactFileSize(min))–\(SearchResultsViewController.formatCompactFileSize(max))"
        }
    }

    func update(state newState: FilterState) {
        guard state != newState else { return }
        state = newState
        syncVisualState()
    }

    func selectAllTypes() {
        guard !state.selectedTypes.isEmpty else { return }
        state.selectedTypes.removeAll()
        syncVisualState()
        onStateChanged?(state)
    }

    func toggleType(_ type: FfiFileType) {
        if state.selectedTypes.contains(type) {
            state.selectedTypes.remove(type)
        } else {
            state.selectedTypes.insert(type)
        }
        if FilterState.concreteFileTypeSet.isSubset(of: state.selectedTypes) {
            state.selectedTypes.removeAll()
        }
        syncVisualState()
        onStateChanged?(state)
    }

    func applySizePreset(minBytes: UInt64?, maxBytes: UInt64?) {
        state.minSizeBytes = minBytes
        state.maxSizeBytes = maxBytes
        syncVisualState()
        onStateChanged?(state)
    }

    func clearAllFilters() {
        guard !state.isUnconstrained else { return }
        state.clearAll()
        syncVisualState()
        onStateChanged?(state)
    }

    func refreshLocalization() {
        allButton.title = L("filter.all")
        for (index, item) in Self.typeTitleKeys.enumerated() where index < typeButtons.count {
            typeButtons[index].button.title = L(item.titleKey)
        }
        sizeMenuButton.title = Self.sizeButtonTitle(for: state)
    }

    private func setupViews() {
        // 直接把水平栈放入筛选条，避免外层无固有宽度的 scrollView 在 Auto Layout 下折叠为 0pt
        chipsStack.translatesAutoresizingMaskIntoConstraints = false
        chipsStack.wantsLayer = true
        chipsStack.layer?.masksToBounds = true
        chipsStack.orientation = .horizontal
        chipsStack.alignment = .centerY
        chipsStack.spacing = 5
        chipsStack.setClippingResistancePriority(.defaultLow, for: .horizontal)
        chipsStack.setHuggingPriority(.required, for: .horizontal)

        allButton.setContentCompressionResistancePriority(.defaultHigh, for: .horizontal)
        allButton.target = self
        allButton.action = #selector(allButtonClicked)
        chipsStack.addArrangedSubview(allButton)

        for (index, item) in Self.typeTitleKeys.enumerated() {
            let button = FloatingGlassPillButton()
            button.tag = index
            button.setContentCompressionResistancePriority(.defaultHigh, for: .horizontal)
            button.target = self
            button.action = #selector(typeButtonClicked(_:))
            chipsStack.addArrangedSubview(button)
            typeButtons.append((item.type, button))
        }

        sizeMenuButton.translatesAutoresizingMaskIntoConstraints = false
        sizeMenuButton.trailingChevron = true
        sizeMenuButton.target = self
        sizeMenuButton.action = #selector(sizeMenuButtonClicked(_:))

        addSubview(chipsStack)
        addSubview(sizeMenuButton)

        NSLayoutConstraint.activate([
            chipsStack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 16),
            chipsStack.centerYAnchor.constraint(equalTo: centerYAnchor),
            chipsStack.trailingAnchor.constraint(lessThanOrEqualTo: sizeMenuButton.leadingAnchor, constant: -8),

            sizeMenuButton.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -16),
            sizeMenuButton.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    private func syncVisualState() {
        let hasTypeConstraint = state.hasTypeConstraint
        allButton.visualStyle = hasTypeConstraint ? .inactive : .neutralActive
        for entry in typeButtons {
            entry.button.visualStyle = state.selectedTypes.contains(entry.type) ? .accentActive : .inactive
        }
        let hasSizeConstraint = state.minSizeBytes != nil || state.maxSizeBytes != nil
        sizeMenuButton.visualStyle = hasSizeConstraint ? .accentActive : .inactive
        sizeMenuButton.title = Self.sizeButtonTitle(for: state)
    }

    @objc private func allButtonClicked() {
        selectAllTypes()
    }

    @objc private func typeButtonClicked(_ sender: NSButton) {
        let idx = sender.tag
        guard idx >= 0, idx < typeButtons.count else { return }
        toggleType(typeButtons[idx].type)
    }

    @objc private func sizeMenuButtonClicked(_ sender: NSButton) {
        let menu = NSMenu()
        menu.autoenablesItems = false

        let anyItem = NSMenuItem(
            title: L("floatingBar.filter.sizeAny"),
            action: #selector(sizeAnySelected),
            keyEquivalent: ""
        )
        anyItem.target = self
        anyItem.state = (state.minSizeBytes == nil && state.maxSizeBytes == nil) ? .on : .off
        menu.addItem(anyItem)
        menu.addItem(NSMenuItem.separator())

        for (index, preset) in Self.sizePresets.enumerated() {
            let item = NSMenuItem(
                title: L(preset.titleKey),
                action: #selector(sizePresetSelected(_:)),
                keyEquivalent: ""
            )
            item.tag = index
            item.target = self
            item.state = (state.minSizeBytes == preset.minBytes && state.maxSizeBytes == preset.maxBytes) ? .on : .off
            menu.addItem(item)
        }

        menu.addItem(NSMenuItem.separator())
        let clearItem = NSMenuItem(
            title: L("filter.clearAll"),
            action: #selector(clearAllMenuSelected),
            keyEquivalent: ""
        )
        clearItem.target = self
        clearItem.isEnabled = !state.isUnconstrained
        menu.addItem(clearItem)

        let location = NSPoint(x: 0, y: sender.bounds.height + 6)
        menu.popUp(positioning: nil, at: location, in: sender)
    }

    @objc private func sizeAnySelected() {
        applySizePreset(minBytes: nil, maxBytes: nil)
    }

    @objc private func sizePresetSelected(_ sender: NSMenuItem) {
        let idx = sender.tag
        guard idx >= 0, idx < Self.sizePresets.count else { return }
        let preset = Self.sizePresets[idx]
        applySizePreset(minBytes: preset.minBytes, maxBytes: preset.maxBytes)
    }

    @objc private func clearAllMenuSelected() {
        clearAllFilters()
    }
}

/// 悬浮搜索条横向玻璃空态视图：精简搜索符号 + 双行语义说明 + 右对齐强调动作胶囊
final class FloatingGlassEmptyStateView: NSView {
    struct Model {
        let symbolName: String
        let title: String
        let subtitle: String
        let actionTitle: String?
        let onAction: (() -> Void)?
    }

    let iconView = NSImageView()
    let titleLabel = NSTextField(labelWithString: "")
    let subtitleLabel = NSTextField(labelWithString: "")
    let actionButton = FloatingGlassPillButton()
    private let textStack = NSStackView()
    private var textTrailingToActionConstraint: NSLayoutConstraint!
    private var textTrailingToEdgeConstraint: NSLayoutConstraint!
    private var actionHandler: (() -> Void)?

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        setupViews()
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        setupViews()
    }

    func apply(_ model: Model) {
        let iconConfig = NSImage.SymbolConfiguration(pointSize: 19, weight: .regular)
        iconView.image = NSImage(systemSymbolName: model.symbolName, accessibilityDescription: nil)?
            .withSymbolConfiguration(iconConfig)
        titleLabel.stringValue = model.title
        subtitleLabel.stringValue = model.subtitle
        subtitleLabel.isHidden = model.subtitle.isEmpty

        actionHandler = model.onAction
        let hasAction = (model.actionTitle?.isEmpty == false)
        if let actionTitle = model.actionTitle, hasAction {
            actionButton.title = actionTitle
            actionButton.isHidden = false
            textTrailingToEdgeConstraint.isActive = false
            textTrailingToActionConstraint.isActive = true
        } else {
            actionButton.title = ""
            actionButton.isHidden = true
            textTrailingToActionConstraint.isActive = false
            textTrailingToEdgeConstraint.isActive = true
        }
    }

    func triggerActionForTesting() {
        actionHandler?()
    }

    private func setupViews() {
        wantsLayer = true

        iconView.translatesAutoresizingMaskIntoConstraints = false
        iconView.contentTintColor = NSColor.secondaryLabelColor.withAlphaComponent(0.75)
        iconView.imageScaling = .scaleProportionallyDown
        iconView.setAccessibilityElement(false)
        addSubview(iconView)

        titleLabel.translatesAutoresizingMaskIntoConstraints = false
        titleLabel.font = .systemFont(ofSize: 13.5, weight: .semibold)
        titleLabel.textColor = .labelColor
        titleLabel.lineBreakMode = .byTruncatingTail
        titleLabel.maximumNumberOfLines = 1
        titleLabel.setContentHuggingPriority(.defaultLow, for: .horizontal)

        subtitleLabel.translatesAutoresizingMaskIntoConstraints = false
        subtitleLabel.font = .systemFont(ofSize: 12, weight: .regular)
        subtitleLabel.textColor = .secondaryLabelColor
        subtitleLabel.lineBreakMode = .byTruncatingTail
        subtitleLabel.maximumNumberOfLines = 2
        subtitleLabel.setContentHuggingPriority(.defaultLow, for: .horizontal)

        textStack.translatesAutoresizingMaskIntoConstraints = false
        textStack.orientation = .vertical
        textStack.alignment = .leading
        textStack.spacing = 3
        textStack.detachesHiddenViews = true
        textStack.setHuggingPriority(.defaultLow, for: .horizontal)
        textStack.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        textStack.addArrangedSubview(titleLabel)
        textStack.addArrangedSubview(subtitleLabel)
        addSubview(textStack)

        actionButton.translatesAutoresizingMaskIntoConstraints = false
        actionButton.visualStyle = .prominentAction
        actionButton.setContentHuggingPriority(.required, for: .horizontal)
        actionButton.setContentCompressionResistancePriority(.required, for: .horizontal)
        actionButton.target = self
        actionButton.action = #selector(actionButtonClicked)
        actionButton.isHidden = true
        addSubview(actionButton)

        textTrailingToActionConstraint = textStack.trailingAnchor.constraint(equalTo: actionButton.leadingAnchor, constant: -16)
        textTrailingToEdgeConstraint = textStack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -20)

        NSLayoutConstraint.activate([
            iconView.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 20),
            iconView.centerYAnchor.constraint(equalTo: centerYAnchor),
            iconView.widthAnchor.constraint(equalToConstant: 24),
            iconView.heightAnchor.constraint(equalToConstant: 24),

            textStack.leadingAnchor.constraint(equalTo: iconView.trailingAnchor, constant: 12),
            textStack.centerYAnchor.constraint(equalTo: centerYAnchor),
            textTrailingToEdgeConstraint,

            actionButton.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -20),
            actionButton.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @objc private func actionButtonClicked() {
        actionHandler?()
    }
}
