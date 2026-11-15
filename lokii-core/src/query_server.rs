//! 外部查询通道：本地 Unix socket，一行 JSON 请求 → 一行 JSON 响应。
//! `lokii` CLI 及 Alfred / Raycast / 快捷指令等第三方脚本都经由此处查询 App 内存中的实时索引。

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::{AppConfig, ConfigStore};
use crate::content_index::scope::ContentSourceKind;
use crate::content_index::verify::{ContentQueryOptions, ContentSearchHit};
use crate::content_index::ContentIndex;
use crate::indexer::{FileEntry, IndexStore};
use crate::search::{self, SearchMode};

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub q: String,
    pub mode: Option<SearchMode>,
    pub limit: Option<usize>,
    pub case_sensitivity: Option<search::CaseSensitivity>,
    #[serde(default)]
    pub force_content: bool,
    #[serde(default)]
    pub force_symbol: bool,
    #[serde(default)]
    pub force_regex: bool,
    #[serde(default)]
    pub budget_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueryContentRange {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueryContentLineMatch {
    pub line_number: u32,
    pub column: u32,
    pub line_text: String,
    pub match_ranges: Vec<QueryContentRange>,
    pub is_symbol_def: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueryContentHit {
    pub path: String,
    pub source_kind: String,
    pub score: i32,
    pub line_matches: Vec<QueryContentLineMatch>,
    pub total_matches_in_file: u32,
    pub size: u64,
    pub modified: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Response {
    pub results: Vec<FileEntry>,
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_case_sensitive: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content_hits: Vec<QueryContentHit>,
}

pub fn socket_path() -> PathBuf {
    AppConfig::config_dir().join("query.sock")
}

/// 绑定 `~/.config/lokii/query.sock`（仅本用户可读写）并在后台线程提供查询服务。
pub fn start(store: IndexStore, config_store: ConfigStore) -> std::io::Result<()> {
    start_with_content(store, config_store, None)
}

/// 绑定 `~/.config/lokii/query.sock`，同时承载文件名索引与可选的混合内容检索引擎。
pub fn start_with_content(
    store: IndexStore,
    config_store: ConfigStore,
    content_index: Option<Arc<ContentIndex>>,
) -> std::io::Result<()> {
    let path = socket_path();
    // 上次进程异常退出会留下陈旧的 socket 文件，不删除则 bind 失败
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    std::thread::spawn(move || {
        serve_with_content(listener, store, config_store, MAX_CONNECTIONS, content_index)
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

#[allow(dead_code)]
fn serve(
    listener: UnixListener,
    store: IndexStore,
    config_store: ConfigStore,
    max_connections: usize,
) {
    serve_with_content(listener, store, config_store, max_connections, None);
}

// 每连接一线程，避免一个慢客户端拖住其他查询；IndexStore/ConfigStore/ContentIndex 都是 Arc 包装，clone 很便宜。
// 超过并发上限的连接直接丢弃；accept/spawn 失败只记日志，不能让服务线程退出。
fn serve_with_content(
    listener: UnixListener,
    store: IndexStore,
    config_store: ConfigStore,
    max_connections: usize,
    content_index: Option<Arc<ContentIndex>>,
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
        let content_index = content_index.clone();
        let spawned = std::thread::Builder::new().spawn(move || {
            let _slot = slot;
            if let Err(e) = handle_connection(
                stream,
                &store,
                &config_store,
                content_index.as_ref(),
            ) {
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
    content_index: Option<&Arc<ContentIndex>>,
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
            if request.force_content || search::content_query::has_content_directive(&request.q) {
                if let Some(parsed) = search::content_query::parse_content_query(
                    &request.q,
                    case_sensitivity,
                    request.force_content,
                    request.force_regex,
                    request.force_symbol,
                ) {
                    match content_index {
                        Some(ci) => {
                            let opts = ContentQueryOptions {
                                max_results: limit.max(1),
                                max_matches_per_file: 3,
                                budget_ms: request.budget_ms.unwrap_or(250).clamp(20, 5000),
                                max_candidates: 25_000,
                                min_size_bytes: parsed.query_filter.size_filter.min_bytes,
                                max_size_bytes: parsed.query_filter.size_filter.max_bytes,
                            };
                            let cancel = AtomicBool::new(false);
                            match ci.search(
                                parsed.mode,
                                |path| parsed.path_filter.matches_path(path),
                                &opts,
                                &cancel,
                            ) {
                                Ok(resp) => Response {
                                    results: Vec::new(),
                                    error: None,
                                    is_case_sensitive: Some(parsed.is_case_sensitive),
                                    content_hits: resp
                                        .hits
                                        .into_iter()
                                        .map(convert_query_content_hit)
                                        .collect(),
                                },
                                Err(e) => Response {
                                    results: Vec::new(),
                                    error: Some(format!("content search failed: {e}")),
                                    is_case_sensitive: Some(parsed.is_case_sensitive),
                                    content_hits: Vec::new(),
                                },
                            }
                        }
                        None => Response {
                            results: Vec::new(),
                            error: Some("content index is not initialized".to_string()),
                            is_case_sensitive: Some(parsed.is_case_sensitive),
                            content_hits: Vec::new(),
                        },
                    }
                } else {
                    Response {
                        results: Vec::new(),
                        error: None,
                        is_case_sensitive: Some(false),
                        content_hits: Vec::new(),
                    }
                }
            } else {
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
                    content_hits: Vec::new(),
                }
            }
        }
        Err(e) => Response {
            results: Vec::new(),
            error: Some(format!("invalid request: {e}")),
            is_case_sensitive: None,
            content_hits: Vec::new(),
        },
    };
    let mut out = serde_json::to_vec(&response)?;
    out.push(b'\n');
    (&stream).write_all(&out)
}

fn convert_query_content_hit(h: ContentSearchHit) -> QueryContentHit {
    let source_kind = match h.source_kind {
        ContentSourceKind::PlainText => "plain_text",
        ContentSourceKind::OfficeXml => "office_xml",
        ContentSourceKind::Pdf => "pdf",
        ContentSourceKind::Image => "image_ocr",
    };
    QueryContentHit {
        path: h.path,
        source_kind: source_kind.to_string(),
        score: h.score,
        line_matches: h
            .line_matches
            .into_iter()
            .map(|m| QueryContentLineMatch {
                line_number: m.line_number,
                column: m.column,
                line_text: m.line_text,
                match_ranges: m
                    .match_ranges
                    .into_iter()
                    .map(|r| QueryContentRange {
                        start: r.start,
                        end: r.end,
                    })
                    .collect(),
                is_symbol_def: m.is_symbol_def,
            })
            .collect(),
        total_matches_in_file: h.total_matches_in_file,
        size: h.size_bytes,
        modified: h.modified_secs.max(0) as u64,
    }
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
    fn content_query_over_socket_returns_line_hits() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("content.sock");
        let idx_dir = dir.path().join("idx");
        let docs_dir = dir.path().join("docs");
        std::fs::create_dir_all(&docs_dir).unwrap();
        let rs_file = docs_dir.join("engine.rs");
        std::fs::write(&rs_file, "pub fn hybrid_match() {\n    let x = 42;\n}\n").unwrap();

        let ci = Arc::new(
            ContentIndex::open(
                &idx_dir,
                crate::content_index::scope::ScopeConfig::default(),
                4 * 1024 * 1024,
            )
            .unwrap(),
        );
        assert!(ci.upsert_file(&rs_file));

        let store = IndexStore::new();
        let config_store = ConfigStore::new(AppConfig::default());
        let listener = UnixListener::bind(&sock_path).unwrap();
        let ci_for_srv = Arc::clone(&ci);
        std::thread::spawn(move || {
            serve_with_content(listener, store, config_store, MAX_CONNECTIONS, Some(ci_for_srv));
        });

        let req = serde_json::json!({
            "q": "sym:hybrid_match ext:rs",
            "force_content": false,
            "limit": 10
        });
        let resp = query(&sock_path, &req.to_string());
        assert!(resp.error.is_none());
        assert!(resp.results.is_empty());
        assert_eq!(resp.content_hits.len(), 1);
        assert_eq!(resp.content_hits[0].path, rs_file.to_string_lossy());
        assert_eq!(resp.content_hits[0].line_matches[0].line_number, 1);
        assert!(resp.content_hits[0].line_matches[0].is_symbol_def);
    }
}
