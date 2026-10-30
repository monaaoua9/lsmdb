mod common;
use common::*;
use lsmdb::{Db, Error, Options};
use std::{collections::BTreeMap, fs, path::Path};

fn ro() -> Options {
    Options { read_only: true, ..small_opts() }
}

// name -> (len, modified); enough to notice any write to the directory
fn fingerprint(dir: &Path) -> BTreeMap<String, (u64, std::time::SystemTime)> {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            let m = e.metadata().unwrap();
            (e.file_name().to_string_lossy().into_owned(), (m.len(), m.modified().unwrap()))
        })
        .collect()
}

#[test]
fn reads_tables_and_unflushed_log() {
    let d = tempfile::tempdir().unwrap();
    {
        let db = open(d.path());
        for i in 0..300 {
            db.put(key(i), val(i, 0)).unwrap();
        }
        db.flush().unwrap();
        // these only exist in the wal
        for i in 300..350 {
            db.put(key(i), val(i, 0)).unwrap();
        }
        db.delete(key(7)).unwrap();
    }
    let before = fingerprint(d.path());
    {
        let db = Db::open(d.path(), ro()).unwrap();
        assert_eq!(db.get(&key(10)).unwrap().unwrap(), val(10, 0));
        assert_eq!(db.get(&key(340)).unwrap().unwrap(), val(340, 0));
        assert!(db.get(&key(7)).unwrap().is_none());
        assert_eq!(db.iter().unwrap().count(), 349);
        assert_eq!(db.iter().unwrap().rev().count(), 349);
        assert_eq!(db.prefix(b"key-0000003").unwrap().count(), 10);
        assert!(db.verify_integrity().is_ok());
    }
    assert_eq!(fingerprint(d.path()), before, "read-only open touched the directory");
}

#[test]
fn writes_are_rejected() {
    let d = tempfile::tempdir().unwrap();
    open(d.path()).put("a", "1").unwrap();
    let db = Db::open(d.path(), ro()).unwrap();
    assert!(matches!(db.put("b", "2"), Err(Error::ReadOnly)));
    assert!(matches!(db.delete("a"), Err(Error::ReadOnly)));
    assert!(matches!(db.flush(), Err(Error::ReadOnly)));
    assert!(matches!(db.compact_all(), Err(Error::ReadOnly)));
    assert!(matches!(db.checkpoint(d.path().join("cp")), Err(Error::ReadOnly)));
    assert!(matches!(db.begin().commit(), Ok(())));
    let mut tx = db.begin();
    tx.put("x", "y");
    assert!(matches!(tx.commit(), Err(Error::ReadOnly)));
    assert!(!d.path().join("cp").exists());
    assert_eq!(db.get(b"a").unwrap().unwrap(), b"1");
}

#[test]
fn readers_share_but_exclude_writers() {
    let d = tempfile::tempdir().unwrap();
    open(d.path()).put("a", "1").unwrap();

    let r1 = Db::open(d.path(), ro()).unwrap();
    let r2 = Db::open(d.path(), ro()).unwrap();
    assert!(matches!(Db::open(d.path(), small_opts()), Err(Error::Locked)));
    drop((r1, r2));

    let w = open(d.path());
    assert!(matches!(Db::open(d.path(), ro()), Err(Error::Locked)));
    drop(w);
    assert!(Db::open(d.path(), ro()).is_ok());
}

#[test]
fn missing_database_is_an_error() {
    let d = tempfile::tempdir().unwrap();
    assert!(Db::open(d.path().join("nope"), ro()).is_err());
    // empty directory: nothing to read, and read-only must not create anything
    assert!(Db::open(d.path(), ro()).is_err());
    assert_eq!(fs::read_dir(d.path()).unwrap().count(), 0);
}

#[test]
fn snapshots_work_on_read_only() {
    let d = tempfile::tempdir().unwrap();
    {
        let db = open(d.path());
        db.put("k", "v").unwrap();
        db.flush().unwrap();
    }
    let db = Db::open(d.path(), ro()).unwrap();
    let s = db.snapshot();
    assert_eq!(s.get(b"k").unwrap().unwrap(), b"v");
    assert_eq!(s.range(None, None).unwrap().count(), 1);
}
