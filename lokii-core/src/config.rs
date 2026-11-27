use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// 「索引应用程序」开关追加的索引根（存在才加入）。
pub(crate) const APP_INDEX_ROOTS: [&str; 3] = [
    "/Applications",
    "/System/Applications",
    "/System/Cryptexes/App/System/Applications",
];

/// macOS 程序坞（Dock）图标显示策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DockIconMode {
    /// 始终在程序坞常驻图标。
    Always,
    /// 仅在存在可见或最小化窗口时显示图标，所有窗口隐藏或关闭后自动切入菜单栏常驻。
    #[default]
    WhenOpen,
    /// 从不在程序坞显示图标，纯菜单栏与全局快捷键常驻。
    Never,
}

/// General application settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "GeneralConfigWire")]
pub struct GeneralConfig {
    /// UI language: "zh-Hans" (default) or "en".
    pub language: String,
    /// macOS Dock icon visibility policy.
    pub dock_icon_mode: DockIconMode,
    /// Preferred terminal application bundle identifier (e.g. "com.googlecode.iterm2")
    /// or absolute application bundle path. Defaults to "com.apple.Terminal".
    pub terminal_bundle_id: String,
    /// 搜索框为空且未激活任何过滤条件时，隐藏默认文件列表并展示轻量搜索语法引导。
    pub hide_results_when_empty: bool,
}

/// 兼容遗留 `show_in_dock` 布尔键的过渡反序列化视图：
/// 序列化时仅输出规范字段 `dock_icon_mode`，避免配置文件残留冲突双键。
#[derive(Deserialize)]
#[serde(default)]
struct GeneralConfigWire {
    language: String,
    dock_icon_mode: Option<DockIconMode>,
    show_in_dock: Option<bool>,
    terminal_bundle_id: String,
    hide_results_when_empty: bool,
}

impl Default for GeneralConfigWire {
    fn default() -> Self {
        let defaults = GeneralConfig::default();
        Self {
            language: defaults.language,
            dock_icon_mode: None,
            show_in_dock: None,
            terminal_bundle_id: defaults.terminal_bundle_id,
            hide_results_when_empty: defaults.hide_results_when_empty,
        }
    }
}

impl From<GeneralConfigWire> for GeneralConfig {
    fn from(wire: GeneralConfigWire) -> Self {
        let dock_icon_mode = wire.dock_icon_mode.unwrap_or_else(|| match wire.show_in_dock {
            Some(true) => DockIconMode::Always,
            Some(false) => DockIconMode::Never,
            None => DockIconMode::default(),
        });
        Self {
            language: wire.language,
            dock_icon_mode,
            terminal_bundle_id: wire.terminal_bundle_id,
            hide_results_when_empty: wire.hide_results_when_empty,
        }
    }
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            language: "zh-Hans".to_string(),
            dock_icon_mode: DockIconMode::WhenOpen,
            terminal_bundle_id: "com.apple.Terminal".to_string(),
            hide_results_when_empty: true,
        }
    }
}

/// Index-related configuration: which directories to scan, what to exclude.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "IndexConfigWire")]
pub struct IndexConfig {
    /// Directories to index (supports "~" for home directory).
    pub index_paths: Vec<String>,
    /// 是否自动索引应用程序目录（/Applications 与系统应用目录）；仅收录应用本身，若目录已在 `index_paths` 中则按常规规则索引。
    pub index_apps: bool,
    /// Directory names to exclude from traversal (component-level match).
    pub exclude: Vec<String>,
    /// Path prefixes to exclude (supports "~", checked by prefix match).
    pub exclude_paths: Vec<String>,
    /// Whether to include hidden files/directories (starting with ".").
    pub include_hidden: bool,
    /// 是否排除凭据目录与文件（.ssh、.gnupg、.aws 等）；仅对隐藏文件生效，显式加入索引根可穿透。
    pub exclude_sensitive: bool,
    /// `exclude_sensitive` 生效时排除的名字列表（目录或文件名，大小写无关）。
    pub sensitive_names: Vec<String>,
    /// Maximum number of search results to return.
    pub max_results: usize,
    /// 是否按设定间隔自动重扫在线网络卷；默认关闭以避免周期唤醒休眠中的 NAS 硬盘。
    pub network_rescan_enabled: bool,
    /// 开启自动重扫后，非本地卷（网络共享等）上的索引根每隔多少分钟整棵重扫一次；FSEvents 看不到远端改动。
    pub network_rescan_minutes: u64,
    /// 强制使用单线程顺序友好批量扫描的卷路径列表（当外置机械盘桥接芯片不上报 IOKit 旋转介质属性时手动覆盖）。
    pub serial_scan_paths: Vec<String>,
}

/// 兼容历史配置中仅以 `network_rescan_minutes = 0` 表示关闭的过渡反序列化视图：
/// 缺失 `network_rescan_enabled` 时默认关闭；当 `network_rescan_minutes == 0` 时统一规约为关闭并回填默认间隔，
/// 确保开关重新开启时不会出现 0 分钟无效周期。
#[derive(Deserialize)]
#[serde(default)]
struct IndexConfigWire {
    index_paths: Vec<String>,
    index_apps: bool,
    exclude: Vec<String>,
    exclude_paths: Vec<String>,
    include_hidden: bool,
    exclude_sensitive: bool,
    sensitive_names: Vec<String>,
    max_results: usize,
    network_rescan_enabled: Option<bool>,
    network_rescan_minutes: Option<u64>,
    serial_scan_paths: Vec<String>,
}

impl Default for IndexConfigWire {
    fn default() -> Self {
        let defaults = IndexConfig::default();
        Self {
            index_paths: defaults.index_paths,
            index_apps: defaults.index_apps,
            exclude: defaults.exclude,
            exclude_paths: defaults.exclude_paths,
            include_hidden: defaults.include_hidden,
            exclude_sensitive: defaults.exclude_sensitive,
            sensitive_names: defaults.sensitive_names,
            max_results: defaults.max_results,
            network_rescan_enabled: None,
            network_rescan_minutes: None,
            serial_scan_paths: defaults.serial_scan_paths,
        }
    }
}

impl From<IndexConfigWire> for IndexConfig {
    fn from(wire: IndexConfigWire) -> Self {
        let defaults = IndexConfig::default();
        let (network_rescan_enabled, network_rescan_minutes) = match wire.network_rescan_minutes {
            Some(0) => (false, defaults.network_rescan_minutes),
            Some(minutes) => (
                wire.network_rescan_enabled
                    .unwrap_or(defaults.network_rescan_enabled),
                minutes,
            ),
            None => (
                wire.network_rescan_enabled
                    .unwrap_or(defaults.network_rescan_enabled),
                defaults.network_rescan_minutes,
            ),
        };
        Self {
            index_paths: wire.index_paths,
            index_apps: wire.index_apps,
            exclude: wire.exclude,
            exclude_paths: wire.exclude_paths,
            include_hidden: wire.include_hidden,
            exclude_sensitive: wire.exclude_sensitive,
            sensitive_names: wire.sensitive_names,
            max_results: wire.max_results,
            network_rescan_enabled,
            network_rescan_minutes,
            serial_scan_paths: wire.serial_scan_paths,
        }
    }
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            index_paths: vec!["~".to_string()],
            index_apps: false,
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
            exclude_sensitive: true,
            sensitive_names: vec![
                ".ssh".to_string(),
                ".gnupg".to_string(),
                ".aws".to_string(),
                ".kube".to_string(),
                ".docker".to_string(),
                ".password-store".to_string(),
                ".netrc".to_string(),
                ".git-credentials".to_string(),
                ".npmrc".to_string(),
                ".pypirc".to_string(),
                ".authinfo".to_string(),
                ".msmtprc".to_string(),
            ],
            max_results: 500,
            network_rescan_enabled: false,
            network_rescan_minutes: 1440,
            serial_scan_paths: Vec::new(),
        }
    }
}

/// Window behavior configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// Expands leading `~` or `~/...` to the user's home directory.
/// If `dirs::home_dir()` returns None, falls back to `/`.
pub(crate) fn expand_tilde(raw: &str) -> PathBuf {
    if raw == "~" {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
    } else if let Some(rest) = raw.strip_prefix("~/") {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/"))
            .join(rest)
    } else {
        PathBuf::from(raw)
    }
}

impl AppConfig {
    /// Resolve index paths, expanding "~" to the user's home directory.
    /// Silently skips paths that don't exist on disk.
    pub fn resolve_paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = self
            .index
            .index_paths
            .iter()
            .filter_map(|p| {
                let resolved = expand_tilde(p);
                if resolved.exists() {
                    Some(resolved)
                } else {
                    None
                }
            })
            .collect();
        // 应用开关追加的未覆盖浅层根不写回用户配置，关闭后自然消失
        for path in self.resolve_app_roots() {
            if !paths
                .iter()
                .any(|existing| crate::path_utils::paths_equal_ci(existing, &path))
            {
                paths.push(path);
            }
        }
        paths
    }

    /// 纯词法计算由 `index_apps` 自动追加且未被用户显式 `index_paths` 覆盖的浅层应用根集合（零磁盘 `stat`）。
    ///
    /// 使用内部 `IndexScopePolicy::from_user_roots_lexical` 避免与 `from_config_lexical` 形成递归，
    /// 并复用与 `scope_diff` 相同的沿途 `IndexNormally` 覆盖判定：已被用户根完整覆盖的应用根直接不追加。
    pub fn resolve_shallow_app_roots(&self) -> Vec<PathBuf> {
        if !self.index.index_apps {
            return Vec::new();
        }
        let user_roots: Vec<PathBuf> = self
            .index
            .index_paths
            .iter()
            .map(|p| expand_tilde(p))
            .collect();
        let user_filter = crate::filter::IndexScopePolicy::from_user_roots_lexical(self);
        let mut out = Vec::new();
        for root in APP_INDEX_ROOTS {
            let app_root = PathBuf::from(root);
            let covered_by_user = user_roots.iter().any(|u| {
                crate::path_utils::paths_equal_ci(&app_root, u)
                    || crate::scope_diff::path_chain_admitted_normally(
                        u,
                        &app_root,
                        &user_filter,
                        self.index.include_hidden,
                        true,
                        &[],
                    )
            });
            if !covered_by_user {
                out.push(app_root);
            }
        }
        out
    }

    /// 纯词法展开索引根目录，仅执行 `~` 替换与去重，绝不触发任何磁盘 `stat` / `exists()` 系统调用。
    /// 供检索热路径（如空结果排除原因推断）调用，避免失联网络卷或休眠外置盘阻塞搜索线程。
    pub fn resolve_paths_lexical(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = Vec::with_capacity(self.index.index_paths.len());
        for p in &self.index.index_paths {
            let resolved = expand_tilde(p);
            if !paths
                .iter()
                .any(|existing| crate::path_utils::paths_equal_ci(existing, &resolved))
            {
                paths.push(resolved);
            }
        }
        for path in self.resolve_shallow_app_roots() {
            if !paths
                .iter()
                .any(|existing| crate::path_utils::paths_equal_ci(existing, &path))
            {
                paths.push(path);
            }
        }
        paths
    }

    pub(crate) fn resolve_app_roots(&self) -> impl Iterator<Item = PathBuf> {
        self.resolve_shallow_app_roots()
            .into_iter()
            .filter(|path| path.exists())
    }

    /// Resolve serial scan override paths, expanding "~" to the user's home directory.
    pub fn resolve_serial_scan_paths(&self) -> Vec<PathBuf> {
        self.index
            .serial_scan_paths
            .iter()
            .map(|p| expand_tilde(p))
            .collect()
    }

    /// Resolve exclude paths, expanding "~" to the user's home directory.
    pub fn resolve_exclude_paths(&self) -> Vec<PathBuf> {
        self.index
            .exclude_paths
            .iter()
            .map(|p| expand_tilde(p))
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
# 界面语言: "zh-Hans", "en"
language = "zh-Hans"

# 程序坞图标显示策略: "always"（始终）, "when_open"（仅窗口打开时）, "never"（从不）
dock_icon_mode = "when_open"

# 未输入搜索词时是否隐藏默认文件列表
hide_results_when_empty = true

[index]
# 索引目录列表（支持 ~ 表示用户主目录）
index_paths = ["~"]

# 是否自动索引应用程序目录（/Applications 与系统应用目录，仅收录应用本身；若目录已在 index_paths 中则按常规规则索引）
index_apps = false

# 排除的目录名（不会索引这些目录及其子目录）
exclude = ["node_modules", ".git", ".DS_Store", "target", ".Trash", ".hg", ".svn", "build", "dist", ".next", ".nuxt", ".cache", ".venv", "venv", "__pycache__", "DerivedData", "Pods", "Carthage", "Contents", "logs", "tmp", "temp", "coverage", ".idea", ".vscode", "vendor", "out", "bin", "obj", ".mypy_cache", ".pytest_cache", ".ruff_cache", ".tox", "xcshareddata", "xcuserdata"]

# 排除的路径前缀（支持 ~，前缀匹配）
exclude_paths = ["/System", "/Library", "/private", "/bin", "/sbin", "/usr", "/dev", "/Volumes", "/opt", "~/Library", "~/.Trash", "~/.cache", "~/.npm", "~/.pnpm-store", "~/.yarn", "~/.cargo/registry", "~/.cargo/git", "~/.rustup", "~/.gradle/caches", "~/.m2/repository", "~/.bun", "~/.deno", "~/.nvm", "~/.orbstack", "~/.docker"]

# 是否索引隐藏文件和目录（以 . 开头的）
include_hidden = false

# 索引隐藏文件时是否仍排除 .ssh、.gnupg、.aws 等凭据目录
exclude_sensitive = true

# 受保护的名字：exclude_sensitive 打开时，这些目录/文件即使索引隐藏文件也不收录
sensitive_names = [".ssh", ".gnupg", ".aws", ".kube", ".docker", ".password-store", ".netrc", ".git-credentials", ".npmrc", ".pypirc", ".authinfo", ".msmtprc"]

# 搜索结果最大返回数量
max_results = 500

# 是否按设定间隔自动重扫在线网络卷（网络共享无法实时感知远端改动；默认关闭以避免唤醒休眠中的 NAS 硬盘）
network_rescan_enabled = false

# 开启自动重扫后，在线网络卷的重扫间隔（分钟，默认 1440 即 24 小时）
network_rescan_minutes = 1440

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
/// On parse error, backs up the corrupted file to `config.toml.bak` before returning
/// `AppConfig::default()` so subsequent UI preference saves never wipe user edits silently.
pub fn load_or_create_config() -> (AppConfig, Option<String>) {
    load_or_create_config_in(&AppConfig::config_dir())
}

pub(crate) fn load_or_create_config_in(config_dir: &std::path::Path) -> (AppConfig, Option<String>) {
    let config_path = config_dir.join("config.toml");

    if !config_path.exists() {
        // First run: create default config (per D-03, CFG-06)
        let _ = write_config_atomic(&config_path, DEFAULT_CONFIG_TOML);
        return (AppConfig::default(), None);
    }

    match std::fs::read_to_string(&config_path) {
        Ok(content) => match AppConfig::from_toml(&content) {
            Ok(config) => (config, None),
            Err(e) => {
                // 解析失败时自动备份原始损坏文件为 config.toml.bak，防止后续设置保存覆盖导致用户配置永久丢失
                let backup_path = config_dir.join("config.toml.bak");
                let backup_note = match std::fs::copy(&config_path, &backup_path) {
                    Ok(_) => format!(" (backed up to {})", backup_path.display()),
                    Err(_) => String::new(),
                };
                let msg = format!(
                    "Config parse error in {}: {}{}",
                    config_path.display(),
                    e,
                    backup_note
                );
                (AppConfig::default(), Some(msg))
            }
        },
        Err(e) => {
            let msg = format!("Failed to read {}: {}", config_path.display(), e);
            (AppConfig::default(), Some(msg))
        }
    }
}

/// Resolves symlink targets before atomic replacement so writing through a
/// symlinked `config.toml` updates the real backing file in place instead of
/// replacing the symlink itself with a regular file.
fn resolve_atomic_write_target(config_path: &std::path::Path) -> std::io::Result<PathBuf> {
    let is_symlink = std::fs::symlink_metadata(config_path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false);
    if !is_symlink {
        return Ok(config_path.to_path_buf());
    }
    if let Ok(canonical) = std::fs::canonicalize(config_path) {
        return Ok(canonical);
    }
    let mut current = config_path.to_path_buf();
    for hop in 0..=32 {
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                if hop == 32 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "too many levels of symbolic links",
                    ));
                }
                let link_target = std::fs::read_link(&current)?;
                current = if link_target.is_absolute() {
                    link_target
                } else {
                    current
                        .parent()
                        .unwrap_or_else(|| std::path::Path::new("/"))
                        .join(link_target)
                };
            }
            _ => return Ok(current),
        }
    }
    Ok(current)
}

/// Serializes atomic config writes with `start_config_watcher` reloads so self-writes
/// publish to `ConfigStore` before the watcher observes the renamed file, while never
/// exposing uncommitted config changes to concurrent readers before `rename` succeeds.
static CONFIG_WRITE_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// Atomically persists serialized TOML configuration in the resolved target
/// directory (`temp file -> preserve permissions -> sync_data -> rename`) so
/// readers never observe a truncated file and symlinks/permissions stay intact.
pub fn write_config_atomic(config_path: &std::path::Path, toml_str: &str) -> std::io::Result<()> {
    write_config_atomic_then(config_path, toml_str, || {})
}

/// Atomically persists `toml_str` and runs `on_committed` under `CONFIG_WRITE_LOCK`
/// only after `rename` succeeds.
pub(crate) fn write_config_atomic_then<F>(
    config_path: &std::path::Path,
    toml_str: &str,
    on_committed: F,
) -> std::io::Result<()>
where
    F: FnOnce(),
{
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static CONFIG_TMP_SEQ: AtomicU64 = AtomicU64::new(0);

    let _write_guard = CONFIG_WRITE_LOCK.lock();
    let target_path = resolve_atomic_write_target(config_path)?;

    let existing_perms = std::fs::metadata(&target_path).ok().map(|m| m.permissions());
    if let Some(parent) = target_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let file_name = target_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("config.toml");
    let seq = CONFIG_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!("{file_name}.tmp.{}.{seq}", std::process::id());
    let tmp_path = target_path.with_file_name(tmp_name);

    let write_res = (|| -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        if let Some(ref perms) = existing_perms {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            // Create the staging file directly with the existing mode so restricted
            // configs (e.g. 0600) are never briefly world-readable under default umask.
            opts.mode(perms.mode() & 0o777);
        }
        let mut file = opts.open(&tmp_path)?;
        file.write_all(toml_str.as_bytes())?;
        file.flush()?;
        if let Some(perms) = existing_perms {
            file.set_permissions(perms)?;
        }
        file.sync_data()?;
        std::fs::rename(&tmp_path, &target_path)?;
        Ok(())
    })();
    if write_res.is_ok() {
        on_committed();
    } else {
        let _ = std::fs::remove_file(&tmp_path);
    }
    write_res
}

/// 配置文件 FSEvents 监听句柄；释放时通知后台线程退出并关闭事件流。
pub struct ConfigWatcherHandle {
    stop: Arc<AtomicBool>,
}

impl Drop for ConfigWatcherHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn event_matches_config(
    event: &crate::fsevents::Event,
    config_path: &Path,
    canonical_config_path: &Path,
    resolved_target: Option<&Path>,
) -> bool {
    const RESCAN_FLAGS: u32 = crate::fsevents::MUST_SCAN_SUBDIRS
        | crate::fsevents::USER_DROPPED
        | crate::fsevents::KERNEL_DROPPED;
    if event.flags & RESCAN_FLAGS != 0 {
        return true;
    }
    event.path == config_path
        || event.path == canonical_config_path
        || resolved_target.is_some_and(|target| event.path == target)
}

/// Start watching the config.toml file for changes via macOS FSEvents.
///
/// On file change, re-parses config and updates the ConfigStore.
/// Calls `on_reload` with the new config on success, `on_error` with message on failure.
/// Dropping the returned handle stops the FSEvents stream and background thread.
pub fn start_config_watcher<F>(
    config_store: ConfigStore,
    on_reload: F,
    on_error: impl Fn(String) + Send + 'static,
) -> std::io::Result<ConfigWatcherHandle>
where
    F: Fn(&AppConfig) + Send + 'static,
{
    let config_path = AppConfig::config_dir().join("config.toml");
    start_config_watcher_at(config_path, config_store, on_reload, on_error)
}

pub(crate) fn start_config_watcher_at<F>(
    config_path: PathBuf,
    config_store: ConfigStore,
    on_reload: F,
    on_error: impl Fn(String) + Send + 'static,
) -> std::io::Result<ConfigWatcherHandle>
where
    F: Fn(&AppConfig) + Send + 'static,
{
    let watch_dir = config_path
        .parent()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "config path has no parent directory"))?
        .to_path_buf();
    std::fs::create_dir_all(&watch_dir)?;

    let canonical_watch_dir =
        std::fs::canonicalize(&watch_dir).unwrap_or_else(|_| watch_dir.clone());
    let canonical_config_path = config_path
        .file_name()
        .map(|name| canonical_watch_dir.join(name))
        .unwrap_or_else(|| config_path.clone());

    let mut watch_roots = vec![canonical_watch_dir.clone()];
    let resolved_target = resolve_atomic_write_target(&config_path).ok().and_then(|target| {
        let canonical_target = target
            .parent()
            .and_then(|p| std::fs::canonicalize(p).ok())
            .and_then(|dir| target.file_name().map(|name| dir.join(name)))
            .unwrap_or(target);
        if let Some(target_dir) = canonical_target.parent() {
            let target_dir_buf = target_dir.to_path_buf();
            if target_dir_buf != canonical_watch_dir && target_dir_buf.is_dir() {
                watch_roots.push(target_dir_buf);
            }
        }
        (canonical_target != canonical_config_path).then_some(canonical_target)
    });

    let (tx, rx) = std::sync::mpsc::channel::<Vec<crate::fsevents::Event>>();
    let stream = crate::fsevents::watch(&watch_roots, crate::fsevents::SINCE_NOW, tx)
        .ok_or_else(|| std::io::Error::other("failed to start FSEvents config watcher"))?;

    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&stop);

    std::thread::spawn(move || {
        let _stream = stream; // keep FSEvents stream alive until stop
        while !stop_flag.load(Ordering::Relaxed) {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(batch) => {
                    if !batch.iter().any(|e| {
                        event_matches_config(
                            e,
                            &config_path,
                            &canonical_config_path,
                            resolved_target.as_deref(),
                        )
                    }) {
                        continue;
                    }
                    // Debounce: drain extra events within 200ms
                    while !stop_flag.load(Ordering::Relaxed)
                        && rx.recv_timeout(Duration::from_millis(200)).is_ok()
                    {}
                    if stop_flag.load(Ordering::Relaxed) {
                        break;
                    }

                    // Hold CONFIG_WRITE_LOCK only while reading and comparing against config_store
                    // so in-process atomic writes finish publishing before the watcher checks equality,
                    // then release the lock prior to invoking external callbacks.
                    let reload_outcome = {
                        let _write_guard = CONFIG_WRITE_LOCK.lock();
                        match std::fs::read_to_string(&config_path) {
                            Ok(content) => match AppConfig::from_toml(&content) {
                                Ok(new_config) => {
                                    // 若磁盘内容与内存 config_store 完全一致（典型于前端自身写盘触发的文件事件），
                                    // 直接跳过重复回调，消除无意义的卷协调器对账与 UI 重载。
                                    if *config_store.read() == new_config {
                                        Ok(None)
                                    } else {
                                        config_store.update(new_config.clone());
                                        Ok(Some(new_config))
                                    }
                                }
                                Err(e) => Err(format!(
                                    "Config parse error: {} (keeping previous config)",
                                    e
                                )),
                            },
                            Err(e) => Err(format!("Failed to read config: {}", e)),
                        }
                    };
                    match reload_outcome {
                        Ok(Some(new_config)) => on_reload(&new_config),
                        Ok(None) => {}
                        Err(msg) => on_error(msg),
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    Ok(ConfigWatcherHandle { stop })
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
language = "en"
show_in_dock = false

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
        assert_eq!(config.general.language, "en");
        assert_eq!(config.general.dock_icon_mode, DockIconMode::Never);
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
    fn test_general_config_defaults() {
        let config = GeneralConfig::default();
        assert_eq!(config.language, "zh-Hans");
        assert_eq!(config.dock_icon_mode, DockIconMode::WhenOpen);
        assert_eq!(config.terminal_bundle_id, "com.apple.Terminal");
    }

    #[test]
    fn test_general_config_backward_compatibility() {
        // 边界守卫与向后兼容：验证遗留 show_in_dock 布尔键能无损迁移至三态 dock_icon_mode，
        // 且缺失 terminal_bundle_id 时优雅回退至默认系统终端
        let json_legacy = r#"{"language":"en","show_in_dock":false}"#;
        let config_json: GeneralConfig = serde_json::from_str(json_legacy).expect("反序列化遗留 JSON 失败");
        assert_eq!(config_json.language, "en");
        assert_eq!(config_json.dock_icon_mode, DockIconMode::Never);
        assert_eq!(config_json.terminal_bundle_id, "com.apple.Terminal");

        let toml_legacy = "language = \"zh-Hans\"\nshow_in_dock = true\n";
        let config_toml: GeneralConfig = toml::from_str(toml_legacy).expect("反序列化遗留 TOML 失败");
        assert_eq!(config_toml.dock_icon_mode, DockIconMode::Always);
        assert_eq!(config_toml.terminal_bundle_id, "com.apple.Terminal");

        // 新旧字段并存时以显式 dock_icon_mode 为准
        let toml_dual = "dock_icon_mode = \"when_open\"\nshow_in_dock = false\n";
        let config_dual: GeneralConfig = toml::from_str(toml_dual).expect("反序列化双键 TOML 失败");
        assert_eq!(config_dual.dock_icon_mode, DockIconMode::WhenOpen);

        // 序列化仅写出规范字段 dock_icon_mode，不再输出遗留 show_in_dock
        let serialized_toml = toml::to_string(&config_dual).expect("序列化 TOML 失败");
        assert!(serialized_toml.contains("dock_icon_mode = \"when_open\""));
        assert!(!serialized_toml.contains("show_in_dock"));

        let serialized_json = serde_json::to_string(&config_dual).expect("序列化 JSON 失败");
        assert!(serialized_json.contains("\"dock_icon_mode\":\"when_open\""));
        assert!(!serialized_json.contains("show_in_dock"));
    }

    #[test]
    fn test_index_config_defaults() {
        let config = IndexConfig::default();
        assert_eq!(config.index_paths, vec!["~".to_string()]);
        assert!(!config.index_apps);
        assert!(config.exclude.contains(&"Contents".to_string()));
        assert!(config.exclude.contains(&"node_modules".to_string()));
        assert!(config.exclude.contains(&".git".to_string()));
        assert!(config.exclude.contains(&".DS_Store".to_string()));
        assert!(!config.exclude.contains(&".nox".to_string()));
        assert!(config.exclude.contains(&"target".to_string()));
        assert!(config.exclude.contains(&".Trash".to_string()));
        assert!(config.exclude_paths.contains(&"~/Library".to_string()));
        assert!(!config.include_hidden);
        assert!(config.exclude_sensitive);
        assert!(config.sensitive_names.contains(&".ssh".to_string()));
        assert_eq!(config.max_results, 500);
        assert!(!config.network_rescan_enabled);
        assert_eq!(config.network_rescan_minutes, 1440);

        let parsed_default = AppConfig::from_toml(DEFAULT_CONFIG_TOML).expect("DEFAULT_CONFIG_TOML valid");
        assert!(!parsed_default.index.include_hidden);
        assert!(!parsed_default.index.index_apps);
        assert!(parsed_default.index.exclude.contains(&".DS_Store".to_string()));
        assert!(parsed_default.index.sensitive_names.contains(&".ssh".to_string()));
        assert!(!parsed_default.index.exclude.contains(&".nox".to_string()));
        assert!(!parsed_default.index.network_rescan_enabled);
        assert_eq!(parsed_default.index.network_rescan_minutes, 1440);
    }

    #[test]
    fn test_network_rescan_config_backward_compatibility() {
        // 缺失新旧字段时回退默认：关闭且默认间隔 24 小时
        let empty: IndexConfig = toml::from_str("").expect("parse empty index");
        assert!(!empty.network_rescan_enabled);
        assert_eq!(empty.network_rescan_minutes, 1440);

        // 遗留配置以 0 表示关闭：映射为关闭并回填 1440，避免再次开启时出现 0 分钟无效间隔
        let legacy_off: IndexConfig =
            toml::from_str("network_rescan_minutes = 0\n").expect("parse legacy off");
        assert!(!legacy_off.network_rescan_enabled);
        assert_eq!(legacy_off.network_rescan_minutes, 1440);

        // 遗留配置无开关字段且含旧间隔：升级后保持默认关闭，同时保留历史间隔便于用户开启后沿用
        let legacy_minutes: IndexConfig =
            toml::from_str("network_rescan_minutes = 30\n").expect("parse legacy minutes");
        assert!(!legacy_minutes.network_rescan_enabled);
        assert_eq!(legacy_minutes.network_rescan_minutes, 30);

        // 仅打开开关而未写间隔时使用 24 小时默认间隔
        let enabled_only: IndexConfig =
            serde_json::from_str(r#"{"network_rescan_enabled":true}"#).expect("parse enabled only");
        assert!(enabled_only.network_rescan_enabled);
        assert_eq!(enabled_only.network_rescan_minutes, 1440);

        // 开关开启但手工写入 0 分钟：强制防呆关闭并回填默认间隔，杜绝零间隔死循环
        let zero_guard: IndexConfig = serde_json::from_str(
            r#"{"network_rescan_enabled":true,"network_rescan_minutes":0}"#,
        )
        .expect("parse zero guard");
        assert!(!zero_guard.network_rescan_enabled);
        assert_eq!(zero_guard.network_rescan_minutes, 1440);

        // 关闭开关时保留用户自定义间隔，TOML/JSON 往返无损
        let custom_off: IndexConfig = serde_json::from_str(
            r#"{"network_rescan_enabled":false,"network_rescan_minutes":360}"#,
        )
        .expect("parse custom off");
        assert!(!custom_off.network_rescan_enabled);
        assert_eq!(custom_off.network_rescan_minutes, 360);
        let toml_out = toml::to_string(&custom_off).expect("serialize toml");
        assert!(toml_out.contains("network_rescan_enabled = false"));
        assert!(toml_out.contains("network_rescan_minutes = 360"));
        let roundtrip: IndexConfig = toml::from_str(&toml_out).expect("roundtrip toml");
        assert_eq!(roundtrip, custom_off);
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
        assert!(DEFAULT_CONFIG_TOML.contains(r#"dock_icon_mode = "when_open""#));
        assert!(!DEFAULT_CONFIG_TOML.contains("show_in_dock"));
        assert!(DEFAULT_CONFIG_TOML.contains("Cmd+Shift+Space"));
        assert!(DEFAULT_CONFIG_TOML.contains(r#"index_paths = ["~"]"#));
        assert!(DEFAULT_CONFIG_TOML.contains("index_apps = false"));
        assert!(DEFAULT_CONFIG_TOML.contains("hide_on_blur = true"));
        assert!(DEFAULT_CONFIG_TOML.contains("remember_position = false"));
        assert!(DEFAULT_CONFIG_TOML.contains("max_results = 500"));
        assert!(DEFAULT_CONFIG_TOML.contains("network_rescan_enabled = false"));
        assert!(DEFAULT_CONFIG_TOML.contains("network_rescan_minutes = 1440"));
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
    fn test_resolve_paths_lexical_keeps_nonexistent_and_app_roots_without_stat() {
        let mut config = AppConfig::default();
        config.index.index_apps = true;
        config.index.index_paths = vec![
            "~".to_string(),
            "/Volumes/DefinitelyOfflineNAS_Test_NoStat/Share".to_string(),
            "/Volumes/DefinitelyOfflineNAS_Test_NoStat/Share".to_string(),
        ];

        let lexical = config.resolve_paths_lexical();
        let home = dirs::home_dir().expect("home dir should exist");
        assert!(lexical.contains(&home));
        let offline = PathBuf::from("/Volumes/DefinitelyOfflineNAS_Test_NoStat/Share");
        assert_eq!(
            lexical.iter().filter(|p| *p == &offline).count(),
            1,
            "lexical resolution must retain nonexistent paths and deduplicate them"
        );
        assert!(lexical.contains(&PathBuf::from("/Applications")));
        assert!(lexical.contains(&PathBuf::from("/System/Applications")));
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
        assert_eq!(restored.general.language, original.general.language);
        assert_eq!(restored.general.dock_icon_mode, original.general.dock_icon_mode);
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
        config.general.dock_icon_mode = DockIconMode::Never;
        config.index.max_results = 1000;
        config.index.include_hidden = false;
        config.window.hide_on_blur = false;
        let json = serde_json::to_string(&config).expect("serialize");
        let restored: AppConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.index.max_results, 1000);
        assert!(!restored.index.include_hidden);
        assert!(!restored.window.hide_on_blur);
        assert_eq!(restored.general.dock_icon_mode, DockIconMode::Never);
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

    #[test]
    fn test_load_or_create_config_in_backs_up_corrupted_toml() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        let corrupted = "invalid {{{{ toml content\n[index]\nmax_results = \"bad\"\n";
        fs::write(&config_path, corrupted).unwrap();

        let (config, error) = load_or_create_config_in(tmp.path());
        assert_eq!(config, AppConfig::default());
        let msg = error.expect("corrupted config must surface an error message");
        assert!(
            msg.contains("config.toml.bak"),
            "error message should mention backup path, got: {msg}"
        );
        let backup_path = tmp.path().join("config.toml.bak");
        assert!(backup_path.exists(), "corrupted config.toml must be backed up");
        assert_eq!(fs::read_to_string(&backup_path).unwrap(), corrupted);
    }

    #[test]
    fn hide_results_when_empty_defaults_true_and_roundtrips() {
        let default = AppConfig::default();
        assert!(default.general.hide_results_when_empty);
        assert!(DEFAULT_CONFIG_TOML.contains("hide_results_when_empty = true"));

        let legacy_toml = r#"
[general]
language = "zh-Hans"
show_in_dock = true
"#;
        let legacy = AppConfig::from_toml(legacy_toml).expect("legacy toml must parse");
        assert!(legacy.general.hide_results_when_empty);
        assert_eq!(legacy.general.dock_icon_mode, DockIconMode::Always);

        let disabled_toml = r#"
[general]
language = "en"
hide_results_when_empty = false
"#;
        let disabled = AppConfig::from_toml(disabled_toml).expect("disabled toml must parse");
        assert!(!disabled.general.hide_results_when_empty);

        let json = serde_json::to_string(&disabled).expect("serialize");
        let roundtrip: AppConfig = serde_json::from_str(&json).expect("deserialize");
        assert!(!roundtrip.general.hide_results_when_empty);
    }

    #[test]
    fn test_write_config_atomic_replaces_without_leftover_tmp() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        write_config_atomic(&path, "[index]\nmax_results = 250\n").unwrap();
        write_config_atomic(&path, "[index]\nmax_results = 350\n").unwrap();
        let parsed = AppConfig::from_toml(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed.index.max_results, 350);
        let leftovers: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");
    }

    #[cfg(unix)]
    #[test]
    fn write_config_atomic_preserves_symlink_target_and_permissions() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let tmp = TempDir::new().unwrap();
        let config_dir = tmp.path().join("config");
        let dotfiles_dir = tmp.path().join("dotfiles");
        fs::create_dir_all(&config_dir).unwrap();
        fs::create_dir_all(&dotfiles_dir).unwrap();

        let real_target = dotfiles_dir.join("lokii.toml");
        fs::write(&real_target, "[index]\nmax_results = 100\n").unwrap();
        fs::set_permissions(&real_target, fs::Permissions::from_mode(0o600)).unwrap();

        let symlink_path = config_dir.join("config.toml");
        symlink(&real_target, &symlink_path).unwrap();

        write_config_atomic(&symlink_path, "[index]\nmax_results = 750\n").unwrap();

        assert!(
            fs::symlink_metadata(&symlink_path)
                .unwrap()
                .file_type()
                .is_symlink(),
            "symlink at config.toml must remain a symlink"
        );
        let updated = AppConfig::from_toml(&fs::read_to_string(&real_target).unwrap()).unwrap();
        assert_eq!(updated.index.max_results, 750);
        let mode = fs::metadata(&real_target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "target permissions must be preserved");
        let dir_leftovers: Vec<_> = fs::read_dir(&config_dir)
            .unwrap()
            .chain(fs::read_dir(&dotfiles_dir).unwrap())
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(dir_leftovers.is_empty(), "leftover temp files: {dir_leftovers:?}");

        // Regular file custom permissions are also preserved across atomic replace.
        let regular_path = tmp.path().join("regular.toml");
        fs::write(&regular_path, "[index]\nmax_results = 100\n").unwrap();
        fs::set_permissions(&regular_path, fs::Permissions::from_mode(0o600)).unwrap();
        write_config_atomic(&regular_path, "[index]\nmax_results = 420\n").unwrap();
        assert_eq!(
            fs::metadata(&regular_path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        // Dangling symlink resolves to the intended target path while keeping the symlink intact.
        let dangling_target = dotfiles_dir.join("created_later.toml");
        let dangling_link = config_dir.join("dangling.toml");
        symlink(&dangling_target, &dangling_link).unwrap();
        write_config_atomic(&dangling_link, "[index]\nmax_results = 888\n").unwrap();
        assert!(
            fs::symlink_metadata(&dangling_link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            AppConfig::from_toml(&fs::read_to_string(&dangling_target).unwrap())
                .unwrap()
                .index
                .max_results,
            888
        );
    }

    #[test]
    fn write_config_atomic_then_invokes_commit_only_after_success() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");

        let mut committed = false;
        write_config_atomic_then(&path, "[index]\nmax_results = 300\n", || {
            assert!(path.exists(), "file must already be renamed when on_committed runs");
            committed = true;
        })
        .unwrap();
        assert!(committed);

        // Replace target with a directory so rename fails: on_committed must never execute.
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        let mut called_on_failure = false;
        let res = write_config_atomic_then(&path, "[index]\nmax_results = 999\n", || {
            called_on_failure = true;
        });
        assert!(res.is_err());
        assert!(
            !called_on_failure,
            "on_committed must not run when atomic rename fails"
        );
    }

    #[test]
    fn fsevents_config_watcher_reloads_external_edits_and_reports_parse_errors() {
        use std::sync::mpsc;

        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        let initial = AppConfig::default();
        fs::write(&config_path, toml::to_string_pretty(&initial).unwrap()).unwrap();

        let store = ConfigStore::new(initial);
        let (reload_tx, reload_rx) = mpsc::channel::<usize>();
        let (err_tx, err_rx) = mpsc::channel::<String>();

        let _handle = start_config_watcher_at(
            config_path.clone(),
            store.clone(),
            move |cfg| {
                let _ = reload_tx.send(cfg.index.max_results);
            },
            move |msg| {
                let _ = err_tx.send(msg);
            },
        )
        .expect("FSEvents config watcher should start");

        // 给 fseventsd 一点时间完成流注册
        std::thread::sleep(Duration::from_millis(250));

        // 1. 进程内原子写入同时同步 ConfigStore：不应触发冗余 on_reload
        let mut self_updated = store.read().clone();
        self_updated.index.max_results = 650;
        let toml_str = toml::to_string_pretty(&self_updated).unwrap();
        let store_for_commit = store.clone();
        let cfg_for_commit = self_updated.clone();
        write_config_atomic_then(&config_path, &toml_str, || {
            store_for_commit.update(cfg_for_commit);
        })
        .unwrap();
        assert!(
            reload_rx.recv_timeout(Duration::from_millis(600)).is_err(),
            "self-write matching ConfigStore should not emit duplicate reload"
        );

        // 2. 外部修改 config.toml：应触发 on_reload 并更新 ConfigStore
        fs::write(&config_path, "[index]\nmax_results = 777\n").unwrap();
        let reloaded = reload_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("external config edit should trigger FSEvents reload");
        assert_eq!(reloaded, 777);
        assert_eq!(store.read().index.max_results, 777);

        // 3. 写入非法 TOML：应触发 on_error 并保留旧配置
        fs::write(&config_path, "[index\nnot_valid_toml = ").unwrap();
        let err_msg = err_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("invalid TOML should trigger on_error");
        assert!(err_msg.contains("Config parse error"), "unexpected error: {err_msg}");
        assert_eq!(store.read().index.max_results, 777);
    }
}
