import SwiftUI
import AppKit

// MARK: - Path Normalization Helper
private enum PreferencesPathNormalizer {
    static func normalize(_ raw: String) -> String {
        var path = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        let home = NSHomeDirectory()
        if path.hasPrefix("~/") {
            // 保持波浪号缩写
        } else if path == home {
            path = "~"
        } else if path.hasPrefix(home + "/") {
            path = "~" + path.dropFirst(home.count)
        }
        while path.count > 1 && path.hasSuffix("/") {
            path.removeLast()
        }
        return path
    }

    static func directoryExists(at path: String) -> Bool {
        var isDir: ObjCBool = false
        let expanded = (path as NSString).expandingTildeInPath
        return FileManager.default.fileExists(atPath: expanded, isDirectory: &isDir) && isDir.boolValue
    }
}

// MARK: - Interactive Path Row (Async Existence Check)
private struct SettingsPathRow: View {
    let icon: String
    let path: String
    let onRemove: () -> Void

    @State private var isHovered = false
    /// 默认按可用渲染并在后台异步探测，避免主线程重绘或掉线网络卷 stat 引发界面卡顿
    @State private var isPathAvailable = true

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: icon)
                .foregroundStyle(isPathAvailable ? Color.accentColor : Color.secondary)
                .font(.system(size: 12))
            Text(path)
                .font(.system(size: 11, design: .monospaced))
                .lineLimit(1)
                .truncationMode(.middle)

            if !isPathAvailable {
                HStack(spacing: 3) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .font(.system(size: 9))
                        .foregroundStyle(.orange)
                    Text(L("prefs.index.pathNotFound"))
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary)
                }
                .help(L("prefs.index.pathNotFound.help"))
            }

            Spacer()
            Button(action: onRemove) {
                Image(systemName: "xmark.circle.fill")
                    .foregroundStyle(isHovered ? Color.red.opacity(0.85) : Color.secondary.opacity(0.35))
                    .font(.system(size: 13))
            }
            .buttonStyle(.plain)
            .opacity(isHovered ? 1.0 : 0.25)
            .animation(.easeInOut(duration: 0.15), value: isHovered)
        }
        .padding(.vertical, 1)
        .contentShape(Rectangle())
        .onHover { hovering in
            isHovered = hovering
        }
        .task(id: path) {
            let targetPath = path
            let exists = await Task.detached(priority: .utility) {
                PreferencesPathNormalizer.directoryExists(at: targetPath)
            }.value
            guard !Task.isCancelled else { return }
            isPathAvailable = exists
        }
    }
}

// MARK: - Native SwiftUI Input Sheet
private enum IndexInputSheetMode: String, Identifiable {
    case indexPath
    case excludePath
    case excludePattern

    var id: String { rawValue }

    var title: String {
        switch self {
        case .indexPath: return L("prefs.index.indexPaths.inputTitle")
        case .excludePath: return L("prefs.index.excludePaths.inputTitle")
        case .excludePattern: return L("prefs.excludePatterns.editor.addTitle")
        }
    }

    var message: String {
        switch self {
        case .indexPath: return L("prefs.index.indexPaths.inputMessage")
        case .excludePath: return L("prefs.index.excludePaths.inputMessage")
        case .excludePattern: return L("prefs.excludePatterns.editor.message")
        }
    }

    var placeholder: String {
        switch self {
        case .indexPath: return L("prefs.index.indexPaths.inputPlaceholder")
        case .excludePath: return L("prefs.index.excludePaths.inputPlaceholder")
        case .excludePattern: return L("prefs.excludePatterns.editor.placeholder")
        }
    }

    var validatesDirectoryExistence: Bool {
        self == .indexPath || self == .excludePath
    }
}

private struct PreferencesTextInputSheet: View {
    let mode: IndexInputSheetMode
    let onCommit: (String) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var text = ""
    @State private var unconfirmedMissingPath: String?
    @FocusState private var isFieldFocused: Bool

    private var trimmedValue: String {
        if mode.validatesDirectoryExistence {
            return PreferencesPathNormalizer.normalize(text)
        }
        return text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 4) {
                Text(mode.title)
                    .font(.headline)
                Text(mode.message)
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            TextField(mode.placeholder, text: $text)
                .textFieldStyle(.roundedBorder)
                .focused($isFieldFocused)
                .onSubmit {
                    handleSubmit()
                }
                .onChange(of: text) { _ in
                    if unconfirmedMissingPath != nil {
                        unconfirmedMissingPath = nil
                    }
                }

            if let missingPath = unconfirmedMissingPath {
                HStack(alignment: .top, spacing: 6) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange)
                        .font(.system(size: 12))
                        .padding(.top, 1)
                    Text(String(format: L("prefs.index.pathConfirm.message"), missingPath))
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .padding(8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Color.orange.opacity(0.08))
                .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
            }

            HStack {
                Spacer()
                Button(L("prefs.excludePatterns.editor.cancel")) {
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)

                Button(unconfirmedMissingPath != nil
                       ? L("prefs.index.pathConfirm.addAnyway")
                       : L("prefs.excludePatterns.editor.confirm")) {
                    handleSubmit()
                }
                .keyboardShortcut(.defaultAction)
                .buttonStyle(.borderedProminent)
                .disabled(trimmedValue.isEmpty)
            }
        }
        .padding(20)
        .frame(width: 380)
        .onAppear {
            isFieldFocused = true
        }
    }

    private func handleSubmit() {
        let value = trimmedValue
        guard !value.isEmpty else { return }

        if mode.validatesDirectoryExistence && unconfirmedMissingPath != value {
            if !PreferencesPathNormalizer.directoryExists(at: value) {
                unconfirmedMissingPath = value
                return
            }
        }

        onCommit(value)
        dismiss()
    }
}

// MARK: - Index Preferences Pane
struct PreferencesIndexPane: View {
    @ObservedObject var viewModel: PreferencesViewModel
    @State private var isPresetsExpanded = false
    @State private var isPatternPresetsExpanded = false
    @State private var activeInputSheet: IndexInputSheetMode?

    var body: some View {
        VStack(spacing: 0) {
            Form {
                // Section 1: 索引目录 (Index Paths)
                Section {
                    if viewModel.indexPaths.isEmpty {
                        Text(L("prefs.indexPaths.empty"))
                            .font(.system(size: 11))
                            .foregroundStyle(.secondary)
                            .frame(maxWidth: .infinity, alignment: .center)
                            .padding(.vertical, 4)
                    } else {
                        ForEach(viewModel.indexPaths, id: \.self) { path in
                            SettingsPathRow(icon: "folder.fill", path: path) {
                                viewModel.indexPaths.removeAll(where: { $0 == path })
                            }
                        }
                    }
                } header: {
                    SettingsSectionHeader(L("prefs.index.group.paths")) {
                        Menu {
                            Button {
                                chooseDirectories { normalized in
                                    if !viewModel.indexPaths.contains(normalized) {
                                        viewModel.indexPaths.append(normalized)
                                    }
                                }
                            } label: {
                                Label(L("prefs.index.excludePaths.chooseFolder"), systemImage: "folder.badge.plus")
                            }
                            Button {
                                activeInputSheet = .indexPath
                            } label: {
                                Label(L("prefs.index.excludePaths.manualInput"), systemImage: "character.cursor.ibeam")
                            }
                        } label: {
                            Label(L("prefs.index.addPath"), systemImage: "plus")
                        }
                        .menuStyle(.borderedButton)
                        .menuIndicator(.hidden)
                        .controlSize(.small)
                    }
                }

                // Section 2: 排除路径 (Exclude Paths)
                Section {
                    if viewModel.customExcludePaths.isEmpty {
                        HStack(spacing: 6) {
                            Image(systemName: "folder")
                                .foregroundStyle(.secondary.opacity(0.6))
                                .font(.system(size: 12))
                            Text(L("prefs.index.excludePaths.empty"))
                                .font(.system(size: 11))
                                .foregroundStyle(.secondary)
                        }
                    } else {
                        ForEach(viewModel.customExcludePaths, id: \.self) { path in
                            SettingsPathRow(icon: "folder.badge.minus", path: path) {
                                viewModel.excludePaths.removeAll(where: { $0 == path })
                            }
                        }
                    }

                    if !viewModel.presetExcludePaths.isEmpty {
                        DisclosureGroup(isExpanded: $isPresetsExpanded) {
                            FlowLayout(spacing: 6) {
                                ForEach(viewModel.presetExcludePaths, id: \.self) { path in
                                    PillTagView(title: path) {
                                        viewModel.excludePaths.removeAll(where: { $0 == path })
                                    }
                                }
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.top, 6)
                            .padding(.bottom, 2)
                        } label: {
                            HStack(spacing: 6) {
                                Image(systemName: "shield.lefthalf.filled")
                                    .foregroundStyle(Color.accentColor)
                                    .font(.system(size: 11))
                                Text(String(format: L("prefs.index.excludePaths.systemPresets.count"), viewModel.presetExcludePaths.count))
                                    .font(.system(size: 11, weight: .medium))
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                } header: {
                    SettingsSectionHeader(
                        L("prefs.index.group.excludePaths"),
                        description: L("prefs.index.group.excludePaths.description")
                    ) {
                        Menu {
                            Button {
                                chooseDirectories { normalized in
                                    if !viewModel.excludePaths.contains(normalized) {
                                        viewModel.excludePaths.append(normalized)
                                    }
                                }
                            } label: {
                                Label(L("prefs.index.excludePaths.chooseFolder"), systemImage: "folder.badge.plus")
                            }
                            Button {
                                activeInputSheet = .excludePath
                            } label: {
                                Label(L("prefs.index.excludePaths.manualInput"), systemImage: "character.cursor.ibeam")
                            }
                        } label: {
                            Label(L("prefs.index.addExcludePath"), systemImage: "plus")
                        }
                        .menuStyle(.borderedButton)
                        .menuIndicator(.hidden)
                        .controlSize(.small)
                    }
                }

                // Section 3: 排除名称规则 (Exclude Patterns - Pill Tags)
                Section {
                    if viewModel.customExcludePatterns.isEmpty {
                        HStack(spacing: 6) {
                            Image(systemName: "tag")
                                .foregroundStyle(.secondary.opacity(0.6))
                                .font(.system(size: 12))
                            Text(L("prefs.index.excludePatterns.empty"))
                                .font(.system(size: 11))
                                .foregroundStyle(.secondary)
                        }
                    } else {
                        FlowLayout(spacing: 6) {
                            ForEach(viewModel.customExcludePatterns, id: \.self) { pattern in
                                PillTagView(title: pattern) {
                                    viewModel.excludePatterns.removeAll(where: { $0 == pattern })
                                }
                            }
                            AddPillButton(title: L("prefs.index.excludePatterns.add")) {
                                activeInputSheet = .excludePattern
                            }
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.vertical, 2)
                    }

                    if !viewModel.presetExcludePatterns.isEmpty {
                        DisclosureGroup(isExpanded: $isPatternPresetsExpanded) {
                            FlowLayout(spacing: 6) {
                                ForEach(viewModel.presetExcludePatterns, id: \.self) { pattern in
                                    PillTagView(title: pattern) {
                                        viewModel.excludePatterns.removeAll(where: { $0 == pattern })
                                    }
                                }
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.top, 6)
                            .padding(.bottom, 2)
                        } label: {
                            HStack(spacing: 6) {
                                Image(systemName: "shield.lefthalf.filled")
                                    .foregroundStyle(Color.accentColor)
                                    .font(.system(size: 11))
                                Text(String(format: L("prefs.index.excludePatterns.systemPresets.count"), viewModel.presetExcludePatterns.count))
                                    .font(.system(size: 11, weight: .medium))
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                } header: {
                    SettingsSectionHeader(
                        L("prefs.index.group.excludes"),
                        description: L("prefs.excludePatterns.description")
                    ) {
                        Button {
                            activeInputSheet = .excludePattern
                        } label: {
                            Label(L("prefs.index.excludePatterns.add"), systemImage: "plus")
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                    }
                }

                // Section 4: 扫描选项
                Section(L("prefs.index.group.options")) {
                    SettingsToggleRow(
                        L("prefs.index.applications"),
                        description: viewModel.appIndexConflicted
                            ? L("prefs.index.applications.conflict")
                            : L("prefs.index.applications.description"),
                        isOn: $viewModel.indexApps
                    )

                    SettingsToggleRow(
                        L("prefs.includeHidden"),
                        description: L("prefs.includeHidden.description"),
                        isOn: $viewModel.includeHidden.animation(.easeInOut(duration: 0.15))
                    )

                    if viewModel.includeHidden {
                        SettingsToggleRow(
                            L("prefs.excludeSensitive"),
                            description: L("prefs.excludeSensitive.description"),
                            isOn: $viewModel.excludeSensitive
                        )
                        .padding(.leading, 16)
                    }

                    HStack(alignment: .center, spacing: 14) {
                        SettingsRowLabel(
                            L("prefs.networkRescan"),
                            description: L("prefs.networkRescan.description")
                        )
                        Spacer(minLength: 12)
                        SettingsNumberStepper(
                            value: $viewModel.networkRescanMinutes,
                            range: 0...120,
                            step: 1,
                            width: 82,
                            unit: viewModel.networkRescanMinutes == 0
                                ? L("prefs.networkRescan.off")
                                : L("prefs.networkRescan.unit")
                        )
                    }
                }
            }
            .formStyle(.grouped)

            indexActionBar
        }
        .sheet(item: $activeInputSheet) { mode in
            PreferencesTextInputSheet(mode: mode) { value in
                switch mode {
                case .indexPath:
                    if !viewModel.indexPaths.contains(value) {
                        viewModel.indexPaths.append(value)
                    }
                case .excludePath:
                    if !viewModel.excludePaths.contains(value) {
                        viewModel.excludePaths.append(value)
                    }
                case .excludePattern:
                    if !viewModel.excludePatterns.contains(value) {
                        viewModel.excludePatterns.append(value)
                    }
                }
            }
        }
    }

    // MARK: - Bottom Action Bar
    private var indexActionBar: some View {
        VStack(spacing: 0) {
            Divider()
            HStack(spacing: 10) {
                if viewModel.isIndexDirty {
                    HStack(spacing: 4) {
                        Image(systemName: "exclamationmark.circle.fill")
                            .foregroundStyle(.orange)
                            .font(.system(size: 11))
                        Text(L("prefs.index.unsaved"))
                            .font(.system(size: 11))
                            .foregroundStyle(.secondary)
                    }
                }
                Spacer()

                Button(L("prefs.resetToDefaults")) {
                    viewModel.restoreIndexDefaults()
                }
                .buttonStyle(.bordered)
                .disabled(!viewModel.isIndexDirty && viewModel.isDefaultIndexState)

                Button(L("prefs.save")) {
                    _ = viewModel.saveIndexChanges()
                }
                .buttonStyle(.borderedProminent)
                .disabled(!viewModel.isIndexDirty)
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 10)
            .background(.bar)
        }
    }

    // MARK: - Directory Panel
    private func chooseDirectories(onSelect: @escaping (String) -> Void) {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = true

        let handleURLs = { (urls: [URL]) in
            for url in urls {
                onSelect(PreferencesPathNormalizer.normalize(url.path))
            }
        }

        if let hostWindow = NSApp.keyWindow {
            panel.beginSheetModal(for: hostWindow) { response in
                guard response == .OK else { return }
                handleURLs(panel.urls)
            }
        } else if panel.runModal() == .OK {
            handleURLs(panel.urls)
        }
    }
}
