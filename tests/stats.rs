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
