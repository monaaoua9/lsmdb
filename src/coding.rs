pub fn put_varint64(buf: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        buf.push((v as u8) | 0x80);
        v >>= 7;
    }
    buf.push(v as u8);
}

pub fn put_varint32(buf: &mut Vec<u8>, v: u32) {
    put_varint64(buf, v as u64);
}

pub fn get_varint64(src: &[u8]) -> Option<(u64, usize)> {
    let mut r = 0u64;
    let mut shift = 0;
    for (i, &b) in src.iter().enumerate() {
        if shift > 63 {
            return None;
        }
        r |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Some((r, i + 1));
        }
        shift += 7;
    }
    None
}

pub fn get_varint32(src: &[u8]) -> Option<(u32, usize)> {
    let (v, n) = get_varint64(src)?;
    Some((u32::try_from(v).ok()?, n))
}

pub fn put_fixed32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

pub fn put_fixed64(buf: &mut Vec<u8>, v: u64) {
    buf.extend_from_slice(&v.to_le_bytes());
}

pub fn read_fixed32(src: &[u8]) -> u32 {
    u32::from_le_bytes(src[..4].try_into().unwrap())
}

pub fn read_fixed64(src: &[u8]) -> u64 {
    u64::from_le_bytes(src[..8].try_into().unwrap())
}

pub fn put_bytes(buf: &mut Vec<u8>, b: &[u8]) {
    put_varint32(buf, b.len() as u32);
    buf.extend_from_slice(b);
}

/// pops a varint-length-prefixed slice off the front of `src`
pub fn get_bytes<'a>(src: &mut &'a [u8]) -> Option<&'a [u8]> {
    let (len, n) = get_varint32(src)?;
    let len = len as usize;
    if src.len() < n + len {
        return None;
    }
    let out = &src[n..n + len];
    *src = &src[n + len..];
    Some(out)
}

const CRC_MASK_DELTA: u32 = 0xa282_ead8;

// masked so that a crc stored inside crc'd data doesn't degrade the checksum
pub fn crc_masked(parts: &[&[u8]]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().rotate_right(15).wrapping_add(CRC_MASK_DELTA)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_roundtrip() {
        for v in [0u64, 1, 127, 128, 300, u32::MAX as u64, u64::MAX] {
            let mut b = Vec::new();
            put_varint64(&mut b, v);
            assert_eq!(get_varint64(&b), Some((v, b.len())));
        }
    }

    #[test]
    fn truncated_varint() {
        assert_eq!(get_varint64(&[0x80, 0x80]), None);
    }

    #[test]
    fn bytes_roundtrip() {
        let mut b = Vec::new();
        put_bytes(&mut b, b"abc");
        put_bytes(&mut b, b"");
        let mut s = &b[..];
        assert_eq!(get_bytes(&mut s), Some(&b"abc"[..]));
        assert_eq!(get_bytes(&mut s), Some(&b""[..]));
        assert!(s.is_empty());
    }
}
