import SwiftUI
import AppKit

// MARK: - General Preferences Pane
struct PreferencesGeneralPane: View {
    @ObservedObject var viewModel: PreferencesViewModel

    /// 下拉菜单中“选择其他...”动作的哨兵标识，拦截选中后打开文件选择面板而不写入配置
    private static let chooseOtherTerminalTag = "__lokii.terminal.chooseOther__"

    var body: some View {
        Form {
            // Section 1: 行为
            Section(L("prefs.section.behavior")) {
                SettingsToggleRow(
                    L("prefs.showInDock"),
                    description: L("prefs.showInDock.description"),
                    isOn: $viewModel.showInDock
                )
                SettingsToggleRow(
                    L("prefs.hideOnBlur"),
                    description: L("prefs.hideOnBlur.description"),
                    isOn: $viewModel.hideOnBlur
                )
                SettingsToggleRow(
                    L("prefs.rememberPosition"),
                    description: L("prefs.rememberPosition.description"),
                    isOn: $viewModel.rememberPosition
                )
                HStack(alignment: .center, spacing: 14) {
                    SettingsRowLabel(
                        L("prefs.windowSize"),
                        description: L("prefs.windowSize.description")
                    )
                    Spacer(minLength: 12)
                    Button(L("prefs.windowSize.reset")) {
                        viewModel.resetWindowSizes()
                    }
                    .buttonStyle(.bordered)
                    .disabled(!viewModel.hasCustomWindowSizes)
                }
                .padding(.vertical, 2)
                SettingsToggleRow(
                    L("prefs.launchAtLogin"),
                    description: L("prefs.launchAtLogin.description"),
                    isOn: $viewModel.launchAtLogin
                )
            }

            // Section 2: 终端与集成
            Section(L("prefs.section.terminalAndIntegration")) {
                HStack(alignment: .center, spacing: 14) {
                    SettingsRowLabel(
                        L("prefs.defaultTerminal"),
                        description: L("prefs.defaultTerminal.description")
                    )
                    Spacer(minLength: 12)
                    Picker("", selection: Binding(
                        get: { viewModel.terminalBundleId },
                        set: { newValue in
                            if newValue == Self.chooseOtherTerminalTag {
                                viewModel.chooseCustomTerminal(window: NSApp.keyWindow)
                            } else {
                                viewModel.terminalBundleId = newValue
                            }
                        }
                    )) {
                        ForEach(viewModel.availableTerminals) { item in
                            Label {
                                Text(item.name)
                            } icon: {
                                Image(nsImage: item.menuIcon)
                            }
                            .tag(item.id)
                        }
                        Divider()
                        Text(L("prefs.defaultTerminal.chooseOther")).tag(Self.chooseOtherTerminalTag)
                    }
                    .pickerStyle(.menu)
                    .labelsHidden()
                    .fixedSize()
                }
                .padding(.vertical, 2)

                CliSettingRow(viewModel: viewModel)
            }

            // Section 3: 搜索
            Section(L("prefs.section.search")) {
                SettingsToggleRow(
                    L("prefs.hideResultsWhenEmpty"),
                    description: L("prefs.hideResultsWhenEmpty.description"),
                    isOn: $viewModel.hideResultsWhenEmpty
                )

                HStack(alignment: .center, spacing: 14) {
                    SettingsRowLabel(
                        L("prefs.maxResults"),
                        description: L("prefs.maxResults.description")
                    )
                    Spacer(minLength: 12)
                    SettingsNumberStepper(
                        value: $viewModel.maxResults,
                        range: 50...5000,
                        step: 50,
                        width: 100
                    )
                }
                .padding(.vertical, 2)

                HStack(alignment: .center, spacing: 14) {
                    SettingsRowLabel(
                        L("prefs.globalShortcut"),
                        description: L("prefs.globalShortcut.description")
                    )
                    Spacer(minLength: 12)
                    KeyboardShortcuts.Recorder(for: .toggleWindow)
                }
                .padding(.vertical, 2)
            }

            // Section 4: 外观与语言
            Section(L("prefs.section.appearanceAndLanguage")) {
                HStack(alignment: .center, spacing: 14) {
                    SettingsRowLabel(
                        L("prefs.appearance"),
                        description: L("prefs.appearance.description")
                    )
                    Spacer(minLength: 12)
                    SettingsSegmentedControl(
                        options: [
                            .init(value: AppTheme.system, title: L("prefs.appearance.system")),
                            .init(value: AppTheme.light, title: L("prefs.appearance.light")),
                            .init(value: AppTheme.dark, title: L("prefs.appearance.dark")),
                        ],
                        selection: $viewModel.theme
                    )
                }
                .padding(.vertical, 2)

                HStack(alignment: .center, spacing: 14) {
                    SettingsRowLabel(
                        L("prefs.language"),
                        description: L("prefs.language.description")
                    )
                    Spacer(minLength: 12)
                    SettingsSegmentedControl(
                        options: [
                            .init(value: "zh-Hans", title: L("prefs.language.zh-Hans")),
                            .init(value: "en", title: L("prefs.language.en")),
                        ],
                        selection: $viewModel.language
                    )
                }
                .padding(.vertical, 2)
            }
        }
        .formStyle(.grouped)
    }
}

// MARK: - Command Line Tool Setting Row
struct CliSettingRow: View {
    @ObservedObject var viewModel: PreferencesViewModel
    @State private var showingErrorSheet = false
    @State private var showingUninstallConfirm = false

    var body: some View {
        // 使用 HStack(.center) 避免 LabeledContent 默认按首行基线对齐导致按钮在右上方悬空
        HStack(alignment: .center, spacing: 14) {
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 8) {
                    Text(L("prefs.cli"))

                    statusBadge
                }

                Text(L("prefs.cli.description"))
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

                if case .failed(let message, let detail) = viewModel.cliState {
                    HStack(spacing: 5) {
                        Image(systemName: "exclamationmark.circle.fill")
                            .font(.system(size: 10))
                            .foregroundStyle(Color.red)
                        Text(message)
                            .font(.system(size: 11))
                            .foregroundStyle(Color.red.opacity(0.9))
                            .lineLimit(1)
                        Text("•")
                            .font(.system(size: 10))
                            .foregroundStyle(.secondary.opacity(0.5))
                        Button(L("prefs.cli.viewLog")) {
                            showingErrorSheet = true
                        }
                        .buttonStyle(.link)
                        .font(.system(size: 11))
                    }
                    .padding(.top, 2)
                    .sheet(isPresented: $showingErrorSheet) {
                        CliErrorDetailSheet(message: message, detail: detail, isPresented: $showingErrorSheet)
                    }
                }
            }

            Spacer(minLength: 12)

            actionButton
        }
        .padding(.vertical, 2)
        .alert(
            L("prefs.cli.uninstallConfirm.title"),
            isPresented: $showingUninstallConfirm
        ) {
            Button(L("prefs.excludePatterns.editor.cancel"), role: .cancel) {}
            Button(L("prefs.cli.uninstallConfirm.confirm"), role: .destructive) {
                viewModel.uninstallCommandLineTool()
            }
        } message: {
            Text(String(format: L("prefs.cli.uninstallConfirm.message"), viewModel.cliTarget))
        }
    }

    @ViewBuilder
    private var statusBadge: some View {
        switch viewModel.cliState {
        case .notInstalled:
            Text(L("prefs.cli.status.notInstalled"))
                .font(.system(size: 10, weight: .medium))
                .foregroundStyle(.secondary)
                .padding(.horizontal, 7)
                .padding(.vertical, 2)
                .background(Color.primary.opacity(0.06))
                .clipShape(Capsule())

        case .installing:
            HStack(spacing: 4) {
                ProgressView()
                    .controlSize(.mini)
                Text(L("prefs.cli.status.installing"))
                    .font(.system(size: 10, weight: .medium))
                    .foregroundStyle(Color.accentColor)
            }
            .padding(.horizontal, 7)
            .padding(.vertical, 2)
            .background(Color.accentColor.opacity(0.10))
            .clipShape(Capsule())

        case .installed:
            Text(L("prefs.cli.status.installed"))
                .font(.system(size: 10, weight: .medium))
                .foregroundStyle(Color.green)
                .padding(.horizontal, 7)
                .padding(.vertical, 2)
                .background(Color.green.opacity(0.12))
                .clipShape(Capsule())

        case .failed:
            Text(L("prefs.cli.status.failed"))
                .font(.system(size: 10, weight: .medium))
                .foregroundStyle(Color.red)
                .padding(.horizontal, 7)
                .padding(.vertical, 2)
                .background(Color.red.opacity(0.10))
                .clipShape(Capsule())
        }
    }

    @ViewBuilder
    private var actionButton: some View {
        switch viewModel.cliState {
        case .notInstalled:
            Button(L("prefs.cli.install")) {
                viewModel.installCommandLineTool()
            }
            .buttonStyle(.bordered)

        case .installing:
            Button(action: {}) {
                HStack(spacing: 4) {
                    ProgressView()
                        .controlSize(.small)
                    Text(L("prefs.cli.installingAction"))
                }
            }
            .buttonStyle(.bordered)
            .disabled(true)

        case .installed:
            Button(L("prefs.cli.uninstall")) {
                showingUninstallConfirm = true
            }
            .buttonStyle(.bordered)

        case .failed:
            Button(L("prefs.cli.retry")) {
                viewModel.retryCommandLineTool()
            }
            .buttonStyle(.bordered)
        }
    }
}

// MARK: - CLI Error Detail Sheet
struct CliErrorDetailSheet: View {
    let message: String
    let detail: String
    @Binding var isPresented: Bool
    @State private var isCopied = false

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                Label(L("prefs.cli.error.detailTitle"), systemImage: "terminal.fill")
                    .font(.system(size: 13, weight: .semibold))
                Spacer()
                Button(action: { isPresented = false }) {
                    Image(systemName: "xmark.circle.fill")
                        .foregroundStyle(.secondary)
                        .font(.system(size: 14))
                }
                .buttonStyle(.plain)
            }

            Text(message)
                .font(.system(size: 12))
                .foregroundStyle(.secondary)

            ScrollView {
                Text(detail)
                    .font(.system(size: 11, design: .monospaced))
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(10)
                    .background(Color(nsColor: .textBackgroundColor).opacity(0.6))
                    .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
            }
            .frame(maxHeight: 180)

            HStack {
                Button(isCopied ? L("prefs.cli.copied") : L("prefs.cli.copyLog")) {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(detail, forType: .string)
                    withAnimation { isCopied = true }
                    DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
                        withAnimation { isCopied = false }
                    }
                }
                Spacer()
                Button(L("ok")) {
                    isPresented = false
                }
                .keyboardShortcut(.defaultAction)
            }
        }
        .padding(18)
        .frame(width: 440)
    }
}
