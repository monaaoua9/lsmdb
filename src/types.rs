use std::cmp::Ordering;

pub type SeqNo = u64;

/// seq shares a u64 with the kind byte, so it has 56 bits
pub const MAX_SEQ: SeqNo = (1 << 56) - 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ValueKind {
    Delete = 0,
    Put = 1,
}

impl ValueKind {
    pub fn from_u8(b: u8) -> Option<Self> {
        match b {
            0 => Some(ValueKind::Delete),
            1 => Some(ValueKind::Put),
            _ => None,
        }
    }
}

/// result of a point lookup in one component; value == None is a tombstone
pub struct Hit {
    pub seq: SeqNo,
    pub value: Option<Vec<u8>>,
}

// internal key = user key ++ le64(seq << 8 | kind)
// order: user key asc, then newest first
pub fn make_ikey(ukey: &[u8], seq: SeqNo, kind: ValueKind) -> Vec<u8> {
    let mut v = Vec::with_capacity(ukey.len() + 8);
    v.extend_from_slice(ukey);
    v.extend_from_slice(&((seq << 8) | kind as u64).to_le_bytes());
    v
}

#[inline]
pub fn user_key(ik: &[u8]) -> &[u8] {
    &ik[..ik.len() - 8]
}

#[inline]
fn trailer(ik: &[u8]) -> u64 {
    u64::from_le_bytes(ik[ik.len() - 8..].try_into().unwrap())
}

pub fn parse_ikey(ik: &[u8]) -> Option<(&[u8], SeqNo, ValueKind)> {
    if ik.len() < 8 {
        return None;
    }
    let t = trailer(ik);
    let kind = ValueKind::from_u8((t & 0xff) as u8)?;
    Some((user_key(ik), t >> 8, kind))
}

#[inline]
pub fn cmp_ikey(a: &[u8], b: &[u8]) -> Ordering {
    user_key(a).cmp(user_key(b)).then_with(|| trailer(b).cmp(&trailer(a)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_sorts_first() {
        let a = make_ikey(b"k", 5, ValueKind::Put);
        let b = make_ikey(b"k", 3, ValueKind::Put);
        assert_eq!(cmp_ikey(&a, &b), Ordering::Less);
    }

    #[test]
    fn user_key_dominates() {
        let a = make_ikey(b"a", 1, ValueKind::Put);
        let b = make_ikey(b"b", 99, ValueKind::Put);
        assert_eq!(cmp_ikey(&a, &b), Ordering::Less);
    }

    #[test]
    fn seek_key_precedes_same_seq_entries() {
        let seek = make_ikey(b"k", 7, ValueKind::Put);
        let del = make_ikey(b"k", 7, ValueKind::Delete);
        assert_ne!(cmp_ikey(&seek, &del), Ordering::Greater);
    }

    #[test]
    fn parse_roundtrip() {
        let ik = make_ikey(b"hello", 42, ValueKind::Delete);
        let (u, s, k) = parse_ikey(&ik).unwrap();
        assert_eq!((u, s, k), (&b"hello"[..], 42, ValueKind::Delete));
        assert!(parse_ikey(b"short").is_none());
    }
}
