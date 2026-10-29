# lsmdb: design decisions

Short version of why it looks the way it does. The full walkthrough (formats, lock order, recovery steps) is in [ARCHITECTURE.md](ARCHITECTURE.md).

## Scope

An embedded, single-process key-value store. Ordered keys, atomic batches, snapshots, optimistic transactions. No network layer, no column families, no merge operators.

## Decisions

**Group commit through a writer queue.** The writer at the head of the queue becomes leader, folds the queued writers into one batch, writes one WAL record and does one `fdatasync`. Every `put` pays for durability, but concurrent writers share the cost. The price is that a follower's latency includes the leader's fsync. With 8 writers and sync on, throughput is bound by the disk (see BENCHMARKS.md).

**Lock-free skiplist memtable that never unlinks nodes.** Readers never block writers and the other way round. Because nodes are only freed when the whole list is dropped, published pointers stay valid and the `unsafe` stays in one file (`memtable/skiplist.rs`), which CI runs under Miri. The cost is that deleted entries take memory until the memtable is flushed, which is fine since memtables are short-lived.

**mmap for SSTable reads, no block cache of its own.** The OS page cache is the cache. Raw blocks are slices of the mapping, so there's no copy. LZ4-compressed blocks are decompressed into a heap buffer and lose that. I didn't add a block cache because it would double-buffer against the page cache; the downside is I can't control eviction.

**Sequence numbers for MVCC.** A snapshot is a registered sequence number. Compaction keeps whatever a live snapshot might still read. Transactions are snapshot isolation with first-committer-wins, checked by the group leader so check-and-write is atomic. Write skew is possible unless the caller uses `get_for_update`.

**A failed WAL write or fsync poisons the database.** After a failed `fsync` the on-disk state is unknown, so continuing to acknowledge writes would be lying. Everything turns read-only and returns `Error::Poisoned`.

**One background thread for flush and compaction.** Simple, no scheduling between jobs, flushes go first. The cost is that a big compaction can delay a flush and trigger write stalls when L0 hits the stop trigger.

**Recovery replays the WAL straight into an L0 table**, then sweeps files the manifest doesn't know about (half-written tables from a crashed compaction, old logs). A torn tail record is dropped, so you get a prefix of the acknowledged history.

## How it's checked

- Crash test: a child process writes and prints what it acknowledged, the parent kills it at a random moment, reopens, and checks every acknowledged write is there and the data is a gap-free prefix.
- Fuzz test: bit flips, truncation and garbage appended to the WAL, tables, manifest and `CURRENT`. Nothing may panic.
- Randomized model check against a `BTreeMap` with flushes, compactions and reopens.
- Miri on the skiplist.

## What's not covered

- The crash test kills the process. Real power loss isn't simulated.
- On Windows the directory fsync after creating files is skipped (std can't open directories there).
- Only LZ4, no rate limiter, one background thread, SI rather than serializable.

## What I'd do next

Parallel compaction, an optional block cache for compressed blocks, zstd.
