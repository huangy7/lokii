#!/usr/bin/env bash
set -euo pipefail

version="${1:?版本不能为空}"
assets="${2:?安装包目录不能为空}"
destination="${3:?清单输出目录不能为空}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
private_key="${LOKII_UPDATE_PRIVATE_KEY_FILE:?未配置更新签名私钥}"
[[ -f "$private_key" ]]
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
tools="${LOKII_UPDATE_TOOLS_DIR:-}"
if [[ -z "$tools" ]]; then
    tools="$(find "$repo_root/Lokii/.build/artifacts" -type f -name generate_appcast -print -quit)"
    [[ -n "$tools" ]] || { echo '[FAIL] 更新发布工具缺失' >&2; exit 1; }
    tools="$(dirname "$tools")"
fi
[[ -x "$tools/generate_appcast" ]]
for architecture in arm64 x86_64; do
    [[ -f "$assets/Lokii-$version-$architecture.zip" ]] || { echo "[FAIL] 缺少 $architecture 更新包" >&2; exit 1; }
done
temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT
public_key="$(/usr/libexec/PlistBuddy -c 'Print :SUPublicEDKey' "$repo_root/Lokii/Lokii/Info.plist")"
mkdir -p "$temporary/feeds"
build_number=''
for architecture in arm64 x86_64; do
    staging="$temporary/$architecture"
    mkdir -p "$staging" "$temporary/extracted-$architecture"
    archive="$assets/Lokii-$version-$architecture.zip"
    ditto -x -k "$archive" "$temporary/extracted-$architecture"
    bundle="$temporary/extracted-$architecture/Lokii.app"
    bash "$repo_root/scripts/verify-update-bundle.sh" "$bundle" "$architecture"
    plist="$bundle/Contents/Info.plist"
    [[ $(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$plist") == "$version" ]]
    [[ $(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$plist") == com.lokii.app ]]
    [[ $(/usr/libexec/PlistBuddy -c 'Print :SUPublicEDKey' "$plist") == "$public_key" ]]
    current_build="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$plist")"
    if [[ -n "$build_number" ]]; then [[ "$build_number" == "$current_build" ]]; fi
    build_number="$current_build"
    cp "$archive" "$staging/"
    if [[ -f "$destination/$architecture.xml" ]]; then cp "$destination/$architecture.xml" "$staging/appcast.xml"; fi
    notes="$assets/Lokii-$version-$architecture.md"
    if [[ -f "$notes" ]]; then cp "$notes" "$staging/"; fi
    "$tools/generate_appcast" --ed-key-file "$private_key" --maximum-deltas 0 --maximum-versions 5 \
        -o "$staging/appcast.xml" --embed-release-notes --download-url-prefix "https://github.com/huangy7/lokii/releases/download/v$version/" \
        --link 'https://github.com/huangy7/lokii/releases/latest' "$staging"
    swift "$repo_root/scripts/verify-update-feed.swift" "$staging/appcast.xml" "$archive" "$public_key" "$version" "$architecture"
    cp "$staging/appcast.xml" "$temporary/feeds/$architecture.xml"
done
mkdir -p "$destination"
cp "$temporary/feeds/arm64.xml" "$destination/arm64.xml"
cp "$temporary/feeds/x86_64.xml" "$destination/x86_64.xml"
