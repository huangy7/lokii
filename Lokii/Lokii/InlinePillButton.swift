import AppKit

/// 点击透传的只读标签，避免拦截外层 NSButton 的鼠标按下与点击事件。
private final class PassthroughLabel: NSTextField {
    override func hitTest(_ point: NSPoint) -> NSView? { nil }
}

/// 禁用态斜杠覆盖层，仅使用纯矢量路径绘制在标签上方，避免触发 NSStringDrawing / CoreText。
private final class PillSlashOverlayView: NSView {
    var strokeColor: NSColor = .tertiaryLabelColor {
        didSet { needsDisplay = true }
    }
    var targetRect: NSRect = .zero {
        didSet { needsDisplay = true }
    }

    override var isFlipped: Bool { true }
    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func draw(_ dirtyRect: NSRect) {
        guard !targetRect.isEmpty else { return }
        let slashPath = NSBezierPath()
        let padding: CGFloat = 1.0
        let startY = targetRect.maxY - 2
        let endY = targetRect.minY + 2
        slashPath.move(to: NSPoint(x: targetRect.minX - padding, y: startY))
        slashPath.line(to: NSPoint(x: targetRect.maxX + padding, y: endY))
        slashPath.lineWidth = 1.3
        slashPath.lineCapStyle = .round
        strokeColor.setStroke()
        slashPath.stroke()
    }
}

/// 搜索框内联胶囊按钮基类。
/// 统一实现：焦点封锁（点击不夺走搜索框光标）、悬停跟踪、胶囊底衬绘制，
/// 以及状态由本地推导激活时的 120ms 弹性微动效。子类仅提供文案、字型与各状态提示。
/// 文案交由原生 NSTextField 渲染，避免在 NSView.draw(_:) 中直接调用 NSStringDrawing
/// 导致系统深浅外观切换时 CoreText 内部抛出字典空值异常。
class InlinePillButton: NSButton {
    enum VisualState: Equatable {
        case inactive          // 幽灵浅灰
        case autoActive        // 柔和强调色微光，120ms 弹性微动效
        case locked            // 强调色实心胶囊，纯白高对比度文字
        case inactiveSlashed   // 幽灵浅灰带斜杠
    }

    var visualState: VisualState = .inactive {
        didSet {
            guard oldValue != visualState else { return }
            updateTooltipAndAccessibility()
            if visualState == .autoActive {
                pulse()
            }
            updateVisualAppearance()
        }
    }

    /// 胶囊内文案（如 "Aa"、".*"）。
    var buttonText: String { "" }

    /// 胶囊内文本字型。
    var labelFont: NSFont { .systemFont(ofSize: 11, weight: .semibold) }

    override var acceptsFirstResponder: Bool { false }
    override var canBecomeKeyView: Bool { false }
    override var needsPanelToBecomeKey: Bool { false }

    override var intrinsicContentSize: NSSize {
        NSSize(width: 28, height: 20)
    }

    override var isHighlighted: Bool {
        didSet {
            if oldValue != isHighlighted {
                updateVisualAppearance()
            }
        }
    }

    private var isHovered: Bool = false {
        didSet {
            if oldValue != isHovered {
                updateVisualAppearance()
            }
        }
    }

    private let titleLabel = PassthroughLabel(labelWithString: "")
    private let slashOverlay = PillSlashOverlayView()
    private var trackingAreaRef: NSTrackingArea?

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        commonInit()
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        commonInit()
    }

    private func commonInit() {
        isBordered = false
        focusRingType = .none
        refusesFirstResponder = true
        wantsLayer = true

        titleLabel.translatesAutoresizingMaskIntoConstraints = false
        titleLabel.stringValue = buttonText
        titleLabel.font = labelFont
        titleLabel.alignment = .center
        titleLabel.lineBreakMode = .byClipping
        titleLabel.isEditable = false
        titleLabel.isSelectable = false
        titleLabel.isBezeled = false
        titleLabel.drawsBackground = false
        addSubview(titleLabel)

        slashOverlay.translatesAutoresizingMaskIntoConstraints = false
        slashOverlay.isHidden = true
        addSubview(slashOverlay)

        NSLayoutConstraint.activate([
            titleLabel.centerXAnchor.constraint(equalTo: centerXAnchor),
            titleLabel.centerYAnchor.constraint(equalTo: centerYAnchor),
            slashOverlay.leadingAnchor.constraint(equalTo: leadingAnchor),
            slashOverlay.trailingAnchor.constraint(equalTo: trailingAnchor),
            slashOverlay.topAnchor.constraint(equalTo: topAnchor),
            slashOverlay.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])

        updateTooltipAndAccessibility()
        updateVisualAppearance()
    }

    override func layout() {
        super.layout()
        slashOverlay.targetRect = titleLabel.frame
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        updateVisualAppearance()
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

    override func mouseEntered(with event: NSEvent) {
        super.mouseEntered(with: event)
        isHovered = true
    }

    override func mouseExited(with event: NSEvent) {
        super.mouseExited(with: event)
        isHovered = false
    }

    /// 120ms 弹性微动效，提示状态已由本地推导激活。
    func pulse() {
        guard let layer = layer else { return }
        layer.removeAnimation(forKey: "capsulePulse")
        let anim = CAKeyframeAnimation(keyPath: "transform")
        let w = bounds.width > 0 ? bounds.width : 28.0
        let h = bounds.height > 0 ? bounds.height : 20.0
        let cx = w * (0.5 - layer.anchorPoint.x)
        let cy = h * (0.5 - layer.anchorPoint.y)

        func makeTransform(scale: CGFloat) -> CATransform3D {
            var t = CATransform3DMakeTranslation(cx, cy, 0)
            t = CATransform3DScale(t, scale, scale, 1.0)
            t = CATransform3DTranslate(t, -cx, -cy, 0)
            return t
        }

        anim.values = [
            NSValue(caTransform3D: makeTransform(scale: 0.90)),
            NSValue(caTransform3D: makeTransform(scale: 1.08)),
            NSValue(caTransform3D: makeTransform(scale: 0.98)),
            NSValue(caTransform3D: makeTransform(scale: 1.0))
        ]
        anim.keyTimes = [0.0, 0.4, 0.75, 1.0]
        anim.duration = 0.12
        anim.timingFunction = CAMediaTimingFunction(name: .easeOut)
        layer.add(anim, forKey: "capsulePulse")
    }

    /// 子类覆写以提供各状态的工具提示与无障碍标签。
    func tooltipText(for state: VisualState) -> String {
        ""
    }

    func updateTooltipAndAccessibility() {
        let text = tooltipText(for: visualState)
        toolTip = text
        setAccessibilityLabel(text)
    }

    private func resolvedColors() -> (bg: NSColor, fg: NSColor) {
        let isPressed = isHighlighted
        switch visualState {
        case .inactive, .inactiveSlashed:
            let bg: NSColor
            if isPressed {
                bg = NSColor.labelColor.withAlphaComponent(0.10)
            } else if isHovered {
                bg = NSColor.labelColor.withAlphaComponent(0.06)
            } else {
                bg = .clear
            }
            let fg: NSColor = (visualState == .inactiveSlashed)
                ? .tertiaryLabelColor
                : (isHovered ? .secondaryLabelColor : .tertiaryLabelColor)
            return (bg, fg)

        case .autoActive:
            let bg: NSColor
            if isPressed {
                bg = NSColor.controlAccentColor.withAlphaComponent(0.22)
            } else if isHovered {
                bg = NSColor.controlAccentColor.withAlphaComponent(0.18)
            } else {
                bg = NSColor.controlAccentColor.withAlphaComponent(0.12)
            }
            return (bg, .controlAccentColor)

        case .locked:
            let bg = isPressed
                ? NSColor.controlAccentColor.withAlphaComponent(0.85)
                : NSColor.controlAccentColor
            return (bg, .white)
        }
    }

    private func updateVisualAppearance() {
        let (_, fgColor) = resolvedColors()
        titleLabel.textColor = fgColor
        slashOverlay.strokeColor = fgColor
        slashOverlay.targetRect = titleLabel.frame
        slashOverlay.isHidden = (visualState != .inactiveSlashed)
        needsDisplay = true
    }

    override func draw(_ dirtyRect: NSRect) {
        let (bgColor, _) = resolvedColors()
        if bgColor != .clear {
            let pillRect = bounds.insetBy(dx: 1, dy: 1)
            let pillPath = NSBezierPath(roundedRect: pillRect, xRadius: 4.5, yRadius: 4.5)
            bgColor.setFill()
            pillPath.fill()
        }
    }
}
