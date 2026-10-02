import Foundation

enum FileKindMap {
    /// Custom developer file extension -> human-readable name mapping.
    /// Priority: this map > UTType.localizedDescription > extension fallback.
    /// Per D-03: Hardcoded in Swift, no config.toml override in this phase.
    static let extensionToKind: [String: String] = [
        // Programming Languages — "[Language] Source"
        "rs": "Rust Source",
        "go": "Go Source",
        "py": "Python Source",
        "swift": "Swift Source",
        "ts": "TypeScript Source",
        "tsx": "TypeScript Source",
        "js": "JavaScript Source",
        "jsx": "JavaScript Source",
        "vue": "Vue Source",
        "svelte": "Svelte Source",
        "kt": "Kotlin Source",
        "dart": "Dart Source",
        "rb": "Ruby Source",
        "lua": "Lua Source",
        "zig": "Zig Source",
        "c": "C Source",
        "cpp": "C++ Source",
        "cc": "C++ Source",
        "cxx": "C++ Source",
        "h": "C Header",
        "hpp": "C++ Header",
        "m": "Objective-C Source",
        "mm": "Objective-C++ Source",
        "java": "Java Source",
        "cs": "C# Source",
        "scala": "Scala Source",
        "r": "R Source",
        "ex": "Elixir Source",
        "exs": "Elixir Script",
        "erl": "Erlang Source",
        "hs": "Haskell Source",
        "proto": "Protocol Buffer",

        // Configuration — "[Format] Configuration"
        "toml": "TOML Configuration",
        "yaml": "YAML Configuration",
        "yml": "YAML Configuration",
        "json": "JSON Configuration",
        "xml": "XML Document",
        "plist": "Property List",
        "ini": "INI Configuration",
        "env": "Environment Configuration",
        "lock": "Lock File",

        // Documentation — "[Format] Document"
        "md": "Markdown Document",
        "markdown": "Markdown Document",
        "rst": "reStructuredText Document",
        "txt": "Plain Text Document",

        // Scripts — "[Language] Script"
        "sh": "Shell Script",
        "bash": "Bash Script",
        "zsh": "Zsh Script",
        "fish": "Fish Script",

        // Web
        "html": "HTML Document",
        "htm": "HTML Document",
        "css": "CSS Stylesheet",
        "scss": "SCSS Stylesheet",
        "sass": "Sass Stylesheet",
        "less": "Less Stylesheet",

        // Data
        "sql": "SQL Script",
        "csv": "CSV Document",
        "tsv": "TSV Document",

        // Build/DevOps
        "dockerfile": "Dockerfile",
        "makefile": "Makefile",
        "cmake": "CMake Configuration",
        "gradle": "Gradle Build Script",
    ]
}
