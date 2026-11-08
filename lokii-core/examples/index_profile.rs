use lokii_core::indexer::{FileEntry, IndexStore};
use lokii_core::search::{dispatch, SearchMode};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let value = |flag: &str| {
        args.windows(2)
            .find(|pair| pair[0] == flag)
            .map(|pair| pair[1].clone())
    };
    let count = value("--entries")
        .unwrap_or_else(|| "1000000".into())
        .parse::<usize>()
        .unwrap();
    let shape = value("--shape").unwrap_or_else(|| "mixed".into());
    let store = IndexStore::new();
    let started = Instant::now();
    let records: Vec<_> = (0..count)
        .map(|ordinal| {
            let name = match shape.as_str() {
                "shared" => format!("file_{:03}.rs", ordinal % 100),
                "unique" => format!("file_{ordinal:07}.rs"),
                _ if ordinal % 4 == 0 => format!("中文_{:03}.txt", ordinal % 400),
                _ if ordinal % 4 == 1 => format!("file_{ordinal:07}.rs"),
                _ => format!("file_{:03}.rs", ordinal % 100),
            };
            let parent = if shape == "unique" {
                ordinal / 2
            } else {
                ordinal / 100
            };
            let path = format!(
                "/synthetic/developer/project_{parent:07}/src/long_directory_component/{name}"
            );
            let extension = if name.ends_with(".rs") { "rs" } else { "txt" };
            FileEntry {
                name,
                path,
                is_dir: false,
                size: ordinal as u64,
                modified: 1_700_000_000,
                created_at: 1_600_000_000,
                extension: extension.into(),
            }
        })
        .collect();
    let legacy_estimated_bytes = records.capacity() * std::mem::size_of::<FileEntry>()
        + records.iter().map(|entry| entry.name.capacity() + entry.path.capacity() + entry.extension.capacity()).sum::<usize>();
    store.replace(records);
    let build_ms = started.elapsed().as_secs_f64() * 1000.0;
    let mut results = Vec::new();
    for (query, mode) in [
        ("file", SearchMode::Substring),
        ("file_001", SearchMode::Substring),
        (".rs", SearchMode::Substring),
        ("*.rs", SearchMode::Wildcard),
        ("regex:^file_00", SearchMode::Regex),
        ("f001", SearchMode::Fuzzy),
        ("missing-needle", SearchMode::Auto),
        ("long_directory_component/file_001", SearchMode::Auto),
        ("in:/synthetic/developer/project_0001234 file", SearchMode::Auto),
    ] {
        std::hint::black_box(dispatch(&store, query, mode, 500));
        let mut samples = Vec::new();
        for _ in 0..15 {
            let started = Instant::now();
            std::hint::black_box(dispatch(&store, query, mode, 500));
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        samples.sort_by(f64::total_cmp);
        results.push(
            serde_json::json!({"query": query, "mode": format!("{mode:?}"),
            "p50_ms": samples[7], "p95_ms": samples[14], "p99_ms": samples[14]}),
        );
    }
    let output = serde_json::json!({"entries":count, "shape":shape, "build_ms":build_ms,
        "legacy_estimated_bytes":legacy_estimated_bytes, "memory":store.read().memory_stats(), "queries":results, "pid":std::process::id()});
    if let Some(path) = value("--out") {
        std::fs::write(path, output.to_string()).unwrap();
    }
    println!("{output}");
    if let Some(seconds) = value("--hold") {
        std::thread::sleep(std::time::Duration::from_secs(seconds.parse().unwrap()));
    }
}
