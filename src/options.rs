use std::{fmt, sync::Arc};

use crate::error::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    None,
    Lz4,
}

/// Called for entries that pass through a compaction; return true to remove
/// the key. Only runs while no snapshot or transaction is open, and never
/// on a flush, so it is a cleanup hook, not a read-time filter.
#[derive(Clone)]
pub struct CompactionFilter(Arc<dyn Fn(&[u8], &[u8]) -> bool + Send + Sync>);

impl CompactionFilter {
    pub fn new(f: impl Fn(&[u8], &[u8]) -> bool + Send + Sync + 'static) -> Self {
        CompactionFilter(Arc::new(f))
    }

    pub(crate) fn should_remove(&self, key: &[u8], value: &[u8]) -> bool {
        (self.0)(key, value)
    }
}

impl fmt::Debug for CompactionFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CompactionFilter")
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    pub create_if_missing: bool,
    pub error_if_exists: bool,
    /// memtable is rotated once it holds about this many bytes
    pub write_buffer_size: usize,
    pub target_file_size: u64,
    pub block_size: usize,
    pub block_restart_interval: usize,
    /// 0 disables bloom filters
    pub bloom_bits_per_key: usize,
    /// applies to data blocks of newly written tables; readers handle both
    pub compression: Compression,
    /// bytes of decompressed blocks kept in memory; 0 disables the cache
    pub block_cache_bytes: usize,
    pub l0_compaction_trigger: usize,
    pub l0_slowdown_trigger: usize,
    pub l0_stop_trigger: usize,
    /// max bytes for level 1; level n holds this * multiplier^(n-1)
    pub level_base_bytes: u64,
    pub level_multiplier: u64,
    pub verify_checksums: bool,
    pub compaction_filter: Option<CompactionFilter>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            create_if_missing: true,
            error_if_exists: false,
            write_buffer_size: 16 << 20,
            target_file_size: 8 << 20,
            block_size: 4096,
            block_restart_interval: 16,
            bloom_bits_per_key: 10,
            compression: Compression::Lz4,
            block_cache_bytes: 8 << 20,
            l0_compaction_trigger: 4,
            l0_slowdown_trigger: 8,
            l0_stop_trigger: 12,
            level_base_bytes: 64 << 20,
            level_multiplier: 10,
            verify_checksums: true,
            compaction_filter: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct WriteOptions {
    /// fdatasync the WAL before acknowledging. concurrent writers share one sync
    pub sync: bool,
}

impl Default for WriteOptions {
    fn default() -> Self {
        WriteOptions { sync: true }
    }
}

impl Options {
    pub fn validate(&self) -> Result<()> {
        if self.write_buffer_size < 1024 {
            return Err(Error::invalid("write_buffer_size must be at least 1 KiB"));
        }
        if self.target_file_size == 0 {
            return Err(Error::invalid("target_file_size must be positive"));
        }
        if self.block_size < 64 {
            return Err(Error::invalid("block_size must be at least 64"));
        }
        if self.block_restart_interval == 0 {
            return Err(Error::invalid("block_restart_interval must be positive"));
        }
        if self.l0_compaction_trigger == 0 {
            return Err(Error::invalid("l0_compaction_trigger must be positive"));
        }
        if !(self.l0_compaction_trigger <= self.l0_slowdown_trigger
            && self.l0_slowdown_trigger <= self.l0_stop_trigger)
        {
            return Err(Error::invalid("need l0 compaction <= slowdown <= stop triggers"));
        }
        if self.level_base_bytes == 0 || self.level_multiplier < 2 {
            return Err(Error::invalid("level_base_bytes must be positive and level_multiplier >= 2"));
        }
        Ok(())
    }
}
