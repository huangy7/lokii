//! 外部查询通道：本地 Unix socket，一行 JSON 请求 → 一行 JSON 响应。
//! `lokii` CLI 及 Alfred / Raycast / 快捷指令等第三方脚本都经由此处查询 App 内存中的实时索引。

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::{AppConfig, ConfigStore};
use crate::indexer::{FileEntry, IndexStore};
use crate::search::{self, SearchMode};
use crate::volume_manager::VolumeCoordinator;

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub q: String,
    pub mode: Option<SearchMode>,
    pub limit: Option<usize>,
    pub case_sensitivity: Option<search::CaseSensitivity>,
    #[serde(default)]
    pub include_offline: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub results: Vec<FileEntry>,
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_case_sensitive: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub offline_indices: Vec<usize>,
}

pub fn socket_path() -> PathBuf {
    AppConfig::config_dir().join("query.sock")
}

/// 绑定 `~/.config/lokii/query.sock`（仅本用户可读写）并在后台线程提供查询服务。
pub fn start(store: IndexStore, config_store: ConfigStore) -> std::io::Result<()> {
    start_with_coordinator(store, None, config_store)
}

/// Binds `path` as a 0600 Unix domain socket without hijacking an active instance.
pub(crate) fn bind_socket(path: &Path) -> std::io::Result<UnixListener> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    // On macOS/BSD, `bind(2)` follows a dangling symlink and creates the socket at the
    // link target; reject symlinks upfront so we never bind or chmod through a symlink.
    if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!("refusing to bind query socket through symlink {}", path.display()),
        ));
    }
    let listener = match UnixListener::bind(path) {
        Ok(listener) => listener,
        Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {
            match UnixStream::connect(path) {
                Ok(_) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AddrInUse,
                        format!(
                            "another Lokii instance is already listening on {}",
                            path.display()
                        ),
                    ));
                }
                Err(connect_err)
                    if matches!(
                        connect_err.kind(),
                        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                    ) => {}
                Err(connect_err) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AddrInUse,
                        format!(
                            "socket {} is already in use and could not be verified stale: {}",
                            path.display(),
                            connect_err
                        ),
                    ));
                }
            }
            match std::fs::symlink_metadata(path) {
                Ok(meta) if meta.file_type().is_socket() => {
                    // Previous process exited without unlinking the socket; remove stale socket only.
                    let _ = std::fs::remove_file(path);
                    UnixListener::bind(path)?
                }
                Err(meta_err) if meta_err.kind() == std::io::ErrorKind::NotFound => {
                    // The stale socket vanished between `bind` and `connect`; retry binding directly.
                    UnixListener::bind(path)?
                }
                _ => return Err(err),
            }
        }
        Err(err) => return Err(err),
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// 绑定 `~/.config/lokii/query.sock` 并在后台线程同时服务主本地索引与挂载卷协调器。
pub fn start_with_coordinator(
    store: IndexStore,
    coordinator: Option<Arc<VolumeCoordinator>>,
    config_store: ConfigStore,
) -> std::io::Result<()> {
    let path = socket_path();
    let listener = bind_socket(&path)?;
    std::thread::spawn(move || {
        serve_with_coordinator(listener, store, coordinator, config_store, MAX_CONNECTIONS)
    });
    Ok(())
}

const MAX_CONNECTIONS: usize = 32;

/// 连接线程退出（含 panic）时归还并发名额
struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

// 每连接一线程，避免一个慢客户端拖住其他查询；IndexStore/ConfigStore 都是 Arc 包装，clone 很便宜。
// 超过并发上限的连接直接丢弃；accept/spawn 失败只记日志，不能让服务线程退出。
pub fn serve(
    listener: UnixListener,
    store: IndexStore,
    config_store: ConfigStore,
    max_connections: usize,
) {
    serve_with_coordinator(listener, store, None, config_store, max_connections);
}

pub fn serve_with_coordinator(
    listener: UnixListener,
    primary: IndexStore,
    coordinator: Option<Arc<VolumeCoordinator>>,
    config_store: ConfigStore,
    max_connections: usize,
) {
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(e) => {
                eprintln!("[Lokii] query accept failed: {e}");
                // fd 耗尽时 accept 会立即反复失败，歇一下避免空转
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        if active.fetch_add(1, Ordering::SeqCst) >= max_connections {
            active.fetch_sub(1, Ordering::SeqCst);
            continue;
        }
        let slot = Slot(Arc::clone(&active));
        let store = primary.clone();
        let coordinator = coordinator.clone();
        let config_store = config_store.clone();
        let spawned = std::thread::Builder::new().spawn(move || {
            let _slot = slot;
            if let Err(e) =
                handle_connection(stream, &store, coordinator.as_deref(), &config_store)
            {
                eprintln!("[Lokii] query connection failed: {e}");
            }
        });
        if let Err(e) = spawned {
            eprintln!("[Lokii] query thread spawn failed: {e}");
        }
    }
}

fn handle_connection(
    stream: UnixStream,
    store: &IndexStore,
    coordinator: Option<&VolumeCoordinator>,
    config_store: &ConfigStore,
) -> std::io::Result<()> {
    // 防止客户端只连不发把服务线程卡死；写超时防止慢客户端卡住回写；读取上限防止无换行长流撑爆内存
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut line = String::new();
    BufReader::new(&stream).take(64 * 1024).read_line(&mut line)?;
    if line.trim().is_empty() {
        return Ok(());
    }
    let response = match serde_json::from_str::<Request>(&line) {
        Ok(request) => {
            // 调用方的 limit 不能突破配置上限，否则一个宽容的 limit 会让 app 内存随匹配数增长
            let max_results = config_store.read().index.max_results;
            let limit = request.limit.unwrap_or(max_results).min(max_results);
            let mode = request.mode.unwrap_or(SearchMode::Auto);
            let case_sensitivity = request.case_sensitivity.unwrap_or_default();
            let include_offline = request.include_offline.unwrap_or(true);
            match coordinator {
                Some(coord) => {
                    let (multi, is_case_sensitive, error, _) = coord.search(
                        store,
                        &request.q,
                        mode,
                        case_sensitivity,
                        limit,
                        include_offline,
                    );
                    let mut results = Vec::with_capacity(multi.len());
                    let mut offline_indices = Vec::new();
                    for (idx, item) in multi.into_iter().enumerate() {
                        if item.is_offline {
                            offline_indices.push(idx);
                        }
                        results.push(item.result.entry);
                    }
                    Response {
                        results,
                        error,
                        is_case_sensitive: Some(is_case_sensitive),
                        offline_indices,
                    }
                }
                None => {
                    let (results, is_case_sensitive, error) = search::dispatch_with_options(
                        store,
                        &request.q,
                        mode,
                        case_sensitivity,
                        limit,
                    );
                    Response {
                        results: results.into_iter().map(|r| r.entry).collect(),
                        error,
                        is_case_sensitive: Some(is_case_sensitive),
                        offline_indices: Vec::new(),
                    }
                }
            }
        }
        Err(e) => Response {
            results: Vec::new(),
            error: Some(format!("invalid request: {e}")),
            is_case_sensitive: None,
            offline_indices: Vec::new(),
        },
    };
    let mut out = serde_json::to_vec(&response)?;
    out.push(b'\n');
    (&stream).write_all(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, path: &str) -> FileEntry {
        FileEntry {
            name: name.to_string(),
            path: path.to_string(),
            is_dir: false,
            size: 1,
            modified: 1700000000,
            created_at: 0,
            extension: "rs".to_string(),
        }
    }

    fn query(path: &std::path::Path, request: &str) -> Response {
        let mut stream = UnixStream::connect(path).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        stream.write_all(b"\n").unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[test]
    fn serves_search_results_and_rejects_malformed_requests() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("query.sock");
        let store = IndexStore::new();
        store.replace(vec![
            entry("main.rs", "/src/main.rs"),
            entry("lib.rs", "/src/lib.rs"),
        ]);
        let listener = UnixListener::bind(&path).unwrap();
        let config_store = ConfigStore::new(AppConfig::default());
        std::thread::spawn(move || serve(listener, store, config_store, MAX_CONNECTIONS));

        let found = query(&path, r#"{"q":"main","mode":"substring"}"#);
        assert_eq!(found.error, None);
        assert_eq!(found.is_case_sensitive, Some(false));
        let paths: Vec<_> = found.results.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["/src/main.rs"]);

        // Auto + uppercase -> smart case sensitive
        let auto_upper = query(&path, r#"{"q":"MAIN","mode":"substring"}"#);
        assert_eq!(auto_upper.error, None);
        assert_eq!(auto_upper.is_case_sensitive, Some(true));
        assert!(auto_upper.results.is_empty());

        // Explicit insensitive
        let forced_insensitive = query(&path, r#"{"q":"MAIN","mode":"substring","case_sensitivity":"insensitive"}"#);
        assert_eq!(forced_insensitive.error, None);
        assert_eq!(forced_insensitive.is_case_sensitive, Some(false));
        assert_eq!(forced_insensitive.results.len(), 1);

        // Explicit sensitive
        let forced_sensitive = query(&path, r#"{"q":"main","mode":"substring","case_sensitivity":"sensitive"}"#);
        assert_eq!(forced_sensitive.error, None);
        assert_eq!(forced_sensitive.is_case_sensitive, Some(true));
        assert_eq!(forced_sensitive.results.len(), 1);

        let limited = query(&path, r#"{"q":".rs","limit":1}"#);
        assert_eq!(limited.results.len(), 1);

        let bad = query(&path, "not json");
        assert!(bad.results.is_empty());
        assert!(bad.error.unwrap().starts_with("invalid request"));

        // 空连接/断开探测应直接返回而不在控制台报错
        let mut probe = UnixStream::connect(&path).unwrap();
        probe.write_all(b"\n").unwrap();
        let mut empty_reply = String::new();
        assert_eq!(BufReader::new(&probe).read_line(&mut empty_reply).unwrap(), 0);
        assert!(empty_reply.is_empty());
    }

    #[test]
    fn drops_connections_beyond_the_limit_and_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("query.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let config_store = ConfigStore::new(AppConfig::default());
        std::thread::spawn(move || serve(listener, IndexStore::new(), config_store, 1));

        // 占住唯一名额：只连不发，服务线程会阻塞在读上
        let holder = UnixStream::connect(&path).unwrap();
        std::thread::sleep(Duration::from_millis(200));

        let mut rejected = UnixStream::connect(&path).unwrap();
        rejected.write_all(b"{\"q\":\"x\"}\n").unwrap();
        let mut reply = String::new();
        assert_eq!(BufReader::new(&rejected).read_line(&mut reply).unwrap(), 0);

        drop(holder);
        let recovered = (0..20).any(|_| {
            std::thread::sleep(Duration::from_millis(100));
            let mut stream = UnixStream::connect(&path).unwrap();
            stream.write_all(b"{\"q\":\"x\"}\n").unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap() > 0
        });
        assert!(
            recovered,
            "slot should be released after the holder disconnects"
        );
    }

    #[test]
    fn serves_coordinator_offline_indices_and_online_only_filter() {
        use crate::cache;
        use crate::compact_index::CompactIndex;
        use crate::volumes::{MountedFsInfo, VolumeIdentity, VolumeKind, VolumeManifest, VolumeRecord};

        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("query.sock");
        let cache_dir = dir.path().join("cache");
        let volumes_dir = cache_dir.join("volumes");

        let identity = VolumeIdentity::Network {
            fstype: "smbfs".to_string(),
            normalized_source: "//nas.local/docs".to_string(),
            subpath: String::new(),
        };
        let record = VolumeRecord {
            volume_id: identity.volume_id(),
            configured_path: "/Volumes/Docs".to_string(),
            active_configured_path: "/Volumes/Docs".to_string(),
            active_mount_root: Some("/Volumes/Docs".to_string()),
            subpath: String::new(),
            display_name: "Docs".to_string(),
            kind: VolumeKind::Network,
            fstype: "smbfs".to_string(),
            identity,
            last_sync_at: Some(1_700_000_000),
            last_event_id: None,
            journal_uuid: None,
            entry_count: 1,
            scope_snapshot: None,
            dirty_upper_bound: false,
            pending_confirm_reason: None,
            dirty_filter: false,
            filter_fingerprint: None,
        };
        let compact = CompactIndex::from_entries(vec![entry("report.rs", "/Volumes/Docs/report.rs")]).unwrap();
        cache::save_compact(&compact, None, &volumes_dir.join(format!("{}.cache", record.volume_id))).unwrap();
        let mut manifest = VolumeManifest::default();
        manifest.upsert(record);
        manifest.save_atomic(&volumes_dir).unwrap();

        let mut cfg = AppConfig::default();
        cfg.index.index_paths = vec!["/Users/me/src".to_string(), "/Volumes/Docs".to_string()];

        let coord = Arc::new(VolumeCoordinator::with_mount_lister(
            cache_dir,
            Arc::new(|| {
                vec![MountedFsInfo {
                    mount_point: "/".to_string(),
                    fstype: "apfs".to_string(),
                    from_name: "/dev/disk3s1s1".to_string(),
                    is_local: true,
                    uuid: Some("SYS".to_string()),
                    total_bytes: 1_000_000,
                    volume_name: "Macintosh HD".to_string(),
                }]
            }),
        ));
        coord.initialize(&cfg, Arc::new(|| {}));

        let primary = IndexStore::new();
        primary.replace(vec![entry("report_local.rs", "/Users/me/src/report_local.rs")]);

        let listener = UnixListener::bind(&sock_path).unwrap();
        let config_store = ConfigStore::new(cfg);
        let coord_clone = Arc::clone(&coord);
        std::thread::spawn(move || {
            serve_with_coordinator(
                listener,
                primary,
                Some(coord_clone),
                config_store,
                MAX_CONNECTIONS,
            )
        });

        let all = query(&sock_path, r#"{"q":"report","mode":"substring"}"#);
        assert_eq!(all.error, None);
        assert_eq!(all.results.len(), 2);
        let offline_pos = all
            .results
            .iter()
            .position(|e| e.path == "/Volumes/Docs/report.rs")
            .unwrap();
        assert_eq!(all.offline_indices, vec![offline_pos]);

        let online_only = query(
            &sock_path,
            r#"{"q":"report","mode":"substring","include_offline":false}"#,
        );
        assert_eq!(online_only.error, None);
        assert_eq!(online_only.results.len(), 1);
        assert_eq!(online_only.results[0].path, "/Users/me/src/report_local.rs");
        assert!(online_only.offline_indices.is_empty());
    }

    #[test]
    fn bind_socket_rejects_live_listener_and_cleans_stale_socket() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("query.sock");

        let listener = bind_socket(&sock_path).unwrap();
        let store = IndexStore::new();
        store.replace(vec![entry("live.rs", "/src/live.rs")]);
        let config_store = ConfigStore::new(AppConfig::default());
        std::thread::spawn(move || serve(listener, store, config_store, MAX_CONNECTIONS));

        // Second bind attempt while server is live must fail without unlinking query.sock.
        let second = bind_socket(&sock_path);
        assert!(second.is_err());
        assert_eq!(second.unwrap_err().kind(), std::io::ErrorKind::AddrInUse);

        let reply = query(&sock_path, r#"{"q":"live","mode":"substring"}"#);
        assert_eq!(reply.results.len(), 1);

        // Unreachable socket left behind by an exited process must be cleaned and rebound.
        let stale_path = dir.path().join("stale.sock");
        let stale_listener = UnixListener::bind(&stale_path).unwrap();
        drop(stale_listener);
        assert!(stale_path.exists());
        let rebound = bind_socket(&stale_path);
        assert!(rebound.is_ok());

        // Socket returning non-ConnectionRefused (e.g. PermissionDenied) must NOT be unlinked.
        let restricted_path = dir.path().join("restricted.sock");
        let _restricted_listener = UnixListener::bind(&restricted_path).unwrap();
        std::fs::set_permissions(&restricted_path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let restricted_err = bind_socket(&restricted_path);
        assert!(restricted_err.is_err());
        assert_eq!(
            restricted_err.unwrap_err().kind(),
            std::io::ErrorKind::AddrInUse
        );
        assert!(
            restricted_path.exists(),
            "socket whose liveness could not be proven stale must not be unlinked"
        );
        let _ = std::fs::set_permissions(&restricted_path, std::fs::Permissions::from_mode(0o600));

        // Dangling symlink must NOT be followed, unlinked, or overwritten.
        let dangling_target = dir.path().join("nonexistent.sock");
        let dangling_path = dir.path().join("dangling.sock");
        std::os::unix::fs::symlink(&dangling_target, &dangling_path).unwrap();
        let dangling_err = bind_socket(&dangling_path);
        assert!(dangling_err.is_err());
        assert_eq!(
            dangling_err.unwrap_err().kind(),
            std::io::ErrorKind::AddrInUse
        );
        assert!(
            std::fs::symlink_metadata(&dangling_path)
                .unwrap()
                .file_type()
                .is_symlink(),
            "dangling symlink must never be unlinked by bind_socket"
        );
        assert!(
            !dangling_target.exists(),
            "bind_socket must not follow a dangling symlink to create target"
        );

        // Regular file at socket path must NOT be unlinked.
        let regular_path = dir.path().join("regular.sock");
        std::fs::write(&regular_path, b"keep").unwrap();
        let regular_err = bind_socket(&regular_path);
        assert!(regular_err.is_err());
        assert_eq!(
            regular_err.unwrap_err().kind(),
            std::io::ErrorKind::AddrInUse
        );
        assert_eq!(std::fs::read(&regular_path).unwrap(), b"keep");
    }
}
