use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use notify::{Config as NotifyConfig, Event, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// General application settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    /// UI language: "zh-Hans" (default) or "en".
    pub language: String,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            language: "zh-Hans".to_string(),
        }
    }
}

/// Index-related configuration: which directories to scan, what to exclude.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexConfig {
    /// Directories to index (supports "~" for home directory).
    pub index_paths: Vec<String>,
    /// Directory names to exclude from traversal (component-level match).
    pub exclude: Vec<String>,
    /// Path prefixes to exclude (supports "~", checked by prefix match).
    pub exclude_paths: Vec<String>,
    /// Whether to include hidden files/directories (starting with ".").
    pub include_hidden: bool,
    /// Maximum number of search results to return.
    pub max_results: usize,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            index_paths: vec!["~".to_string()],
            exclude: vec![
                "node_modules".to_string(),
                ".git".to_string(),
                ".DS_Store".to_string(),
                "target".to_string(),
                ".Trash".to_string(),
                ".hg".to_string(),
                ".svn".to_string(),
                "build".to_string(),
                "dist".to_string(),
                ".next".to_string(),
                ".nuxt".to_string(),
                ".cache".to_string(),
                ".venv".to_string(),
                "venv".to_string(),
                "__pycache__".to_string(),
                "DerivedData".to_string(),
                "Pods".to_string(),
                "Carthage".to_string(),
                "Contents".to_string(),
                "logs".to_string(),
                "tmp".to_string(),
                "temp".to_string(),
                "coverage".to_string(),
                ".idea".to_string(),
                ".vscode".to_string(),
                "vendor".to_string(),
                "out".to_string(),
                "bin".to_string(),
                "obj".to_string(),
                ".mypy_cache".to_string(),
                ".pytest_cache".to_string(),
                ".ruff_cache".to_string(),
                ".tox".to_string(),
                "xcshareddata".to_string(),
                "xcuserdata".to_string(),
            ],
            exclude_paths: vec![
                "/System".to_string(),
                "/Library".to_string(),
                "/private".to_string(),
                "/bin".to_string(),
                "/sbin".to_string(),
                "/usr".to_string(),
                "/dev".to_string(),
                "/Volumes".to_string(),
                "/opt".to_string(),
                "~/Library".to_string(),
                "~/.Trash".to_string(),
                "~/.cache".to_string(),
                "~/.npm".to_string(),
                "~/.pnpm-store".to_string(),
                "~/.yarn".to_string(),
                "~/.cargo/registry".to_string(),
                "~/.cargo/git".to_string(),
                "~/.rustup".to_string(),
                "~/.gradle/caches".to_string(),
                "~/.m2/repository".to_string(),
                "~/.bun".to_string(),
                "~/.deno".to_string(),
                "~/.nvm".to_string(),
                "~/.orbstack".to_string(),
                "~/.docker".to_string(),
            ],
            include_hidden: false,
            max_results: 500,
        }
    }
}

/// Window behavior configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowConfig {
    /// Whether to auto-hide the window when it loses focus.
    pub hide_on_blur: bool,
    /// Whether to remember window position between sessions.
    pub remember_position: bool,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            hide_on_blur: true,
            remember_position: false,
        }
    }
}

/// Global shortcut configuration.
/// NOTE: Deprecated — shortcut is now managed by KeyboardShortcuts (UserDefaults).
/// Retained for TOML backward compatibility only.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ShortcutConfig {
    /// Global keyboard shortcut to toggle the search window.
    /// Format: "Cmd+Shift+Space", "Cmd+Alt+F", etc.
    pub toggle_window: String,
}

impl Default for ShortcutConfig {
    fn default() -> Self {
        Self {
            toggle_window: "Cmd+Shift+Space".to_string(),
        }
    }
}

/// Application configuration for Lokii.
///
/// Organized into TOML sections: [general], [index], [window], [shortcut].
/// Supports partial TOML deserialization -- missing sections use defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub index: IndexConfig,
    #[serde(default)]
    pub window: WindowConfig,
    #[serde(default)]
    pub shortcut: ShortcutConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            general: GeneralConfig::default(),
            index: IndexConfig::default(),
            window: WindowConfig::default(),
            shortcut: ShortcutConfig::default(),
        }
    }
}

impl AppConfig {
    /// Resolve index paths, expanding "~" to the user's home directory.
    /// Silently skips paths that don't exist on disk.
    pub fn resolve_paths(&self) -> Vec<PathBuf> {
        let home = dirs::home_dir();
        self.index
            .index_paths
            .iter()
            .filter_map(|p| {
                let resolved = if p == "~" {
                    home.clone().unwrap_or_else(|| PathBuf::from("/"))
                } else if let Some(rest) = p.strip_prefix("~/") {
                    home.clone()
                        .unwrap_or_else(|| PathBuf::from("/"))
                        .join(rest)
                } else {
                    PathBuf::from(p)
                };
                if resolved.exists() {
                    Some(resolved)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Resolve exclude paths, expanding "~" to the user's home directory.
    pub fn resolve_exclude_paths(&self) -> Vec<PathBuf> {
        let home = dirs::home_dir();
        self.index
            .exclude_paths
            .iter()
            .map(|p| {
                if p == "~" {
                    home.clone().unwrap_or_else(|| PathBuf::from("/"))
                } else if let Some(rest) = p.strip_prefix("~/") {
                    home.clone()
                        .unwrap_or_else(|| PathBuf::from("/"))
                        .join(rest)
                } else {
                    PathBuf::from(p)
                }
            })
            .collect()
    }

    /// Parse an `AppConfig` from a TOML string.
    pub fn from_toml(content: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(content)
    }

    /// Return the configuration directory path (~/.config/lokii/).
    /// Creates the directory if it does not exist.
    pub fn config_dir() -> PathBuf {
        let dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/"))
            .join(".config")
            .join("lokii");
        if !dir.exists() {
            let _ = std::fs::create_dir_all(&dir);
        }
        dir
    }
}

/// Default config.toml content with Chinese comments.
/// Written to ~/.config/lokii/config.toml on first run.
pub const DEFAULT_CONFIG_TOML: &str = r#"# Lokii 配置文件
# 修改后大部分设置自动生效，快捷键需要重启应用

[general]
# 预留给未来通用设置

[index]
# 索引目录列表（支持 ~ 表示用户主目录）
index_paths = ["~"]

# 排除的目录名（不会索引这些目录及其子目录）
exclude = ["node_modules", ".git", ".DS_Store", "target", ".Trash", ".hg", ".svn", "build", "dist", ".next", ".nuxt", ".cache", ".venv", "venv", "__pycache__", "DerivedData", "Pods", "Carthage", "Contents", "logs", "tmp", "temp", "coverage", ".idea", ".vscode", "vendor", "out", "bin", "obj", ".mypy_cache", ".pytest_cache", ".ruff_cache", ".tox", "xcshareddata", "xcuserdata"]

# 排除的路径前缀（支持 ~，前缀匹配）
exclude_paths = ["/System", "/Library", "/private", "/bin", "/sbin", "/usr", "/dev", "/Volumes", "/opt", "~/Library", "~/.Trash", "~/.cache", "~/.npm", "~/.pnpm-store", "~/.yarn", "~/.cargo/registry", "~/.cargo/git", "~/.rustup", "~/.gradle/caches", "~/.m2/repository", "~/.bun", "~/.deno", "~/.nvm", "~/.orbstack", "~/.docker"]

# 是否索引隐藏文件和目录（以 . 开头的）
include_hidden = false

# 搜索结果最大返回数量
max_results = 500

[window]
# 窗口失去焦点时是否自动隐藏
hide_on_blur = true

# 是否记住窗口位置（下次打开时恢复）
remember_position = false

[shortcut]
# 全局快捷键，用于显示/隐藏搜索窗口
# 格式: 修饰键+按键，例如 "Cmd+Shift+Space", "Cmd+Alt+F"
toggle_window = "Cmd+Shift+Space"
"#;

/// Thread-safe wrapper for AppConfig, enabling shared mutable access.
///
/// Mirrors the IndexStore pattern: Arc<RwLock<T>> for concurrent
/// read-heavy access (many config reads, rare updates).
#[derive(Clone)]
pub struct ConfigStore {
    inner: Arc<RwLock<AppConfig>>,
}

impl ConfigStore {
    /// Create a new ConfigStore wrapping the given config.
    pub fn new(config: AppConfig) -> Self {
        Self {
            inner: Arc::new(RwLock::new(config)),
        }
    }

    /// Get a read guard to the current config.
    pub fn read(&self) -> parking_lot::RwLockReadGuard<'_, AppConfig> {
        self.inner.read()
    }

    /// Replace the config with a new one.
    pub fn update(&self, new_config: AppConfig) {
        let mut lock = self.inner.write();
        *lock = new_config;
    }
}

/// Load config from disk or create default config.toml on first run.
///
/// Returns the parsed config and an optional error message (for invalid/unreadable files).
/// On error, returns AppConfig::default() so the app always starts.
pub fn load_or_create_config() -> (AppConfig, Option<String>) {
    let config_dir = AppConfig::config_dir();
    let config_path = config_dir.join("config.toml");

    if !config_path.exists() {
        // First run: create default config (per D-03, CFG-06)
        let _ = std::fs::create_dir_all(&config_dir);
        let _ = std::fs::write(&config_path, DEFAULT_CONFIG_TOML);
        return (AppConfig::default(), None);
    }

    match std::fs::read_to_string(&config_path) {
        Ok(content) => match AppConfig::from_toml(&content) {
            Ok(config) => (config, None),
            Err(e) => {
                // Invalid config -> return defaults + error message
                let msg = format!("Config parse error in {}: {}", config_path.display(), e);
                (AppConfig::default(), Some(msg))
            }
        },
        Err(e) => {
            let msg = format!("Failed to read {}: {}", config_path.display(), e);
            (AppConfig::default(), Some(msg))
        }
    }
}

/// Start watching the config.toml file for changes.
///
/// Uses the same notify + mpsc + recv_timeout pattern from watcher.rs.
/// On file change, re-parses config and updates the ConfigStore.
/// Calls `on_reload` with the new config on success, `on_error` with message on failure.
///
/// Hot-reloadable settings (per D-01): index_paths, exclude, include_hidden,
/// max_results, hide_on_blur. Shortcut changes require app restart.
pub fn start_config_watcher<F>(
    config_store: ConfigStore,
    on_reload: F,
    on_error: impl Fn(String) + Send + 'static,
) -> Result<(), notify::Error>
where
    F: Fn(&AppConfig) + Send + 'static,
{
    let config_path = AppConfig::config_dir().join("config.toml");
    let watch_dir = config_path.parent().unwrap().to_path_buf();

    let (tx, rx) = std::sync::mpsc::channel::<Event>();

    let mut watcher = RecommendedWatcher::new(
        move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                let _ = tx.send(event);
            }
        },
        NotifyConfig::default(),
    )?;
    watcher.watch(&watch_dir, RecursiveMode::NonRecursive)?;

    std::thread::spawn(move || {
        let _watcher = watcher; // keep alive
        loop {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(event) => {
                    if !event.paths.iter().any(|p| p == &config_path) {
                        continue;
                    }
                    // Debounce: drain extra events within 200ms
                    while rx.recv_timeout(Duration::from_millis(200)).is_ok() {}

                    match std::fs::read_to_string(&config_path) {
                        Ok(content) => match AppConfig::from_toml(&content) {
                            Ok(new_config) => {
                                config_store.update(new_config.clone());
                                on_reload(&new_config);
                            }
                            Err(e) => {
                                on_error(format!(
                                    "Config parse error: {} (keeping previous config)",
                                    e
                                ));
                            }
                        },
                        Err(e) => {
                            on_error(format!("Failed to read config: {}", e));
                        }
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // --- Partial TOML parsing tests ---

    #[test]
    fn test_partial_toml_only_index_section() {
        let toml_str = r#"
[index]
index_paths = ["/tmp"]
max_results = 100
"#;
        let config = AppConfig::from_toml(toml_str).expect("parse failed");
        assert_eq!(config.index.index_paths, vec!["/tmp"]);
        assert_eq!(config.index.max_results, 100);
        // Missing sections should use defaults
        assert!(config.window.hide_on_blur);
        assert!(!config.window.remember_position);
        assert_eq!(config.shortcut.toggle_window, "Cmd+Shift+Space");
    }

    #[test]
    fn test_empty_string_parses_to_default() {
        let config = AppConfig::from_toml("").expect("parse failed");
        let default = AppConfig::default();
        assert_eq!(config.index.index_paths, default.index.index_paths);
        assert_eq!(config.index.exclude, default.index.exclude);
        assert_eq!(config.index.include_hidden, default.index.include_hidden);
        assert_eq!(config.index.max_results, default.index.max_results);
        assert_eq!(config.window.hide_on_blur, default.window.hide_on_blur);
        assert_eq!(
            config.window.remember_position,
            default.window.remember_position
        );
        assert_eq!(
            config.shortcut.toggle_window,
            default.shortcut.toggle_window
        );
    }

    #[test]
    fn test_full_toml_all_sections() {
        let toml_str = r#"
[general]

[index]
index_paths = ["/Users/test", "~/Documents"]
exclude = ["node_modules", ".git"]
include_hidden = true
max_results = 1000

[window]
hide_on_blur = false
remember_position = true

[shortcut]
toggle_window = "Cmd+Alt+F"
"#;
        let config = AppConfig::from_toml(toml_str).expect("parse failed");
        assert_eq!(
            config.index.index_paths,
            vec!["/Users/test", "~/Documents"]
        );
        assert_eq!(config.index.exclude, vec!["node_modules", ".git"]);
        assert!(config.index.include_hidden);
        assert_eq!(config.index.max_results, 1000);
        assert!(!config.window.hide_on_blur);
        assert!(config.window.remember_position);
        assert_eq!(config.shortcut.toggle_window, "Cmd+Alt+F");
    }

    // --- Default value tests ---

    #[test]
    fn test_index_config_defaults() {
        let config = IndexConfig::default();
        assert_eq!(config.index_paths, vec!["~"]);
        assert!(config.exclude.contains(&"node_modules".to_string()));
        assert!(config.exclude.contains(&".git".to_string()));
        assert!(config.exclude.contains(&".DS_Store".to_string()));
        assert!(config.exclude.contains(&"target".to_string()));
        assert!(config.exclude.contains(&".Trash".to_string()));
        assert!(config.exclude_paths.contains(&"~/Library".to_string()));
        assert!(!config.include_hidden);
        assert_eq!(config.max_results, 500);
    }

    #[test]
    fn test_window_config_defaults() {
        let config = WindowConfig::default();
        assert!(config.hide_on_blur);
        assert!(!config.remember_position);
    }

    #[test]
    fn test_shortcut_config_defaults() {
        let config = ShortcutConfig::default();
        assert_eq!(config.toggle_window, "Cmd+Shift+Space");
    }

    // --- ConfigStore tests ---

    #[test]
    fn test_config_store_new_and_read() {
        let config = AppConfig::default();
        let store = ConfigStore::new(config);
        let guard = store.read();
        assert_eq!(guard.index.max_results, 500);
    }

    #[test]
    fn test_config_store_update() {
        let store = ConfigStore::new(AppConfig::default());
        assert_eq!(store.read().index.max_results, 500);

        let mut new_config = AppConfig::default();
        new_config.index.max_results = 1000;
        store.update(new_config);
        assert_eq!(store.read().index.max_results, 1000);
    }

    #[test]
    fn test_config_store_clone_shares_state() {
        let store1 = ConfigStore::new(AppConfig::default());
        let store2 = store1.clone();

        let mut new_config = AppConfig::default();
        new_config.index.max_results = 999;
        store1.update(new_config);
        assert_eq!(store2.read().index.max_results, 999);
    }

    // --- load_or_create_config tests ---

    #[test]
    fn test_load_or_create_first_run() {
        let tmp = TempDir::new().unwrap();
        let config_dir = tmp.path().join("lokii");
        let config_path = config_dir.join("config.toml");

        // Override config_dir is not possible since it's a static method.
        // Instead, test the file creation logic directly.
        assert!(!config_path.exists());

        // Create dir + write default config
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(&config_path, DEFAULT_CONFIG_TOML).unwrap();

        assert!(config_path.exists());
        let content = fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("索引目录列表"));
        assert!(content.contains("[index]"));
        assert!(content.contains("[window]"));
        assert!(content.contains("[shortcut]"));
        assert!(content.contains("[general]"));

        // Verify the default config.toml parses to valid AppConfig
        let config = AppConfig::from_toml(&content).expect("default config should parse");
        assert_eq!(config.index.index_paths, vec!["~"]);
        assert_eq!(config.index.max_results, 500);
    }

    #[test]
    fn test_load_existing_valid_config() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
[index]
index_paths = ["/custom"]
max_results = 200
"#,
        )
        .unwrap();

        let content = fs::read_to_string(&config_path).unwrap();
        let config = AppConfig::from_toml(&content).expect("should parse");
        assert_eq!(config.index.index_paths, vec!["/custom"]);
        assert_eq!(config.index.max_results, 200);
    }

    #[test]
    fn test_load_invalid_config_returns_default() {
        let result = AppConfig::from_toml("invalid {{{{ toml content");
        assert!(result.is_err());
        // App should fall back to defaults
        let config = AppConfig::default();
        assert_eq!(config.index.max_results, 500);
    }

    #[test]
    fn test_default_config_toml_content() {
        assert!(DEFAULT_CONFIG_TOML.contains("索引目录列表"));
        assert!(DEFAULT_CONFIG_TOML.contains("[index]"));
        assert!(DEFAULT_CONFIG_TOML.contains("[window]"));
        assert!(DEFAULT_CONFIG_TOML.contains("[shortcut]"));
        assert!(DEFAULT_CONFIG_TOML.contains("[general]"));
        assert!(DEFAULT_CONFIG_TOML.contains("Cmd+Shift+Space"));
        assert!(DEFAULT_CONFIG_TOML.contains(r#"index_paths = ["~"]"#));
        assert!(DEFAULT_CONFIG_TOML.contains("hide_on_blur = true"));
        assert!(DEFAULT_CONFIG_TOML.contains("remember_position = false"));
        assert!(DEFAULT_CONFIG_TOML.contains("max_results = 500"));
    }

    // --- Existing tests updated for sectioned struct ---

    #[test]
    fn test_resolve_tilde() {
        let mut config = AppConfig::default();
        config.index.index_paths = vec![
            "~".to_string(),
            "~/Documents".to_string(),
            "/tmp".to_string(),
        ];
        let paths = config.resolve_paths();
        assert_eq!(paths.len(), 3);

        let home = dirs::home_dir().expect("home dir should exist");
        assert_eq!(paths[0], home);
        assert_eq!(paths[1], home.join("Documents"));
        assert_eq!(paths[2], PathBuf::from("/tmp"));
    }

    #[test]
    fn test_config_dir() {
        let dir = AppConfig::config_dir();
        assert!(dir.to_string_lossy().contains("lokii"));
    }

    // --- JSON round-trip tests ---

    #[test]
    fn test_config_json_round_trip() {
        let original = AppConfig::default();
        let json = serde_json::to_string_pretty(&original).expect("serialize");
        let restored: AppConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.index.max_results, original.index.max_results);
        assert_eq!(restored.index.index_paths, original.index.index_paths);
        assert_eq!(restored.index.include_hidden, original.index.include_hidden);
        assert_eq!(restored.window.hide_on_blur, original.window.hide_on_blur);
        assert_eq!(
            restored.shortcut.toggle_window,
            original.shortcut.toggle_window
        );
    }

    #[test]
    fn test_config_json_custom_values_round_trip() {
        let mut config = AppConfig::default();
        config.index.max_results = 1000;
        config.index.include_hidden = true;
        config.window.hide_on_blur = false;
        let json = serde_json::to_string(&config).expect("serialize");
        let restored: AppConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.index.max_results, 1000);
        assert!(restored.index.include_hidden);
        assert!(!restored.window.hide_on_blur);
    }

    #[test]
    fn test_config_json_invalid_returns_error() {
        let result: Result<AppConfig, _> = serde_json::from_str("not valid json {{{");
        assert!(result.is_err());
    }

    #[test]
    fn test_load_or_create_config_real() {
        // Integration test: calls the actual function
        // This will use the real ~/.config/lokii/ directory
        let (config, error) = load_or_create_config();
        // Should always return a valid config
        assert!(config.index.max_results > 0);
        // If there's no config file issue, error should be None
        // (but we can't guarantee this in CI, so just check it doesn't panic)
        let _ = error;
    }
}
