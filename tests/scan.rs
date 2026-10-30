mod common;
use common::*;

fn collect(it: lsmdb::DbIter) -> Vec<(Vec<u8>, Vec<u8>)> {
    it.map(|r| r.unwrap()).collect()
}

#[test]
fn prefix_scan() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    for k in ["user:1", "user:2", "user:3", "users", "usf", "a", "zz"] {
        db.put(k, "v").unwrap();
    }
    let keys: Vec<_> = collect(db.prefix(b"user:").unwrap()).into_iter().map(|(k, _)| k).collect();
    assert_eq!(keys, vec![b"user:1".to_vec(), b"user:2".to_vec(), b"user:3".to_vec()]);

    // "user" also matches "users", but not "usf"
    assert_eq!(collect(db.prefix(b"user").unwrap()).len(), 4);
    assert_eq!(collect(db.prefix(b"nope").unwrap()).len(), 0);
    assert_eq!(collect(db.prefix(b"").unwrap()).len(), 7);
}

#[test]
fn prefix_with_ff_bytes() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    db.put(b"\xff\xff".to_vec(), "a").unwrap();
    db.put(b"\xff\xff\x01".to_vec(), "b").unwrap();
    db.put(b"\xff\xfe".to_vec(), "c").unwrap();
    assert_eq!(collect(db.prefix(b"\xff\xff").unwrap()).len(), 2);
}

#[test]
fn prefix_across_flush_and_deletes() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    for i in 0..500 {
        db.put(format!("p/{i:04}"), val(i, 0)).unwrap();
        db.put(format!("q/{i:04}"), val(i, 0)).unwrap();
    }
    db.flush().unwrap();
    for i in (0..500).step_by(2) {
        db.delete(format!("p/{i:04}")).unwrap();
    }
    assert_eq!(collect(db.prefix(b"p/").unwrap()).len(), 250);
    db.compact_all().unwrap();
    assert_eq!(collect(db.prefix(b"p/").unwrap()).len(), 250);
    assert_eq!(collect(db.prefix(b"q/").unwrap()).len(), 500);
}

#[test]
fn iterator_seek() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    for i in (0..100).step_by(10) {
        db.put(key(i), val(i, 0)).unwrap();
    }
    db.flush().unwrap();
    db.put(key(55), "mem").unwrap();

    let mut it = db.iter().unwrap();
    it.seek(&key(20));
    assert_eq!(it.key(), &key(20)[..]);
    it.seek(&key(51));
    assert_eq!(it.key(), &key(55)[..]);
    it.seek(&key(90));
    assert_eq!(it.key(), &key(90)[..]);
    it.seek(&key(91));
    assert!(!it.valid());
    // backwards seek works too, it's a fresh positioning
    it.seek(&key(0));
    assert_eq!(it.key(), &key(0)[..]);

    let mut bounded = db.range(None, Some(&key(60))).unwrap();
    bounded.seek(&key(58));
    assert!(!bounded.valid());
}

#[test]
fn first_key_value() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    assert!(db.first_key_value().unwrap().is_none());
    db.put("m", "1").unwrap();
    db.put("b", "2").unwrap();
    db.delete("b").unwrap();
    assert_eq!(db.first_key_value().unwrap().unwrap().0, b"m");
}
