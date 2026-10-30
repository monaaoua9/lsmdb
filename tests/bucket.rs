mod common;
use common::*;
use lsmdb::Error;

#[test]
fn buckets_are_isolated() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    let users = db.bucket("users").unwrap();
    let posts = db.bucket("posts").unwrap();
    users.put(b"1", "ada").unwrap();
    users.put(b"2", "grace").unwrap();
    posts.put(b"1", "hello").unwrap();

    assert_eq!(users.get(b"1").unwrap().unwrap(), b"ada");
    assert_eq!(posts.get(b"1").unwrap().unwrap(), b"hello");
    assert!(posts.get(b"2").unwrap().is_none());
    assert!(users.contains_key(b"2").unwrap());

    let all: Vec<_> = users.iter().unwrap().map(|r| r.unwrap()).collect();
    assert_eq!(all, vec![(b"1".to_vec(), b"ada".to_vec()), (b"2".to_vec(), b"grace".to_vec())]);
    assert_eq!(posts.iter().unwrap().count(), 1);
    // the raw db sees the prefixed keys
    assert_eq!(db.iter().unwrap().count(), 3);
}

#[test]
fn similar_names_dont_leak() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    let a = db.bucket("a").unwrap();
    let ab = db.bucket("ab").unwrap();
    a.put(b"x", "1").unwrap();
    ab.put(b"x", "2").unwrap();
    a.put(b"\xff\xff", "3").unwrap();
    assert_eq!(a.iter().unwrap().count(), 2);
    assert_eq!(ab.iter().unwrap().count(), 1);
}

#[test]
fn range_clear_and_batch() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    let b = db.bucket("b").unwrap();
    let other = db.bucket("c").unwrap();
    other.put(b"keep", "me").unwrap();

    b.write(|w| {
        for i in 0..10u8 {
            w.put(&[i], vec![i]);
        }
        w.delete(&[3]);
    })
    .unwrap();
    assert!(b.get(&[3]).unwrap().is_none());

    let mid: Vec<_> = b.range(Some(&[2]), Some(&[6])).unwrap().map(|r| r.unwrap().0).collect();
    assert_eq!(mid, vec![vec![2], vec![4], vec![5]]);
    assert_eq!(b.range(Some(&[8]), None).unwrap().count(), 2);

    assert_eq!(b.clear().unwrap(), 9);
    assert_eq!(b.iter().unwrap().count(), 0);
    assert_eq!(other.get(b"keep").unwrap().unwrap(), b"me");
}

#[test]
fn bad_names_rejected() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    assert!(matches!(db.bucket(""), Err(Error::InvalidArgument(_))));
    assert!(matches!(db.bucket("a\0b"), Err(Error::InvalidArgument(_))));
}
