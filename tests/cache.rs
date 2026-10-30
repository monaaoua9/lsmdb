mod common;
use common::*;
use lsmdb::{Db, Options};

fn fill(db: &Db) {
    for i in 0..1500 {
        db.put(key(i), "z".repeat(200)).unwrap();
    }
    db.flush().unwrap();
    db.compact_all().unwrap();
}

#[test]
fn repeated_reads_hit_the_cache() {
    let d = tempfile::tempdir().unwrap();
    let db = Db::open(d.path(), Options { block_cache_bytes: 4 << 20, ..small_opts() }).unwrap();
    fill(&db);

    for i in 0..1500 {
        assert_eq!(db.get(&key(i)).unwrap().unwrap().len(), 200);
    }
    let first = db.stats();
    assert!(first.cache_misses > 0);
    assert!(first.cache_bytes > 0);

    for i in 0..1500 {
        assert_eq!(db.get(&key(i)).unwrap().unwrap().len(), 200);
    }
    let second = db.stats();
    assert_eq!(second.cache_misses, first.cache_misses, "second pass should not decompress anything");
    assert!(second.cache_hits > first.cache_hits);
    assert_eq!(db.iter().unwrap().count(), 1500);
}

#[test]
fn disabled_cache_still_works() {
    let d = tempfile::tempdir().unwrap();
    let db = Db::open(d.path(), Options { block_cache_bytes: 0, ..small_opts() }).unwrap();
    fill(&db);
    for i in 0..1500 {
        assert!(db.get(&key(i)).unwrap().is_some());
    }
    let s = db.stats();
    assert_eq!((s.cache_hits, s.cache_misses, s.cache_bytes), (0, 0, 0));
}

#[test]
fn tiny_cache_stays_within_budget() {
    let d = tempfile::tempdir().unwrap();
    let budget = 16 << 10;
    let db = Db::open(d.path(), Options { block_cache_bytes: budget, ..small_opts() }).unwrap();
    fill(&db);
    for _ in 0..3 {
        for i in 0..1500 {
            assert!(db.get(&key(i)).unwrap().is_some());
        }
    }
    assert!(db.stats().cache_bytes <= budget);
}
