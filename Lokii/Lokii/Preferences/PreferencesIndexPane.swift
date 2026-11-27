import SwiftUI
import AppKit
import UniformTypeIdentifiers

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
    var piercedRule: (label: String, help: String)? = nil
    var isHighlighted: Bool = false
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
                    } else if let piercedRule {
                        HStack(spacing: 3) {
                            Image(systemName: "checkmark.shield.fill")
                                .font(.system(size: 9))
                                .foregroundStyle(.green)
                            Text(piercedRule.label)
                                .font(.system(size: 10))
                                .foregroundStyle(.secondary)
                                .fixedSize()
                        }
                        .help(piercedRule.help)
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
        .padding(.vertical, 2)
        .padding(.horizontal, isHighlighted ? 6 : 0)
        .background(isHighlighted ? Color.accentColor.opacity(0.10) : Color.clear)
        .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 6, style: .continuous)
                .strokeBorder(isHighlighted ? Color.accentColor.opacity(0.55) : Color.clear, lineWidth: 1)
        )
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

// MARK: - Managed Cloud Drive Row
private struct ManagedCloudDriveRow: View {
    let item: ManagedCloudDriveItem
    let onToggle: (Bool) -> Void

    private var iconName: String {
        if item.providerHint == "icloud" {
            return (item.isAvailableOnDisk || item.isIndexed) ? "icloud" : "icloud.slash"
        }
        return "cloud"
    }

    private var iconColor: Color {
        (item.isIndexed && item.isAvailableOnDisk) ? Color.accentColor : .secondary
    }

    var body: some View {
        HStack(alignment: .center, spacing: 10) {
            Image(systemName: iconName)
                .foregroundStyle(iconColor)
                .font(.system(size: 13))
                .frame(width: 18)

            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(item.displayName)
                        .lineLimit(1)
                        .truncationMode(.middle)

                    statusBadge
                }

                Text(item.path)
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }

            Spacer(minLength: 8)

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
        } else if let status = item.volumeStatus {
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
                    Text(L("prefs.volume.status.scanning"))
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
            case .offline, .missing:
                HStack(spacing: 3) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .font(.system(size: 9))
                        .foregroundStyle(.orange)
                    Text(L("prefs.index.pathNotFound"))
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary)
                        .fixedSize()
                }
            }
        } else if !item.isAvailableOnDisk {
            HStack(spacing: 3) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.system(size: 9))
                    .foregroundStyle(.orange)
                Text(L("prefs.index.pathNotFound"))
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
                    .fixedSize()
            }
        } else {
            HStack(spacing: 4) {
                Circle()
                    .fill(Color.green.opacity(0.85))
                    .frame(width: 6, height: 6)
                Text(L("prefs.cloudDrive.status.indexed"))
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
                    .fixedSize()
            }
        }
    }
}

// MARK: - Path Diagnostic & Rule Lookup Section
private struct IndexPathDiagnosticSection: View {
    @ObservedObject var viewModel: PreferencesViewModel
    let onLocateExcludeRule: (String) -> Void
    let onChooseTarget: () -> Void

    @State private var isDropTargeted = false
    @State private var isChooseHovered = false
    @State private var isClearHovered = false
    @FocusState private var isInputFocused: Bool

    private var isRingActive: Bool {
        isDropTargeted || isInputFocused
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 7) {
                Image(systemName: isDropTargeted ? "arrow.down.doc.fill" : "magnifyingglass")
                    .foregroundStyle(isRingActive ? Color.accentColor : Color.secondary)
                    .font(.system(size: 12, weight: .medium))

                // 显式置空标题并隐藏标签，防止分组表单把首参占位文渲染成左侧多行标签而挤压输入框
                TextField(
                    "",
                    text: $viewModel.diagnosticInput,
                    prompt: Text(L("prefs.diagnostic.placeholder"))
                )
                .textFieldStyle(.plain)
                .labelsHidden()
                .multilineTextAlignment(.leading)
                .font(viewModel.isDiagnosticInputPath ? .system(size: 12, design: .monospaced) : .system(size: 12))
                .focused($isInputFocused)
                .frame(maxWidth: .infinity, alignment: .leading)
                .onSubmit {
                    viewModel.refreshDiagnostic()
                }

                if !viewModel.diagnosticInput.isEmpty {
                    Button {
                        viewModel.diagnosticInput = ""
                        isInputFocused = true
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .foregroundStyle(isClearHovered ? Color.primary.opacity(0.65) : Color.secondary.opacity(0.65))
                            .font(.system(size: 12))
                    }
                    .buttonStyle(.plain)
                    .help(L("prefs.diagnostic.clear"))
                    .onHover { hovering in
                        isClearHovered = hovering
                    }
                }

                Rectangle()
                    .fill(Color(nsColor: .separatorColor).opacity(0.6))
                    .frame(width: 1, height: 14)

                Button(action: onChooseTarget) {
                    HStack(spacing: 4) {
                        Image(systemName: "folder")
                            .font(.system(size: 11))
                        Text(L("prefs.diagnostic.chooseButton"))
                            .font(.system(size: 11.5, weight: .medium))
                    }
                    .foregroundStyle(isChooseHovered ? Color.primary : Color.secondary)
                    .padding(.horizontal, 7)
                    .padding(.vertical, 3.5)
                    .background(
                        RoundedRectangle(cornerRadius: 5, style: .continuous)
                            .fill(isChooseHovered ? Color.primary.opacity(0.07) : Color.clear)
                    )
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help(L("prefs.diagnostic.chooseHelp"))
                .onHover { hovering in
                    isChooseHovered = hovering
                }
            }
            .padding(.leading, 10)
            .padding(.trailing, 5)
            .frame(height: 32)
            .background(
                ZStack {
                    Color(nsColor: .textBackgroundColor)
                    if isDropTargeted {
                        Color.accentColor.opacity(0.06)
                    }
                }
            )
            .clipShape(RoundedRectangle(cornerRadius: 7, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .strokeBorder(
                        isRingActive ? Color.accentColor.opacity(0.85) : Color(nsColor: .separatorColor),
                        lineWidth: 1
                    )
            )
            .overlay(
                RoundedRectangle(cornerRadius: 8.5, style: .continuous)
                    .stroke(Color.accentColor.opacity(isRingActive ? 0.28 : 0), lineWidth: 2.5)
                    .padding(-1.5)
            )
            .contentShape(Rectangle())
            .onTapGesture {
                isInputFocused = true
            }

            switch viewModel.diagnosticResult {
            case .empty:
                EmptyView()
            case let .path(draft, saved):
                Divider()
                    .padding(.top, 1)
                pathDiagnosisRow(draft: draft, saved: saved)
            case let .keyword(match):
                Divider()
                    .padding(.top, 1)
                keywordMatchSection(match)
            }
        }
        .padding(.vertical, 2)
        .onDrop(of: [UTType.fileURL], isTargeted: $isDropTargeted) { providers in
            handleDrop(providers: providers)
        }
    }

    private func handleDrop(providers: [NSItemProvider]) -> Bool {
        guard let provider = providers.first(where: { $0.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier) }) else {
            return false
        }
        provider.loadItem(forTypeIdentifier: UTType.fileURL.identifier, options: nil) { item, _ in
            let resolvedURL: URL? = {
                if let url = item as? URL { return url }
                if let nsurl = item as? NSURL { return nsurl as URL }
                if let data = item as? Data {
                    return URL(dataRepresentation: data, relativeTo: nil)
                        ?? String(data: data, encoding: .utf8)
                            .flatMap { URL(string: $0.trimmingCharacters(in: .whitespacesAndNewlines)) }
                }
                return nil
            }()
            guard let resolvedURL else { return }
            let normalized = PreferencesPathNormalizer.normalize(resolvedURL.path)
            DispatchQueue.main.async {
                viewModel.diagnosticInput = normalized
            }
        }
        return true
    }

    @ViewBuilder
    private func pathDiagnosisRow(draft: FfiPathDiagnosis, saved: FfiPathDiagnosis?) -> some View {
        let isPendingSaveIndexed = draft.status == .indexed && saved != nil && saved?.status != .indexed
        let isPositive = draft.status == .indexed
        let statusColor: Color = isPositive ? (isPendingSaveIndexed ? .orange : .green) : .orange
        let iconName = isPositive
            ? (isPendingSaveIndexed ? "clock.badge.checkmark" : "checkmark.circle.fill")
            : "exclamationmark.triangle.fill"

        HStack(alignment: .top, spacing: 10) {
            RoundedRectangle(cornerRadius: 1.5, style: .continuous)
                .fill(statusColor.opacity(0.85))
                .frame(width: 2.5)
                .padding(.vertical, 1)

            VStack(alignment: .leading, spacing: 6) {
                HStack(alignment: .center, spacing: 6) {
                    Image(systemName: iconName)
                        .foregroundStyle(statusColor)
                        .font(.system(size: 12))

                    Text(diagnosisTitle(draft: draft, saved: saved))
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(.primary)

                    if let rule = draft.matchedRule, !rule.isEmpty, draft.status != .indexed {
                        Text(rule)
                            .font(.system(size: 10.5, design: .monospaced))
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 1.5)
                            .background(Color.primary.opacity(0.06))
                            .clipShape(RoundedRectangle(cornerRadius: 4, style: .continuous))

                        if draft.isPresetRule {
                            Text(L("prefs.diagnostic.presetBadge"))
                                .font(.system(size: 10))
                                .foregroundStyle(.tertiary)
                                .fixedSize()
                        }
                    }

                    Spacer(minLength: 4)

                    if canLocateRule(for: draft), let rule = draft.matchedRule {
                        Button(L("prefs.diagnostic.locateRule")) {
                            onLocateExcludeRule(rule)
                        }
                        .buttonStyle(.borderless)
                        .font(.system(size: 11, weight: .medium))
                        .foregroundStyle(Color.accentColor)
                        .fixedSize()
                    }
                }

                Text(diagnosisSubtitle(draft: draft, saved: saved))
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

                if draft.status == .indexed, let symlinkTarget = draft.symlinkTarget, !symlinkTarget.isEmpty {
                    Text(String(format: L("prefs.diagnostic.symlinkTargetNote"), symlinkTarget))
                        .font(.system(size: 10.5))
                        .foregroundStyle(.secondary)
                }

                if draft.alsoRequiresFda {
                    Text(L("prefs.diagnostic.alsoRequiresFda"))
                        .font(.system(size: 10.5))
                        .foregroundStyle(.orange)
                        .fixedSize(horizontal: false, vertical: true)
                }

                if draft.existsOnDisk == false {
                    Text(L("prefs.diagnostic.notFoundOnDisk"))
                        .font(.system(size: 10.5))
                        .foregroundStyle(.orange)
                }

                if hasActionButtons(for: draft) {
                    FlowLayout(spacing: 8) {
                        if let suggested = draft.suggestedIndexPath {
                            Button {
                                viewModel.addIndexPathIfMissing(suggested)
                            } label: {
                                Text(primaryFixTitle(status: draft.status, suggestedPath: suggested))
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                            }
                            .buttonStyle(.borderedProminent)
                            .controlSize(.small)
                            .help(suggested)
                        }

                        if draft.status == .excludedByName, let rule = draft.matchedRule {
                            Button(String(format: L("prefs.diagnostic.fix.removeRule"), rule)) {
                                viewModel.removeExcludePattern(rule)
                            }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                        }

                        if draft.status == .excludedByPathPrefix,
                           draft.suggestedIndexPath == nil,
                           let rule = draft.matchedRule {
                            Button(String(format: L("prefs.diagnostic.fix.removeExcludePath"), rule)) {
                                viewModel.removeExcludePath(rule)
                            }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                        }

                        if draft.status == .excludedByHidden {
                            Button(L("prefs.diagnostic.fix.enableHidden")) {
                                viewModel.includeHidden = true
                            }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                        }

                        if draft.status == .protectedByFda || draft.alsoRequiresFda {
                            Button(L("prefs.diagnostic.fix.openPrivacy")) {
                                viewModel.openPrivacyTab()
                            }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                        }
                    }

                    if draft.status == .excludedByPathPrefix,
                       draft.suggestedIndexPath != nil,
                       let rule = draft.matchedRule {
                        Text(String(format: L("prefs.diagnostic.fix.pierceNote"), rule))
                            .font(.system(size: 10.5))
                            .foregroundStyle(.tertiary)
                    }
                }
            }
        }
        .padding(.vertical, 2)
    }

    @ViewBuilder
    private func keywordMatchSection(_ match: KeywordRuleMatchResult) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            if match.isEmpty {
                Text(L("prefs.diagnostic.keyword.noMatches"))
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
            } else {
                if !match.matchedCloudDrives.isEmpty {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(L("prefs.diagnostic.keyword.cloudDrives"))
                            .font(.system(size: 10.5, weight: .medium))
                            .foregroundStyle(.secondary)
                        ForEach(match.matchedCloudDrives) { drive in
                            ManagedCloudDriveRow(item: drive) { enabled in
                                viewModel.setCloudDriveIndexed(drive, enabled: enabled)
                            }
                        }
                    }
                }

                if !match.matchedExcludePaths.isEmpty {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(L("prefs.diagnostic.keyword.excludePaths"))
                            .font(.system(size: 10.5, weight: .medium))
                            .foregroundStyle(.secondary)
                        FlowLayout(spacing: 6) {
                            ForEach(match.matchedExcludePaths) { item in
                                Button {
                                    onLocateExcludeRule(item.value)
                                } label: {
                                    HStack(spacing: 4) {
                                        Text(item.value)
                                            .font(.system(size: 11, design: .monospaced))
                                        Text(L(item.isPreset ? "prefs.diagnostic.presetShort" : "prefs.diagnostic.customShort"))
                                            .font(.system(size: 9.5))
                                            .foregroundStyle(.secondary)
                                    }
                                    .padding(.horizontal, 8)
                                    .padding(.vertical, 3)
                                    .background(Color.primary.opacity(0.06))
                                    .clipShape(Capsule())
                                }
                                .buttonStyle(.plain)
                                .help(L("prefs.diagnostic.locateRule"))
                            }
                        }
                    }
                }

                if !match.matchedExcludePatterns.isEmpty {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(L("prefs.diagnostic.keyword.excludePatterns"))
                            .font(.system(size: 10.5, weight: .medium))
                            .foregroundStyle(.secondary)
                        FlowLayout(spacing: 6) {
                            ForEach(match.matchedExcludePatterns) { item in
                                Button {
                                    onLocateExcludeRule(item.value)
                                } label: {
                                    HStack(spacing: 4) {
                                        Text(item.value)
                                            .font(.system(size: 11, design: .monospaced))
                                        Text(L(item.isPreset ? "prefs.diagnostic.presetShort" : "prefs.diagnostic.customShort"))
                                            .font(.system(size: 9.5))
                                            .foregroundStyle(.secondary)
                                    }
                                    .padding(.horizontal, 8)
                                    .padding(.vertical, 3)
                                    .background(Color.primary.opacity(0.06))
                                    .clipShape(Capsule())
                                }
                                .buttonStyle(.plain)
                                .help(L("prefs.diagnostic.locateRule"))
                            }
                        }
                    }
                }

                if !match.matchedIndexPaths.isEmpty {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(L("prefs.diagnostic.keyword.indexPaths"))
                            .font(.system(size: 10.5, weight: .medium))
                            .foregroundStyle(.secondary)
                        FlowLayout(spacing: 6) {
                            ForEach(match.matchedIndexPaths, id: \.self) { path in
                                Text(path)
                                    .font(.system(size: 11, design: .monospaced))
                                    .padding(.horizontal, 8)
                                    .padding(.vertical, 3)
                                    .background(Color.accentColor.opacity(0.10))
                                    .clipShape(Capsule())
                            }
                        }
                    }
                }
            }
        }
        .padding(.vertical, 2)
    }

    private func hasActionButtons(for draft: FfiPathDiagnosis) -> Bool {
        draft.suggestedIndexPath != nil
            || (draft.status == .excludedByName && draft.matchedRule != nil)
            || (draft.status == .excludedByPathPrefix && draft.suggestedIndexPath == nil && draft.matchedRule != nil)
            || draft.status == .excludedByHidden
            || draft.status == .protectedByFda
            || draft.alsoRequiresFda
    }

    private func canLocateRule(for draft: FfiPathDiagnosis) -> Bool {
        guard let rule = draft.matchedRule else { return false }
        switch draft.status {
        case .excludedByPathPrefix:
            return viewModel.excludePaths.contains {
                SettingsPathStatusResolver.normalizePath($0).caseInsensitiveCompare(SettingsPathStatusResolver.normalizePath(rule)) == .orderedSame
            }
        case .excludedByName:
            return viewModel.excludePatterns.contains(rule)
        default:
            return false
        }
    }

    private func primaryFixTitle(status: FfiPathDiagnosisStatus, suggestedPath: String) -> String {
        switch status {
        case .excludedByPathPrefix:
            return String(format: L("prefs.diagnostic.fix.piercePath"), suggestedPath)
        case .symlinkNotFollowed:
            return String(format: L("prefs.diagnostic.fix.addSymlinkTarget"), suggestedPath)
        default:
            return String(format: L("prefs.diagnostic.fix.addIndexPath"), suggestedPath)
        }
    }

    private func diagnosisTitle(draft: FfiPathDiagnosis, saved: FfiPathDiagnosis?) -> String {
        switch draft.status {
        case .indexed:
            if let saved, saved.status != .indexed {
                return L("prefs.diagnostic.status.pendingSaveTitle")
            }
            return L("prefs.diagnostic.status.indexedTitle")
        case .outsideIndexRoots:
            return L("prefs.diagnostic.status.outsideTitle")
        case .excludedByPathPrefix:
            return L("prefs.diagnostic.status.excludedPathTitle")
        case .excludedByName:
            return L("prefs.diagnostic.status.excludedNameTitle")
        case .excludedByHidden:
            return L("prefs.diagnostic.status.excludedHiddenTitle")
        case .excludedBySensitive:
            return L("prefs.diagnostic.status.excludedSensitiveTitle")
        case .excludedByShallowApp:
            return L("prefs.diagnostic.status.excludedAppTitle")
        case .excludedByPackageBundle:
            return L("prefs.diagnostic.status.excludedPackageTitle")
        case .protectedByFda:
            return L("prefs.diagnostic.status.protectedFdaTitle")
        case .symlinkNotFollowed:
            return L("prefs.diagnostic.status.symlinkTitle")
        }
    }

    private func diagnosisSubtitle(draft: FfiPathDiagnosis, saved: FfiPathDiagnosis?) -> String {
        switch draft.status {
        case .indexed:
            let root = draft.matchedRoot ?? "~"
            if let saved, saved.status != .indexed {
                return String(format: L("prefs.diagnostic.status.pendingSaveSubtitle"), root)
            }
            if let pierced = draft.piercedExcludePath, !pierced.isEmpty {
                return String(format: L("prefs.diagnostic.status.indexedPiercedSubtitle"), root, pierced)
            }
            return String(format: L("prefs.diagnostic.status.indexedSubtitle"), root)
        case .outsideIndexRoots:
            return L("prefs.diagnostic.status.outsideSubtitle")
        case .excludedByPathPrefix:
            return String(format: L("prefs.diagnostic.status.excludedPathSubtitle"), draft.matchedRule ?? "")
        case .excludedByName:
            return String(format: L("prefs.diagnostic.status.excludedNameSubtitle"), draft.matchedRule ?? "")
        case .excludedByHidden:
            return String(format: L("prefs.diagnostic.status.excludedHiddenSubtitle"), draft.matchedRule ?? "")
        case .excludedBySensitive:
            return String(format: L("prefs.diagnostic.status.excludedSensitiveSubtitle"), draft.matchedRule ?? "")
        case .excludedByShallowApp:
            return String(format: L("prefs.diagnostic.status.excludedAppSubtitle"), draft.matchedRule ?? "")
        case .excludedByPackageBundle:
            return String(format: L("prefs.diagnostic.status.excludedPackageSubtitle"), draft.matchedRule ?? "")
        case .protectedByFda:
            return String(format: L("prefs.diagnostic.status.protectedFdaSubtitle"), draft.matchedRule ?? "")
        case .symlinkNotFollowed:
            return String(format: L("prefs.diagnostic.status.symlinkSubtitle"), draft.matchedRule ?? "", draft.symlinkTarget ?? "")
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

            ScrollViewReader { scrollProxy in
                Form {
                    // Section 0: 路径诊断与规则反查
                    Section {
                        IndexPathDiagnosticSection(
                            viewModel: viewModel,
                            onLocateExcludeRule: { rule in
                                locateExcludeRule(rule, scrollProxy: scrollProxy)
                            },
                            onChooseTarget: {
                                chooseDiagnosticTarget()
                            }
                        )
                    } header: {
                        SettingsSectionHeader(
                            L("prefs.diagnostic.sectionTitle"),
                            description: L("prefs.diagnostic.sectionDescription")
                        )
                    }

                    // Section 1: 索引目录 (Index Paths)
                    Section {
                        if viewModel.regularIndexPaths.isEmpty {
                            Text(L("prefs.indexPaths.empty"))
                                .font(.system(size: 11))
                                .foregroundStyle(.secondary)
                                .frame(maxWidth: .infinity, alignment: .center)
                                .padding(.vertical, 4)
                        } else {
                            ForEach(viewModel.regularIndexPaths, id: \.self) { path in
                                let exactStatus = viewModel.exactSavedVolumeStatus(for: path)
                                let pierced = SettingsPathStatusResolver.piercedExcludePath(
                                    for: path,
                                    excludePaths: viewModel.excludePaths
                                )
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
                                    piercedRule: pierced.map { rule in
                                        (
                                            String(format: L("prefs.index.piercedExclude.badge"), rule),
                                            String(format: L("prefs.index.piercedExclude.help"), rule)
                                        )
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

                    // Section 2: 本机云盘（仅当检测到可用或已配置云盘时展示）
                    if !viewModel.managedCloudDrives.isEmpty {
                        Section {
                            ForEach(viewModel.managedCloudDrives) { drive in
                                ManagedCloudDriveRow(item: drive) { enabled in
                                    viewModel.setCloudDriveIndexed(drive, enabled: enabled)
                                }
                            }
                        } header: {
                            SettingsSectionHeader(
                                L("prefs.cloudDrive.group.title"),
                                description: L("prefs.cloudDrive.group.description")
                            )
                        }
                    }

                    // Section 3: 外置磁盘与网络卷 (Mounted & Offline Volumes)
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

                    // Section 4: 排除路径 (Exclude Paths)
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
                                    isHighlighted: isRuleHighlighted(viewModel.highlightedRule, target: path),
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
                                        PillTagView(
                                            title: path,
                                            isHighlighted: isRuleHighlighted(viewModel.highlightedRule, target: path)
                                        ) {
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
                        .id("excludePathsSection")
                    }

                    // Section 5: 排除名称规则 (Exclude Patterns - Pill Tags)
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
                                    PillTagView(
                                        title: pattern,
                                        isHighlighted: viewModel.highlightedRule == pattern
                                    ) {
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
                                        PillTagView(
                                            title: pattern,
                                            isHighlighted: viewModel.highlightedRule == pattern
                                        ) {
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
                        .id("excludePatternsSection")
                    }

                    // Section 6: 扫描选项
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
            }
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

    // MARK: - Diagnostic Helpers & Directory Panel
    private func isRuleHighlighted(_ highlighted: String?, target: String) -> Bool {
        guard let highlighted, !highlighted.isEmpty else { return false }
        return SettingsPathStatusResolver.normalizePath(highlighted)
            .caseInsensitiveCompare(SettingsPathStatusResolver.normalizePath(target)) == .orderedSame
    }

    private func locateExcludeRule(_ rule: String, scrollProxy: ScrollViewProxy) {
        let normRule = SettingsPathStatusResolver.normalizePath(rule)
        if viewModel.presetExcludePaths.contains(where: {
            SettingsPathStatusResolver.normalizePath($0).caseInsensitiveCompare(normRule) == .orderedSame
        }) {
            isPresetsExpanded = true
            viewModel.highlightRuleTemporarily(rule)
            withAnimation(.easeInOut(duration: 0.2)) {
                scrollProxy.scrollTo("excludePathsSection", anchor: .top)
            }
            return
        }
        if viewModel.customExcludePaths.contains(where: {
            SettingsPathStatusResolver.normalizePath($0).caseInsensitiveCompare(normRule) == .orderedSame
        }) {
            viewModel.highlightRuleTemporarily(rule)
            withAnimation(.easeInOut(duration: 0.2)) {
                scrollProxy.scrollTo("excludePathsSection", anchor: .top)
            }
            return
        }
        if viewModel.presetExcludePatterns.contains(rule) {
            isPatternPresetsExpanded = true
            viewModel.highlightRuleTemporarily(rule)
            withAnimation(.easeInOut(duration: 0.2)) {
                scrollProxy.scrollTo("excludePatternsSection", anchor: .top)
            }
            return
        }
        if viewModel.customExcludePatterns.contains(rule) {
            viewModel.highlightRuleTemporarily(rule)
            withAnimation(.easeInOut(duration: 0.2)) {
                scrollProxy.scrollTo("excludePatternsSection", anchor: .top)
            }
        }
    }

    private func chooseDiagnosticTarget() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = true
        panel.allowsMultipleSelection = false

        let handleSelection = { (urls: [URL]) in
            guard let url = urls.first else { return }
            viewModel.diagnosticInput = PreferencesPathNormalizer.normalize(url.path)
        }

        if let hostWindow = NSApp.keyWindow {
            panel.beginSheetModal(for: hostWindow) { response in
                guard response == .OK else { return }
                handleSelection(panel.urls)
            }
        } else if panel.runModal() == .OK {
            handleSelection(panel.urls)
        }
    }

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
