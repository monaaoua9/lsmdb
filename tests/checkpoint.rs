mod common;
use common::*;
use lsmdb::{Db, Error};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[test]
fn checkpoint_is_a_frozen_copy() {
    let d = tempfile::tempdir().unwrap();
    let db = open(&d.path().join("live"));
    for i in 0..1000 {
        db.put(key(i), val(i, 0)).unwrap();
    }
    db.flush().unwrap();
    // still only in the memtable when the checkpoint starts
    for i in 1000..1100 {
        db.put(key(i), val(i, 0)).unwrap();
    }

    let cp = d.path().join("cp");
    db.checkpoint(&cp).unwrap();

    for i in 0..1100 {
        db.put(key(i), val(i, 1)).unwrap();
    }
    db.delete(key(5)).unwrap();
    db.compact_all().unwrap();

    let copy = open(&cp);
    assert_eq!(copy.iter().unwrap().count(), 1100);
    assert_eq!(copy.get(&key(5)).unwrap().unwrap(), val(5, 0));
    assert_eq!(copy.get(&key(1099)).unwrap().unwrap(), val(1099, 0));
    // and the original kept going
    assert_eq!(db.get(&key(5)).unwrap(), None);
    assert_eq!(db.get(&key(6)).unwrap().unwrap(), val(6, 1));

    // the copy is a real db: it accepts writes without touching the original
    copy.put("extra", "1").unwrap();
    assert_eq!(db.get(b"extra").unwrap(), None);
}

#[test]
fn checkpoint_survives_original_compaction_and_reopen() {
    let d = tempfile::tempdir().unwrap();
    let live = d.path().join("live");
    let cp = d.path().join("cp");
    {
        let db = open(&live);
        for round in 0..4 {
            for i in 0..600 {
                db.put(key(i), val(i, round)).unwrap();
            }
            db.flush().unwrap();
        }
        db.checkpoint(&cp).unwrap();
        for i in 0..600 {
            db.delete(key(i)).unwrap();
        }
        db.compact_all().unwrap();
        db.compact_all().unwrap();
    }
    let copy = open(&cp);
    for i in 0..600 {
        assert_eq!(copy.get(&key(i)).unwrap().unwrap(), val(i, 3));
    }
}

#[test]
fn checkpoint_refuses_nonempty_dir() {
    let d = tempfile::tempdir().unwrap();
    let db = open(&d.path().join("live"));
    db.put("a", "1").unwrap();
    let cp = d.path().join("cp");
    std::fs::create_dir_all(&cp).unwrap();
    std::fs::write(cp.join("junk"), b"x").unwrap();
    assert!(matches!(db.checkpoint(&cp), Err(Error::InvalidArgument(_))));
    // untouched
    assert!(cp.join("junk").exists());

    // an existing empty dir is fine
    let empty = d.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    db.checkpoint(&empty).unwrap();
    assert_eq!(open(&empty).get(b"a").unwrap().unwrap(), b"1");
}

#[test]
fn checkpoint_under_concurrent_writes() {
    let d = tempfile::tempdir().unwrap();
    let db = Arc::new(open(&d.path().join("live")));
    for i in 0..500 {
        db.put(key(i), val(i, 0)).unwrap();
    }
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (db, stop) = (db.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut g = 1;
            while !stop.load(Ordering::Relaxed) {
                for i in 0..500 {
                    db.put(key(i), val(i, g)).unwrap();
                }
                g += 1;
            }
        })
    };
    for n in 0..3 {
        let cp = d.path().join(format!("cp{n}"));
        db.checkpoint(&cp).unwrap();
        let copy = Db::open(&cp, small_opts()).unwrap();
        assert_eq!(copy.iter().unwrap().count(), 500);
    }
    stop.store(true, Ordering::Relaxed);
    writer.join().unwrap();
}
