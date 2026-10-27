use crate::{
    coding::*,
    error::{Error, Result},
    types::{SeqNo, ValueKind},
};

pub(crate) const MAX_KEY_LEN: usize = 1 << 20;
pub(crate) const MAX_VALUE_LEN: usize = 256 << 20;

pub(crate) struct Op {
    pub kind: ValueKind,
    pub key: Vec<u8>,
    pub value: Vec<u8>,
}

/// atomic group of writes, applied under consecutive sequence numbers
#[derive(Default)]
pub struct WriteBatch {
    pub(crate) ops: Vec<Op>,
    bytes: usize,
}

impl WriteBatch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&mut self, key: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> &mut Self {
        let (key, value) = (key.into(), value.into());
        self.bytes += key.len() + value.len();
        self.ops.push(Op { kind: ValueKind::Put, key, value });
        self
    }

    pub fn delete(&mut self, key: impl Into<Vec<u8>>) -> &mut Self {
        let key = key.into();
        self.bytes += key.len();
        self.ops.push(Op { kind: ValueKind::Delete, key, value: Vec::new() });
        self
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    pub fn clear(&mut self) {
        self.ops.clear();
        self.bytes = 0;
    }

    pub(crate) fn byte_size(&self) -> usize {
        self.bytes
    }

    pub(crate) fn validate(&self) -> Result<()> {
        for op in &self.ops {
            if op.key.len() > MAX_KEY_LEN {
                return Err(Error::invalid("key too large"));
            }
            if op.value.len() > MAX_VALUE_LEN {
                return Err(Error::invalid("value too large"));
            }
        }
        Ok(())
    }
}

// WAL record: first_seq u64 | count u32 | ops
// op: kind u8 | key (len-prefixed) | value (len-prefixed, puts only)
pub(crate) const RECORD_HEADER: usize = 12;

pub(crate) fn encode_op(buf: &mut Vec<u8>, kind: ValueKind, key: &[u8], value: &[u8]) {
    buf.push(kind as u8);
    put_bytes(buf, key);
    if kind == ValueKind::Put {
        put_bytes(buf, value);
    }
}

/// returns the highest sequence number in the record
pub(crate) fn replay_record(rec: &[u8], mut f: impl FnMut(SeqNo, ValueKind, &[u8], &[u8])) -> Result<SeqNo> {
    if rec.len() < RECORD_HEADER {
        return Err(Error::corruption("short wal record"));
    }
    let first = read_fixed64(rec);
    let count = read_fixed32(&rec[8..]);
    let mut src = &rec[RECORD_HEADER..];
    for i in 0..count as u64 {
        let (&kb, rest) = src.split_first().ok_or_else(|| Error::corruption("wal op truncated"))?;
        src = rest;
        let kind = ValueKind::from_u8(kb).ok_or_else(|| Error::corruption("bad op kind"))?;
        let key = get_bytes(&mut src).ok_or_else(|| Error::corruption("bad wal key"))?;
        let value = if kind == ValueKind::Put {
            get_bytes(&mut src).ok_or_else(|| Error::corruption("bad wal value"))?
        } else {
            &[][..]
        };
        f(first + i, kind, key, value);
    }
    Ok(first + (count as u64).saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_roundtrip() {
        let mut rec = Vec::new();
        put_fixed64(&mut rec, 10);
        put_fixed32(&mut rec, 2);
        encode_op(&mut rec, ValueKind::Put, b"a", b"1");
        encode_op(&mut rec, ValueKind::Delete, b"b", b"");
        let mut seen = Vec::new();
        let last = replay_record(&rec, |s, k, key, v| seen.push((s, k, key.to_vec(), v.to_vec()))).unwrap();
        assert_eq!(last, 11);
        assert_eq!(seen[0], (10, ValueKind::Put, b"a".to_vec(), b"1".to_vec()));
        assert_eq!(seen[1], (11, ValueKind::Delete, b"b".to_vec(), vec![]));
    }

    #[test]
    fn truncated_record_is_error() {
        let mut rec = Vec::new();
        put_fixed64(&mut rec, 1);
        put_fixed32(&mut rec, 1);
        rec.push(1);
        assert!(replay_record(&rec, |_, _, _, _| {}).is_err());
    }
}
