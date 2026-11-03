#!/usr/bin/env bash
set -euo pipefail

bundle="${1:?应用路径不能为空}"
architecture="${2:?架构不能为空}"
case "$architecture" in arm64|x86_64) ;; *) exit 1 ;; esac
plist="$bundle/Contents/Info.plist"
read_plist() { /usr/libexec/PlistBuddy -c "Print :$1" "$plist"; }
public_key="$(read_plist SUPublicEDKey)"
[[ $(printf '%s' "$public_key" | base64 --decode | wc -c | tr -d ' ') == 32 ]]
[[ $(read_plist SUFeedURL) == "https://huangy7.github.io/lokii/updates/$architecture.xml" ]]
[[ $(read_plist SUAllowsAutomaticUpdates) == false ]]
[[ $(read_plist SUAutomaticallyUpdate) == false ]]
[[ $(read_plist SUEnableAutomaticChecks) == true ]]
[[ $(read_plist SUVerifyUpdateBeforeExtraction) == true ]]
[[ $(read_plist SUUpdateCheckInterval) == 86400 ]]
[[ $(read_plist LSMinimumSystemVersion) == 13.0 ]]
framework="$bundle/Contents/Frameworks/Sparkle.framework"
[[ -L "$framework/Versions/Current" ]]
[[ -x "$framework/Sparkle" ]]
[[ -x "$framework/Versions/Current/Autoupdate" ]]
[[ -x "$framework/Versions/Current/Updater.app/Contents/MacOS/Updater" ]]
lipo "$bundle/Contents/MacOS/Lokii" -verify_arch "$architecture"
lipo "$framework/Sparkle" -verify_arch "$architecture"
lipo "$bundle/Contents/Helpers/lokii" -verify_arch "$architecture"
otool -L "$bundle/Contents/MacOS/Lokii" | grep -q '@rpath/Sparkle.framework/'
otool -l "$bundle/Contents/MacOS/Lokii" | grep -q '@executable_path/../Frameworks'

# 验证 Mach-O 头 LC_BUILD_VERSION sdk 未被 ld-prime 降级为 minos (13.0)
macho_sdk="$(otool -l "$bundle/Contents/MacOS/Lokii" | awk '/cmd LC_BUILD_VERSION/{flag=1} flag && /sdk/{print $2; exit}')"
macho_sdk_major="${macho_sdk%%.*}"
if [[ -z "$macho_sdk_major" || "$macho_sdk_major" -lt 14 ]]; then
    printf '[FAIL] Mach-O LC_BUILD_VERSION sdk (%s) 低于 14.0，存在 Ventura 降级风险！\n' "$macho_sdk" >&2
    exit 1
fi

codesign --verify --deep --strict "$bundle"
printf '更新包结构与签名检查通过：%s\n' "$architecture"
