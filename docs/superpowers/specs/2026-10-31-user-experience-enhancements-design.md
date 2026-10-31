# Lokii 用户体验全链路优化设计规范

> 状态：设计定稿。覆盖交互流转、多屏生态、空状态反馈与本土化拼音检索四大模块。

---

## 1. 概述与设计原则

Lokii 专注于为 Mac 用户与开发者提供极速、无感、低开销的即时文件检索体验。本规范系统性规划并落实以下四大体验维度优化：
1. **跨应用协同与文件流转**：原生拖拽（Drag & Drop）、动态“打开方式（Open With...）”、多选空格连续快速预览；
2. **桌面与多屏生态协同**：鼠标所在屏幕智能跟随、macOS 13+ 原生开机自启动、视窗置顶/钉住模式（Pin）；
3. **空状态反馈与首屏价值**：零结果智能诊断与微反馈、默认空查询“最近修改”倒序、搜索历史记录与快速召回；
4. **本土化与细节雕琢**：路径列文本居中截断（Truncate Middle）、右键一键忽略/排除目录、中文全拼与首字母即时命中。

### 核心不变量与设计守卫
- **键盘优先**：所有新交互必须具备原生快捷键支持，不强迫使用鼠标。
- **零阻塞与单向流**：所有外部调用（应用探测、配置写入、拼音生成）均在异步线程或单向响应链中执行，绝对不阻塞主输入与虚拟列表滚动。
- **无外部依赖污染**：中文拼音转换采用轻量内建映射机制，不引入庞大冗余的外部字典依赖，维持极低物理内存开销。

---

## 2. 详细技术方案

### 2.1 跨应用流转与文件交互拓展

#### 2.1.1 原生拖拽支持（Drag and Drop）
- **协议实现**：在 `SearchResultsViewController` 中实现 `NSTableViewDataSource` 的拖拽协议：
  ```swift
  func tableView(_ tableView: NSTableView, pasteboardWriterForRow row: Int) -> NSPasteboardWriting?
  ```
- **写入格式**：对于行索引对应之条目，包装为 `NSURL(fileURLWithPath: result.path) as NSPasteboardWriting` 写入剪贴板。
- **拖拽行为**：在 `setupTableView()` 中声明 `tableView.setDraggingSourceOperationMask([.copy, .link], forLocal: false)`，支持将单选或多选文件无缝拖拽至访达视窗、桌面、即时通讯工具、终端或外部编辑器。

#### 2.1.2 “打开方式（Open With...）” 右键二级菜单
- **动态应用探活**：基于 `NSWorkspace.shared.urlsForApplications(toOpen:)` 接口，针对当前选中文件（若是单选则针对该文件；多选则针对共有类型或首项）动态解析系统已安装的关联应用程序。
- **菜单构建与图标渲染**：
  - 构建级联子菜单 `search.ctx.openWith`；
  - 遍历匹配到的 App URL，读取其本地化应用名及小尺寸原生图标（16x16 pt）；
  - 勾选标记指示当前系统默认打开程序；
  - 末尾追加“其他... (Other...)”菜单项，点击弹出 `NSOpenPanel` 允许用户自由挑选 `/Applications` 中的任意应用打开目标文件。

#### 2.1.3 多选条目空格连续快速预览（Quick Look）
- **数据源扩展**：
  - `numberOfPreviewItems(in:)` 改为返回 `selectedResults.count`（若为空则为 0）；
  - `previewPanel(_:previewItemAt:)` 依据索引返回 `selectedResults[index].path` 的 `NSURL`。
- **动画锚点与平滑切换**：
  - 预览面板翻页时，动态更新当前选中行的视觉焦点，并同步底部路径栏展示。

---

### 2.2 桌面与多屏生态协同

#### 2.2.1 鼠标所在屏幕智能跟随（Follow Active Display）
- **屏幕检测**：在 `SearchWindow.showWindow(_:)` 触发时，若窗口当前处于隐藏状态，检测全局鼠标所在屏幕：
  ```swift
  let mouseLocation = NSEvent.mouseLocation
  let targetScreen = NSScreen.screens.first { NSMouseInRect(mouseLocation, $0.frame, false) } ?? NSScreen.main
  ```
- **自适应重居中**：当检测到当前鼠标所在显示器与视窗当前记录的屏幕不同时，在目标屏幕的黄金视觉中心（垂直微偏上 5%）平滑重设视窗坐标，杜绝副屏操作时视窗在主屏突兀弹出的视线割裂。

#### 2.2.2 开机自启动设置（Launch at Login）
- **系统接口**：基于 macOS 13+ 原生 `SMAppService.mainApp`：
  - 状态读取：`SMAppService.mainApp.status == .enabled`；
  - 启用：`try SMAppService.mainApp.register()`；
  - 禁用：`try SMAppService.mainApp.unregister()`。
- **UI 装配**：在偏好设置的“常规”面板中，新增“登录时启动 Lokii”开关，并处理系统权限错误自愈提示。

#### 2.2.3 窗口置顶与钉住模式（Pin / Always on Top）
- **窗口层级控制**：
  - 新增状态 `isPinned: Bool`；
  - 开启时：`window.level = .floating`；关闭时：`window.level = .normal`；
  - 工具栏增加钉住状态按钮，快捷键绑定为 `⌘P`。
- **失焦行为联动**：
  - 在 `applicationDidResignActive` 中，当 `isPinned == true` 时，强制豁免 `hideOnBlur` 行为，保持视窗稳定悬浮，方便用户连续跨窗口比对与拖拽文件。

---

### 2.3 空状态反馈与首屏价值

#### 2.3.1 零结果智能引导微反馈（Empty State View）
- **视图构建**：在 `SearchResultsViewController` 中构建轻量居中占位视图，包含主提示标题与次要引导文本。
- **智能诊断提示**：
  - 标题：“未找到匹配文件”；
  - 辅助建议：根据当前输入动态分析，若包含过滤语法提示“可尝试按 ⌥F 调整文件大小或类型过滤”；若当前未开启显示隐藏文件，提示“可检查偏好设置中是否包含隐藏文件”。
- **显隐状态流**：当且仅当搜索完成且 `rawResults.isEmpty` 且查询非空时淡入显示；一旦重新开始检索或清空输入时立即淡出。

#### 2.3.2 默认无输入视图：按“最近修改”倒序排列
- **排序逻辑调整**：
  - 在 `SearchWindow.loadDefaultResults()` 中，通配查询结果不再采用纯字母序 A-Z，而是改为按文件的 `modified` 时间戳倒序排列（`modified` 降序优先，相同者按文件名升序）。
  - 用户一键唤起 Lokii 时，首屏呈现最近刚刚操作、修改、导出的高频活跃文件。

#### 2.3.3 搜索历史记录与召回（Search History）
- **持久化容器**：在 `SearchWindow` 中使用 `UserDefaults` 维护最近搜索词列表（上限 10 条，去重保鲜）。
- **召回途径**：
  - 搜索框处于聚焦且文本为空时，按键盘 `↑` 键自动回填上一条检索词；
  - 搜索框放大镜图标下拉菜单中追加“最近搜索”子菜单，点击直接回填并触发检索；提供“清空搜索历史”操作。

---

### 2.4 本土化与细节雕琢

#### 2.4.1 路径列文本居中截断（Truncate Middle）
- **排版微调**：
  - 在 `SearchResultsViewController.swift` 中将 `pathCell` 的 `textField.lineBreakMode` 明确设定为 `.byTruncatingMiddle`。
  - 效果：超长深层路径压缩为 `~/Workspace/.../Lokii`，使用户既能看清根路径，又能清晰识别直接父目录。

#### 2.4.2 右键“将此目录加入忽略列表”
- **上下文动作**：
  - 在结果列表与面包屑菜单中增加“忽略此目录（Exclude Directory）”；
  - 目标路径判定：若选中的是文件夹，则为自身路径；若是文件，则提取其父目录；
  - 联动逻辑：通过 `SearchEngine` 读取配置 -> 追加到 `exclude_paths` -> 保存配置并广播更新，触发增量防抖队列自动剔除对应索引。

#### 2.4.3 中文拼音与首字母即时检索（Pinyin Search）
- **架构方案**：
  - 在 `lokii-core` 中引入轻量中文汉字转拼音模块（涵盖全拼与首字母）；
  - 在 `CompactIndex` 构建唯一名池或查询预检时：
    - 对含有 CJK 统一表意文字的文件名，生成折叠拼音串与首字母串；
    - 在子串检索与前缀检索阶段，当用户输入纯 ASCII 小写字母（如 `gzhb` 或 `gongzuo`）时，同时在拼音映射索引中进行亚毫秒级匹配；
    - 返回给前端的匹配位置区间能准确映射回原中文字符，实现中文词条高亮渲染。

---

## 3. 验收与测试策略

1. **拖拽验证**：从 Lokii 搜索结果成功拖拽单个及多个文件至访达指定文件夹与桌面，验证文件副本正常落地。
2. **打开方式验证**：右键文本文件，成功展现系统支持的各种编辑器，点击后被选应用正常激活并打开目标文件。
3. **多屏定位验证**：外接屏幕环境下，鼠标移至副屏按热键呼出，视窗准确出现在副屏视觉中心。
4. **开机启动验证**：在偏好设置中切换开机启动开关，验证 `SMAppService.mainApp.status` 正确变更。
5. **空状态与最近文件验证**：输入无匹配字符时展示空状态提示；清空输入框时列表展示最近修改的文件。
6. **拼音检索验证**：包含中文名的测试数据集下，输入全拼和首字母均能准确检索出目标文件，并在 `cargo test --workspace` 与 `swift build` 下完全通过。
