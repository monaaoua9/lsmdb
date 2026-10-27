mod common;
use common::*;
use lsmdb::Error;
use std::{sync::Arc, thread};

#[test]
fn snapshot_ignores_later_writes() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    db.put("k", "v1").unwrap();
    let snap = db.snapshot();
    db.put("k", "v2").unwrap();
    db.put("new", "x").unwrap();
    db.delete("k").unwrap();

    assert_eq!(snap.get(b"k").unwrap().unwrap(), b"v1");
    assert_eq!(snap.get(b"new").unwrap(), None);
    assert_eq!(db.get(b"k").unwrap(), None);
}

#[test]
fn snapshot_survives_flush_and_compaction() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    for i in 0..500 {
        db.put(key(i), val(i, 0)).unwrap();
    }
    let snap = db.snapshot();
    for gen in 1..4 {
        for i in 0..500 {
            db.put(key(i), val(i, gen)).unwrap();
        }
        db.flush().unwrap();
    }
    for i in 0..500 {
        db.delete(key(i)).unwrap();
    }
    db.compact_all().unwrap();

    for i in 0..500 {
        assert_eq!(snap.get(&key(i)).unwrap().unwrap(), val(i, 0), "key {i}");
        assert_eq!(db.get(&key(i)).unwrap(), None);
    }
    drop(snap);
    // with the snapshot gone a second pass may now reclaim the history
    db.compact_all().unwrap();
    assert_eq!(db.iter().unwrap().count(), 0);
}

#[test]
fn snapshot_iterator_is_stable() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    for i in 0..100 {
        db.put(key(i), val(i, 0)).unwrap();
    }
    let it = db.snapshot().range(None, None).unwrap();
    for i in 0..100 {
        db.put(key(i), val(i, 1)).unwrap();
        db.put(key(1000 + i), val(i, 1)).unwrap();
    }
    db.flush().unwrap();
    let got: Vec<_> = it.map(|r| r.unwrap()).collect();
    assert_eq!(got.len(), 100);
    for (i, (k, v)) in got.iter().enumerate() {
        assert_eq!(k, &key(i as u32));
        assert_eq!(v, &val(i as u32, 0));
    }
}

#[test]
fn readers_are_not_blocked_by_writers() {
    let d = tempfile::tempdir().unwrap();
    let db = Arc::new(open(d.path()));
    for i in 0..200 {
        db.put(key(i), val(i, 0)).unwrap();
    }
    let writer = {
        let db = db.clone();
        thread::spawn(move || {
            for gen in 1..=20 {
                for i in 0..200 {
                    db.put(key(i), val(i, gen)).unwrap();
                }
            }
        })
    };
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let db = db.clone();
            thread::spawn(move || {
                for _ in 0..200 {
                    let snap = db.snapshot();
                    // within a snapshot every key must carry the same generation
                    // or an older-and-consistent one: re-reading gives identical bytes
                    let a: Vec<_> = (0..200).map(|i| snap.get(&key(i)).unwrap().unwrap()).collect();
                    let b: Vec<_> = (0..200).map(|i| snap.get(&key(i)).unwrap().unwrap()).collect();
                    assert_eq!(a, b);
                }
            })
        })
        .collect();
    writer.join().unwrap();
    for r in readers {
        r.join().unwrap();
    }
}

#[test]
fn txn_reads_own_writes_and_commits() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    db.put("a", "1").unwrap();

    let mut t = db.begin();
    assert_eq!(t.get(b"a").unwrap().unwrap(), b"1");
    t.put("a", "2");
    t.put("b", "3");
    t.delete("c");
    assert_eq!(t.get(b"a").unwrap().unwrap(), b"2");
    // not visible outside until commit
    assert_eq!(db.get(b"a").unwrap().unwrap(), b"1");
    t.commit().unwrap();
    assert_eq!(db.get(b"a").unwrap().unwrap(), b"2");
    assert_eq!(db.get(b"b").unwrap().unwrap(), b"3");
}

#[test]
fn txn_rollback_discards() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    let mut t = db.begin();
    t.put("a", "1");
    t.rollback();
    assert_eq!(db.get(b"a").unwrap(), None);
}

#[test]
fn txn_write_write_conflict() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    db.put("x", "0").unwrap();

    let mut t1 = db.begin();
    let mut t2 = db.begin();
    t1.put("x", "from-t1");
    t2.put("x", "from-t2");
    t1.commit().unwrap();
    assert!(matches!(t2.commit(), Err(Error::Conflict)));
    assert_eq!(db.get(b"x").unwrap().unwrap(), b"from-t1");
}

#[test]
fn txn_conflicts_with_plain_write() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    let mut t = db.begin();
    t.put("x", "txn");
    db.put("x", "plain").unwrap();
    assert!(matches!(t.commit(), Err(Error::Conflict)));
}

#[test]
fn txn_conflicts_with_concurrent_delete() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    db.put("x", "0").unwrap();
    let mut t = db.begin();
    t.put("x", "txn");
    db.delete("x").unwrap();
    db.compact_all().unwrap();
    assert!(matches!(t.commit(), Err(Error::Conflict)));
}

#[test]
fn txn_disjoint_keys_both_commit() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    let mut t1 = db.begin();
    let mut t2 = db.begin();
    t1.put("a", "1");
    t2.put("b", "2");
    t1.commit().unwrap();
    t2.commit().unwrap();
    assert_eq!(db.get(b"a").unwrap().unwrap(), b"1");
    assert_eq!(db.get(b"b").unwrap().unwrap(), b"2");
}

#[test]
fn get_for_update_blocks_write_skew() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    db.put("on_call_a", "yes").unwrap();
    db.put("on_call_b", "yes").unwrap();

    // classic write skew: each txn checks the other, then goes off call
    let mut t1 = db.begin();
    let mut t2 = db.begin();
    t1.get_for_update(b"on_call_b").unwrap();
    t2.get_for_update(b"on_call_a").unwrap();
    t1.put("on_call_a", "no");
    t2.put("on_call_b", "no");
    t1.commit().unwrap();
    assert!(matches!(t2.commit(), Err(Error::Conflict)));
}

#[test]
fn concurrent_counter_increments_never_lose_updates() {
    let d = tempfile::tempdir().unwrap();
    let db = Arc::new(open(d.path()));
    db.put("ctr", "0").unwrap();

    let threads = 6;
    let per = 40;
    let hs: Vec<_> = (0..threads)
        .map(|_| {
            let db = db.clone();
            thread::spawn(move || {
                let mut done = 0;
                while done < per {
                    let mut t = db.begin();
                    let cur: u64 = String::from_utf8(t.get_for_update(b"ctr").unwrap().unwrap())
                        .unwrap()
                        .parse()
                        .unwrap();
                    t.put("ctr", (cur + 1).to_string());
                    match t.commit() {
                        Ok(()) => done += 1,
                        Err(Error::Conflict) => {}
                        Err(e) => panic!("{e}"),
                    }
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    let v = String::from_utf8(db.get(b"ctr").unwrap().unwrap()).unwrap();
    assert_eq!(v, (threads * per).to_string());
}
