# lsmdb architecture

An embedded LSM-tree key-value store in Rust (~3k lines, three small dependencies: `memmap2`, `crc32fast`, `lz4_flex`).

```
 put/delete/WriteBatch/Transaction.commit
            │
            ▼
   ┌─────────────────┐  leader/follower group commit: one WAL append + one fdatasync per group
   │  writer queue   │
   └────────┬────────┘
            ▼
   ┌─────────────────┐   ┌──────────────────────┐
   │  WAL (.log)     │   │ MemTable (skiplist)  │  lock-free CAS inserts, readers never block
   └─────────────────┘   └──────────┬───────────┘
        (1) append+sync (2) insert  │ full → rotate, new WAL
                                    ▼
                         immutable MemTable ──flush──▶ L0 SSTables (overlapping, newest first)
                                                         │ leveled compaction (background)
                                                         ▼
                                     L1 … L6 (sorted, non-overlapping, ×10 size each)
```

## Write path

1. `Db::write` validates the batch and enqueues a `Writer`.
2. The writer at the queue head is the **leader**. It folds queued writers (up to 1 MiB) into one group.
3. Leader calls `make_room_for_write`: stalls while L0 ≥ `l0_stop_trigger`, sleeps 1 ms once at the slowdown trigger, and rotates the memtable (new WAL file, old memtable becomes `imm`) when it is full.
4. Transaction writers in the group are conflict-checked here, serially, so check-and-write is atomic (see MVCC).
5. The group is encoded as **one WAL record** (`first_seq | count | ops`) and appended with a single `write`; `fdatasync` runs if any member asked for `sync` (the default).
6. Ops are inserted into the memtable with consecutive sequence numbers.
7. `last_seq` is published with `Release` **after** all inserts, so readers see a batch entirely or not at all.

Durability = step 5. Anything acknowledged with `sync: true` survives power loss; `sync: false` survives process crash (OS page cache) but not power loss.

## Read path

`get(key)`: read `last_seq` → clone `Arc`s of `{mem, imm, version}` under one short mutex hold → search `mem`, then `imm`, then the version: every overlapping L0 file newest-first, then at most one file per deeper level (binary search on file ranges). Per table: bloom filter on the user key → binary search of the sparse index → binary search of the block's restart array → short linear scan. First hit wins; a tombstone is a definitive "not found".

No lock is held while touching data. Writers never block readers.

## Components

| Piece | File | Notes |
|---|---|---|
| Internal key | `types.rs` | `user_key ‖ le64(seq<<8 \| kind)`; order = key asc, seq desc, so the newest visible version is the first one a seek lands on |
| MemTable | `memtable/skiplist.rs` | Lock-free multi-writer skiplist (CAS per level, level 0 first). Nodes are never unlinked, so published pointers stay valid until the list is dropped, which is what makes the `unsafe` small and local. Iterators hold an `Arc` to the list |
| WAL | `wal.rs` | `masked crc32 ‖ len ‖ payload`. Replay stops at the first bad/short record (point-in-time recovery: you get a prefix of the acknowledged history) |
| SSTable | `sstable/*` | `[data blocks][bloom][index][footer]`. Data blocks use prefix compression with restart points every 16 keys; each block has a 5-byte trailer (type + masked CRC). Data blocks are LZ4-compressed (`Options::compression`, default on) and kept raw when LZ4 saves less than 1/8; the filter and index blocks are always raw. Compressed blocks are decompressed into a heap buffer on read, so they lose the zero-copy mmap path that raw blocks keep. Index has one entry per block (its last key) with restart interval 1, i.e. a pure binary search. Bloom is keyed on user key (10 bits/key ≈ 1% FPR) |
| mmap reads | `sstable/reader.rs` | Tables are mapped read-only; blocks are slices of the mapping, so no copy into user space and the OS page cache is the block cache. CRC verified on every block read (configurable) |
| Manifest | `version/*` | Log of `VersionEdit`s (add/delete file, log number, next file, last seq), same record format as the WAL. A fresh snapshot manifest is written on every open and `CURRENT` is flipped by write-tmp + fsync + rename |
| Compaction | `compaction.rs`, `db/background.rs` | Leveled. L0→L1 when ≥4 files; Ln→Ln+1 when a level exceeds `base × 10^(n-1)` (round-robin file pick). Trivial moves when a file overlaps nothing below. One background thread does flushes first, then compactions |

## MVCC and transactions

* Every write gets a sequence number. A **snapshot** is just a registered sequence number; reads at snapshot `s` ignore anything with `seq > s`.
* Compaction keeps, for each user key, the newest version plus everything a live snapshot might still need. An entry is dropped when a newer entry for the same key is already ≤ the oldest live snapshot. A tombstone is dropped only if it is also ≤ the oldest snapshot **and** no deeper level can contain the key.
* `Transaction` = snapshot + private write set. `commit` submits a normal write with a conflict list; the group leader rejects it with `Error::Conflict` if any listed key has a version newer than the transaction's snapshot (checked against memtables, tables, and earlier members of the same group). This is **snapshot isolation with first-committer-wins**. `get_for_update` adds read keys to the check, which closes write-skew cases when you use it.
* The transaction keeps its snapshot registered until commit returns, so the versions/tombstones the check depends on cannot be compacted away.

## Concurrency model and lock order

* `queue` mutex: only the write queue.
* `versions` mutex → `state` mutex, always in that order. Readers take `state` only. `install()` swaps the new `Version` into `state` while still holding `versions`, so two installers can't publish stale versions over each other.
* `compaction_lock` serializes background and manual (`compact_all`) compactions.
* A flush publishes the new `Version` and clears `imm` under one `state` hold, so a reader can't observe "memtable gone, table not there yet".
* Obsolete files are deleted by `FileMeta::drop`, i.e. when the last `Version`/iterator referencing them goes away: running iterators and snapshots keep their files alive without any explicit reference counting protocol.

## Recovery

1. Lock the directory (`LOCK`, exclusive), then start the file-number counter above every numbered file already on disk (the manifest's counter can lag behind files a crashed run created, e.g. a rotated WAL or a half-written table).
2. Replay `CURRENT` → manifest to rebuild levels, `log_number`, `last_seq`.
3. Replay every WAL ≥ `log_number` into a memtable; a torn tail record is dropped.
4. Flush the recovered memtable straight to an L0 table, record the new `log_number` in the manifest, open a fresh WAL.
5. Sweep files the manifest doesn't know about (orphaned tables from a crashed compaction, old logs/manifests, temp files).

## Failure handling

A WAL append/fsync failure puts the database in a poisoned, read-only state (`Error::Poisoned`): after a failed `fsync` the on-disk state is unknowable, so continuing to acknowledge writes would be lying. Background flush/compaction errors do the same. Corrupt blocks surface as `Error::Corruption` from the read that hit them, never as wrong data.

## Tests

* Unit: skiplist (including 8-thread concurrent insert, reader/writer race), block builder/iterator, bloom FPR, WAL torn-tail and bit-flip, varints, manifest edits.
* Crash (`tests/crash.rs`): the test binary re-executes itself as a writer that prints each acknowledged key, the parent `kill`s it at a random moment, reopens, and checks that every acknowledged write survived (sync mode), that the recovered data is a gap-free prefix, and that two-key batches are atomic. Repeated over a dozen kill/recover cycles per run. This is process death, not power loss.
* Fuzz (`tests/fuzz.rs`): flips bits, truncates, overwrites and appends garbage in random files (WAL, tables, manifest, CURRENT) of a healthy db, then opens, reads, scans and compacts; nothing may panic.
* Miri: the skiplist (the only module with real `unsafe` aliasing) is run under Miri in CI.
* Integration (`tests/`): CRUD, scans, bloom effectiveness, reopen cycles, torn/corrupt WAL, corrupt SSTable, orphan sweep, snapshot isolation across flush+compaction, transaction conflicts, write skew, lost-update counter under contention, randomized model check against a `BTreeMap` with flushes/compactions/reopens, write-stall liveness.

## Honest limitations

* LZ4 only (no zstd/snappy), no rate limiter, no column families, no merge operators.
* Single background thread (flush and compaction are serialized).
* No explicit block cache (relies on the OS page cache through mmap); `O_DIRECT` isn't used because it fights mmap and the page cache that serves the read path.
* Isolation is SI, not serializable (write skew is possible unless you use `get_for_update`).
* The manifest is compacted by rewrite on open, not while running.
* Directory fsync after creating files and renaming `CURRENT` is done on Unix; on Windows it is skipped (std can't open directories there). Crash tests kill the process; real power-loss behaviour of the filesystem is not simulated.

## Later additions

* **Reverse iteration.** Every `InternalIterator` has `seek_to_last`/`prev`. Blocks step back by jumping to the restart point before the current entry and scanning forward; the skiplist uses `find_lt`; `MergingIterator` re-seeks the other children when the direction flips. `DbIter` walks versions of a key oldest-first in reverse mode, so each visible entry overwrites the last one and a tombstone clears the candidate.
* **Block cache.** Only LZ4 blocks need it (raw blocks are mmap slices). 16-shard LRU keyed by (table id, offset), budget from `Options::block_cache_bytes` (default 0 = off: a cache smaller than the hot set costs more than the LZ4 decompression it saves, see BENCHMARKS.md).
* **Compaction filter.** A put the filter removes becomes a tombstone unless no deeper level can hold the key. The filter is skipped while any snapshot is open, and trivial moves are disabled when one is configured so every table is seen once.
* **TtlDb.** Values carry an 8 byte deadline; reads hide expired entries, a compaction filter reclaims them.
* **Checkpoint.** Flush, then under the compaction lock hard-link live tables and write a fresh single-record manifest into the target directory.
* **Read-only.** `VersionSet` skips the snapshot manifest, the log is replayed into a memtable that is never flushed, `LOCK` is taken shared.
