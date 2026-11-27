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

private func volumeConfirmReasonTooltip(_ reason: FfiVolumeConfirmReason?) -> String {
    switch reason ?? .initialScan {
    case .initialScan:
        return L("prefs.volume.confirmReason.initialScan")
    case .filterWidened:
        return L("prefs.volume.confirmReason.filterWidened")
    case .journalInvalidated:
        return L("prefs.volume.confirmReason.journalInvalidated")
    case .schemaUpgrade:
        return L("prefs.volume.confirmReason.schemaUpgrade")
    }
}

// MARK: - Interactive Path Row (Non-Blocking Volume & Local Status)
private struct SettingsPathRow: View {
    let icon: String
    let path: String
    let availabilityState: SettingsPathAvailabilityState
    var showVolumeMetrics: Bool = true
    var onRescan: (() -> Void)? = nil
    var onConfirmScan: (() -> Void)? = nil
    var onCancelScan: (() -> Void)? = nil
    var onClearCache: (() -> Void)? = nil
    var conflict: (label: String, help: String)? = nil
    let onRemove: () -> Void

    @State private var isHovered = false

    private var rowIconName: String {
        switch availabilityState {
        case let .volumeOnline(_, _, _, kind, _),
             let .volumeSyncing(kind, _),
             let .volumeNeedsConfirmation(_, _, _, kind, _),
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
        case .volumeNeedsConfirmation:
            return .orange
        case .volumeOffline, .volumeUncachedOffline, .localChecking, .localMissing:
            return .secondary
        }
    }

    private var rowIconHelp: String? {
        switch availabilityState {
        case let .volumeOnline(_, _, _, kind, _),
             let .volumeSyncing(kind, _),
             let .volumeNeedsConfirmation(_, _, _, kind, _),
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

    private var remountedActivePath: String? {
        switch availabilityState {
        case let .volumeOnline(_, activePath, isRemounted, _, _),
             let .volumeNeedsConfirmation(_, activePath, isRemounted, _, _):
            return isRemounted ? activePath : nil
        default:
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

                    if let conflict {
                        HStack(spacing: 3) {
                            Image(systemName: "exclamationmark.triangle.fill")
                                .font(.system(size: 9))
                                .foregroundStyle(.orange)
                            Text(conflict.label)
                                .font(.system(size: 10))
                                .foregroundStyle(.secondary)
                                .fixedSize()
                        }
                        .help(conflict.help)
                    }
                }

                if let activePath = remountedActivePath, !activePath.isEmpty {
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
        case let .volumeOnline(entryCount, _, _, kind, supportsPersistentJournal):
            if showVolumeMetrics {
                HStack(spacing: 4) {
                    Circle()
                        .fill(Color.green.opacity(0.85))
                        .frame(width: 6, height: 6)
                    Text(String(format: L("prefs.volume.status.online"), formatFileCount(entryCount)))
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary)
                        .fixedSize()
                    if kind == "local_external" && !supportsPersistentJournal {
                        Image(systemName: "info.circle")
                            .font(.system(size: 9.5))
                            .foregroundStyle(.secondary)
                            .help(L("prefs.volume.nonPersistentJournalHint"))
                    }
                }
            }
        case let .volumeSyncing(_, scannedCount):
            if showVolumeMetrics {
                HStack(spacing: 4) {
                    ProgressView()
                        .controlSize(.mini)
                    let label = scannedCount > 0
                        ? String(format: L("prefs.volume.status.syncingWithCount"), formatFileCount(scannedCount))
                        : L("prefs.volume.status.scanning")
                    Text(label)
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary)
                        .fixedSize()
                }
            }
        case let .volumeNeedsConfirmation(_, _, _, _, confirmReason):
            if showVolumeMetrics {
                HStack(spacing: 4) {
                    Circle()
                        .fill(Color.orange)
                        .frame(width: 6, height: 6)
                    Text(L("prefs.volume.status.needsConfirmation"))
                        .font(.system(size: 10))
                        .foregroundStyle(.orange)
                        .fixedSize()
                }
                .help(volumeConfirmReasonTooltip(confirmReason))
            }
        case let .volumeOffline(entryCount, _):
            HStack(spacing: 4) {
                Circle()
                    .fill(Color.secondary.opacity(entryCount > 0 ? 0.6 : 0.45))
                    .frame(width: 6, height: 6)
                let label: String = {
                    if entryCount == 0 {
                        return L("prefs.volume.state.offlineUncached")
                    }
                    return showVolumeMetrics
                        ? String(format: L("prefs.volume.status.offline"), formatFileCount(entryCount))
                        : L("prefs.volume.status.offlineNoCount")
                }()
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
        case .volumeNeedsConfirmation:
            if let onConfirmScan {
                Button(L("prefs.volume.updateIndex"), action: onConfirmScan)
                    .buttonStyle(.borderless)
                    .font(.system(size: 10, weight: .medium))
                    .foregroundStyle(Color.accentColor)
                    .fixedSize()
            }
        case .volumeSyncing:
            if let onCancelScan {
                Button(L("prefs.volume.cancelScan"), action: onCancelScan)
                    .buttonStyle(.borderless)
                    .font(.system(size: 10, weight: .medium))
                    .foregroundStyle(.secondary)
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
        case .volumeUncachedOffline, .localAvailable, .localChecking, .localMissing:
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
    let isConflicting: (String) -> Bool
    let onCommit: (String) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var text = ""
    @State private var unconfirmedMissingPath: String?
    @State private var unconfirmedConflictPath: String?
    @State private var isProbingPath = false
    @State private var probeTask: Task<Void, Never>?
    @FocusState private var isFieldFocused: Bool

    private var trimmedValue: String {
        if mode.validatesDirectoryExistence {
            return PreferencesPathNormalizer.normalize(text)
        }
        return text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var warningMessage: String? {
        if let missingPath = unconfirmedMissingPath {
            return String(format: L("prefs.index.pathConfirm.message"), missingPath)
        }
        guard unconfirmedConflictPath != nil else { return nil }
        switch mode {
        case .indexPath: return L("prefs.index.pathConfirm.conflictExcluded")
        case .excludePath: return L("prefs.index.pathConfirm.conflictIndexed")
        case .excludePattern: return nil
        }
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
                    unconfirmedMissingPath = nil
                    unconfirmedConflictPath = nil
                }

            if let warningMessage {
                HStack(alignment: .top, spacing: 6) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange)
                        .font(.system(size: 12))
                        .padding(.top, 1)
                    Text(warningMessage)
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

                Button(warningMessage != nil
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
                    commit(value)
                } else {
                    unconfirmedMissingPath = value
                }
            }
            return
        }

        commit(value)
    }

    /// 与另一列表存在完全相同的路径时先拦截确认，避免写入后静默不生效。
    private func commit(_ value: String) {
        if unconfirmedConflictPath != value && isConflicting(value) {
            unconfirmedConflictPath = value
            return
        }
        onCommit(value)
        dismiss()
    }
}

// MARK: - Custom Network Rescan Interval Sheet
enum NetworkRescanIntervalUnit: String, CaseIterable, Identifiable {
    case minutes
    case hours
    case days

    var id: String { rawValue }

    /// 最长限制为 365 天（525,600 分钟），避免误输超大数字导致事实上的永不触发。
    static let maxTotalMinutes = 365 * 1440

    var minutesMultiplier: Int {
        switch self {
        case .minutes: return 1
        case .hours: return 60
        case .days: return 1440
        }
    }

    var maxValue: Int {
        Self.maxTotalMinutes / minutesMultiplier
    }

    var label: String {
        switch self {
        case .minutes: return L("prefs.networkRescan.customSheet.unit.minutes")
        case .hours: return L("prefs.networkRescan.customSheet.unit.hours")
        case .days: return L("prefs.networkRescan.customSheet.unit.days")
        }
    }

    static func decompose(minutes: Int) -> (amount: Int, unit: NetworkRescanIntervalUnit) {
        let clamped = max(1, minutes)
        if clamped >= 1440 && clamped % 1440 == 0 {
            return (clamped / 1440, .days)
        }
        if clamped >= 60 && clamped % 60 == 0 {
            return (clamped / 60, .hours)
        }
        return (clamped, .minutes)
    }

    static func parseTotalMinutes(text: String, unit: NetworkRescanIntervalUnit) -> Int? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, let value = Int(trimmed), value >= 1, value <= unit.maxValue else {
            return nil
        }
        return value * unit.minutesMultiplier
    }
}

private struct CustomNetworkRescanSheet: View {
    let initialMinutes: Int
    let onCommit: (Int) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var amountText: String
    @State private var unit: NetworkRescanIntervalUnit
    @FocusState private var isFieldFocused: Bool

    init(initialMinutes: Int, onCommit: @escaping (Int) -> Void) {
        self.initialMinutes = initialMinutes
        self.onCommit = onCommit
        let decomposed = NetworkRescanIntervalUnit.decompose(minutes: initialMinutes)
        _amountText = State(initialValue: String(decomposed.amount))
        _unit = State(initialValue: decomposed.unit)
    }

    private var parsedTotalMinutes: Int? {
        NetworkRescanIntervalUnit.parseTotalMinutes(text: amountText, unit: unit)
    }

    private var showValidationHint: Bool {
        !amountText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && parsedTotalMinutes == nil
    }

    private func stepAmount(by delta: Int) {
        let trimmed = amountText.trimmingCharacters(in: .whitespacesAndNewlines)
        let next: Int
        if let current = Int(trimmed) {
            if current < 1 {
                next = 1
            } else if current > unit.maxValue {
                next = unit.maxValue
            } else {
                next = min(max(current + delta, 1), unit.maxValue)
            }
        } else {
            next = 1
        }
        amountText = String(next)
    }

    private func handleSubmit() {
        guard let totalMinutes = parsedTotalMinutes else { return }
        onCommit(totalMinutes)
        dismiss()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 4) {
                Text(L("prefs.networkRescan.customSheet.title"))
                    .font(.headline)
                Text(L("prefs.networkRescan.customSheet.message"))
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            HStack(spacing: 8) {
                TextField("", text: $amountText)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 13).monospacedDigit())
                    .focused($isFieldFocused)
                    .onSubmit {
                        handleSubmit()
                    }

                Stepper("", onIncrement: {
                    stepAmount(by: 1)
                }, onDecrement: {
                    stepAmount(by: -1)
                })
                .labelsHidden()
                .fixedSize()

                Picker("", selection: $unit) {
                    ForEach(NetworkRescanIntervalUnit.allCases) { item in
                        Text(item.label).tag(item)
                    }
                }
                .pickerStyle(.menu)
                .labelsHidden()
                .frame(width: 96)
            }

            if showValidationHint {
                Text(String(format: L("prefs.networkRescan.customSheet.invalidRange"), unit.maxValue))
                    .font(.system(size: 11))
                    .foregroundStyle(.orange)
            }

            HStack {
                Spacer()
                Button(L("prefs.excludePatterns.editor.cancel")) {
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)

                Button(L("prefs.networkRescan.customSheet.apply")) {
                    handleSubmit()
                }
                .keyboardShortcut(.defaultAction)
                .buttonStyle(.borderedProminent)
                .disabled(parsedTotalMinutes == nil)
            }
        }
        .padding(20)
        .frame(width: 340)
        .onAppear {
            isFieldFocused = true
        }
    }
}

// MARK: - Managed Volume Row
private struct ManagedVolumeRow: View {
    let item: ManagedVolumeItem
    let onToggle: (Bool) -> Void
    let onRescan: () -> Void
    let onConfirmScan: () -> Void
    let onCancelScan: () -> Void
    let onClearCache: () -> Void

    private var iconName: String {
        item.kind == "network" ? "externaldrive.connected.to.line.below" : "externaldrive"
    }

    private var iconColor: Color {
        if item.isIndexed && item.status?.state == .needsConfirmation {
            return .orange
        }
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
                    if status.kind == "local_external" && !status.supportsPersistentJournal {
                        Image(systemName: "info.circle")
                            .font(.system(size: 9.5))
                            .foregroundStyle(.secondary)
                            .help(L("prefs.volume.nonPersistentJournalHint"))
                    }
                }
            case .syncing:
                HStack(spacing: 4) {
                    ProgressView()
                        .controlSize(.mini)
                    let label = status.scannedCount > 0
                        ? String(format: L("prefs.volume.status.syncingWithCount"), formatFileCount(status.scannedCount))
                        : L("prefs.volume.status.scanning")
                    Text(label)
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary)
                        .fixedSize()
                }
            case .needsConfirmation:
                HStack(spacing: 4) {
                    Circle()
                        .fill(Color.orange)
                        .frame(width: 6, height: 6)
                    Text(L("prefs.volume.status.needsConfirmation"))
                        .font(.system(size: 10))
                        .foregroundStyle(.orange)
                        .fixedSize()
                }
                .help(volumeConfirmReasonTooltip(status.confirmReason))
            case .offline:
                HStack(spacing: 4) {
                    Circle()
                        .fill(Color.secondary.opacity(status.entryCount > 0 ? 0.6 : 0.45))
                        .frame(width: 6, height: 6)
                    let label = status.entryCount > 0
                        ? String(format: L("prefs.volume.status.offline"), formatFileCount(status.entryCount))
                        : L("prefs.volume.state.offlineUncached")
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
                Text(L("prefs.volume.status.scanning"))
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
            case .needsConfirmation:
                Button(L("prefs.volume.updateIndex"), action: onConfirmScan)
                    .buttonStyle(.borderedProminent)
                    .controlSize(.mini)
                    .fixedSize()
            case .syncing:
                Button(L("prefs.volume.cancelScan"), action: onCancelScan)
                    .buttonStyle(.borderless)
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(.secondary)
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
    private static let customNetworkRescanTag = -1
    static let networkRescanPresetMinutes: [Int] = [30, 60, 180, 360, 720, 1440, 4320, 10080, 43200]

    static func networkRescanIntervalLabel(minutes: Int) -> String {
        if minutes == 1440 {
            return L("prefs.networkRescan.daySingular")
        }
        if minutes > 1440 && minutes % 1440 == 0 {
            return String(format: L("prefs.networkRescan.days"), minutes / 1440)
        }
        if minutes == 60 {
            return L("prefs.networkRescan.hourSingular")
        }
        if minutes >= 60 && minutes % 60 == 0 {
            return String(format: L("prefs.networkRescan.hours"), minutes / 60)
        }
        return String(format: L("prefs.networkRescan.minutes"), minutes)
    }

    static func networkRescanCustomIntervalLabel(minutes: Int) -> String {
        if minutes >= 1440 && minutes % 1440 == 0 {
            return String(format: L("prefs.networkRescan.customDays"), minutes / 1440)
        }
        if minutes >= 60 && minutes % 60 == 0 {
            return String(format: L("prefs.networkRescan.customHours"), minutes / 60)
        }
        return String(format: L("prefs.networkRescan.customMinutes"), minutes)
    }

    @ObservedObject var viewModel: PreferencesViewModel
    @State private var isPresetsExpanded = false
    @State private var isPatternPresetsExpanded = false
    @State private var activeInputSheet: IndexInputSheetMode?
    @State private var isCustomRescanSheetPresented = false
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
                                onConfirmScan: exactStatus.map { status in
                                    { viewModel.confirmVolumeScan(path: status.volumeId) }
                                },
                                onCancelScan: exactStatus.map { status in
                                    { viewModel.cancelVolumeScan(path: status.volumeId) }
                                },
                                onClearCache: exactStatus.map { status in
                                    { pendingClearCachePath = status.volumeId }
                                },
                                conflict: SettingsPathStatusResolver.containsSamePath(viewModel.excludePaths, path)
                                    ? (L("prefs.index.pathConflict.badge"), L("prefs.index.indexPaths.alsoExcluded")) : nil,
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
                                onConfirmScan: {
                                    viewModel.confirmVolumeScan(path: volume.status?.volumeId ?? volume.configuredPath)
                                },
                                onCancelScan: {
                                    viewModel.cancelVolumeScan(path: volume.status?.volumeId ?? volume.configuredPath)
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

                    SettingsToggleRow(
                        L("prefs.networkRescan"),
                        description: L("prefs.networkRescan.description"),
                        isOn: $viewModel.networkRescanEnabled.animation(.easeInOut(duration: 0.15))
                    )

                    if viewModel.networkRescanEnabled {
                        HStack(alignment: .center, spacing: 14) {
                            SettingsRowLabel(
                                L("prefs.networkRescan.interval"),
                                description: L("prefs.networkRescan.interval.description")
                            )
                            Spacer(minLength: 12)
                            Picker("", selection: Binding(
                                get: { viewModel.networkRescanMinutes },
                                set: { newValue in
                                    if newValue == Self.customNetworkRescanTag {
                                        isCustomRescanSheetPresented = true
                                    } else {
                                        viewModel.networkRescanMinutes = newValue
                                    }
                                }
                            )) {
                                ForEach(Self.networkRescanPresetMinutes, id: \.self) { minutes in
                                    Text(Self.networkRescanIntervalLabel(minutes: minutes))
                                        .tag(minutes)
                                }
                                Divider()
                                if !Self.networkRescanPresetMinutes.contains(viewModel.networkRescanMinutes) {
                                    Text(Self.networkRescanCustomIntervalLabel(minutes: viewModel.networkRescanMinutes))
                                        .tag(viewModel.networkRescanMinutes)
                                }
                                Text(L("prefs.networkRescan.customAction"))
                                    .tag(Self.customNetworkRescanTag)
                            }
                            .pickerStyle(.menu)
                            .labelsHidden()
                            .fixedSize()
                        }
                        .padding(.leading, 16)
                        .padding(.vertical, 2)
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
                                conflict: SettingsPathStatusResolver.containsSamePath(viewModel.indexPaths, path)
                                    ? (L("prefs.index.pathConflict.badge"), L("prefs.index.excludePaths.alsoIndexed")) : nil,
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
            .sheet(isPresented: $isCustomRescanSheetPresented) {
                CustomNetworkRescanSheet(initialMinutes: viewModel.networkRescanMinutes) { totalMinutes in
                    viewModel.networkRescanMinutes = totalMinutes
                }
            }

            indexActionBar
        }
        .sheet(item: $activeInputSheet) { mode in
            PreferencesTextInputSheet(
                mode: mode,
                isPathOffline: { expanded in
                    viewModel.engine.isPathOffline(expanded)
                },
                isConflicting: { value in
                    switch mode {
                    case .indexPath:
                        return SettingsPathStatusResolver.containsSamePath(viewModel.excludePaths, value)
                    case .excludePath:
                        return SettingsPathStatusResolver.containsSamePath(viewModel.indexPaths, value)
                    case .excludePattern:
                        return false
                    }
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
                // 维护性次要动作放左侧弱化；重建中由紧随其后的进度指示占位，避免与进度文字挤在一起
                if !viewModel.isRebuildingIndex {
                    Button {
                        viewModel.triggerManualRebuild()
                    } label: {
                        Label(L("prefs.index.rebuildNow"), systemImage: "arrow.clockwise")
                    }
                    .buttonStyle(.borderless)
                    .font(.system(size: 11, weight: .medium))
                    // 显式着色的无边框按钮禁用时不会自动变灰
                    .foregroundStyle(viewModel.isIndexDirty ? Color.secondary : Color.accentColor)
                    .fixedSize()
                    .disabled(viewModel.isIndexDirty || viewModel.isRebuildingIndex)
                    .help(L("prefs.index.rebuildNow.help"))
                }

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

                Button(viewModel.pendingSaveRequiresRebuild ? L("prefs.saveAndRebuild") : L("prefs.save")) {
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
