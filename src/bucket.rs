use crate::{
    batch::WriteBatch,
    db::{prefix_end, Db, DbIter},
    error::{Error, Result},
    options::WriteOptions,
};

/// A named keyspace inside one `Db`: every key is stored as `name\0key`, so
/// buckets never see each other and each one scans in its own key order.
/// Bucket names can't contain a NUL byte.
pub struct Bucket<'a> {
    db: &'a Db,
    prefix: Vec<u8>,
}

pub struct BucketBatch<'b> {
    prefix: &'b [u8],
    batch: WriteBatch,
}

impl Db {
    pub fn bucket(&self, name: &str) -> Result<Bucket<'_>> {
        if name.is_empty() || name.contains('\0') {
            return Err(Error::invalid("bucket name must be non-empty and free of NUL bytes"));
        }
        let mut prefix = name.as_bytes().to_vec();
        prefix.push(0);
        Ok(Bucket { db: self, prefix })
    }
}

impl<'a> Bucket<'a> {
    fn full(&self, key: &[u8]) -> Vec<u8> {
        let mut k = Vec::with_capacity(self.prefix.len() + key.len());
        k.extend_from_slice(&self.prefix);
        k.extend_from_slice(key);
        k
    }

    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.db.get(&self.full(key))
    }

    pub fn contains_key(&self, key: &[u8]) -> Result<bool> {
        self.db.contains_key(&self.full(key))
    }

    pub fn put(&self, key: &[u8], value: impl Into<Vec<u8>>) -> Result<()> {
        self.db.put(self.full(key), value)
    }

    pub fn delete(&self, key: &[u8]) -> Result<()> {
        self.db.delete(self.full(key))
    }

    /// atomic multi-key write confined to this bucket
    pub fn write(&self, f: impl FnOnce(&mut BucketBatch<'_>)) -> Result<()> {
        let mut b = BucketBatch { prefix: &self.prefix, batch: WriteBatch::new() };
        f(&mut b);
        self.db.write(b.batch, &WriteOptions::default())
    }

    pub fn iter(&self) -> Result<BucketIter> {
        self.range(None, None)
    }

    /// keys in [lower, upper), relative to the bucket
    pub fn range(&self, lower: Option<&[u8]>, upper: Option<&[u8]>) -> Result<BucketIter> {
        let lo = self.full(lower.unwrap_or(&[]));
        let hi = match upper {
            Some(u) => Some(self.full(u)),
            None => prefix_end(&self.prefix),
        };
        let inner = self.db.range(Some(&lo), hi.as_deref())?;
        Ok(BucketIter { inner, strip: self.prefix.len() })
    }

    /// deletes every key in the bucket and returns how many there were
    pub fn clear(&self) -> Result<u64> {
        self.db.delete_prefix(&self.prefix)
    }
}

impl BucketBatch<'_> {
    pub fn put(&mut self, key: &[u8], value: impl Into<Vec<u8>>) -> &mut Self {
        self.batch.put([self.prefix, key].concat(), value);
        self
    }

    pub fn delete(&mut self, key: &[u8]) -> &mut Self {
        self.batch.delete([self.prefix, key].concat());
        self
    }
}

pub struct BucketIter {
    inner: DbIter,
    strip: usize,
}

impl Iterator for BucketIter {
    type Item = Result<(Vec<u8>, Vec<u8>)>;

    fn next(&mut self) -> Option<Self::Item> {
        let strip = self.strip;
        self.inner.next().map(|r| {
            r.map(|(mut k, v)| {
                k.drain(..strip);
                (k, v)
            })
        })
    }
}
