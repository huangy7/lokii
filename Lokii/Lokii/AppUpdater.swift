import Foundation
import AppKit

struct ReleaseAsset: Decodable {
    let name: String
    let browserDownloadUrl: String
    let size: Int
    
    enum CodingKeys: String, CodingKey {
        case name
        case browserDownloadUrl = "browser_download_url"
        case size
    }
}

struct GitHubRelease: Decodable {
    let tagName: String
    let name: String?
    let body: String?
    let htmlUrl: String
    let publishedAt: String?
    let assets: [ReleaseAsset]
    
    enum CodingKeys: String, CodingKey {
        case tagName = "tag_name"
        case name
        case body
        case htmlUrl = "html_url"
        case publishedAt = "published_at"
        case assets
    }
}

@MainActor
final class AppUpdater: ObservableObject {
    static let shared = AppUpdater()
    
    @Published var isChecking: Bool = false
    @Published var hasNewVersion: Bool = false
    @Published var latestRelease: GitHubRelease? = nil
    @Published var lastCheckTime: Date? = nil
    @Published var errorMessage: String? = nil
    
    var currentVersion: String {
        Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "0.1.0"
    }
    
    var currentArch: String {
        #if arch(arm64)
        return "arm64"
        #else
        return "x86_64"
        #endif
    }
    
    func checkForUpdates(isUserInitiated: Bool = false) {
        guard !isChecking else { return }
        isChecking = true
        errorMessage = nil
        
        guard let url = URL(string: "https://api.github.com/repos/huangy7/lokii/releases/latest") else {
            isChecking = false
            return
        }
        
        var request = URLRequest(url: url)
        request.setValue("application/vnd.github+json", forHTTPHeaderField: "Accept")
        request.setValue("Lokii-App", forHTTPHeaderField: "User-Agent")
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.timeoutInterval = 10
        
        Task {
            do {
                let (data, response) = try await URLSession.shared.data(for: request)
                guard let httpResponse = response as? HTTPURLResponse, httpResponse.statusCode == 200 else {
                    throw URLError(.badServerResponse)
                }
                
                let release = try JSONDecoder().decode(GitHubRelease.self, from: data)
                self.lastCheckTime = Date()
                self.latestRelease = release
                
                let remoteVer = release.tagName.hasPrefix("v") ? String(release.tagName.dropFirst()) : release.tagName
                if Self.compareVersion(remoteVer, isGreaterThan: currentVersion) {
                    self.hasNewVersion = true
                } else {
                    self.hasNewVersion = false
                }
                self.isChecking = false
            } catch {
                self.isChecking = false
                if isUserInitiated {
                    self.errorMessage = error.localizedDescription
                }
            }
        }
    }
    
    /// 语义化版本号比较 (例如 "0.2.0" > "0.1.0")
    static func compareVersion(_ v1: String, isGreaterThan v2: String) -> Bool {
        let parts1 = v1.split(separator: ".").compactMap { Int($0) }
        let parts2 = v2.split(separator: ".").compactMap { Int($0) }
        
        let maxCount = max(parts1.count, parts2.count)
        for i in 0..<maxCount {
            let p1 = i < parts1.count ? parts1[i] : 0
            let p2 = i < parts2.count ? parts2[i] : 0
            if p1 > p2 { return true }
            if p1 < p2 { return false }
        }
        return false
    }
    
    /// 获取适配当前架构的 DMG 下载链接
    var matchingAsset: ReleaseAsset? {
        guard let assets = latestRelease?.assets else { return nil }
        return assets.first { $0.name.hasSuffix(".dmg") && $0.name.contains(currentArch) }
            ?? assets.first { $0.name.hasSuffix(".dmg") }
    }
    
    func openDownloadPage() {
        if let asset = matchingAsset, let url = URL(string: asset.browserDownloadUrl) {
            NSWorkspace.shared.open(url)
        } else if let release = latestRelease, let url = URL(string: release.htmlUrl) {
            NSWorkspace.shared.open(url)
        } else if let defaultUrl = URL(string: "https://github.com/huangy7/lokii/releases") {
            NSWorkspace.shared.open(defaultUrl)
        }
    }
    
    func openReleaseNotes() {
        if let release = latestRelease, let url = URL(string: release.htmlUrl) {
            NSWorkspace.shared.open(url)
        }
    }
}
