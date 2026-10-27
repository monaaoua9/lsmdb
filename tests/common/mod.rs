#![allow(dead_code)]
use lsmdb::{Db, Options};
use std::path::Path;

// tiny sizes so tests hit flush + compaction after a few hundred keys
pub fn small_opts() -> Options {
    Options {
        write_buffer_size: 32 << 10,
        target_file_size: 16 << 10,
        block_size: 1024,
        level_base_bytes: 64 << 10,
        l0_compaction_trigger: 2,
        ..Options::default()
    }
}

pub fn open(dir: &Path) -> Db {
    Db::open(dir, small_opts()).unwrap()
}

pub fn key(i: u32) -> Vec<u8> {
    format!("key-{i:08}").into_bytes()
}

pub fn val(i: u32, gen: u32) -> Vec<u8> {
    format!("value-{i}-gen{gen}-{}", "x".repeat(40)).into_bytes()
}

pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}
