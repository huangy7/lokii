#!/usr/bin/env bash

# ==============================================================================
# Lokii 统一构建与发布脚本
#
# 用法:
#   ./scripts/build.sh <command> [options]
#
# 子命令:
#   bindings    - 编译 Rust 核心并生成 UniFFI Swift 绑定与头文件
#   app         - 编译 Rust 与 Swift 并组装 .app Bundle（支持 --debug 或 --release）
#   dmg         - 完整发布流水线（编译、组装、代码签名、公证、生成 DMG 镜像）
#   help        - 显示帮助说明
#
# 选项:
#   --arch=<arm64|x86_64>   指定目标架构（默认自动检测当前系统架构）
#   --debug                 构建 Debug 版本
#   --release               构建 Release 优化版本（app / dmg 默认即 release）
# ==============================================================================

set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "[FAIL] Lokii 仅支持在 macOS (Darwin) 环境下构建与打包。" >&2
    exit 1
fi

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
version="$(grep -m1 '^version = ' "$repo_root/lokii-core/Cargo.toml" | cut -d '"' -f2)"
build_number="$(git -C "$repo_root" rev-list --count HEAD 2>/dev/null || echo 1)"

command="${1:-help}"
if [[ $# -gt 0 ]]; then
    shift
fi

arch_flag=""
build_mode="release"
target_version=""

if [[ "$command" == "bump-version" || "$command" == "bump" ]]; then
    target_version="${1:-}"
else
    for arg in "$@"; do
        case "$arg" in
            --arch=arm64)   arch_flag="arm64" ;;
            --arch=x86_64)  arch_flag="x86_64" ;;
            --debug)        build_mode="debug" ;;
            --release)      build_mode="release" ;;
            --arch=*)       echo "[FAIL] 未知架构: $arg（仅支持 arm64 / x86_64）" >&2; exit 1 ;;
            *)              echo "[FAIL] 未知参数: $arg" >&2; exit 1 ;;
        esac
    done
fi

if [[ -z "$arch_flag" ]]; then
    arch_flag="$(uname -m)"
fi

case "$arch_flag" in
    arm64)   rust_target="aarch64-apple-darwin" ;;
    x86_64)  rust_target="x86_64-apple-darwin" ;;
    *)       echo "[FAIL] 不支持的架构: $arch_flag" >&2; exit 1 ;;
esac

app_name="Lokii"
bundle_id="com.lokii.app"
output_dir="$repo_root/output"
bundle_dir="$output_dir/$app_name.app"
dmg_path="$output_dir/${app_name}-${arch_flag}.dmg"
entitlements="$repo_root/Lokii/Lokii/Lokii.entitlements"
signing_identity="${LOKII_SIGN_IDENTITY:-}"
notary_profile="${LOKII_NOTARY_PROFILE:-}"

show_help() {
    cat <<EOF
Lokii 统一构建与发布脚本

用法:
  ./scripts/build.sh <command> [options]

子命令:
  bindings               仅生成 UniFFI Swift 绑定与头文件
  app                    编译并组装 .app Bundle（默认 release 模式）
  dmg                    完整流水线打包并生成 DMG 安装镜像
  bump-version <version> 一键更新全量版本号并自动同步 Cargo.lock 与 Info.plist
  help                   显示本帮助信息

选项:
  --arch=<arm64|x86_64>   指定目标架构（默认: $(uname -m)）
  --debug                 构建 Debug 版本
  --release               构建 Release 优化版本（默认）
EOF
}

generate_bindings() {
    echo "==> [1/2] 编译 Rust 核心库 ($rust_target, release)..."
    cargo build -p lokii-core --lib --release --target "$rust_target" --manifest-path "$repo_root/Cargo.toml"

    echo "==> [2/2] 生成 UniFFI Swift 绑定与头文件..."
    local out_dir
    out_dir="$(mktemp -d)"

    cargo run -p lokii-core --bin uniffi-bindgen --manifest-path "$repo_root/Cargo.toml" -- generate \
        --library "$repo_root/target/$rust_target/release/liblokii_core.dylib" \
        --language swift \
        --out-dir "$out_dir"

    local swift_dir="$repo_root/Lokii"
    cp "$out_dir/lokii_core.swift" "$swift_dir/Lokii/lokii_core.swift"
    cp "$out_dir/lokii_coreFFI.h" "$swift_dir/LokiiCore/Headers/lokii_coreFFI.h"
    cp "$out_dir/lokii_coreFFI.modulemap" "$swift_dir/LokiiCore/Headers/module.modulemap"

    rm -rf "$out_dir"
    echo "==> UniFFI 绑定更新完成"
}

build_app() {
    generate_bindings

    echo "==> 编译 Swift 原生应用 ($build_mode, $arch_flag)..."
    cd "$repo_root/Lokii"

    local swift_flags=("--arch" "$arch_flag")
    if [[ "$build_mode" == "release" ]]; then
        swift_flags+=("-c" "release")
    fi

    LOKII_RUST_TARGET="$rust_target" swift build "${swift_flags[@]}"
    local bin_dir
    bin_dir="$(LOKII_RUST_TARGET="$rust_target" swift build "${swift_flags[@]}" --show-bin-path)"

    echo "==> 组装 .app Bundle..."
    mkdir -p "$output_dir"
    local xcassets="$repo_root/Lokii/Lokii/Assets.xcassets"
    local icns_path="$output_dir/Lokii.icns"
    local iconset_tmp
    iconset_tmp="$(mktemp -d)/Lokii.iconset"
    mkdir -p "$iconset_tmp"

    # 生成应用图标 (.icns)
    xcrun actool \
        --compile "$output_dir" \
        --platform macosx \
        --minimum-deployment-target 13.0 \
        --output-format human-readable-text \
        "$xcassets" >/dev/null 2>&1 || true

    if [[ ! -f "$output_dir/AppIcon.icns" ]]; then
        sips -s format png -z 512 512 \
            /System/Library/CoreServices/CoreTypes.bundle/Contents/Resources/GenericApplicationIcon.icns \
            --out "$iconset_tmp/icon_512x512.png" 2>/dev/null || true
        iconutil -c icns "$iconset_tmp" -o "$icns_path" 2>/dev/null || \
            cp /System/Library/CoreServices/CoreTypes.bundle/Contents/Resources/GenericApplicationIcon.icns "$icns_path"
    else
        mv "$output_dir/AppIcon.icns" "$icns_path"
    fi
    rm -rf "$(dirname "$iconset_tmp")"

    # 组装 Bundle 目录结构
    rm -rf "$bundle_dir"
    mkdir -p "$bundle_dir/Contents/MacOS" "$bundle_dir/Contents/Resources"

    cp "$bin_dir/Lokii" "$bundle_dir/Contents/MacOS/Lokii"
    cp "$icns_path"     "$bundle_dir/Contents/Resources/Lokii.icns"
    chmod +x "$bundle_dir/Contents/MacOS/Lokii"

    # 拷贝多语言资源到标准 macOS Contents/Resources/
    cp -R "$repo_root/Lokii/Lokii/Resources/"* "$bundle_dir/Contents/Resources/"

    # 拷贝依赖包资源束（若有）至标准 Contents/Resources/
    find "$bin_dir" -maxdepth 1 -name "*.bundle" | while read -r b; do
        [[ -e "$b" ]] || continue
        cp -R "$b" "$bundle_dir/Contents/Resources/"
    done

    # 生成 Info.plist
    cat <<EOF > "$bundle_dir/Contents/Info.plist"
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>zh-Hans</string>
    <key>CFBundleDisplayName</key>
    <string>${app_name}</string>
    <key>CFBundleExecutable</key>
    <string>Lokii</string>
    <key>CFBundleIconFile</key>
    <string>Lokii</string>
    <key>CFBundleIdentifier</key>
    <string>${bundle_id}</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>${app_name}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>${version}</string>
    <key>CFBundleVersion</key>
    <string>${build_number}</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
    <key>LSUIElement</key>
    <true/>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
</dict>
</plist>
EOF

    plutil -lint "$bundle_dir/Contents/Info.plist" >/dev/null

    # 签名 .app（若无身份则进行 ad-hoc 签名）
    if [[ -n "$signing_identity" ]]; then
        echo "==> 使用正式开发者证书签名: $signing_identity"
        find "$bundle_dir" -name "*.bundle" | while read -r b; do
            codesign --force --options runtime --timestamp \
                --entitlements "$entitlements" --sign "$signing_identity" "$b"
        done
        codesign --force --options runtime --timestamp \
            --entitlements "$entitlements" --sign "$signing_identity" \
            "$bundle_dir"
        codesign --verify --deep --strict --verbose=2 "$bundle_dir"
    else
        echo "==> 执行 Ad-hoc 本地代码签名..."
        find "$bundle_dir" -name "*.bundle" | while read -r b; do
            codesign --force --sign - "$b"
        done
        codesign --force --sign - "$bundle_dir"
    fi

    echo "==> App Bundle 组装完成: $bundle_dir"
}

package_dmg() {
    build_mode="release"
    build_app

    # 公证 .app（如果配置了公证凭证）
    if [[ -n "$signing_identity" && -n "$notary_profile" ]]; then
        echo "==> 提交应用公证..."
        local tmp_zip="$output_dir/${app_name}-notarize-tmp.zip"
        ditto -c -k --keepParent "$bundle_dir" "$tmp_zip"
        xcrun notarytool submit "$tmp_zip" --keychain-profile "$notary_profile" --wait
        xcrun stapler staple -v "$bundle_dir"
        rm -f "$tmp_zip"
    fi

    echo "==> 创建 DMG 镜像: $dmg_path"
    rm -f "$dmg_path"

    if command -v create-dmg &>/dev/null; then
        create-dmg \
            --volname "$app_name" \
            --window-pos 200 120 \
            --window-size 560 340 \
            --icon-size 96 \
            --text-size 13 \
            --icon "$app_name.app" 150 175 \
            --hide-extension "$app_name.app" \
            --app-drop-link 400 175 \
            --no-internet-enable \
            "$dmg_path" \
            "$bundle_dir"
    else
        echo "  提示: 未检测到 create-dmg 工具，回退至系统 hdiutil 生成基础 DMG 镜像..." >&2
        local tmp_dmg_dir
        tmp_dmg_dir="$(mktemp -d)"
        cp -R "$bundle_dir" "$tmp_dmg_dir/"
        ln -s /Applications "$tmp_dmg_dir/Applications"
        hdiutil create \
            -volname "$app_name" \
            -srcfolder "$tmp_dmg_dir" \
            -ov \
            -format UDZO \
            "$dmg_path"
        rm -rf "$tmp_dmg_dir"
    fi

    # 签名 DMG
    if [[ -n "$signing_identity" ]]; then
        codesign --force --sign "$signing_identity" --timestamp "$dmg_path"
    fi

    # 公证 DMG
    if [[ -n "$signing_identity" && -n "$notary_profile" ]]; then
        xcrun notarytool submit "$dmg_path" --keychain-profile "$notary_profile" --wait
        xcrun stapler staple -v "$dmg_path"
    fi

    echo ""
    echo "================================================================="
    echo "构建与打包完成:"
    echo "  App Bundle:  $bundle_dir"
    echo "  DMG 镜像:    $dmg_path"
    [[ -n "$signing_identity" ]] && echo "  签名状态:    $signing_identity" || echo "  签名状态:    Ad-hoc"
    echo "================================================================="
}

bump_version() {
    local new_ver="${1:-}"
    if [[ -z "$new_ver" ]]; then
        echo "[FAIL] 请指定版本号，例如: ./scripts/build.sh bump-version 0.1.0" >&2
        exit 1
    fi
    new_ver="${new_ver#v}"

    echo "[INFO] 正在将 Lokii 项目版本更新为: $new_ver"

    local cargo_toml="$repo_root/lokii-core/Cargo.toml"
    sed -i '' "s/^version = \".*\"/version = \"$new_ver\"/" "$cargo_toml"

    local info_plist="$repo_root/Lokii/Lokii/Info.plist"
    if [[ -f "$info_plist" ]]; then
        sed -i '' -E "/<key>CFBundleShortVersionString<\/key>/ { n; s|<string>.*</string>|<string>${new_ver}</string>|; }" "$info_plist"
    fi

    echo "[INFO] 正在同步 Cargo.lock..."
    (cd "$repo_root" && cargo check --workspace --quiet)

    echo "[OK] 版本更新完成: $new_ver"
    echo "  - lokii-core/Cargo.toml: $(grep -m1 '^version = ' "$cargo_toml")"
    echo "  - Lokii/Lokii/Info.plist: $(grep -A1 'CFBundleShortVersionString' "$info_plist" | tr '\n' ' ')"
}

case "$command" in
    bindings|generate)
        generate_bindings
        ;;
    app)
        build_app
        ;;
    dmg|package)
        package_dmg
        ;;
    bump-version|bump)
        bump_version "$target_version"
        ;;
    help|--help|-h)
        show_help
        ;;
    *)
        echo "[FAIL] 未知子命令: $command" >&2
        show_help
        exit 1
        ;;
esac
