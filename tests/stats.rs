mod common;
use common::*;
use lsmdb::{WriteBatch, WriteOptions};

#[test]
fn write_counters() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    assert_eq!(db.stats().write_amplification(), 0.0);

    let mut b = WriteBatch::new();
    b.put("aa", "bbbb").put("c", "d").delete("e");
    db.write(b, &WriteOptions { sync: false }).unwrap();
    let s = db.stats();
    assert_eq!(s.write_ops, 3);
    assert_eq!(s.write_bytes, 2 + 4 + 1 + 1 + 1);
    assert_eq!(s.wal_syncs, 0);

    db.put("x", "y").unwrap();
    assert_eq!(db.stats().wal_syncs, 1);
}

#[test]
fn flush_and_compaction_counters() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    for round in 0..3 {
        for i in 0..400 {
            db.put(key(i), val(i, round)).unwrap();
        }
        db.flush().unwrap();
    }
    db.compact_all().unwrap();
    let s = db.stats();
    assert!(s.flushes >= 3, "{s:?}");
    assert!(s.flush_bytes > 0);
    assert!(s.compactions + s.trivial_moves > 0);
    assert!(s.compaction_write_bytes > 0 && s.compaction_read_bytes > 0);
    assert!(s.write_amplification() > 0.0);
}

#[test]
fn conflicts_are_counted() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    db.put("k", "1").unwrap();
    let mut tx = db.begin();
    tx.get_for_update(b"k").unwrap();
    db.put("k", "2").unwrap();
    tx.put("k", "3");
    assert!(tx.commit().is_err());
    assert_eq!(db.stats().conflicts, 1);
}

#[test]
fn table_info_matches_stats() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    assert!(db.table_info().is_empty());
    for round in 0..3 {
        for i in 0..500 {
            db.put(key(i), val(i, round)).unwrap();
        }
        db.flush().unwrap();
    }
    let info = db.table_info();
    let s = db.stats();
    for level in 0..s.files_per_level.len() {
        let here: Vec<_> = info.iter().filter(|t| t.level == level).collect();
        assert_eq!(here.len(), s.files_per_level[level]);
        assert_eq!(here.iter().map(|t| t.size).sum::<u64>(), s.bytes_per_level[level]);
    }
    for t in &info {
        assert!(t.smallest_key <= t.largest_key);
    }
    assert_eq!(info.iter().map(|t| &t.smallest_key).min().unwrap(), &key(0));
    assert_eq!(info.iter().map(|t| &t.largest_key).max().unwrap(), &key(499));
}

#[test]
fn approximate_size_follows_ranges() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    for i in 0..4000 {
        db.put(key(i), val(i, 0)).unwrap();
    }
    db.flush().unwrap();
    db.compact_all().unwrap();

    let total: u64 = db.stats().bytes_per_level.iter().sum();
    assert_eq!(db.approximate_size(b"", b"\xff"), total);
    let part = db.approximate_size(&key(0), &key(1000));
    assert!(part > 0 && part < total, "{part} of {total}");
    assert_eq!(db.approximate_size(b"zzz", b"zzzz"), 0);
    assert_eq!(db.approximate_size(&key(100), &key(100)), 0);
    assert_eq!(db.approximate_size(&key(200), &key(100)), 0);
}
