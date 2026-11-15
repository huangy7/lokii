//! `lokii` 命令行二进制的端到端行为；用临时 HOME 下的假 socket 代替真实 App。

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixListener;
use std::process::{Command, Output};

use lokii_core::indexer::FileEntry;
use lokii_core::query_server::Response;

fn lokii(home: &std::path::Path, args: &[&OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lokii"))
        .env("HOME", home)
        .args(args)
        .output()
        .unwrap()
}

fn fake_app(home: &std::path::Path) -> UnixListener {
    let dir = home.join(".config/lokii");
    std::fs::create_dir_all(&dir).unwrap();
    UnixListener::bind(dir.join("query.sock")).unwrap()
}

#[test]
fn rejects_unknown_options_and_missing_query() {
    let home = tempfile::tempdir().unwrap();
    for args in [&["search", "--limt", "5", "foo"][..], &["search"], &["search", "--json=true", "foo"]] {
        let args: Vec<_> = args.iter().map(OsStr::new).collect();
        assert_eq!(lokii(home.path(), &args).status.code(), Some(64), "{args:?}");
    }
}

#[test]
fn non_utf8_argument_does_not_panic() {
    let home = tempfile::tempdir().unwrap();
    let bad = OsStr::from_bytes(b"\xff\xfe");
    let out = lokii(home.path(), &[OsStr::new("search"), OsStr::new("--"), bad]);
    assert_eq!(out.status.code(), Some(2)); // 没有 App 在监听 → 连不上，而不是 101 panic
}

#[test]
fn double_dash_allows_queries_starting_with_dash_and_alfred_gets_match() {
    let home = tempfile::tempdir().unwrap();
    let listener = fake_app(home.path());
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(&stream).read_line(&mut request).unwrap();
        let entry = FileEntry {
            name: "query_server.rs".into(),
            path: "/src/query_server.rs".into(),
            is_dir: false,
            size: 1,
            modified: 0,
            created_at: 0,
            extension: "rs".into(),
        };
        let response = Response {
            results: vec![entry],
            error: None,
            is_case_sensitive: None,
            content_hits: Vec::new(),
        };
        (&stream).write_all(format!("{}\n", serde_json::to_string(&response).unwrap()).as_bytes()).unwrap();
        request
    });

    let out = lokii(home.path(), &["search", "--alfred", "--", "-src/query"].map(OsStr::new));
    assert_eq!(out.status.code(), Some(0));
    assert!(server.join().unwrap().contains(r#""q":"-src/query""#));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["items"][0]["match"], "-src/query");
}

#[test]
fn times_out_when_app_never_replies() {
    let home = tempfile::tempdir().unwrap();
    let listener = fake_app(home.path());
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(10));
        drop(stream);
    });
    let started = std::time::Instant::now();
    let out = lokii(home.path(), &["search", "foo"].map(OsStr::new));
    assert_eq!(out.status.code(), Some(2));
    assert!(started.elapsed() < std::time::Duration::from_secs(9));
}

#[test]
fn cli_content_flag_formats_lines_and_json_hits() {
    use lokii_core::query_server::{QueryContentHit, QueryContentLineMatch, QueryContentRange};

    let home = tempfile::tempdir().unwrap();
    let listener = fake_app(home.path());
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut req_line = String::new();
        BufReader::new(&stream).read_line(&mut req_line).unwrap();
        let hit = QueryContentHit {
            path: "/tmp/demo/lib.rs".into(),
            source_kind: "plain_text".into(),
            score: 120,
            line_matches: vec![QueryContentLineMatch {
                line_number: 12,
                column: 8,
                line_text: "pub fn run_search() {}".into(),
                match_ranges: vec![QueryContentRange { start: 7, end: 17 }],
                is_symbol_def: true,
            }],
            total_matches_in_file: 1,
            size: 42,
            modified: 1700000000,
        };
        let response = Response {
            results: Vec::new(),
            error: None,
            is_case_sensitive: Some(false),
            content_hits: vec![hit],
        };
        (&stream)
            .write_all(format!("{}\n", serde_json::to_string(&response).unwrap()).as_bytes())
            .unwrap();
        req_line
    });

    let out = lokii(home.path(), &["search", "-c", "run_search"].map(OsStr::new));
    assert_eq!(out.status.code(), Some(0));
    let req_json: serde_json::Value = serde_json::from_str(&server.join().unwrap()).unwrap();
    assert_eq!(req_json["force_content"], true);
    assert_eq!(req_json["q"], "run_search");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("/tmp/demo/lib.rs:12:8: pub fn run_search() {}"));

    let home2 = tempfile::tempdir().unwrap();
    let listener2 = fake_app(home2.path());
    let server2 = std::thread::spawn(move || {
        let (stream, _) = listener2.accept().unwrap();
        let mut req_line = String::new();
        BufReader::new(&stream).read_line(&mut req_line).unwrap();
        let response = Response {
            results: Vec::new(),
            error: None,
            is_case_sensitive: Some(true),
            content_hits: vec![QueryContentHit {
                path: "/tmp/demo/lib.rs".into(),
                source_kind: "plain_text".into(),
                score: 90,
                line_matches: vec![QueryContentLineMatch {
                    line_number: 3,
                    column: 1,
                    line_text: "fn needle()".into(),
                    match_ranges: vec![],
                    is_symbol_def: true,
                }],
                total_matches_in_file: 1,
                size: 12,
                modified: 1700000000,
            }],
        };
        (&stream)
            .write_all(format!("{}\n", serde_json::to_string(&response).unwrap()).as_bytes())
            .unwrap();
        req_line
    });
    let out2 = lokii(home2.path(), &["search", "--sym", "--json", "needle"].map(OsStr::new));
    assert_eq!(out2.status.code(), Some(0));
    let req2: serde_json::Value = serde_json::from_str(&server2.join().unwrap()).unwrap();
    assert_eq!(req2["force_symbol"], true);
    assert_eq!(req2["force_content"], true);
    let hits: serde_json::Value = serde_json::from_slice(&out2.stdout).unwrap();
    assert_eq!(hits[0]["line_matches"][0]["line_number"], 3);
}
