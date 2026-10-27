mod common;
use common::*;
use lsmdb::{Db, Options, WriteOptions};
use std::{fs, fs::OpenOptions, path::Path};

fn files_with_ext(dir: &Path, ext: &str) -> Vec<std::path::PathBuf> {
    let mut v: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == ext))
        .collect();
    v.sort();
    v
}

#[test]
fn unflushed_writes_come_back_from_wal() {
    let d = tempfile::tempdir().unwrap();
    {
        let db = open(d.path());
        for i in 0..200 {
            db.put(key(i), val(i, 0)).unwrap();
        }
        db.delete(key(5)).unwrap();
        // dropped without flush(): only the WAL has this data
    }
    let db = open(d.path());
    for i in 0..200 {
        let want = if i == 5 { None } else { Some(val(i, 0)) };
        assert_eq!(db.get(&key(i)).unwrap(), want);
    }
}

#[test]
fn data_split_across_wal_and_tables() {
    let d = tempfile::tempdir().unwrap();
    {
        let db = open(d.path());
        for i in 0..2000 {
            db.put(key(i), val(i, 0)).unwrap();
        }
        db.flush().unwrap();
        for i in 0..2000 {
            db.put(key(i), val(i, 1)).unwrap();
        }
    }
    let db = open(d.path());
    for i in 0..2000 {
        assert_eq!(db.get(&key(i)).unwrap().unwrap(), val(i, 1), "key {i}");
    }
}

#[test]
fn many_reopen_cycles_keep_everything() {
    let d = tempfile::tempdir().unwrap();
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let mut model: std::collections::BTreeMap<Vec<u8>, Vec<u8>> = std::collections::BTreeMap::new();
    for round in 0..8 {
        let db = open(d.path());
        for (k, v) in &model {
            assert_eq!(db.get(k).unwrap().as_ref(), Some(v), "round {round}");
        }
        for _ in 0..600 {
            let i = rng.below(400) as u32;
            if rng.below(5) == 0 {
                db.delete(key(i)).unwrap();
                model.remove(&key(i));
            } else {
                let v = val(i, round);
                db.put(key(i), v.clone()).unwrap();
                model.insert(key(i), v);
            }
        }
        if round % 3 == 0 {
            db.compact_all().unwrap();
        }
    }
    let db = open(d.path());
    let got: Vec<_> = db.iter().unwrap().map(|r| r.unwrap()).collect();
    let want: Vec<_> = model.into_iter().collect();
    assert_eq!(got, want);
}

#[test]
fn torn_wal_tail_loses_only_the_last_write() {
    let d = tempfile::tempdir().unwrap();
    {
        let db = Db::open(d.path(), Options::default()).unwrap();
        for i in 0..50 {
            db.put(key(i), val(i, 0)).unwrap();
        }
    }
    let log = files_with_ext(d.path(), "log").pop().unwrap();
    let len = fs::metadata(&log).unwrap().len();
    // simulate power loss in the middle of the last record
    OpenOptions::new().write(true).open(&log).unwrap().set_len(len - 5).unwrap();

    let db = Db::open(d.path(), Options::default()).unwrap();
    for i in 0..49 {
        assert_eq!(db.get(&key(i)).unwrap().unwrap(), val(i, 0), "key {i}");
    }
    assert_eq!(db.get(&key(49)).unwrap(), None);
    // and the db is fully usable afterwards
    db.put(key(49), val(49, 1)).unwrap();
    assert_eq!(db.get(&key(49)).unwrap().unwrap(), val(49, 1));
}

#[test]
fn corrupted_wal_record_stops_replay_at_that_point() {
    let d = tempfile::tempdir().unwrap();
    {
        let db = Db::open(d.path(), Options::default()).unwrap();
        for i in 0..20 {
            db.put(key(i), val(i, 0)).unwrap();
        }
    }
    let log = files_with_ext(d.path(), "log").pop().unwrap();
    let mut bytes = fs::read(&log).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    fs::write(&log, bytes).unwrap();

    let db = Db::open(d.path(), Options::default()).unwrap();
    // prefix property: whatever survives must be a prefix of the write order
    let mut seen_gap = false;
    for i in 0..20 {
        let present = db.get(&key(i)).unwrap().is_some();
        assert!(!(seen_gap && present), "key {i} survived after a gap");
        seen_gap |= !present;
    }
    assert!(seen_gap);
}

#[test]
fn leftover_files_are_swept_on_open() {
    let d = tempfile::tempdir().unwrap();
    {
        let db = open(d.path());
        for i in 0..100 {
            db.put(key(i), val(i, 0)).unwrap();
        }
        db.flush().unwrap();
    }
    fs::write(d.path().join("999999.sst"), b"orphan from a crashed compaction").unwrap();
    fs::write(d.path().join("CURRENT.tmp"), b"junk").unwrap();
    let db = open(d.path());
    assert!(!d.path().join("999999.sst").exists());
    assert!(!d.path().join("CURRENT.tmp").exists());
    assert_eq!(db.get(&key(7)).unwrap().unwrap(), val(7, 0));
}

#[test]
fn corrupt_sstable_is_reported_not_returned() {
    let d = tempfile::tempdir().unwrap();
    {
        let db = open(d.path());
        for i in 0..300 {
            db.put(key(i), val(i, 0)).unwrap();
        }
        db.flush().unwrap();
    }
    let sst = files_with_ext(d.path(), "sst").pop().unwrap();
    let mut bytes = fs::read(&sst).unwrap();
    bytes[10] ^= 0xff;
    fs::write(&sst, bytes).unwrap();

    let db = open(d.path());
    let mut errs = 0;
    for i in 0..300 {
        match db.get(&key(i)) {
            Err(lsmdb::Error::Corruption(_)) => errs += 1,
            Ok(Some(v)) => assert_eq!(v, val(i, 0)),
            Ok(None) => {}
            Err(e) => panic!("unexpected {e}"),
        }
    }
    assert!(errs > 0, "checksum should have caught the flipped byte");
}

#[test]
fn unsynced_writes_still_replay_after_clean_drop() {
    let d = tempfile::tempdir().unwrap();
    {
        let db = open(d.path());
        let wo = WriteOptions { sync: false };
        for i in 0..100 {
            let mut b = lsmdb::WriteBatch::new();
            b.put(key(i), val(i, 0));
            db.write(b, &wo).unwrap();
        }
    }
    let db = open(d.path());
    assert_eq!(db.get(&key(99)).unwrap().unwrap(), val(99, 0));
}
