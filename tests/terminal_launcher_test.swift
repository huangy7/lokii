import Cocoa
import UniformTypeIdentifiers

// MARK: - Standalone Test Harness for TerminalLauncher & GeneralConfig

let searchEnginePath = "Lokii/Lokii/SearchEngine.swift"
let terminalLauncherPath = "Lokii/Lokii/TerminalLauncher.swift"

guard FileManager.default.fileExists(atPath: searchEnginePath),
      FileManager.default.fileExists(atPath: terminalLauncherPath) else {
    fputs("tests/terminal_launcher_test.swift: error: 找不到源码文件\n", stderr)
    exit(1)
}

guard let searchEngineSource = try? String(contentsOfFile: searchEnginePath, encoding: .utf8),
      let terminalLauncherSource = try? String(contentsOfFile: terminalLauncherPath, encoding: .utf8) else {
    fputs("tests/terminal_launcher_test.swift: error: 无法读取源码文件\n", stderr)
    exit(1)
}

guard let startRange = searchEngineSource.range(of: "struct LokiiConfig: Codable {"),
      let endRange = searchEngineSource.range(of: "final class SearchEngine") else {
    fputs("tests/terminal_launcher_test.swift: error: 未能在 SearchEngine.swift 中定位 LokiiConfig\n", stderr)
    exit(1)
}

let configSection = searchEngineSource[startRange.lowerBound..<endRange.lowerBound]
guard let lastBrace = configSection.range(of: "}", options: .backwards) else {
    fputs("tests/terminal_launcher_test.swift: error: 未能在 LokiiConfig 片段中找到闭合括号\n", stderr)
    exit(1)
}
let lokiiConfigCode = String(configSection[..<lastBrace.upperBound])

let runnerSource = """
import Cocoa
import UniformTypeIdentifiers

\(lokiiConfigCode)

\(terminalLauncherSource)

func assertEqual<T: Equatable>(_ actual: T, _ expected: T, _ message: String = "", file: StaticString = #file, line: UInt = #line) {
    if actual != expected {
        fputs("断言失败: [\\(actual)] != [\\(expected)] - \\(message) (\\(file):\\(line))\\n", stderr)
        exit(1)
    }
}

func assertTrue(_ condition: Bool, _ message: String = "", file: StaticString = #file, line: UInt = #line) {
    if !condition {
        fputs("断言失败: 条件不满足 - \\(message) (\\(file):\\(line))\\n", stderr)
        exit(1)
    }
}

// [1/6] 测试 LokiiConfig.GeneralConfig 缺失 terminal_bundle_id 时优雅回退至默认值
print("[1/6] 测试 LokiiConfig.GeneralConfig 缺失 terminal_bundle_id 时优雅回退至默认值...")
let legacyGeneralJSON = \"\"\"
{
    "language": "en",
    "show_in_dock": false
}
\"\"\".data(using: .utf8)!

let decodedLegacy = try! JSONDecoder().decode(LokiiConfig.GeneralConfig.self, from: legacyGeneralJSON)
assertEqual(decodedLegacy.language, "en", "语言字段应正确解码")
assertEqual(decodedLegacy.showInDock, false, "showInDock 应正确解码")
assertEqual(decodedLegacy.terminalBundleId, "com.apple.Terminal", "缺失 terminal_bundle_id 时应自动兜底回系统终端")

// [2/6] 测试 LokiiConfig.GeneralConfig 具备 terminal_bundle_id 时的正常编解码往返
print("[2/6] 测试 LokiiConfig.GeneralConfig 具备 terminal_bundle_id 时的正常编解码往返...")
let modernGeneral = LokiiConfig.GeneralConfig(language: "zh-Hans", showInDock: true, terminalBundleId: "com.googlecode.iterm2")
let encodedData = try! JSONEncoder().encode(modernGeneral)
let decodedModern = try! JSONDecoder().decode(LokiiConfig.GeneralConfig.self, from: encodedData)
assertEqual(decodedModern.terminalBundleId, "com.googlecode.iterm2", "terminal_bundle_id 编解码往返后应完全一致")

// [3/6] 测试 TerminalLauncher.installedTerminals() 探活并确保系统终端必选命中
print("[3/6] 测试 TerminalLauncher.installedTerminals() 探活并确保系统终端必选命中...")
let installed = TerminalLauncher.installedTerminals()
assertTrue(!installed.isEmpty, "探活终端列表不应为空")
let hasSystemTerminal = installed.contains { $0.id == "com.apple.Terminal" }
assertTrue(hasSystemTerminal, "探活结果中必须包含系统终端 com.apple.Terminal 作为基线保障")
for item in installed {
    assertTrue(!item.name.isEmpty, "终端项名称 \\(item.id) 不能为空")
    assertTrue(FileManager.default.fileExists(atPath: item.appURL.path), "已探活终端路径 \\(item.appURL.path) 必须在物理磁盘上存在")
}

// [4/6] 测试 TerminalLauncher.resolveApplicationURL 针对有效与失效标识符的自愈降级回退
print("[4/6] 测试 TerminalLauncher.resolveApplicationURL 针对有效与失效标识符的自愈降级回退...")
let (systemURL, isFallback1) = TerminalLauncher.resolveApplicationURL(for: "com.apple.Terminal")
assertEqual(isFallback1, false, "系统终端标识不应被判定为回退降级")
assertTrue(FileManager.default.fileExists(atPath: systemURL.path), "系统终端 URL 必须物理存在")

let (invalidURL, isFallback2) = TerminalLauncher.resolveApplicationURL(for: "com.nonexistent.fake.terminal")
assertEqual(isFallback2, true, "未知或未安装的终端标识必须触发自愈回退")
assertEqual(invalidURL.path, systemURL.path, "自愈降级后应直接指向系统终端路径")

let (emptyURL, isFallback3) = TerminalLauncher.resolveApplicationURL(for: nil)
assertEqual(isFallback3, false, "传参 nil 应自然使用默认系统终端")
assertEqual(emptyURL.path, systemURL.path, "空传参应解析为系统终端")

// [5/6] 测试自定义终端项追加
print("[5/6] 测试自定义终端项追加...")
let customInstalled = TerminalLauncher.installedTerminals(selectedId: systemURL.path)
let containsCustom = customInstalled.contains { $0.id == systemURL.path }
assertTrue(containsCustom, "传入自定义有效路径时应被收纳进已安装列表")

// [6/6] 测试目录推导、有序去重与最多 5 窗口限流保护
print("[6/6] 测试 openBatch 路径推导、有序去重与最多 5 窗口限流保护...")
// 模拟传递 8 个目标路径，其中前 3 个属于同一目录，后续 6 个各属于不同目录（合计 7 个唯一目录）
let tempRoot = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
try! FileManager.default.createDirectory(at: tempRoot, withIntermediateDirectories: true)
defer { try? FileManager.default.removeItem(at: tempRoot) }

var testTargetURLs: [URL] = []
// 同一目录下的 3 个文件
let subDirA = tempRoot.appendingPathComponent("dirA")
try! FileManager.default.createDirectory(at: subDirA, withIntermediateDirectories: true)
for i in 1...3 {
    let fileURL = subDirA.appendingPathComponent("file\\(i).txt")
    FileManager.default.createFile(atPath: fileURL.path, contents: nil)
    testTargetURLs.append(fileURL)
}

// 另外 6 个独立子目录
for i in 1...6 {
    let subDir = tempRoot.appendingPathComponent("subDir\\(i)")
    try! FileManager.default.createDirectory(at: subDir, withIntermediateDirectories: true)
    testTargetURLs.append(subDir)
}

// 直接验证生产代码的目录推导、去重与截断逻辑
let uniqueDirectoryURLs = TerminalLauncher.uniqueDirectories(from: testTargetURLs)
assertEqual(uniqueDirectoryURLs.count, 7, "7 个唯一目录应被去重识别")
let throttled = TerminalLauncher.uniqueDirectories(from: testTargetURLs, limit: TerminalLauncher.maxBatchOpenCount)
assertEqual(throttled.count, 5, "超过上限的目录数应被截断为最大限流数 5")

print("TerminalLauncher 全部测试通过！")
"""

let tempDir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
try! FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
defer { try? FileManager.default.removeItem(at: tempDir) }

let harnessFile = tempDir.appendingPathComponent("main.swift")
try! runnerSource.write(to: harnessFile, atomically: true, encoding: .utf8)

let process = Process()
process.executableURL = URL(fileURLWithPath: "/usr/bin/swift")
process.arguments = [harnessFile.path]
process.standardOutput = FileHandle.standardOutput
process.standardError = FileHandle.standardError

try! process.run()
process.waitUntilExit()

if process.terminationStatus != 0 {
    exit(process.terminationStatus)
}
