import Foundation

/// 目录聚合树节点模型
///
/// 承载当前目录层级的显示片段、绝对完整路径、直属命中与后序累加命中计数。
/// 支持单链空目录紧凑折叠（如 "workspace/lokii/src"）。
public final class DirectoryTreeNode: Hashable {
    /// 节点显示名称（如 "src" 或紧凑折叠后的 "workspace/lokii/src"）
    public var component: String
    /// 目录完整绝对路径
    public var fullPath: String
    /// 该目录下直接匹配条目数
    public var directMatchCount: Int
    /// 该目录及所有子目录下累计匹配条目数
    public var recursiveMatchCount: Int
    /// 子级目录列表
    public var children: [DirectoryTreeNode]
    /// 弱引用父节点，避免循环引用
    public weak var parent: DirectoryTreeNode?

    public init(
        component: String,
        fullPath: String,
        directMatchCount: Int = 0,
        recursiveMatchCount: Int = 0,
        children: [DirectoryTreeNode] = [],
        parent: DirectoryTreeNode? = nil
    ) {
        self.component = component
        self.fullPath = fullPath
        self.directMatchCount = directMatchCount
        self.recursiveMatchCount = recursiveMatchCount
        self.children = children
        self.parent = parent
    }

    public static func == (lhs: DirectoryTreeNode, rhs: DirectoryTreeNode) -> Bool {
        if lhs === rhs { return true }
        return lhs.component == rhs.component &&
               lhs.fullPath == rhs.fullPath &&
               lhs.directMatchCount == rhs.directMatchCount &&
               lhs.recursiveMatchCount == rhs.recursiveMatchCount &&
               lhs.children == rhs.children
    }

    public func hash(into hasher: inout Hasher) {
        hasher.combine(fullPath)
    }
}

/// 动态前缀聚合树模型
///
/// 消费即时检索结果集 `[FfiSearchResult]`，在内存中构建目录层级 Trie 并执行
/// 单链空目录紧凑折叠（Compact Folders），支持亚毫秒级结果集范围过滤。
public final class ResultDirectoryTree {
    /// 聚合树顶层根节点集合（已按标准名称升序排列）
    public let rootNodes: [DirectoryTreeNode]
    /// 纳入该树累计计算的总命中数
    public let totalMatchCount: Int

    public init(rootNodes: [DirectoryTreeNode], totalMatchCount: Int) {
        self.rootNodes = rootNodes
        self.totalMatchCount = totalMatchCount
    }

    /// 从检索结果集构建动态聚合前缀树
    ///
    /// - Parameters:
    ///   - results: 搜索命中的文件/目录结果数组
    ///   - rootPrefix: 可选根路径前缀。若指定，则仅聚合该前缀下的结果，并以其相对层级展开；若为 nil，则从全局顶层开始折叠。
    /// - Returns: 组装并压缩完成的 `ResultDirectoryTree`
    public static func build(from results: [FfiSearchResult], rootPrefix: String? = nil) -> ResultDirectoryTree {
        guard !results.isEmpty else {
            return ResultDirectoryTree(rootNodes: [], totalMatchCount: 0)
        }

        // 规范化 rootPrefix；根斜杠本身等价于全局范围
        let normalizedRootPrefix: String? = {
            guard let prefix = rootPrefix, !prefix.isEmpty, prefix != "/" else { return nil }
            return normalizedPath(prefix)
        }()

        // 筛选出属于 rootPrefix 范围的结果（若指定）
        let scopedResults = filter(results: results, byDirectoryPrefix: normalizedRootPrefix)

        guard !scopedResults.isEmpty else {
            return ResultDirectoryTree(rootNodes: [], totalMatchCount: 0)
        }

        // 1. 构建 Trie 前缀树
        let rootBuilder = TrieBuilderNode(component: "", fullPath: normalizedRootPrefix ?? "")

        for result in scopedResults {
            let dirPath: String
            if result.isDir {
                dirPath = result.path
            } else {
                dirPath = (result.path as NSString).deletingLastPathComponent
            }

            // 规范化路径，去除冗余尾斜杠
            let cleanDirPath = normalizedPath(dirPath)

            // 计算路径分段
            let components: [String]
            let basePath: String
            if let root = normalizedRootPrefix {
                if cleanDirPath == root {
                    components = []
                    basePath = root
                } else if isContained(cleanDirPath, inDirectory: root) {
                    let relative = String(cleanDirPath.dropFirst(root.count + 1))
                    components = relative.split(separator: "/").map(String.init)
                    basePath = root
                } else {
                    components = cleanDirPath.split(separator: "/").map(String.init)
                    basePath = ""
                }
            } else {
                components = cleanDirPath.split(separator: "/").map(String.init)
                basePath = ""
            }

            var current = rootBuilder
            var currentFullPath = basePath

            for comp in components {
                if currentFullPath.isEmpty || currentFullPath == "/" {
                    currentFullPath = "/" + comp
                } else {
                    currentFullPath = currentFullPath + "/" + comp
                }

                if let existing = current.children[comp] {
                    current = existing
                } else {
                    let newChild = TrieBuilderNode(component: comp, fullPath: currentFullPath)
                    current.children[comp] = newChild
                    current = newChild
                }
            }

            current.directMatchCount += 1
        }

        // 2. 后序 DFS 转换、递归计数统计与单链紧凑折叠
        let sortedTopKeys = rootBuilder.children.keys.sorted {
            $0.localizedStandardCompare($1) == .orderedAscending
        }

        var rootNodes: [DirectoryTreeNode] = []

        // 若根构建器本身有直属命中（例如结果正好命中在 rootPrefix 目录本身）
        if rootBuilder.directMatchCount > 0 {
            let rootNode = DirectoryTreeNode(
                component: normalizedRootPrefix.map { ($0 as NSString).lastPathComponent } ?? "/",
                fullPath: rootBuilder.fullPath.isEmpty ? "/" : rootBuilder.fullPath,
                directMatchCount: rootBuilder.directMatchCount,
                recursiveMatchCount: rootBuilder.directMatchCount,
                children: [],
                parent: nil
            )
            rootNodes.append(rootNode)
        }

        for key in sortedTopKeys {
            let childBuilder = rootBuilder.children[key]!
            let childNode = convertAndCompress(builder: childBuilder)
            rootNodes.append(childNode)
        }

        // 根节点按名称标准排序
        rootNodes.sort {
            $0.component.localizedStandardCompare($1.component) == .orderedAscending
        }

        return ResultDirectoryTree(rootNodes: rootNodes, totalMatchCount: scopedResults.count)
    }

    /// 基于选中的目录前缀过滤检索结果
    ///
    /// - Parameters:
    ///   - results: 待过滤的全量搜索结果集
    ///   - prefix: 目标目录完整路径，为 nil 或空字符串时返回全量结果集
    /// - Returns: 属于该目录或其子目录的结果集合
    public static func filter(results: [FfiSearchResult], byDirectoryPrefix prefix: String?) -> [FfiSearchResult] {
        guard let prefix = prefix, !prefix.isEmpty else {
            return results
        }

        let cleanPrefix = normalizedPath(prefix)
        if cleanPrefix == "/" {
            return results.filter { $0.path.hasPrefix("/") }
        }

        return results.filter { isContained($0.path, inDirectory: cleanPrefix) }
    }

    // MARK: - Path Helpers

    /// 去除路径冗余尾斜杠；根斜杠本身保持不变
    private static func normalizedPath(_ path: String) -> String {
        path.count > 1 && path.hasSuffix("/") ? String(path.dropLast()) : path
    }

    /// 判断路径是否位于目标目录之下（含目录自身）
    static func isContained(_ path: String, inDirectory directory: String) -> Bool {
        path == directory || path.hasPrefix(directory + "/")
    }

    // MARK: - Private Helper Methods

    private static func convertAndCompress(builder: TrieBuilderNode) -> DirectoryTreeNode {
        let sortedChildKeys = builder.children.keys.sorted {
            $0.localizedStandardCompare($1) == .orderedAscending
        }

        var convertedChildren: [DirectoryTreeNode] = []
        for key in sortedChildKeys {
            let childBuilder = builder.children[key]!
            convertedChildren.append(convertAndCompress(builder: childBuilder))
        }

        let recursiveCount = builder.directMatchCount + convertedChildren.reduce(0) { $0 + $1.recursiveMatchCount }

        let node = DirectoryTreeNode(
            component: builder.component,
            fullPath: builder.fullPath,
            directMatchCount: builder.directMatchCount,
            recursiveMatchCount: recursiveCount,
            children: convertedChildren
        )

        for child in convertedChildren {
            child.parent = node
        }

        // 单链紧凑折叠 (Compact Folders)：
        // 若当前节点无直属命中且仅拥有 1 个子节点，则合并为 component/childComponent
        var currentNode = node
        while currentNode.directMatchCount == 0 && currentNode.children.count == 1 {
            let singleChild = currentNode.children[0]
            let mergedComponent = currentNode.component.isEmpty
                ? singleChild.component
                : "\(currentNode.component)/\(singleChild.component)"

            let mergedNode = DirectoryTreeNode(
                component: mergedComponent,
                fullPath: singleChild.fullPath,
                directMatchCount: singleChild.directMatchCount,
                recursiveMatchCount: singleChild.recursiveMatchCount,
                children: singleChild.children
            )
            for grandchild in mergedNode.children {
                grandchild.parent = mergedNode
            }
            currentNode = mergedNode
        }

        return currentNode
    }
}

// MARK: - Trie Builder Internal Node

private final class TrieBuilderNode {
    var component: String
    var fullPath: String
    var directMatchCount: Int = 0
    var children: [String: TrieBuilderNode] = [:]

    init(component: String, fullPath: String) {
        self.component = component
        self.fullPath = fullPath
    }
}
