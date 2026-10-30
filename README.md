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

Old build (0.1.1, before the later features) against the current one, same bench, same session, run alternately five times each. 200k keys, 100 B values. Median ops/s, ratio is new / old:

| workload | 0.1.1 | current | ratio |
|---|---:|---:|---:|
| fill, batches of 100, no sync | 346K | 347K | 1.00 |
| random read, hits | 275K | 279K | 1.01 |
| random read, misses (bloom) | 1.56M | 1.62M | 1.04 |
| full scan | 3.19M | 3.26M | 1.02 |
| 8 writers, fsync | 3.3K | 3.4K | 1.03 |

An earlier comparison showed reads about 7% slower; that was the 8 MiB block cache thrashing on a table bigger than it, so the cache is now off by default (details in [docs/BENCHMARKS.md](docs/BENCHMARKS.md)).

The machine is a weak i5-8365U laptop with 8 GB, several terminals open and an almost full disk, so timings swing a lot (the same build gave 165K fill in an earlier session and 340K in this one). Only compare numbers from the same sitting; the absolute values here are a sanity check, not a benchmark against other engines.

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

LZ4 is the only codec, the block cache for decompressed blocks is off by default (it only helps when it fits the hot set; the OS page cache already serves mmap reads), one background thread, snapshot isolation rather than serializable, no directory fsync on Windows. Developed and tested on Windows; CI also covers Linux and macOS.

## License

MIT
