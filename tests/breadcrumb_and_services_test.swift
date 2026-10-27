import Cocoa
import UniformTypeIdentifiers

// MARK: - Standalone Test Harness for Breadcrumb & Services

let sourceFiles = [
    "Lokii/Lokii/L10n.swift",
    "Lokii/Lokii/LocalizedMenu.swift",
    "Lokii/Lokii/TerminalLauncher.swift",
    "Lokii/Lokii/ResultDirectoryTree.swift",
    "Lokii/Lokii/SidebarTreeViewController.swift",
    "Lokii/Lokii/SearchResultsViewController.swift",
    "Lokii/Lokii/SearchMainSplitViewController.swift",
]

for path in sourceFiles {
    guard FileManager.default.fileExists(atPath: path) else {
        fputs("tests/breadcrumb_and_services_test.swift: error: 找不到 \(path)\n", stderr)
        exit(1)
    }
}

let mockFfiSource = """
public struct FfiMatchRange: Equatable, Hashable {
    public var start: UInt32
    public var end: UInt32
    public init(start: UInt32 = 0, end: UInt32 = 0) {
        self.start = start
        self.end = end
    }
}

public struct FfiSearchResult: Equatable, Hashable {
    public var name: String
    public var path: String
    public var `extension`: String
    public var isDir: Bool
    public var size: UInt64
    public var created: Int64
    public var modified: Int64
    public var matchPositions: [FfiMatchRange]
    public var parentMatchPositions: [FfiMatchRange]

    public init(
        name: String,
        path: String,
        `extension`: String,
        isDir: Bool,
        size: UInt64,
        created: Int64,
        modified: Int64,
        matchPositions: [FfiMatchRange] = [],
        parentMatchPositions: [FfiMatchRange] = []
    ) {
        self.name = name
        self.path = path
        self.extension = `extension`
        self.isDir = isDir
        self.size = size
        self.created = created
        self.modified = modified
        self.matchPositions = matchPositions
        self.parentMatchPositions = parentMatchPositions
    }
}
"""

var loadedSources: [String] = []
for path in sourceFiles {
    guard let content = try? String(contentsOfFile: path, encoding: .utf8) else {
        fputs("tests/breadcrumb_and_services_test.swift: error: 无法读取 \(path)\n", stderr)
        exit(1)
    }
    loadedSources.append(content)
}

let testCasesRunner = #"""
// MARK: - Test Cases Runner

// [1/6] 验证 BreadcrumbPathControl 初始化与 acceptsFirstMouse 契约
print("[1/6] 验证 BreadcrumbPathControl 初始化与 acceptsFirstMouse 契约...")
let breadcrumbControl = BreadcrumbPathControl(frame: NSRect(x: 0, y: 0, width: 500, height: 28))
breadcrumbControl.pathStyle = .standard
assertEqual(breadcrumbControl.acceptsFirstMouse(for: nil), true, "BreadcrumbPathControl 在无事件传参下 acceptsFirstMouse 应为 true")

let dummyMouseEvent = NSEvent.mouseEvent(
    with: .rightMouseDown,
    location: NSPoint(x: 10, y: 10),
    modifierFlags: [],
    timestamp: 0,
    windowNumber: 0,
    context: nil,
    eventNumber: 0,
    clickCount: 1,
    pressure: 1.0
)
assertEqual(breadcrumbControl.acceptsFirstMouse(for: dummyMouseEvent), true, "BreadcrumbPathControl 在右键点击事件下 acceptsFirstMouse 应为 true")

// [2/6] 验证 BreadcrumbPathControl 几何命中与空白防穿透
print("[2/6] 验证 BreadcrumbPathControl 几何命中与空白防穿透...")
final class MockBreadcrumbActionDelegate: BreadcrumbPathControlDelegate {
    var requestedURLs: [URL] = []
    func breadcrumbPathControl(_ control: BreadcrumbPathControl, menuFor url: URL) -> NSMenu? {
        requestedURLs.append(url)
        let menu = NSMenu(title: "MockBreadcrumbMenu")
        let item = NSMenuItem(title: url.lastPathComponent, action: nil, keyEquivalent: "")
        item.representedObject = url
        menu.addItem(item)
        return menu
    }
}

let testWindow = NSWindow(
    contentRect: NSRect(x: 0, y: 0, width: 800, height: 600),
    styleMask: [.titled],
    backing: .buffered,
    defer: false
)
let pathControl = BreadcrumbPathControl(frame: NSRect(x: 0, y: 0, width: 600, height: 28))
pathControl.pathStyle = .standard
let mockBreadcrumbDelegate = MockBreadcrumbActionDelegate()
pathControl.actionDelegate = mockBreadcrumbDelegate
testWindow.contentView?.addSubview(pathControl)

// 设置测试 URL 路径
let samplePath = "/workspace/src/Lokii"
pathControl.url = URL(fileURLWithPath: samplePath)

guard let pathCell = pathControl.cell as? NSPathCell else {
    fputs("断言失败: pathControl.cell 必须为 NSPathCell\n", stderr)
    exit(1)
}

let components = pathCell.pathComponentCells
assertTrue(components.count >= 3, "路径应至少解析出 3 个 pathComponentCell")

// 选取末尾 componentCell 测试有效几何命中
let targetComponent = components[components.count - 1]
let targetRect = pathCell.rect(of: targetComponent, withFrame: pathControl.bounds, in: pathControl)
assertTrue(targetRect.width > 0 && targetRect.height > 0, "组件单元格应具备有效的非空几何区域")

let hitPointInView = NSPoint(x: targetRect.midX, y: targetRect.midY)
let hitPointInWindow = pathControl.convert(hitPointInView, to: nil)
let hitEvent = NSEvent.mouseEvent(
    with: .rightMouseDown,
    location: hitPointInWindow,
    modifierFlags: [],
    timestamp: 0,
    windowNumber: testWindow.windowNumber,
    context: nil,
    eventNumber: 1,
    clickCount: 1,
    pressure: 1.0
)!

let menuResult = pathControl.menu(for: hitEvent)
assertTrue(menuResult != nil, "点击在 componentCell 内部时必须返回有效上下文菜单")
assertEqual(mockBreadcrumbDelegate.requestedURLs.last?.path, targetComponent.url?.path, "代理方法收到的 URL 必须与命中的 componentCell URL 一致")

// 测试右侧空白区域点击防穿透
var maxX: CGFloat = 0
for comp in components {
    let r = pathCell.rect(of: comp, withFrame: pathControl.bounds, in: pathControl)
    maxX = max(maxX, r.maxX)
}
assertTrue(maxX < 550, "所有组件的最大 X 边界应小于 550 pt")

let blankPointInView = NSPoint(x: maxX + 30, y: 14)
let blankPointInWindow = pathControl.convert(blankPointInView, to: nil)
let blankEvent = NSEvent.mouseEvent(
    with: .rightMouseDown,
    location: blankPointInWindow,
    modifierFlags: [],
    timestamp: 0,
    windowNumber: testWindow.windowNumber,
    context: nil,
    eventNumber: 2,
    clickCount: 1,
    pressure: 1.0
)!

let blankMenuResult = pathControl.menu(for: blankEvent)
assertTrue(blankMenuResult == nil, "右键点击控件右侧空白无组件区域时必须严格返回 nil，防止菜单穿透逃逸")

// 测试点击超出控件 bounds 范围
let outsidePointInWindow = pathControl.convert(NSPoint(x: -20, y: -20), to: nil)
let outsideEvent = NSEvent.mouseEvent(
    with: .rightMouseDown,
    location: outsidePointInWindow,
    modifierFlags: [],
    timestamp: 0,
    windowNumber: testWindow.windowNumber,
    context: nil,
    eventNumber: 3,
    clickCount: 1,
    pressure: 1.0
)!
let outsideMenuResult = pathControl.menu(for: outsideEvent)
assertTrue(outsideMenuResult == nil, "右键点击超出控件 bounds 范围时必须严格返回 nil")

// [3/6] 验证 SearchResultsViewController NSServicesRequests 响应者链
print("[3/6] 验证 SearchResultsViewController NSServicesRequests 响应者链...")
let searchVC = SearchResultsViewController()
_ = searchVC.view

// 1. 无选区且无临时租约时，validRequestor 返回 nil
let emptyRequestor = searchVC.validRequestor(forSendType: .fileURL, returnType: nil)
assertTrue(emptyRequestor == nil, "无选区且无 activeContextMenuPayload 时 validRequestor 应返回 nil")

// 2. 注入结果条目并单选文件
let itemA = FfiSearchResult(name: "alpha.rs", path: "/workspace/src/alpha.rs", extension: "rs", isDir: false, size: 1024, created: 100, modified: 200)
let itemB = FfiSearchResult(name: "beta.md", path: "/workspace/docs/beta.md", extension: "md", isDir: false, size: 2048, created: 100, modified: 300)
searchVC.displayResults([itemA, itemB], preserveSelection: false)
searchVC.selectFirstRow()
assertEqual(searchVC.selectedResults.count, 1, "主列表应已选中 1 项")

// 此时 validRequestor(forSendType: .fileURL, returnType: nil) 应返回 searchVC (self)
let validReq = searchVC.validRequestor(forSendType: .fileURL, returnType: nil)
assertTrue((validReq as? SearchResultsViewController) === searchVC, "主列表选中文件时 validRequestor(forSendType: .fileURL) 应返回当前控制器")

// 验证对 string 与 NSFilenamesPboardType 类型的响应
let validReqString = searchVC.validRequestor(forSendType: .string, returnType: nil)
assertTrue((validReqString as? SearchResultsViewController) === searchVC, "主列表选中文件时 validRequestor(forSendType: .string) 应返回当前控制器")

let plainTextType = NSPasteboard.PasteboardType("public.plain-text")
let validReqPlainText = searchVC.validRequestor(forSendType: plainTextType, returnType: nil)
assertTrue((validReqPlainText as? SearchResultsViewController) === searchVC, "主列表选中文件时 validRequestor(forSendType: public.plain-text) 应返回当前控制器以支持 iTerm2 等服务")

let directoryType = NSPasteboard.PasteboardType("public.directory")
let invalidReqDirForFile = searchVC.validRequestor(forSendType: directoryType, returnType: nil)
assertTrue(invalidReqDirForFile == nil, "所选项为普通文件时 validRequestor(forSendType: public.directory) 应返回 nil 屏蔽目录专用服务")

let filenamesType = NSPasteboard.PasteboardType("NSFilenamesPboardType")
let validReqFilenames = searchVC.validRequestor(forSendType: filenamesType, returnType: nil)
assertTrue((validReqFilenames as? SearchResultsViewController) === searchVC, "主列表选中文件时 validRequestor(forSendType: NSFilenamesPboardType) 应返回当前控制器")

// 验证当选中项为目录时对 public.directory 的响应
let dirItem = FfiSearchResult(name: "src", path: "/workspace/src", extension: "", isDir: true, size: 0, created: 100, modified: 200)
searchVC.displayResults([dirItem], preserveSelection: false)
searchVC.selectFirstRow()
let validReqDir = searchVC.validRequestor(forSendType: directoryType, returnType: nil)
assertTrue((validReqDir as? SearchResultsViewController) === searchVC, "主列表选中目录条目时 validRequestor(forSendType: public.directory) 应返回当前控制器")

// 恢复选中 itemA
searchVC.displayResults([itemA, itemB], preserveSelection: false)
searchVC.selectFirstRow()

// 不支持的类型应返回 nil
let invalidReq = searchVC.validRequestor(forSendType: .pdf, returnType: nil)
assertTrue(invalidReq == nil, "不支持的 PasteboardType validRequestor 应返回 nil")

// 3. 验证 writeSelection 写入剪贴板
let testPboard = NSPasteboard(name: NSPasteboard.Name("LokiiTestServicesPasteboard.3"))
testPboard.clearContents()
let writeSuccess = searchVC.writeSelection(to: testPboard, types: [.fileURL, .string, plainTextType, filenamesType])
assertTrue(writeSuccess, "writeSelection 应返回 true 表示写入成功")

let readURLs = testPboard.readObjects(forClasses: [NSURL.self], options: nil) as? [URL]
assertEqual(readURLs?.count, 1, "剪贴板中应读取到 1 个 URL 对象")
assertEqual(readURLs?.first?.path, "/workspace/src/alpha.rs", "剪贴板写入的 URL 路径必须与选中项一致")

let readString = testPboard.string(forType: .string)
assertEqual(readString, "/workspace/src/alpha.rs", "剪贴板写入的文本字符串必须与选中项路径一致")

let readPlainText = testPboard.string(forType: plainTextType)
assertEqual(readPlainText, "/workspace/src/alpha.rs", "剪贴板写入的 public.plain-text 文本必须与选中项路径一致以支持 iTerm2")

let readFilenames = testPboard.propertyList(forType: filenamesType) as? [String]
assertEqual(readFilenames?.count, 1, "剪贴板中应读取到 1 个文件名路径")
assertEqual(readFilenames?.first, "/workspace/src/alpha.rs", "剪贴板写入的 NSFilenamesPboardType 路径必须与选中项一致")

testPboard.releaseGlobally()

// [4/6] 验证 Contextual Scope Lease 临时上下文载荷优先消费与防污染
print("[4/6] 验证 Contextual Scope Lease 临时上下文载荷优先消费与防污染...")
// 当前主列表仍选中 itemA ("/workspace/src/alpha.rs")
let dirB = URL(fileURLWithPath: "/workspace/src")
let breadcrumbMenu = searchVC.breadcrumbPathControl(searchVC.pathBar, menuFor: dirB)
assertTrue(breadcrumbMenu != nil, "面包屑右键菜单应成功创建")
assertEqual(breadcrumbMenu?.allowsContextMenuPlugIns, true, "面包屑菜单必须开启 allowsContextMenuPlugIns 由 AppKit 原生托管服务")

let manualServicesItem = breadcrumbMenu?.items.first {
    $0.title == L("search.ctx.services") || $0.representedObject as? String == "search.ctx.services"
}
assertTrue(manualServicesItem == nil, "面包屑菜单不应包含冗余的手动服务菜单项")

// 验证菜单项排布顺序（先打开，再在访达中显示）
assertEqual(breadcrumbMenu?.items[0].action, NSSelectorFromString("breadcrumbOpenItem:"), "面包屑菜单第 0 项应为打开")
assertEqual(breadcrumbMenu?.items[1].action, NSSelectorFromString("breadcrumbRevealInFinder:"), "面包屑菜单第 1 项应为在访达中显示")

// 验证在访达中显示动作路由至 revealFilesHandler
var breadcrumbRevealedURLs: [URL] = []
searchVC.revealFilesHandler = { urls in
    breadcrumbRevealedURLs = urls
}
let revealMenuItem = breadcrumbMenu?.items.first {
    $0.action == NSSelectorFromString("breadcrumbRevealInFinder:")
}
assertTrue(revealMenuItem != nil, "面包屑菜单必须包含在访达中显示动作项")
if let revealItem = revealMenuItem {
    _ = searchVC.perform(revealItem.action, with: revealItem)
}
assertEqual(breadcrumbRevealedURLs.map { $0.path }, ["/workspace/src"], "触发面包屑菜单在访达中显示应调用 revealFilesHandler 定位该目录")

// 验证在终端中打开动作路由至 openInTerminalHandler
var breadcrumbTerminalURLs: [URL] = []
searchVC.openInTerminalHandler = { urls in
    breadcrumbTerminalURLs = urls
}
let terminalMenuItem = breadcrumbMenu?.items.first {
    $0.action == NSSelectorFromString("breadcrumbOpenInTerminal:")
}
assertTrue(terminalMenuItem != nil, "面包屑菜单必须包含在终端中打开动作项")
if let terminalItem = terminalMenuItem {
    _ = searchVC.perform(terminalItem.action, with: terminalItem)
}
assertEqual(breadcrumbTerminalURLs.map { $0.path }, ["/workspace/src"], "触发面包屑菜单在终端中打开应调用 openInTerminalHandler 派发该目录")

// 验证在目录租约下，validRequestor 正确响应 public.directory
let leaseReqDir = searchVC.validRequestor(forSendType: directoryType, returnType: nil)
assertTrue((leaseReqDir as? SearchResultsViewController) === searchVC, "在面包屑目录租约激活时，validRequestor 应支持 public.directory")

// 验证临时租约激活期间，writeSelection 优先消费目录 B 的载荷
let testPboard4 = NSPasteboard(name: NSPasteboard.Name("LokiiTestServicesPasteboard.4"))
testPboard4.clearContents()
let leaseWriteSuccess = searchVC.writeSelection(to: testPboard4, types: [.fileURL])
assertTrue(leaseWriteSuccess, "租约激活下 writeSelection 应返回 true")

let writtenUrlsLeased = testPboard4.readObjects(forClasses: [NSURL.self], options: nil) as? [URL]
assertEqual(writtenUrlsLeased?.first?.path, "/workspace/src", "在临时租约激活时，writeSelection 必须优先写入目录 B，避免主列表选区错位")

// 触发菜单关闭回调，清理临时作用域租约
if let menu = breadcrumbMenu, let menuDelegate = menu.delegate {
    menuDelegate.menuDidClose?(menu)
} else {
    searchVC.pathBar.menuDelegate.menuDidClose(breadcrumbMenu ?? NSMenu())
}

// 验证菜单关闭后租约已清空，writeSelection 恢复为主列表选区 itemA
testPboard4.clearContents()
let restoredWriteSuccess = searchVC.writeSelection(to: testPboard4, types: [.fileURL])
assertTrue(restoredWriteSuccess, "租约关闭后 writeSelection 应成功执行")
let writtenUrlsRestored = testPboard4.readObjects(forClasses: [NSURL.self], options: nil) as? [URL]
assertEqual(writtenUrlsRestored?.first?.path, "/workspace/src/alpha.rs", "菜单关闭后临时租约清空，writeSelection 必须恢复为主列表选中的文件 A")

testPboard4.releaseGlobally()

// [5/6] 验证 BreadcrumbMenuDelegate 对 NSApp.servicesMenu 的成对保存与恢复
print("[5/6] 验证 BreadcrumbMenuDelegate 对 NSApp.servicesMenu 的成对保存与恢复...")
let originalAppServicesMenu = NSMenu(title: "OriginalGlobalServicesMenu")
NSApp.servicesMenu = originalAppServicesMenu

let breadcrumbMenuDelegate = BreadcrumbMenuDelegate()

// 模拟活跃高亮组件单元格
let mockComponentCell = NSPathComponentCell()
mockComponentCell.isHighlighted = true
breadcrumbMenuDelegate.activeComponentCell = mockComponentCell

// 构建包含 Services 子菜单的菜单树
let testContextMenu = NSMenu(title: "TestContextMenu")
let servicesItem = NSMenuItem(title: "服务", action: nil, keyEquivalent: "")
servicesItem.representedObject = "search.ctx.services"
let contextServicesSubmenu = NSMenu(title: "ContextServicesSubmenu")
servicesItem.submenu = contextServicesSubmenu
testContextMenu.addItem(servicesItem)

var onCloseTriggered = false
breadcrumbMenuDelegate.onClose = {
    onCloseTriggered = true
}

// 模拟菜单展开
breadcrumbMenuDelegate.menuWillOpen(testContextMenu)
assertTrue(NSApp.servicesMenu === contextServicesSubmenu, "menuWillOpen 应将 NSApp.servicesMenu 临时重定向为上下文菜单中的 services 子菜单")

// 模拟菜单关闭
breadcrumbMenuDelegate.menuDidClose(testContextMenu)
assertTrue(NSApp.servicesMenu === originalAppServicesMenu, "menuDidClose 必须严格成对恢复原 NSApp.servicesMenu")
assertEqual(mockComponentCell.isHighlighted, false, "menuDidClose 必须重置组件单元格高亮状态为 false")
assertTrue(breadcrumbMenuDelegate.activeComponentCell == nil, "menuDidClose 必须释放 activeComponentCell 引用")
assertTrue(onCloseTriggered, "menuDidClose 必须触发 onClose 回调通知租约生命周期结束")

// [6/6] 验证以此目录为范围过滤与单链压缩兜底派发
print("[6/6] 验证以此目录为范围过滤与单链压缩兜底派发...")
let splitVC = SearchMainSplitViewController()
_ = splitVC.view

// 初始状态下侧边栏必须为折叠状态
assertEqual(splitVC.isSidebarCollapsed, true, "初始状态下侧边栏必须为折叠状态")

// 准备测试数据，模拟包含单链压缩折叠目录结构的结果集
let item1 = FfiSearchResult(name: "main.rs", path: "/workspace/lokii/src/main.rs", extension: "rs", isDir: false, size: 1024, created: 100, modified: 200)
let item2 = FfiSearchResult(name: "util.rs", path: "/workspace/lokii/src/util.rs", extension: "rs", isDir: false, size: 2048, created: 100, modified: 210)
let item3 = FfiSearchResult(name: "Cargo.toml", path: "/workspace/lokii/Cargo.toml", extension: "toml", isDir: false, size: 512, created: 100, modified: 220)
let item4 = FfiSearchResult(name: "notes.txt", path: "/other/docs/notes.txt", extension: "txt", isDir: false, size: 128, created: 100, modified: 230)

let allResults = [item1, item2, item3, item4]
let fullTree = ResultDirectoryTree.build(from: allResults)
splitVC.updateSearchResults(rawResults: allResults, tree: fullTree)
assertEqual(splitVC.searchResultsViewController.results.count, 4, "加载后初始结果数量应为 4")

// 模拟触发 searchResultsDidRequestScopeFilter(path: "/workspace/lokii/src")
splitVC.searchResultsDidRequestScopeFilter(path: "/workspace/lokii/src")

// 1. 验证侧边栏是否被自动展开
assertEqual(splitVC.isSidebarCollapsed, false, "收到目录范围过滤请求后侧边栏必须自动展开")

// 2. 验证结果集是否经过前缀过滤裁剪
assertEqual(splitVC.searchResultsViewController.results.count, 2, "以 /workspace/lokii/src 为范围过滤后结果应仅剩 2 项")
for res in splitVC.searchResultsViewController.results {
    assertTrue(res.path.hasPrefix("/workspace/lokii/src"), "过滤后条目路径必须均以前缀 /workspace/lokii/src 开头")
}

// 3. 验证对单链压缩中间折叠路径的兜底过滤能力
splitVC.searchResultsDidRequestScopeFilter(path: "/workspace/lokii")
assertEqual(splitVC.searchResultsViewController.results.count, 3, "以 /workspace/lokii 为范围过滤后结果应包含 3 项")
for res in splitVC.searchResultsViewController.results {
    assertTrue(res.path.hasPrefix("/workspace/lokii"), "过滤后条目路径必须均以前缀 /workspace/lokii 开头")
}

// 4. 验证直接调用 filterByDirectory 接口
splitVC.filterByDirectory(path: "/other")
assertEqual(splitVC.searchResultsViewController.results.count, 1, "以 /other 为范围过滤后结果应包含 1 项")
assertEqual(splitVC.searchResultsViewController.results.first?.name, "notes.txt", "条目名称应为 notes.txt")

print("测试全部通过：6/6 项断言成功。")
"""#

let combinedSource = """
import Cocoa
import UniformTypeIdentifiers
import Quartz

\(mockFfiSource)

\(loadedSources.joined(separator: "\n\n"))

func assertEqual<T: Equatable>(_ actual: T, _ expected: T, _ message: String = "", file: StaticString = #file, line: UInt = #line) {
    if actual != expected {
        fputs("断言失败: [\\(actual)] != [\\(expected)] - \\(message) (\\(file):\\(line))\\n", stderr)
        exit(1)
    }
}

func assertTrue(_ condition: Bool, _ message: String = "", file: StaticString = #file, line: UInt = #line) {
    if !condition {
        fputs("断言失败: 条件不满足 - \\(message) (\\(file):\\(line))\\n", stderr)
        exit(1)
    }
}

\(testCasesRunner)
"""

let tempDir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
try! FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
defer { try? FileManager.default.removeItem(at: tempDir) }

let harnessFile = tempDir.appendingPathComponent("main.swift")
try! combinedSource.write(to: harnessFile, atomically: true, encoding: .utf8)

let process = Process()
process.executableURL = URL(fileURLWithPath: "/usr/bin/swift")
process.arguments = [harnessFile.path]
process.standardOutput = FileHandle.standardOutput
process.standardError = FileHandle.standardError

try! process.run()
process.waitUntilExit()

if process.terminationStatus != 0 {
    try? FileManager.default.removeItem(at: tempDir)
    exit(process.terminationStatus)
}
