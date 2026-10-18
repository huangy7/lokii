import SwiftUI
import AppKit

// MARK: - Pasteboard Writer for .app Bundle
final class AppBundlePasteboardWriter: NSObject, NSPasteboardWriting {
    let url: URL

    init(url: URL) {
        self.url = url
    }

    func writableTypes(for pasteboard: NSPasteboard) -> [NSPasteboard.PasteboardType] {
        [
            .fileURL,
            .URL,
            NSPasteboard.PasteboardType("NSFilenamesPboardType"),
            NSPasteboard.PasteboardType("com.apple.pasteboard.promised-file-url"),
            .string
        ]
    }

    func pasteboardPropertyList(forType type: NSPasteboard.PasteboardType) -> Any? {
        switch type {
        case .fileURL, .URL, NSPasteboard.PasteboardType("com.apple.pasteboard.promised-file-url"):
            return url.absoluteString
        case NSPasteboard.PasteboardType("NSFilenamesPboardType"):
            return [url.path]
        case .string:
            return url.path
        default:
            return nil
        }
    }
}

// MARK: - Draggable NSView (conforming to NSDraggingSource)
final class DraggableIconNSView: NSView, NSDraggingSource {
    var appURL: URL
    var size: CGFloat
    private var mouseDownPoint: NSPoint?
    private var hasBegunDragging = false

    init(appURL: URL, size: CGFloat) {
        self.appURL = appURL
        self.size = size
        super.init(frame: NSRect(x: 0, y: 0, width: size, height: size))
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    override func resetCursorRects() {
        super.resetCursorRects()
        addCursorRect(bounds, cursor: .openHand)
    }

    override func mouseDown(with event: NSEvent) {
        mouseDownPoint = convert(event.locationInWindow, from: nil)
        hasBegunDragging = false
    }

    override func mouseDragged(with event: NSEvent) {
        guard !hasBegunDragging, let mouseDownPoint = mouseDownPoint else { return }
        let currentPoint = convert(event.locationInWindow, from: nil)
        let distance = hypot(currentPoint.x - mouseDownPoint.x, currentPoint.y - mouseDownPoint.y)
        guard distance > 3 else { return }

        hasBegunDragging = true
        beginAppDrag(with: event)
    }

    override func mouseUp(with event: NSEvent) {
        mouseDownPoint = nil
        hasBegunDragging = false
    }

    private func beginAppDrag(with event: NSEvent) {
        let writer = AppBundlePasteboardWriter(url: appURL)
        let draggingItem = NSDraggingItem(pasteboardWriter: writer)

        let icon = dragIconImage()
        let dragPoint = convert(event.locationInWindow, from: nil)
        let dragFrame = NSRect(
            x: dragPoint.x - size / 2,
            y: dragPoint.y - size / 2,
            width: size,
            height: size
        )
        draggingItem.setDraggingFrame(dragFrame, contents: icon)

        let session = beginDraggingSession(with: [draggingItem], event: event, source: self)
        session.animatesToStartingPositionsOnCancelOrFail = true
        session.draggingFormation = .none
    }

    private func dragIconImage() -> NSImage {
        if let customIcon = NSImage(named: "AppIcon"), customIcon.isValid {
            let copy = customIcon.copy() as! NSImage
            copy.size = NSSize(width: size, height: size)
            return copy
        }
        let fileIcon = NSWorkspace.shared.icon(forFile: appURL.path)
        fileIcon.size = NSSize(width: size, height: size)
        return fileIcon
    }

    func draggingSession(_ session: NSDraggingSession, sourceOperationMaskFor context: NSDraggingContext) -> NSDragOperation {
        .copy
    }

    func ignoreModifierKeys(for session: NSDraggingSession) -> Bool {
        true
    }
}

// MARK: - SwiftUI Representable for Drag Area
private struct DraggableAppBadgeRepresentable: NSViewRepresentable {
    let appURL: URL
    let size: CGFloat

    func makeNSView(context: Context) -> DraggableIconNSView {
        DraggableIconNSView(appURL: appURL, size: size)
    }

    func updateNSView(_ nsView: DraggableIconNSView, context: Context) {
        nsView.appURL = appURL
        nsView.size = size
    }
}

// MARK: - FDA Guide View (Apple Native White Style with Direct Drag)
struct FDAGuideView: View {
    var onLater: (Bool) -> Void
    var onOpenSettings: (Bool) -> Void

    @State private var dontShowAgain = false

    private var appURL: URL {
        let url = Bundle.main.bundleURL
        if url.pathExtension == "app" {
            return url
        }
        var cur = url
        while cur.path != "/" {
            if cur.pathExtension == "app" {
                return cur
            }
            cur = cur.deletingLastPathComponent()
        }
        return url
    }

    var body: some View {
        VStack(spacing: 0) {
            // Main Alert Lockup (Horizontal: Left Icon + Right Texts)
            HStack(alignment: .top, spacing: 18) {
                // Left: Draggable App Icon Lockup
                VStack(spacing: 6) {
                    ZStack {
                        appIconBadge(size: 54)

                        // Transparent native drag gesture layer over the icon
                        DraggableAppBadgeRepresentable(appURL: appURL, size: 54)
                            .frame(width: 54, height: 54)
                    }

                    HStack(spacing: 3) {
                        Image(systemName: "hand.draw.fill")
                            .font(.system(size: 8.5))
                        Text(L("fda.drag.badge"))
                            .font(.system(size: 10, weight: .medium))
                    }
                    .foregroundColor(.secondary)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(
                        Capsule()
                            .fill(Color(nsColor: .quaternaryLabelColor).opacity(0.35))
                    )
                }

                // Right: Text info & Finder fallback
                VStack(alignment: .leading, spacing: 6) {
                    Text(L("fda.title"))
                        .font(.system(size: 13.5, weight: .semibold))
                        .foregroundColor(.primary)

                    Text(L("fda.description"))
                        .font(.system(size: 12))
                        .foregroundColor(.secondary)
                        .lineSpacing(3)
                        .fixedSize(horizontal: false, vertical: true)

                    Button {
                        NSWorkspace.shared.activateFileViewerSelecting([appURL])
                    } label: {
                        HStack(spacing: 4) {
                            Image(systemName: "folder")
                                .font(.system(size: 10.5))
                            Text(L("fda.showInFinder"))
                                .font(.system(size: 11))
                        }
                        .foregroundColor(.accentColor)
                    }
                    .buttonStyle(.plain)
                    .onHover { isHovered in
                        if isHovered {
                            NSCursor.pointingHand.push()
                        } else {
                            NSCursor.pop()
                        }
                    }
                    .padding(.top, 2)
                }
            }
            .padding(.top, 20)

            Spacer(minLength: 14)

            // Bottom Actions Bar
            bottomBar
        }
        .padding(.horizontal, 22)
        .padding(.bottom, 16)
        .frame(width: 410, height: 166)
        .background(
            Color(nsColor: .windowBackgroundColor)
                .ignoresSafeArea()
        )
        .onAppear {
            dontShowAgain = FDAGuideWindowController.isPromptDismissed
        }
    }

    // MARK: - Bottom Actions Bar
    private var bottomBar: some View {
        HStack(alignment: .center) {
            Toggle(L("fda.dontShowAgain"), isOn: $dontShowAgain)
                .toggleStyle(.checkbox)
                .font(.system(size: 11.5))
                .foregroundColor(.secondary)

            Spacer()

            HStack(spacing: 10) {
                Button(L("fda.later")) {
                    onLater(dontShowAgain)
                }
                .keyboardShortcut(.cancelAction)
                .buttonStyle(.bordered)
                .controlSize(.regular)

                Button {
                    onOpenSettings(dontShowAgain)
                } label: {
                    HStack(spacing: 4) {
                        Text(L("fda.openSystemSettings"))
                            .font(.system(size: 12, weight: .medium))
                        Image(systemName: "arrow.up.forward.app")
                            .font(.system(size: 10, weight: .semibold))
                    }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .controlSize(.regular)
            }
        }
    }

    // MARK: - App Icon Badge
    private func appIconBadge(size: CGFloat) -> some View {
        Group {
            if let customIcon = NSImage(named: "AppIcon"), customIcon.isValid {
                Image(nsImage: customIcon)
                    .resizable()
                    .scaledToFit()
                    .frame(width: size, height: size)
                    .clipShape(RoundedRectangle(cornerRadius: 13.5, style: .continuous))
                    .overlay(
                        RoundedRectangle(cornerRadius: 13.5, style: .continuous)
                            .strokeBorder(Color(nsColor: .separatorColor).opacity(0.35), lineWidth: 0.5)
                    )
                    .shadow(color: Color.black.opacity(0.12), radius: 6, x: 0, y: 3)
            } else {
                ZStack {
                    RoundedRectangle(cornerRadius: 13.5, style: .continuous)
                        .fill(
                            LinearGradient(
                                colors: [Color.accentColor, Color(nsColor: .systemIndigo)],
                                startPoint: .topLeading,
                                endPoint: .bottomTrailing
                            )
                        )
                        .frame(width: size, height: size)
                        .overlay(
                            RoundedRectangle(cornerRadius: 13.5, style: .continuous)
                                .strokeBorder(Color.white.opacity(0.24), lineWidth: 1)
                        )
                        .shadow(color: Color.black.opacity(0.14), radius: 6, x: 0, y: 3)

                    Image(systemName: "magnifyingglass")
                        .font(.system(size: 24, weight: .bold))
                        .foregroundColor(.white)
                }
            }
        }
    }
}

// MARK: - Indexing Overlay State
final class IndexingOverlayState: ObservableObject {
    @Published var hasFDA: Bool
    @Published var promptDismissed: Bool
    @Published var scannedCount: UInt64 = 0
    @Published var headingText: String
    @Published var statusText: String

    init(hasFDA: Bool, promptDismissed: Bool, heading: String) {
        self.hasFDA = hasFDA
        self.promptDismissed = promptDismissed
        self.headingText = heading
        self.statusText = L("search.scanningFiles")
    }

    func markFDAGranted() {
        guard !hasFDA else { return }
        withAnimation(.spring(response: 0.45, dampingFraction: 0.8)) {
            hasFDA = true
        }
        FDAGuideWindowController.clearPromptDismissed()
    }

    func syncDismissedState() {
        let dismissed = FDAGuideWindowController.isPromptDismissed
        if self.promptDismissed != dismissed {
            withAnimation(.spring(response: 0.45, dampingFraction: 0.8)) {
                self.promptDismissed = dismissed
            }
        }
    }
}

// MARK: - Slim Shimmer Progress Bar
struct SlimShimmerProgressBar: View {
    @State private var phase: CGFloat = 0

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                RoundedRectangle(cornerRadius: 1.5, style: .continuous)
                    .fill(Color.secondary.opacity(0.12))

                RoundedRectangle(cornerRadius: 1.5, style: .continuous)
                    .fill(
                        LinearGradient(
                            colors: [
                                Color.accentColor.opacity(0.2),
                                Color.accentColor,
                                Color(nsColor: .systemTeal),
                                Color.accentColor.opacity(0.2)
                            ],
                            startPoint: .leading,
                            endPoint: .trailing
                        )
                    )
                    .frame(width: max(geo.size.width * 0.45, 60))
                    .offset(x: phase * (geo.size.width - max(geo.size.width * 0.45, 60)))
            }
        }
        .frame(height: 2.5)
        .clipShape(RoundedRectangle(cornerRadius: 1.5, style: .continuous))
        .onAppear {
            withAnimation(
                .easeInOut(duration: 1.8)
                .repeatForever(autoreverses: true)
            ) {
                phase = 1.0
            }
        }
    }
}

// MARK: - Bottom Trailing FDA Pill
struct BottomTrailingFDAPill: View {
    @ObservedObject var state: IndexingOverlayState
    @State private var isHovered = false

    var body: some View {
        if !state.hasFDA && !state.promptDismissed {
            Button {
                FDAGuideWindowController.show()
            } label: {
                HStack(spacing: 6) {
                    Circle()
                        .fill(Color.orange)
                        .frame(width: 5, height: 5)
                        .shadow(color: Color.orange.opacity(0.6), radius: 2)

                    Text(L("fda.restrictedHint"))
                        .font(.system(size: 11.5, weight: .medium))
                        .foregroundColor(isHovered ? .primary : .secondary)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .background(
                    Capsule()
                        .fill(Color(nsColor: .controlBackgroundColor).opacity(isHovered ? 1.0 : 0.94))
                )
                .overlay(
                    Capsule()
                        .strokeBorder(Color(nsColor: .separatorColor).opacity(isHovered ? 0.6 : 0.35), lineWidth: 0.75)
                )
                .shadow(color: Color.black.opacity(isHovered ? 0.08 : 0.04), radius: 6, x: 0, y: 2)
            }
            .buttonStyle(.plain)
            .onHover { hovering in
                isHovered = hovering
                if hovering {
                    NSCursor.pointingHand.push()
                } else {
                    NSCursor.pop()
                }
            }
            .transition(.opacity.combined(with: .scale(scale: 0.95)))
        }
    }
}

// MARK: - Fullscreen Indexing Overlay View
struct IndexingFullScreenOverlayView: View {
    @ObservedObject var state: IndexingOverlayState

    var body: some View {
        ZStack {
            // 纯正明亮的白色主体背景（浅色模式纯净视窗白，深色模式自适应系统深色）
            Color(nsColor: .controlBackgroundColor)
                .ignoresSafeArea()

            // 居中原生视觉主体
            VStack(spacing: 22) {
                // 苹果原生大号菊花转轮
                ProgressView()
                    .progressViewStyle(.circular)
                    .controlSize(.large)
                    .scaleEffect(1.2)

                // 标题与数据指示区（排版严密，彻底根除文本重复）
                VStack(spacing: 6) {
                    Text(state.headingText)
                        .font(.system(size: 15, weight: .semibold))
                        .foregroundColor(.primary)

                    if state.scannedCount > 0 {
                        Text(formatFileCount(state.scannedCount))
                            .font(.system(size: 38, weight: .bold, design: .monospaced))
                            .foregroundColor(.primary)

                        Text(L("search.scannedFilesLabel"))
                            .font(.system(size: 12))
                            .foregroundColor(.secondary)
                    } else {
                        Text(state.statusText)
                            .font(.system(size: 12))
                            .foregroundColor(.secondary)
                    }
                }

                // 2.5pt 原生微流光细条
                SlimShimmerProgressBar()
                    .frame(width: 200)
                    .padding(.top, 2)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)

            // 右下角克制悬浮白底药丸胶囊
            VStack {
                Spacer()
                HStack {
                    Spacer()
                    BottomTrailingFDAPill(state: state)
                        .padding(.trailing, 20)
                        .padding(.bottom, 16)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
