mod common;
use common::*;
use lsmdb::{Db, Error, Options};
use std::fs;

fn table_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut v: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "sst"))
        .collect();
    v.sort();
    v
}

#[test]
fn clean_db_verifies() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    let empty = db.verify_integrity().unwrap();
    assert_eq!((empty.tables, empty.entries), (0, 0));

    for round in 0..3 {
        for i in 0..800 {
            db.put(key(i), val(i, round)).unwrap();
        }
        db.flush().unwrap();
    }
    db.delete(key(1)).unwrap();
    db.flush().unwrap();
    let r = db.verify_integrity().unwrap();
    assert!(r.tables >= 2);
    // background compaction may already have merged some of the versions
    assert!(r.entries > 799 && r.entries <= 3 * 800 + 1);
    assert_eq!(r.bytes, db.stats().bytes_per_level.iter().sum::<u64>());

    db.compact_all().unwrap();
    let r = db.verify_integrity().unwrap();
    assert_eq!(r.entries, 799);
}

#[test]
fn flipped_byte_is_found() {
    let d = tempfile::tempdir().unwrap();
    {
        let db = open(d.path());
        for i in 0..500 {
            db.put(key(i), val(i, 0)).unwrap();
        }
        db.flush().unwrap();
    }
    let t = &table_files(d.path())[0];
    let mut bytes = fs::read(t).unwrap();
    bytes[40] ^= 0x55;
    fs::write(t, bytes).unwrap();

    // reads skip checksums here, so the damage is only caught by the explicit scan
    let db = Db::open(d.path(), Options { verify_checksums: false, ..small_opts() }).unwrap();
    assert!(matches!(db.verify_integrity(), Err(Error::Corruption(_))));
}

#[test]
fn truncated_table_is_found() {
    let d = tempfile::tempdir().unwrap();
    {
        let db = open(d.path());
        for i in 0..500 {
            db.put(key(i), val(i, 0)).unwrap();
        }
        db.flush().unwrap();
    }
    let t = &table_files(d.path())[0];
    let f = fs::OpenOptions::new().write(true).open(t).unwrap();
    let len = f.metadata().unwrap().len();
    f.set_len(len - 7).unwrap();
    drop(f);

    let db = Db::open(d.path(), small_opts()).unwrap();
    assert!(db.verify_integrity().is_err());
}
