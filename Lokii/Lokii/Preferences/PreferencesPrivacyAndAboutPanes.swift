import SwiftUI
import AppKit

// MARK: - Privacy Preferences Pane
struct PreferencesPrivacyPane: View {
    @ObservedObject var viewModel: PreferencesViewModel

    var body: some View {
        Form {
            Section(L("prefs.privacy.fullDiskAccess")) {
                LabeledContent {
                    HStack(spacing: 6) {
                        Circle()
                            .fill(viewModel.hasFullDiskAccess ? Color.green : Color.orange)
                            .frame(width: 8, height: 8)
                        Text(viewModel.hasFullDiskAccess ? L("prefs.privacy.badge.granted") : L("prefs.privacy.badge.missing"))
                            .font(.system(size: 11, weight: .medium))
                            .foregroundStyle(viewModel.hasFullDiskAccess ? .green : .orange)
                    }
                } label: {
                    SettingsRowLabel(
                        viewModel.hasFullDiskAccess ? L("prefs.privacy.status.granted") : L("prefs.privacy.status.missing")
                    )
                }

                VStack(alignment: .leading, spacing: 10) {
                    Text(viewModel.hasFullDiskAccess ? L("prefs.privacy.status.granted.description") : L("prefs.privacy.status.missing.description"))
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)

                    if !viewModel.hasFullDiskAccess {
                        HStack(spacing: 10) {
                            Button(L("prefs.privacy.viewGuide")) {
                                FDAGuideWindowController.show()
                            }
                            .buttonStyle(.bordered)

                            Button(L("prefs.privacy.openSettings")) {
                                FDAGuideWindowController.openSystemSettingsPage()
                            }
                            .buttonStyle(.borderedProminent)
                        }
                    } else {
                        Text(L("prefs.privacy.rebuildHint"))
                            .font(.system(size: 11))
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                .padding(.vertical, 4)
            }
        }
        .formStyle(.grouped)
    }
}

// MARK: - About Preferences Pane
struct PreferencesAboutPane: View {
    @ObservedObject private var updater = AppUpdater.shared

    var body: some View {
        Form {
            // Header
            Section {
                VStack(spacing: 8) {
                    if let appIcon = NSImage(named: "AppIcon") ?? NSApp.applicationIconImage {
                        Image(nsImage: appIcon)
                            .resizable()
                            .scaledToFit()
                            .frame(width: 76, height: 76)
                    } else {
                        Image(systemName: "magnifyingglass.circle.fill")
                            .resizable()
                            .scaledToFit()
                            .frame(width: 76, height: 76)
                            .foregroundStyle(Color.accentColor)
                            .clipShape(RoundedRectangle(cornerRadius: 18, style: .continuous))
                    }

                    Text("Lokii")
                        .font(.title2)
                        .bold()

                    let version = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? ""
                    if !version.isEmpty {
                        Text("v\(version)")
                            .font(.system(size: 12, design: .monospaced))
                            .foregroundStyle(.secondary)
                    }

                    Text(L("about.tagline"))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 8)
            }
            .listRowBackground(Color.clear)

            // Update Section
            Section(L("prefs.about.update.section")) {
                SettingsToggleRow(
                    L("prefs.about.update.automatic"),
                    isOn: Binding(
                        get: { updater.automaticallyChecksForUpdates },
                        set: { updater.automaticallyChecksForUpdates = $0 }
                    )
                )

                LabeledContent {
                    HStack(spacing: 8) {
                        if updater.isChecking {
                            ProgressView()
                                .controlSize(.small)
                            Text(L("prefs.about.update.checking"))
                                .font(.system(size: 11))
                                .foregroundStyle(.secondary)
                        } else if let error = updater.errorMessage {
                            Text(error)
                                .font(.system(size: 11))
                                .foregroundStyle(.red)
                                .lineLimit(1)
                        } else if updater.lastCheckTime != nil {
                            Text(L(updater.hasNewVersion ? "prefs.about.update.found" : "prefs.about.update.latest"))
                                .font(.system(size: 11))
                                .foregroundStyle(.secondary)
                        }

                        Button(L("prefs.about.update.checkNow")) {
                            updater.checkForUpdates(isUserInitiated: true)
                        }
                        .disabled(updater.isChecking)
                    }
                } label: {
                    SettingsRowLabel(L("prefs.about.update.status"))
                }

                if updater.errorMessage != nil {
                    LabeledContent {
                        Button(L("prefs.about.update.download")) {
                            updater.openDownloadPage()
                        }
                    } label: {
                        SettingsRowLabel(L("prefs.about.update.manualDownload"))
                    }
                }
            }

            // Info Section
            Section {
                LabeledContent(L("prefs.about.architecture")) {
                    Text(L("prefs.about.architecture.value"))
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                }

                LabeledContent(L("prefs.about.license")) {
                    Text(L("prefs.about.license.value"))
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                }

                LabeledContent(L("prefs.about.source")) {
                    Button(L("prefs.about.source.action")) {
                        if let url = URL(string: "https://github.com/huangy7/lokii") {
                            NSWorkspace.shared.open(url)
                        }
                    }
                    .buttonStyle(.link)
                }
            }
        }
        .formStyle(.grouped)
    }
}
