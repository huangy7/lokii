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

// MARK: - Interactive Path Row (Non-Blocking Volume & Local Status)
private struct SettingsPathRow: View {
    let icon: String
    let path: String
    let availabilityState: SettingsPathAvailabilityState
    var showVolumeMetrics: Bool = true
    var onRescan: (() -> Void)? = nil
    var onClearCache: (() -> Void)? = nil
    let onRemove: () -> Void

    @State private var isHovered = false

    private var rowIconName: String {
        switch availabilityState {
        case let .volumeOnline(_, _, _, kind),
             let .volumeSyncing(kind),
             let .volumeOffline(_, kind):
            if kind == "network" {
                return "externaldrive.connected.to.line.below"
            } else if kind == "local_external" {
                return "externaldrive"
            }
            return icon
        case .volumeUncachedOffline:
            return "externaldrive"
        case .localAvailable, .localChecking, .localMissing:
            return icon
        }
    }

    private var iconColor: Color {
        switch availabilityState {
        case .volumeOnline, .volumeSyncing, .localAvailable:
            return .accentColor
        case .volumeOffline, .volumeUncachedOffline, .localChecking, .localMissing:
            return .secondary
        }
    }

    private var rowIconHelp: String? {
        switch availabilityState {
        case let .volumeOnline(_, _, _, kind),
             let .volumeSyncing(kind),
             let .volumeOffline(_, kind):
            if kind == "network" {
                return L("prefs.volume.kind.network")
            } else if kind == "local_external" {
                return L("prefs.volume.kind.localExternal")
            }
            return nil
        case .volumeUncachedOffline, .localAvailable, .localChecking, .localMissing:
            return nil
        }
    }

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: rowIconName)
                .foregroundStyle(iconColor)
                .font(.system(size: 12))
                .help(rowIconHelp ?? "")

            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(path)
                        .font(.system(size: 11, design: .monospaced))
                        .lineLimit(1)
                        .truncationMode(.middle)

                    statusBadge
                }

                if case let .volumeOnline(_, activePath, isRemounted, _) = availabilityState,
                   isRemounted, let activePath {
                    Text(String(format: L("prefs.volume.status.remounted"), activePath))
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }

            Spacer(minLength: 6)

            actionControl

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
    }

    @ViewBuilder
    private var statusBadge: some View {
        switch availabilityState {
        case let .volumeOnline(entryCount, _, _, _):
            if showVolumeMetrics {
                HStack(spacing: 4) {
                    Circle()
                        .fill(Color.green.opacity(0.85))
                        .frame(width: 6, height: 6)
                    Text(String(format: L("prefs.volume.status.online"), formatFileCount(entryCount)))
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary)
                        .fixedSize()
                }
            }
        case .volumeSyncing:
            if showVolumeMetrics {
                HStack(spacing: 4) {
                    ProgressView()
                        .controlSize(.mini)
                    Text(L("prefs.volume.status.syncing"))
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary)
                        .fixedSize()
                }
            }
        case let .volumeOffline(entryCount, _):
            HStack(spacing: 4) {
                Circle()
                    .fill(Color.secondary.opacity(0.6))
                    .frame(width: 6, height: 6)
                let label = (showVolumeMetrics && entryCount > 0)
                    ? String(format: L("prefs.volume.status.offline"), formatFileCount(entryCount))
                    : L("prefs.volume.status.offlineNoCount")
                Text(label)
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
                    .fixedSize()
            }
        case .volumeUncachedOffline:
            HStack(spacing: 4) {
                Circle()
                    .fill(Color.secondary.opacity(0.45))
                    .frame(width: 6, height: 6)
                Text(L("prefs.volume.state.offlineUncached"))
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
                    .fixedSize()
            }
        case .localMissing:
            HStack(spacing: 3) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.system(size: 9))
                    .foregroundStyle(.orange)
                Text(L("prefs.index.pathNotFound"))
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
                    .fixedSize()
            }
            .help(L("prefs.index.pathNotFound.help"))
        case .localChecking:
            Text(L("prefs.index.pathChecking"))
                .font(.system(size: 10))
                .foregroundStyle(.tertiary)
                .fixedSize()
        case .localAvailable:
            EmptyView()
        }
    }

    @ViewBuilder
    private var actionControl: some View {
        switch availabilityState {
        case .volumeOnline:
            if let onRescan {
                Button(L("prefs.volume.rescan"), action: onRescan)
                    .buttonStyle(.borderless)
                    .font(.system(size: 10, weight: .medium))
                    .foregroundStyle(Color.accentColor)
                    .fixedSize()
            }
        case let .volumeOffline(entryCount, _):
            if entryCount > 0, let onClearCache {
                Button(L("prefs.volume.clearCache"), action: onClearCache)
                    .buttonStyle(.borderless)
                    .font(.system(size: 10, weight: .medium))
                    .foregroundStyle(.secondary)
                    .fixedSize()
            }
        case .volumeSyncing, .volumeUncachedOffline, .localAvailable, .localChecking, .localMissing:
            EmptyView()
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
    let isPathOffline: (String) -> Bool
    let onCommit: (String) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var text = ""
    @State private var unconfirmedMissingPath: String?
    @State private var isProbingPath = false
    @State private var probeTask: Task<Void, Never>?
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
                .disabled(isProbingPath)
                .onSubmit {
                    handleSubmit()
                }
                .onChange(of: text) { _ in
                    probeTask?.cancel()
                    probeTask = nil
                    isProbingPath = false
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
                    probeTask?.cancel()
                    probeTask = nil
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
                .disabled(trimmedValue.isEmpty || isProbingPath)
            }
        }
        .padding(20)
        .frame(width: 380)
        .onAppear {
            isFieldFocused = true
        }
        .onDisappear {
            probeTask?.cancel()
            probeTask = nil
        }
    }

    private func handleSubmit() {
        let value = trimmedValue
        guard !value.isEmpty, !isProbingPath else { return }

        if mode.validatesDirectoryExistence && unconfirmedMissingPath != value {
            let expanded = SettingsPathStatusResolver.normalizePath(value)
            if isPathOffline(expanded) {
                unconfirmedMissingPath = value
                return
            }

            // 后台线程探测目录存在性，并在取消或关闭面板时立即中止任务，避免迟到回调误提交
            isProbingPath = true
            probeTask?.cancel()
            probeTask = Task { @MainActor in
                let exists = await Task.detached(priority: .userInitiated) {
                    PreferencesPathNormalizer.directoryExists(at: value)
                }.value
                guard !Task.isCancelled else { return }
                isProbingPath = false
                probeTask = nil
                if exists {
                    onCommit(value)
                    dismiss()
                } else {
                    unconfirmedMissingPath = value
                }
            }
            return
        }

        onCommit(value)
        dismiss()
    }
}

// MARK: - Managed Volume Row
private struct ManagedVolumeRow: View {
    let item: ManagedVolumeItem
    let onToggle: (Bool) -> Void
    let onRescan: () -> Void
    let onClearCache: () -> Void

    private var iconName: String {
        item.kind == "network" ? "externaldrive.connected.to.line.below" : "externaldrive"
    }

    private var iconColor: Color {
        if item.isIndexed && item.isMounted {
            return .accentColor
        }
        return .secondary
    }

    private var kindTooltip: String {
        item.kind == "network" ? L("prefs.volume.kind.network") : L("prefs.volume.kind.localExternal")
    }

    var body: some View {
        HStack(alignment: .center, spacing: 10) {
            Image(systemName: iconName)
                .foregroundStyle(iconColor)
                .font(.system(size: 13))
                .frame(width: 18)
                .help(kindTooltip)

            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(item.displayName)
                        .lineLimit(1)
                        .truncationMode(.middle)

                    statusBadge
                }

                if let status = item.status, status.isRemounted, let activePath = status.activePath, !activePath.isEmpty {
                    Text(String(format: L("prefs.volume.status.remounted"), activePath))
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                } else {
                    Text(item.configuredPath)
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }

            Spacer(minLength: 8)

            actionControl

            Toggle("", isOn: Binding(
                get: { item.isIndexed },
                set: { onToggle($0) }
            ))
            .toggleStyle(.switch)
            .labelsHidden()
            .controlSize(.small)
            .accessibilityLabel(item.displayName)
        }
        .padding(.vertical, 2)
    }

    @ViewBuilder
    private var statusBadge: some View {
        if item.isIndexed != item.isSavedIndexed {
            Text(L("prefs.volume.status.pendingSave"))
                .font(.system(size: 10))
                .foregroundStyle(.orange)
                .fixedSize()
        } else if !item.isIndexed {
            Text(L("prefs.volume.status.notIndexed"))
                .font(.system(size: 10))
                .foregroundStyle(.tertiary)
                .fixedSize()
        } else if let status = item.status {
            switch status.state {
            case .online:
                HStack(spacing: 4) {
                    Circle()
                        .fill(Color.green.opacity(0.85))
                        .frame(width: 6, height: 6)
                    Text(String(format: L("prefs.volume.status.online"), formatFileCount(status.entryCount)))
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary)
                        .fixedSize()
                }
            case .syncing:
                HStack(spacing: 4) {
                    ProgressView()
                        .controlSize(.mini)
                    Text(L("prefs.volume.status.syncing"))
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary)
                        .fixedSize()
                }
            case .offline:
                HStack(spacing: 4) {
                    Circle()
                        .fill(Color.secondary.opacity(0.6))
                        .frame(width: 6, height: 6)
                    let label = status.entryCount > 0
                        ? String(format: L("prefs.volume.status.offline"), formatFileCount(status.entryCount))
                        : L("prefs.volume.status.offlineNoCount")
                    Text(label)
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary)
                        .fixedSize()
                }
            case .missing:
                HStack(spacing: 4) {
                    Circle()
                        .fill(Color.secondary.opacity(0.45))
                        .frame(width: 6, height: 6)
                    Text(L("prefs.volume.state.offlineUncached"))
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary)
                        .fixedSize()
                }
            }
        } else if item.isMounted {
            HStack(spacing: 4) {
                ProgressView()
                    .controlSize(.mini)
                Text(L("prefs.volume.status.syncing"))
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
                    .fixedSize()
            }
        } else {
            HStack(spacing: 4) {
                Circle()
                    .fill(Color.secondary.opacity(0.45))
                    .frame(width: 6, height: 6)
                Text(L("prefs.volume.state.offlineUncached"))
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
                    .fixedSize()
            }
        }
    }

    @ViewBuilder
    private var actionControl: some View {
        if item.isIndexed && item.isSavedIndexed, let status = item.status {
            switch status.state {
            case .online:
                Button(L("prefs.volume.rescan"), action: onRescan)
                    .buttonStyle(.borderless)
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(Color.accentColor)
                    .fixedSize()
            case .offline where status.entryCount > 0:
                Button(L("prefs.volume.clearCache"), action: onClearCache)
                    .buttonStyle(.borderless)
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(.secondary)
                    .fixedSize()
            default:
                EmptyView()
            }
        }
    }
}

// MARK: - Index Preferences Pane
struct PreferencesIndexPane: View {
    @ObservedObject var viewModel: PreferencesViewModel
    @State private var isPresetsExpanded = false
    @State private var isPatternPresetsExpanded = false
    @State private var activeInputSheet: IndexInputSheetMode?
    @State private var pendingClearCachePath: String? = nil

    var body: some View {
        VStack(spacing: 0) {
            if let errorMessage = viewModel.volumeActionErrorMessage {
                HStack(spacing: 8) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange)
                        .font(.system(size: 12))
                    Text("\(L("prefs.volume.errorTitle")): \(errorMessage)")
                        .font(.system(size: 11))
                        .foregroundStyle(.primary)
                        .lineLimit(2)
                    Spacer()
                    Button {
                        viewModel.volumeActionErrorMessage = nil
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .foregroundStyle(.secondary)
                            .font(.system(size: 12))
                    }
                    .buttonStyle(.plain)
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 8)
                .background(Color.orange.opacity(0.12))
                Divider()
            }

            Form {
                // Section 1: 索引目录 (Index Paths)
                Section {
                    if viewModel.nonVolumeIndexPaths.isEmpty {
                        Text(L("prefs.indexPaths.empty"))
                            .font(.system(size: 11))
                            .foregroundStyle(.secondary)
                            .frame(maxWidth: .infinity, alignment: .center)
                            .padding(.vertical, 4)
                    } else {
                        ForEach(viewModel.nonVolumeIndexPaths, id: \.self) { path in
                            let exactStatus = viewModel.exactSavedVolumeStatus(for: path)
                            SettingsPathRow(
                                icon: "folder.fill",
                                path: path,
                                availabilityState: viewModel.availabilityState(for: path),
                                onRescan: exactStatus.map { status in
                                    { viewModel.rescanVolume(path: status.volumeId) }
                                },
                                onClearCache: exactStatus.map { status in
                                    { pendingClearCachePath = status.volumeId }
                                },
                                onRemove: {
                                    viewModel.indexPaths.removeAll(where: { $0 == path })
                                }
                            )
                        }
                    }
                } header: {
                    SettingsSectionHeader(L("prefs.index.group.paths")) {
                        Menu {
                            Button {
                                chooseDirectories { normalized in
                                    let normalizedKey = SettingsPathStatusResolver.normalizePath(normalized).lowercased()
                                    if !viewModel.indexPaths.contains(where: { SettingsPathStatusResolver.normalizePath($0).lowercased() == normalizedKey }) {
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

                // Section 2: 外置磁盘与网络卷 (Mounted & Offline Volumes)
                Section {
                    if viewModel.managedVolumes.isEmpty {
                        HStack(spacing: 6) {
                            Image(systemName: "externaldrive")
                                .foregroundStyle(.secondary.opacity(0.6))
                                .font(.system(size: 12))
                            Text(L("prefs.volume.empty"))
                                .font(.system(size: 11))
                                .foregroundStyle(.secondary)
                        }
                    } else {
                        ForEach(viewModel.managedVolumes) { volume in
                            ManagedVolumeRow(
                                item: volume,
                                onToggle: { enabled in
                                    viewModel.setVolumeIndexed(volume, enabled: enabled)
                                },
                                onRescan: {
                                    viewModel.rescanVolume(path: volume.configuredPath)
                                },
                                onClearCache: {
                                    pendingClearCachePath = volume.configuredPath
                                }
                            )
                        }
                    }

                    SettingsToggleRow(
                        L("prefs.volume.includeOffline"),
                        description: L("prefs.volume.includeOffline.description"),
                        isOn: $viewModel.includeOfflineResults
                    )

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
                } header: {
                    SettingsSectionHeader(
                        L("prefs.index.group.volumes"),
                        description: L("prefs.index.group.volumes.description")
                    )
                }

                // Section 3: 排除路径 (Exclude Paths)
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
                            SettingsPathRow(
                                icon: "folder.badge.minus",
                                path: path,
                                availabilityState: viewModel.availabilityState(for: path),
                                showVolumeMetrics: false,
                                onRemove: {
                                    viewModel.excludePaths.removeAll(where: { $0 == path })
                                }
                            )
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
                                    let normalizedKey = SettingsPathStatusResolver.normalizePath(normalized).lowercased()
                                    if !viewModel.excludePaths.contains(where: { SettingsPathStatusResolver.normalizePath($0).lowercased() == normalizedKey }) {
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
                }
            }
            .formStyle(.grouped)

            indexActionBar
        }
        .sheet(item: $activeInputSheet) { mode in
            PreferencesTextInputSheet(
                mode: mode,
                isPathOffline: { expanded in
                    viewModel.engine.isPathOffline(expanded)
                }
            ) { value in
                switch mode {
                case .indexPath:
                    let key = SettingsPathStatusResolver.normalizePath(value).lowercased()
                    if !viewModel.indexPaths.contains(where: { SettingsPathStatusResolver.normalizePath($0).lowercased() == key }) {
                        viewModel.indexPaths.append(value)
                    }
                case .excludePath:
                    let key = SettingsPathStatusResolver.normalizePath(value).lowercased()
                    if !viewModel.excludePaths.contains(where: { SettingsPathStatusResolver.normalizePath($0).lowercased() == key }) {
                        viewModel.excludePaths.append(value)
                    }
                case .excludePattern:
                    if !viewModel.excludePatterns.contains(value) {
                        viewModel.excludePatterns.append(value)
                    }
                }
            }
        }
        .alert(
            L("prefs.volume.clearCacheConfirm.title"),
            isPresented: Binding(
                get: { pendingClearCachePath != nil },
                set: { if !$0 { pendingClearCachePath = nil } }
            ),
            presenting: pendingClearCachePath
        ) { targetPath in
            Button(L("prefs.volume.clearCacheConfirm.confirm"), role: .destructive) {
                viewModel.clearVolumeCache(path: targetPath)
                pendingClearCachePath = nil
            }
            Button(L("prefs.index.pathConfirm.cancel"), role: .cancel) {
                pendingClearCachePath = nil
            }
        } message: { targetPath in
            Text(String(format: L("prefs.volume.clearCacheConfirm.message"), targetPath))
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
                } else if let errorMessage = viewModel.indexSaveErrorMessage {
                    HStack(spacing: 4) {
                        Image(systemName: "xmark.circle.fill")
                            .foregroundStyle(.red)
                            .font(.system(size: 11))
                        Text(errorMessage)
                            .font(.system(size: 11))
                            .foregroundStyle(.red)
                    }
                } else if viewModel.isRebuildingIndex {
                    HStack(spacing: 6) {
                        ProgressView()
                            .controlSize(.small)
                        Text(
                            viewModel.rebuildScannedCount > 0
                                ? String(format: L("search.indexingCount"), formatFileCount(viewModel.rebuildScannedCount))
                                : L("search.indexing")
                        )
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
