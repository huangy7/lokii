import AppKit
import SwiftUI

// MARK: - 过滤浮层视图宿主状态模型

/// 桥接 SwiftUI 声明式状态与 AppKit 控制器生命周期的独立状态容器
private final class FilterPopoverViewModel: ObservableObject {
    @Published var state: FilterState = FilterState()
}

/// SwiftUI 包装视图，统一双向绑定并分发条件变更与完成关闭事件
private struct FilterPopoverContainerView: View {
    @ObservedObject var viewModel: FilterPopoverViewModel
    var onStateChanged: ((FilterState) -> Void)?
    var onDismiss: (() -> Void)?

    var body: some View {
        FilterPopoverView(
            state: $viewModel.state,
            onStateChanged: onStateChanged,
            onDismiss: onDismiss
        )
    }
}

// MARK: - 过滤气泡浮层控制器

/// 负责 AppKit NSPopover 容器生命周期管理、双向语法同步协调与胶囊按钮唤起联动。
/// 核心职责：
/// 1. 挂载 SwiftUI 原生毛玻璃气泡视图，设置瞬态（transient）响应行为以支持失焦与 Esc 自动收起；
/// 2. 对接 FilterQuerySyncCoordinator 语法协调器，支持弹出前逆向回填状态与点选时即搜语法回写；
/// 3. 与主视窗 SearchWindow 工具栏过滤胶囊按钮保持双向状态联动与数量角标刷新。
public final class FilterPopoverController: NSObject, NSPopoverDelegate {
    private let popover = NSPopover()
    private let coordinator = FilterQuerySyncCoordinator()
    private let viewModel = FilterPopoverViewModel()
    private weak var searchField: NSSearchField?
    private var onSearch: ((String) -> Void)?
    private var onFilterStateChanged: ((FilterState) -> Void)?

    /// 浮层是否处于展示状态
    public var isShown: Bool {
        popover.isShown
    }

    /// 浮层动画开关（默认开启，支持无障碍减少动态效果或自动化测试）
    public var animates: Bool {
        get { popover.animates }
        set { popover.animates = newValue }
    }

    /// 当前激活的过滤状态快照
    public var currentState: FilterState {
        viewModel.state
    }

    public init(animates: Bool = true) {
        super.init()
        setupPopover(animates: animates)
    }

    // MARK: - 浮层装配与事件委托

    private func setupPopover(animates: Bool) {
        popover.behavior = .transient
        popover.animates = animates
        popover.delegate = self

        let containerView = FilterPopoverContainerView(
            viewModel: viewModel,
            onStateChanged: { [weak self] newState in
                guard let self = self else { return }
                self.viewModel.state = newState
                self.onFilterStateChanged?(newState)
                if let field = self.searchField {
                    // 若用户并非在浮层内部数值输入框中键入，确保搜索框处于编辑态以便光标推进至尾部
                    let popoverEditorActive = self.popover.contentViewController?.view.window?.firstResponder is NSTextView
                    if field.currentEditor() == nil && !popoverEditorActive {
                        field.window?.makeFirstResponder(field)
                    }
                    self.coordinator.updateQueryFromUI(state: newState, searchField: field) { [weak self] query in
                        self?.onSearch?(query)
                    }
                }
            },
            onDismiss: { [weak self] in
                self?.close()
            }
        )

        let hostingController = NSHostingController(rootView: containerView)
        if #available(macOS 13.0, *) {
            hostingController.sizingOptions = [.preferredContentSize]
        }
        popover.contentViewController = hostingController
        updatePopoverContentSize()
    }

    /// 预测量 SwiftUI 宿主视图的真实拟合尺寸并同步至 NSPopover.contentSize。
    /// 原因：AppKit NSPopover 在首次弹出计算屏幕坐标时依赖预设的 contentSize 确定窗口底边原点（origin.y = anchorBottomY - windowHeight）；
    /// 若未提前写入真实尺寸，NSHostingController 挂载后高度膨胀会导致浮层保持底边不变而向上顶穿工具栏。
    private func updatePopoverContentSize() {
        guard let hostingView = popover.contentViewController?.view else { return }
        hostingView.layoutSubtreeIfNeeded()
        let fitting = hostingView.fittingSize
        if fitting.width > 0 && fitting.height > 0 {
            popover.contentSize = fitting
        }
    }

    // MARK: - 公共联动接口

    /// 配置搜索输入框弱引用与检索/状态回调闭包
    public func configure(
        searchField: NSSearchField,
        onSearch: @escaping (String) -> Void,
        onFilterStateChanged: @escaping (FilterState) -> Void
    ) {
        self.searchField = searchField
        self.onSearch = onSearch
        self.onFilterStateChanged = onFilterStateChanged
    }

    /// 弹出气泡浮层，并在展示前根据当前输入框文本反向解析回填状态
    public func show(relativeTo view: NSView) {
        guard view.window != nil else { return }
        if let field = searchField {
            syncStateFromQuery(field.stringValue)
        }
        updatePopoverContentSize()
        view.window?.contentView?.layoutSubtreeIfNeeded()
        view.superview?.layoutSubtreeIfNeeded()
        let anchorRect = view.bounds.isEmpty ? NSRect(x: 0, y: 0, width: 38, height: 22) : view.bounds
        popover.show(relativeTo: anchorRect, of: view, preferredEdge: .maxY)
    }

    /// 关闭气泡浮层并归还第一响应者焦点至主搜索框
    public func close() {
        popover.performClose(nil)
        if popover.isShown {
            popover.close()
        }
        restoreSearchFieldFocus()
    }

    /// NSPopoverDelegate 代理回调：浮层彻底收起后交接焦点，确保键盘交互链路连续
    public func popoverDidClose(_ notification: Notification) {
        restoreSearchFieldFocus()
    }

    /// 恢复搜索输入框的第一响应者焦点并将光标置于末尾，保障全键盘交互不丢光标
    private func restoreSearchFieldFocus() {
        guard let field = searchField else { return }
        field.window?.makeFirstResponder(field)
        if let editor = field.currentEditor() as? NSTextView {
            PillLayoutManager.ensureInstalled(on: editor)
            let length = (field.stringValue as NSString).length
            editor.selectedRange = NSRange(location: length, length: 0)
        }
    }

    /// 切换气泡浮层的显隐状态
    public func toggle(relativeTo view: NSView) {
        if popover.isShown {
            close()
        } else {
            show(relativeTo: view)
        }
    }

    /// 外部输入框按键变动时，实时同步更新浮层内部选定状态
    public func syncStateFromQuery(_ query: String) {
        coordinator.updateUIFromQuery(query: query) { [weak self] newState in
            self?.viewModel.state = newState
            self?.onFilterStateChanged?(newState)
        }
    }
}
