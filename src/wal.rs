use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::Path,
};

use crate::{
    coding::{crc_masked, put_fixed32, read_fixed32},
    error::Result,
};

// record: crc32 (masked, over len+payload) | len u32 | payload
const HEADER: usize = 8;

pub struct LogWriter {
    file: File,
    scratch: Vec<u8>,
}

impl LogWriter {
    pub fn create(path: &Path) -> Result<Self> {
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        Ok(LogWriter { file, scratch: Vec::new() })
    }

    // one write(2) per record so a crash leaves at most one torn tail
    pub fn append(&mut self, payload: &[u8]) -> Result<()> {
        self.scratch.clear();
        put_fixed32(&mut self.scratch, 0);
        put_fixed32(&mut self.scratch, payload.len() as u32);
        self.scratch.extend_from_slice(payload);
        let crc = crc_masked(&[&self.scratch[4..]]);
        self.scratch[..4].copy_from_slice(&crc.to_le_bytes());
        self.file.write_all(&self.scratch)?;
        Ok(())
    }

    pub fn sync(&mut self) -> Result<()> {
        self.file.sync_data()?;
        Ok(())
    }
}

pub struct LogReader {
    data: Vec<u8>,
    pos: usize,
    torn: bool,
}

impl LogReader {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(LogReader { data: fs::read(path)?, pos: 0, torn: false })
    }

    #[cfg(test)]
    /// true if reading stopped at a partial or corrupt record rather than clean EOF
    pub fn hit_torn_tail(&self) -> bool {
        self.torn
    }

    // stops at the first bad record: everything before it was acknowledged
    // in order, everything after it can't be trusted
    pub fn read_record(&mut self) -> Option<&[u8]> {
        let rest = &self.data[self.pos..];
        if rest.is_empty() {
            return None;
        }
        if rest.len() < HEADER {
            self.torn = true;
            return None;
        }
        let len = read_fixed32(&rest[4..]) as usize;
        if rest.len() < HEADER + len {
            self.torn = true;
            return None;
        }
        if crc_masked(&[&rest[4..HEADER + len]]) != read_fixed32(rest) {
            self.torn = true;
            return None;
        }
        let start = self.pos + HEADER;
        self.pos = start + len;
        Some(&self.data[start..start + len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.log");
        let mut w = LogWriter::create(&p).unwrap();
        w.append(b"one").unwrap();
        w.append(b"").unwrap();
        w.append(&vec![7u8; 100_000]).unwrap();
        w.sync().unwrap();

        let mut r = LogReader::open(&p).unwrap();
        assert_eq!(r.read_record(), Some(&b"one"[..]));
        assert_eq!(r.read_record(), Some(&b""[..]));
        assert_eq!(r.read_record().unwrap().len(), 100_000);
        assert!(r.read_record().is_none());
        assert!(!r.hit_torn_tail());
    }

    #[test]
    fn torn_tail_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.log");
        let mut w = LogWriter::create(&p).unwrap();
        w.append(b"good").unwrap();
        w.append(b"will be cut").unwrap();
        drop(w);
        let len = fs::metadata(&p).unwrap().len();
        OpenOptions::new().write(true).open(&p).unwrap().set_len(len - 3).unwrap();

        let mut r = LogReader::open(&p).unwrap();
        assert_eq!(r.read_record(), Some(&b"good"[..]));
        assert!(r.read_record().is_none());
        assert!(r.hit_torn_tail());
    }

    #[test]
    fn bit_flip_stops_replay() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.log");
        let mut w = LogWriter::create(&p).unwrap();
        w.append(b"aaaa").unwrap();
        w.append(b"bbbb").unwrap();
        drop(w);
        let mut bytes = fs::read(&p).unwrap();
        let n = bytes.len();
        bytes[n - 1] ^= 0xff;
        fs::write(&p, bytes).unwrap();

        let mut r = LogReader::open(&p).unwrap();
        assert_eq!(r.read_record(), Some(&b"aaaa"[..]));
        assert!(r.read_record().is_none());
        assert!(r.hit_torn_tail());
    }
}
