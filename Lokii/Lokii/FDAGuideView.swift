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

// MARK: - FDA Guide View (Plan 2: Compact Native Alert with Direct Drag)
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
            HStack(alignment: .top, spacing: 16) {
                // Left: Draggable App Icon Lockup
                VStack(spacing: 5) {
                    ZStack {
                        appIconBadge(size: 52)

                        // Transparent native drag gesture layer over the icon
                        DraggableAppBadgeRepresentable(appURL: appURL, size: 52)
                            .frame(width: 52, height: 52)
                    }

                    HStack(spacing: 2) {
                        Image(systemName: "hand.draw.fill")
                            .font(.system(size: 8))
                        Text(L("fda.drag.badge"))
                            .font(.system(size: 9.5, weight: .medium))
                    }
                    .foregroundColor(.secondary)
                }

                // Right: Text info & Finder fallback
                VStack(alignment: .leading, spacing: 5) {
                    Text(L("fda.title"))
                        .font(.system(size: 13, weight: .bold))
                        .foregroundColor(.primary)

                    Text(L("fda.description"))
                        .font(.system(size: 11.5))
                        .foregroundColor(.secondary)
                        .lineSpacing(2)
                        .fixedSize(horizontal: false, vertical: true)

                    Button {
                        NSWorkspace.shared.activateFileViewerSelecting([appURL])
                    } label: {
                        HStack(spacing: 3) {
                            Image(systemName: "folder")
                                .font(.system(size: 9.5))
                            Text(L("fda.showInFinder"))
                                .font(.system(size: 10.5))
                        }
                        .foregroundColor(.secondary)
                    }
                    .buttonStyle(.plain)
                    .padding(.top, 2)
                }
            }
            .padding(.top, 18)

            Spacer(minLength: 12)

            // Bottom Actions Bar
            bottomBar
        }
        .padding(.horizontal, 20)
        .padding(.bottom, 14)
        .frame(width: 380, height: 154)
        .background(FDAVisualEffectView().ignoresSafeArea())
        .onAppear {
            dontShowAgain = UserDefaults.standard.bool(forKey: "FDAPromptDismissed")
        }
    }

    // MARK: - Bottom Actions Bar
    private var bottomBar: some View {
        HStack(alignment: .center) {
            Toggle(L("fda.dontShowAgain"), isOn: $dontShowAgain)
                .toggleStyle(.checkbox)
                .font(.system(size: 11))

            Spacer()

            HStack(spacing: 8) {
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
                    .clipShape(RoundedRectangle(cornerRadius: 13, style: .continuous))
            } else {
                ZStack {
                    RoundedRectangle(cornerRadius: 13, style: .continuous)
                        .fill(
                            LinearGradient(
                                colors: [Color.accentColor, Color(nsColor: .systemIndigo)],
                                startPoint: .topLeading,
                                endPoint: .bottomTrailing
                            )
                        )
                        .frame(width: size, height: size)
                        .overlay(
                            RoundedRectangle(cornerRadius: 13, style: .continuous)
                                .strokeBorder(Color.white.opacity(0.22), lineWidth: 1)
                        )
                        .shadow(color: Color.black.opacity(0.15), radius: 4, x: 0, y: 2)

                    Image(systemName: "magnifyingglass")
                        .font(.system(size: 22, weight: .bold))
                        .foregroundColor(.white)
                }
            }
        }
    }
}

// MARK: - Native Visual Effect View
private struct FDAVisualEffectView: NSViewRepresentable {
    func makeNSView(context: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.blendingMode = .behindWindow
        view.state = .active
        view.material = .underWindowBackground
        return view
    }
    func updateNSView(_ nsView: NSVisualEffectView, context: Context) {}
}
