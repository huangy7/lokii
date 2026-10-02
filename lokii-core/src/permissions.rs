use std::fs::File;

/// Check if the app has Full Disk Access on macOS.
/// Uses probe-file heuristic: tries to open the TCC database.
/// Returns true if FDA is granted, false if permission denied.
pub fn has_full_disk_access() -> bool {
    match File::open("/Library/Application Support/com.apple.TCC/TCC.db") {
        Ok(_) => true,
        Err(e) => e.kind() != std::io::ErrorKind::PermissionDenied,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_has_full_disk_access_returns_bool() {
        // Just verify it doesn't panic and returns a bool
        let _result: bool = has_full_disk_access();
    }
}
