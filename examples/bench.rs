//! cargo run --release --example bench -- [num_keys] [dir]
use lsmdb::{Db, Options, WriteBatch, WriteOptions};
use std::{
    env, fs,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

fn key(i: u64) -> Vec<u8> {
    // scrambled so "random" phases don't walk the keyspace in order
    format!("user{:016x}", i.wrapping_mul(0x9e37_79b9_7f4a_7c15)).into_bytes()
}

struct Xor(u64);
impl Xor {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn report(name: &str, ops: u64, t: Duration) {
    let secs = t.as_secs_f64();
    println!("{name:<34} {:>12.0} ops/s   {:>8.2} s", ops as f64 / secs, secs);
}

fn fresh(dir: &str, opts: Options) -> Db {
    let _ = fs::remove_dir_all(dir);
    Db::open(dir, opts).unwrap()
}

fn main() {
    let n: u64 = env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(500_000);
    let dir = env::args().nth(2).unwrap_or_else(|| "bench-data".into());
    let value = vec![b'v'; 100];
    println!("lsmdb bench: {n} keys, 16B-ish keys, 100B values\n");

    let cache: usize = env::var("BENCH_CACHE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(Options::default().block_cache_bytes);
    let opts = Options { write_buffer_size: 32 << 20, block_cache_bytes: cache, ..Options::default() };
    let db = Arc::new(fresh(&dir, opts.clone()));

    let wo = WriteOptions { sync: false };
    let t = Instant::now();
    for chunk in (0..n).collect::<Vec<_>>().chunks(100) {
        let mut b = WriteBatch::new();
        for &i in chunk {
            b.put(key(i), value.clone());
        }
        db.write(b, &wo).unwrap();
    }
    report("fill (batch=100, no sync)", n, t.elapsed());

    db.flush().unwrap();
    let t = Instant::now();
    db.compact_all().unwrap();
    report("compact_all", n, t.elapsed());
    println!("  levels: {:?}\n", db.stats().files_per_level);

    let reads = n.min(300_000);
    let mut rng = Xor(88172645463325252);
    let t = Instant::now();
    for _ in 0..reads {
        let i = rng.next() % n;
        assert!(db.get(&key(i)).unwrap().is_some());
    }
    report("readrandom (hits)", reads, t.elapsed());

    let before = db.stats();
    let t = Instant::now();
    for _ in 0..reads {
        let i = n + rng.next() % n;
        assert!(db.get(&key(i)).unwrap().is_none());
    }
    report("readrandom (misses, bloom on)", reads, t.elapsed());
    let after = db.stats();
    println!(
        "  bloom skipped {} of {} table probes, {} data-block reads\n",
        after.bloom_skipped - before.bloom_skipped,
        after.bloom_checked - before.bloom_checked,
        after.block_reads - before.block_reads
    );

    let t = Instant::now();
    let cnt = db.iter().unwrap().count() as u64;
    report("full scan", cnt, t.elapsed());

    let threads = 8;
    let per = 1_500u64;
    let t = Instant::now();
    let hs: Vec<_> = (0..threads)
        .map(|th| {
            let db = db.clone();
            let value = value.clone();
            thread::spawn(move || {
                for i in 0..per {
                    db.put(key(n * 4 + th * per + i), value.clone()).unwrap();
                }
            })
        })
        .collect();
    hs.into_iter().for_each(|h| h.join().unwrap());
    report("8 writers, 1 put each, fsync", threads * per, t.elapsed());

    let t = Instant::now();
    let readers: Vec<_> = (0..4)
        .map(|r| {
            let db = db.clone();
            thread::spawn(move || {
                let mut rng = Xor(0x1234_5678 + r);
                for _ in 0..100_000 {
                    let _ = db.get(&key(rng.next() % n)).unwrap();
                }
            })
        })
        .collect();
    let writer = {
        let db = db.clone();
        let value = value.clone();
        thread::spawn(move || {
            let wo = WriteOptions { sync: false };
            for i in 0..100_000u64 {
                let mut b = WriteBatch::new();
                b.put(key(n * 8 + i), value.clone());
                db.write(b, &wo).unwrap();
            }
        })
    };
    readers.into_iter().for_each(|h| h.join().unwrap());
    writer.join().unwrap();
    report("4 readers + 1 writer (reads+writes)", 500_000, t.elapsed());

    let st = db.stats();
    println!("\nfinal: levels {:?}, memtable {} B", st.files_per_level, st.memtable_bytes);
    drop(db);
    let _ = fs::remove_dir_all(&dir);
}
