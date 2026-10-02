use std::sync::atomic::AtomicU64;

/// Watcher and indexing metrics for diagnostics.
///
/// All counters use relaxed ordering — metrics are informational,
/// not used for synchronization.
#[derive(Default)]
pub struct WatcherMetrics {
    /// Total FSEvents received in callback.
    pub events_received: AtomicU64,
    /// Events dropped by fast-path filter (excluded paths).
    pub events_dropped_excluded: AtomicU64,
    /// Events that passed the filter and were enqueued.
    pub events_enqueued: AtomicU64,
    /// Batches processed after debounce/coalescing timeout.
    pub batches_processed: AtomicU64,
}

impl WatcherMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return a snapshot as a JSON string for diagnostics.
    pub fn snapshot_json(&self) -> String {
        let received = self.events_received.load(std::sync::atomic::Ordering::Relaxed);
        let dropped = self.events_dropped_excluded.load(std::sync::atomic::Ordering::Relaxed);
        let enqueued = self.events_enqueued.load(std::sync::atomic::Ordering::Relaxed);
        let batches = self.batches_processed.load(std::sync::atomic::Ordering::Relaxed);

        format!(
            r#"{{"events_received":{},"events_dropped_excluded":{},"events_enqueued":{},"batches_processed":{}}}"#,
            received, dropped, enqueued, batches
        )
    }
}
