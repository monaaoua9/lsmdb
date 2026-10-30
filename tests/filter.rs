mod common;
use common::*;
use lsmdb::{CompactionFilter, Db, Options};

fn with_filter(f: impl Fn(&[u8], &[u8]) -> bool + Send + Sync + 'static) -> Options {
    Options { compaction_filter: Some(CompactionFilter::new(f)), ..small_opts() }
}

#[test]
fn filter_removes_matching_entries_on_compaction() {
    let d = tempfile::tempdir().unwrap();
    let db = Db::open(d.path(), with_filter(|_, v| v.starts_with(b"tmp"))).unwrap();
    for i in 0..600 {
        let v = if i % 3 == 0 { format!("tmp-{i}") } else { format!("keep-{i}") };
        db.put(key(i), v).unwrap();
    }
    db.flush().unwrap();
    // flushes don't run the filter
    assert_eq!(db.iter().unwrap().count(), 600);

    db.compact_all().unwrap();
    assert_eq!(db.iter().unwrap().count(), 400);
    assert!(db.get(&key(3)).unwrap().is_none());
    assert!(db.get(&key(4)).unwrap().is_some());
    assert_eq!(db.stats().filtered, 200);
}

#[test]
fn filtered_key_does_not_resurrect_older_value() {
    let d = tempfile::tempdir().unwrap();
    // only the newer value is junk, the one below it is perfectly fine
    let db = Db::open(d.path(), with_filter(|_, v| v == b"junk")).unwrap();
    db.put("k", "good").unwrap();
    db.flush().unwrap();
    db.compact_all().unwrap(); // pushes the good value down a level
    db.put("k", "junk").unwrap();
    db.flush().unwrap();
    assert_eq!(db.get(b"k").unwrap().unwrap(), b"junk");

    // L0 -> L1 sees "junk" while "good" sits deeper: needs a tombstone, not a plain drop
    db.compact_all().unwrap();
    assert_eq!(db.get(b"k").unwrap(), None);
    db.compact_all().unwrap();
    assert_eq!(db.get(b"k").unwrap(), None);
    assert_eq!(db.iter().unwrap().count(), 0);
}

#[test]
fn open_snapshot_pauses_the_filter() {
    let d = tempfile::tempdir().unwrap();
    let db = Db::open(d.path(), with_filter(|_, v| v == b"junk")).unwrap();
    for i in 0..50 {
        db.put(key(i), "junk").unwrap();
    }
    db.flush().unwrap();

    let snap = db.snapshot();
    db.compact_all().unwrap();
    assert_eq!(db.stats().filtered, 0);
    assert_eq!(snap.get(&key(1)).unwrap().unwrap(), b"junk");
    drop(snap);

    // the first batch already sits in the bottom level and isn't revisited;
    // anything written from now on goes through the filter on its way down
    for i in 50..80 {
        db.put(key(i), "junk").unwrap();
    }
    db.flush().unwrap();
    db.compact_all().unwrap();
    assert_eq!(db.stats().filtered, 30);
    assert_eq!(db.iter().unwrap().count(), 50);
}

#[test]
fn filter_sees_user_keys() {
    let d = tempfile::tempdir().unwrap();
    let db = Db::open(d.path(), with_filter(|k, _| k.starts_with(b"cache/"))).unwrap();
    db.put("cache/a", "1").unwrap();
    db.put("data/a", "1").unwrap();
    db.flush().unwrap();
    db.compact_all().unwrap();
    assert!(db.get(b"cache/a").unwrap().is_none());
    assert!(db.get(b"data/a").unwrap().is_some());
}
