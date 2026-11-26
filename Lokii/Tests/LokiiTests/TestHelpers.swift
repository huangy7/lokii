import Foundation
import XCTest
@testable import LokiiAppLib

enum TestFixture {
    /// 基于编译期 `#filePath` 定位仓库根目录，避免依赖测试执行时的工作目录。
    static let repoRoot: URL = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent() // LokiiTests
        .deletingLastPathComponent() // Tests
        .deletingLastPathComponent() // Lokii
        .deletingLastPathComponent() // repo root

    static func repoFileURL(_ relativePath: String) -> URL {
        repoRoot.appendingPathComponent(relativePath)
    }

    static func readRepoFile(
        _ relativePath: String,
        file: StaticString = #filePath,
        line: UInt = #line
    ) throws -> String {
        let url = repoFileURL(relativePath)
        XCTAssertTrue(
            FileManager.default.fileExists(atPath: url.path),
            "找不到仓库文件 \(relativePath)",
            file: file,
            line: line
        )
        return try String(contentsOf: url, encoding: .utf8)
    }

    static func searchResult(
        name: String,
        path: String,
        extension ext: String = "",
        isDir: Bool = false,
        size: UInt64 = 0,
        created: UInt64 = 0,
        modified: UInt64 = 0,
        score: Double = 0,
        matchPositions: [FfiMatchRange] = [],
        parentMatchPositions: [FfiMatchRange] = [],
        searchMode: FfiSearchMode = .auto,
        isOffline: Bool = false,
        volumeName: String? = nil
    ) -> FfiSearchResult {
        FfiSearchResult(
            name: name,
            path: path,
            isDir: isDir,
            size: size,
            modified: modified,
            created: created,
            extension: ext,
            score: score,
            matchPositions: matchPositions,
            parentMatchPositions: parentMatchPositions,
            searchMode: searchMode,
            isOffline: isOffline,
            volumeName: volumeName
        )
    }

    static func volumeStatus(
        volumeId: String = "vol-1",
        configuredPath: String,
        activePath: String? = nil,
        displayName: String = "Disk",
        kind: String = "network",
        state: FfiVolumeState,
        entryCount: UInt64 = 0,
        lastSyncAt: UInt64? = nil,
        isRemounted: Bool = false,
        hasCache: Bool = false,
        confirmReason: FfiVolumeConfirmReason? = nil,
        scannedCount: UInt64 = 0,
        supportsPersistentJournal: Bool = true
    ) -> FfiVolumeStatus {
        FfiVolumeStatus(
            volumeId: volumeId,
            configuredPath: configuredPath,
            activePath: activePath,
            displayName: displayName,
            kind: kind,
            state: state,
            entryCount: entryCount,
            lastSyncAt: lastSyncAt,
            isRemounted: isRemounted,
            hasCache: hasCache,
            confirmReason: confirmReason,
            scannedCount: scannedCount,
            supportsPersistentJournal: supportsPersistentJournal
        )
    }
}
