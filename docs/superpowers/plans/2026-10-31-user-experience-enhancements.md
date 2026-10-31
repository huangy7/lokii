# Lokii 用户体验全链路优化实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 全面落地 Lokii 的跨应用拖拽流转、动态打开方式、多选连续预览、多屏屏幕智能跟随、开机自启、视窗钉住置顶、零结果空状态引导、最近修改默认排序、搜索历史、右键一键忽略目录以及内核中文拼音即时检索能力。

**Architecture:** 前端基于 Swift/AppKit 虚拟列表、拖拽数据源协议、`SMAppService` 与 `NSWorkspace` 实现系统生态级深度联动；后端在 `lokii-core` 中扩展轻量拼音转写与索引匹配管道，保持单向流与亚毫秒级响应。

**Tech Stack:** Swift 5.9+, AppKit, Quick Look, SMAppService, Rust 2021 (lokii-core), UniFFI.

**Spec:** `docs/superpowers/specs/2026-10-31-user-experience-enhancements-design.md`

## Global Constraints

- **严禁使用 Emoji**：在任何回复、发帖文案、宣传草稿、代码注释、文档与提交信息中，严禁添加任何 emoji 表情符号。
- **提交规范**：`<type>: <中文概括>`，分点说明改动，不要用 Emoji。未经用户验收不私自提交。
- **红线禁令**：严禁提及任何外部/第三方参考仓储名称。全篇全模块使用 Lokii 自身的产品语言。
- **验证基线**：任何代码变更前后均必须保证 `cargo test --workspace` 与 `swift build` 全绿通过。

## Review Focus

1. 拖拽文件到桌面与访达时，确保生成正确的文件副本，绝不可损坏或移除原文件。
2. 动态打开方式探测与执行时，必须具备异常捕获与降级处理，规避由于被选应用崩溃导致主进程卡死。
3. 钉住（Pin）视窗状态下，必须准确覆盖失焦自动隐藏逻辑，确保视窗稳定悬浮。
4. 拼音检索必须保证原有英文、通配符、正则与模糊检索行为 100% 兼容，不得引入性能回退或内存泄漏。
5. 忽略目录操作必须原子化写入配置文件并广播变更，避免索引状态不一致。

---

### Task 1: 视觉质感与即时交互基础（居中截断、最近修改排序、空状态提示）

**Files:**
- Modify: `Lokii/Lokii/SearchResultsViewController.swift`
- Modify: `Lokii/Lokii/SearchWindow.swift`
- Modify: `Lokii/Lokii/Resources/zh-Hans.lproj/Localizable.strings`
- Modify: `Lokii/Lokii/Resources/en.lproj/Localizable.strings`

- [x] **Step 1: 路径列文本改为居中截断**
  - 在 `SearchResultsViewController.swift` 的 `pathCell` 中，将 `textField.lineBreakMode` 明确设置为 `.byTruncatingMiddle`。
  - 确保超长深层路径展示为 `~/Parent/.../TargetFolder`。

- [x] **Step 2: 默认空查询改为按“最近修改”倒序排序**
  - 在 `SearchWindow.swift` 的 `loadDefaultResults()` 中，将排序器修改为优先按 `item.modified` 降序排序，相同者再按 `name.localizedStandardCompare` 升序排序。
  - 确保视窗首次打开或清空输入时，首屏呈现最近活跃的文件条目。

- [x] **Step 3: 添加零结果空状态引导视图**
  - 在 `SearchResultsViewController.swift` 中创建 `emptyStateView`，包含居中主提示文本与辅助诊断指引。
  - 在 `displayResults(_:preserveSelection:)` 中根据 `results.isEmpty` 与当前检索文本动态控制空状态视图的显隐。
  - 在多语言资源文件中补充“未找到匹配文件”与辅助指引文本。

- [x] **Step 4: 验证编译与效果**
  - 运行 `cd Lokii && swift build` 验证前端编译通过。

---

### Task 2: 跨应用流转与文件交互拓展（拖拽、打开方式、多选连续快速预览）

**Files:**
- Modify: `Lokii/Lokii/SearchResultsViewController.swift`
- Modify: `Lokii/Lokii/Resources/zh-Hans.lproj/Localizable.strings`
- Modify: `Lokii/Lokii/Resources/en.lproj/Localizable.strings`

- [x] **Step 1: 实现 NSTableView 原生拖拽数据源协议**
  - 在 `SearchResultsViewController.swift` 的 `setupTableView()` 中声明 `tableView.setDraggingSourceOperationMask([.copy, .link], forLocal: false)`。
  - 实现 `NSTableViewDataSource` 的 `tableView(_:pasteboardWriterForRow:)`，为对应行返回 `NSURL(fileURLWithPath: result.path) as NSPasteboardWriting`。

- [x] **Step 2: 实现“打开方式（Open With...）”右键二级菜单**
  - 在 `SearchResultsViewController.swift` 中新增应用探活辅助方法，利用 `NSWorkspace.shared.urlsForApplications(toOpen:)` 获取当前选中文件的可用打开程序。
  - 在 `setupContextMenu()` 中动态挂载“打开方式”子菜单，展示各应用图标与名称，并在末尾提供“其他...”文件选择器。
  - 绑定选中 App 的唤起动作：`NSWorkspace.shared.open([url], withApplicationAt: appURL, configuration: ...)`。

- [x] **Step 3: 扩展 Quick Look 多选连续翻页支持**
  - 在 `SearchResultsViewController.swift` 中将 `numberOfPreviewItems(in:)` 调整为 `selectedResults.count`。
  - 在 `previewPanel(_:previewItemAt:)` 中根据索引返回对应条目的 `NSURL`。

- [x] **Step 4: 验证编译与交互**
  - 运行 `cd Lokii && swift build` 确保无编译报错。

---

### Task 3: 桌面与多屏生态协同（屏幕智能跟随、开机自启、视窗钉住置顶）

**Files:**
- Modify: `Lokii/Lokii/SearchWindow.swift`
- Modify: `Lokii/Lokii/AppDelegate.swift`
- Modify: `Lokii/Lokii/PreferencesView.swift`
- Modify: `Lokii/Lokii/Resources/zh-Hans.lproj/Localizable.strings`
- Modify: `Lokii/Lokii/Resources/en.lproj/Localizable.strings`

- [x] **Step 1: 实现多显示器光标屏幕智能跟随**
  - 在 `SearchWindow.swift` 的 `showWindow(_:)` 触发时，当视窗未显示时检测当前全局鼠标坐标 `NSEvent.mouseLocation` 所在的屏幕。
  - 若目标屏幕与当前记录的屏幕不同，则在目标屏幕的工作区黄金中心重排视窗几何尺寸与居中位置。

- [x] **Step 2: 实现视窗置顶与钉住模式（Pin / Always on Top）**
  - 在 `SearchWindow.swift` 中维护 `isPinned: Bool`，暴露公有切换接口。
  - 在主工具栏中添加钉住按钮（图标 `pin` / `pin.fill`），绑定全局快捷键 `⌘P`。
  - 当处于钉住状态时设置 `window.level = .floating`；在 `AppDelegate.applicationDidResignActive` 中，当 `isPinned == true` 时豁免自动失焦隐藏。

- [x] **Step 3: 接入 macOS 13+ 原生开机自启动（SMAppService）**
  - 在 `PreferencesViewModel` 中加入 `launchAtLogin: Bool` 状态，基于 `SMAppService.mainApp` 进行状态探测与注册/反注册。
  - 在 `PreferencesView.swift` 的通用设置面板中呈现“登录时自动启动 Lokii”开关。

- [x] **Step 4: 验证编译与集成**
  - 运行 `cd Lokii && swift build` 验证构建通过。

---

### Task 4: 检索效率与配置便捷化（搜索历史、右键一键忽略目录）

**Files:**
- Modify: `Lokii/Lokii/SearchWindow.swift`
- Modify: `Lokii/Lokii/SearchResultsViewController.swift`
- Modify: `Lokii/Lokii/Resources/zh-Hans.lproj/Localizable.strings`
- Modify: `Lokii/Lokii/Resources/en.lproj/Localizable.strings`

- [x] **Step 1: 实现搜索历史记录与按键召回**
  - 在 `SearchWindow.swift` 中封装最近搜索词持久化（上限 10 条）。
  - 在搜索框左侧放大镜菜单中挂载“最近搜索”项与“清除历史”选项。
  - 在搜索框聚焦且文本为空时，按 `↑` 键自动回填上一次搜索词。

- [x] **Step 2: 实现右键“将此目录加入忽略列表”**
  - 在 `SearchResultsViewController.swift` 右键菜单中增加“忽略此目录”菜单项。
  - 点击时获取条目对应的目录绝对路径，调用引擎更新配置追加至 `exclude_paths` 并保存。
  - 刷新当前检索列表以即时剔除被忽略目录中的条目。

- [x] **Step 3: 验证编译与联动**
  - 运行 `cd Lokii && swift build` 验证构建无警告无报错。

---

### Task 5: 内核中文拼音即时检索（lokii-core）

**Files:**
- Create: `lokii-core/src/pinyin.rs`
- Modify: `lokii-core/src/lib.rs`
- Modify: `lokii-core/src/compact_index.rs`
- Modify: `lokii-core/src/search/substring.rs`
- Modify: `lokii-core/src/search/fuzzy.rs`
- Modify: `lokii-core/src/search/mod.rs`
- Test: `lokii-core/tests/pinyin_search.rs`

- [x] **Step 1: 编写拼音单元测试（TDD 驱动）**
  - 创建 `lokii-core/tests/pinyin_search.rs`，覆盖中文汉字全拼与首字母检索预期。
  - 运行 `cargo test --test pinyin_search` 确认失败。

- [x] **Step 2: 实现轻量拼音转写与首字母提取模块**
  - 在 `lokii-core/src/pinyin.rs` 中实现常用 CJK 统一表意文字到拼音全拼与首字母的高效查表映射。
  - 提供 `to_pinyin_tokens(text: &str) -> (String, String)`，返回全拼串与首字母串。

- [x] **Step 3: 在紧凑倒排索引与检索管道中接入拼音匹配**
  - 在 `lokii-core/src/compact_index.rs` 唯一名初始化阶段，为含有中文字符的名字建立拼音投影索引。
  - 在 `substring.rs` 与 `fuzzy.rs` 检索时，若查询词包含纯 ASCII 字母且包含中文字符库，双向对比折叠名与拼音投影，并记录原始中文字符的高亮区间。

- [x] **Step 4: 全量测试验证**
  - 运行 `cargo test --workspace` 确保所有 Rust 单元测试与基准测试通过。
  - 运行 `make` 重新生成 UniFFI 桥接并完成端到端构建。

---

### Task 6: 专项代码评审与深度缺陷收敛

- [x] **Critical 1: 拼音映射非 ASCII 标点越界崩溃修复**
  - 在 `lokii-core/src/pinyin.rs` 中使 `initials_byte_ranges` 与 `pinyin_char_byte_ranges` 与 UTF-8 字节严格 1:1 映射，杜绝 `str::find` 字节偏移在多字节符号（如全角书名号、破折号）下的越界 panic。
- [x] **Critical 2: 签名超集过滤器无字母查询绕过修复**
  - 在 `lokii-core/src/string_pool.rs` 的 `NameSignature::contains` 中，确保仅在查询词包含 ASCII 字母时放宽 CJK 二元组约束，并严格校验基础字符位（数字、符号及脚本标记），消除无字母查询时的全库 CJK 预筛穿透。
- [x] **Important 1 & 2: 检索热路径零堆分配与混合查询高亮完整性**
  - 分离拼音打分与高亮区间提取，`score_pinyin_match` 杜绝临时 `Vec` 堆分配。
  - 在 `lokii-core/src/search/mod.rs` 的 `find_positions` 中补齐混合词高亮（直接匹配词与拼音词分别独立产出对应字节区间）。
- [x] **Important 3 & 4: 搜索历史单次击键抖动治理与全局最近修改真切片**
  - 在 `SearchWindow.swift` 中将搜索历史记录收敛至回车确认与条目打开时触发，引入前缀归并去重。
  - 在 `lokii-core/src/search/wildcard.rs` 的通配符 `*` 检索中，改为按文件修改时间倒序全局保留 Top-K，消除首屏字母顺序截断带来的最近文件漏显。
- [x] **Important 5 & 6: 菜单打开应用上下文安全与开机自启状态递归防护**
  - 在 `SearchResultsViewController.swift` 中使用捕获的 `urls` 快照。
  - 在 `PreferencesView.swift` 中使用 `isLoading` 状态守卫阻断 `SMAppService` 状态同步时的 `didSet` 递归回环。
- [x] **全链路双端校验通过**
  - Rust 289 项测试全部通过（`cargo test --workspace`）。
  - Swift 原生包编译通过（`cd Lokii && swift build`）。
  - 打包、资源签名与 Bundle 校验全部通过（`make`）。
