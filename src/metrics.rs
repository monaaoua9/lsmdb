use std::sync::atomic::AtomicU64;

#[derive(Default)]
pub struct Metrics {
    pub bloom_checked: AtomicU64,
    pub bloom_skipped: AtomicU64,
    pub block_reads: AtomicU64,
}
