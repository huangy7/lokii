# Contributing to Lokii

Thank you for your interest in contributing to Lokii! Whether you are reporting a bug, proposing new features, improving documentation, or writing code, your help is welcome.

## Code of Conduct

Please be respectful, constructive, and considerate when interacting with other contributors and users across discussions, issues, and pull requests.

## Architecture Overview

Lokii is organized as a hybrid codebase:
- `lokii-core/`: A high-performance Rust core responsible for filesystem scanning, in-memory inverted indexing, FSEvents tracking, query matching (substring, regex, wildcards, fuzzy), and persistent bitcode caching.
- `Lokii/`: A native macOS application written in Swift and AppKit, handling user interface rendering, virtualized table views, global hotkeys, settings, and permissions.
- `scripts/`: Packaging and UniFFI bridging utilities.

## Development Setup

### Prerequisites

- macOS 13.0 or later
- Rust stable toolchain (2021 edition)
- Xcode 15+ (Swift 5.9+)
- Make

### Building Locally

```bash
# Clone the repository
git clone https://github.com/huangy7/lokii.git
cd lokii

# Build UniFFI bindings and compile the Swift app
make

# Run the debug app
cd Lokii && .build/debug/Lokii
```

### Running Tests

```bash
# Run Rust core tests
cd lokii-core && cargo test

# Run Swift application tests
cd ../Lokii && swift test
```

## Pull Request Guidelines

1. **Create a topic branch**: Branch off `main` for your work (e.g., `feat/search-filter` or `fix/crash-on-open`).
2. **Keep changes focused**: Smaller, targeted PRs are reviewed and merged faster.
3. **Write tests**: Add unit tests in Rust or Swift for any new logic or bug fixes.
4. **Follow conventions**:
   - Write clear, descriptive commit messages.
   - Do not commit generated build artifacts (such as `.build/`, `target/`, or `.dmg` files).
5. **Open a Pull Request**: Provide a clear description of the problem solved, changes made, and screenshots for any UI updates.

## Reporting Issues

If you find a bug or have a suggestion, please check existing issues before submitting a new one. When reporting bugs, include:
- macOS version and architecture (Apple Silicon vs. Intel)
- Lokii version / commit hash
- Clear steps to reproduce
- Expected vs. actual behavior
