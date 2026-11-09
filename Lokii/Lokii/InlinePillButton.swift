import AppKit

/// 搜索框内联胶囊按钮基类。
/// 统一实现：焦点封锁（点击不夺走搜索框光标）、悬停跟踪、胶囊底衬绘制，
/// 以及状态由本地推导激活时的 120ms 弹性微动效。子类仅提供文案、字型与各状态提示。
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
            needsDisplay = true
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

    private var isHovered: Bool = false {
        didSet {
            if oldValue != isHovered {
                needsDisplay = true
            }
        }
    }

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
        updateTooltipAndAccessibility()
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

    override func draw(_ dirtyRect: NSRect) {
        let isPressed = isHighlighted
        let pillRect = bounds.insetBy(dx: 1, dy: 1)
        let pillPath = NSBezierPath(roundedRect: pillRect, xRadius: 4.5, yRadius: 4.5)

        let bgColor: NSColor
        let fgColor: NSColor

        switch visualState {
        case .inactive, .inactiveSlashed:
            if isPressed {
                bgColor = NSColor.labelColor.withAlphaComponent(0.10)
            } else if isHovered {
                bgColor = NSColor.labelColor.withAlphaComponent(0.06)
            } else {
                bgColor = .clear
            }
            if visualState == .inactiveSlashed {
                fgColor = .tertiaryLabelColor
            } else {
                fgColor = isHovered ? .secondaryLabelColor : .tertiaryLabelColor
            }

        case .autoActive:
            if isPressed {
                bgColor = NSColor.controlAccentColor.withAlphaComponent(0.22)
            } else if isHovered {
                bgColor = NSColor.controlAccentColor.withAlphaComponent(0.18)
            } else {
                bgColor = NSColor.controlAccentColor.withAlphaComponent(0.12)
            }
            fgColor = .controlAccentColor

        case .locked:
            if isPressed {
                bgColor = NSColor.controlAccentColor.withAlphaComponent(0.85)
            } else {
                bgColor = .controlAccentColor
            }
            fgColor = .white
        }

        if bgColor != .clear {
            bgColor.setFill()
            pillPath.fill()
        }

        let text = buttonText as NSString
        let attrs: [NSAttributedString.Key: Any] = [
            .font: labelFont,
            .foregroundColor: fgColor
        ]
        let textSize = text.size(withAttributes: attrs)
        let textRect = NSRect(
            x: round((bounds.width - textSize.width) / 2.0),
            y: round((bounds.height - textSize.height) / 2.0),
            width: textSize.width,
            height: textSize.height
        )
        text.draw(in: textRect, withAttributes: attrs)

        if visualState == .inactiveSlashed {
            let slashPath = NSBezierPath()
            let padding: CGFloat = 1.0
            let startY = isFlipped ? (textRect.maxY - 2) : (textRect.minY + 2)
            let endY = isFlipped ? (textRect.minY + 2) : (textRect.maxY - 2)
            slashPath.move(to: NSPoint(x: textRect.minX - padding, y: startY))
            slashPath.line(to: NSPoint(x: textRect.maxX + padding, y: endY))
            slashPath.lineWidth = 1.3
            slashPath.lineCapStyle = .round
            fgColor.setStroke()
            slashPath.stroke()
        }
    }
}
