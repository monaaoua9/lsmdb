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

## 0.1.1 vs current

Measured 2026-10-31 on the same laptop, with other terminals open and C: nearly full. The old build is commit 1ce386a (0.1.1) with the current `examples/bench.rs` copied in; both binaries were built `--release` into separate target dirs and run alternately (old, new, old, new, ...) five times each, 200,000 keys. Cells are median with min-max.

| workload | 0.1.1 ops/s | current ops/s | new / old |
|---|---:|---:|---:|
| fill, batches of 100, no sync | 346,224 (317,855 - 355,604) | 347,035 (307,825 - 359,501) | 1.00 |
| `compact_all` | 1,486,532 (1,364,479 - 1,542,007) | 1,506,675 (1,375,614 - 1,597,924) | 1.01 |
| random read, hits | 275,244 (262,997 - 281,418) | 279,249 (258,014 - 283,078) | 1.01 |
| random read, misses | 1,557,446 (1,502,883 - 1,680,302) | 1,620,853 (1,520,328 - 1,662,398) | 1.04 |
| full scan | 3,187,424 (3,151,075 - 3,274,362) | 3,257,043 (3,078,031 - 3,414,852) | 1.02 |
| 8 writers, fsync | 3,303 (3,170 - 3,392) | 3,386 (3,201 - 3,602) | 1.03 |
| 4 readers + 1 writer | 446,420 (426,840 - 459,691) | 455,212 (430,741 - 462,531) | 1.02 |

Everything is within the spread of the runs.

## The block cache regression

The first 0.1.1 vs 0.2.0 run (same method) had random read hits at 0.93x and full scan at 0.93x, and the hit ranges didn't overlap. Setting `BENCH_CACHE` (a bench-only env var for `block_cache_bytes`) isolated it. Random read hits, median of 4 runs interleaved:

| block cache | ops/s |
|---|---:|
| 0 (off) | 248,664 (231,836 - 258,108) |
| 8 MiB (old default) | 210,444 (188,641 - 233,799) |
| 64 MiB | 299,300 (264,278 - 334,120) |

The table is about 25 MB, so an 8 MiB cache mostly misses and every read pays for the lookup, the insert and the eviction on top of the LZ4 decompression. A cache that holds the working set is ~20% faster than none. So `block_cache_bytes` now defaults to 0; size it to your hot set if you want it.

Both tables above come from the same laptop on the same day but different sittings (compare the 0 and 8 MiB rows with the first table in this section: absolute values drift by 10-30%). Only compare numbers inside one table.
