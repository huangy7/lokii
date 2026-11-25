import Cocoa
import UniformTypeIdentifiers
import XCTest
@testable import LokiiAppLib

final class TerminalLauncherTests: XCTestCase {
    func testGeneralConfigFallbackAndRoundtrip() throws {
        let legacyGeneralJSON = """
        {
            "language": "en",
            "show_in_dock": false
        }
        """.data(using: .utf8)!

        let decodedLegacy = try JSONDecoder().decode(LokiiConfig.GeneralConfig.self, from: legacyGeneralJSON)
        XCTAssertEqual(decodedLegacy.language, "en")
        XCTAssertEqual(decodedLegacy.showInDock, false)
        XCTAssertEqual(decodedLegacy.terminalBundleId, "com.apple.Terminal")

        let modernGeneral = LokiiConfig.GeneralConfig(
            language: "zh-Hans",
            showInDock: true,
            terminalBundleId: "com.googlecode.iterm2"
        )
        let encodedData = try JSONEncoder().encode(modernGeneral)
        let decodedModern = try JSONDecoder().decode(LokiiConfig.GeneralConfig.self, from: encodedData)
        XCTAssertEqual(decodedModern.terminalBundleId, "com.googlecode.iterm2")
    }

    func testInstalledTerminalsAndFallbackResolution() {
        let installed = TerminalLauncher.installedTerminals()
        XCTAssertFalse(installed.isEmpty)
        XCTAssertTrue(installed.contains { $0.id == "com.apple.Terminal" })
        for item in installed {
            XCTAssertFalse(item.name.isEmpty)
            XCTAssertTrue(FileManager.default.fileExists(atPath: item.appURL.path))
        }

        let (systemURL, isFallback1) = TerminalLauncher.resolveApplicationURL(for: "com.apple.Terminal")
        XCTAssertFalse(isFallback1)
        XCTAssertTrue(FileManager.default.fileExists(atPath: systemURL.path))

        let (invalidURL, isFallback2) = TerminalLauncher.resolveApplicationURL(for: "com.nonexistent.fake.terminal")
        XCTAssertTrue(isFallback2)
        XCTAssertEqual(invalidURL.path, systemURL.path)

        let (emptyURL, isFallback3) = TerminalLauncher.resolveApplicationURL(for: nil)
        XCTAssertFalse(isFallback3)
        XCTAssertEqual(emptyURL.path, systemURL.path)

        let customInstalled = TerminalLauncher.installedTerminals(selectedId: systemURL.path)
        XCTAssertTrue(customInstalled.contains { $0.id == systemURL.path })
    }

    func testUniqueDirectoriesAndBatchLimit() throws {
        let tempRoot = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: tempRoot, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tempRoot) }

        var testTargetURLs: [URL] = []
        let subDirA = tempRoot.appendingPathComponent("dirA")
        try FileManager.default.createDirectory(at: subDirA, withIntermediateDirectories: true)
        for i in 1...3 {
            let fileURL = subDirA.appendingPathComponent("file\(i).txt")
            FileManager.default.createFile(atPath: fileURL.path, contents: nil)
            testTargetURLs.append(fileURL)
        }

        for i in 1...6 {
            let subDir = tempRoot.appendingPathComponent("subDir\(i)")
            try FileManager.default.createDirectory(at: subDir, withIntermediateDirectories: true)
            testTargetURLs.append(subDir)
        }

        let uniqueDirectoryURLs = TerminalLauncher.uniqueDirectories(from: testTargetURLs)
        XCTAssertEqual(uniqueDirectoryURLs.count, 7)
        let throttled = TerminalLauncher.uniqueDirectories(from: testTargetURLs, limit: TerminalLauncher.maxBatchOpenCount)
        XCTAssertEqual(throttled.count, 5)
    }
}
