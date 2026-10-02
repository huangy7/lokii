// swift-tools-version: 5.9
import PackageDescription
import Foundation

// 支持通过环境变量指定 Rust target，默认 aarch64-apple-darwin
let rustTarget = ProcessInfo.processInfo.environment["LOKII_RUST_TARGET"] ?? "aarch64-apple-darwin"
let rustLibPath = "../target/\(rustTarget)/release"

let package = Package(
    name: "Lokii",
    defaultLocalization: "zh-Hans",
    platforms: [.macOS(.v13)],
    dependencies: [],
    targets: [
        .systemLibrary(
            name: "lokii_coreFFI",
            path: "LokiiCore/Headers"
        ),
        .executableTarget(
            name: "Lokii",
            dependencies: [
                "lokii_coreFFI",
            ],
            path: "Lokii",
            exclude: ["Info.plist", "Lokii.entitlements", "Assets.xcassets", "Resources"],
            linkerSettings: [
                // 强制静态链接，避免运行时依赖 dylib 路径
                .unsafeFlags(["\(rustLibPath)/liblokii_core.a"]),
            ]
        ),
    ]
)
