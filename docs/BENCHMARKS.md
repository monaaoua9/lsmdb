# lsmdb: benchmarks

Measured 2026-10-29.

| | |
|---|---|
| CPU | Intel i5-8365U, 4 cores / 8 threads, 1.6 GHz base |
| RAM / disk | 8 GB, SSD |
| OS | Windows 11 |
| Toolchain | Rust 1.99.1, `--release` |

Reproduce:

```
cargo run --release --example bench -- 200000
```

200,000 keys, ~16 byte keys, 100 byte values. Three back-to-back runs; each cell is the median with the min–max in brackets.

| workload | ops/s |
|---|---:|
| fill, batches of 100, no sync | 165,466 (154,826 – 193,909) |
| `compact_all` (single pass) | 934,566 (915,009 – 1,070,793) |
| random read, hits | 187,200 (145,646 – 202,994) |
| random read, misses (bloom on) | 1,114,486 (807,186 – 1,153,282) |
| full scan | 1,831,634 (1,146,058 – 2,421,774) |
| 8 writers, 1 put each, fsync | 4,123 (3,676 – 4,178) |
| 4 readers + 1 writer | 243,574 (224,275 – 263,860) |

After `compact_all` all data sits in one L6 table. On the miss workload the bloom filter skipped 198,141 of 199,998 table probes, leaving 1,857 data-block reads.

## Reading these numbers

- **They move a lot between runs.** The scan row spans 1.1M to 2.4M ops/s inside three runs. A fourth run earlier the same day, not included above, was faster still (fill 344K, scan 3.4M). This is a laptop CPU that changes clock speed with temperature and power state, so only compare numbers that came from the same sitting.
- The fsync row is bound by the disk. On this laptop a Windows fsync is slow, and group commit can only share it between writers that arrive together.
- Reads hit a single big table after compaction, which is the best case for the read path. A write-heavy database with several populated levels would read slower.
- Nothing here compares lsmdb with another engine. I haven't run RocksDB or LevelDB on this machine, so there's no claim about being faster or slower than them.

## 0.1.1 vs 0.2.0

Measured 2026-10-31 on the same laptop, with other terminals open and C: nearly full. The old build is commit 1ce386a (0.1.1) with the current `examples/bench.rs` copied in; both binaries were built `--release` into separate target dirs and run alternately (old, new, old, new, ...) five times each, 200,000 keys. Cells are median with min-max.

| workload | 0.1.1 ops/s | 0.2.0 ops/s | new / old |
|---|---:|---:|---:|
| fill, batches of 100, no sync | 340,533 (287,748 - 368,411) | 359,446 (332,994 - 370,953) | 1.06 |
| `compact_all` | 1,604,926 (1,573,737 - 1,673,266) | 1,573,417 (1,414,832 - 1,653,278) | 0.98 |
| random read, hits | 278,379 (277,596 - 301,193) | 259,938 (251,051 - 263,832) | 0.93 |
| random read, misses | 1,667,121 (1,587,954 - 1,697,747) | 1,688,175 (1,424,467 - 1,762,966) | 1.01 |
| full scan | 3,322,496 (2,748,869 - 3,775,451) | 3,096,009 (3,049,045 - 3,290,735) | 0.93 |
| 8 writers, fsync | 3,382 (3,203 - 3,834) | 3,438 (1,735 - 3,621) | 1.02 |
| 4 readers + 1 writer | 445,342 (443,242 - 501,501) | 442,815 (425,615 - 487,327) | 0.99 |

- Read hits and full scan are ~7% slower in 0.2.0. For hits the min-max ranges don't overlap, so it is probably real. Suspects: the new counters on the read path, the block cache lookup, the rewritten iterator. Not profiled yet.
- Everything else is within the spread of the runs.
- These absolute numbers are about twice the ones in the 2026-10-29 table above. Same laptop, different sitting: clock speed and background load. Compare only within one table.
