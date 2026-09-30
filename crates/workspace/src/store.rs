//! The workspace's files under `<CAMPFIRE_STORAGE_PATH>/hermes/`, next to `workspace.json`: JSON
//! documents written atomically (a temporary file in the same directory, `fsync`, a rename, then the
//! directory's `fsync`) and JSON Lines logs appended to, rotated by size. Campfire's database is
//! never used (upstream's schema stays untouched).

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Writes `value` as pretty JSON to `path`, atomically.
pub(crate) fn write_json_atomically<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let dir = parent(path);
    std::fs::create_dir_all(&dir)?;
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("workspace.json");
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let temporary = dir.join(format!(".{name}.{}-{nanos}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        let mut json = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
        json.push(b'\n');
        file.write_all(&json)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result?;
    sync_dir(&dir)
}

/// Appends `value` as one JSON line to `path`. When the file has grown past `max_bytes`, it is first
/// renamed to `<path>.1` (replacing the previous one), so at most about twice `max_bytes` are kept.
pub(crate) fn append_json_line<T: Serialize>(path: &Path, value: &T, max_bytes: u64) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    line.push(b'\n');
    std::fs::create_dir_all(parent(path))?;
    if std::fs::metadata(path).is_ok_and(|meta| meta.len() + line.len() as u64 > max_bytes) {
        std::fs::rename(path, rotated(path))?;
    }
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(&line)?;
    file.sync_data()
}

/// Replaces `path` with `values`, one JSON line each, atomically (compacting a log).
pub(crate) fn write_json_lines_atomically<T: Serialize>(path: &Path, values: &[T]) -> std::io::Result<()> {
    let dir = parent(path);
    std::fs::create_dir_all(&dir)?;
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("log.jsonl");
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let temporary = dir.join(format!(".{name}.{}-{nanos}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        for value in values {
            let mut line = serde_json::to_vec(value).map_err(std::io::Error::other)?;
            line.push(b'\n');
            file.write_all(&line)?;
        }
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result?;
    sync_dir(&dir)
}

/// `<path>.1`, where [`append_json_line`] moves a full log.
pub(crate) fn rotated(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".1");
    PathBuf::from(name)
}

/// The entries of a JSON Lines file, oldest first; a missing file is empty and a line that doesn't
/// decode (a torn last line after a crash, an older format) is skipped.
pub(crate) fn read_json_lines<T: DeserializeOwned>(path: &Path) -> std::io::Result<Vec<T>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut entries = Vec::new();
    for line in std::io::BufReader::new(file).lines() {
        let line = line?;
        if let Ok(entry) = serde_json::from_str(&line) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

fn parent(path: &Path) -> PathBuf {
    path.parent().filter(|dir| !dir.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf()
}

/// Makes the rename durable: a crash right after it can't bring the old file back.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::File::open(dir)?.sync_all()
}

/// Windows can't open a directory as a file; its renames are journaled.
#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

/// A fresh directory under the system's temporary directory, for tests.
#[cfg(test)]
pub(crate) fn scratch_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("campfire-workspace-{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_append_rotate_and_skip_torn_ones() {
        let dir = scratch_dir("lines");
        let path = dir.join("log.jsonl");
        assert!(read_json_lines::<u32>(&path).unwrap().is_empty(), "missing = empty");
        for n in 0..3u32 {
            append_json_line(&path, &n, 1024).unwrap();
        }
        assert_eq!(read_json_lines::<u32>(&path).unwrap(), [0, 1, 2]);
        // A crash in the middle of a line: that line is skipped, the others are kept.
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"{\"torn").unwrap();
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"\n7\n").unwrap();
        assert_eq!(read_json_lines::<u32>(&path).unwrap(), [0, 1, 2, 7]);
        // Past the size, the file moves to .1 and a new one starts.
        append_json_line(&path, &"x".repeat(2000), 1024).unwrap();
        append_json_line(&path, &9u32, 1024).unwrap();
        assert_eq!(read_json_lines::<u32>(&path).unwrap(), [9], "the big line rotated the next one out");
        assert!(rotated(&path).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn documents_are_written_atomically() {
        let dir = scratch_dir("doc");
        let path = dir.join("nested").join("doc.json");
        write_json_atomically(&path, &serde_json::json!({"a": 1})).unwrap();
        let entries: Vec<String> =
            std::fs::read_dir(path.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        assert_eq!(entries, ["doc.json"]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
