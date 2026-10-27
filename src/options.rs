#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    None,
    Lz4,
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
    pub l0_compaction_trigger: usize,
    pub l0_slowdown_trigger: usize,
    pub l0_stop_trigger: usize,
    /// max bytes for level 1; level n holds this * multiplier^(n-1)
    pub level_base_bytes: u64,
    pub level_multiplier: u64,
    pub verify_checksums: bool,
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
            l0_compaction_trigger: 4,
            l0_slowdown_trigger: 8,
            l0_stop_trigger: 12,
            level_base_bytes: 64 << 20,
            level_multiplier: 10,
            verify_checksums: true,
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
