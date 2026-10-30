# lsmdb

[![ci](https://github.com/monaaoua9/lsmdb/actions/workflows/ci.yml/badge.svg)](https://github.com/monaaoua9/lsmdb/actions)

Embedded LSM-tree storage engine in Rust. Group-committed WAL, lock-free skiplist memtable, mmap'd SSTables with bloom filters and sparse indexes, background leveled compaction, MVCC snapshots and snapshot-isolation transactions. Three small dependencies (`memmap2`, `crc32fast`, `lz4_flex`).

## Features

- **Durability**: write-ahead log with CRC per record, one `fdatasync` per group of concurrent writers, prefix recovery after a crash or torn write.
- **Concurrency**: multi-writer lock-free memtable; readers never block on writers.
- **Reads**: prefix-compressed and LZ4-compressed blocks, per-file bloom filter, binary search over a sparse block index, tables read through `mmap`.
- **Compaction**: leveled (L0 to L6), trivial moves, duplicate and tombstone removal that respects live snapshots.
- **MVCC**: cheap snapshots and range iterators; transactions with first-committer-wins conflict detection (`get_for_update` to avoid write skew).
- **Safety**: manifest with atomic `CURRENT` switch, directory lock, orphan-file sweep on open, DB goes read-only (`Error::Poisoned`) if the WAL or a background job fails.

- **Iteration**: forward and reverse (`rev()`, `seek_for_prev`), prefix scans, `delete_range`, per-transaction range scans that overlay uncommitted writes.
- **Extras**: `transact` with conflict retry, `compare_and_swap`, `increment`, named buckets, `TtlDb` with per-key expiry, compaction filter hook, `checkpoint` (hard-linked backup), read-only open, `verify_integrity`, LRU cache for decompressed blocks.

## Usage

```toml
[dependencies]
lsmdb-rs = "0.2"
```

```rust
use lsmdb::{Db, Options};

let db = Db::open("data", Options::default())?;
db.put("user:1", "ada")?;

let snap = db.snapshot();
db.put("user:1", "grace")?;
assert_eq!(snap.get(b"user:1")?.unwrap(), b"ada");   // frozen view

for kv in db.range(Some(&b"user:"[..]), Some(&b"user;"[..]))? {
    let (k, v) = kv?;
}

let mut tx = db.begin();
let n = tx.get_for_update(b"counter")?;
tx.put("counter", "1");
tx.commit()?;                                        // Err(Error::Conflict) if someone beat us
```

Atomic multi-key writes go through `WriteBatch`; pass `WriteOptions { sync: false }` to trade power-loss durability for speed.

### More API

```rust
let users = db.bucket("users")?;                 // isolated keyspace
users.put(b"1", "ada")?;

for kv in db.range(Some(&b"a"[..]), None)?.rev() { /* descending */ }
let hits = db.increment(b"hits", 1)?;            // atomic counter
db.checkpoint("backup-2026-10-30")?;             // consistent copy, hard-linked tables
let report = db.verify_integrity()?;             // reads every block, checks crc/order/bloom
```

`examples/lsmcli.rs` is a small command line front end (`get`, `put`, `scan`, `stats`, `verify`, `checkpoint`, ...):

```
cargo run --release --example lsmcli -- ./data scan user: 10
```

## Build and test

Needs Rust 1.89+.

```
cargo test --release                         # unit + integration + recovery + stress
cargo run --release --example bench -- 500000
```

## Testing

`cargo test --release` runs unit tests, integration tests, a randomized model check against a `BTreeMap`, a crash test that `kill`s a writer process at random moments and verifies recovery, and a fuzz test that corrupts random bytes in every kind of file. CI runs it on Linux, macOS and Windows.

## Numbers

200k keys, 100 B values, i5-8365U laptop, Windows 11, release build. Median of three runs:

| workload | ops/s |
|---|---|
| fill, batches of 100, no sync | ~165K |
| random read, hits | ~187K |
| random read, misses (bloom) | ~1.1M |
| full scan | ~1.8M |
| 8 writers, fsync every group | ~4.1K (fsync-bound on Windows) |

These move a lot between runs on a laptop (the scan row ranged from 1.1M to 2.4M), so treat them as a sanity check, not a benchmark against other engines. Method, caveats and the other workloads are in [docs/BENCHMARKS.md](docs/BENCHMARKS.md).

## Layout

```
src/
  db/            Db handle, write path (group commit), background flush/compaction, iterator, transactions
  memtable/      lock-free skiplist + memtable
  sstable/       block builder/iterator, table builder, mmap reader, footer format
  version/       file metadata, version edits, manifest-backed VersionSet
  wal.rs         log writer/reader (also used for the manifest)
  compaction.rs  leveled compaction picker
  bloom.rs  coding.rs  types.rs  iter.rs  batch.rs  options.rs  error.rs
tests/           integration, recovery, MVCC, randomized model check
examples/bench.rs
docs/ARCHITECTURE.md
docs/DESIGN.md
docs/BENCHMARKS.md
```

Design, on-disk formats, lock ordering, recovery and failure handling: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). The reasoning behind the main choices is shorter and lives in [docs/DESIGN.md](docs/DESIGN.md).

## Limitations

LZ4 is the only codec, no standalone block cache (the OS page cache serves mmap reads), one background thread, snapshot isolation rather than serializable, no directory fsync on Windows. Developed and tested on Windows; CI also covers Linux and macOS.

## License

MIT
