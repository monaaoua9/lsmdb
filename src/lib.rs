//! `lsmdb`: an embedded LSM-tree key-value engine.
//!
//! Writes are group-committed to a WAL and applied to a lock-free skiplist
//! memtable; full memtables become immutable mmap'd SSTables (prefix-
//! compressed blocks, sparse index, bloom filter); a background thread runs
//! leveled compaction. Reads are lock-free against writers and use MVCC
//! sequence numbers for snapshot isolation.

mod batch;
mod bloom;
mod coding;
mod compaction;
mod db;
mod error;
mod filename;
mod iter;
mod memtable;
mod metrics;
mod options;
mod sstable;
mod types;
mod version;
mod wal;

pub use batch::WriteBatch;
pub use db::{Db, DbIter, Snapshot, Stats, Transaction};
pub use error::{Error, Result};
pub use options::{Options, WriteOptions};
