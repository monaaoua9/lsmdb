mod common;
use common::*;
use lsmdb::{Compression, Db, Options};
use std::{fs, path::Path};

fn sst_bytes(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "sst"))
        .map(|e| e.metadata().unwrap().len())
        .sum()
}

fn fill(db: &Db, n: u32) {
    for i in 0..n {
        db.put(key(i), val(i, 0)).unwrap();
    }
    db.flush().unwrap();
    db.compact_all().unwrap();
}

#[test]
fn lz4_roundtrip_and_smaller() {
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let plain = Options { compression: Compression::None, ..small_opts() };
    let packed = Options { compression: Compression::Lz4, ..small_opts() };

    let (dp, dl) = (Db::open(a.path(), plain).unwrap(), Db::open(b.path(), packed).unwrap());
    fill(&dp, 3000);
    fill(&dl, 3000);

    for i in (0..3000).step_by(7) {
        assert_eq!(dl.get(&key(i)).unwrap().unwrap(), val(i, 0));
    }
    assert_eq!(dl.iter().unwrap().count(), 3000);
    assert!(sst_bytes(b.path()) < sst_bytes(a.path()) * 3 / 4);
}

#[test]
fn mixed_tables_survive_option_change() {
    let d = tempfile::tempdir().unwrap();
    {
        let db = Db::open(d.path(), Options { compression: Compression::None, ..small_opts() }).unwrap();
        fill(&db, 800);
    }
    let db = Db::open(d.path(), Options { compression: Compression::Lz4, ..small_opts() }).unwrap();
    for i in 800..1600 {
        db.put(key(i), val(i, 1)).unwrap();
    }
    db.flush().unwrap();
    db.compact_all().unwrap();
    drop(db);

    let db = open(d.path());
    for i in (0..1600).step_by(3) {
        let want = if i < 800 { val(i, 0) } else { val(i, 1) };
        assert_eq!(db.get(&key(i)).unwrap().unwrap(), want);
    }
}

#[test]
fn incompressible_blocks_stay_raw_and_readable() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    let mut r = Rng(0x9e3779b97f4a7c15);
    let mut vals = Vec::new();
    for i in 0..400u32 {
        let v: Vec<u8> = (0..64).map(|_| r.below(256) as u8).collect();
        db.put(key(i), v.clone()).unwrap();
        vals.push(v);
    }
    db.flush().unwrap();
    for (i, v) in vals.iter().enumerate() {
        assert_eq!(&db.get(&key(i as u32)).unwrap().unwrap(), v);
    }
}

#[test]
fn corrupt_compressed_block_is_an_error() {
    let d = tempfile::tempdir().unwrap();
    {
        let db = open(d.path());
        fill(&db, 500);
    }
    for e in fs::read_dir(d.path()).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "sst") {
            let mut b = fs::read(&p).unwrap();
            b[10] ^= 0xff;
            fs::write(&p, b).unwrap();
        }
    }
    let db = open(d.path());
    let mut saw_err = false;
    for i in 0..500 {
        match db.get(&key(i)) {
            Ok(Some(v)) => assert_eq!(v, val(i, 0)),
            Ok(None) => {}
            Err(_) => saw_err = true,
        }
    }
    assert!(saw_err);
}
