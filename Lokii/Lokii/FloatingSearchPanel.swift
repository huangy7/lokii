import AppKit
import Quartz

/// 将 Quick Look 预览面板的响应链委托桥接至悬浮搜索控制器，避免无标题栏面板拦截系统预览生命周期
protocol FloatingSearchPreviewPanelHandling: AnyObject {
    func floatingPanel(_ panel: NSPanel, acceptsPreviewPanelControl previewPanel: QLPreviewPanel!) -> Bool
    func floatingPanel(_ panel: NSPanel, beginPreviewPanelControl previewPanel: QLPreviewPanel!)
    func floatingPanel(_ panel: NSPanel, endPreviewPanelControl previewPanel: QLPreviewPanel!)
}

/// 无标题栏悬浮快速搜索面板，采用非占用式浮动层级以规避程序坞图标闪烁并支持全局跨空间唤起
final class FloatingSearchPanel: NSPanel {
    var onEscape: (() -> Void)?
    var onCommandShiftM: (() -> Void)?
    var onOptionF: (() -> Void)?
    var onCommandF: (() -> Void)?
    var onCommandReturn: (() -> Void)?
    var onCommandShiftC: (() -> Void)?
    var onCommandT: (() -> Void)?
    var onQuickLookShortcut: (() -> Void)?
    weak var previewPanelHandler: FloatingSearchPreviewPanelHandling?

    init(contentRect: NSRect) {
        super.init(
            contentRect: contentRect,
            styleMask: [.borderless],
            backing: .buffered,
            defer: false
        )
        isFloatingPanel = true
        level = .floating
        isOpaque = false
        backgroundColor = .clear
        hasShadow = true
        hidesOnDeactivate = false
        isMovableByWindowBackground = true
        isReleasedWhenClosed = false
        collectionBehavior = [.moveToActiveSpace, .fullScreenAuxiliary, .ignoresCycle]
    }

    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { true }

    override func animationResizeTime(_ newFrame: NSRect) -> TimeInterval {
        0.12
    }

    override func cancelOperation(_ sender: Any?) {
        onEscape?()
    }

    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        let flags = event.modifierFlags.intersection([.command, .option, .control, .shift])
        let key = event.charactersIgnoringModifiers?.lowercased()

        if flags == [.command, .shift] && key == "m" {
            onCommandShiftM?()
            return true
        }
        if flags == .option && key == "f" {
            onOptionF?()
            return true
        }
        if flags == .command && key == "f" {
            onCommandF?()
            return true
        }
        if flags == .command && event.keyCode == 36 {
            onCommandReturn?()
            return true
        }
        if flags == [.command, .shift] && key == "c" {
            onCommandShiftC?()
            return true
        }
        if flags == .command && key == "t" {
            onCommandT?()
            return true
        }
        if flags == .command && key == "y" {
            onQuickLookShortcut?()
            return true
        }
        return super.performKeyEquivalent(with: event)
    }

    override func acceptsPreviewPanelControl(_ panel: QLPreviewPanel!) -> Bool {
        previewPanelHandler?.floatingPanel(self, acceptsPreviewPanelControl: panel) ?? false
    }

    override func beginPreviewPanelControl(_ panel: QLPreviewPanel!) {
        previewPanelHandler?.floatingPanel(self, beginPreviewPanelControl: panel)
    }

    override func endPreviewPanelControl(_ panel: QLPreviewPanel!) {
        previewPanelHandler?.floatingPanel(self, endPreviewPanelControl: panel)
    }
}
