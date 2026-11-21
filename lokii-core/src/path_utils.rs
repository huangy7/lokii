use std::path::{Component, Path};

#[cfg(target_os = "macos")]
fn component_eq_ci(a: Component<'_>, b: Component<'_>) -> bool {
    match (a, b) {
        (Component::Normal(sa), Component::Normal(sb)) => {
            if sa == sb {
                return true;
            }
            let la = sa.to_string_lossy();
            let lb = sb.to_string_lossy();
            if la.is_ascii() && lb.is_ascii() {
                la.eq_ignore_ascii_case(&lb)
            } else {
                la.to_lowercase() == lb.to_lowercase()
            }
        }
        _ => a == b,
    }
}

/// Compare two paths by components, treating normal segments case-insensitively on macOS.
///
/// Component-level matching avoids UTF-8 byte-vs-char indexing mismatches on non-ASCII paths
/// and respects component boundaries (e.g. `/Volumes/Disk` does not match `/Volumes/Disk2`).
pub fn path_starts_with_ci(path: &Path, prefix: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        let mut path_comps = path.components();
        for p_comp in prefix.components() {
            match path_comps.next() {
                Some(c) if component_eq_ci(c, p_comp) => {}
                _ => return false,
            }
        }
        true
    }
    #[cfg(not(target_os = "macos"))]
    {
        path.starts_with(prefix)
    }
}

/// Compare two paths for equality, ignoring ASCII/Unicode case differences on macOS.
pub fn paths_equal_ci(a: &Path, b: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        let mut ac = a.components();
        let mut bc = b.components();
        loop {
            match (ac.next(), bc.next()) {
                (Some(ca), Some(cb)) if component_eq_ci(ca, cb) => {}
                (None, None) => return true,
                _ => return false,
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_ascii_and_boundary_matching() {
        assert!(path_starts_with_ci(
            Path::new("/Volumes/共享盘/项目/报告.pdf"),
            Path::new("/Volumes/共享盘")
        ));
        assert!(path_starts_with_ci(
            Path::new("/Volumes/共享盘"),
            Path::new("/Volumes/共享盘/")
        ));
        assert!(!path_starts_with_ci(
            Path::new("/Volumes/共享盘2/报告.pdf"),
            Path::new("/Volumes/共享盘")
        ));
        assert!(!path_starts_with_ci(
            Path::new("/Volumes/Disk2/file.txt"),
            Path::new("/Volumes/Disk")
        ));
        assert!(path_starts_with_ci(
            Path::new("/Volumes/Foo/bar"),
            Path::new("/")
        ));
        assert!(!path_starts_with_ci(Path::new("relative/path"), Path::new("/")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_case_insensitive_ascii_and_unicode() {
        assert!(path_starts_with_ci(
            Path::new("/Users/张三/文档/notes.md"),
            Path::new("/users/张三")
        ));
        assert!(path_starts_with_ci(
            Path::new("/Users/Café/notes.md"),
            Path::new("/users/café")
        ));
        assert!(paths_equal_ci(
            Path::new("/Volumes/共享盘/"),
            Path::new("/volumes/共享盘")
        ));
        assert!(!paths_equal_ci(
            Path::new("/Volumes/共享盘"),
            Path::new("/Volumes/共享盘2")
        ));
    }
}
