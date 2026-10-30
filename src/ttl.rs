use std::{
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
    db::{Db, DbIter},
    error::{Error, Result},
    options::{CompactionFilter, Options},
};

const NEVER: u64 = u64::MAX;

type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

fn wall_clock_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// A `Db` whose entries can expire. Every value is stored behind an 8 byte
/// deadline (unix millis); reads hide expired entries right away and compaction
/// reclaims the space later. Don't open the same directory as a plain `Db`,
/// the values would show up with the deadline still attached.
pub struct TtlDb {
    db: Db,
    clock: Clock,
}

fn encode(deadline: u64, value: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + value.len());
    v.extend_from_slice(&deadline.to_le_bytes());
    v.extend_from_slice(value);
    v
}

fn split(raw: &[u8]) -> Result<(u64, &[u8])> {
    if raw.len() < 8 {
        return Err(Error::Corruption("ttl value shorter than its header".into()));
    }
    Ok((u64::from_le_bytes(raw[..8].try_into().unwrap()), &raw[8..]))
}

impl TtlDb {
    pub fn open(path: impl AsRef<Path>, opts: Options) -> Result<TtlDb> {
        Self::open_with_clock(path, opts, Arc::new(wall_clock_ms))
    }

    /// `clock` returns the current time in unix milliseconds
    pub fn open_with_clock(path: impl AsRef<Path>, mut opts: Options, clock: Clock) -> Result<TtlDb> {
        let user = opts.compaction_filter.take();
        let c = clock.clone();
        opts.compaction_filter = Some(CompactionFilter::new(move |k, raw| match split(raw) {
            Ok((deadline, payload)) => {
                deadline <= c() || user.as_ref().is_some_and(|f| f.should_remove(k, payload))
            }
            Err(_) => false,
        }));
        Ok(TtlDb { db: Db::open(path, opts)?, clock })
    }

    pub fn put(&self, key: impl Into<Vec<u8>>, value: &[u8], ttl: Duration) -> Result<()> {
        let deadline = (self.clock)().saturating_add(ttl.as_millis().min(u64::MAX as u128 - 1) as u64);
        self.db.put(key, encode(deadline, value))
    }

    pub fn put_forever(&self, key: impl Into<Vec<u8>>, value: &[u8]) -> Result<()> {
        self.db.put(key, encode(NEVER, value))
    }

    pub fn delete(&self, key: impl Into<Vec<u8>>) -> Result<()> {
        self.db.delete(key)
    }

    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let Some(raw) = self.db.get(key)? else { return Ok(None) };
        let (deadline, payload) = split(&raw)?;
        Ok((deadline > (self.clock)()).then(|| payload.to_vec()))
    }

    pub fn iter(&self) -> Result<TtlIter> {
        Ok(TtlIter { inner: self.db.iter()?, now: (self.clock)() })
    }

    /// the underlying db, for flush/compaction/stats. values read through it carry the deadline header
    pub fn db(&self) -> &Db {
        &self.db
    }
}

/// live entries only, as of the moment the iterator was created
pub struct TtlIter {
    inner: DbIter,
    now: u64,
}

impl Iterator for TtlIter {
    type Item = Result<(Vec<u8>, Vec<u8>)>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (k, raw) = match self.inner.next()? {
                Ok(kv) => kv,
                Err(e) => return Some(Err(e)),
            };
            match split(&raw) {
                Ok((deadline, payload)) if deadline > self.now => return Some(Ok((k, payload.to_vec()))),
                Ok(_) => continue,
                Err(e) => return Some(Err(e)),
            }
        }
    }
}
