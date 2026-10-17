use crate::store::JsonlStore;
use anyhow::Result;

/// Output of a manual `cache rebuild` invocation.
#[derive(Debug, Clone, Copy)]
pub struct CacheRebuildOutcome {
    /// Whether the on-disk cache had to be rebuilt or was already fresh.
    pub rebuilt: bool,
    /// Wall time spent, including the freshness check and rebuild.
    pub elapsed: f64,
    pub entries: u64,
    pub tags: u64,
    /// Cache file size in bytes.
    pub bytes: u64,
}

/// Synchronous (blocking) cache warm-up: does nothing but stale-cache work
/// when the cache already matches the current JSONL signature.
pub fn handle_cache_rebuild(store: &JsonlStore) -> Result<CacheRebuildOutcome> {
    let started = std::time::Instant::now();
    let report = store.warm_cache()?;
    Ok(CacheRebuildOutcome {
        rebuilt: report.rebuilt,
        elapsed: started.elapsed().as_secs_f64(),
        entries: report.entries,
        tags: report.tags,
        bytes: report.bytes,
    })
}
