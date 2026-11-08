use lokii_core::cache;
use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("cache path required");
    let started = Instant::now();
    let (index, checkpoint) =
        cache::load_compact(std::path::Path::new(&path)).expect("read-only cache load");
    if let Some(destination) = std::env::args().nth(2) {
        cache::save_compact(&index, checkpoint, std::path::Path::new(&destination)).unwrap();
    }
    println!(
        "{}",
        serde_json::json!({"entries":index.len(), "load_ms":started.elapsed().as_secs_f64()*1000.0,
        "pid":std::process::id(), "memory":index.memory_stats()})
    );
    std::thread::sleep(std::time::Duration::from_secs(15));
    std::hint::black_box(index);
}
