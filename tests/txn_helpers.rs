mod common;
use common::*;
use std::sync::Arc;
use std::thread;

#[test]
fn transact_retries_conflicts() {
    let d = tempfile::tempdir().unwrap();
    let db = Arc::new(open(d.path()));
    db.put("n", "0").unwrap();

    let hs: Vec<_> = (0..4)
        .map(|_| {
            let db = db.clone();
            thread::spawn(move || {
                for _ in 0..50 {
                    db.transact(1000, |tx| {
                        let cur: u32 =
                            String::from_utf8(tx.get_for_update(b"n")?.unwrap()).unwrap().parse().unwrap();
                        tx.put("n", (cur + 1).to_string());
                        Ok(())
                    })
                    .unwrap();
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    assert_eq!(db.get(b"n").unwrap().unwrap(), b"200");
}

#[test]
fn transact_gives_up_and_propagates_closure_errors() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    db.put("k", "1").unwrap();

    let mut runs = 0;
    let res = db.transact(2, |tx| {
        runs += 1;
        tx.get_for_update(b"k")?;
        // somebody else always gets in first
        db.put("k", runs.to_string()).unwrap();
        tx.put("k", "mine");
        Ok(())
    });
    assert!(res.unwrap_err().is_conflict());
    assert_eq!(runs, 3);

    let res: lsmdb::Result<()> = db.transact(5, |_| Err(lsmdb::Error::Locked));
    assert!(matches!(res, Err(lsmdb::Error::Locked)));
}

#[test]
fn compare_and_swap() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    assert!(db.compare_and_swap(b"k", None, Some(b"a".to_vec())).unwrap());
    assert!(!db.compare_and_swap(b"k", None, Some(b"b".to_vec())).unwrap());
    assert!(!db.compare_and_swap(b"k", Some(b"zzz"), Some(b"b".to_vec())).unwrap());
    assert!(db.compare_and_swap(b"k", Some(b"a"), Some(b"b".to_vec())).unwrap());
    assert_eq!(db.get(b"k").unwrap().unwrap(), b"b");
    assert!(db.compare_and_swap(b"k", Some(b"b"), None).unwrap());
    assert_eq!(db.get(b"k").unwrap(), None);
}

#[test]
fn cas_has_a_single_winner() {
    let d = tempfile::tempdir().unwrap();
    let db = Arc::new(open(d.path()));
    let wins: usize = (0..8)
        .map(|i| {
            let db = db.clone();
            thread::spawn(move || db.compare_and_swap(b"lock", None, Some(vec![i])).unwrap())
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| h.join().unwrap() as usize)
        .sum();
    assert_eq!(wins, 1);
}

#[test]
fn txn_range_overlays_local_writes() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    for i in 0..10 {
        db.put(key(i), "base").unwrap();
    }
    db.flush().unwrap();

    let mut tx = db.begin();
    tx.put(key(3), "mine");
    tx.delete(key(4));
    tx.put(key(100), "new");
    tx.put(key(5), "mine5");
    // not visible to the transaction, committed after it began
    db.put(key(6), "later").unwrap();

    let got: Vec<_> = tx.range(None, None).unwrap().map(|r| r.unwrap()).collect();
    let keys: Vec<_> = got.iter().map(|(k, _)| k.clone()).collect();
    assert_eq!(keys.len(), 10);
    assert!(!keys.contains(&key(4)));
    assert_eq!(keys.last().unwrap(), &key(100));
    assert_eq!(got.iter().find(|(k, _)| *k == key(3)).unwrap().1, b"mine");
    assert_eq!(got.iter().find(|(k, _)| *k == key(6)).unwrap().1, b"base");

    let sub: Vec<_> = tx.range(Some(&key(3)), Some(&key(6))).unwrap().map(|r| r.unwrap().0).collect();
    assert_eq!(sub, vec![key(3), key(5)]);

    assert_eq!(tx.range(Some(&key(9)), Some(&key(2))).unwrap().count(), 0);
    tx.rollback();
    assert_eq!(db.get(&key(3)).unwrap().unwrap(), b"base");
}

#[test]
fn increment_counts_under_contention() {
    let d = tempfile::tempdir().unwrap();
    let db = Arc::new(open(d.path()));
    assert_eq!(db.increment(b"c", 5).unwrap(), 5);
    let hs: Vec<_> = (0..4)
        .map(|_| {
            let db = db.clone();
            thread::spawn(move || {
                for _ in 0..100 {
                    db.increment(b"c", 1).unwrap();
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    assert_eq!(db.increment(b"c", -5).unwrap(), 400);
    assert_eq!(db.increment(b"fresh", -2).unwrap(), -2);

    db.put("text", "not a counter").unwrap();
    assert!(matches!(db.increment(b"text", 1), Err(lsmdb::Error::InvalidArgument(_))));
}
