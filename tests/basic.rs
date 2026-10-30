mod common;
use common::*;
use lsmdb::{Db, Error, Options, WriteBatch, WriteOptions};

#[test]
fn put_get_delete() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    assert_eq!(db.get(b"a").unwrap(), None);
    db.put("a", "1").unwrap();
    db.put("b", "2").unwrap();
    assert_eq!(db.get(b"a").unwrap().unwrap(), b"1");
    db.put("a", "3").unwrap();
    assert_eq!(db.get(b"a").unwrap().unwrap(), b"3");
    db.delete("a").unwrap();
    assert_eq!(db.get(b"a").unwrap(), None);
    assert_eq!(db.get(b"b").unwrap().unwrap(), b"2");
}

#[test]
fn empty_key_and_value() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    db.put("", "").unwrap();
    assert_eq!(db.get(b"").unwrap().unwrap(), b"");
}

#[test]
fn batch_is_atomic_and_ordered() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    let mut b = WriteBatch::new();
    b.put("k", "first").put("k", "second").put("x", "1").delete("x");
    db.write(b, &WriteOptions::default()).unwrap();
    assert_eq!(db.get(b"k").unwrap().unwrap(), b"second");
    assert_eq!(db.get(b"x").unwrap(), None);
}

#[test]
fn oversized_key_rejected() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    let r = db.put(vec![0u8; (1 << 20) + 1], "v");
    assert!(matches!(r, Err(Error::InvalidArgument(_))));
}

#[test]
fn reads_survive_flush_and_compaction() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    for i in 0..3000 {
        db.put(key(i), val(i, 0)).unwrap();
    }
    for i in (0..3000).step_by(3) {
        db.put(key(i), val(i, 1)).unwrap();
    }
    for i in (0..3000).step_by(7) {
        db.delete(key(i)).unwrap();
    }
    db.flush().unwrap();
    for i in 0..3000 {
        let want = if i % 7 == 0 {
            None
        } else if i % 3 == 0 {
            Some(val(i, 1))
        } else {
            Some(val(i, 0))
        };
        assert_eq!(db.get(&key(i)).unwrap(), want, "key {i}");
    }
    db.compact_all().unwrap();
    for i in 0..3000 {
        let want = if i % 7 == 0 {
            None
        } else if i % 3 == 0 {
            Some(val(i, 1))
        } else {
            Some(val(i, 0))
        };
        assert_eq!(db.get(&key(i)).unwrap(), want, "after compaction, key {i}");
    }
    let st = db.stats();
    assert!(st.files_per_level.iter().sum::<usize>() > 0);
    assert_eq!(st.files_per_level[0], 0);
}

#[test]
fn scan_is_sorted_and_hides_deleted() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    for i in (0..1000).rev() {
        db.put(key(i), val(i, 0)).unwrap();
    }
    for i in (0..1000).step_by(2) {
        db.delete(key(i)).unwrap();
    }
    db.flush().unwrap();
    for i in 1000..1100 {
        db.put(key(i), val(i, 0)).unwrap();
    }

    let got: Vec<_> = db.iter().unwrap().map(|r| r.unwrap().0).collect();
    let want: Vec<_> = (0..1100).filter(|i| *i >= 1000 || i % 2 == 1).map(key).collect();
    assert_eq!(got, want);
}

#[test]
fn range_scan_bounds() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    for i in 0..500 {
        db.put(key(i), val(i, 0)).unwrap();
    }
    db.flush().unwrap();
    let got: Vec<_> = db.range(Some(&key(100)), Some(&key(110))).unwrap().map(|r| r.unwrap().0).collect();
    assert_eq!(got, (100..110).map(key).collect::<Vec<_>>());
    assert_eq!(db.range(Some(&key(9999)), None).unwrap().count(), 0);
}

#[test]
fn bloom_filters_skip_absent_keys() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    for i in 0..2000 {
        db.put(key(i * 2), val(i, 0)).unwrap();
    }
    db.compact_all().unwrap();
    let before = db.stats();
    for i in 0..2000 {
        assert_eq!(db.get(&key(i * 2 + 1)).unwrap(), None);
    }
    let after = db.stats();
    let checked = after.bloom_checked - before.bloom_checked;
    let skipped = after.bloom_skipped - before.bloom_skipped;
    assert!(checked > 0);
    assert!(skipped * 100 >= checked * 90, "skipped {skipped} of {checked}");
    // the whole point: skipped probes never touch a data block
    assert!(after.block_reads - before.block_reads <= (checked - skipped));
}

#[test]
fn second_open_is_locked() {
    let d = tempfile::tempdir().unwrap();
    let _db = open(d.path());
    assert!(matches!(Db::open(d.path(), small_opts()), Err(Error::Locked)));
}

#[test]
fn open_flags() {
    let d = tempfile::tempdir().unwrap();
    let missing = d.path().join("nope");
    let r = Db::open(&missing, Options { create_if_missing: false, ..Options::default() });
    assert!(r.is_err());

    {
        let db = open(d.path());
        db.put("a", "b").unwrap();
    }
    let r = Db::open(d.path(), Options { error_if_exists: true, ..small_opts() });
    assert!(r.is_err());
}

#[test]
fn big_values() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    let big = vec![0xabu8; 3 << 20];
    db.put("big", big.clone()).unwrap();
    db.put("small", "s").unwrap();
    db.flush().unwrap();
    assert_eq!(db.get(b"big").unwrap().unwrap(), big);
    assert_eq!(db.get(b"small").unwrap().unwrap(), b"s");
}

#[test]
fn contains_and_get_many() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    db.put("a", "1").unwrap();
    db.put("c", "3").unwrap();
    db.delete("c").unwrap();
    assert!(db.contains_key(b"a").unwrap());
    assert!(!db.contains_key(b"c").unwrap());

    let got = db.get_many(&[b"a", b"b", b"c"]).unwrap();
    assert_eq!(got, vec![Some(b"1".to_vec()), None, None]);

    let snap = db.snapshot();
    db.put("a", "2").unwrap();
    assert_eq!(snap.get_many(&[b"a"]).unwrap()[0].as_deref(), Some(&b"1"[..]));
    assert!(snap.contains_key(b"a").unwrap());
    assert_eq!(db.path(), d.path());
}
