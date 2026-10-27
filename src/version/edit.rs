use crate::{
    coding::*,
    error::{Error, Result},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewFile {
    pub number: u64,
    pub size: u64,
    pub smallest: Vec<u8>,
    pub largest: Vec<u8>,
}

#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct VersionEdit {
    pub log_number: Option<u64>,
    pub next_file: Option<u64>,
    pub last_seq: Option<u64>,
    pub deleted: Vec<(usize, u64)>,
    pub added: Vec<(usize, NewFile)>,
}

const TAG_LOG: u32 = 1;
const TAG_NEXT_FILE: u32 = 2;
const TAG_LAST_SEQ: u32 = 3;
const TAG_DELETE: u32 = 4;
const TAG_ADD: u32 = 5;

impl VersionEdit {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        for (tag, v) in
            [(TAG_LOG, self.log_number), (TAG_NEXT_FILE, self.next_file), (TAG_LAST_SEQ, self.last_seq)]
        {
            if let Some(v) = v {
                put_varint32(&mut b, tag);
                put_varint64(&mut b, v);
            }
        }
        for (level, num) in &self.deleted {
            put_varint32(&mut b, TAG_DELETE);
            put_varint32(&mut b, *level as u32);
            put_varint64(&mut b, *num);
        }
        for (level, f) in &self.added {
            put_varint32(&mut b, TAG_ADD);
            put_varint32(&mut b, *level as u32);
            put_varint64(&mut b, f.number);
            put_varint64(&mut b, f.size);
            put_bytes(&mut b, &f.smallest);
            put_bytes(&mut b, &f.largest);
        }
        b
    }

    pub fn decode(mut src: &[u8]) -> Result<VersionEdit> {
        let bad = || Error::corruption("bad version edit");
        let mut e = VersionEdit::default();
        let u64_of = |src: &mut &[u8]| -> Result<u64> {
            let (v, n) = get_varint64(src).ok_or_else(bad)?;
            *src = &src[n..];
            Ok(v)
        };
        while !src.is_empty() {
            let (tag, n) = get_varint32(src).ok_or_else(bad)?;
            src = &src[n..];
            match tag {
                TAG_LOG => e.log_number = Some(u64_of(&mut src)?),
                TAG_NEXT_FILE => e.next_file = Some(u64_of(&mut src)?),
                TAG_LAST_SEQ => e.last_seq = Some(u64_of(&mut src)?),
                TAG_DELETE => {
                    let level = u64_of(&mut src)? as usize;
                    e.deleted.push((level, u64_of(&mut src)?));
                }
                TAG_ADD => {
                    let level = u64_of(&mut src)? as usize;
                    let number = u64_of(&mut src)?;
                    let size = u64_of(&mut src)?;
                    let smallest = get_bytes(&mut src).ok_or_else(bad)?.to_vec();
                    let largest = get_bytes(&mut src).ok_or_else(bad)?.to_vec();
                    e.added.push((level, NewFile { number, size, smallest, largest }));
                }
                _ => return Err(bad()),
            }
        }
        Ok(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let e = VersionEdit {
            log_number: Some(7),
            next_file: Some(99),
            last_seq: Some(1 << 40),
            deleted: vec![(0, 3), (2, 4)],
            added: vec![(
                1,
                NewFile { number: 8, size: 1234, smallest: b"a".to_vec(), largest: b"zz".to_vec() },
            )],
        };
        assert_eq!(VersionEdit::decode(&e.encode()).unwrap(), e);
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(VersionEdit::decode(&[99, 1, 2]).is_err());
        assert!(VersionEdit::decode(&[TAG_ADD as u8, 0, 1]).is_err());
    }
}
