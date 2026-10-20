import Cocoa

// MARK: - Standalone Test Harness for LokiiConfig.IndexConfig

let searchEnginePath = "Lokii/Lokii/SearchEngine.swift"
guard FileManager.default.fileExists(atPath: searchEnginePath) else {
    fputs("tests/index_config_test.swift: error: 找不到 \(searchEnginePath)\n", stderr)
    exit(1)
}

guard let searchEngineSource = try? String(contentsOfFile: searchEnginePath, encoding: .utf8) else {
    fputs("tests/index_config_test.swift: error: 无法读取 \(searchEnginePath)\n", stderr)
    exit(1)
}

guard let startRange = searchEngineSource.range(of: "struct LokiiConfig: Codable {"),
      let endRange = searchEngineSource.range(of: "final class SearchEngine") else {
    fputs("tests/index_config_test.swift: error: 未能在 \(searchEnginePath) 中定位 LokiiConfig\n", stderr)
    exit(1)
}

let configSection = searchEngineSource[startRange.lowerBound..<endRange.lowerBound]
guard let lastBrace = configSection.range(of: "}", options: .backwards) else {
    fputs("tests/index_config_test.swift: error: 未能在 LokiiConfig 片段中找到闭合括号\n", stderr)
    exit(1)
}
let lokiiConfigCode = String(configSection[..<lastBrace.upperBound])

let runnerSource = """
import Cocoa

\(lokiiConfigCode)

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

do {
    print("[1/3] 测试 IndexConfig 具备 exclude_paths 时的正常反序列化...")
    let jsonWithExcludePaths = \"\"\"
    {
        "index_paths": ["~", "/Volumes/Data"],
        "exclude": ["node_modules", ".git"],
        "exclude_paths": ["~/Library", "/System"],
        "include_hidden": false,
        "max_results": 200
    }
    \"\"\".data(using: .utf8)!

    let decoder = JSONDecoder()
    let config1 = try decoder.decode(LokiiConfig.IndexConfig.self, from: jsonWithExcludePaths)
    assertEqual(config1.indexPaths, ["~", "/Volumes/Data"], "indexPaths 解析匹配")
    assertEqual(config1.exclude, ["node_modules", ".git"], "exclude 解析匹配")
    assertEqual(config1.excludePaths, ["~/Library", "/System"], "excludePaths 解析匹配")
    assertEqual(config1.maxResults, 200, "maxResults 解析匹配")

    print("[2/3] 测试 IndexConfig 缺少 exclude_paths 时的优雅回退（默认空数组）...")
    let jsonWithoutExcludePaths = \"\"\"
    {
        "index_paths": ["~"],
        "exclude": ["target"],
        "include_hidden": true,
        "max_results": 500
    }
    \"\"\".data(using: .utf8)!

    let config2 = try decoder.decode(LokiiConfig.IndexConfig.self, from: jsonWithoutExcludePaths)
    assertEqual(config2.indexPaths, ["~"], "indexPaths 正常")
    assertEqual(config2.exclude, ["target"], "exclude 正常")
    assertEqual(config2.excludePaths, [], "缺少 exclude_paths 时回退为空数组")
    assertTrue(config2.includeHidden, "includeHidden 为 true")

    print("[3/3] 测试 IndexConfig 序列化能正确输出 exclude_paths 字段...")
    let encoder = JSONEncoder()
    let encodedData = try encoder.encode(config1)
    let encodedJSON = String(data: encodedData, encoding: .utf8)!
    assertTrue(encodedJSON.contains("\\"exclude_paths\\":"), "JSON 中应包含 exclude_paths 字段")
    print("IndexConfig 编解码与回退测试全部通过！")
} catch {
    fputs("测试执行抛出异常: \\(error)\\n", stderr)
    exit(1)
}
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
