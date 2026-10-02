use std::path::Path;

/// macOS 文件系统通常不区分大小写，但 Path::starts_with 是区分大小写的。
/// 为避免由于路径大小写不一致（例如 /Users/HuangY vs /Users/huangy）导致过滤失效，
/// 在 macOS 上提供不区分大小写的比较。
pub fn path_starts_with_ci(path: &Path, prefix: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        let path_s = path.to_string_lossy().to_lowercase();
        let prefix_s = prefix.to_string_lossy().to_lowercase();
        if path_s == prefix_s {
            return true;
        }
        if path_s.starts_with(&prefix_s) {
            // 确保是完整的组件匹配，避免 /Users/abc 匹配 /Users/abcd
            let prefix_len = prefix_s.len();
            if let Some(c) = path_s.chars().nth(prefix_len) {
                return c == '/' || prefix_s.ends_with('/');
            }
        }
        false
    }
    #[cfg(not(target_os = "macos"))]
    {
        path.starts_with(prefix)
    }
}

pub fn paths_equal_ci(a: &Path, b: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    }
    #[cfg(not(target_os = "macos"))]
    {
        a == b
    }
}
