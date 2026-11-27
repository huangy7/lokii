import AppKit
import UniformTypeIdentifiers

/// 悬浮搜索条高饱和胶囊行选区视图：使用系统强调色圆角胶囊呈现选中项，并与玻璃容器边缘保持 10pt 内缩
final class FloatingGlassRowView: NSTableRowView {
    override var interiorBackgroundStyle: NSView.BackgroundStyle { .normal }

    override var isSelected: Bool {
        didSet {
            guard oldValue != isSelected else { return }
            propagateSelectionState()
        }
    }

    override func didAddSubview(_ subview: NSView) {
        super.didAddSubview(subview)
        if let cell = subview as? FloatingSearchResultCellView {
            cell.setRowSelected(isSelected)
        }
    }

    override func drawSelection(in dirtyRect: NSRect) {
        guard isSelected else { return }
        let pillRect = bounds.insetBy(dx: 10, dy: 2)
        guard pillRect.width > 0, pillRect.height > 0 else { return }

        let path = NSBezierPath(roundedRect: pillRect, xRadius: 14, yRadius: 14)
        NSColor.controlAccentColor.setFill()
        path.fill()
    }

    private func propagateSelectionState() {
        for subview in subviews {
            if let cell = subview as? FloatingSearchResultCellView {
                cell.setRowSelected(isSelected)
            }
        }
    }
}

/// 悬浮搜索条单列双行结果单元格：首行展示 32pt 图标与高亮文件名，次行展示类型、体积、修改时间与目录元数据
final class FloatingSearchResultCellView: NSTableCellView {
    static let reuseIdentifier = NSUserInterfaceItemIdentifier("FloatingSearchResultCell")

    let iconView = NSImageView()
    let nameLabel = NSTextField(labelWithString: "")
    let pathLabel = NSTextField(labelWithString: "")

    var representedPath: String?
    private(set) var isRowSelected = false

    private var nameString = ""
    private var nameRanges: [NSRange] = []
    private var pathString = ""
    private var pathRanges: [NSRange] = []
    private var isOfflineItem = false

    private static let nameFont = NSFont.systemFont(ofSize: 14.5, weight: .regular)
    private static let nameBoldFont = NSFont.systemFont(ofSize: 14.5, weight: .semibold)
    private static let pathFont = NSFont.systemFont(ofSize: 12, weight: .regular)
    private static let pathBoldFont = NSFont.systemFont(ofSize: 12, weight: .semibold)

    private static var kindDescriptionCache: [String: String] = [:]
    private static let dateFormatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateFormat = "yyyy/M/d HH:mm"
        return formatter
    }()

    private static let tailTruncateStyle: NSParagraphStyle = {
        let style = NSMutableParagraphStyle()
        style.lineBreakMode = .byTruncatingTail
        return style
    }()

    static func clearLocalizationCaches() {
        kindDescriptionCache.removeAll()
    }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        setupViews()
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        setupViews()
    }

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

    func setRowSelected(_ selected: Bool) {
        isRowSelected = selected
        renderText()
    }

    func configure(result: FfiSearchResult, icon: NSImage?, isOffline: Bool) {
        representedPath = result.path
        isOfflineItem = isOffline

        iconView.image = icon

        nameString = result.name
        nameRanges = SearchHighlightCellView.nsRanges(
            in: result.name,
            byteRanges: result.matchPositions.map { (Int($0.start), Int($0.end)) }
        )

        let (metadataLine, mappedParentRanges) = buildMetadataLine(for: result, isOffline: isOffline)
        pathString = metadataLine
        pathRanges = SearchHighlightCellView.nsRanges(in: metadataLine, byteRanges: mappedParentRanges)

        let alpha: CGFloat = isOffline ? 0.55 : 1.0
        iconView.alphaValue = alpha
        nameLabel.alphaValue = alpha
        pathLabel.alphaValue = alpha

        toolTip = result.path
        renderText()
    }

    /// 纯内存组装次行紧凑元数据：`类型 · 体积 · 修改时间 · 目录名/高亮路径`
    private func buildMetadataLine(
        for result: FfiSearchResult,
        isOffline: Bool
    ) -> (String, [(start: Int, end: Int)]) {
        var prefixParts: [String] = []

        if let kind = localizedKindDescription(for: result), !kind.isEmpty {
            prefixParts.append(kind)
        }
        if !result.isDir {
            prefixParts.append(SearchResultsViewController.formatCompactFileSize(result.size))
        }
        if result.modified > 0 {
            prefixParts.append(Self.dateFormatter.string(from: Date(timeIntervalSince1970: TimeInterval(result.modified))))
        }

        let prefixText = prefixParts.joined(separator: " · ")
        let parent = (result.path as NSString).deletingLastPathComponent
        var line = prefixText
        var shiftedParentRanges: [(start: Int, end: Int)] = []

        if !parent.isEmpty {
            let separator = line.isEmpty ? "" : " · "
            let locationOffsetBytes = line.utf8.count + separator.utf8.count
            if !result.parentMatchPositions.isEmpty {
                let abbreviated = abbreviateHomePrefix(parent)
                line += separator + abbreviated.display
                shiftedParentRanges = result.parentMatchPositions.compactMap { range in
                    guard let mapped = abbreviated.displayRange(for: range) else { return nil }
                    return (mapped.start + locationOffsetBytes, mapped.end + locationOffsetBytes)
                }
            } else {
                let folderName: String
                if parent == NSHomeDirectory() {
                    folderName = "~"
                } else if parent == "/" {
                    folderName = "/"
                } else {
                    folderName = (parent as NSString).lastPathComponent
                }
                if !folderName.isEmpty {
                    line += separator + folderName
                }
            }
        }

        if isOffline {
            line += line.isEmpty ? L("search.result.offline") : " · \(L("search.result.offline"))"
        }

        return (line, shiftedParentRanges)
    }

    private func localizedKindDescription(for result: FfiSearchResult) -> String? {
        let isBundleDir = result.isDir
            && (SearchResultsViewController.needsCustomBundleIcon(path: result.path)
                || SearchResultsViewController.isPackageBundle(path: result.path))
        if result.isDir && !isBundleDir {
            let key = "__dir__"
            if let cached = Self.kindDescriptionCache[key] { return cached }
            let desc = UTType.folder.localizedDescription ?? ""
            Self.kindDescriptionCache[key] = desc
            return desc
        }

        let ext = !result.extension.isEmpty
            ? result.extension.lowercased()
            : (result.path as NSString).pathExtension.lowercased()
        let key = isBundleDir ? "__pkg_\(ext)" : "__ext_\(ext)"
        if let cached = Self.kindDescriptionCache[key] { return cached }
        let utType = SearchResultsViewController.resolvedUTType(forExtension: ext, isBundleDir: isBundleDir)
        let desc = utType.localizedDescription ?? ""
        Self.kindDescriptionCache[key] = desc
        return desc
    }

    private func setupViews() {
        iconView.translatesAutoresizingMaskIntoConstraints = false
        iconView.imageScaling = .scaleProportionallyUpOrDown
        addSubview(iconView)
        self.imageView = iconView

        configureLabel(nameLabel, font: Self.nameFont, lineBreakMode: .byTruncatingTail)
        configureLabel(pathLabel, font: Self.pathFont, lineBreakMode: .byTruncatingTail)
        nameLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        pathLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        let textContainer = NSView()
        textContainer.translatesAutoresizingMaskIntoConstraints = false
        textContainer.addSubview(nameLabel)
        textContainer.addSubview(pathLabel)
        addSubview(textContainer)
        self.textField = nameLabel

        NSLayoutConstraint.activate([
            iconView.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 20),
            iconView.centerYAnchor.constraint(equalTo: centerYAnchor),
            iconView.widthAnchor.constraint(equalToConstant: 32),
            iconView.heightAnchor.constraint(equalToConstant: 32),

            textContainer.leadingAnchor.constraint(equalTo: iconView.trailingAnchor, constant: 12),
            textContainer.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -20),
            textContainer.centerYAnchor.constraint(equalTo: centerYAnchor),

            nameLabel.topAnchor.constraint(equalTo: textContainer.topAnchor),
            nameLabel.leadingAnchor.constraint(equalTo: textContainer.leadingAnchor),
            nameLabel.trailingAnchor.constraint(equalTo: textContainer.trailingAnchor),

            pathLabel.topAnchor.constraint(equalTo: nameLabel.bottomAnchor, constant: 2),
            pathLabel.leadingAnchor.constraint(equalTo: textContainer.leadingAnchor),
            pathLabel.trailingAnchor.constraint(equalTo: textContainer.trailingAnchor),
            pathLabel.bottomAnchor.constraint(equalTo: textContainer.bottomAnchor),
        ])
    }

    private func configureLabel(_ label: NSTextField, font: NSFont, lineBreakMode: NSLineBreakMode) {
        label.translatesAutoresizingMaskIntoConstraints = false
        label.font = font
        label.lineBreakMode = lineBreakMode
        label.maximumNumberOfLines = 1
        label.cell?.wraps = false
        label.cell?.isScrollable = false
        label.isEditable = false
        label.isBezeled = false
        label.drawsBackground = false
    }

    private func renderText() {
        let isDark = effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua

        let nameBaseColor: NSColor
        let nameHighlightColor: NSColor
        let pathBaseColor: NSColor
        let pathHighlightColor: NSColor

        if isRowSelected {
            nameBaseColor = NSColor.white.withAlphaComponent(0.94)
            nameHighlightColor = .white
            pathBaseColor = NSColor.white.withAlphaComponent(0.80)
            pathHighlightColor = .white
        } else {
            nameBaseColor = .labelColor
            nameHighlightColor = isDark ? .controlAccentColor : .labelColor
            pathBaseColor = .secondaryLabelColor
            pathHighlightColor = isDark ? .controlAccentColor : .labelColor
        }

        applyHighlightedText(
            to: nameLabel,
            text: nameString,
            ranges: nameRanges,
            regularFont: Self.nameFont,
            boldFont: Self.nameBoldFont,
            baseColor: nameBaseColor,
            highlightColor: nameHighlightColor,
            paragraphStyle: Self.tailTruncateStyle
        )

        applyHighlightedText(
            to: pathLabel,
            text: pathString,
            ranges: pathRanges,
            regularFont: Self.pathFont,
            boldFont: Self.pathBoldFont,
            baseColor: pathBaseColor,
            highlightColor: pathHighlightColor,
            paragraphStyle: Self.tailTruncateStyle
        )
    }

    private func applyHighlightedText(
        to label: NSTextField,
        text: String,
        ranges: [NSRange],
        regularFont: NSFont,
        boldFont: NSFont,
        baseColor: NSColor,
        highlightColor: NSColor,
        paragraphStyle: NSParagraphStyle
    ) {
        guard !text.isEmpty else {
            label.stringValue = ""
            return
        }
        if ranges.isEmpty {
            label.font = regularFont
            label.textColor = baseColor
            label.stringValue = text
            return
        }
        let attributed = NSMutableAttributedString(
            string: text,
            attributes: [
                .font: regularFont,
                .foregroundColor: baseColor,
                .paragraphStyle: paragraphStyle,
            ]
        )
        for range in ranges {
            attributed.addAttributes([
                .font: boldFont,
                .foregroundColor: highlightColor,
            ], range: range)
        }
        label.attributedStringValue = attributed
    }
}
