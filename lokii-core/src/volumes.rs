use crate::config::{expand_tilde, AppConfig};
use crate::path_utils::{path_starts_with_ci, paths_equal_ci};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VolumeKind {
    LocalExternal,
    Network,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VolumeIdentity {
    LocalUuid {
        uuid: String,
        subpath: String,
    },
    LocalFallback {
        fstype: String,
        source: String,
        total_bytes: u64,
        volume_name: String,
        subpath: String,
        /// 同一配置中存在多个同容量且剥离冲突后缀后同名的无 UUID 本地卷时置 true，
        /// 改用未剥离的精确卷名生成卷 ID 与匹配挂载点，避免双盘互相覆写槽位与缓存。
        #[serde(default)]
        exact_name_key: bool,
    },
    Network {
        fstype: String,
        normalized_source: String,
        subpath: String,
    },
    PendingPath {
        normalized_path: String,
    },
}

impl VolumeIdentity {
    pub fn volume_id(&self) -> String {
        match self {
            VolumeIdentity::LocalUuid { uuid, subpath } => {
                let key = format!("local_uuid:{}:{}", uuid.to_ascii_lowercase(), subpath.trim_matches('/'));
                fnv1a_dual(key.as_bytes())
            }
            VolumeIdentity::LocalFallback {
                fstype,
                source: _,
                total_bytes,
                volume_name,
                subpath,
                exact_name_key,
            } => {
                // 瞬态设备节点（/dev/diskXsY）与 macOS 挂载冲突序号（-1/-2）随拔插漂移；
                // 单盘使用剥离后缀后的基础卷名生成稳定 ID，并发配置同名双盘时通过 exact_name_key 保留精确卷名消歧。
                let key = if *exact_name_key {
                    format!(
                        "local_fallback_exact:{}:{}:{}:{}",
                        fstype.to_ascii_lowercase(),
                        total_bytes,
                        volume_name.to_ascii_lowercase(),
                        subpath.trim_matches('/')
                    )
                } else {
                    let base_name = strip_mount_drift_suffix(volume_name).to_ascii_lowercase();
                    format!(
                        "local_fallback:{}:{}:{}:{}",
                        fstype.to_ascii_lowercase(),
                        total_bytes,
                        base_name,
                        subpath.trim_matches('/')
                    )
                };
                fnv1a_dual(key.as_bytes())
            }
            VolumeIdentity::Network {
                fstype,
                normalized_source,
                subpath,
            } => {
                let key = format!(
                    "network:{}:{}:{}",
                    fstype.to_ascii_lowercase(),
                    normalized_source,
                    subpath.trim_matches('/')
                );
                fnv1a_dual(key.as_bytes())
            }
            VolumeIdentity::PendingPath { normalized_path } => {
                let key = format!("pending:{}", normalized_path.trim_end_matches('/'));
                fnv1a_dual(key.as_bytes())
            }
        }
    }
}

/// 剥离 macOS 挂载冲突自动追加的尾随正整数序号（例如 `USB-1` -> `USB`）。
///
/// 仅匹配 1..=99 且不含前导零的纯数字后缀，避免误伤年份卷名（如 `Photos-2025`）或版本号（如 `Disk-01`、`Disk-v2`）。
pub(crate) fn strip_mount_drift_suffix(name: &str) -> &str {
    let Some((base, suffix)) = name.rsplit_once('-') else {
        return name;
    };
    if base.is_empty()
        || suffix.is_empty()
        || suffix.len() > 2
        || suffix.starts_with('0')
        || !suffix.bytes().all(|b| b.is_ascii_digit())
    {
        return name;
    }
    match suffix.parse::<u8>() {
        Ok(1..=99) => base,
        _ => name,
    }
}

fn fnv1a_dual(bytes: &[u8]) -> String {
    const FNV_OFFSET_1: u64 = 0xcbf29ce484222325;
    const FNV_PRIME_1: u64 = 0x100000001b3;
    const FNV_OFFSET_2: u64 = 0x84222325cbf29ce4;
    const FNV_PRIME_2: u64 = 0x10000000233;

    let mut h1 = FNV_OFFSET_1;
    let mut h2 = FNV_OFFSET_2;

    for &b in bytes {
        h1 ^= b as u64;
        h1 = h1.wrapping_mul(FNV_PRIME_1);
        h2 ^= b as u64;
        h2 = h2.wrapping_mul(FNV_PRIME_2);
    }

    format!("{:016x}{:016x}", h1, h2)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeRecord {
    pub volume_id: String,
    pub configured_path: String,
    pub active_configured_path: String,
    pub active_mount_root: Option<String>,
    pub subpath: String,
    pub display_name: String,
    pub kind: VolumeKind,
    pub fstype: String,
    pub identity: VolumeIdentity,
    pub last_sync_at: Option<u64>,
    pub last_event_id: Option<u64>,
    pub journal_uuid: Option<String>,
    pub entry_count: u64,
    pub dirty_filter: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeManifest {
    pub records: Vec<VolumeRecord>,
}

impl VolumeManifest {
    pub fn load_from_dir(cache_dir: &Path) -> Self {
        let path = cache_dir.join("manifest.json");
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(manifest) = serde_json::from_str::<VolumeManifest>(&content) {
                return manifest;
            }
        }
        Self::default()
    }

    pub fn save_atomic(&self, cache_dir: &Path) -> Result<(), std::io::Error> {
        use std::io::Write;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::Mutex;

        // Serialize manifest writes across background workers and use unique temp paths
        // so concurrent saves never overwrite or unlink each other's staging file.
        static MANIFEST_SAVE_LOCK: Mutex<()> = Mutex::new(());
        static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

        let _guard = MANIFEST_SAVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::fs::create_dir_all(cache_dir)?;
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp_path = cache_dir.join(format!("manifest.json.tmp.{}.{seq}", std::process::id()));
        let final_path = cache_dir.join("manifest.json");
        let data = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        let write_result = (|| -> Result<(), std::io::Error> {
            let mut file = std::fs::File::create(&tmp_path)?;
            file.write_all(&data)?;
            file.sync_all()?;
            std::fs::rename(&tmp_path, &final_path)?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = std::fs::remove_file(&tmp_path);
        }
        write_result
    }

    pub fn find_by_identity(&self, id: &VolumeIdentity) -> Option<&VolumeRecord> {
        let target_vid = id.volume_id();
        self.records
            .iter()
            .find(|r| r.identity == *id || r.volume_id == target_vid)
    }

    pub fn find_by_path(&self, path: &str) -> Option<&VolumeRecord> {
        let target_path = Path::new(path);
        self.records.iter().find(|r| {
            paths_equal_ci(Path::new(&r.configured_path), target_path)
                || paths_equal_ci(Path::new(&r.active_configured_path), target_path)
        })
    }

    pub fn upsert(&mut self, record: VolumeRecord) {
        if let Some(pos) = self.records.iter().position(|r| {
            r.volume_id == record.volume_id
                || paths_equal_ci(Path::new(&r.configured_path), Path::new(&record.configured_path))
        }) {
            self.records[pos] = record;
        } else {
            self.records.push(record);
        }
    }

    pub fn remove_by_path_or_id(&mut self, path_or_id: &str) -> Option<VolumeRecord> {
        let target_path = Path::new(path_or_id);
        if let Some(pos) = self.records.iter().position(|r| {
            r.volume_id == path_or_id
                || paths_equal_ci(Path::new(&r.configured_path), target_path)
                || paths_equal_ci(Path::new(&r.active_configured_path), target_path)
        }) {
            Some(self.records.remove(pos))
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountedFsInfo {
    pub mount_point: String,
    pub fstype: String,
    pub from_name: String,
    pub is_local: bool,
    pub uuid: Option<String>,
    pub total_bytes: u64,
    pub volume_name: String,
}

#[cfg(target_os = "macos")]
fn get_volume_uuid(mount_point: &str) -> Option<String> {
    #[repr(C, packed(4))]
    struct VolUuidBuf {
        length: u32,
        uuid: [u8; 16],
    }

    let c_path = std::ffi::CString::new(mount_point).ok()?;
    let mut attr_list: libc::attrlist = unsafe { std::mem::zeroed() };
    attr_list.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    attr_list.volattr = 0x80000000 | 0x00040000; // ATTR_VOL_INFO | ATTR_VOL_UUID

    let mut buf: VolUuidBuf = unsafe { std::mem::zeroed() };
    let ret = unsafe {
        libc::getattrlist(
            c_path.as_ptr(),
            &mut attr_list as *mut _ as *mut libc::c_void,
            &mut buf as *mut _ as *mut libc::c_void,
            std::mem::size_of::<VolUuidBuf>(),
            0,
        )
    };

    if ret == 0 && buf.length >= 20 {
        let b = buf.uuid;
        // 拒绝全零 UUID（部分 FAT32/ExFAT/虚拟卷会返回全零），回退到 LocalFallback 避免不同磁盘碰撞
        if b == [0u8; 16] {
            return None;
        }
        Some(format!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            b[0], b[1], b[2], b[3],
            b[4], b[5],
            b[6], b[7],
            b[8], b[9],
            b[10], b[11], b[12], b[13], b[14], b[15]
        ))
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn list_mounted_volumes_impl(fetch_uuid: bool) -> Vec<MountedFsInfo> {
    let mut results = Vec::new();
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if count <= 0 {
        return results;
    }
    // 预留额外槽位，防止两次 getfsstat 之间发生并发挂载导致最新挂载点被截断
    let capacity = (count as usize).saturating_add(16);
    let mut buffer: Vec<libc::statfs> = Vec::with_capacity(capacity);
    let byte_size = capacity * std::mem::size_of::<libc::statfs>();
    let actual = unsafe {
        libc::getfsstat(
            buffer.as_mut_ptr(),
            byte_size as libc::c_int,
            libc::MNT_NOWAIT,
        )
    };
    if actual <= 0 {
        return results;
    }
    unsafe { buffer.set_len((actual as usize).min(capacity)) };

    for stat in buffer {
        let fstype = unsafe { std::ffi::CStr::from_ptr(stat.f_fstypename.as_ptr()) }
            .to_string_lossy()
            .to_string();
        // Skip pseudo/virtual filesystems
        if fstype == "devfs" || fstype == "autofs" || fstype == "nullfs" {
            continue;
        }

        let mount_point = unsafe { std::ffi::CStr::from_ptr(stat.f_mntonname.as_ptr()) }
            .to_string_lossy()
            .to_string();
        let from_name = unsafe { std::ffi::CStr::from_ptr(stat.f_mntfromname.as_ptr()) }
            .to_string_lossy()
            .to_string();

        let is_local = (stat.f_flags & (libc::MNT_LOCAL as u32)) != 0;
        let total_bytes = (stat.f_blocks as u64).saturating_mul(stat.f_bsize as u64);

        let volume_name = if mount_point == "/" {
            "Macintosh HD".to_string()
        } else if let Some(name) = Path::new(&mount_point).file_name() {
            name.to_string_lossy().to_string()
        } else {
            mount_point.clone()
        };

        // 系统根卷与 /System/Volumes/* 不参与外置卷分片匹配，跳过 getattrlist 减少后台轮询开销；
        // 轻量挂载点检查模式（fetch_uuid == false）彻底跳过 getattrlist，避免打开普通本地文件时唤醒休眠外置机械硬盘。
        let uuid = if fetch_uuid && is_local && !is_system_mount_point(&mount_point) {
            get_volume_uuid(&mount_point)
        } else {
            None
        };

        results.push(MountedFsInfo {
            mount_point,
            fstype,
            from_name,
            is_local,
            uuid,
            total_bytes,
            volume_name,
        });
    }
    results
}

#[cfg(target_os = "macos")]
pub fn list_mounted_volumes() -> Vec<MountedFsInfo> {
    list_mounted_volumes_impl(true)
}

/// 轻量非阻塞挂载表快照：仅读取内核 `getfsstat(MNT_NOWAIT)` 缓存表，不调用 `getattrlist` 查询卷 UUID，
/// 确保打开或定位任意本地文件时绝不触碰外置盘或网络卷。
#[cfg(target_os = "macos")]
pub fn list_mounted_volumes_fast() -> Vec<MountedFsInfo> {
    list_mounted_volumes_impl(false)
}

#[cfg(not(target_os = "macos"))]
pub fn list_mounted_volumes() -> Vec<MountedFsInfo> {
    Vec::new()
}

#[cfg(not(target_os = "macos"))]
pub fn list_mounted_volumes_fast() -> Vec<MountedFsInfo> {
    Vec::new()
}

pub fn is_system_mount_point(mount_point: &str) -> bool {
    let mp = mount_point.trim_end_matches('/');
    mp.is_empty()
        || mp == "/"
        || mp == "/System/Volumes"
        || mp.starts_with("/System/Volumes/")
        || mp.to_ascii_lowercase().starts_with("/system/volumes/")
}

pub fn is_system_local_mount(mount_point: &str, path: &str) -> bool {
    let p = path.trim_end_matches('/');

    // Any path inside /Volumes is treated as external/network, not system local
    if p == "/Volumes"
        || p.starts_with("/Volumes/")
        || p.eq_ignore_ascii_case("/volumes")
        || p.to_ascii_lowercase().starts_with("/volumes/")
    {
        return false;
    }

    is_system_mount_point(mount_point)
}

pub fn normalize_network_source(_fstype: &str, from_name: &str) -> String {
    let trimmed = from_name.trim();
    let without_scheme = if let Some(rest) = trimmed.strip_prefix("//") {
        rest
    } else if let Some(idx) = trimmed.find("://") {
        &trimmed[idx + 3..]
    } else {
        trimmed
    };

    let without_trailing = without_scheme.trim_end_matches('/');

    let (auth_and_host, path) = match without_trailing.find('/') {
        Some(idx) => (&without_trailing[..idx], &without_trailing[idx..]),
        None => (without_trailing, ""),
    };

    let host = match auth_and_host.rfind('@') {
        Some(idx) => &auth_and_host[idx + 1..],
        None => auth_and_host,
    };

    let host_lowered = host.to_ascii_lowercase();
    format!("//{host_lowered}{path}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathPartition {
    pub primary_paths: Vec<PathBuf>,
    pub mounted_targets: Vec<MountedPathTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountedPathTarget {
    pub configured_path: PathBuf,
    pub mounted: Option<ResolvedMountMatch>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMountMatch {
    pub mount_root: PathBuf,
    pub subpath: String,
    pub display_name: String,
    pub kind: VolumeKind,
    pub fstype: String,
    pub identity: VolumeIdentity,
}

fn deepest_mount<'a>(path: &Path, mounts: &'a [MountedFsInfo]) -> Option<&'a MountedFsInfo> {
    mounts
        .iter()
        .filter(|m| path_starts_with_ci(path, Path::new(&m.mount_point)))
        .max_by_key(|m| m.mount_point.len())
}

/// `/Volumes/<entry>` 可能只是启动卷的软链（macOS 上 `Macintosh HD -> /`），并非真实挂载点。
/// 入口不是挂载点、软链目标是绝对路径、且目标不在非系统挂载卷上时，返回改写后的路径。
/// shortcut: 只处理 `/Volumes` 这一种大小写规范的前缀，用户手输小写路径仍按卷目标处理。
fn resolve_volumes_entry_symlink(
    path: &Path,
    mounts: &[MountedFsInfo],
    volumes_root: &Path,
) -> Option<PathBuf> {
    let mut comps = path.strip_prefix(volumes_root).ok()?.components();
    let entry = match comps.next()? {
        Component::Normal(name) => name,
        _ => return None,
    };
    let entry_path = volumes_root.join(entry);
    // 真实挂载点不读软链，避免在挂起的网络卷上做元数据查询
    if mounts
        .iter()
        .any(|m| paths_equal_ci(Path::new(&m.mount_point), &entry_path))
    {
        return None;
    }
    let target = std::fs::read_link(&entry_path).ok()?;
    if !target.is_absolute() {
        return None;
    }
    let mut resolved = target;
    for comp in comps {
        resolved.push(comp.as_os_str());
    }
    // 软链指向真实挂载卷时保持原路径，仍交给卷分片
    if deepest_mount(&resolved, mounts)
        .is_some_and(|m| !is_system_mount_point(&m.mount_point))
    {
        return None;
    }
    Some(resolved)
}

pub fn partition_index_paths(
    config: &AppConfig,
    manifest: &VolumeManifest,
    mounts: &[MountedFsInfo],
) -> PathPartition {
    let mut primary_candidates: Vec<PathBuf> = Vec::new();
    let mut mounted_candidates: Vec<MountedPathTarget> = Vec::new();

    for raw_path in &config.index.index_paths {
        let configured = expand_tilde(raw_path);
        let expanded = resolve_volumes_entry_symlink(&configured, mounts, Path::new("/Volumes"))
            .unwrap_or(configured);
        let path_str = expanded.to_string_lossy().to_string();
        let best_mount = deepest_mount(&expanded, mounts);

        let is_in_volumes = (path_str.starts_with("/Volumes/") && path_str.len() > "/Volumes/".len())
            || (path_str.to_ascii_lowercase().starts_with("/volumes/")
                && path_str.len() > "/volumes/".len());
        let is_in_manifest = manifest.find_by_path(&path_str).is_some();

        // Only non-system mounts can be active online mounts for volume slots.
        let is_online_volume = best_mount
            .map(|m| !is_system_mount_point(&m.mount_point))
            .unwrap_or(false);

        if is_online_volume {
            let m = best_mount.unwrap();
            // Non-system mount match -> online mounted target
            let mount_root = PathBuf::from(&m.mount_point);
            let subpath = if let Ok(rel) = expanded.strip_prefix(&mount_root) {
                rel.to_string_lossy().trim_matches('/').to_string()
            } else {
                String::new()
            };

            let display_name = if subpath.is_empty() {
                m.volume_name.clone()
            } else if m.volume_name.is_empty() {
                subpath.clone()
            } else {
                format!("{}/{}", m.volume_name, subpath)
            };

            let kind = if m.is_local {
                VolumeKind::LocalExternal
            } else {
                VolumeKind::Network
            };

            let identity = match kind {
                VolumeKind::Network => VolumeIdentity::Network {
                    fstype: m.fstype.clone(),
                    normalized_source: normalize_network_source(&m.fstype, &m.from_name),
                    subpath: subpath.clone(),
                },
                VolumeKind::LocalExternal => {
                    if let Some(uuid) = &m.uuid {
                        VolumeIdentity::LocalUuid {
                            uuid: uuid.clone(),
                            subpath: subpath.clone(),
                        }
                    } else {
                        VolumeIdentity::LocalFallback {
                            fstype: m.fstype.clone(),
                            source: m.from_name.clone(),
                            total_bytes: m.total_bytes,
                            volume_name: m.volume_name.clone(),
                            subpath: subpath.clone(),
                            exact_name_key: false,
                        }
                    }
                }
            };

            mounted_candidates.push(MountedPathTarget {
                configured_path: expanded,
                mounted: Some(ResolvedMountMatch {
                    mount_root,
                    subpath,
                    display_name,
                    kind,
                    fstype: m.fstype.clone(),
                    identity,
                }),
            });
        } else if is_in_volumes || is_in_manifest {
            // Volume domain path, but currently offline (no non-system mount online)
            mounted_candidates.push(MountedPathTarget {
                configured_path: expanded,
                mounted: None,
            });
        } else if let Some(m) = best_mount {
            if is_system_local_mount(&m.mount_point, &path_str) {
                // System local root: keep if exists on disk
                if expanded.exists() {
                    primary_candidates.push(expanded);
                }
            }
        }
    }

    primary_candidates.extend(config.resolve_app_roots());

    // 子根仅在与上层根之间无排除/隐藏剪枝时才算冗余，否则显式子根会被排除墙吞掉（issue #61）。
    // 词法策略：分区路径不做磁盘 stat。
    let scope = crate::filter::CompiledFilter::from_config_lexical(config);
    let include_hidden = config.index.include_hidden;
    let final_primary =
        crate::scanner::deduplicate_scan_roots(&primary_candidates, &scope, include_hidden);

    // Deduplicate mounted_targets (handle strictly nested paths on same volume, but keep sibling subpaths!)
    let mut unique_mounted: Vec<MountedPathTarget> = Vec::new();
    for t in mounted_candidates {
        if !unique_mounted
            .iter()
            .any(|existing| paths_equal_ci(&existing.configured_path, &t.configured_path))
        {
            unique_mounted.push(t);
        }
    }
    let mounted_paths: Vec<PathBuf> =
        unique_mounted.iter().map(|t| t.configured_path.clone()).collect();
    let kept_mounted =
        crate::scanner::deduplicate_scan_roots(&mounted_paths, &scope, include_hidden);
    let mut final_mounted: Vec<MountedPathTarget> = unique_mounted
        .into_iter()
        .filter(|t| kept_mounted.contains(&t.configured_path))
        .collect();

    // 当同一配置中包含多个剥离后缀后同名的本地无 UUID 目标（如 Backup-1 与 Backup-2）时，
    // 升级为 exact_name_key 精确卷名标识，确保双盘拥有独立槽位与缓存文件且不随单盘离线翻转。
    let mut configured_base_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut fallback_vid_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for target in &final_mounted {
        if let Some(root_name) = target
            .mounted
            .as_ref()
            .and_then(|m| m.mount_root.file_name())
            .or_else(|| target.configured_path.file_name())
        {
            let base = strip_mount_drift_suffix(&root_name.to_string_lossy()).to_ascii_lowercase();
            *configured_base_counts.entry(base).or_insert(0) += 1;
        }
        if let Some(ref m) = target.mounted {
            if matches!(m.identity, VolumeIdentity::LocalFallback { .. }) {
                *fallback_vid_counts.entry(m.identity.volume_id()).or_insert(0) += 1;
            }
        }
    }
    for target in &mut final_mounted {
        let Some(ref mut m) = target.mounted else {
            continue;
        };
        if let VolumeIdentity::LocalFallback {
            total_bytes,
            ref volume_name,
            ref mut exact_name_key,
            ..
        } = m.identity
        {
            let vid = VolumeIdentity::LocalFallback {
                fstype: m.fstype.clone(),
                source: String::new(),
                total_bytes,
                volume_name: volume_name.clone(),
                subpath: m.subpath.clone(),
                exact_name_key: false,
            }
            .volume_id();
            let base = strip_mount_drift_suffix(volume_name).to_ascii_lowercase();
            if fallback_vid_counts.get(&vid).copied().unwrap_or(0) > 1
                || configured_base_counts.get(&base).copied().unwrap_or(0) > 1
            {
                *exact_name_key = true;
            }
        }
    }

    PathPartition {
        primary_paths: final_primary,
        mounted_targets: final_mounted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn system_apfs_firmlink_stays_primary_while_volumes_are_partitioned() {
        assert!(is_system_local_mount("/", "/Users/dev"));
        assert!(is_system_local_mount("/System/Volumes/Data", "/Users/dev/Documents"));
        assert!(!is_system_local_mount("/Volumes/WorkDisk", "/Volumes/WorkDisk/repo"));
    }

    #[test]
    fn network_source_normalization_strips_transient_credentials() {
        assert_eq!(
            normalize_network_source("smbfs", "//alice@nas._smb._tcp.local/Share"),
            normalize_network_source("smbfs", "//bob:secret@NAS._smb._tcp.local/Share/")
        );
        assert_ne!(
            normalize_network_source("smbfs", "//nas.local/ShareA"),
            normalize_network_source("smbfs", "//nas.local/ShareB")
        );
    }

    #[test]
    fn offline_volumes_path_preserved_in_partition_without_stat() {
        let mut cfg = AppConfig::default();
        cfg.index.index_paths = vec![
            "~".into(),
            "/Volumes/OfflineNAS/Projects".into(),
            "/Volumes/WorkDisk/repo".into(),
            "/nonexistent_local_only".into(),
        ];
        let manifest = VolumeManifest::default();
        let mounts = vec![
            MountedFsInfo {
                mount_point: "/".into(),
                fstype: "apfs".into(),
                from_name: "/dev/disk3s1s1".into(),
                is_local: true,
                uuid: Some("ROOT-UUID".into()),
                total_bytes: 1_000_000,
                volume_name: "Macintosh HD".into(),
            },
            MountedFsInfo {
                mount_point: "/System/Volumes/Data".into(),
                fstype: "apfs".into(),
                from_name: "/dev/disk3s5".into(),
                is_local: true,
                uuid: Some("SYS-UUID".into()),
                total_bytes: 1_000_000,
                volume_name: "Data".into(),
            },
            MountedFsInfo {
                mount_point: "/Volumes/WorkDisk".into(),
                fstype: "apfs".into(),
                from_name: "/dev/disk4s1".into(),
                is_local: true,
                uuid: Some("WORK-UUID".into()),
                total_bytes: 500_000,
                volume_name: "WorkDisk".into(),
            },
        ];
        let part = partition_index_paths(&cfg, &manifest, &mounts);
        // There should be 2 mounted targets: OfflineNAS (offline) and WorkDisk (online)
        assert_eq!(part.mounted_targets.len(), 2);

        let offline_target = part
            .mounted_targets
            .iter()
            .find(|t| t.configured_path == PathBuf::from("/Volumes/OfflineNAS/Projects"))
            .expect("OfflineNAS target must be preserved");
        assert!(offline_target.mounted.is_none());

        let online_target = part
            .mounted_targets
            .iter()
            .find(|t| t.configured_path == PathBuf::from("/Volumes/WorkDisk/repo"))
            .expect("WorkDisk target must be found");
        let mount_match = online_target
            .mounted
            .as_ref()
            .expect("WorkDisk must be resolved as online mount");
        assert_eq!(mount_match.mount_root, PathBuf::from("/Volumes/WorkDisk"));
        assert_eq!(mount_match.subpath, "repo");
        assert_eq!(mount_match.kind, VolumeKind::LocalExternal);
    }

    #[test]
    fn volume_identity_deterministic_hash_and_subpath_isolation() {
        let id_work = VolumeIdentity::Network {
            fstype: "smbfs".into(),
            normalized_source: "//nas.local/Share".into(),
            subpath: "Work".into(),
        };
        let id_media = VolumeIdentity::Network {
            fstype: "smbfs".into(),
            normalized_source: "//nas.local/Share".into(),
            subpath: "Media".into(),
        };
        let vid_work = id_work.volume_id();
        let vid_media = id_media.volume_id();
        assert_eq!(vid_work.len(), 32);
        assert_eq!(vid_media.len(), 32);
        assert_ne!(vid_work, vid_media);
        // Deterministic on same input
        assert_eq!(vid_work, id_work.volume_id());
    }

    #[test]
    fn volume_manifest_atomic_save_load_and_crud() {
        let tmp = TempDir::new().unwrap();
        let mut manifest = VolumeManifest::default();
        let record = VolumeRecord {
            volume_id: "test1234567890abcdef1234567890ab".into(),
            configured_path: "/Volumes/Share/Work".into(),
            active_configured_path: "/Volumes/Share-1/Work".into(),
            active_mount_root: Some("/Volumes/Share-1".into()),
            subpath: "Work".into(),
            display_name: "Share/Work".into(),
            kind: VolumeKind::Network,
            fstype: "smbfs".into(),
            identity: VolumeIdentity::Network {
                fstype: "smbfs".into(),
                normalized_source: "//nas.local/Share".into(),
                subpath: "Work".into(),
            },
            last_sync_at: Some(100),
            last_event_id: None,
            journal_uuid: None,
            entry_count: 42,
            dirty_filter: false,
        };

        manifest.upsert(record.clone());
        assert_eq!(manifest.records.len(), 1);
        assert!(manifest.find_by_path("/Volumes/Share/Work").is_some());
        assert!(manifest.find_by_path("/Volumes/Share-1/Work").is_some());
        assert!(manifest.find_by_identity(&record.identity).is_some());

        // Test save atomic
        manifest.save_atomic(tmp.path()).unwrap();
        let loaded = VolumeManifest::load_from_dir(tmp.path());
        assert_eq!(loaded.records.len(), 1);
        assert_eq!(loaded.records[0].volume_id, record.volume_id);

        // Test remove
        manifest.remove_by_path_or_id(&record.volume_id);
        assert_eq!(manifest.records.len(), 0);
    }

    #[test]
    fn partition_index_paths_nested_subpath_handling() {
        let mut cfg = AppConfig::default();
        // Sibling subpaths on same volume + nested child path on same volume
        cfg.index.index_paths = vec![
            "/Volumes/NAS/Work".into(),
            "/Volumes/NAS/Media".into(),
            "/Volumes/NAS/Work/Deep".into(),
        ];
        let manifest = VolumeManifest::default();
        let mounts = vec![MountedFsInfo {
            mount_point: "/Volumes/NAS".into(),
            fstype: "smbfs".into(),
            from_name: "//nas/NAS".into(),
            is_local: false,
            uuid: None,
            total_bytes: 1_000_000,
            volume_name: "NAS".into(),
        }];

        let part = partition_index_paths(&cfg, &manifest, &mounts);
        // Work and Media must remain distinct slots; Deep must be dropped as strictly nested child of Work
        assert_eq!(part.mounted_targets.len(), 2);
        let paths: Vec<PathBuf> = part
            .mounted_targets
            .into_iter()
            .map(|t| t.configured_path)
            .collect();
        assert!(paths.contains(&PathBuf::from("/Volumes/NAS/Work")));
        assert!(paths.contains(&PathBuf::from("/Volumes/NAS/Media")));
        assert!(!paths.contains(&PathBuf::from("/Volumes/NAS/Work/Deep")));
    }

    /// Issue #61：默认排除 `~/Library` 时，显式索引的 `~/Library/xxx` 必须穿透排除墙。
    #[test]
    fn partition_keeps_primary_child_root_behind_excluded_ancestor() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_path_buf();
        let library = home.join("Library");
        let sub = library.join("xxx");
        let plain = home.join("Docs").join("Deep");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(sub.join("note.txt"), b"x").unwrap();

        let mut cfg = AppConfig::default();
        cfg.index.index_paths = vec![
            home.to_string_lossy().into_owned(),
            sub.to_string_lossy().into_owned(),
            plain.to_string_lossy().into_owned(),
        ];
        cfg.index.exclude_paths = vec![library.to_string_lossy().into_owned()];
        let mounts = vec![MountedFsInfo {
            mount_point: "/".into(),
            fstype: "apfs".into(),
            from_name: "/dev/disk3s1s1".into(),
            is_local: true,
            uuid: Some("ROOT-UUID".into()),
            total_bytes: 1_000_000,
            volume_name: "Macintosh HD".into(),
        }];

        let part = partition_index_paths(&cfg, &VolumeManifest::default(), &mounts);
        assert!(part.primary_paths.contains(&home));
        assert!(part.primary_paths.contains(&sub), "{:?}", part.primary_paths);
        // 沿途无排除墙的嵌套子根仍按冗余去重
        assert!(!part.primary_paths.contains(&plain));

        let filter = crate::filter::CompiledFilter::from_primary_config(&cfg, &part.primary_paths, &[]);
        let entries = crate::scanner::scan_directories(
            &part.primary_paths,
            std::sync::Arc::new(filter),
            false,
            std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        );
        assert!(entries.iter().any(|e| e.name == "note.txt"));
    }

    #[test]
    fn partition_keeps_mounted_child_root_behind_excluded_ancestor() {
        let mut cfg = AppConfig::default();
        cfg.index.index_paths = vec!["/Volumes/NAS".into(), "/Volumes/NAS/Private/Keep".into()];
        cfg.index.exclude_paths = vec!["/Volumes/NAS/Private".into()];
        let mounts = vec![MountedFsInfo {
            mount_point: "/Volumes/NAS".into(),
            fstype: "smbfs".into(),
            from_name: "//nas/NAS".into(),
            is_local: false,
            uuid: None,
            total_bytes: 1_000_000,
            volume_name: "NAS".into(),
        }];

        let part = partition_index_paths(&cfg, &VolumeManifest::default(), &mounts);
        let paths: Vec<PathBuf> = part.mounted_targets.into_iter().map(|t| t.configured_path).collect();
        assert!(paths.contains(&PathBuf::from("/Volumes/NAS")));
        assert!(paths.contains(&PathBuf::from("/Volumes/NAS/Private/Keep")));
    }

    #[test]
    fn volumes_entry_symlink_to_system_root_redirects_to_primary_path() {
        let volumes_root = TempDir::new().unwrap();
        let root = volumes_root.path();
        let boot_link = root.join("Macintosh HD");
        std::os::unix::fs::symlink("/", &boot_link).unwrap();
        let system_mount = MountedFsInfo {
            mount_point: "/".into(),
            fstype: "apfs".into(),
            from_name: "/dev/disk3s1s1".into(),
            is_local: true,
            uuid: Some("ROOT-UUID".into()),
            total_bytes: 1_000_000,
            volume_name: "Macintosh HD".into(),
        };
        let mounts = vec![system_mount.clone()];

        assert_eq!(
            resolve_volumes_entry_symlink(&root.join("Macintosh HD/Users/dev"), &mounts, root),
            Some(PathBuf::from("/Users/dev"))
        );
        assert_eq!(
            resolve_volumes_entry_symlink(&boot_link, &mounts, root),
            Some(PathBuf::from("/"))
        );

        // 真实挂载点不读软链
        let real_mount = root.join("NAS");
        std::fs::create_dir(&real_mount).unwrap();
        let mounts_with_nas = vec![
            system_mount,
            MountedFsInfo {
                mount_point: real_mount.to_string_lossy().into_owned(),
                ..mounts[0].clone()
            },
        ];
        assert_eq!(
            resolve_volumes_entry_symlink(&real_mount.join("docs"), &mounts_with_nas, root),
            None
        );

        // 软链指向真实挂载卷时保持原路径，继续交给卷分片
        let link_to_mount = root.join("LinkToMount");
        std::os::unix::fs::symlink(&real_mount, &link_to_mount).unwrap();
        assert_eq!(
            resolve_volumes_entry_symlink(&link_to_mount.join("docs"), &mounts_with_nas, root),
            None
        );
    }

    #[test]
    fn test_list_mounted_volumes_fast_skips_uuid_lookup() {
        let fast = list_mounted_volumes_fast();
        assert!(
            fast.iter().all(|m| m.uuid.is_none()),
            "fast mount listing must not invoke getattrlist UUID lookup"
        );
        #[cfg(target_os = "macos")]
        assert!(
            fast.iter().any(|m| m.mount_point == "/"),
            "fast mount listing on macOS must include root mount"
        );
    }

    #[test]
    fn strip_mount_drift_suffix_handles_macos_collision_numbers_only() {
        assert_eq!(strip_mount_drift_suffix("USB-1"), "USB");
        assert_eq!(strip_mount_drift_suffix("NO NAME-2"), "NO NAME");
        assert_eq!(strip_mount_drift_suffix("Backup-99"), "Backup");
        assert_eq!(strip_mount_drift_suffix("Photos-2025"), "Photos-2025");
        assert_eq!(strip_mount_drift_suffix("Disk-01"), "Disk-01");
        assert_eq!(strip_mount_drift_suffix("Disk-0"), "Disk-0");
        assert_eq!(strip_mount_drift_suffix("Disk-100"), "Disk-100");
        assert_eq!(strip_mount_drift_suffix("Disk-v2"), "Disk-v2");
        assert_eq!(strip_mount_drift_suffix("-1"), "-1");
    }

    #[test]
    fn local_fallback_volume_id_stable_across_device_node_and_drift_suffix() {
        let original = VolumeIdentity::LocalFallback {
            fstype: "exfat".into(),
            source: "/dev/disk4s1".into(),
            total_bytes: 64_000_000_000,
            volume_name: "MyUSB".into(),
            subpath: "Docs".into(),
            exact_name_key: false,
        };
        let replugged_drifted = VolumeIdentity::LocalFallback {
            fstype: "ExFAT".into(),
            source: "/dev/disk6s1".into(),
            total_bytes: 64_000_000_000,
            volume_name: "MyUSB-1".into(),
            subpath: "Docs".into(),
            exact_name_key: false,
        };
        assert_eq!(original.volume_id(), replugged_drifted.volume_id());
    }
}
