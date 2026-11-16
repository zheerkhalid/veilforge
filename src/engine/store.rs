//! Files in the data folder: atomic writes, rolling backups, the cross-process
//! lock and the raw journal file.

use crate::secret::random_array;
use crate::util;
use std::io::{Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const MAP_FILE: &str = "map.vfm";
pub const JOURNAL_FILE: &str = "journal.vfj";
pub const LOCK_FILE: &str = "map.lock";
/// How many `.bakN` copies of the map are kept.
pub const BACKUPS: u32 = 3;
/// How long a mutating call waits for another VeilForge process.
pub const LOCK_WAIT: Duration = Duration::from_secs(20);

pub fn backup_path(path: &Path, n: u32) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".bak{n}"));
    path.with_file_name(name)
}

/// Shifts `.bak1` -> `.bak2` ... and copies the current file to `.bak1`.
pub fn rotate_backups(path: &Path, keep: u32) -> std::io::Result<()> {
    if keep == 0 || !path.exists() {
        return Ok(());
    }
    let _ = std::fs::remove_file(backup_path(path, keep));
    for n in (1..keep).rev() {
        let from = backup_path(path, n);
        if from.exists() {
            std::fs::rename(&from, backup_path(path, n + 1))?;
        }
    }
    std::fs::copy(path, backup_path(path, 1))?;
    Ok(())
}

/// Writes to a temporary file in the same folder, flushes it to disk, then
/// renames it over `path`: a crash leaves the old file or the new one.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(".{}.tmp", util::hex(&random_array::<6>())));
    let tmp = path.with_file_name(tmp_name);
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// The cross-process lock: an exclusively opened file in the data folder.
/// Windows releases it when the handle closes, also when the process dies.
pub struct MapLock {
    _file: std::fs::File,
}

impl MapLock {
    pub fn acquire(data_dir: &Path, wait: Duration) -> Result<MapLock, String> {
        std::fs::create_dir_all(data_dir).map_err(|e| e.to_string())?;
        let path = data_dir.join(LOCK_FILE);
        let start = Instant::now();
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .share_mode(0)
                .open(&path)
            {
                Ok(file) => return Ok(MapLock { _file: file }),
                Err(e) if e.raw_os_error() == Some(32) || e.raw_os_error() == Some(5) => {
                    if start.elapsed() >= wait {
                        return Err(
                            "Another VeilForge window is busy with the protected items. Try again in a moment."
                                .into(),
                        );
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(e) => return Err(format!("The lock file cannot be opened: {e}")),
            }
        }
    }
}

/// Appends one length-prefixed record to the journal and flushes it to disk.
pub fn journal_append(data_dir: &Path, record: &[u8]) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(data_dir.join(JOURNAL_FILE))?;
    let mut buf = Vec::with_capacity(record.len() + 4);
    buf.extend_from_slice(&(record.len() as u32).to_le_bytes());
    buf.extend_from_slice(record);
    f.write_all(&buf)?;
    f.sync_all()
}

/// All complete records in the journal (a torn last record is ignored).
pub fn journal_read(data_dir: &Path) -> Vec<Vec<u8>> {
    let mut bytes = Vec::new();
    match std::fs::File::open(data_dir.join(JOURNAL_FILE)) {
        Ok(mut f) => {
            if f.read_to_end(&mut bytes).is_err() {
                return Vec::new();
            }
        }
        Err(_) => return Vec::new(),
    }
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 4 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        at += 4;
        if at + len > bytes.len() {
            break;
        }
        out.push(bytes[at..at + len].to_vec());
        at += len;
    }
    out
}

pub fn journal_exists(data_dir: &Path) -> bool {
    std::fs::metadata(data_dir.join(JOURNAL_FILE))
        .map(|m| m.len() > 0)
        .unwrap_or(false)
}

pub fn journal_clear(data_dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(data_dir.join(JOURNAL_FILE)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_backups_journal_and_lock() {
        let dir = util::unique_temp_dir("store");
        let path = dir.join(MAP_FILE);
        for n in 0..5u8 {
            rotate_backups(&path, BACKUPS).unwrap();
            write_atomic(&path, &[n; 4]).unwrap();
        }
        assert_eq!(std::fs::read(&path).unwrap(), [4u8; 4]);
        assert_eq!(std::fs::read(backup_path(&path, 1)).unwrap(), [3u8; 4]);
        assert_eq!(std::fs::read(backup_path(&path, 3)).unwrap(), [1u8; 4]);
        assert!(!backup_path(&path, 4).exists());
        // No temp files are left behind.
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .count();
        assert_eq!(leftovers, 0);

        assert!(!journal_exists(&dir));
        journal_append(&dir, b"one").unwrap();
        journal_append(&dir, b"second").unwrap();
        assert!(journal_exists(&dir));
        assert_eq!(
            journal_read(&dir),
            vec![b"one".to_vec(), b"second".to_vec()]
        );
        // A torn tail is ignored.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join(JOURNAL_FILE))
            .unwrap();
        f.write_all(&[9, 0, 0, 0, 1, 2]).unwrap();
        drop(f);
        assert_eq!(journal_read(&dir).len(), 2);
        journal_clear(&dir).unwrap();
        journal_clear(&dir).unwrap();
        assert!(!journal_exists(&dir));

        let held = MapLock::acquire(&dir, Duration::from_millis(50)).unwrap();
        assert!(MapLock::acquire(&dir, Duration::from_millis(80)).is_err());
        drop(held);
        assert!(MapLock::acquire(&dir, Duration::from_millis(80)).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
