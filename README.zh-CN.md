<p align="center">
  <img src="assets/icon.png" width="96" height="96" alt="Lokii Icon">
</p>

<h1 align="center">Lokii</h1>

<p align="center">
  <b>极致轻量、亚毫秒级响应的 macOS 原生即时文件搜索工具 —— 专属于 Mac 的 “Everything” 体验</b>
</p>

<p align="center">
  <a href="README.md"><img src="https://img.shields.io/badge/lang-English-lightgrey?style=flat-square" alt="English"></a>
  <a href="README.zh-CN.md"><img src="https://img.shields.io/badge/lang-简体中文-blue?style=flat-square" alt="简体中文"></a>
</p>

<p align="center">
  <a href="https://github.com/huangy7/lokii/releases/latest"><b>下载 macOS 版（支持 Apple Silicon 与 Intel）</b></a>
</p>

<p align="center">
  <a href="https://github.com/huangy7/lokii/releases/latest"><img src="https://img.shields.io/github/v/release/huangy7/lokii?style=flat-square&label=latest" alt="最新版本"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/huangy7/lokii?style=flat-square" alt="License"></a>
  <img src="https://img.shields.io/badge/platform-macOS%2013%2B-blue?style=flat-square" alt="Platform">
  <a href="https://github.com/huangy7/lokii/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/huangy7/lokii/ci.yml?branch=main&style=flat-square&label=CI" alt="CI"></a>
  <a href="https://github.com/huangy7/lokii/releases"><img src="https://img.shields.io/github/downloads/huangy7/lokii/total?style=flat-square&color=success" alt="Downloads"></a>
  <img src="https://komarev.com/ghpvc/?username=huangy7-lokii&label=Views&color=0071e3&style=flat-square" alt="Views">
</p>

<p align="center">
  <img src="assets/screenshot-main.png" alt="Lokii 即时搜索主界面" width="880">
</p>

Lokii 是一款专为 Mac 用户与开发者打造的原生即时文件搜索利器，为你带来类似 Windows 下 Everything 般极速、轻巧的文件检索体验。它基于高性能 Rust 内存倒排索引核心（`lokii-core`），通过 UniFFI 桥接至纯原生 Swift/AppKit 用户界面，在数十万文件的规模下依然提供亚毫秒级的输入即检索响应，并通过 FSEvents 实时监控文件系统的增量变更。

## 核心功能

| 功能 | 说明 |
| :--- | :--- |
| **亚毫秒级即时检索** | 输入即显示，50 万级别文件检索延迟 < 10ms，内存占用极低（< 100MB）。 |
| **多模态搜索模式** | 智能模式识别：支持纯子串匹配、通配符（`*`, `?`）、正则表达式（`^...$`）以及模糊拼写纠错（Fuzzy）。 |
| **FSEvents 实时事件流** | 基于多路 FSEvents 增量事件流与防抖合并队列，实时跟踪文件增删改与目录重命名，索引 CPU 开销接近为零。 |
| **原生 AppKit 界面质感** | 高信息密度原生虚拟列表、自适应斑马纹、原生系统文件图标渲染与弹性阻尼视窗微动效。 |
| **全键盘极速流交互** | 全局快捷键呼出（`⌘⇧Space`）、方向键平滑导航、`Enter` 在访达中直达定位、`⌘C` 复制完整路径、`Esc` 瞬间隐藏。 |
| **持久化二进制缓存** | 基于 bitcode 紧凑二进制格式序列化存储索引，重启应用后毫秒级热加载恢复就绪状态。 |
| **原生偏好设置与权限指引** | 纯原生设置窗口：支持自定义索引目录、忽略规则、浅色/深色/跟随系统外观切换，以及完全磁盘访问拖拽授权指引。 |

## 隐私与安全

Lokii 100% 运行在您的本地 Mac 上。没有任何用户账号体系，没有遥测（Telemetry），不发送任何网络请求。您的文件名、磁盘结构与搜索记录永远不会离开您的设备。

## 架构组成

```
lokii-core/       Rust 核心库 — 内存倒排索引、并行目录扫描器、FSEvents 监听队列、bitcode 持久化缓存
Lokii/            macOS Swift/AppKit 原生应用 — 高性能虚拟列表、系统托盘常驻、设置窗口
scripts/          构建与打包脚本（UniFFI 桥接生成、自动化 DMG 镜像装配）
```

## 从源码构建

### 环境要求

- macOS 13.0+
- Rust stable（2021 edition）
- Xcode 15+（Swift 5.9+）

### 编译与运行

```bash
# 1. 克隆代码仓库
git clone https://github.com/huangy7/lokii.git
cd lokii

# 2. 一键编译应用（自动生成 UniFFI 桥接并编译 Swift 前端）
make

# 3. 运行调试版应用
cd Lokii && .build/debug/Lokii
```

### 打包 DMG 安装镜像

通过 Makefile 一键构建独立发布的 `.app` 和 `.dmg`：

```bash
# 构建 Apple Silicon 架构版本 (arm64)
make package ARCH=arm64

# 构建 Intel 架构版本 (x86_64)
make package ARCH=x86_64
```

构建输出路径位于 `output/Lokii-<arch>.dmg`。

## 贡献者

<p align="center">
  <a href="https://github.com/huangy7/lokii/graphs/contributors">
    <img src="https://contrib.rocks/image?repo=huangy7/lokii&max=100" alt="Contributors" />
  </a>
</p>

## 许可

基于 [MIT](LICENSE) 开源许可协议发布。

---

<p align="center">
  由 <a href="https://github.com/huangy7">huangy7</a> 开发与维护
</p>
