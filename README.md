<p align="center">
  <img src="assets/icon.png" width="96" height="96" alt="Lokii Icon">
</p>

<h1 align="center">Lokii</h1>

<p align="center">
  <b>Ultra-fast, lightweight native macOS instant file search tool — like Everything for Mac</b>
</p>

<p align="center">
  <a href="README.md"><img src="https://img.shields.io/badge/lang-English-blue?style=flat-square" alt="English"></a>
  <a href="README.zh-CN.md"><img src="https://img.shields.io/badge/lang-简体中文-lightgrey?style=flat-square" alt="简体中文"></a>
</p>

<p align="center">
  <a href="https://github.com/huangy7/lokii/releases/latest"><b>Download for macOS (Apple Silicon & Intel)</b></a>
</p>

<p align="center">
  <a href="https://github.com/huangy7/lokii/releases/latest"><img src="https://img.shields.io/github/v/release/huangy7/lokii?style=flat-square&label=latest" alt="Latest release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/huangy7/lokii?style=flat-square" alt="License"></a>
  <img src="https://img.shields.io/badge/platform-macOS%2013%2B-blue?style=flat-square" alt="Platform">
  <a href="https://github.com/huangy7/lokii/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/huangy7/lokii/ci.yml?branch=main&style=flat-square&label=CI" alt="CI"></a>
  <a href="https://github.com/huangy7/lokii/releases"><img src="https://img.shields.io/github/downloads/huangy7/lokii/total?style=flat-square&color=success" alt="Downloads"></a>
  <img src="https://komarev.com/ghpvc/?username=huangy7-lokii&label=Views&color=0071e3&style=flat-square" alt="Views">
</p>

<p align="center">
  <img src="assets/screenshot-main.png" alt="Lokii instant search window" width="880">
</p>

Lokii is a native macOS instant file search tool engineered for developer-grade speed and desktop ergonomics — bringing the beloved, lightning-fast "Everything" file search experience to macOS. Powered by a high-performance in-memory index engine in Rust (`lokii-core`) bridged to a native Swift/AppKit user interface via UniFFI, Lokii delivers sub-millisecond query latency across hundreds of thousands of files with real-time FSEvents filesystem synchronization.

## Features

| Feature | Description |
| :--- | :--- |
| **Instant As-You-Type Search** | Sub-millisecond response latency across 500,000+ files with low memory footprint (under 100MB). |
| **Multi-Mode Query Engine** | Intelligent mode detection supporting substring, wildcard (`*`, `?`), regular expression (`^...$`), and fuzzy matching. |
| **Real-Time FSEvents Stream** | Multi-channel FSEvents event stream with dirty-queue debouncing to keep the index in sync without high CPU overhead. |
| **Native AppKit Density** | High-density virtualized table view, native macOS file icons, zebra striping, and spring-damped micro-interactions. |
| **Keyboard-First Workflow** | Global shortcut toggle (`⌘⇧Space`), arrow navigation, `Enter` to reveal in Finder, `⌘C` to copy path, and `Esc` to hide. |
| **Persistent Binary Cache** | Versioned bitcode cache with shared string tables, automatic loading of legacy v3 caches, and v4 saves after background verification. |
| **Native Settings & FDA** | Native Preferences window with custom search roots, exclusion rules, theme switching (Light/Dark/System), and interactive Full Disk Access setup. |

The compact index shares filenames, parent paths, and extensions to reduce duplicate storage. Search materializes only the selected results while preserving matching and ordering behavior. Initialization failures display an error instead of reporting an empty index as ready. Whole-app performance validation is still pending; core-process measurements are not whole-app guarantees.

## Why Lokii?

macOS comes with powerful system search tools, yet developers and power users consistently miss the simplicity and raw speed of Windows tools like voidtools Everything. Lokii was built to fill that gap.

### Lokii vs. Spotlight

- **Instant Filename Indexing**: Spotlight is designed for comprehensive metadata, email, and deep content indexing. In contrast, Lokii focuses exclusively on blazing-fast file and folder name indexing. It avoids the heavy background disk thrashing and indexing delays typical of Spotlight.
- **Deep Directory Visibility**: Spotlight frequently excludes, deprioritizes, or delays indexing hidden directories, build artifacts, and nested developer folders (`node_modules`, `target`, `.git`, `.venv`). Lokii scans and tracks the entire filesystem uniformly via direct FSEvents integration.
- **Developer Query Modes**: Lokii natively supports wildcards (`*.rs`, `test_*.swift`), regular expressions (`^IMG_.*\.png$`), and exact extension filters, giving developers precise control without complex Spotlight syntax.

### Lokii vs. Raycast & Alfred

- **Dedicated High-Performance Architecture**: While Raycast and Alfred are versatile productivity launchers, their file search commands rely under the hood on macOS's built-in Spotlight metadata service. When Spotlight has not indexed a file or is struggling with load, launcher file search inherits the exact same delay.
- **Ultra-Lightweight Resource Footprint**: Lokii's in-memory inverted index is written in Rust with compacted bitcode string tables, consuming under 100 MB of RAM while indexing over 500,000 files. It is an instant, keyboard-driven utility that opens in under 10 milliseconds and disappears the moment you press Escape.

## Privacy

Lokii runs 100% locally on your Mac. There are no user accounts, no telemetry, no tracking, and zero outbound network connections. Your filenames, directory structures, and search queries never leave your device.

## Architecture

```
lokii-core/       Rust library — in-memory inverted index, parallel scanner, FSEvents watcher, bitcode cache
Lokii/            macOS Swift/AppKit application — virtualized table, settings, tray integration
scripts/          Build and packaging automation (UniFFI bindings generation, DMG creation)
```

## Build from Source

### Prerequisites

- macOS 13.0+
- Rust stable (2021 edition)
- Xcode 15+ (Swift 5.9+)

### Compiling and Running

```bash
# 1. Clone the repository
git clone https://github.com/huangy7/lokii.git
cd lokii

# 2. Build the app (generates UniFFI bindings & compiles Swift app)
make

# 3. Launch debug app
cd Lokii && .build/debug/Lokii
```

### Packaging DMG

To create a distributable `.app` and `.dmg`:

```bash
# Build for Apple Silicon (arm64)
make package ARCH=arm64

# Build for Intel (x86_64)
make package ARCH=x86_64
```

Output disk images will be generated in `output/Lokii-<arch>.dmg`.

## Community

- **GitHub Discussions**: [Ask questions, share workflows, and discuss ideas](https://github.com/huangy7/lokii/discussions)
- **Issue Tracker**: [Report bugs or submit feature requests](https://github.com/huangy7/lokii/issues)

## Contributing

Contributions, bug reports, and suggestions are welcome! Please check out [CONTRIBUTING.md](CONTRIBUTING.md) for details on setting up your local development environment and submitting pull requests.

## Contributors

<p align="center">
  <a href="https://github.com/huangy7/lokii/graphs/contributors">
    <img src="https://contrib.rocks/image?repo=huangy7/lokii&max=100" alt="Contributors" />
  </a>
</p>

## License

Released under the [MIT](LICENSE) License.

---

<p align="center">
  Developed and maintained by <a href="https://github.com/huangy7">huangy7</a>
</p>
