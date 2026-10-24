# 原生过滤气泡浮层（Filters Popover）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 Lokii 打造高信息密度、键盘优先的原生搜索过滤气泡浮层（NSPopover），通过语法双向同步器与零堆分配核心初筛引擎，支持复合文件类型、大小区间以及检索行为开关的实时即搜。

**Architecture:** 前端基于 AppKit `NSPopover` 结合 SwiftUI 视图，通过包含防死循环重入锁与光标保护的 `FilterQuerySyncCoordinator` 与主搜索框双向联动；Rust 核心层扩展复合类型并集位掩码初筛与纯标量单周期文件大小比较，维持亚毫秒级极致吞吐。

**Tech Stack:** Rust 2021 (`lokii-core`), Swift 5.9 / AppKit / SwiftUI (`Lokii`), UniFFI.

---

### Task 1: 核心引擎复合类型与扩展名并集初筛 (`lokii-core`)

**Files:**
- Modify: `lokii-core/src/search/filter.rs`
- Test: `lokii-core/src/search/filter.rs`

- [ ] **Step 1: 编写复合文件类型与多扩展名解析失败测试**

在 `lokii-core/src/search/filter.rs` 的测试模块追加：
```rust
#[test]
fn test_parse_multi_file_type() {
    let (clean, filter) = parse_query_filter("main type:code,doc");
    assert_eq!(clean, "main");
    assert_eq!(filter.file_types, vec![FileType::Code, FileType::Document]);
}

#[test]
fn test_parse_multi_extension() {
    let (clean, filter) = parse_query_filter("test ext:rs,swift,ts");
    assert_eq!(clean, "test");
    assert_eq!(filter.custom_exts, vec!["rs", "swift", "ts"]);
}
```

- [ ] **Step 2: 运行测试验证失败**

运行：`cargo test --package lokii-core --lib search::filter::tests::test_parse_multi_file_type`
预期：编译失败，`file_types` 或 `custom_exts` 字段尚不存在。

- [ ] **Step 3: 实现复合类型切分与位掩码并集编译**

在 `lokii-core/src/search/filter.rs` 中：
1. 扩展 `QueryFilter` 包含 `file_types: Vec<FileType>` 和 `custom_exts: Vec<String>`；
2. 在 `parse_query_filter` 中解析以逗号切分的 `type:` 与 `ext:` 标签；
3. 在 `CompiledRecordFilter::compile` 中，将所有选定类型的扩展名集合取并集写入 `valid_ext_mask`；
4. 若包含 `FileType::Folder`，置 `allow_dirs = true`；若仅有 `FileType::Folder`，置 `dirs_only = true`；若包含非目录类型且无 Folder，置 `allow_dirs = false`；
5. 未指定任何类型（`file_types.is_empty()`）时，置 `has_type_filter = false`，直通放行。

- [ ] **Step 4: 运行测试验证通过**

运行：`cargo test --package lokii-core --lib search::filter::tests`
预期：PASS。

- [ ] **Step 5: 运行全工作区测试并提交**

运行：`cargo test --workspace`
预期：所有 225+ 个单元测试全部通过。
```bash
git add lokii-core/src/search/filter.rs
git commit -m "feat: 支持复合文件类型与多扩展名并集初筛"
```

---

### Task 2: 核心引擎文件大小区间解析与零堆分配初筛 (`lokii-core`)

**Files:**
- Modify: `lokii-core/src/search/filter.rs`
- Modify: `lokii-core/src/search/mod.rs`
- Test: `lokii-core/src/search/filter.rs`

- [ ] **Step 1: 编写文件大小解析与初筛失败测试**

在 `lokii-core/src/search/filter.rs` 的测试模块追加：
```rust
#[test]
fn test_parse_size_filters() {
    let (c1, f1) = parse_query_filter("test size:>10m");
    assert_eq!(c1, "test");
    assert_eq!(f1.size_filter.min_bytes, Some(10_000_000));
    assert_eq!(f1.size_filter.max_bytes, None);

    let (c2, f2) = parse_query_filter("test size:<500k");
    assert_eq!(c2, "test");
    assert_eq!(f2.size_filter.min_bytes, None);
    assert_eq!(f2.size_filter.max_bytes, Some(500_000));

    let (c3, f3) = parse_query_filter("test size:1.5m..100m");
    assert_eq!(c3, "test");
    assert_eq!(f3.size_filter.min_bytes, Some(1_500_000));
    assert_eq!(f3.size_filter.max_bytes, Some(100_000_000));
}

#[test]
fn test_compiled_filter_matches_size() {
    let store = crate::indexer::IndexStore::new();
    store.replace(vec![
        crate::indexer::FileEntry {
            name: "small.txt".to_string(),
            path: "/small.txt".to_string(),
            is_dir: false,
            size: 1_000,
            modified: 0,
            created_at: 0,
            extension: "txt".to_string(),
        },
        crate::indexer::FileEntry {
            name: "large.txt".to_string(),
            path: "/large.txt".to_string(),
            is_dir: false,
            size: 20_000_000,
            modified: 0,
            created_at: 0,
            extension: "txt".to_string(),
        },
    ]);

    let (results, _) = crate::search::dispatch(&store, "txt size:>10m", crate::search::SearchMode::Auto, 10);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entry.name, "large.txt");
}
```

- [ ] **Step 2: 运行测试验证失败**

运行：`cargo test --package lokii-core --lib search::filter::tests::test_parse_size_filters`
预期：编译失败，`size_filter` 字段尚未实现。

- [ ] **Step 3: 实现文件大小语法解析与标量范围初筛**

1. 定义 `SizeFilter { pub min_bytes: Option<u64>, pub max_bytes: Option<u64> }`；
2. 实现 `parse_size_token` 支持 `b`, `k`/`kb` ($10^3$), `m`/`mb` ($10^6$), `g`/`gb` ($10^9$)，使用 `saturating_mul` 防御溢出，并支持浮点解析；
3. 更新 `CompiledRecordFilter`：
   - 字段 `min_size_bytes: u64`（默认 0）；
   - 字段 `max_size_bytes: u64`（默认 `u64::MAX`）；
   - 字段 `has_size_filter: bool`；
4. 在 `matches_record` 中：
   - 目录且 `has_size_filter == true` 时返回 `false`（大小过滤仅作用于文件）；
   - 单时钟周期无分支标量比对：`if record.size < self.min_size_bytes || record.size > self.max_size_bytes { return false; }`。

- [ ] **Step 4: 运行测试验证通过**

运行：`cargo test --package lokii-core --lib search::filter::tests`
预期：PASS。

- [ ] **Step 5: 验证全工作区与提交**

运行：`cargo test --workspace`
预期：全部通过。
```bash
git add lokii-core/src/search/filter.rs lokii-core/src/search/mod.rs
git commit -m "feat: 支持文件大小区间语法与单时钟周期标量初筛"
```

---

### Task 3: 语法双向协调器 `FilterQuerySyncCoordinator` (`Lokii`)

**Files:**
- Create: `Lokii/Lokii/FilterQuerySyncCoordinator.swift`
- Test: `Lokii/LokiiTests/FilterQuerySyncCoordinatorTests.swift`

- [ ] **Step 1: 编写双向同步与防死循环单元测试**

在 `Lokii/LokiiTests/FilterQuerySyncCoordinatorTests.swift` 编写测试：
```swift
import XCTest
@testable import Lokii

final class FilterQuerySyncCoordinatorTests: XCTestCase {
    func testQuerySerializationPreservesUserKeyword() {
        var state = FilterState()
        state.selectedTypes = [.code, .document]
        state.minSizeBytes = 10_000_000

        let result = FilterQuerySyncCoordinator.serialize(keyword: "lokii", state: state)
        XCTAssertTrue(result.contains("lokii"))
        XCTAssertTrue(result.contains("type:code,doc"))
        XCTAssertTrue(result.contains("size:>10m"))
    }

    func testQueryDeserializationExtractsState() {
        let (keyword, state) = FilterQuerySyncCoordinator.deserialize(query: "myproject type:code,doc size:>10m")
        XCTAssertEqual(keyword, "myproject")
        XCTAssertEqual(state.selectedTypes, [.code, .document])
        XCTAssertEqual(state.minSizeBytes, 10_000_000)
    }
}
```

- [ ] **Step 2: 运行测试验证失败**

运行：`cd Lokii && swift test --filter FilterQuerySyncCoordinatorTests`
预期：FAIL，找不到 `FilterQuerySyncCoordinator` 与 `FilterState`。

- [ ] **Step 3: 实现 `FilterQuerySyncCoordinator` 与重入锁**

在 `Lokii/Lokii/FilterQuerySyncCoordinator.swift` 中实现：
1. `FilterState` 结构体：
   - `selectedTypes: Set<FfiFileType>`
   - `minSizeBytes: UInt64?`
   - `maxSizeBytes: UInt64?`
   - `matchPath: Bool`
   - `fuzzyMatching: Bool`
2. `serialize(keyword: String, state: FilterState) -> String`
3. `deserialize(query: String) -> (keyword: String, state: FilterState)`
4. 单向同步守卫 `isSynchronizing: Bool`，保障 `updateQueryFromUI` 与 `updateUIFromQuery` 互斥不递归；
5. 接入 `NSSearchField.currentEditor()`，检查 `hasMarkedText()` 保护中文/多字节输入法组字状态，保留 `selectedRange` 光标位置。

- [ ] **Step 4: 运行测试验证通过**

运行：`cd Lokii && swift test --filter FilterQuerySyncCoordinatorTests`
预期：PASS。

- [ ] **Step 5: 提交**

```bash
git add Lokii/Lokii/FilterQuerySyncCoordinator.swift Lokii/LokiiTests/FilterQuerySyncCoordinatorTests.swift
git commit -m "feat: 实现带重入锁与光标保护的过滤语法双向协调器"
```

---

### Task 4: 原生 SwiftUI 过滤浮层视图 `FilterPopoverView` (`Lokii`)

**Files:**
- Create: `Lokii/Lokii/FilterPopoverView.swift`

- [ ] **Step 1: 创建声明式过滤视图组件**

在 `Lokii/Lokii/FilterPopoverView.swift` 中实现：
1. 顶部操作栏：标题“过滤条件”与“全部清除”按钮；
2. 文件类型网格：
   - 顶部右侧带「全部 (All)」复选开关；
   - 2 列紧凑网格（代码、文档、图片、多媒体、压缩包、文件夹）；
   - 支持多选切换，每次变动通过回调通知父级执行即时过滤；
3. 文件大小区间：
   - 大于 / 小于复选开关 + 文本输入框 + 单位下拉框（MB, KB, GB, Bytes）；
   - 实时输入防抖校验与单位换算；
4. 检索行为开关：
   - 匹配路径（Match Path）、模糊跳字（Fuzzy）；
5. 底部操作栏：
   - “完成”按钮，点击收起 Popover。

- [ ] **Step 2: 本地编译检查**

运行：`cd Lokii && swift build`
预期：Build complete，无编译警告。

- [ ] **Step 3: 提交**

```bash
git add Lokii/Lokii/FilterPopoverView.swift
git commit -m "feat: 实现高信息密度原生过滤气泡浮层视图"
```

---

### Task 5: AppKit 浮层控制器挂载与 `SearchWindow` 实时联动 (`Lokii`)

**Files:**
- Create: `Lokii/Lokii/FilterPopoverController.swift`
- Modify: `Lokii/Lokii/SearchWindow.swift`
- Modify: `Lokii/project.yml`

- [ ] **Step 1: 实现 `FilterPopoverController`**

在 `Lokii/Lokii/FilterPopoverController.swift` 中封装 `NSPopover`：
1. 材质设为 `.hudWindow`，`behavior = .transient`；
2. 使用 `NSHostingView(rootView: FilterPopoverView(...))` 承载内容；
3. 对接 `FilterQuerySyncCoordinator`，复选框与大小输入变动时实时写回 `searchField` 并执行 `performSearch`。

- [ ] **Step 2: 在 `SearchWindow` 宿主容器挂载胶囊按钮**

在 `Lokii/Lokii/SearchWindow.swift` 中：
1. 在 `searchContainer` 内、`searchField` 右侧添加独立的过滤胶囊按钮 `filterButton`；
2. 按钮包含 `line.3.horizontal.decrease` 图标与过滤数量角标（如 `2`）；
3. 注册 `⌥F`（Option+F）快捷键触发按钮动作；
4. 点击弹出/收起 `NSPopover`，锚定在 `filterButton` 下边缘；
5. 在每次检索完成时更新 `filterButton` 的激活数量高亮状态。

- [ ] **Step 3: 更新 XcodeGen 项目描述并生成工程**

在 `Lokii/project.yml` 中注册新建的 Swift 文件（若需显式声明），并执行：
`xcodegen generate`

- [ ] **Step 4: 编译并验证**

运行：`cd Lokii && swift build && xcodebuild -project Lokii.xcodeproj -scheme Lokii -destination 'platform=macOS' build`
预期：**BUILD SUCCEEDED**。

- [ ] **Step 5: 提交**

```bash
git add Lokii/Lokii/FilterPopoverController.swift Lokii/Lokii/SearchWindow.swift Lokii/project.yml Lokii/Lokii.xcodeproj/
git commit -m "feat: 集成过滤胶囊按钮与实时即搜原生气泡浮层"
```

---

### Task 6: 双语本地化与端到端系统验收

**Files:**
- Modify: `Lokii/Lokii/Resources/en.lproj/Localizable.strings`
- Modify: `Lokii/Lokii/Resources/zh-Hans.lproj/Localizable.strings`

- [ ] **Step 1: 补齐浮层多语言词条**

在 `zh-Hans.lproj` 与 `en.lproj` 中补齐：
- `filter.title`: "过滤条件" / "Filters"
- `filter.clearAll`: "全部清除" / "Clear All"
- `filter.all`: "全部" / "All"
- `filter.size.title`: "文件大小" / "File Size"
- `filter.size.over`: "大于" / "Over"
- `filter.size.under`: "小于" / "Under"
- `filter.behavior.path`: "匹配完整路径" / "Match Path"
- `filter.behavior.fuzzy`: "模糊跳字匹配" / "Fuzzy Matching"
- `filter.done`: "完成" / "Done"

- [ ] **Step 2: 执行全量测试**

1. `cargo test --workspace`：确保全量 Rust 核心检索测试通过（225+ 个）；
2. `cd Lokii && swift test`：确保原生测试全部通过；
3. `cd Lokii && xcodebuild -project Lokii.xcodeproj -scheme Lokii -destination 'platform=macOS' build`：确保 Xcode 构建通过；
4. `./scripts/build.sh app`：确保打包脚本与代码签名校验成功。

- [ ] **Step 3: 提交并准备交付**

```bash
git add Lokii/Lokii/Resources/
git commit -m "feat: 补齐过滤浮层双语本地化支持"
```
