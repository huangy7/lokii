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
            offline_indices: vec![],
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
        std::thread::sleep(std::time::Duration::from_secs(8));
        drop(stream);
    });
    let started = std::time::Instant::now();
    let out = lokii(home.path(), &["search", "foo"].map(OsStr::new));
    assert_eq!(out.status.code(), Some(2));
    assert!(started.elapsed() < std::time::Duration::from_secs(7));
}

#[test]
fn online_only_flag_sends_include_offline_false() {
    let home = tempfile::tempdir().unwrap();
    let listener = fake_app(home.path());
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(&stream).read_line(&mut request).unwrap();
        let response = Response {
            results: vec![],
            error: None,
            is_case_sensitive: Some(false),
            offline_indices: vec![],
        };
        (&stream)
            .write_all(format!("{}\n", serde_json::to_string(&response).unwrap()).as_bytes())
            .unwrap();
        request
    });

    let out = lokii(home.path(), &["search", "--online-only", "foo"].map(OsStr::new));
    assert_eq!(out.status.code(), Some(0));
    let req_json: serde_json::Value = serde_json::from_str(&server.join().unwrap()).unwrap();
    assert_eq!(req_json["include_offline"], false);
}

#[test]
fn alfred_format_omits_icon_and_prefixes_offline_subtitle_for_offline_indices() {
    let home = tempfile::tempdir().unwrap();
    let listener = fake_app(home.path());
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(&stream).read_line(&mut request).unwrap();
        let online_entry = FileEntry {
            name: "online.txt".into(),
            path: "/Users/me/online.txt".into(),
            is_dir: false,
            size: 1,
            modified: 0,
            created_at: 0,
            extension: "txt".into(),
        };
        let offline_entry = FileEntry {
            name: "offline.txt".into(),
            path: "/Volumes/NAS/offline.txt".into(),
            is_dir: false,
            size: 1,
            modified: 0,
            created_at: 0,
            extension: "txt".into(),
        };
        let response = Response {
            results: vec![online_entry, offline_entry],
            error: None,
            is_case_sensitive: Some(false),
            offline_indices: vec![1],
        };
        (&stream)
            .write_all(format!("{}\n", serde_json::to_string(&response).unwrap()).as_bytes())
            .unwrap();
    });

    let out = lokii(home.path(), &["search", "--alfred", "txt"].map(OsStr::new));
    assert_eq!(out.status.code(), Some(0));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let items = json["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["subtitle"], "/Users/me/online.txt");
    assert!(items[0].get("icon").is_some());
    assert_eq!(items[1]["subtitle"], "[Offline] /Volumes/NAS/offline.txt");
    assert!(
        items[1].get("icon").is_none(),
        "offline Alfred item must omit icon so Alfred skips stat on unmounted volumes"
    );
}
