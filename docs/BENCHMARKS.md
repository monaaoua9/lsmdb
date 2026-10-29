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

200,000 keys, ~16 byte keys, 100 byte values. One run, no averaging.

| workload | ops/s |
|---|---:|
| fill, batches of 100, no sync | 343,957 |
| `compact_all` (single pass) | 1,364,173 |
| random read, hits | 230,590 |
| random read, misses (bloom on) | 1,400,190 |
| full scan | 3,408,560 |
| 8 writers, 1 put each, fsync | 3,532 |
| 4 readers + 1 writer | 451,863 |

After `compact_all` all data sits in one L6 table. The bloom filter skipped 198,141 of 199,998 table probes on the miss workload, leaving 1,857 data-block reads.

## Reading these numbers

- The fsync row is bound by the disk. On this laptop a Windows fsync is slow, and group commit can only share it between writers that arrive together.
- Reads hit a single big table after compaction, which is the best case for the read path. A write-heavy database with several populated levels would read slower.
- The README has an older table from a 300k-key run on the same machine, and some rows differ a lot from this run (fill ~100K there vs 344K here). I haven't investigated why; different key count and a laptop that throttles are the likely suspects. Trust this file for the current code.
- This compares lsmdb with nothing. I haven't run RocksDB or LevelDB on this machine, so there's no claim here about being faster or slower than them.
