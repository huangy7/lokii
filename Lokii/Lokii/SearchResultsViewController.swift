import Cocoa
import UniformTypeIdentifiers
import Quartz

/// 承载旋转微动效的居中稳定容器，锁定中心锚点规避 AppKit 图层坐标漂移
final class SearchResultsCenterSpinContainerView: NSView {
    override func layout() {
        super.layout()
        guard let subview = subviews.first else { return }
        subview.frame = bounds
        if let layer = subview.layer {
            layer.anchorPoint = CGPoint(x: 0.5, y: 0.5)
            layer.position = CGPoint(x: bounds.midX, y: bounds.midY)
        }
    }
}

/// 底栏克制权限提示按钮：提供原生手型指针交互响应
private final class InteractiveHintButton: NSButton {
    override func resetCursorRects() {
        super.resetCursorRects()
        addCursorRect(bounds, cursor: .pointingHand)
    }
}

/// 空态顶部柔和圆形图标徽章：在深浅色下维持轻盈层级，不抢占标题视觉权重
final class EmptyIconBadgeView: NSView {
    let imageView = NSImageView()

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        setupView()
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    override var wantsUpdateLayer: Bool { true }

    override func updateLayer() {
        super.updateLayer()
        guard let layer else { return }
        let isDark = effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        layer.cornerRadius = 24
        layer.borderWidth = 1.0
        layer.backgroundColor = NSColor.labelColor.withAlphaComponent(isDark ? 0.065 : 0.042).cgColor
        layer.borderColor = NSColor.labelColor.withAlphaComponent(isDark ? 0.11 : 0.065).cgColor
    }

    private func setupView() {
        wantsLayer = true
        translatesAutoresizingMaskIntoConstraints = false
        setAccessibilityElement(false)

        imageView.translatesAutoresizingMaskIntoConstraints = false
        imageView.contentTintColor = .secondaryLabelColor
        imageView.imageScaling = .scaleProportionallyDown
        imageView.setAccessibilityElement(false)
        addSubview(imageView)

        NSLayoutConstraint.activate([
            widthAnchor.constraint(equalToConstant: 48),
            heightAnchor.constraint(equalToConstant: 48),
            imageView.centerXAnchor.constraint(equalTo: centerXAnchor),
            imageView.centerYAnchor.constraint(equalTo: centerYAnchor),
            imageView.widthAnchor.constraint(equalToConstant: 24),
            imageView.heightAnchor.constraint(equalToConstant: 24),
        ])
    }
}

/// 空搜索状态下的常用检索语法速查卡片：支持深浅色自适应高对比度、无位移悬停反馈与一键直填试搜
final class SyntaxTipCardView: NSView {
    let syntaxExample: String
    let descriptionKey: String
    var onSelect: ((String) -> Void)?

    private let syntaxLabel = NSTextField(labelWithString: "")
    private let descriptionLabel = NSTextField(labelWithString: "")
    private var isHovered = false {
        didSet { needsDisplay = true }
    }
    private var isPressed = false {
        didSet { needsDisplay = true }
    }

    init(syntaxExample: String, descriptionKey: String) {
        self.syntaxExample = syntaxExample
        self.descriptionKey = descriptionKey
        super.init(frame: .zero)
        setupView()
        refreshLocalization()
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    override var wantsUpdateLayer: Bool { true }

    override func updateLayer() {
        super.updateLayer()
        guard let layer else { return }
        let isDark = effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        layer.cornerRadius = 9
        layer.borderWidth = 1.0
        if isDark {
            if isPressed {
                layer.backgroundColor = NSColor.controlAccentColor.withAlphaComponent(0.14).cgColor
                layer.borderColor = NSColor.controlAccentColor.withAlphaComponent(0.50).cgColor
            } else if isHovered {
                layer.backgroundColor = NSColor.white.withAlphaComponent(0.085).cgColor
                layer.borderColor = NSColor.controlAccentColor.withAlphaComponent(0.45).cgColor
            } else {
                layer.backgroundColor = NSColor.white.withAlphaComponent(0.048).cgColor
                layer.borderColor = NSColor.white.withAlphaComponent(0.10).cgColor
            }
        } else {
            if isPressed {
                layer.backgroundColor = NSColor.controlAccentColor.withAlphaComponent(0.09).cgColor
                layer.borderColor = NSColor.controlAccentColor.withAlphaComponent(0.48).cgColor
            } else if isHovered {
                layer.backgroundColor = NSColor.controlAccentColor.withAlphaComponent(0.04).cgColor
                layer.borderColor = NSColor.controlAccentColor.withAlphaComponent(0.40).cgColor
            } else {
                layer.backgroundColor = NSColor.controlBackgroundColor.cgColor
                layer.borderColor = NSColor.separatorColor.withAlphaComponent(0.55).cgColor
            }
        }
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        for area in trackingAreas {
            removeTrackingArea(area)
        }
        addTrackingArea(NSTrackingArea(
            rect: bounds,
            options: [.mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect],
            owner: self,
            userInfo: nil
        ))
    }

    override func resetCursorRects() {
        super.resetCursorRects()
        addCursorRect(bounds, cursor: .pointingHand)
    }

    override func mouseEntered(with event: NSEvent) {
        isHovered = true
    }

    override func mouseExited(with event: NSEvent) {
        isHovered = false
        isPressed = false
    }

    override func mouseDown(with event: NSEvent) {
        isPressed = true
    }

    override func mouseUp(with event: NSEvent) {
        let wasPressed = isPressed
        isPressed = false
        guard wasPressed else { return }
        let location = convert(event.locationInWindow, from: nil)
        if bounds.contains(location) {
            onSelect?(syntaxExample)
        }
    }

    /// 供自动化测试触发与鼠标点击完全一致的回调路径
    func simulateClick() {
        onSelect?(syntaxExample)
    }

    override func accessibilityPerformPress() -> Bool {
        onSelect?(syntaxExample)
        return true
    }

    func refreshLocalization() {
        let desc = L(descriptionKey)
        descriptionLabel.stringValue = desc
        setAccessibilityLabel("\(syntaxExample), \(desc)")
    }

    private func setupView() {
        wantsLayer = true
        translatesAutoresizingMaskIntoConstraints = false
        setAccessibilityElement(true)
        setAccessibilityRole(.button)

        syntaxLabel.translatesAutoresizingMaskIntoConstraints = false
        syntaxLabel.stringValue = syntaxExample
        syntaxLabel.font = .monospacedSystemFont(ofSize: 12, weight: .medium)
        syntaxLabel.textColor = .labelColor
        syntaxLabel.lineBreakMode = .byTruncatingTail
        syntaxLabel.setContentCompressionResistancePriority(.required, for: .horizontal)
        syntaxLabel.setContentHuggingPriority(.required, for: .horizontal)

        descriptionLabel.translatesAutoresizingMaskIntoConstraints = false
        descriptionLabel.font = .systemFont(ofSize: 11.5, weight: .regular)
        descriptionLabel.textColor = .secondaryLabelColor
        descriptionLabel.alignment = .right
        descriptionLabel.lineBreakMode = .byTruncatingTail
        descriptionLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        addSubview(syntaxLabel)
        addSubview(descriptionLabel)

        NSLayoutConstraint.activate([
            heightAnchor.constraint(equalToConstant: 38),
            syntaxLabel.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 14),
            syntaxLabel.centerYAnchor.constraint(equalTo: centerYAnchor),
            descriptionLabel.leadingAnchor.constraint(greaterThanOrEqualTo: syntaxLabel.trailingAnchor, constant: 10),
            descriptionLabel.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -14),
            descriptionLabel.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }
}

/// 最近搜索历史单行胶囊：防止超长查询词换行撑破布局，悬停提示完整内容并支持一键回填检索
final class RecentSearchChipView: NSView {
    let query: String
    var onSelect: ((String) -> Void)?

    private let iconView = NSImageView()
    private let titleLabel = NSTextField(labelWithString: "")
    private var isHovered = false {
        didSet { needsDisplay = true }
    }
    private var isPressed = false {
        didSet { needsDisplay = true }
    }

    init(query: String) {
        self.query = query
        super.init(frame: .zero)
        setupView()
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    override var wantsUpdateLayer: Bool { true }

    override func updateLayer() {
        super.updateLayer()
        guard let layer else { return }
        let isDark = effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        layer.cornerRadius = 8
        layer.borderWidth = 1.0
        if isDark {
            layer.backgroundColor = NSColor.white.withAlphaComponent(isPressed ? 0.12 : (isHovered ? 0.085 : 0.048)).cgColor
            layer.borderColor = (isHovered
                ? NSColor.controlAccentColor.withAlphaComponent(0.45)
                : NSColor.white.withAlphaComponent(0.10)).cgColor
        } else {
            layer.backgroundColor = (isPressed
                ? NSColor.controlAccentColor.withAlphaComponent(0.08)
                : (isHovered ? NSColor.controlAccentColor.withAlphaComponent(0.04) : NSColor.controlBackgroundColor)).cgColor
            layer.borderColor = (isHovered
                ? NSColor.controlAccentColor.withAlphaComponent(0.40)
                : NSColor.separatorColor.withAlphaComponent(0.55)).cgColor
        }
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        for area in trackingAreas {
            removeTrackingArea(area)
        }
        addTrackingArea(NSTrackingArea(
            rect: bounds,
            options: [.mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect],
            owner: self,
            userInfo: nil
        ))
    }

    override func resetCursorRects() {
        super.resetCursorRects()
        addCursorRect(bounds, cursor: .pointingHand)
    }

    override func mouseEntered(with event: NSEvent) {
        isHovered = true
    }

    override func mouseExited(with event: NSEvent) {
        isHovered = false
        isPressed = false
    }

    override func mouseDown(with event: NSEvent) {
        isPressed = true
    }

    override func mouseUp(with event: NSEvent) {
        let wasPressed = isPressed
        isPressed = false
        guard wasPressed else { return }
        let location = convert(event.locationInWindow, from: nil)
        if bounds.contains(location) {
            onSelect?(query)
        }
    }

    func simulateClick() {
        onSelect?(query)
    }

    override func accessibilityPerformPress() -> Bool {
        onSelect?(query)
        return true
    }

    private func setupView() {
        wantsLayer = true
        // 由父容器 FlowWrapView 直接基于 frame 执行自动换行排布，保留 autoresizing 转换避免约束冲突
        translatesAutoresizingMaskIntoConstraints = true
        toolTip = query
        setAccessibilityElement(true)
        setAccessibilityRole(.button)
        setAccessibilityLabel(query)

        let iconConfig = NSImage.SymbolConfiguration(pointSize: 11, weight: .regular)
        iconView.translatesAutoresizingMaskIntoConstraints = false
        iconView.image = NSImage(systemSymbolName: "clock", accessibilityDescription: nil)?
            .withSymbolConfiguration(iconConfig)
        iconView.contentTintColor = .tertiaryLabelColor
        iconView.setAccessibilityElement(false)

        titleLabel.translatesAutoresizingMaskIntoConstraints = false
        titleLabel.stringValue = query
        titleLabel.font = .systemFont(ofSize: 12, weight: .regular)
        titleLabel.textColor = .labelColor
        titleLabel.lineBreakMode = .byTruncatingMiddle
        titleLabel.maximumNumberOfLines = 1

        addSubview(iconView)
        addSubview(titleLabel)

        NSLayoutConstraint.activate([
            iconView.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 10),
            iconView.centerYAnchor.constraint(equalTo: centerYAnchor),
            iconView.widthAnchor.constraint(equalToConstant: 12),
            iconView.heightAnchor.constraint(equalToConstant: 12),
            titleLabel.leadingAnchor.constraint(equalTo: iconView.trailingAnchor, constant: 6),
            titleLabel.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            titleLabel.centerYAnchor.constraint(equalTo: centerYAnchor),
            titleLabel.widthAnchor.constraint(lessThanOrEqualToConstant: 170),
        ])
    }

    override var fittingSize: NSSize {
        let labelWidth = min(ceil(titleLabel.intrinsicContentSize.width), 170)
        return NSSize(width: 10 + 12 + 6 + labelWidth + 10, height: 28)
    }
}

/// 纯 AppKit 自动换行流式布局容器，承载最近搜索胶囊在不同窗口宽度下的自然折行
final class FlowWrapView: NSView {
    var horizontalSpacing: CGFloat = 8
    var verticalSpacing: CGFloat = 8
    private var cachedHeight: CGFloat = 0

    override var isFlipped: Bool { true }

    override var intrinsicContentSize: NSSize {
        NSSize(width: NSView.noIntrinsicMetric, height: cachedHeight)
    }

    override func layout() {
        super.layout()
        recalculateLayout()
    }

    func recalculateLayout() {
        let availableWidth = bounds.width > 0 ? bounds.width : 560
        var cursorX: CGFloat = 0
        var cursorY: CGFloat = 0
        var rowHeight: CGFloat = 0

        for subview in subviews where !subview.isHidden {
            let size = subview.fittingSize
            let clampedWidth = min(size.width, availableWidth)
            if cursorX > 0 && cursorX + clampedWidth > availableWidth {
                cursorX = 0
                cursorY += rowHeight + verticalSpacing
                rowHeight = 0
            }
            subview.frame = NSRect(x: cursorX, y: cursorY, width: clampedWidth, height: size.height)
            cursorX += clampedWidth + horizontalSpacing
            rowHeight = max(rowHeight, size.height)
        }

        let totalHeight = subviews.contains(where: { !$0.isHidden }) ? (cursorY + rowHeight) : 0
        if abs(totalHeight - cachedHeight) > 0.5 {
            cachedHeight = totalHeight
            invalidateIntrinsicContentSize()
        }
    }
}

/// 无结果页的可点击行动建议卡片：图标底托 + 标题 + 说明，支持禁用态（例如模糊匹配已开启）
final class EmptyActionCardView: NSView {
    let normalSymbol: String
    let disabledSymbol: String?
    let titleKey: String
    let subtitleKey: String
    let disabledSubtitleKey: String?
    var onAction: (() -> Void)?

    private let iconWell = NSView()
    private let iconView = NSImageView()
    private let titleLabel = NSTextField(labelWithString: "")
    private let subtitleLabel = NSTextField(labelWithString: "")

    private var isHovered = false {
        didSet { needsDisplay = true }
    }
    private var isPressed = false {
        didSet { needsDisplay = true }
    }

    var isCardEnabled: Bool = true {
        didSet {
            if !isCardEnabled {
                isHovered = false
                isPressed = false
            }
            refreshContent()
            window?.invalidateCursorRects(for: self)
        }
    }

    init(
        symbol: String,
        disabledSymbol: String? = nil,
        titleKey: String,
        subtitleKey: String,
        disabledSubtitleKey: String? = nil
    ) {
        self.normalSymbol = symbol
        self.disabledSymbol = disabledSymbol
        self.titleKey = titleKey
        self.subtitleKey = subtitleKey
        self.disabledSubtitleKey = disabledSubtitleKey
        super.init(frame: .zero)
        setupView()
        refreshContent()
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    override var wantsUpdateLayer: Bool { true }

    override func updateLayer() {
        super.updateLayer()
        updateAppearance()
    }

    private func updateAppearance() {
        guard let layer else { return }
        let isDark = effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        layer.cornerRadius = 10
        layer.borderWidth = 1.0
        alphaValue = isCardEnabled ? 1.0 : 0.58

        if !isCardEnabled {
            layer.backgroundColor = (isDark
                ? NSColor.white.withAlphaComponent(0.03)
                : NSColor.labelColor.withAlphaComponent(0.025)).cgColor
            layer.borderColor = (isDark
                ? NSColor.white.withAlphaComponent(0.07)
                : NSColor.separatorColor.withAlphaComponent(0.35)).cgColor
            iconView.contentTintColor = .tertiaryLabelColor
        } else if isDark {
            if isPressed {
                layer.backgroundColor = NSColor.controlAccentColor.withAlphaComponent(0.14).cgColor
                layer.borderColor = NSColor.controlAccentColor.withAlphaComponent(0.50).cgColor
            } else if isHovered {
                layer.backgroundColor = NSColor.white.withAlphaComponent(0.085).cgColor
                layer.borderColor = NSColor.controlAccentColor.withAlphaComponent(0.45).cgColor
            } else {
                layer.backgroundColor = NSColor.white.withAlphaComponent(0.048).cgColor
                layer.borderColor = NSColor.white.withAlphaComponent(0.10).cgColor
            }
            iconView.contentTintColor = isHovered ? .controlAccentColor : .secondaryLabelColor
        } else {
            if isPressed {
                layer.backgroundColor = NSColor.controlAccentColor.withAlphaComponent(0.09).cgColor
                layer.borderColor = NSColor.controlAccentColor.withAlphaComponent(0.48).cgColor
            } else if isHovered {
                layer.backgroundColor = NSColor.controlAccentColor.withAlphaComponent(0.04).cgColor
                layer.borderColor = NSColor.controlAccentColor.withAlphaComponent(0.40).cgColor
            } else {
                layer.backgroundColor = NSColor.controlBackgroundColor.cgColor
                layer.borderColor = NSColor.separatorColor.withAlphaComponent(0.55).cgColor
            }
            iconView.contentTintColor = isHovered ? .controlAccentColor : .secondaryLabelColor
        }

        iconWell.layer?.cornerRadius = 7
        iconWell.layer?.backgroundColor = NSColor.labelColor.withAlphaComponent(
            isDark ? (isHovered && isCardEnabled ? 0.09 : 0.06) : (isHovered && isCardEnabled ? 0.06 : 0.042)
        ).cgColor
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        for area in trackingAreas {
            removeTrackingArea(area)
        }
        addTrackingArea(NSTrackingArea(
            rect: bounds,
            options: [.mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect],
            owner: self,
            userInfo: nil
        ))
    }

    override func resetCursorRects() {
        super.resetCursorRects()
        if isCardEnabled {
            addCursorRect(bounds, cursor: .pointingHand)
        }
    }

    override func mouseEntered(with event: NSEvent) {
        guard isCardEnabled else { return }
        isHovered = true
    }

    override func mouseExited(with event: NSEvent) {
        isHovered = false
        isPressed = false
    }

    override func mouseDown(with event: NSEvent) {
        guard isCardEnabled else { return }
        isPressed = true
    }

    override func mouseUp(with event: NSEvent) {
        let wasPressed = isPressed
        isPressed = false
        guard isCardEnabled, wasPressed else { return }
        let location = convert(event.locationInWindow, from: nil)
        if bounds.contains(location) {
            onAction?()
        }
    }

    func simulateClick() {
        guard isCardEnabled else { return }
        onAction?()
    }

    override func accessibilityPerformPress() -> Bool {
        guard isCardEnabled else { return false }
        onAction?()
        return true
    }

    func refreshContent() {
        let title = L(titleKey)
        let activeSubtitleKey = (!isCardEnabled && disabledSubtitleKey != nil) ? disabledSubtitleKey! : subtitleKey
        let subtitle = L(activeSubtitleKey)
        titleLabel.stringValue = title
        subtitleLabel.stringValue = subtitle

        let symbolName = (!isCardEnabled && disabledSymbol != nil) ? disabledSymbol! : normalSymbol
        let config = NSImage.SymbolConfiguration(pointSize: 14, weight: .regular)
        iconView.image = NSImage(systemSymbolName: symbolName, accessibilityDescription: nil)?
            .withSymbolConfiguration(config)

        setAccessibilityEnabled(isCardEnabled)
        setAccessibilityLabel("\(title), \(subtitle)")
        needsDisplay = true
    }

    private func setupView() {
        wantsLayer = true
        translatesAutoresizingMaskIntoConstraints = false
        setAccessibilityElement(true)
        setAccessibilityRole(.button)

        iconWell.wantsLayer = true
        iconWell.translatesAutoresizingMaskIntoConstraints = false
        iconWell.setAccessibilityElement(false)

        iconView.translatesAutoresizingMaskIntoConstraints = false
        iconView.imageScaling = .scaleProportionallyDown
        iconView.setAccessibilityElement(false)
        iconWell.addSubview(iconView)

        titleLabel.translatesAutoresizingMaskIntoConstraints = false
        titleLabel.font = .systemFont(ofSize: 12.5, weight: .medium)
        titleLabel.textColor = .labelColor
        titleLabel.lineBreakMode = .byTruncatingTail
        titleLabel.maximumNumberOfLines = 1

        subtitleLabel.translatesAutoresizingMaskIntoConstraints = false
        subtitleLabel.font = .systemFont(ofSize: 11, weight: .regular)
        subtitleLabel.textColor = .secondaryLabelColor
        subtitleLabel.lineBreakMode = .byTruncatingTail
        subtitleLabel.maximumNumberOfLines = 1

        let textStack = NSStackView(views: [titleLabel, subtitleLabel])
        textStack.translatesAutoresizingMaskIntoConstraints = false
        textStack.orientation = .vertical
        textStack.alignment = .leading
        textStack.spacing = 2

        addSubview(iconWell)
        addSubview(textStack)

        NSLayoutConstraint.activate([
            heightAnchor.constraint(equalToConstant: 58),
            iconWell.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            iconWell.centerYAnchor.constraint(equalTo: centerYAnchor),
            iconWell.widthAnchor.constraint(equalToConstant: 32),
            iconWell.heightAnchor.constraint(equalToConstant: 32),
            iconView.centerXAnchor.constraint(equalTo: iconWell.centerXAnchor),
            iconView.centerYAnchor.constraint(equalTo: iconWell.centerYAnchor),
            iconView.widthAnchor.constraint(equalToConstant: 16),
            iconView.heightAnchor.constraint(equalToConstant: 16),
            textStack.leadingAnchor.constraint(equalTo: iconWell.trailingAnchor, constant: 10),
            textStack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -12),
            textStack.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }
}

/// 底部状态栏微反馈轻量气泡视图：展示后台扫描提示文案与独立旋转微动效
final class StatusFeedbackCapsuleView: NSView {
    let label = NSTextField(labelWithString: "")
    let iconContainer = SearchResultsCenterSpinContainerView()
    let iconView = NSImageView()
    var onClick: (() -> Void)?

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        setupView()
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        setupView()
    }

    override var wantsUpdateLayer: Bool { true }

    override func updateLayer() {
        super.updateLayer()
        guard let layer = layer else { return }
        layer.cornerRadius = 5
        layer.borderWidth = 1.0
        layer.borderColor = NSColor.controlAccentColor.withAlphaComponent(0.32).cgColor
        layer.backgroundColor = NSColor.controlAccentColor.withAlphaComponent(0.14).cgColor
    }

    override func resetCursorRects() {
        super.resetCursorRects()
        addCursorRect(bounds, cursor: .pointingHand)
    }

    override func mouseDown(with event: NSEvent) {
        onClick?()
    }

    private func setupView() {
        wantsLayer = true
        translatesAutoresizingMaskIntoConstraints = false

        layer?.cornerRadius = 5
        layer?.borderWidth = 1.0
        layer?.borderColor = NSColor.controlAccentColor.withAlphaComponent(0.32).cgColor
        layer?.backgroundColor = NSColor.controlAccentColor.withAlphaComponent(0.14).cgColor

        label.translatesAutoresizingMaskIntoConstraints = false
        label.font = .systemFont(ofSize: 11, weight: .medium)
        label.textColor = .controlAccentColor
        label.alignment = .right
        addSubview(label)

        iconContainer.translatesAutoresizingMaskIntoConstraints = false
        addSubview(iconContainer)

        iconView.translatesAutoresizingMaskIntoConstraints = true
        iconView.frame = NSRect(x: 0, y: 0, width: 12, height: 12)
        iconView.wantsLayer = true
        let iconConfig = NSImage.SymbolConfiguration(pointSize: 11, weight: .medium)
        iconView.image = NSImage(
            systemSymbolName: "arrow.triangle.2.circlepath",
            accessibilityDescription: nil
        )?.withSymbolConfiguration(iconConfig) ?? NSImage(systemSymbolName: "arrow.clockwise", accessibilityDescription: nil)
        iconView.contentTintColor = .controlAccentColor
        iconContainer.addSubview(iconView)

        NSLayoutConstraint.activate([
            label.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 7),
            label.centerYAnchor.constraint(equalTo: centerYAnchor),

            iconContainer.leadingAnchor.constraint(equalTo: label.trailingAnchor, constant: 5),
            iconContainer.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -7),
            iconContainer.centerYAnchor.constraint(equalTo: centerYAnchor),
            iconContainer.widthAnchor.constraint(equalToConstant: 12),
            iconContainer.heightAnchor.constraint(equalToConstant: 12),

            heightAnchor.constraint(equalToConstant: 20),
        ])
    }

    var isSpinning: Bool {
        iconView.layer?.animation(forKey: "spin") != nil
    }

    func startSpin() {
        iconView.layer?.addInfiniteSpin()
    }

    func stopSpin() {
        iconView.layer?.removeAnimation(forKey: "spin")
    }

    func pulse() {
        guard let layer = layer else { return }
        layer.removeAnimation(forKey: "capsulePulse")
        let pop = CASpringAnimation(keyPath: "transform.scale")
        pop.fromValue = 0.92
        pop.toValue = 1.0
        pop.damping = 12
        pop.stiffness = 260
        pop.duration = pop.settlingDuration
        layer.add(pop, forKey: "capsulePulse")
    }
}

extension CALayer {
    /// 追加匀速无限旋转；自适应 AppKit 默认 (0, 0) 锚点与居中 (0.5, 0.5) 锚点，确保绝对绕几何中心原位旋转规避漂移
    func addInfiniteSpin() {
        let anim = CAKeyframeAnimation(keyPath: "transform")
        let w = bounds.width > 0 ? bounds.width : 20.0
        let h = bounds.height > 0 ? bounds.height : 20.0
        let cx = w * (0.5 - anchorPoint.x)
        let cy = h * (0.5 - anchorPoint.y)

        func makeTransform(angle: CGFloat) -> CATransform3D {
            var t = CATransform3DMakeTranslation(cx, cy, 0)
            t = CATransform3DRotate(t, angle, 0, 0, 1)
            t = CATransform3DTranslate(t, -cx, -cy, 0)
            return t
        }

        anim.values = [
            NSValue(caTransform3D: makeTransform(angle: 0)),
            NSValue(caTransform3D: makeTransform(angle: -CGFloat.pi * 0.5)),
            NSValue(caTransform3D: makeTransform(angle: -CGFloat.pi)),
            NSValue(caTransform3D: makeTransform(angle: -CGFloat.pi * 1.5)),
            NSValue(caTransform3D: makeTransform(angle: -CGFloat.pi * 2)),
        ]
        anim.keyTimes = [0, 0.25, 0.5, 0.75, 1.0]
        anim.duration = 1.0
        anim.repeatCount = .infinity
        anim.timingFunction = CAMediaTimingFunction(name: .linear)
        anim.isRemovedOnCompletion = false
        add(anim, forKey: "spin")
    }
}

// MARK: - Notification & Density Model

extension Notification.Name {
    static let tableDensityDidChange = Notification.Name("LokiiTableDensityDidChange")
}

/// 检索结果列表排版密度与尺寸联动模型
public enum TableDensity: Int, CaseIterable {
    case compact = 11
    case standard = 13
    case comfortable = 15
    case large = 17

    public static let minFontSize: Int = 11
    public static let maxFontSize: Int = 19
    public static let defaultFontSize: Int = 13
    public static let fontSizeUserDefaultsKey = "LokiiTableFontSize"

    public var fontSize: CGFloat { CGFloat(rawValue) }

    /// 档位对应的本地化标题键
    var titleKey: String {
        switch self {
        case .compact: return "density.compact"
        case .standard: return "density.standard"
        case .comfortable: return "density.comfortable"
        case .large: return "density.large"
        }
    }

    /// 生效字号，读写统一落在同一 UserDefaults 键；未持久化时回退默认档位
    static var savedFontSize: CGFloat {
        get {
            let saved = UserDefaults.standard.double(forKey: fontSizeUserDefaultsKey)
            return saved > 0 ? CGFloat(saved) : CGFloat(defaultFontSize)
        }
        set {
            UserDefaults.standard.set(Double(newValue), forKey: fontSizeUserDefaultsKey)
        }
    }

    /// 行高、图标与日期列宽随字号连续缩放，三者共享同一套分档边界，故边界只在此定义一次
    private static func tier(for fontSize: CGFloat) -> Int {
        switch Int(fontSize) {
        case ...11: return 0
        case 12...13: return 1
        case 14...15: return 2
        case 16...17: return 3
        default: return 4
        }
    }

    private static let rowHeights: [CGFloat] = [21, 24, 27, 30, 33]
    private static let iconSizes: [CGFloat] = [14, 16, 18, 20, 22]
    private static let dateColumnMinWidths: [CGFloat] = [130, 150, 170, 190, 210]

    public static func calculateRowHeight(for fontSize: CGFloat) -> CGFloat {
        rowHeights[tier(for: fontSize)]
    }

    public static func calculateIconSize(for fontSize: CGFloat) -> CGFloat {
        iconSizes[tier(for: fontSize)]
    }

    public static func calculateDateMinWidth(for fontSize: CGFloat) -> CGFloat {
        dateColumnMinWidths[tier(for: fontSize)]
    }
}

/// 列表字体缓存池：按生效字号集中构建，避免高频滚动时重复分配 CoreText 字体描述符
struct TableFontCache {
    let regular: NSFont
    let bold: NSFont
    let monospacedDigit: NSFont

    init(fontSize: CGFloat) {
        self.regular = .systemFont(ofSize: fontSize)
        self.bold = .systemFont(ofSize: fontSize, weight: .bold)
        self.monospacedDigit = .monospacedDigitSystemFont(ofSize: fontSize, weight: .regular)
    }
}

// MARK: - SearchResultsViewControllerDelegate

/// 检索虚拟列表事件代理协议
protocol SearchResultsViewControllerDelegate: AnyObject {
    /// 虚拟列表选中项变更回调
    func searchResultsDidSelect(result: FfiSearchResult?)
    /// 虚拟列表行双击确认回调
    func searchResultsDidDoubleClick(result: FfiSearchResult)
    /// 索引重建请求回调
    func searchResultsDidRequestRebuild()
    /// 虚拟列表首行上移回调，用于将焦点交还搜索输入框
    func searchResultsDidRequestFocusSearchField()
    /// 检索结果条目成功移入废纸篓回调（用于联动上层容器与目录聚合树同步）
    func searchResultsDidTrashItems(paths: Set<String>)
    /// 请求设置作用域目录过滤回调
    func searchResultsDidRequestScopeFilter(path: String)
    /// 检索结果条目被用户有效消费回调（如访达定位、终端唤起、复制、预览、拖拽等），用于归档有效搜索历史
    func searchResultsDidConsumeResult()
}

extension SearchResultsViewControllerDelegate {
    func searchResultsDidRequestRebuild() {}
    func searchResultsDidRequestFocusSearchField() {}
    func searchResultsDidTrashItems(paths: Set<String>) {}
    func searchResultsDidRequestScopeFilter(path: String) {}
    func searchResultsDidConsumeResult() {}
}

// MARK: - Breadcrumb Navigation & Delegation

/// 系统服务粘贴板类型清单：注册、请求应答与剪贴板写入共用同一份定义
enum ServicesPasteboard {
    static let filenamesType = NSPasteboard.PasteboardType("NSFilenamesPboardType")

    static let directoryTypes: Set<NSPasteboard.PasteboardType> = [
        NSPasteboard.PasteboardType("public.directory"),
        NSPasteboard.PasteboardType("public.folder"),
    ]

    static let generalTypes: Set<NSPasteboard.PasteboardType> = [
        .fileURL,
        filenamesType,
        .string,
        NSPasteboard.PasteboardType("public.plain-text"),
        NSPasteboard.PasteboardType("public.item"),
        NSPasteboard.PasteboardType("public.data"),
        NSPasteboard.PasteboardType("com.apple.resolvable"),
    ]

    static let allTypes = directoryTypes.union(generalTypes)

    /// 以文件对象形式写入剪贴板的类型子集
    static let fileObjectTypes = directoryTypes.union([.fileURL, filenamesType])
}

private var servicesMenuKey: UInt8 = 0

extension NSApplication {
    /// 确保 NSApp.servicesMenu 支持上下文菜单临时租约动态路由与成对恢复
    static let swizzleServicesMenuOnce: Void = {
        guard let originalGetter = class_getInstanceMethod(NSApplication.self, #selector(getter: NSApplication.servicesMenu)),
              let originalSetter = class_getInstanceMethod(NSApplication.self, #selector(setter: NSApplication.servicesMenu)),
              let swizzledGetter = class_getInstanceMethod(NSApplication.self, #selector(NSApplication.lokii_servicesMenu)),
              let swizzledSetter = class_getInstanceMethod(NSApplication.self, #selector(NSApplication.lokii_setServicesMenu(_:))) else {
            return
        }
        method_exchangeImplementations(originalGetter, swizzledGetter)
        method_exchangeImplementations(originalSetter, swizzledSetter)
    }()

    @objc func lokii_servicesMenu() -> NSMenu? {
        if let menu = objc_getAssociatedObject(self, &servicesMenuKey) as? NSMenu {
            return menu
        }
        return self.lokii_servicesMenu()
    }

    @objc func lokii_setServicesMenu(_ menu: NSMenu?) {
        objc_setAssociatedObject(self, &servicesMenuKey, menu, .OBJC_ASSOCIATION_RETAIN_NONATOMIC)
        self.lokii_setServicesMenu(menu)
    }
}

/// 面包屑路径导航交互代理协议
protocol BreadcrumbPathControlDelegate: AnyObject {
    /// 请求指定路径节点对应的上下文菜单
    func breadcrumbPathControl(_ control: BreadcrumbPathControl, menuFor url: URL) -> NSMenu?
}

/// 上下文菜单生命周期代理：成对管理系统服务菜单路由，并按需恢复面包屑高亮
final class BreadcrumbMenuDelegate: NSObject, NSMenuDelegate {
    private weak var previousServicesMenu: NSMenu?
    weak var activeComponentCell: NSPathComponentCell?
    var onClose: (() -> Void)?
    var onMenuWillOpen: ((NSMenu) -> Void)?

    override init() {
        super.init()
        _ = NSApplication.swizzleServicesMenuOnce
    }

    func menuWillOpen(_ menu: NSMenu) {
        onMenuWillOpen?(menu)
        previousServicesMenu = NSApp.servicesMenu
        if let servicesItem = menu.items.first(where: {
            $0.submenu?.title == L("search.ctx.services") ||
            $0.representedObject as? String == "search.ctx.services"
        }), let submenu = servicesItem.submenu {
            NSApp.servicesMenu = submenu
        }
    }

    func menuDidClose(_ menu: NSMenu) {
        NSApp.servicesMenu = previousServicesMenu
        previousServicesMenu = nil
        activeComponentCell?.isHighlighted = false
        activeComponentCell = nil
        let handler = onClose
        onClose = nil
        handler?()
    }
}

/// “打开方式”二级子菜单代理：仅在用户真正展开子菜单时按需查询 LaunchServices，避免主右键菜单弹出卡顿
private final class OpenWithMenuDelegate: NSObject, NSMenuDelegate {
    var onNeedsUpdate: ((NSMenu) -> Void)?

    func menuNeedsUpdate(_ menu: NSMenu) {
        onNeedsUpdate?(menu)
    }
}

/// 原生面包屑路径导航控件：支持组件单元格坐标精确几何命中与空白区域防穿透
final class BreadcrumbPathControl: NSPathControl {
    weak var actionDelegate: BreadcrumbPathControlDelegate?
    let menuDelegate = BreadcrumbMenuDelegate()
    private(set) var isOfflinePathMode = false
    private(set) var offlineComponentURLs: [URL] = []

    func setOfflineItems(_ items: [NSPathControlItem], urls: [URL]) {
        isOfflinePathMode = true
        offlineComponentURLs = urls
        url = nil
        pathItems = items
    }

    func clearOfflineState() {
        isOfflinePathMode = false
        offlineComponentURLs = []
    }

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool {
        return true
    }

    override func menu(for event: NSEvent) -> NSMenu? {
        let location = convert(event.locationInWindow, from: nil)
        guard let pathCell = cell as? NSPathCell else { return nil }

        for (index, componentCell) in pathCell.pathComponentCells.enumerated() {
            let rect = pathCell.rect(of: componentCell, withFrame: bounds, in: self)
            let resolvedURL: URL? = isOfflinePathMode
                ? (index < offlineComponentURLs.count ? offlineComponentURLs[index] : nil)
                : componentCell.url
            if rect.contains(location), let url = resolvedURL {
                componentCell.isHighlighted = true
                menuDelegate.activeComponentCell = componentCell
                guard let menu = actionDelegate?.breadcrumbPathControl(self, menuFor: url) else {
                    componentCell.isHighlighted = false
                    menuDelegate.activeComponentCell = nil
                    return nil
                }
                menu.delegate = menuDelegate
                return menu
            }
        }

        // 边界守卫：点击控件内部右侧空白无组件区域时阻断默认菜单，防止事件逃逸穿透
        return nil
    }
}

// MARK: - SearchResultsViewController

/// 原生 AppKit 高信息密度虚拟列表控制器
///
/// 承载检索命中结果的高性能虚拟列表渲染、多列权重排版、表头点击排序、
/// 匹配字符高亮、系统图标缓存、上下文菜单交互以及内聚式底栏状态呈现。
final class SearchResultsViewController: NSViewController {
    weak var delegate: SearchResultsViewControllerDelegate?

    let tableView = SearchTableView()
    let tableScrollView = NSScrollView()

    // ── 内聚底栏核心组件 ──
    let separator = NSBox()
    let bottomBar = NSView()
    let pathBar = BreadcrumbPathControl()
    let statusLabel = NSTextField(labelWithString: "")
    let inlineProgressIndicator: NSProgressIndicator = {
        let pi = NSProgressIndicator()
        pi.translatesAutoresizingMaskIntoConstraints = false
        pi.style = .spinning
        pi.controlSize = .small
        pi.isIndeterminate = true
        pi.isDisplayedWhenStopped = false
        pi.isHidden = true
        pi.wantsLayer = true
        return pi
    }()
    let rebuildContainer = SearchResultsCenterSpinContainerView()
    let rebuildButton = NSButton()
    let statusFeedbackCapsule = StatusFeedbackCapsuleView()
    let fdaHintButton: NSButton = InteractiveHintButton()
    private var hasFullDiskAccess: Bool = true
    /// 完全磁盘访问受限指引点击回调
    var onOpenFDAGuide: (() -> Void)?

    // MARK: - Status Bar State & Temporary Feedback
    /// 权威持久状态文本（由业务层或后台进度广播实时驱动）
    private var persistedStatusText: String = ""
    /// 当前是否处于临时状态反馈展示期
    private var isShowingTemporaryStatus: Bool = false
    /// 临时状态自动恢复任务
    private var temporaryStatusWorkItem: DispatchWorkItem?

    private(set) var results: [FfiSearchResult] = []
    private let iconCache: NSCache<NSString, NSImage> = {
        let cache = NSCache<NSString, NSImage>()
        cache.countLimit = 256
        return cache
    }()
    /// 基于文件类型/扩展名的纯内存图标缓存（仅在主线程读写），规避普通文件逐路径查盘与缓存颠簸
    private var typeIconCache: [String: NSImage] = [:]
    /// 应用包自定义图标异步加载队列，确保主线程滚动零磁盘 I/O
    private let iconLoadQueue = DispatchQueue(label: "com.lokii.results.iconLoad", qos: .userInitiated)
    private var openWithAppNameCache: [String: String] = [:]
    private var openWithAppIconCache: [String: NSImage] = [:]
    private let openWithMenuDelegate = OpenWithMenuDelegate()
    /// 表头右键列显隐菜单代理（展开时动态刷新勾选态与当前语言标题）
    private let columnHeaderMenuDelegate = OpenWithMenuDelegate()
    /// 强持有主表头视图，便于空态隐藏表头后无损恢复列头与右键列显隐菜单
    private(set) var savedHeaderView: NSTableHeaderView?

    static let hiddenColumnsDefaultsKey = "LokiiHiddenResultColumns"
    static let defaultHiddenColumnIdentifiers: Set<NSUserInterfaceItemIdentifier> = [
        .columnCreated,
        .columnExtension,
    ]
    private static let canonicalColumnDefinitions: [(id: NSUserInterfaceItemIdentifier, titleKey: String, width: CGFloat, sortKey: String)] = [
        (.columnName, "search.col.name", 280, "name"),
        (.columnPath, "search.col.path", 240, "path"),
        (.columnModified, "search.col.dateModified", 160, "modified"),
        (.columnCreated, "search.col.dateCreated", 160, "created"),
        (.columnExtension, "search.col.extension", 80, "extension"),
        (.columnSize, "search.col.size", 80, "size"),
    ]

    private var lastReportedPath: String?
    private var lastReportedOffline: Bool?
    private var lastRenderedBottomPath: String?
    private var lastRenderedBottomOffline: Bool = false
    private var hasRenderedBottomPath: Bool = false
    private(set) var currentFontSize: CGFloat = CGFloat(TableDensity.defaultFontSize)
    private(set) var fontCache = TableFontCache(fontSize: CGFloat(TableDensity.defaultFontSize))
    private(set) var currentIconSize: CGFloat = TableDensity.calculateIconSize(for: CGFloat(TableDensity.defaultFontSize))

    /// 注入式废纸篓操作闭包，默认调用系统 Foundation 标准接口
    var trashItemHandler: (URL) throws -> URL = { url in
        var resultingURL: NSURL?
        try FileManager.default.trashItem(at: url, resultingItemURL: &resultingURL)
        return (resultingURL as URL?) ?? url
    }

    /// 注入式访达定位操作闭包，默认调用系统 NSWorkspace 接口在父级视窗选中高亮目标条目
    var revealFilesHandler: ([URL]) -> Void = { urls in
        NSWorkspace.shared.activateFileViewerSelecting(urls)
    }

    /// 注入式终端唤起操作闭包，默认调用 TerminalLauncher 在目标目录启动终端应用
    var openInTerminalHandler: ([URL]) -> Void = { urls in
        TerminalLauncher.openBatch(at: urls)
    }

    /// 注入式将目录加入排除配置操作闭包
    var onExcludeDirectory: ((String) -> Void)?

    /// 注入式确认弹窗闭包，默认通过主视窗弹出原生确认 Sheet，返回用户是否确认执行
    var confirmTrashHandler: ((Int, @escaping (Bool) -> Void) -> Void)?

    /// In-memory offline path predicate supplied by SearchEngine; avoids disk checks on UI events.
    var isPathOfflineHandler: ((String) -> Bool)?

    /// Injectable workspace icon lookup used for online entries; bypassed completely for offline entries.
    var workspaceIconProvider: (String) -> NSImage = { path in
        NSWorkspace.shared.icon(forFile: path)
    }

    struct AppleScriptOutcome {
        let succeeded: Bool
        let errorNumber: Int?
        let errorMessage: String?
    }

    static let automationPrivacySettingsURLs = [
        "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Automation",
        "x-apple.systempreferences:com.apple.preference.security?Privacy_Automation",
    ]

    private static var isPresentingAutomationAlert = false

    static func openAutomationPrivacySettings() {
        for rawValue in automationPrivacySettingsURLs {
            guard let url = URL(string: rawValue) else { continue }
            if NSWorkspace.shared.open(url) {
                return
            }
        }
    }

    static func presentAutomationPermissionAlert() {
        guard !isPresentingAutomationAlert else { return }
        isPresentingAutomationAlert = true
        defer { isPresentingAutomationAlert = false }

        // System TCC prompts or Finder activation steal focus and hide SearchWindow when
        // hideOnBlur is enabled, so force-activate the app (consistent with SearchWindow /
        // AppDelegate) before running a modal alert even in accessory mode on macOS 14+.
        NSApp.activate(ignoringOtherApps: true)

        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = L("search.getInfo.automationDenied.title")
        alert.informativeText = L("search.getInfo.automationDenied.message")
        alert.addButton(withTitle: L("search.getInfo.automationDenied.openSettings"))
        alert.addButton(withTitle: L("fda.later"))
        if alert.runModal() == .alertFirstButtonReturn {
            openAutomationPrivacySettings()
        }
    }

    /// Injectable AppleScript runner for Finder Get Info actions.
    var appleScriptExecutor: (String) -> AppleScriptOutcome = { source in
        guard let script = NSAppleScript(source: source) else {
            return AppleScriptOutcome(
                succeeded: false,
                errorNumber: nil,
                errorMessage: nil
            )
        }
        var error: NSDictionary?
        script.executeAndReturnError(&error)
        if let error = error {
            return AppleScriptOutcome(
                succeeded: false,
                errorNumber: error[NSAppleScript.errorNumber] as? Int,
                errorMessage: error[NSAppleScript.errorMessage] as? String
            )
        }
        return AppleScriptOutcome(succeeded: true, errorNumber: nil, errorMessage: nil)
    }

    /// Invoked on main thread when Finder Automation permission (-1743) is denied.
    var automationPermissionDeniedHandler: () -> Void = {
        SearchResultsViewController.presentAutomationPermissionAlert()
    }

    /// 上下文作用域租约：当面包屑弹出右键菜单时临时接管有效载荷，规避与主列表选区发生交叉错位
    var activeContextMenuPayload: [URL]?

    // MARK: - Empty State View
    enum EmptyState: Equatable {
        case noMatches
        case excluded(rule: String)
        case indexing
        case idleEmpty
    }

    /// 统一判定是否应进入空搜索隐藏态：仅当开关开启、输入为空（含纯空格）、过滤气泡无约束、目录树未选中且索引空闲时成立
    static func shouldShowIdleEmptyState(
        hideWhenEmpty: Bool,
        query: String,
        isFilterUnconstrained: Bool,
        hasActiveDirectory: Bool,
        isIndexingIdle: Bool
    ) -> Bool {
        hideWhenEmpty
            && query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && isFilterUnconstrained
            && !hasActiveDirectory
            && isIndexingIdle
    }

    var emptyState: EmptyState = .noMatches {
        didSet {
            if oldValue != emptyState { renderEmptyState() }
        }
    }
    var onManageIndexRules: (() -> Void)?
    var onSelectSyntaxExample: ((String) -> Void)?
    var onSelectRecentSearch: ((String) -> Void)?
    var onClearRecentSearches: (() -> Void)?
    var onFocusSearchField: (() -> Void)?
    var onEnableFuzzyMode: (() -> Void)?
    var onOpenFilterPopover: (() -> Void)?

    let emptyStateContainer = NSStackView()
    private let emptyIconBadge = EmptyIconBadgeView()
    var emptyIconImageView: NSImageView { emptyIconBadge.imageView }
    let emptyTitleLabel = NSTextField(labelWithString: "")
    let emptySubtitleLabel = NSTextField(wrappingLabelWithString: "")
    let emptyRuleRow = NSStackView()
    private let emptyRuleTagLabel = NSTextField(labelWithString: "")
    let emptyRuleNameLabel = NSTextField(labelWithString: "")
    private let emptyManageRulesButton = NSButton()

    let emptySyntaxSection = NSStackView()
    private let emptySyntaxSectionTitle = NSTextField(labelWithString: "")
    let emptySyntaxGrid = NSStackView()
    private(set) var syntaxTipButtons: [SyntaxTipCardView] = []

    let emptyRecentSection = NSStackView()
    private let emptyRecentDivider = NSBox()
    private let emptyRecentHeaderRow = NSStackView()
    private let emptyRecentTitleLabel = NSTextField(labelWithString: "")
    private let emptyClearRecentButton = InteractiveHintButton()
    private let emptyRecentChipsContainer = FlowWrapView()
    private(set) var recentSearchChips: [RecentSearchChipView] = []
    private var recentSearchQueries: [String] = []
    private static let maxEmptyStateRecentChips = 5

    let emptyNoMatchActionsGrid = NSStackView()
    private(set) var noMatchActionCards: [EmptyActionCardView] = []
    private var emptyContextQuery: String = ""
    private var isFuzzyModeActive: Bool = false

    private static let idleSyntaxExamples: [(example: String, descKey: String)] = [
        ("*.pdf", "search.empty.idle.tip.wildcard"),
        ("ext:pdf,docx", "search.empty.idle.tip.ext"),
        ("foo | bar", "search.empty.idle.tip.or"),
        ("size:>10m", "search.empty.idle.tip.size"),
        ("!node_modules", "search.empty.idle.tip.exclude"),
        ("in:Desktop", "search.empty.idle.tip.scope"),
    ]

    private static let idleSearchSymbolImage = NSImage(systemSymbolName: "magnifyingglass", accessibilityDescription: nil)?
        .withSymbolConfiguration(NSImage.SymbolConfiguration(pointSize: 22, weight: .regular))
    private static let noMatchSymbolImage = NSImage(systemSymbolName: "doc.text.magnifyingglass", accessibilityDescription: nil)?
        .withSymbolConfiguration(NSImage.SymbolConfiguration(pointSize: 22, weight: .regular))
    private static let excludedSymbolImage = NSImage(systemSymbolName: "folder.badge.minus", accessibilityDescription: nil)?
        .withSymbolConfiguration(NSImage.SymbolConfiguration(pointSize: 22, weight: .regular))
    private static let indexingSymbolImage = NSImage(systemSymbolName: "arrow.triangle.2.circlepath", accessibilityDescription: nil)?
        .withSymbolConfiguration(NSImage.SymbolConfiguration(pointSize: 22, weight: .regular))

    /// 首次结果集是否已写入；建库完成到默认列表返回之间结果仍为空，此时不能把空态当成「未找到匹配项」闪出来
    private var hasAppliedResults = false

    /// 同步空态引导页的最近搜索胶囊（最多展示 5 条），并在当前处于 `.idleEmpty` 时即时刷新显隐
    func updateRecentSearches(_ queries: [String]) {
        let clamped = Array(queries.prefix(Self.maxEmptyStateRecentChips))
        guard clamped != recentSearchQueries else { return }
        recentSearchQueries = clamped
        rebuildRecentSearchChips()
        renderEmptyState()
    }

    /// 同步无结果态的当前搜索词与模糊匹配激活状态，动态刷新副标题回显与“尝试模糊匹配”卡片可用态
    func updateEmptyContext(query: String, isFuzzyActive: Bool) {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed != emptyContextQuery || isFuzzyActive != isFuzzyModeActive else { return }
        emptyContextQuery = trimmed
        isFuzzyModeActive = isFuzzyActive
        if noMatchActionCards.count > 1 {
            noMatchActionCards[1].isCardEnabled = !isFuzzyActive
        }
        renderEmptyState()
    }

    /// 供自动化测试触发“清空历史”动作，与用户点击按钮走同一路径
    func triggerClearRecentSearchesForTesting() {
        clearRecentSearchesClicked()
    }

    private func rebuildRecentSearchChips() {
        for subview in emptyRecentChipsContainer.subviews {
            subview.removeFromSuperview()
        }
        recentSearchChips.removeAll()
        for query in recentSearchQueries {
            let chip = RecentSearchChipView(query: query)
            chip.onSelect = { [weak self] selected in
                self?.onSelectRecentSearch?(selected)
            }
            recentSearchChips.append(chip)
            emptyRecentChipsContainer.addSubview(chip)
        }
        emptyRecentChipsContainer.recalculateLayout()
        emptyRecentChipsContainer.needsLayout = true
    }

    private static func truncatedEmptyQueryDisplay(_ query: String) -> String {
        let maxLen = 36
        guard query.count > maxLen else { return query }
        let head = query.prefix(17)
        let tail = query.suffix(18)
        return "\(head)…\(tail)"
    }

    private func renderEmptyState() {
        let shouldCollapseHeader = results.isEmpty && emptyState == .idleEmpty
        if shouldCollapseHeader {
            if tableView.headerView != nil {
                tableView.headerView = nil
            }
        } else if let savedHeader = savedHeaderView, tableView.headerView !== savedHeader {
            tableView.headerView = savedHeader
        }

        emptyStateContainer.isHidden = !results.isEmpty || (emptyState == .noMatches && !hasAppliedResults)
        guard results.isEmpty else { return }

        let isExcluded: Bool
        let isIdleEmpty: Bool
        let isNoMatches: Bool
        switch emptyState {
        case .noMatches:
            emptyTitleLabel.stringValue = L("search.empty.title")
            emptyTitleLabel.font = .systemFont(ofSize: 18, weight: .semibold)
            if emptyContextQuery.isEmpty {
                emptySubtitleLabel.stringValue = L("search.empty.subtitle")
            } else {
                emptySubtitleLabel.stringValue = String(
                    format: L("search.empty.subtitleWithQuery"),
                    Self.truncatedEmptyQueryDisplay(emptyContextQuery)
                )
            }
            emptyIconImageView.image = Self.noMatchSymbolImage
            for (index, card) in noMatchActionCards.enumerated() {
                if index == 1 {
                    card.isCardEnabled = !isFuzzyModeActive
                }
                card.refreshContent()
            }
            isExcluded = false
            isIdleEmpty = false
            isNoMatches = true
        case .excluded(let rule):
            emptyTitleLabel.stringValue = L("search.empty.excluded.title")
            emptyTitleLabel.font = .systemFont(ofSize: 17, weight: .semibold)
            emptySubtitleLabel.stringValue = L("search.empty.excluded.subtitle")
            emptyRuleNameLabel.stringValue = rule
            emptyIconImageView.image = Self.excludedSymbolImage
            isExcluded = true
            isIdleEmpty = false
            isNoMatches = false
        case .indexing:
            emptyTitleLabel.stringValue = L("search.empty.indexing.title")
            emptyTitleLabel.font = .systemFont(ofSize: 17, weight: .semibold)
            emptySubtitleLabel.stringValue = L("search.empty.indexing.subtitle")
            emptyIconImageView.image = Self.indexingSymbolImage
            isExcluded = false
            isIdleEmpty = false
            isNoMatches = false
        case .idleEmpty:
            emptyTitleLabel.stringValue = L("search.empty.idle.title")
            emptyTitleLabel.font = .systemFont(ofSize: 18, weight: .semibold)
            emptySubtitleLabel.stringValue = L("search.empty.idle.subtitle")
            emptyIconImageView.image = Self.idleSearchSymbolImage
            emptySyntaxSectionTitle.stringValue = L("search.empty.idle.section.syntax")
            emptyRecentTitleLabel.stringValue = L("search.empty.idle.section.recent")
            updateClearRecentButtonTitle()
            for card in syntaxTipButtons {
                card.refreshLocalization()
            }
            isExcluded = false
            isIdleEmpty = true
            isNoMatches = false
        }

        emptyRuleTagLabel.stringValue = "\(L("search.empty.excluded.tag")): "
        emptyManageRulesButton.title = L("search.empty.manageRules")
        emptyManageRulesButton.setAccessibilityLabel(L("search.empty.manageRules"))
        emptySubtitleLabel.isHidden = false
        emptyRuleRow.isHidden = !isExcluded
        emptyManageRulesButton.isHidden = !isExcluded
        emptySyntaxSection.isHidden = !isIdleEmpty
        emptyRecentSection.isHidden = !(isIdleEmpty && !recentSearchQueries.isEmpty)
        emptyNoMatchActionsGrid.isHidden = !isNoMatches
    }

    /// 主检索列表右键上下文菜单代理：成对维护 NSApp.servicesMenu 状态机
    private let tableMenuDelegate = BreadcrumbMenuDelegate()

    /// 时间与体积的展示串只取决于原始数值与当前语言，按原始值缓存以规避滚动时对每个进入视口的行重复格式化
    private var dateTextCache: [TimeInterval: String] = [:]
    private var sizeTextCache: [UInt64: String] = [:]

    private static let dateFormatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateFormat = "yyyy/M/d, HH:mm"
        return formatter
    }()

    private static let sizeFormatter: ByteCountFormatter = {
        let formatter = ByteCountFormatter()
        formatter.countStyle = .file
        return formatter
    }()

    init() {
        super.init(nibName: nil, bundle: nil)
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
    }

    deinit {
        temporaryStatusWorkItem?.cancel()
        statusFeedbackCapsule.stopSpin()
        NotificationCenter.default.removeObserver(self)
    }

    override func loadView() {
        let containerView = NSView()
        containerView.wantsLayer = true

        tableScrollView.translatesAutoresizingMaskIntoConstraints = false
        tableScrollView.documentView = tableView
        tableScrollView.hasVerticalScroller = true
        tableScrollView.hasHorizontalScroller = false
        tableScrollView.autohidesScrollers = true
        tableScrollView.borderType = .noBorder
        tableScrollView.drawsBackground = true
        // 显式禁用自动内容边距推导，由 safeAreaLayoutGuide 约束精确控制视口边界，
        // 彻底杜绝 AppKit 默认滚动边距机制导致数据行向上穿越表头并渗透进标题栏工具区
        tableScrollView.automaticallyAdjustsContentInsets = false
        tableScrollView.contentInsets = NSEdgeInsetsZero
        containerView.addSubview(tableScrollView)

        separator.translatesAutoresizingMaskIntoConstraints = false
        separator.boxType = .separator
        containerView.addSubview(separator)

        bottomBar.translatesAutoresizingMaskIntoConstraints = false
        bottomBar.wantsLayer = true
        containerView.addSubview(bottomBar)

        setupBottomBar()

        setupEmptyState(in: containerView)

        NSLayoutConstraint.activate([
            // 顶端锚定至安全区布局辅助线，使表头与滚动视口始终对齐至窗口工具栏底缘，
            // 滚动时列表内容在表头底线处干净截断
            tableScrollView.topAnchor.constraint(equalTo: containerView.safeAreaLayoutGuide.topAnchor),
            tableScrollView.leadingAnchor.constraint(equalTo: containerView.leadingAnchor),
            tableScrollView.trailingAnchor.constraint(equalTo: containerView.trailingAnchor),
            tableScrollView.bottomAnchor.constraint(equalTo: separator.topAnchor),

            separator.leadingAnchor.constraint(equalTo: containerView.leadingAnchor),
            separator.trailingAnchor.constraint(equalTo: containerView.trailingAnchor),
            separator.bottomAnchor.constraint(equalTo: bottomBar.topAnchor),

            bottomBar.leadingAnchor.constraint(equalTo: containerView.leadingAnchor),
            bottomBar.trailingAnchor.constraint(equalTo: containerView.trailingAnchor),
            bottomBar.bottomAnchor.constraint(equalTo: containerView.bottomAnchor),
            bottomBar.heightAnchor.constraint(equalToConstant: 28),

            emptyStateContainer.centerXAnchor.constraint(equalTo: tableScrollView.centerXAnchor),
            emptyStateContainer.centerYAnchor.constraint(equalTo: tableScrollView.centerYAnchor, constant: -12),
            emptyStateContainer.widthAnchor.constraint(lessThanOrEqualTo: tableScrollView.widthAnchor, constant: -56),
        ])

        self.view = containerView
    }

    private func setupEmptyState(in containerView: NSView) {
        emptyStateContainer.translatesAutoresizingMaskIntoConstraints = false
        emptyStateContainer.orientation = .vertical
        emptyStateContainer.alignment = .centerX
        emptyStateContainer.spacing = 8
        containerView.addSubview(emptyStateContainer)

        emptyTitleLabel.font = .systemFont(ofSize: 18, weight: .semibold)
        emptyTitleLabel.textColor = .labelColor
        emptyTitleLabel.alignment = .center
        emptySubtitleLabel.font = .systemFont(ofSize: 12.5)
        emptySubtitleLabel.textColor = .secondaryLabelColor
        emptySubtitleLabel.alignment = .center
        emptySubtitleLabel.maximumNumberOfLines = 2

        emptyRuleRow.orientation = .horizontal
        emptyRuleRow.alignment = .centerY
        emptyRuleRow.spacing = 4

        emptyRuleTagLabel.font = .systemFont(ofSize: 12)
        emptyRuleTagLabel.textColor = .secondaryLabelColor
        emptyRuleTagLabel.setContentCompressionResistancePriority(.required, for: .horizontal)

        emptyRuleNameLabel.font = .systemFont(ofSize: 12, weight: .medium)
        emptyRuleNameLabel.textColor = .labelColor
        emptyRuleNameLabel.lineBreakMode = .byTruncatingMiddle
        emptyRuleNameLabel.maximumNumberOfLines = 1

        emptyRuleRow.addArrangedSubview(emptyRuleTagLabel)
        emptyRuleRow.addArrangedSubview(emptyRuleNameLabel)

        emptyManageRulesButton.bezelStyle = .rounded
        emptyManageRulesButton.controlSize = .regular
        emptyManageRulesButton.image = NSImage(systemSymbolName: "slider.horizontal.3", accessibilityDescription: nil)?
            .withSymbolConfiguration(NSImage.SymbolConfiguration(pointSize: 12, weight: .regular))
        emptyManageRulesButton.imagePosition = .imageLeading
        emptyManageRulesButton.contentTintColor = .secondaryLabelColor
        emptyManageRulesButton.target = self
        emptyManageRulesButton.action = #selector(manageIndexRulesClicked)

        // 1. 「试试这样搜索」语法分区
        emptySyntaxSection.translatesAutoresizingMaskIntoConstraints = false
        emptySyntaxSection.orientation = .vertical
        emptySyntaxSection.alignment = .leading
        emptySyntaxSection.spacing = 8

        emptySyntaxSectionTitle.translatesAutoresizingMaskIntoConstraints = false
        emptySyntaxSectionTitle.font = .systemFont(ofSize: 11.5, weight: .medium)
        emptySyntaxSectionTitle.textColor = .secondaryLabelColor

        emptySyntaxGrid.translatesAutoresizingMaskIntoConstraints = false
        emptySyntaxGrid.orientation = .vertical
        emptySyntaxGrid.alignment = .centerX
        emptySyntaxGrid.spacing = 8
        syntaxTipButtons.removeAll()
        for rowIndex in stride(from: 0, to: Self.idleSyntaxExamples.count, by: 2) {
            let rowStack = NSStackView()
            rowStack.translatesAutoresizingMaskIntoConstraints = false
            rowStack.orientation = .horizontal
            rowStack.distribution = .fillEqually
            rowStack.spacing = 12
            for offset in 0..<2 where rowIndex + offset < Self.idleSyntaxExamples.count {
                let item = Self.idleSyntaxExamples[rowIndex + offset]
                let card = SyntaxTipCardView(syntaxExample: item.example, descriptionKey: item.descKey)
                card.onSelect = { [weak self] example in
                    self?.onSelectSyntaxExample?(example)
                }
                syntaxTipButtons.append(card)
                rowStack.addArrangedSubview(card)
            }
            emptySyntaxGrid.addArrangedSubview(rowStack)
            rowStack.widthAnchor.constraint(equalTo: emptySyntaxGrid.widthAnchor).isActive = true
        }
        emptySyntaxSection.addArrangedSubview(emptySyntaxSectionTitle)
        emptySyntaxSection.addArrangedSubview(emptySyntaxGrid)

        // 2. 「最近搜索」分区（仅存在历史记录时显示）
        emptyRecentSection.translatesAutoresizingMaskIntoConstraints = false
        emptyRecentSection.orientation = .vertical
        emptyRecentSection.alignment = .leading
        emptyRecentSection.spacing = 8

        emptyRecentDivider.translatesAutoresizingMaskIntoConstraints = false
        emptyRecentDivider.boxType = .separator

        emptyRecentHeaderRow.translatesAutoresizingMaskIntoConstraints = false
        emptyRecentHeaderRow.orientation = .horizontal
        emptyRecentHeaderRow.alignment = .centerY
        emptyRecentHeaderRow.spacing = 8

        emptyRecentTitleLabel.translatesAutoresizingMaskIntoConstraints = false
        emptyRecentTitleLabel.font = .systemFont(ofSize: 11.5, weight: .medium)
        emptyRecentTitleLabel.textColor = .secondaryLabelColor

        let spacer = NSView()
        spacer.translatesAutoresizingMaskIntoConstraints = false
        spacer.setContentHuggingPriority(.defaultLow, for: .horizontal)

        emptyClearRecentButton.translatesAutoresizingMaskIntoConstraints = false
        emptyClearRecentButton.isBordered = false
        emptyClearRecentButton.focusRingType = .none
        emptyClearRecentButton.contentTintColor = .secondaryLabelColor
        let trashConfig = NSImage.SymbolConfiguration(pointSize: 11, weight: .regular)
        emptyClearRecentButton.image = NSImage(systemSymbolName: "trash", accessibilityDescription: nil)?
            .withSymbolConfiguration(trashConfig)
        emptyClearRecentButton.imagePosition = .imageTrailing
        emptyClearRecentButton.imageHugsTitle = true
        emptyClearRecentButton.target = self
        emptyClearRecentButton.action = #selector(clearRecentSearchesClicked)
        updateClearRecentButtonTitle()

        emptyRecentHeaderRow.addArrangedSubview(emptyRecentTitleLabel)
        emptyRecentHeaderRow.addArrangedSubview(spacer)
        emptyRecentHeaderRow.addArrangedSubview(emptyClearRecentButton)

        emptyRecentChipsContainer.translatesAutoresizingMaskIntoConstraints = false
        emptyRecentSection.addArrangedSubview(emptyRecentDivider)
        emptyRecentSection.addArrangedSubview(emptyRecentHeaderRow)
        emptyRecentSection.addArrangedSubview(emptyRecentChipsContainer)
        emptyRecentSection.setCustomSpacing(12, after: emptyRecentDivider)

        // 3. 无匹配结果 2x2 可点击行动建议卡片
        emptyNoMatchActionsGrid.translatesAutoresizingMaskIntoConstraints = false
        emptyNoMatchActionsGrid.orientation = .vertical
        emptyNoMatchActionsGrid.alignment = .centerX
        emptyNoMatchActionsGrid.spacing = 10

        let editQueryCard = EmptyActionCardView(
            symbol: "textformat.abc",
            titleKey: "search.empty.action.checkKeyword.title",
            subtitleKey: "search.empty.action.checkKeyword.subtitle"
        )
        editQueryCard.onAction = { [weak self] in
            self?.onFocusSearchField?()
        }

        let fuzzyCard = EmptyActionCardView(
            symbol: "sparkle.magnifyingglass",
            disabledSymbol: "checkmark.circle",
            titleKey: "search.empty.action.tryFuzzy.title",
            subtitleKey: "search.empty.action.tryFuzzy.subtitle",
            disabledSubtitleKey: "search.empty.action.tryFuzzy.activeSubtitle"
        )
        fuzzyCard.onAction = { [weak self] in
            self?.onEnableFuzzyMode?()
        }

        let filterCard = EmptyActionCardView(
            symbol: "slider.horizontal.3",
            titleKey: "search.empty.action.checkFilters.title",
            subtitleKey: "search.empty.action.checkFilters.subtitle"
        )
        filterCard.onAction = { [weak self] in
            self?.onOpenFilterPopover?()
        }

        let scopeCard = EmptyActionCardView(
            symbol: "folder.badge.questionmark",
            titleKey: "search.empty.action.checkScope.title",
            subtitleKey: "search.empty.action.checkScope.subtitle"
        )
        scopeCard.onAction = { [weak self] in
            self?.onManageIndexRules?()
        }

        noMatchActionCards = [editQueryCard, fuzzyCard, filterCard, scopeCard]
        for rowIndex in stride(from: 0, to: noMatchActionCards.count, by: 2) {
            let rowStack = NSStackView()
            rowStack.translatesAutoresizingMaskIntoConstraints = false
            rowStack.orientation = .horizontal
            rowStack.distribution = .fillEqually
            rowStack.spacing = 12
            rowStack.addArrangedSubview(noMatchActionCards[rowIndex])
            rowStack.addArrangedSubview(noMatchActionCards[rowIndex + 1])
            emptyNoMatchActionsGrid.addArrangedSubview(rowStack)
            rowStack.widthAnchor.constraint(equalTo: emptyNoMatchActionsGrid.widthAnchor).isActive = true
        }

        for element in [
            emptyIconBadge,
            emptyTitleLabel,
            emptySubtitleLabel,
            emptyRuleRow,
            emptyManageRulesButton,
            emptySyntaxSection,
            emptyRecentSection,
            emptyNoMatchActionsGrid,
        ] {
            element.translatesAutoresizingMaskIntoConstraints = false
            emptyStateContainer.addArrangedSubview(element)
        }
        emptyStateContainer.setCustomSpacing(10, after: emptyIconBadge)
        emptyStateContainer.setCustomSpacing(5, after: emptyTitleLabel)
        emptyStateContainer.setCustomSpacing(18, after: emptySubtitleLabel)
        emptyStateContainer.setCustomSpacing(16, after: emptyRuleRow)
        emptyStateContainer.setCustomSpacing(16, after: emptySyntaxSection)

        let preferredWidth = emptyStateContainer.widthAnchor.constraint(equalToConstant: 560)
        preferredWidth.priority = .defaultHigh
        let actionsWidth = emptyNoMatchActionsGrid.widthAnchor.constraint(equalToConstant: 560)
        actionsWidth.priority = .defaultHigh
        NSLayoutConstraint.activate([
            preferredWidth,
            emptyTitleLabel.widthAnchor.constraint(equalTo: emptyStateContainer.widthAnchor),
            emptySubtitleLabel.widthAnchor.constraint(equalTo: emptyStateContainer.widthAnchor),
            emptyRuleNameLabel.widthAnchor.constraint(lessThanOrEqualToConstant: 380),
            emptySyntaxSection.widthAnchor.constraint(equalTo: emptyStateContainer.widthAnchor),
            emptySyntaxGrid.widthAnchor.constraint(equalTo: emptySyntaxSection.widthAnchor),
            emptyRecentSection.widthAnchor.constraint(equalTo: emptyStateContainer.widthAnchor),
            emptyRecentDivider.widthAnchor.constraint(equalTo: emptyRecentSection.widthAnchor),
            emptyRecentHeaderRow.widthAnchor.constraint(equalTo: emptyRecentSection.widthAnchor),
            emptyRecentChipsContainer.widthAnchor.constraint(equalTo: emptyRecentSection.widthAnchor),
            actionsWidth,
            emptyNoMatchActionsGrid.widthAnchor.constraint(lessThanOrEqualTo: emptyStateContainer.widthAnchor),
        ])
        renderEmptyState()
    }

    private func updateClearRecentButtonTitle() {
        let title = L("search.empty.idle.clearHistory")
        emptyClearRecentButton.attributedTitle = NSAttributedString(
            string: title + " ",
            attributes: [
                .font: NSFont.systemFont(ofSize: 11.5, weight: .regular),
                .foregroundColor: NSColor.secondaryLabelColor,
            ]
        )
        emptyClearRecentButton.setAccessibilityLabel(title)
    }

    @objc private func manageIndexRulesClicked() {
        onManageIndexRules?()
    }

    @objc private func clearRecentSearchesClicked() {
        onClearRecentSearches?()
    }

    private func setupBottomBar() {
        pathBar.translatesAutoresizingMaskIntoConstraints = false
        pathBar.pathStyle = .standard
        pathBar.isEditable = false
        pathBar.actionDelegate = self
        pathBar.font = .systemFont(ofSize: 12)
        pathBar.backgroundColor = .clear
        pathBar.focusRingType = .none
        pathBar.refusesFirstResponder = true
        pathBar.target = self
        pathBar.action = #selector(pathBarClicked)
        pathBar.url = nil
        pathBar.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        pathBar.setContentHuggingPriority(.defaultLow, for: .horizontal)
        bottomBar.addSubview(pathBar)

        fdaHintButton.translatesAutoresizingMaskIntoConstraints = false
        fdaHintButton.isBordered = false
        fdaHintButton.bezelStyle = .inline
        fdaHintButton.setButtonType(.momentaryPushIn)
        fdaHintButton.focusRingType = .none
        fdaHintButton.refusesFirstResponder = true
        fdaHintButton.target = self
        fdaHintButton.action = #selector(fdaHintButtonClicked)
        fdaHintButton.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        fdaHintButton.setContentHuggingPriority(.defaultLow, for: .horizontal)
        fdaHintButton.isHidden = true
        updateFDAButtonContent()
        bottomBar.addSubview(fdaHintButton)

        bottomBar.addSubview(inlineProgressIndicator)

        statusLabel.translatesAutoresizingMaskIntoConstraints = false
        statusLabel.font = .systemFont(ofSize: 12)
        statusLabel.textColor = .secondaryLabelColor
        statusLabel.alignment = .right
        statusLabel.setContentHuggingPriority(.defaultHigh, for: .horizontal)
        bottomBar.addSubview(statusLabel)

        rebuildContainer.translatesAutoresizingMaskIntoConstraints = false
        bottomBar.addSubview(rebuildContainer)

        rebuildButton.translatesAutoresizingMaskIntoConstraints = true
        rebuildButton.frame = NSRect(x: 0, y: 0, width: 20, height: 20)
        rebuildButton.wantsLayer = true
        rebuildButton.image = NSImage(systemSymbolName: "arrow.clockwise", accessibilityDescription: "Rebuild Index")
        rebuildButton.bezelStyle = .toolbar
        rebuildButton.isBordered = false
        rebuildButton.toolTip = L("search.rebuildIndex.tooltip")
        rebuildButton.contentTintColor = .secondaryLabelColor
        rebuildButton.target = self
        rebuildButton.action = #selector(rebuildButtonClicked)
        rebuildButton.isEnabled = false
        rebuildContainer.addSubview(rebuildButton)

        statusFeedbackCapsule.translatesAutoresizingMaskIntoConstraints = false
        statusFeedbackCapsule.isHidden = true
        statusFeedbackCapsule.alphaValue = 0.0
        statusFeedbackCapsule.onClick = { [weak self] in
            self?.rebuildButtonClicked()
        }
        bottomBar.addSubview(statusFeedbackCapsule)

        NSLayoutConstraint.activate([
            pathBar.leadingAnchor.constraint(equalTo: bottomBar.leadingAnchor, constant: 14),
            pathBar.centerYAnchor.constraint(equalTo: bottomBar.centerYAnchor),
            pathBar.trailingAnchor.constraint(lessThanOrEqualTo: inlineProgressIndicator.leadingAnchor, constant: -8),

            fdaHintButton.leadingAnchor.constraint(equalTo: bottomBar.leadingAnchor, constant: 14),
            fdaHintButton.centerYAnchor.constraint(equalTo: bottomBar.centerYAnchor),
            fdaHintButton.trailingAnchor.constraint(lessThanOrEqualTo: inlineProgressIndicator.leadingAnchor, constant: -8),

            inlineProgressIndicator.trailingAnchor.constraint(equalTo: statusLabel.leadingAnchor, constant: -4),
            inlineProgressIndicator.centerYAnchor.constraint(equalTo: bottomBar.centerYAnchor),
            inlineProgressIndicator.widthAnchor.constraint(equalToConstant: 16),
            inlineProgressIndicator.heightAnchor.constraint(equalToConstant: 16),

            statusLabel.trailingAnchor.constraint(equalTo: rebuildContainer.leadingAnchor, constant: -4),
            statusLabel.centerYAnchor.constraint(equalTo: bottomBar.centerYAnchor),

            rebuildContainer.trailingAnchor.constraint(equalTo: bottomBar.trailingAnchor, constant: -14),
            rebuildContainer.centerYAnchor.constraint(equalTo: bottomBar.centerYAnchor),
            rebuildContainer.widthAnchor.constraint(equalToConstant: 20),
            rebuildContainer.heightAnchor.constraint(equalToConstant: 20),

            statusFeedbackCapsule.trailingAnchor.constraint(equalTo: bottomBar.trailingAnchor, constant: -14),
            statusFeedbackCapsule.centerYAnchor.constraint(equalTo: bottomBar.centerYAnchor),
        ])

        updateBottomBarVisibility()
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        setupTableView()
        addTableColumns()
        setupContextMenu()

        // 语言切换经主分栏中介统一转发（SearchWindow → SearchMainSplitViewController），
        // 此处不再自行订阅，避免同一通知触发两次整表刷新
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(onTableDensityChanged(_:)),
            name: .tableDensityDidChange,
            object: nil
        )
    }

    private func setupTableView() {
        tableView.style = .fullWidth
        let initialFontSize = TableDensity.savedFontSize
        currentFontSize = initialFontSize
        fontCache = TableFontCache(fontSize: initialFontSize)
        currentIconSize = TableDensity.calculateIconSize(for: initialFontSize)
        tableView.rowHeight = TableDensity.calculateRowHeight(for: initialFontSize)
        tableView.intercellSpacing = NSSize(width: 0, height: 0)
        tableView.columnAutoresizingStyle = .uniformColumnAutoresizingStyle
        tableView.allowsColumnReordering = true
        tableView.allowsColumnResizing = true
        tableView.allowsMultipleSelection = true
        tableView.allowsColumnSelection = false
        tableView.autosaveTableColumns = true
        tableView.autosaveName = "SearchResultsTable"
        tableView.gridStyleMask = []
        tableView.selectionHighlightStyle = .regular
        tableView.usesAlternatingRowBackgroundColors = true
        tableView.setDraggingSourceOperationMask([.copy, .link], forLocal: false)
        tableView.dataSource = self
        tableView.delegate = self
        tableView.doubleAction = #selector(tableDoubleClicked)
        tableView.target = self
        tableView.onMoveUpAtTop = { [weak self] in
            self?.delegate?.searchResultsDidRequestFocusSearchField()
        }
    }

    private func addTableColumns() {
        let nameCol = NSTableColumn(identifier: .columnName)
        nameCol.title = L("search.col.name")
        nameCol.width = 280
        nameCol.minWidth = 140
        nameCol.sortDescriptorPrototype = NSSortDescriptor(
            key: "name",
            ascending: true,
            selector: #selector(NSString.localizedStandardCompare(_:))
        )
        tableView.addTableColumn(nameCol)

        let pathCol = NSTableColumn(identifier: .columnPath)
        pathCol.title = L("search.col.path")
        pathCol.width = 240
        pathCol.minWidth = 100
        pathCol.sortDescriptorPrototype = NSSortDescriptor(
            key: "path",
            ascending: true,
            selector: #selector(NSString.localizedStandardCompare(_:))
        )
        tableView.addTableColumn(pathCol)

        let modifiedCol = NSTableColumn(identifier: .columnModified)
        modifiedCol.title = L("search.col.dateModified")
        modifiedCol.width = 160
        modifiedCol.minWidth = TableDensity.calculateDateMinWidth(for: currentFontSize)
        modifiedCol.sortDescriptorPrototype = NSSortDescriptor(key: "modified", ascending: false)
        tableView.addTableColumn(modifiedCol)

        let createdCol = NSTableColumn(identifier: .columnCreated)
        createdCol.title = L("search.col.dateCreated")
        createdCol.width = 160
        createdCol.minWidth = TableDensity.calculateDateMinWidth(for: currentFontSize)
        createdCol.sortDescriptorPrototype = NSSortDescriptor(key: "created", ascending: false)
        tableView.addTableColumn(createdCol)

        let extensionCol = NSTableColumn(identifier: .columnExtension)
        extensionCol.title = L("search.col.extension")
        extensionCol.width = 80
        extensionCol.minWidth = 56
        extensionCol.sortDescriptorPrototype = NSSortDescriptor(key: "extension", ascending: true)
        tableView.addTableColumn(extensionCol)

        let sizeCol = NSTableColumn(identifier: .columnSize)
        sizeCol.title = L("search.col.size")
        sizeCol.width = 80
        sizeCol.minWidth = 50
        sizeCol.sortDescriptorPrototype = NSSortDescriptor(key: "size", ascending: false)
        tableView.addTableColumn(sizeCol)

        setupColumnHeaderMenu()
        applySavedColumnVisibility()
        updateColumnAlignments()
    }

    private func setupColumnHeaderMenu() {
        let headerView = tableView.headerView ?? NSTableHeaderView()
        if tableView.headerView == nil {
            tableView.headerView = headerView
        }
        savedHeaderView = headerView

        let menu = NSMenu()
        menu.autoenablesItems = false
        menu.delegate = columnHeaderMenuDelegate
        columnHeaderMenuDelegate.onNeedsUpdate = { [weak self] targetMenu in
            self?.populateColumnHeaderMenu(targetMenu)
        }
        headerView.menu = menu
    }

    private func populateColumnHeaderMenu(_ menu: NSMenu) {
        menu.removeAllItems()
        for def in Self.canonicalColumnDefinitions {
            let item = NSMenuItem(
                title: L(def.titleKey),
                action: #selector(toggleColumnVisibilityFromMenu(_:)),
                keyEquivalent: ""
            )
            item.target = self
            item.representedObject = def.id.rawValue
            let isHidden = tableView.tableColumn(withIdentifier: def.id)?.isHidden ?? false
            item.state = isHidden ? .off : .on
            // 名称列锁定不可隐藏，确保结果列表始终具备可辨识主列
            item.isEnabled = (def.id != .columnName)
            menu.addItem(item)
        }
        menu.addItem(NSMenuItem.separator())
        let resetItem = NSMenuItem(
            title: L("search.col.resetDefault"),
            action: #selector(resetColumnsMenuClicked),
            keyEquivalent: ""
        )
        resetItem.target = self
        menu.addItem(resetItem)
    }

    @objc private func toggleColumnVisibilityFromMenu(_ sender: NSMenuItem) {
        guard let rawId = sender.representedObject as? String else { return }
        let identifier = NSUserInterfaceItemIdentifier(rawId)
        guard let col = tableView.tableColumn(withIdentifier: identifier) else { return }
        setColumnHidden(!col.isHidden, for: identifier)
    }

    @objc private func resetColumnsMenuClicked() {
        resetColumnsToDefault()
    }

    private func loadHiddenColumnIdentifiers() -> Set<NSUserInterfaceItemIdentifier> {
        guard let stored = UserDefaults.standard.stringArray(forKey: Self.hiddenColumnsDefaultsKey) else {
            return Self.defaultHiddenColumnIdentifiers
        }
        var hidden = Set(stored.map { NSUserInterfaceItemIdentifier($0) })
        hidden.remove(.columnName)
        return hidden
    }

    private func persistHiddenColumnIdentifiers() {
        let hiddenRaw = tableView.tableColumns
            .filter { $0.isHidden && $0.identifier != .columnName }
            .map { $0.identifier.rawValue }
            .sorted()
        UserDefaults.standard.set(hiddenRaw, forKey: Self.hiddenColumnsDefaultsKey)
    }

    private func applySavedColumnVisibility() {
        let hidden = loadHiddenColumnIdentifiers()
        for col in tableView.tableColumns {
            col.isHidden = (col.identifier != .columnName) && hidden.contains(col.identifier)
        }
    }

    /// 切换指定结果列表列的可见性，并同步持久化、清理失效排序与刷新末列对齐
    func setColumnHidden(_ hidden: Bool, for identifier: NSUserInterfaceItemIdentifier) {
        guard identifier != .columnName,
              let col = tableView.tableColumn(withIdentifier: identifier) else { return }
        col.isHidden = hidden
        if hidden,
           let activeSortKey = tableView.sortDescriptors.first?.key,
           let colSortKey = col.sortDescriptorPrototype?.key,
           activeSortKey == colSortKey {
            tableView.sortDescriptors = []
        }
        persistHiddenColumnIdentifiers()
        updateColumnAlignments()
        tableView.reloadData()
    }

    /// 恢复结果列表默认列序、默认宽度与默认可见状态（显示名称/路径/修改日期/大小，隐藏创建日期/扩展名）
    func resetColumnsToDefault() {
        UserDefaults.standard.removeObject(forKey: Self.hiddenColumnsDefaultsKey)
        UserDefaults.standard.removeObject(forKey: "NSTableView Columns SearchResultsTable")
        for (targetIndex, def) in Self.canonicalColumnDefinitions.enumerated() {
            let currentIndex = tableView.column(withIdentifier: def.id)
            if currentIndex >= 0 && currentIndex != targetIndex {
                tableView.moveColumn(currentIndex, toColumn: targetIndex)
            }
            if let col = tableView.tableColumn(withIdentifier: def.id) {
                col.width = def.width
                col.isHidden = Self.defaultHiddenColumnIdentifiers.contains(def.id)
            }
        }
        if let activeSortKey = tableView.sortDescriptors.first?.key,
           let hiddenDef = Self.canonicalColumnDefinitions.first(where: { $0.sortKey == activeSortKey }),
           Self.defaultHiddenColumnIdentifiers.contains(hiddenDef.id) {
            tableView.sortDescriptors = []
        }
        updateColumnAlignments()
        tableView.reloadData()
    }

    /// 动态刷新列头与单元格文本对齐方式：
    /// 仅大小、修改日期与创建日期列在处于当前可见末列时采用右对齐以贴合视窗边界，处于非末列时统一左对齐保持纵向对齐线整洁；
    /// 名称、路径与扩展名列始终保持左对齐。
    public func updateColumnAlignments() {
        guard let lastVisibleCol = tableView.tableColumns.last(where: { !$0.isHidden }) else { return }
        for col in tableView.tableColumns {
            let isLastVisible = (col === lastVisibleCol)
            let alignment: NSTextAlignment
            switch col.identifier {
            case .columnSize, .columnModified, .columnCreated:
                alignment = isLastVisible ? .right : .left
            default:
                alignment = .left
            }
            if col.headerCell.alignment != alignment {
                col.headerCell.alignment = alignment
            }
        }
        tableView.headerView?.needsDisplay = true
    }

    @objc private func onTableDensityChanged(_ notification: Notification) {
        let newSize = notification.userInfo?["fontSize"] as? CGFloat ?? TableDensity.savedFontSize
        applyFontSize(newSize)
    }

    func applyFontSize(_ fontSize: CGFloat) {
        currentFontSize = fontSize
        fontCache = TableFontCache(fontSize: fontSize)
        currentIconSize = TableDensity.calculateIconSize(for: fontSize)
        tableView.rowHeight = TableDensity.calculateRowHeight(for: fontSize)

        let dateMinWidth = TableDensity.calculateDateMinWidth(for: fontSize)
        for id in [NSUserInterfaceItemIdentifier.columnModified, .columnCreated] {
            if let dateCol = tableView.tableColumns.first(where: { $0.identifier == id }) {
                dateCol.minWidth = dateMinWidth
            }
        }
        tableView.reloadData()
    }

    private func setupContextMenu() {
        let menu = NSMenu()
        menu.addLocalizedItem("search.ctx.open", action: #selector(openSelected), keyEquivalent: "\r")

        let openWithItem = NSMenuItem(title: L("search.ctx.openWith"), action: nil, keyEquivalent: "")
        let openWithSubmenu = NSMenu(title: L("search.ctx.openWith"))
        openWithSubmenu.delegate = openWithMenuDelegate
        openWithMenuDelegate.onNeedsUpdate = { [weak self] submenu in
            self?.populateOpenWithSubmenu(submenu)
        }
        openWithItem.submenu = openWithSubmenu
        menu.addItem(openWithItem)

        let qlItem = menu.addLocalizedItem("search.ctx.quickLook", action: #selector(quickLookSelected), keyEquivalent: " ")
        qlItem.keyEquivalentModifierMask = []
        let revealItem = menu.addLocalizedItem("search.ctx.revealInFinder", action: #selector(revealSelected), keyEquivalent: "r")
        revealItem.keyEquivalentModifierMask = [.command]
        menu.addLocalizedItem("search.ctx.filterByDirectory", action: #selector(filterByDirectorySelected), keyEquivalent: "")
        menu.addLocalizedItem("search.ctx.openInTerminal", action: #selector(openInTerminalSelected), keyEquivalent: "")
        menu.addLocalizedItem("search.ctx.getInfo", action: #selector(showInfoSelected), keyEquivalent: "i")
        menu.addItem(NSMenuItem.separator())
        menu.addLocalizedItem("search.ctx.copyFile", action: #selector(copyFile), keyEquivalent: "c")
        let copyPathItem = menu.addLocalizedItem("search.ctx.copyPath", action: #selector(copyPath), keyEquivalent: "C")
        copyPathItem.keyEquivalentModifierMask = [.command, .option]
        let copyNameItem = menu.addLocalizedItem("search.ctx.copyName", action: #selector(copyName), keyEquivalent: "C")
        copyNameItem.keyEquivalentModifierMask = [.command, .shift]
        menu.addItem(NSMenuItem.separator())
        menu.addLocalizedItem("search.ctx.excludeDirectory", action: #selector(excludeDirectorySelected), keyEquivalent: "")
        menu.addItem(NSMenuItem.separator())
        let trashItem = menu.addLocalizedItem(
            "search.ctx.moveToTrash",
            action: #selector(moveToTrashSelected),
            keyEquivalent: String(format: "%c", NSDeleteCharacter)
        )
        trashItem.keyEquivalentModifierMask = [.command]
        menu.allowsContextMenuPlugIns = true
        menu.delegate = tableMenuDelegate
        tableMenuDelegate.onMenuWillOpen = { [weak self] m in
            self?.updateOpenWithItemAvailability(in: m)
        }
        tableView.menu = menu
    }

    // MARK: - Public API

    /// 当前选中的首个条目
    var selectedResult: FfiSearchResult? {
        let row = tableView.selectedRow
        guard row >= 0, row < results.count else { return nil }
        return results[row]
    }

    /// 当前选中的全部条目集合
    var selectedResults: [FfiSearchResult] {
        let indices = tableView.selectedRowIndexes.filter { $0 >= 0 && $0 < results.count }
        return indices.map { results[$0] }
    }

    /// 右键作用域当前生效的目标 URL：面包屑临时租约优先，其次回落到主列表选区
    private var contextualURLs: [URL] {
        if let payload = activeContextMenuPayload, !payload.isEmpty { return payload }
        return selectedResults.map { URL(fileURLWithPath: $0.path) }
    }

    private static func normalizeOfflineCheckPath(_ path: String) -> String {
        let expanded = (path as NSString).expandingTildeInPath
        guard expanded.count > 1 else { return expanded }
        var trimmed = expanded
        while trimmed.count > 1 && trimmed.hasSuffix("/") {
            trimmed.removeLast()
        }
        return trimmed
    }

    private static func offlineVolumeMountRoot(for path: String) -> String? {
        let components = (path as NSString).pathComponents.filter { $0 != "/" }
        guard components.count >= 2,
              components[0].caseInsensitiveCompare("Volumes") == .orderedSame,
              !components[1].isEmpty else {
            return nil
        }
        return "/Volumes/" + components[1]
    }

    private static func isPath(_ target: String, insideOrEqual root: String) -> Bool {
        if target.caseInsensitiveCompare(root) == .orderedSame {
            return true
        }
        return target.lowercased().hasPrefix(root.lowercased() + "/")
    }

    /// Returns whether a search result belongs to an offline volume without querying the filesystem.
    func isResultOffline(_ result: FfiSearchResult) -> Bool {
        if result.isOffline { return true }
        return isPathOfflineHandler?(result.path) ?? false
    }

    /// Returns whether a given path belongs to an offline volume without querying disk.
    /// Root ("/") and bare "/Volumes" are never treated as offline due to descendant results.
    func isPathOffline(_ path: String) -> Bool {
        let normalized = Self.normalizeOfflineCheckPath(path)
        if normalized.isEmpty || normalized == "/" || normalized.caseInsensitiveCompare("/Volumes") == .orderedSame {
            return false
        }
        if let handler = isPathOfflineHandler {
            return handler(normalized)
        }
        return results.contains { item in
            guard item.isOffline else { return false }
            let itemPath = Self.normalizeOfflineCheckPath(item.path)
            if Self.isPath(normalized, insideOrEqual: itemPath) {
                return true
            }
            if let mountRoot = Self.offlineVolumeMountRoot(for: itemPath),
               Self.isPath(normalized, insideOrEqual: mountRoot) {
                return true
            }
            return false
        }
    }

    /// Returns true when `items` is non-empty and all selected items are online; otherwise shows status notice.
    private func requireAllOnline(_ items: [FfiSearchResult]) -> Bool {
        guard !items.isEmpty else { return false }
        if items.contains(where: { isResultOffline($0) }) {
            showBlockedActionFeedback()
            return false
        }
        return true
    }

    /// Returns true when every active contextual target (breadcrumb URL or table selection) is online.
    private var areAllContextualTargetsOnline: Bool {
        if let payload = activeContextMenuPayload, !payload.isEmpty {
            if pathBar.isOfflinePathMode { return false }
            return !payload.contains { isPathOffline($0.path) }
        }
        let selected = selectedResults
        guard !selected.isEmpty else { return false }
        return !selected.contains { isResultOffline($0) }
    }

    /// Re-render visible table cells and breadcrumb bar after volume status changes without clearing selection.
    func refreshOfflineStates() {
        let selected = tableView.selectedRowIndexes
        tableView.reloadData()
        if !selected.isEmpty {
            let valid = IndexSet(selected.filter { $0 >= 0 && $0 < results.count })
            if !valid.isEmpty {
                tableView.selectRowIndexes(valid, byExtendingSelection: false)
            }
        }
        hasRenderedBottomPath = false
        updateBottomPath(for: selectedResult)
        if QLPreviewPanel.sharedPreviewPanelExists() && QLPreviewPanel.shared().isVisible {
            QLPreviewPanel.shared().reloadData()
        }
    }

    /// 更新并渲染检索结果集合
    func displayResults(
        _ results: [FfiSearchResult],
        preserveSelection: Bool,
        autoSelectFirstRow: Bool = false
    ) {
        let previousPath = preserveSelection ? selectedResult?.path : nil
        self.results = results
        hasAppliedResults = true
        // 图标按类型稳定复用，跨检索保留；展示串缓存随结果集更替释放，避免无限增长
        dateTextCache.removeAll()
        sizeTextCache.removeAll()

        if !preserveSelection {
            tableView.sortDescriptors = []
        } else if let descriptor = tableView.sortDescriptors.first, let key = descriptor.key {
            sortResults(by: key, ascending: descriptor.ascending)
        }

        // 高频击键刷新直接整表重绘，规避每次按键触发 150ms 离屏图层淡入合成与视觉残影
        tableView.reloadData()

        if let previousPath,
           let matchIndex = self.results.firstIndex(where: { $0.path == previousPath }) {
            tableView.selectRowIndexes(IndexSet(integer: matchIndex), byExtendingSelection: false)
            tableView.scrollRowToVisible(matchIndex)
        } else if !preserveSelection && autoSelectFirstRow && !self.results.isEmpty {
            // 活跃检索自动预选首行，便于用户直接按 Return 打开最佳匹配项并实时预览底栏路径，不抢占搜索框焦点
            tableView.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
            tableView.scrollRowToVisible(0)
        } else {
            tableView.deselectAll(nil)
        }
        renderEmptyState()
        notifySelectionChanged()
        if QLPreviewPanel.sharedPreviewPanelExists() && QLPreviewPanel.shared().isVisible {
            QLPreviewPanel.shared().reloadData()
        }
    }

    /// 选中首行并滚动至可见视野
    func selectFirstRow() {
        guard !results.isEmpty else { return }
        tableView.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
        tableView.scrollRowToVisible(0)
        notifySelectionChanged()
    }

    /// 激活检索结果列表键盘焦点，并根据指定行号或已有选区联动可视区域与状态反馈
    @discardableResult
    func focusResults(row: Int? = nil) -> Bool {
        guard !results.isEmpty else { return false }
        let targetRow: Int
        if let row = row, row >= 0 && row < results.count {
            targetRow = row
        } else if tableView.selectedRow >= 0 && tableView.selectedRow < results.count {
            targetRow = tableView.selectedRow
        } else {
            targetRow = 0
        }
        tableView.selectRowIndexes(IndexSet(integer: targetRow), byExtendingSelection: false)
        tableView.scrollRowToVisible(targetRow)
        notifySelectionChanged()
        view.window?.makeFirstResponder(tableView)
        return true
    }

    // MARK: - Bottom Bar Interface

    /// 直接依据已选中的检索条目 O(1) 更新底栏文件路径展示，消除结果集线性扫描
    func updateBottomPath(for result: FfiSearchResult?) {
        guard let result else {
            applyBottomPath(path: nil, isDir: false, fileExtension: nil, isOffline: false)
            return
        }
        applyBottomPath(
            path: result.path,
            isDir: result.isDir,
            fileExtension: result.extension,
            isOffline: isResultOffline(result)
        )
    }

    /// 更新底栏文件路径展示并协同控制与权限提示的互斥显隐
    func updateBottomPath(with path: String?) {
        guard let path else {
            applyBottomPath(path: nil, isDir: false, fileExtension: nil, isOffline: false)
            return
        }
        let matched = results.first(where: { $0.path == path })
        let isTargetOffline = matched.map { isResultOffline($0) } ?? isPathOffline(path)
        let isTargetDir = matched?.isDir ?? false
        applyBottomPath(
            path: path,
            isDir: isTargetDir,
            fileExtension: matched?.extension,
            isOffline: isTargetOffline
        )
    }

    private func applyBottomPath(path: String?, isDir: Bool, fileExtension: String?, isOffline: Bool) {
        if hasRenderedBottomPath,
           lastRenderedBottomPath == path,
           lastRenderedBottomOffline == isOffline {
            updateBottomBarVisibility()
            return
        }
        hasRenderedBottomPath = true
        lastRenderedBottomPath = path
        lastRenderedBottomOffline = isOffline

        if let path, isOffline {
            // 离线面包屑纯内存构建，绝不向 NSPathControl 写入 url 触发挂死挂载点 stat；
            // 图标使用系统 UTType 全彩图标并配合柔和降透明态，视觉与原生访达路径栏保持统一。
            applyOfflinePathItems(for: path, isDir: isDir, fileExtension: fileExtension)
            pathBar.alphaValue = 0.68
        } else {
            pathBar.clearOfflineState()
            if path == nil {
                pathBar.pathItems = []
            }
            pathBar.url = path.map { URL(fileURLWithPath: $0) }
            pathBar.alphaValue = 1.0
        }
        pathBar.toolTip = path
        updateBottomBarVisibility()
    }

    private static let bootVolumeDisplayName: String = {
        let name = FileManager.default.displayName(atPath: "/")
        return name.isEmpty ? "/" : name
    }()

    private func applyOfflinePathItems(for path: String, isDir isTargetDir: Bool, fileExtension: String?) {
        let folderIcon = nonBlockingTypeIcon(for: "", isDir: true)
        let volumeIcon = nonBlockingVolumeIcon()
        let components = (path as NSString).pathComponents.filter { $0 != "/" }
        var items: [NSPathControlItem] = []
        var urls: [URL] = []

        // `/Volumes/<卷名>/...` 与 macOS 访达路径栏保持一致：直接以卷名与磁盘图标起头，省去生硬的 `/ > Volumes >` 前缀
        let startIndex: Int
        var current: String
        if components.count >= 2 && components[0].caseInsensitiveCompare("Volumes") == .orderedSame {
            let volumeName = components[1]
            let volumePath = "/\(components[0])/\(volumeName)"
            let volumeItem = NSPathControlItem()
            volumeItem.title = volumeName
            volumeItem.image = volumeIcon
            items.append(volumeItem)
            urls.append(URL(fileURLWithPath: volumePath, isDirectory: true))
            startIndex = 2
            current = volumePath
        } else {
            let rootItem = NSPathControlItem()
            rootItem.title = Self.bootVolumeDisplayName
            rootItem.image = volumeIcon
            items.append(rootItem)
            urls.append(URL(fileURLWithPath: "/", isDirectory: true))
            startIndex = 0
            current = ""
        }

        if startIndex < components.count {
            for idx in startIndex..<components.count {
                let component = components[idx]
                current += "/" + component
                let isLast = idx == components.count - 1
                let itemIsDir = isLast ? isTargetDir : true
                let item = NSPathControlItem()
                item.title = component
                if itemIsDir {
                    item.image = (isLast && Self.needsCustomBundleIcon(path: current))
                        ? nonBlockingTypeIcon(for: current, isDir: true, fileExtension: fileExtension)
                        : folderIcon
                } else {
                    item.image = nonBlockingTypeIcon(
                        for: current,
                        isDir: false,
                        fileExtension: isLast ? fileExtension : nil
                    )
                }
                items.append(item)
                urls.append(URL(fileURLWithPath: current, isDirectory: itemIsDir))
            }
        }

        pathBar.setOfflineItems(items, urls: urls)
    }

    /// 更新当前完全磁盘访问权限状态并刷新底栏互斥展示
    func updateFDAStatus(hasFDA: Bool) {
        guard self.hasFullDiskAccess != hasFDA else { return }
        self.hasFullDiskAccess = hasFDA
        updateBottomBarVisibility()
    }

    /// 根据当前路径选中状态与完全磁盘访问权限协同控制显隐让位
    private func updateBottomBarVisibility() {
        // 路径展示优先级高于权限提示：有选中路径时始终让位给路径
        let hasBreadcrumb = pathBar.url != nil || !pathBar.pathItems.isEmpty
        let showFDAHint = !hasBreadcrumb && !hasFullDiskAccess
        pathBar.isHidden = showFDAHint
        fdaHintButton.isHidden = !showFDAHint
    }

    /// 更新底栏状态统计文本（权威持久状态驱动，若正在展示临时状态则仅静默暂存最新进度）
    func updateStatusText(_ text: String) {
        persistedStatusText = text
        if !isShowingTemporaryStatus {
            statusLabel.stringValue = text
            statusLabel.textColor = .secondaryLabelColor
        }
    }

    /// 取消临时状态展示并立即恢复最新持久状态
    func cancelTemporaryStatus() {
        temporaryStatusWorkItem?.cancel()
        temporaryStatusWorkItem = nil
        isShowingTemporaryStatus = false
        statusFeedbackCapsule.stopSpin()
        statusFeedbackCapsule.isHidden = true
        statusFeedbackCapsule.alphaValue = 0.0
        statusLabel.alphaValue = 1.0
        rebuildContainer.alphaValue = 1.0
        statusLabel.stringValue = persistedStatusText
        statusLabel.textColor = .secondaryLabelColor
    }

    /// Display a brief non-blocking notice in the status bar when an offline or disallowed
    /// filesystem operation is blocked, then automatically restore persisted status.
    func showBlockedActionFeedback(_ message: String? = nil) {
        let displayMessage = message ?? L("search.offline.actionBlocked")
        isShowingTemporaryStatus = true
        statusFeedbackCapsule.stopSpin()
        statusFeedbackCapsule.isHidden = true
        statusFeedbackCapsule.alphaValue = 0.0
        statusLabel.alphaValue = 1.0
        rebuildContainer.alphaValue = 1.0
        statusLabel.stringValue = displayMessage
        statusLabel.textColor = .systemOrange

        temporaryStatusWorkItem?.cancel()
        let workItem = DispatchWorkItem { [weak self] in
            self?.cancelTemporaryStatus()
        }
        temporaryStatusWorkItem = workItem
        DispatchQueue.main.asyncAfter(deadline: .now() + 2.5, execute: workItem)
    }

    /// 当后台正在增量索引或扫描时，点击刷新按钮触发的状态栏微反馈
    func notifyBackgroundIndexingActive() {
        // 1. 同步底层无障碍与持久语义状态
        statusLabel.stringValue = L("search.backgroundRefresh")
        statusLabel.textColor = .controlAccentColor

        // 2. 配置轻量微气泡与持续旋转微动效
        isShowingTemporaryStatus = true
        statusFeedbackCapsule.label.stringValue = L("search.backgroundRefresh")
        statusFeedbackCapsule.isHidden = false
        statusFeedbackCapsule.startSpin()
        statusFeedbackCapsule.pulse()

        NSAnimationContext.runAnimationGroup { ctx in
            ctx.duration = 0.18
            ctx.timingFunction = CAMediaTimingFunction(name: .easeOut)
            statusLabel.animator().alphaValue = 0.0
            rebuildContainer.animator().alphaValue = 0.0
            statusFeedbackCapsule.animator().alphaValue = 1.0
        }

        // 3. 取消前序计时器并重新调度 2.0 秒平滑恢复
        temporaryStatusWorkItem?.cancel()
        let workItem = DispatchWorkItem { [weak self] in
            guard let self = self else { return }
            self.isShowingTemporaryStatus = false
            NSAnimationContext.runAnimationGroup({ ctx in
                ctx.duration = 0.22
                ctx.timingFunction = CAMediaTimingFunction(name: .easeInEaseOut)
                self.statusFeedbackCapsule.animator().alphaValue = 0.0
                self.statusLabel.animator().alphaValue = 1.0
                self.rebuildContainer.animator().alphaValue = 1.0
            }, completionHandler: {
                self.statusFeedbackCapsule.isHidden = true
                self.statusFeedbackCapsule.stopSpin()
            })
            self.statusLabel.stringValue = self.persistedStatusText
            self.statusLabel.textColor = .secondaryLabelColor
            self.temporaryStatusWorkItem = nil
        }
        temporaryStatusWorkItem = workItem
        DispatchQueue.main.asyncAfter(deadline: .now() + 2.0, execute: workItem)
    }

    /// 控制底栏内联进度指示器显隐与动画
    func setInlineProgressVisible(_ visible: Bool) {
        inlineProgressIndicator.isHidden = !visible
        if visible {
            inlineProgressIndicator.startAnimation(nil)
        } else {
            inlineProgressIndicator.stopAnimation(nil)
        }
    }

    /// 控制索引重建按钮交互可用性
    func setRebuildEnabled(_ enabled: Bool) {
        rebuildButton.isEnabled = enabled
    }

    /// 控制索引重建按钮旋转微动效
    func setRebuildSpinning(_ spinning: Bool) {
        if spinning {
            cancelTemporaryStatus()
            startRebuildButtonAnimation()
        } else {
            stopRebuildButtonAnimation()
        }
    }

    // MARK: - Notifications

    @objc func onLanguageChanged() {
        for def in Self.canonicalColumnDefinitions {
            tableView.tableColumn(withIdentifier: def.id)?.title = L(def.titleKey)
        }
        tableView.headerView?.needsDisplay = true
        tableView.menu?.refreshLocalizedTitles()
        // 格式化结果与应用本地化显示名随语言环境变化，缓存需整体失效
        dateTextCache.removeAll()
        sizeTextCache.removeAll()
        openWithAppNameCache.removeAll()
        tableView.reloadData()
        rebuildButton.toolTip = L("search.rebuildIndex.tooltip")
        rebuildButton.setAccessibilityLabel(L("search.rebuildIndex"))
        if isShowingTemporaryStatus {
            statusLabel.stringValue = L("search.backgroundRefresh")
            statusFeedbackCapsule.label.stringValue = L("search.backgroundRefresh")
        }
        renderEmptyState()
        updateFDAButtonContent()
    }

    // MARK: - Bottom Bar Actions & Animation

    @objc private func fdaHintButtonClicked() {
        onOpenFDAGuide?()
    }

    private func updateFDAButtonContent() {
        let font = NSFont.systemFont(ofSize: 11.5, weight: .regular)
        let config = NSImage.SymbolConfiguration(pointSize: 11, weight: .medium)
            .applying(.init(paletteColors: [.systemOrange]))
        let image = NSImage(systemSymbolName: "exclamationmark.triangle.fill", accessibilityDescription: nil)?
            .withSymbolConfiguration(config)

        fdaHintButton.image = image
        fdaHintButton.imagePosition = .imageLeading
        fdaHintButton.imageHugsTitle = true

        let paragraphStyle = NSMutableParagraphStyle()
        paragraphStyle.lineBreakMode = .byTruncatingTail

        let attributedTitle = NSAttributedString(
            string: " " + L("fda.restrictedHint"),
            attributes: [
                .font: font,
                .foregroundColor: NSColor.secondaryLabelColor,
                .paragraphStyle: paragraphStyle,
            ]
        )
        fdaHintButton.attributedTitle = attributedTitle
    }

    @objc private func pathBarClicked() {
        guard !pathBar.isOfflinePathMode else {
            showBlockedActionFeedback()
            return
        }
        if let url = pathBar.clickedPathItem?.url ?? pathBar.url {
            guard !isPathOffline(url.path) else {
                showBlockedActionFeedback()
                return
            }
            delegate?.searchResultsDidConsumeResult()
            revealFilesHandler([url])
        }
    }

    @objc private func rebuildButtonClicked() {
        delegate?.searchResultsDidRequestRebuild()
    }

    private func startRebuildButtonAnimation() {
        rebuildButton.layer?.addInfiniteSpin()
    }

    private func stopRebuildButtonAnimation() {
        guard let layer = rebuildButton.layer else { return }
        layer.removeAnimation(forKey: "spin")
        layer.removeAnimation(forKey: "stopSpin")
        layer.transform = CATransform3DIdentity
        rebuildButton.contentTintColor = .secondaryLabelColor
    }

    // MARK: - Private Selection & Actions

    /// 整表刷新并叠加淡入过渡，规避结果集整体跳变
    private func reloadWithFade() {
        let transition = CATransition()
        transition.type = .fade
        transition.duration = 0.15
        transition.timingFunction = CAMediaTimingFunction(name: .easeOut)
        tableView.layer?.add(transition, forKey: "fade")
        tableView.reloadData()
    }

    private func notifySelectionChanged() {
        let current = selectedResult
        let currentPath = current?.path
        let currentOffline = current.map { isResultOffline($0) }
        if currentPath != lastReportedPath || currentOffline != lastReportedOffline {
            lastReportedPath = currentPath
            lastReportedOffline = currentOffline
            delegate?.searchResultsDidSelect(result: current)
        }
    }

    @objc private func tableDoubleClicked() {
        let row = tableView.clickedRow
        guard row >= 0, row < results.count else { return }
        let result = results[row]
        delegate?.searchResultsDidDoubleClick(result: result)
    }

    @objc func copyFile(_ sender: Any? = nil) {
        let selected = selectedResults
        guard requireAllOnline(selected) else { return }
        delegate?.searchResultsDidConsumeResult()
        let urls = selected.map { NSURL(fileURLWithPath: $0.path) }
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.writeObjects(urls)
        let joinedPaths = selected.map(\.path).joined(separator: "\n")
        pasteboard.setString(joinedPaths, forType: .string)
    }

    @objc func quickLookSelected(_ sender: Any? = nil) {
        let selected = selectedResults
        guard requireAllOnline(selected) else { return }
        delegate?.searchResultsDidConsumeResult()
        let panel = QLPreviewPanel.shared()
        // 先显示，让面板经响应链接管 dataSource（显示时同步挂载 controller），再刷新
        panel?.makeKeyAndOrderFront(nil)
        panel?.reloadData()
    }

    @objc func showInfoSelected(_ sender: Any? = nil) {
        let selected = selectedResults
        guard requireAllOnline(selected) else { return }
        openFinderInfoWindow(for: selected.map(\.path))
    }

    private func openFinderInfoWindow(for paths: [String]) {
        guard !paths.contains(where: { isPathOffline($0) }) else {
            showBlockedActionFeedback()
            return
        }
        let candidatePaths = Array(paths.prefix(10))
        guard !candidatePaths.isEmpty else { return }
        let executor = appleScriptExecutor
        delegate?.searchResultsDidConsumeResult()

        // 将 fileExists 过滤也放入后台队列，避免失联网络挂载点在点击“显示简介”时阻塞主线程
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let targets = candidatePaths.filter { FileManager.default.fileExists(atPath: $0) }
            guard !targets.isEmpty else {
                DispatchQueue.main.async {
                    self?.showBlockedActionFeedback(L("search.getInfo.notFound"))
                }
                return
            }
            let statements = targets.map { path -> String in
                let escaped = path
                    .replacingOccurrences(of: "\\", with: "\\\\")
                    .replacingOccurrences(of: "\"", with: "\\\"")
                return "open information window of (POSIX file \"\(escaped)\" as alias)"
            }.joined(separator: "\n            ")

            let scriptSource = """
            tell application "Finder"
                \(statements)
                activate
            end tell
            """

            let outcome = executor(scriptSource)
            guard !outcome.succeeded else { return }

            if outcome.errorNumber == -128 {
                // User dismissed or cancelled the system authorization / AppleScript prompt.
                return
            }

            if outcome.errorNumber == -1743 {
                NSLog("[Lokii] Finder Automation permission denied (-1743)")
                DispatchQueue.main.async {
                    self?.showBlockedActionFeedback(L("search.getInfo.automationDenied.title"))
                    self?.automationPermissionDeniedHandler()
                }
                return
            }

            let rawErrorMessage = outcome.errorMessage
            NSLog("[Lokii] Finder Get Info AppleScript failed (%@): %@",
                  outcome.errorNumber.map(String.init) ?? "unknown",
                  rawErrorMessage ?? "unknown error")
            DispatchQueue.main.async {
                let trimmed = rawErrorMessage?.trimmingCharacters(in: .whitespacesAndNewlines)
                let displayMessage = (trimmed?.isEmpty == false) ? trimmed! : L("search.getInfo.failed")
                self?.showBlockedActionFeedback(displayMessage)
            }
        }
    }

    /// 以纯文本写入系统剪贴板，供路径与名称复制动作共用
    private func writeStringToPasteboard(_ string: String) {
        delegate?.searchResultsDidConsumeResult()
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(string, forType: .string)
    }

    @objc func copyPath(_ sender: Any? = nil) {
        let selected = selectedResults
        guard !selected.isEmpty else { return }
        writeStringToPasteboard(selected.map(\.path).joined(separator: "\n"))
    }

    @objc func copyName(_ sender: Any? = nil) {
        let selected = selectedResults
        guard !selected.isEmpty else { return }
        writeStringToPasteboard(selected.map(\.name).joined(separator: "\n"))
    }

    @objc func openSelected(_ sender: Any? = nil) {
        let selected = selectedResults
        guard requireAllOnline(selected) else { return }
        for item in selected {
            delegate?.searchResultsDidDoubleClick(result: item)
        }
    }

    @objc func revealSelected(_ sender: Any? = nil) {
        let selected = selectedResults
        guard requireAllOnline(selected) else { return }
        delegate?.searchResultsDidConsumeResult()
        // 统一调用 activateFileViewerSelecting 接口，无论单选或多选、文件或文件夹，
        // 均在父级视窗中精准定位并高亮目标项，规避 selectFile(nil, ...) 导致的直接打开文档或穿透进入文件夹缺陷
        let urls = selected.map { URL(fileURLWithPath: $0.path) }
        revealFilesHandler(urls)
    }

    @objc func openInTerminalSelected(_ sender: Any? = nil) {
        let selected = selectedResults
        guard requireAllOnline(selected) else { return }
        delegate?.searchResultsDidConsumeResult()
        let urls = selected.map { URL(fileURLWithPath: $0.path) }
        openInTerminalHandler(urls)
    }

    /// 主右键菜单展开时仅执行纯内存可用性判定，不阻塞查询 LaunchServices
    private func updateOpenWithItemAvailability(in menu: NSMenu) {
        guard let openWithItem = menu.items.first(where: {
            $0.submenu?.title == L("search.ctx.openWith") || $0.title == L("search.ctx.openWith")
        }) else { return }
        openWithItem.isEnabled = areAllContextualTargetsOnline && !contextualURLs.isEmpty
    }

    /// 仅在用户真正悬停展开“打开方式”子菜单时按需填充候选应用，并对显示名与 16pt 图标做内存复用
    private func populateOpenWithSubmenu(_ submenu: NSMenu) {
        submenu.removeAllItems()

        // Skip NSWorkspace queries entirely when target paths are offline so Finder/LaunchServices never block.
        guard areAllContextualTargetsOnline, let firstURL = contextualURLs.first else {
            return
        }

        let appURLs = NSWorkspace.shared.urlsForApplications(toOpen: firstURL)
        let defaultAppURL = NSWorkspace.shared.urlForApplication(toOpen: firstURL)

        for appURL in appURLs {
            let appPath = appURL.path
            let appName: String
            if let cachedName = openWithAppNameCache[appPath] {
                appName = cachedName
            } else {
                let resolved = FileManager.default.displayName(atPath: appPath)
                openWithAppNameCache[appPath] = resolved
                appName = resolved
            }

            let item = NSMenuItem(title: appName, action: #selector(openWithApp(_:)), keyEquivalent: "")
            item.target = self
            item.representedObject = appURL

            let icon: NSImage
            if let cachedIcon = openWithAppIconCache[appPath] {
                icon = cachedIcon
            } else {
                let loaded = NSWorkspace.shared.icon(forFile: appPath)
                loaded.size = NSSize(width: 16, height: 16)
                openWithAppIconCache[appPath] = loaded
                icon = loaded
            }
            item.image = icon
            if let defaultURL = defaultAppURL, defaultURL.path == appPath {
                item.state = .on
            }
            submenu.addItem(item)
        }

        if !appURLs.isEmpty {
            submenu.addItem(NSMenuItem.separator())
        }

        let otherItem = NSMenuItem(title: L("search.ctx.openWith.other"), action: #selector(chooseOtherAppToOpen(_:)), keyEquivalent: "")
        otherItem.target = self
        submenu.addItem(otherItem)
    }

    @objc func openWithApp(_ sender: NSMenuItem) {
        guard let appURL = sender.representedObject as? URL else { return }
        let urls = contextualURLs
        guard !urls.isEmpty else { return }
        guard areAllContextualTargetsOnline else {
            showBlockedActionFeedback()
            return
        }
        delegate?.searchResultsDidConsumeResult()
        let config = NSWorkspace.OpenConfiguration()
        config.activates = true
        NSWorkspace.shared.open(urls, withApplicationAt: appURL, configuration: config, completionHandler: nil)
    }

    @objc func chooseOtherAppToOpen(_ sender: Any? = nil) {
        let urls = contextualURLs
        guard !urls.isEmpty else { return }
        guard areAllContextualTargetsOnline else {
            showBlockedActionFeedback()
            return
        }
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.directoryURL = URL(fileURLWithPath: "/Applications")
        panel.allowedContentTypes = [UTType.application, UTType.applicationBundle]
        panel.begin { [weak self] response in
            guard response == .OK, let appURL = panel.url else { return }
            self?.delegate?.searchResultsDidConsumeResult()
            let config = NSWorkspace.OpenConfiguration()
            config.activates = true
            NSWorkspace.shared.open(urls, withApplicationAt: appURL, configuration: config, completionHandler: nil)
        }
    }

    @objc func filterByDirectorySelected(_ sender: Any? = nil) {
        guard selectedResults.count == 1,
              let item = selectedResults.first,
              let targetDir = scopeFilterDirectory(for: item) else {
            return
        }
        delegate?.searchResultsDidConsumeResult()
        delegate?.searchResultsDidRequestScopeFilter(path: targetDir)
    }

    private func scopeFilterDirectory(for result: FfiSearchResult) -> String? {
        var path = result.path
        while path.count > 1 && path.hasSuffix("/") {
            path.removeLast()
        }
        guard !path.isEmpty else { return nil }
        if result.isDir {
            return path
        }
        guard path != "/" else { return nil }
        let parent = (path as NSString).deletingLastPathComponent
        return parent.isEmpty ? nil : parent
    }

    /// 判定路径是否位于 `/Volumes/<entry>` 挂载卷命名空间内，供主线程免磁盘探测守卫使用
    private static func isUnderVolumesNamespace(_ path: String) -> Bool {
        let lower = path.lowercased()
        guard lower.hasPrefix("/volumes/") else { return false }
        let remainder = lower.dropFirst("/volumes/".count)
        return !remainder.isEmpty && !remainder.allSatisfy { $0 == "/" }
    }

    /// Resolve target directory vs parent folder without probing disconnected or mounted `/Volumes/*` paths on disk.
    private func resolveExcludedDirectoryURL(for url: URL) -> URL {
        if let matched = results.first(where: { $0.path == url.path }) {
            return matched.isDir ? url : url.deletingLastPathComponent()
        }
        if isPathOffline(url.path) || !areAllContextualTargetsOnline || pathBar.isOfflinePathMode || Self.isUnderVolumesNamespace(url.path) {
            return url.hasDirectoryPath ? url : url.deletingLastPathComponent()
        }
        return TerminalLauncher.directoryURL(for: url)
    }

    @objc func excludeDirectorySelected(_ sender: Any? = nil) {
        guard let url = contextualURLs.first else { return }
        let dirURL = resolveExcludedDirectoryURL(for: url)
        onExcludeDirectory?(dirURL.path)
    }

    @objc func moveToTrashSelected(_ sender: Any? = nil) {
        let selected = selectedResults
        guard requireAllOnline(selected) else { return }

        confirmTrashIfNeeded(count: selected.count) { [weak self] confirmed in
            guard let self = self, confirmed else { return }

            let handler = self.trashItemHandler
            // 调度至后台用户交互队列执行同步阻塞式文件系统 I/O，规避大目录或外接磁盘下的主线程掉帧
            DispatchQueue.global(qos: .userInitiated).async { [weak self] in
                var trashedPaths = Set<String>()
                var errors: [(String, Error)] = []

                for item in selected {
                    let url = URL(fileURLWithPath: item.path)
                    do {
                        _ = try handler(url)
                        trashedPaths.insert(item.path)
                    } catch {
                        let nsError = error as NSError
                        if nsError.domain == NSCocoaErrorDomain && nsError.code == NSFileNoSuchFileError {
                            // 边界守卫与自愈容错：若目标文件在物理文件系统上已不存在（如已在终端被清理），
                            // 表明该项目在源路径消除的业务诉求已达成，直接顺势将其作为已移除项从视图与结果集中剔除，
                            // 规避误报弹窗打扰用户
                            trashedPaths.insert(item.path)
                        } else {
                            errors.append((item.name, error))
                        }
                    }
                }

                DispatchQueue.main.async { [weak self] in
                    guard let self = self else { return }
                    if !trashedPaths.isEmpty {
                        self.applyTrashedItems(trashedPaths)
                        self.playTrashSound()
                    }
                    if !errors.isEmpty {
                        self.showTrashErrors(errors)
                    }
                }
            }
        }
    }

    /// 批量删除安全阈值确认：超过 5 项时弹出原生警示表单，防止误触全选造成破坏性删除
    private func confirmTrashIfNeeded(count: Int, completion: @escaping (Bool) -> Void) {
        if let customHandler = confirmTrashHandler {
            customHandler(count, completion)
            return
        }
        guard count > 5, let window = view.window else {
            completion(true)
            return
        }
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = String(format: L("search.trash.confirmTitle"), count)
        alert.informativeText = L("search.trash.confirmMessage")
        alert.addButton(withTitle: L("search.trash.confirmButton"))
        alert.addButton(withTitle: L("search.trash.cancelButton"))
        alert.beginSheetModal(for: window) { response in
            completion(response == .alertFirstButtonReturn)
        }
    }

    /// 乐观更新：在主列表内使用原生行滑动与淡出动画即时剔除已移入废纸篓的项目，并自动维护选区顺延
    private func applyTrashedItems(_ trashedPaths: Set<String>) {
        let selectedRow = tableView.selectedRow
        let indexSet = NSMutableIndexSet()
        for (idx, item) in results.enumerated() {
            if trashedPaths.contains(item.path) {
                indexSet.add(idx)
            }
        }

        results.removeAll { trashedPaths.contains($0.path) }

        if indexSet.count > 0 {
            tableView.beginUpdates()
            tableView.removeRows(at: indexSet as IndexSet, withAnimation: [.slideUp, .effectFade])
            tableView.endUpdates()
        }

        if !results.isEmpty {
            let nextRow = min(max(0, selectedRow), results.count - 1)
            tableView.selectRowIndexes(IndexSet(integer: nextRow), byExtendingSelection: false)
            tableView.scrollRowToVisible(nextRow)
        } else {
            tableView.deselectAll(nil)
        }

        notifySelectionChanged()
        delegate?.searchResultsDidTrashItems(paths: trashedPaths)
    }

    private func showTrashErrors(_ errors: [(String, Error)]) {
        guard let window = view.window else { return }
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = L("search.trash.failedTitle")
        let detail = errors.map { "\($0.0): \($0.1.localizedDescription)" }.joined(separator: "\n")
        alert.informativeText = String(format: L("search.trash.failedMessage"), errors.count, detail)
        alert.addButton(withTitle: L("ok"))
        alert.beginSheetModal(for: window, completionHandler: nil)
    }

    private static let trashSound: NSSound? = {
        let soundPath = "/System/Library/Components/CoreAudio.component/Contents/SharedSupport/SystemSounds/dock/drag to trash.aif"
        guard FileManager.default.fileExists(atPath: soundPath) else { return nil }
        return NSSound(contentsOfFile: soundPath, byReference: true)
    }()

    private func playTrashSound() {
        Self.trashSound?.stop()
        Self.trashSound?.play()
    }

    private func sortResults(by key: String, ascending: Bool) {
        results.sort { a, b in
            let cmp: ComparisonResult
            switch key {
            case "name":
                cmp = a.name.localizedStandardCompare(b.name)
            case "path":
                let pathA = (a.path as NSString).deletingLastPathComponent
                let pathB = (b.path as NSString).deletingLastPathComponent
                cmp = pathA.localizedStandardCompare(pathB)
            case "modified":
                if a.modified < b.modified { cmp = .orderedAscending }
                else if a.modified > b.modified { cmp = .orderedDescending }
                else { cmp = .orderedSame }
            case "created":
                if a.created < b.created { cmp = .orderedAscending }
                else if a.created > b.created { cmp = .orderedDescending }
                else { cmp = .orderedSame }
            case "extension":
                let extA = a.isDir ? "" : a.extension
                let extB = b.isDir ? "" : b.extension
                // 无论升降序，有效扩展名始终排在文件夹或无扩展名文件之前，保证有意义后缀立即可见
                if extA.isEmpty != extB.isEmpty {
                    return !extA.isEmpty
                }
                if extA.isEmpty {
                    return a.name.localizedStandardCompare(b.name) == .orderedAscending
                }
                let extCmp = extA.localizedCaseInsensitiveCompare(extB)
                if extCmp == .orderedSame {
                    return a.name.localizedStandardCompare(b.name) == .orderedAscending
                }
                return ascending ? extCmp == .orderedAscending : extCmp == .orderedDescending
            case "size":
                if a.size < b.size { cmp = .orderedAscending }
                else if a.size > b.size { cmp = .orderedDescending }
                else { cmp = .orderedSame }
            default:
                cmp = .orderedSame
            }
            return ascending ? cmp == .orderedAscending : cmp == .orderedDescending
        }
    }

    private static func fileExtensionLowercased(of path: String) -> String {
        let lastSlash = path.lastIndex(of: "/")
        let nameStart = lastSlash.map { path.index(after: $0) } ?? path.startIndex
        guard nameStart < path.endIndex,
              let dot = path[nameStart...].lastIndex(of: "."),
              dot > nameStart else {
            return ""
        }
        let extStart = path.index(after: dot)
        guard extStart < path.endIndex else { return "" }
        return path[extStart...].lowercased()
    }

    /// 仅应用程序包与系统偏好面板具有独立自定义图标，普通文件与常规目录统一复用扩展名/类型纯内存图标
    private static func needsCustomBundleIcon(path: String) -> Bool {
        let ext = fileExtensionLowercased(of: path)
        return ext == "app" || ext == "prefpane"
    }

    private func nonBlockingVolumeIcon() -> NSImage {
        let key = "__type_icon_volume__"
        if let cached = typeIconCache[key] { return cached }
        let icon = NSWorkspace.shared.icon(for: .volume)
        typeIconCache[key] = icon
        return icon
    }

    private func nonBlockingTypeIcon(for path: String, isDir: Bool, fileExtension: String? = nil) -> NSImage {
        if isDir && !Self.needsCustomBundleIcon(path: path) {
            let key = "__type_icon_dir__"
            if let cached = typeIconCache[key] { return cached }
            let icon = NSWorkspace.shared.icon(for: .folder)
            typeIconCache[key] = icon
            return icon
        }
        let ext = (fileExtension?.isEmpty == false ? fileExtension!.lowercased() : Self.fileExtensionLowercased(of: path))
        let key = "__type_icon_ext_\(ext)"
        if let cached = typeIconCache[key] { return cached }
        let utType = ext.isEmpty ? UTType.data : (UTType(filenameExtension: ext) ?? .data)
        let icon = NSWorkspace.shared.icon(for: utType)
        typeIconCache[key] = icon
        return icon
    }

    func iconForFile(at path: String, isDir: Bool = false, isOffline: Bool = false, fileExtension: String? = nil) -> NSImage {
        // 离线条目与 `/Volumes/*` 绝不触发磁盘 stat 或自定义应用包磁盘图标查询，
        // 统一走基于扩展名/UTType 的 O(1) 纯内存全彩系统图标缓存，兼顾零阻塞与原生视觉一致性。
        if isOffline || isPathOffline(path) {
            return nonBlockingTypeIcon(for: path, isDir: isDir, fileExtension: fileExtension)
        }
        if !Self.isUnderVolumesNamespace(path),
           Self.needsCustomBundleIcon(path: path),
           let cached = iconCache.object(forKey: path as NSString) {
            return cached
        }
        return nonBlockingTypeIcon(for: path, isDir: isDir, fileExtension: fileExtension)
    }

    /// 异步加载应用包（.app / .prefPane）的专属自定义图标并回填仍展示该路径的单元格
    private func loadCustomBundleIconIfNeeded(for result: FfiSearchResult, isOffline: Bool, into cell: SearchNameTableCellView) {
        let path = result.path
        cell.representedPath = path
        guard !isOffline,
              !isPathOffline(path),
              !Self.isUnderVolumesNamespace(path),
              Self.needsCustomBundleIcon(path: path),
              iconCache.object(forKey: path as NSString) == nil else {
            return
        }
        let provider = workspaceIconProvider
        iconLoadQueue.async { [weak self, weak cell] in
            let loaded = provider(path)
            DispatchQueue.main.async {
                guard let self else { return }
                self.iconCache.setObject(loaded, forKey: path as NSString)
                if let cell, cell.representedPath == path {
                    cell.imageView?.image = loaded
                }
            }
        }
    }

    // MARK: - NSServicesRequests

    override func validRequestor(forSendType sendType: NSPasteboard.PasteboardType?, returnType: NSPasteboard.PasteboardType?) -> Any? {
        guard returnType == nil else {
            return super.validRequestor(forSendType: sendType, returnType: returnType)
        }
        guard let sendType = sendType else {
            return super.validRequestor(forSendType: sendType, returnType: returnType)
        }

        guard ServicesPasteboard.allTypes.contains(sendType) else {
            return super.validRequestor(forSendType: sendType, returnType: returnType)
        }

        guard !contextualURLs.isEmpty else {
            return super.validRequestor(forSendType: sendType, returnType: returnType)
        }

        // File-object and directory services require all targets to be online; never query disk for offline entries.
        if ServicesPasteboard.fileObjectTypes.contains(sendType) || ServicesPasteboard.directoryTypes.contains(sendType) {
            guard areAllContextualTargetsOnline else {
                return super.validRequestor(forSendType: sendType, returnType: returnType)
            }
        }

        // 若服务请求特定于目录容器类型，精准校验目标载荷是否包含实际目录实体
        if ServicesPasteboard.directoryTypes.contains(sendType) {
            let containsDirectory: Bool
            if let payload = activeContextMenuPayload, !payload.isEmpty {
                containsDirectory = payload.contains { url in
                    guard !isPathOffline(url.path) else { return false }
                    if let matched = results.first(where: { $0.path == url.path }) {
                        return matched.isDir
                    }
                    if Self.isUnderVolumesNamespace(url.path) {
                        return url.hasDirectoryPath || url.pathExtension.isEmpty
                    }
                    var isDir: ObjCBool = false
                    if FileManager.default.fileExists(atPath: url.path, isDirectory: &isDir) {
                        return isDir.boolValue
                    }
                    return url.hasDirectoryPath || url.pathExtension.isEmpty
                }
            } else {
                containsDirectory = selectedResults.contains { $0.isDir && !isResultOffline($0) }
            }
            guard containsDirectory else {
                return super.validRequestor(forSendType: sendType, returnType: returnType)
            }
        }

        return self
    }
}

// MARK: - NSServicesMenuRequestor

extension SearchResultsViewController: NSServicesMenuRequestor {
    @objc(writeSelectionToPasteboard:types:)
    func writeSelection(to pboard: NSPasteboard, types: [NSPasteboard.PasteboardType]) -> Bool {
        let targetURLs = contextualURLs
        guard !targetURLs.isEmpty else { return false }
        let allowFileObjects = areAllContextualTargetsOnline

        var didWrite = false
        pboard.clearContents()

        let paths = targetURLs.map(\.path)
        let pathString = paths.joined(separator: "\n")

        if allowFileObjects && types.contains(where: { ServicesPasteboard.fileObjectTypes.contains($0) }) {
            pboard.writeObjects(targetURLs as [NSURL])
            didWrite = true
        }

        if allowFileObjects && types.contains(ServicesPasteboard.filenamesType) {
            pboard.setPropertyList(paths, forType: ServicesPasteboard.filenamesType)
            didWrite = true
        }

        if types.contains(.string) {
            pboard.setString(pathString, forType: .string)
            didWrite = true
        }

        let plainTextType = NSPasteboard.PasteboardType("public.plain-text")
        if types.contains(plainTextType) {
            pboard.setString(pathString, forType: plainTextType)
            didWrite = true
        }

        if didWrite {
            delegate?.searchResultsDidConsumeResult()
        }
        return didWrite
    }
}

// MARK: - NSTableViewDataSource

extension SearchResultsViewController: NSTableViewDataSource {
    func numberOfRows(in tableView: NSTableView) -> Int {
        results.count
    }

    func tableView(_ tableView: NSTableView, pasteboardWriterForRow row: Int) -> NSPasteboardWriting? {
        guard row >= 0, row < results.count else { return nil }
        let result = results[row]
        if isResultOffline(result) {
            return nil
        }
        return NSURL(fileURLWithPath: result.path)
    }

    func tableView(_ tableView: NSTableView, draggingSession session: NSDraggingSession, willBeginAt screenPoint: NSPoint, forRowIndexes rowIndexes: IndexSet) {
        delegate?.searchResultsDidConsumeResult()
    }

    func tableView(_ tableView: NSTableView, sortDescriptorsDidChange oldDescriptors: [NSSortDescriptor]) {
        guard let descriptor = tableView.sortDescriptors.first,
              let key = descriptor.key else { return }

        sortResults(by: key, ascending: descriptor.ascending)

        tableView.deselectAll(nil)

        reloadWithFade()
        notifySelectionChanged()
    }
}

// MARK: - NSTableViewDelegate

extension SearchResultsViewController: NSTableViewDelegate {
    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard row < results.count, let column = tableColumn else { return nil }
        let result = results[row]
        let offline = isResultOffline(result)
        let colID = column.identifier
        let isLastVisibleColumn = tableView.tableColumns.last(where: { !$0.isHidden })?.identifier == colID

        switch colID {
        case .columnName:
            return nameCell(for: result, isOffline: offline, in: tableView)
        case .columnPath:
            return pathCell(for: result, isOffline: offline, in: tableView)
        case .columnModified:
            return textCell(
                string: dateText(for: TimeInterval(result.modified)),
                font: fontCache.regular,
                color: offline ? NSColor.labelColor.withAlphaComponent(0.55) : .labelColor,
                alignment: isLastVisibleColumn ? .right : .left,
                identifier: colID,
                in: tableView
            )
        case .columnCreated:
            let createdStr = result.created == 0 ? "--" : dateText(for: TimeInterval(result.created))
            return textCell(
                string: createdStr,
                font: fontCache.monospacedDigit,
                color: offline ? NSColor.labelColor.withAlphaComponent(0.55) : .labelColor,
                alignment: isLastVisibleColumn ? .right : .left,
                identifier: colID,
                in: tableView
            )
        case .columnExtension:
            let extStr = (result.isDir || result.extension.isEmpty) ? "--" : result.extension
            return textCell(
                string: extStr,
                font: fontCache.regular,
                color: offline ? NSColor.secondaryLabelColor.withAlphaComponent(0.55) : .secondaryLabelColor,
                alignment: .left,
                identifier: colID,
                in: tableView
            )
        case .columnSize:
            let str = result.isDir ? "--" : sizeText(for: result.size)
            return textCell(
                string: str,
                font: fontCache.monospacedDigit,
                color: offline ? NSColor.secondaryLabelColor.withAlphaComponent(0.55) : .secondaryLabelColor,
                alignment: isLastVisibleColumn ? .right : .left,
                identifier: colID,
                in: tableView
            )
        default:
            return nil
        }
    }

    /// 用户拖拽调整列顺序回调：动态计算末列并刷新表头及单元格对齐方式
    public func tableViewColumnDidMove(_ notification: Notification) {
        updateColumnAlignments()
        tableView.reloadData()
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        notifySelectionChanged()
        if QLPreviewPanel.sharedPreviewPanelExists() && QLPreviewPanel.shared().isVisible {
            QLPreviewPanel.shared().reloadData()
        }
    }

    /// 修改时间展示串；按原始时间戳命中缓存，避免滚动时逐行重复格式化
    private func dateText(for modified: TimeInterval) -> String {
        if let cached = dateTextCache[modified] { return cached }
        let text = Self.dateFormatter.string(from: Date(timeIntervalSince1970: modified))
        dateTextCache[modified] = text
        return text
    }

    /// 文件体积展示串；按原始字节数命中缓存，避免滚动时逐行重复格式化
    private func sizeText(for size: UInt64) -> String {
        if let cached = sizeTextCache[size] { return cached }
        let text = Self.sizeFormatter.string(fromByteCount: Int64(size))
        sizeTextCache[size] = text
        return text
    }

    private func nameCell(for result: FfiSearchResult, isOffline: Bool, in tableView: NSTableView) -> NSTableCellView {
        let id = NSUserInterfaceItemIdentifier("NameCell")
        if let reused = tableView.makeView(withIdentifier: id, owner: nil) as? SearchNameTableCellView {
            reused.imageView?.image = iconForFile(at: result.path, isDir: result.isDir, isOffline: isOffline, fileExtension: result.extension)
            reused.updateIconSize(currentIconSize)
            reused.applyFonts(regular: fontCache.regular, bold: fontCache.bold)
            reused.configure(name: result.name, matchPositions: result.matchPositions, isOffline: isOffline)
            loadCustomBundleIconIfNeeded(for: result, isOffline: isOffline, into: reused)
            return reused
        }

        let cell = SearchNameTableCellView()
        cell.identifier = id

        let imageView = NSImageView()
        imageView.translatesAutoresizingMaskIntoConstraints = false
        imageView.imageScaling = .scaleProportionallyUpOrDown
        imageView.image = iconForFile(at: result.path, isDir: result.isDir, isOffline: isOffline, fileExtension: result.extension)
        cell.addSubview(imageView)
        cell.imageView = imageView

        let textField = NSTextField(labelWithString: "")
        textField.translatesAutoresizingMaskIntoConstraints = false
        textField.font = fontCache.regular
        textField.lineBreakMode = .byTruncatingTail
        textField.maximumNumberOfLines = 1
        textField.cell?.wraps = false
        textField.cell?.isScrollable = false
        textField.isEditable = false
        textField.isBezeled = false
        textField.drawsBackground = false
        cell.addSubview(textField)
        cell.textField = textField

        let widthConstraint = imageView.widthAnchor.constraint(equalToConstant: currentIconSize)
        let heightConstraint = imageView.heightAnchor.constraint(equalToConstant: currentIconSize)

        NSLayoutConstraint.activate([
            imageView.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 4),
            imageView.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
            widthConstraint,
            heightConstraint,
            textField.leadingAnchor.constraint(equalTo: imageView.trailingAnchor, constant: 4),
            textField.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -2),
            textField.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
        ])

        cell.setIconConstraints(width: widthConstraint, height: heightConstraint)
        cell.applyFonts(regular: fontCache.regular, bold: fontCache.bold)
        cell.configure(name: result.name, matchPositions: result.matchPositions, isOffline: isOffline)
        loadCustomBundleIconIfNeeded(for: result, isOffline: isOffline, into: cell)

        return cell
    }

    private func pathCell(for result: FfiSearchResult, isOffline: Bool, in tableView: NSTableView) -> NSTableCellView {
        let id = NSUserInterfaceItemIdentifier("PathCell")
        if let reused = tableView.makeView(withIdentifier: id, owner: nil) as? SearchPathTableCellView {
            reused.applyFonts(regular: fontCache.regular, bold: fontCache.bold)
            reused.configure(fullPath: result.path, parentMatchPositions: result.parentMatchPositions, isOffline: isOffline)
            return reused
        }

        let cell = SearchPathTableCellView()
        cell.identifier = id

        let textField = NSTextField(labelWithString: "")
        textField.translatesAutoresizingMaskIntoConstraints = false
        textField.font = fontCache.regular
        textField.alignment = .left
        textField.lineBreakMode = .byTruncatingMiddle
        textField.maximumNumberOfLines = 1
        textField.cell?.wraps = false
        textField.cell?.isScrollable = false
        textField.isEditable = false
        textField.isBezeled = false
        textField.drawsBackground = false
        cell.addSubview(textField)
        cell.textField = textField

        NSLayoutConstraint.activate([
            textField.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 4),
            textField.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -4),
            textField.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
        ])

        cell.applyFonts(regular: fontCache.regular, bold: fontCache.bold)
        cell.configure(fullPath: result.path, parentMatchPositions: result.parentMatchPositions, isOffline: isOffline)
        return cell
    }

    private func textCell(
        string: String,
        font: NSFont,
        color: NSColor,
        alignment: NSTextAlignment,
        identifier: NSUserInterfaceItemIdentifier,
        in tableView: NSTableView
    ) -> NSView {
        let cellID = NSUserInterfaceItemIdentifier(identifier.rawValue + "Cell")
        if let reused = tableView.makeView(withIdentifier: cellID, owner: nil) as? NSTableCellView {
            reused.textField?.stringValue = string
            reused.textField?.font = font
            reused.textField?.textColor = color
            reused.textField?.alignment = alignment
            return reused
        }

        let cell = NSTableCellView()
        cell.identifier = cellID

        let textField = NSTextField(labelWithString: string)
        textField.translatesAutoresizingMaskIntoConstraints = false
        textField.font = font
        textField.textColor = color
        textField.alignment = alignment
        textField.lineBreakMode = .byTruncatingTail
        textField.isEditable = false
        textField.isBezeled = false
        textField.drawsBackground = false
        cell.addSubview(textField)
        cell.textField = textField

        NSLayoutConstraint.activate([
            textField.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 4),
            textField.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -4),
            textField.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
        ])

        return cell
    }
}

// MARK: - NSMenuItemValidation

extension SearchResultsViewController: NSMenuItemValidation {
    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        if menuItem.representedObject as? String == "search.ctx.services" || menuItem.hasSubmenu {
            return areAllContextualTargetsOnline
        }
        switch menuItem.action {
        case #selector(openSelected),
             #selector(quickLookSelected),
             #selector(revealSelected),
             #selector(openInTerminalSelected),
             #selector(showInfoSelected),
             #selector(copyFile),
             #selector(moveToTrashSelected),
             #selector(openWithApp(_:)),
             #selector(chooseOtherAppToOpen(_:)):
            return areAllContextualTargetsOnline
        case #selector(copyPath),
             #selector(copyName),
             #selector(excludeDirectorySelected(_:)):
            return !contextualURLs.isEmpty
        case #selector(filterByDirectorySelected):
            guard selectedResults.count == 1, let item = selectedResults.first else { return false }
            return scopeFilterDirectory(for: item) != nil
        case #selector(breadcrumbRevealInFinder(_:)),
             #selector(breadcrumbOpenItem(_:)),
             #selector(breadcrumbOpenInTerminal(_:)),
             #selector(breadcrumbShowInfo(_:)):
            guard let url = menuItem.representedObject as? URL else { return false }
            guard !pathBar.isOfflinePathMode, !isPathOffline(url.path) else { return false }
            if Self.isUnderVolumesNamespace(url.path) {
                return true
            }
            return FileManager.default.fileExists(atPath: url.path)
        case #selector(breadcrumbFilterByDirectory(_:)):
            guard let url = menuItem.representedObject as? URL else { return false }
            if pathBar.isOfflinePathMode || isPathOffline(url.path) || Self.isUnderVolumesNamespace(url.path) {
                return selectedResult?.path != url.path || selectedResult?.isDir == true
            }
            var isDir: ObjCBool = false
            return FileManager.default.fileExists(atPath: url.path, isDirectory: &isDir) && isDir.boolValue
        case #selector(breadcrumbCopyPath(_:)),
             #selector(breadcrumbCopyName(_:)):
            return menuItem.representedObject is URL
        default:
            return true
        }
    }
}

// MARK: - QLPreviewPanelDataSource & QLPreviewPanelDelegate

extension SearchResultsViewController: QLPreviewPanelDataSource, QLPreviewPanelDelegate {
    private var previewableResults: [FfiSearchResult] {
        let selected = selectedResults
        guard !selected.isEmpty, !selected.contains(where: { isResultOffline($0) }) else {
            return []
        }
        return selected
    }

    override func acceptsPreviewPanelControl(_ panel: QLPreviewPanel!) -> Bool {
        !previewableResults.isEmpty
    }

    override func beginPreviewPanelControl(_ panel: QLPreviewPanel!) {
        panel.delegate = self
        panel.dataSource = self
    }

    override func endPreviewPanelControl(_ panel: QLPreviewPanel!) {
        panel.delegate = nil
        panel.dataSource = nil
    }

    func numberOfPreviewItems(in panel: QLPreviewPanel!) -> Int {
        previewableResults.count
    }

    func previewPanel(_ panel: QLPreviewPanel!, previewItemAt index: Int) -> QLPreviewItem! {
        let items = previewableResults
        guard index >= 0, index < items.count else { return nil }
        return NSURL(fileURLWithPath: items[index].path)
    }

    /// 当前选中项对应的文件图标视图，用作快速预览等比缩放动画的屏幕锚点与过渡位图
    private var selectedIconImageView: NSImageView? {
        let row = tableView.selectedRow
        guard row >= 0, row < tableView.numberOfRows else { return nil }
        let col = tableView.column(withIdentifier: .columnName)
        guard col >= 0,
              let cellView = tableView.view(atColumn: col, row: row, makeIfNecessary: false) as? NSTableCellView else {
            return nil
        }
        return cellView.imageView
    }

    func previewPanel(_ panel: QLPreviewPanel!, sourceFrameOnScreenFor item: QLPreviewItem!) -> NSRect {
        guard let imageView = selectedIconImageView,
              let window = imageView.window,
              imageView.visibleRect.height > 0 else {
            return .zero
        }
        let rectInWindow = imageView.convert(imageView.bounds, to: nil)
        return window.convertToScreen(rectInWindow)
    }

    func previewPanel(_ panel: QLPreviewPanel!, transitionImageFor item: QLPreviewItem!, contentRect: UnsafeMutablePointer<NSRect>!) -> Any! {
        guard let image = selectedIconImageView?.image else { return nil }
        contentRect?.pointee = NSRect(origin: .zero, size: image.size)
        return image
    }
}

// MARK: - BreadcrumbPathControlDelegate

extension SearchResultsViewController: BreadcrumbPathControlDelegate {
    func breadcrumbPathControl(_ control: BreadcrumbPathControl, menuFor url: URL) -> NSMenu? {
        activeContextMenuPayload = [url]
        control.menuDelegate.onClose = { [weak self] in
            self?.activeContextMenuPayload = nil
        }

        let menu = NSMenu()
        menu.addLocalizedItem("search.ctx.open", action: #selector(breadcrumbOpenItem(_:)), keyEquivalent: "")
        menu.addLocalizedItem("search.ctx.revealInFinder", action: #selector(breadcrumbRevealInFinder(_:)), keyEquivalent: "")
        menu.addItem(NSMenuItem.separator())
        menu.addLocalizedItem("search.ctx.copyPath", action: #selector(breadcrumbCopyPath(_:)), keyEquivalent: "")
        menu.addLocalizedItem("search.ctx.copyName", action: #selector(breadcrumbCopyName(_:)), keyEquivalent: "")
        menu.addItem(NSMenuItem.separator())
        menu.addLocalizedItem("search.ctx.openInTerminal", action: #selector(breadcrumbOpenInTerminal(_:)), keyEquivalent: "")
        menu.addLocalizedItem("search.ctx.filterByDirectory", action: #selector(breadcrumbFilterByDirectory(_:)), keyEquivalent: "")
        menu.addLocalizedItem("search.ctx.getInfo", action: #selector(breadcrumbShowInfo(_:)), keyEquivalent: "")

        for item in menu.items where !item.isSeparatorItem {
            item.target = self
            item.representedObject = url
        }

        menu.allowsContextMenuPlugIns = true
        return menu
    }

    // MARK: - Breadcrumb Actions

    @objc private func breadcrumbRevealInFinder(_ sender: NSMenuItem) {
        guard let url = sender.representedObject as? URL else { return }
        guard !pathBar.isOfflinePathMode, !isPathOffline(url.path) else {
            showBlockedActionFeedback()
            return
        }
        delegate?.searchResultsDidConsumeResult()
        revealFilesHandler([url])
    }

    @objc private func breadcrumbOpenItem(_ sender: NSMenuItem) {
        guard let url = sender.representedObject as? URL else { return }
        guard !pathBar.isOfflinePathMode, !isPathOffline(url.path) else {
            showBlockedActionFeedback()
            return
        }
        delegate?.searchResultsDidConsumeResult()
        NSWorkspace.shared.open(url)
    }

    @objc private func breadcrumbCopyPath(_ sender: NSMenuItem) {
        guard let url = sender.representedObject as? URL else { return }
        writeStringToPasteboard(url.path)
    }

    @objc private func breadcrumbCopyName(_ sender: NSMenuItem) {
        guard let url = sender.representedObject as? URL else { return }
        writeStringToPasteboard(url.lastPathComponent)
    }

    @objc private func breadcrumbOpenInTerminal(_ sender: NSMenuItem) {
        guard let url = sender.representedObject as? URL else { return }
        guard !pathBar.isOfflinePathMode, !isPathOffline(url.path) else {
            showBlockedActionFeedback()
            return
        }
        delegate?.searchResultsDidConsumeResult()
        openInTerminalHandler([url])
    }

    @objc private func breadcrumbFilterByDirectory(_ sender: NSMenuItem) {
        guard let url = sender.representedObject as? URL else { return }
        delegate?.searchResultsDidConsumeResult()
        delegate?.searchResultsDidRequestScopeFilter(path: url.path)
    }

    @objc private func breadcrumbShowInfo(_ sender: NSMenuItem) {
        guard let url = sender.representedObject as? URL else { return }
        guard !pathBar.isOfflinePathMode, !isPathOffline(url.path) else {
            showBlockedActionFeedback()
            return
        }
        openFinderInfoWindow(for: [url.path])
    }
}

// MARK: - SearchTableView

/// 原生 AppKit 高信息密度虚拟列表，封装右键智能选区联动与前台即时响应机制。
final class SearchTableView: NSTableView {
    // 仅在真实数据行有效区域内绘制交替斑马纹，末尾空白区域填充统一纯色背景
    override func drawBackground(inClipRect clipRect: NSRect) {
        guard usesAlternatingRowBackgroundColors else {
            super.drawBackground(inClipRect: clipRect)
            return
        }

        let totalRows = numberOfRows
        guard totalRows > 0 else {
            backgroundColor.setFill()
            clipRect.fill()
            return
        }

        let lastRowRect = rect(ofRow: totalRows - 1)
        let contentBottom = lastRowRect.maxY

        let contentRect = clipRect.intersection(NSRect(
            x: bounds.minX,
            y: bounds.minY,
            width: bounds.width,
            height: contentBottom
        ))
        if !contentRect.isEmpty {
            super.drawBackground(inClipRect: contentRect)
        }

        if clipRect.minY < bounds.minY {
            let topOverhang = NSRect(
                x: clipRect.minX,
                y: clipRect.minY,
                width: clipRect.width,
                height: bounds.minY - clipRect.minY
            )
            backgroundColor.setFill()
            topOverhang.fill()
        }

        if clipRect.maxY > contentBottom {
            let bottomOverhang = NSRect(
                x: clipRect.minX,
                y: max(clipRect.minY, contentBottom),
                width: clipRect.width,
                height: clipRect.maxY - max(clipRect.minY, contentBottom)
            )
            backgroundColor.setFill()
            bottomOverhang.fill()
        }
    }

    /// 列表焦点位于首行（或无选区）时继续向上移动的接力回调
    var onMoveUpAtTop: (() -> Void)?

    override func keyDown(with event: NSEvent) {
        // Escape (keyCode 53)：当焦点处于结果列表时，触发统一 Escape 分层响应
        if event.keyCode == 53 {
            window?.cancelOperation(nil)
            return
        }

        if event.keyCode == 49 { // Space key
            if QLPreviewPanel.sharedPreviewPanelExists() && QLPreviewPanel.shared().isVisible {
                QLPreviewPanel.shared().orderOut(nil)
            } else if selectedRow >= 0 {
                (delegate as? SearchResultsViewController)?.quickLookSelected()
            }
            return
        }

        // 方向上键（keyCode 126）：当选区停在首行或无选区时，将焦点与光标接力回搜索输入框末尾。
        // 规避 ⇧/⌘ 组合键，确保多选与滚动至顶部行为不受影响。
        let navigationModifiers: NSEvent.ModifierFlags = [.command, .shift, .control]
        if event.keyCode == 126,
           event.modifierFlags.intersection(navigationModifiers).isEmpty,
           selectedRow <= 0 {
            onMoveUpAtTop?()
            return
        }

        // ⌘⌫ (Command + Delete，keyCode 51)：当焦点处于列表且存在选区时，触发移到废纸篓动作
        if event.keyCode == 51 && event.modifierFlags.contains(.command) {
            if let targetController = delegate as? SearchResultsViewController,
               !targetController.selectedResults.isEmpty {
                targetController.moveToTrashSelected()
                return
            }
        }

        super.keyDown(with: event)
    }

    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        // 焦点位于文本编辑器（如搜索输入框）时，将 ⌘⌫ 交还给文本编辑器的整行删除语义，防止误将预选首行移入废纸篓
        if event.keyCode == 51 && event.modifierFlags.contains(.command),
           window?.firstResponder is NSTextView {
            return false
        }
        if let menu = self.menu, menu.performKeyEquivalent(with: event) {
            return true
        }
        return super.performKeyEquivalent(with: event)
    }
    override func menu(for event: NSEvent) -> NSMenu? {
        let point = convert(event.locationInWindow, from: nil)
        let row = self.row(at: point)

        // 边界守卫：点击虚拟列表末尾空白区域时阻断上下文菜单，规避悬空操作
        guard row >= 0 && row < numberOfRows else {
            return nil
        }

        // 选区智能维护：
        // 若右键命中的行未被包含在当前选区，则将选区收拢重置为该单行；
        // 若命中的行已在选中集合内（例如用户已多选多项），则完整保持多选集合不被破坏。
        if !selectedRowIndexes.contains(row) {
            selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        }

        return super.menu(for: event)
    }

    // 视窗在非激活状态下接收初次点按，保证点击即选中，消除二次点击开销
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool {
        return true
    }
}

// MARK: - Highlighted Table Cell Views

/// 用户家目录，进程生命周期内恒定。
private let homeDirectory = NSHomeDirectory()

/// 家目录缩写结果：展示串与被替换前缀的 UTF-8 字节数。
private struct AbbreviatedHomePath {
    let display: String
    /// 被替换的家目录前缀字节数；命中区间起点落在此范围内即属于被缩写部分。
    let prefixBytes: Int

    /// 将原绝对路径上的 UTF-8 字节区间重映射到展示串上；若区间起点落在被缩写的前缀内则返回 nil。
    func displayRange(for range: FfiMatchRange) -> (start: Int, end: Int)? {
        let start = Int(range.start)
        guard start >= prefixBytes else { return nil }
        // '~' 占 1 字节，故整体前移 (prefixBytes - 1)；prefixBytes 为 0 时位移取 0
        let shift = max(prefixBytes - 1, 0)
        return (start - shift, Int(range.end) - shift)
    }
}

/// 把路径开头的家目录前缀缩写为 '~'；无需缩写时原样返回。
private func abbreviateHomePrefix(_ path: String) -> AbbreviatedHomePath {
    // 必须精确匹配家目录或其子路径，避免误伤同名前缀的其他目录
    guard path == homeDirectory || path.hasPrefix(homeDirectory + "/") else {
        return AbbreviatedHomePath(display: path, prefixBytes: 0)
    }

    let prefixIndex = path.index(path.startIndex, offsetBy: homeDirectory.count)
    // 直接量 path 切片内的实际字节数，规避 NFC/NFD 规范化差异带来的偏移
    let prefixBytes = path[..<prefixIndex].utf8.count
    return AbbreviatedHomePath(display: "~" + path[prefixIndex...], prefixBytes: prefixBytes)
}

/// 高亮单元格基类：统一字体、选中态配色与高亮渲染。
///
/// 展示串与 NSRange 在 `setDisplay` 阶段算好缓存，选中态变化只重新着色。
class SearchHighlightCellView: NSTableCellView {
    /// 未选中时的基准字色。
    var baseColor: NSColor { .labelColor }
    /// 选中态下基准字色的不透明度，突出命中字符。
    var selectedAlpha: CGFloat { 1 }
    /// 自定段落排版样式（如截断规则），默认为空由系统按文本框设定推导
    var paragraphStyle: NSParagraphStyle? { nil }

    /// 字体对象随全局排版密度动态绑定，并复用外部缓存池
    private(set) var regularFont: NSFont = NSFont.systemFont(ofSize: 13)
    private(set) var boldFont: NSFont = NSFont.systemFont(ofSize: 13, weight: .bold)

    func applyFonts(regular: NSFont, bold: NSFont) {
        if self.regularFont != regular || self.boldFont != bold {
            self.regularFont = regular
            self.boldFont = bold
            renderText()
        }
    }

    private var displayString = ""
    private var displayRanges: [NSRange] = []

    override var backgroundStyle: NSView.BackgroundStyle {
        didSet {
            guard oldValue != backgroundStyle else { return }
            renderText()
        }
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        renderText()
    }

    /// 更新展示串与 UTF-8 字节高亮区间；区间在此一次性换算为 NSRange 并缓存。
    func setDisplay(_ string: String, ranges: [(start: Int, end: Int)]) {
        displayString = string
        displayRanges = Self.nsRanges(in: string, byteRanges: ranges)
        renderText()
    }

    /// 后端下发的是 UTF-8 字节偏移，需夹取后映射为 NSRange。
    private static func nsRanges(in string: String,
                                 byteRanges: [(start: Int, end: Int)]) -> [NSRange] {
        guard !byteRanges.isEmpty else { return [] }
        let utf8 = string.utf8
        return byteRanges.compactMap { range in
            guard range.start < range.end,
                  let start = utf8.index(utf8.startIndex, offsetBy: range.start, limitedBy: utf8.endIndex),
                  let end = utf8.index(utf8.startIndex, offsetBy: range.end, limitedBy: utf8.endIndex),
                  start < end else {
                return nil
            }
            return NSRange(start..<end, in: string)
        }
    }

    private func renderText() {
        guard !displayString.isEmpty else {
            textField?.stringValue = ""
            return
        }

        let isSelected = backgroundStyle == .emphasized
        let defaultColor: NSColor = isSelected ? NSColor.white.withAlphaComponent(selectedAlpha) : baseColor

        // 无高亮区间时走纯文本快路径：NSTextField 原生保留自身的 lineBreakMode，无需逐行分配 NSMutableAttributedString
        if displayRanges.isEmpty {
            textField?.font = regularFont
            textField?.textColor = defaultColor
            textField?.stringValue = displayString
            return
        }

        let isDark = effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        let highlightColor: NSColor = isSelected ? .white : (isDark ? .controlAccentColor : .labelColor)

        var baseAttributes: [NSAttributedString.Key: Any] = [
            .font: regularFont,
            .foregroundColor: defaultColor,
        ]
        if let style = paragraphStyle {
            baseAttributes[.paragraphStyle] = style
        }

        let attributed = NSMutableAttributedString(string: displayString, attributes: baseAttributes)

        for range in displayRanges {
            attributed.addAttributes([
                .font: boldFont,
                .foregroundColor: highlightColor,
            ], range: range)
        }
        textField?.attributedStringValue = attributed
    }
}

// MARK: - Search Name Table Cell View

final class SearchNameTableCellView: SearchHighlightCellView {
    /// 选中态下普通字符用微透明白保持层级，命中字符用纯白加粗突出。
    override var selectedAlpha: CGFloat { 0.82 }

    /// 当前单元格绑定的文件路径，供异步应用包图标加载完成后校验是否已被复用
    var representedPath: String?

    private(set) var iconWidthConstraint: NSLayoutConstraint?
    private(set) var iconHeightConstraint: NSLayoutConstraint?

    func setIconConstraints(width: NSLayoutConstraint, height: NSLayoutConstraint) {
        self.iconWidthConstraint = width
        self.iconHeightConstraint = height
    }

    func updateIconSize(_ size: CGFloat) {
        iconWidthConstraint?.constant = size
        iconHeightConstraint?.constant = size
    }

    func configure(name: String, matchPositions: [FfiMatchRange], isOffline: Bool = false) {
        setDisplay(name, ranges: matchPositions.map { (Int($0.start), Int($0.end)) })
        let alpha: CGFloat = isOffline ? 0.55 : 1.0
        imageView?.alphaValue = alpha
        textField?.alphaValue = alpha
        toolTip = name
        textField?.toolTip = name
    }
}

// MARK: - Search Path Table Cell View

/// 路径列单元格：父目录高亮 + 家目录缩写重映射。
final class SearchPathTableCellView: SearchHighlightCellView {
    override var baseColor: NSColor { .secondaryLabelColor }
    override var selectedAlpha: CGFloat { 0.75 }

    private static let middleTruncateStyle: NSParagraphStyle = {
        let style = NSMutableParagraphStyle()
        style.lineBreakMode = .byTruncatingMiddle
        return style
    }()

    override var paragraphStyle: NSParagraphStyle? {
        Self.middleTruncateStyle
    }

    func configure(fullPath: String, parentMatchPositions: [FfiMatchRange], isOffline: Bool = false) {
        let parent = (fullPath as NSString).deletingLastPathComponent
        guard !parent.isEmpty else {
            let fallback = isOffline ? L("search.result.offline") : ""
            setDisplay(fallback, ranges: [])
            textField?.alphaValue = isOffline ? 0.55 : 1.0
            toolTip = fallback.isEmpty ? nil : fullPath
            textField?.toolTip = fallback.isEmpty ? nil : fullPath
            return
        }

        let abbreviated = abbreviateHomePrefix(parent)
        let display = isOffline
            ? "\(abbreviated.display) · \(L("search.result.offline"))"
            : abbreviated.display
        setDisplay(display,
                   ranges: parentMatchPositions.compactMap { abbreviated.displayRange(for: $0) })
        textField?.alphaValue = isOffline ? 0.55 : 1.0
        toolTip = fullPath
        textField?.toolTip = fullPath
    }
}

// MARK: - Column Identifiers

extension NSUserInterfaceItemIdentifier {
    static let columnName = NSUserInterfaceItemIdentifier("Name")
    static let columnPath = NSUserInterfaceItemIdentifier("Path")
    static let columnSize = NSUserInterfaceItemIdentifier("Size")
    static let columnModified = NSUserInterfaceItemIdentifier("DateModified")
    static let columnCreated = NSUserInterfaceItemIdentifier("DateCreated")
    static let columnExtension = NSUserInterfaceItemIdentifier("Extension")
}
