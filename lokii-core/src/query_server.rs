//! 外部查询通道：本地 Unix socket，一行 JSON 请求 → 一行 JSON 响应。
//! `lokii` CLI 及 Alfred / Raycast / 快捷指令等第三方脚本都经由此处查询 App 内存中的实时索引。

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::{AppConfig, ConfigStore};
use crate::indexer::{FileEntry, IndexStore};
use crate::search::{self, SearchMode};

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub q: String,
    pub mode: Option<SearchMode>,
    pub limit: Option<usize>,
    pub case_sensitivity: Option<search::CaseSensitivity>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub results: Vec<FileEntry>,
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_case_sensitive: Option<bool>,
}

pub fn socket_path() -> PathBuf {
    AppConfig::config_dir().join("query.sock")
}

/// 绑定 `~/.config/lokii/query.sock`（仅本用户可读写）并在后台线程提供查询服务。
pub fn start(store: IndexStore, config_store: ConfigStore) -> std::io::Result<()> {
    let path = socket_path();
    // 上次进程异常退出会留下陈旧的 socket 文件，不删除则 bind 失败
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    std::thread::spawn(move || serve(listener, store, config_store, MAX_CONNECTIONS));
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
fn serve(
    listener: UnixListener,
    store: IndexStore,
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
        let store = store.clone();
        let config_store = config_store.clone();
        let spawned = std::thread::Builder::new().spawn(move || {
            let _slot = slot;
            if let Err(e) = handle_connection(stream, &store, &config_store) {
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
            let (results, is_case_sensitive, error) =
                search::dispatch_with_options(store, &request.q, mode, case_sensitivity, limit);
            Response {
                results: results.into_iter().map(|r| r.entry).collect(),
                error,
                is_case_sensitive: Some(is_case_sensitive),
            }
        }
        Err(e) => Response {
            results: Vec::new(),
            error: Some(format!("invalid request: {e}")),
            is_case_sensitive: None,
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
}
