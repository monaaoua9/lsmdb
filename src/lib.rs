//! `lsmdb`: an embedded LSM-tree key-value engine.
//!
//! Writes are group-committed to a WAL and applied to a lock-free skiplist
//! memtable; full memtables become immutable mmap'd SSTables (prefix-
//! compressed blocks, sparse index, bloom filter); a background thread runs
//! leveled compaction. Reads are lock-free against writers and use MVCC
//! sequence numbers for snapshot isolation.
//!
//! ```
//! use lsmdb::{Db, Options};
//!
//! # let dir = std::env::temp_dir().join(format!("lsmdb-doc-{}", std::process::id()));
//! let db = Db::open(&dir, Options::default())?;
//! db.put("user:1", "ada")?;
//!
//! let snap = db.snapshot();
//! db.put("user:1", "grace")?;
//! assert_eq!(snap.get(b"user:1")?.unwrap(), b"ada");
//! assert_eq!(db.get(b"user:1")?.unwrap(), b"grace");
//!
//! let mut tx = db.begin();
//! tx.put("counter", "1");
//! tx.commit()?;
//! # drop(db);
//! # std::fs::remove_dir_all(&dir).ok();
//! # Ok::<(), lsmdb::Error>(())
//! ```

mod batch;
mod bloom;
mod cache;
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
pub use db::{Db, DbIter, RevIter, Snapshot, Stats, Transaction, TxnIter};
pub use error::{Error, Result};
pub use options::{Compression, Options, WriteOptions};
