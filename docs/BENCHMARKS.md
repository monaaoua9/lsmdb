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
