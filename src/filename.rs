use std::path::{Path, PathBuf};

pub enum FileKind {
    Log(u64),
    Table(u64),
    Manifest(u64),
    Temp,
    Other,
}

pub fn table_path(dir: &Path, n: u64) -> PathBuf {
    dir.join(format!("{n:06}.sst"))
}

pub fn log_path(dir: &Path, n: u64) -> PathBuf {
    dir.join(format!("{n:06}.log"))
}

pub fn manifest_name(n: u64) -> String {
    format!("MANIFEST-{n:06}")
}

pub fn current_path(dir: &Path) -> PathBuf {
    dir.join("CURRENT")
}

pub fn lock_path(dir: &Path) -> PathBuf {
    dir.join("LOCK")
}

pub fn parse(name: &str) -> FileKind {
    if let Some(n) = name.strip_prefix("MANIFEST-") {
        return n.parse().map(FileKind::Manifest).unwrap_or(FileKind::Other);
    }
    if name.ends_with(".tmp") {
        return FileKind::Temp;
    }
    if let Some(n) = name.strip_suffix(".sst") {
        return n.parse().map(FileKind::Table).unwrap_or(FileKind::Other);
    }
    if let Some(n) = name.strip_suffix(".log") {
        return n.parse().map(FileKind::Log).unwrap_or(FileKind::Other);
    }
    FileKind::Other
}
