use std::sync::atomic::AtomicU64;

#[derive(Default)]
pub struct Metrics {
    pub bloom_checked: AtomicU64,
    pub bloom_skipped: AtomicU64,
    pub block_reads: AtomicU64,
    pub write_ops: AtomicU64,
    pub write_bytes: AtomicU64,
    pub wal_syncs: AtomicU64,
    pub conflicts: AtomicU64,
    pub stalls: AtomicU64,
    pub flushes: AtomicU64,
    pub flush_bytes: AtomicU64,
    pub compactions: AtomicU64,
    pub trivial_moves: AtomicU64,
    pub compaction_read_bytes: AtomicU64,
    pub compaction_write_bytes: AtomicU64,
}
