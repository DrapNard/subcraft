//! Bounded, per-user Activity history, separate from project data, undo, and diagnostics.
//!
//! Adapted from Kalium's `activity_log` module by DrapNard and contributors (AGPL-3.0-only).
//! The UI supplies only action summaries; project contents and absolute paths are not recorded.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const VERSION: u32 = 1;
const MAX_ENTRIES: usize = 256;
const MAX_CATEGORY_BYTES: usize = 48;
const MAX_MESSAGE_BYTES: usize = 768;
const MAX_FILE_BYTES: u64 = 256 * 1024;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivityLevel {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityEntry {
    pub timestamp_unix_ms: u64,
    pub level: ActivityLevel,
    pub category: String,
    pub message: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivityFile {
    version: u32,
    entries: Vec<ActivityEntry>,
}

/// Entries are stored oldest first and exposed newest first.
pub struct ActivityLog {
    path: PathBuf,
    entries: Vec<ActivityEntry>,
}

/// Per-user native data path. Browser sessions retain Activity only in memory.
#[cfg(not(target_arch = "wasm32"))]
pub fn default_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|path| path.join("SubCraft").join("activity-v1.json"))
}

#[cfg(target_arch = "wasm32")]
pub fn default_path() -> Option<PathBuf> {
    None
}

impl ActivityLog {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    path,
                    entries: Vec::new(),
                });
            }
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("cannot open activity log {}", path.display()));
            }
        };
        if file.metadata()?.len() > MAX_FILE_BYTES {
            bail!("activity log exceeds {MAX_FILE_BYTES} bytes");
        }
        let mut bytes = Vec::new();
        file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            bail!("activity log exceeds {MAX_FILE_BYTES} bytes");
        }
        let data: ActivityFile = serde_json::from_slice(&bytes).context("invalid activity log")?;
        if data.version != VERSION {
            bail!("unsupported activity log version {}", data.version);
        }
        validate(&data.entries)?;
        Ok(Self {
            path,
            entries: data.entries,
        })
    }

    pub fn entries_newest_first(&self) -> impl ExactSizeIterator<Item = &ActivityEntry> {
        self.entries.iter().rev()
    }

    pub fn record(&mut self, level: ActivityLevel, category: &str, message: &str) -> Result<()> {
        let timestamp_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before the Unix epoch")?
            .as_millis()
            .min(u64::MAX as u128) as u64;
        let mut next = self.entries.clone();
        if next.len() == MAX_ENTRIES {
            next.remove(0);
        }
        next.push(ActivityEntry {
            timestamp_unix_ms,
            level,
            category: truncate(category, MAX_CATEGORY_BYTES),
            message: truncate(message, MAX_MESSAGE_BYTES),
        });
        self.replace(next)
    }

    pub fn clear(&mut self) -> Result<()> {
        self.replace(Vec::new())
    }

    fn replace(&mut self, next: Vec<ActivityEntry>) -> Result<()> {
        validate(&next)?;
        persist(&self.path, &next)?;
        self.entries = next;
        Ok(())
    }
}

fn validate(entries: &[ActivityEntry]) -> Result<()> {
    if entries.len() > MAX_ENTRIES {
        bail!("activity log has more than {MAX_ENTRIES} entries");
    }
    if entries
        .iter()
        .any(|e| e.category.len() > MAX_CATEGORY_BYTES || e.message.len() > MAX_MESSAGE_BYTES)
    {
        bail!("activity log entry exceeds its size limit");
    }
    Ok(())
}

fn truncate(value: &str, cap: usize) -> String {
    if value.len() <= cap {
        return value.to_owned();
    }
    let mut end = cap;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn persist(path: &Path, entries: &[ActivityEntry]) -> Result<()> {
    let bytes = serde_json::to_vec(&ActivityFile {
        version: VERSION,
        entries: entries.to_vec(),
    })?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        bail!("serialized activity log exceeds {MAX_FILE_BYTES} bytes");
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)
        .with_context(|| format!("cannot create activity data directory {}", parent.display()))?;
    let name = path
        .file_name()
        .context("activity log path has no filename")?
        .to_string_lossy();
    let mut allocated = None;
    for _ in 0..32 {
        let n = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), n));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                allocated = Some((candidate, file));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).context("cannot create temporary activity log"),
        }
    }
    let (temp, mut file) = allocated.context("could not allocate activity log temporary file")?;
    let result = (|| -> Result<()> {
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temp, path)
            .with_context(|| format!("cannot atomically replace {}", path.display()))?;
        #[cfg(unix)]
        File::open(parent)?
            .sync_all()
            .context("cannot sync activity data directory")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[allow(unsafe_code)]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::{ffi::OsStr, os::windows::ffi::OsStrExt};
        const REPLACE_EXISTING: u32 = 1;
        const WRITE_THROUGH: u32 = 8;
        fn wide(p: &OsStr) -> Vec<u16> {
            p.encode_wide().chain(std::iter::once(0)).collect()
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
        }
        let from = wide(from.as_os_str());
        let to = wide(to.as_os_str());
        // SAFETY: both arguments point to NUL-terminated buffers valid for this call.
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), REPLACE_EXISTING | WRITE_THROUGH) } == 0
        {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    #[cfg(not(windows))]
    {
        fs::rename(from, to)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static TEST_ID: AtomicU64 = AtomicU64::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "subcraft-activity-{}-{}",
                std::process::id(),
                TEST_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> PathBuf {
            self.0.join("activity.json")
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn persists_entries_newest_first_and_clear_is_durable() {
        let temp = Temp::new();
        let path = temp.path();
        let mut log = ActivityLog::load(&path).unwrap();
        log.record(ActivityLevel::Info, "Project", "Saved project")
            .unwrap();
        log.record(ActivityLevel::Warning, "Export", "Export failed")
            .unwrap();
        let loaded = ActivityLog::load(&path).unwrap();
        let entries: Vec<_> = loaded.entries_newest_first().collect();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].category, "Export");
        log.clear().unwrap();
        assert_eq!(
            ActivityLog::load(path)
                .unwrap()
                .entries_newest_first()
                .len(),
            0
        );
    }

    #[test]
    fn caps_entry_count_and_truncates_on_utf8_boundaries() {
        let temp = Temp::new();
        let mut log = ActivityLog::load(temp.path()).unwrap();
        for i in 0..=MAX_ENTRIES {
            log.record(ActivityLevel::Info, "event", &i.to_string())
                .unwrap();
        }
        assert_eq!(log.entries_newest_first().len(), MAX_ENTRIES);
        let mut other = ActivityLog::load(temp.0.join("utf8.json")).unwrap();
        other
            .record(
                ActivityLevel::Info,
                &format!("{}é", "a".repeat(47)),
                &format!("{}🖌", "b".repeat(767)),
            )
            .unwrap();
        let entry = other.entries_newest_first().next().unwrap();
        assert_eq!(entry.category.len(), 47);
        assert_eq!(entry.message.len(), 767);
        assert!(entry.category.is_char_boundary(entry.category.len()));
        assert!(entry.message.is_char_boundary(entry.message.len()));
    }

    #[test]
    fn rejects_oversized_and_unknown_version_logs_without_replacing_them() {
        let temp = Temp::new();
        let path = temp.path();
        fs::write(&path, br#"{"version":9,"entries":[]}"#).unwrap();
        assert!(ActivityLog::load(&path).is_err());
        fs::write(&path, vec![b' '; MAX_FILE_BYTES as usize + 1]).unwrap();
        assert!(ActivityLog::load(path).is_err());
    }

    #[test]
    fn failed_persistence_leaves_current_file_and_memory_unchanged() {
        let temp = Temp::new();
        let path = temp.path();
        let mut log = ActivityLog::load(&path).unwrap();
        log.record(ActivityLevel::Info, "Project", "Saved project")
            .unwrap();
        let previous_bytes = fs::read(&path).unwrap();
        let previous_entries = log.entries_newest_first().cloned().collect::<Vec<_>>();
        // A regular file cannot serve as the parent directory for the replacement.
        let blocker = temp.0.join("not-a-directory");
        fs::write(&blocker, b"keep").unwrap();
        log.path = blocker.join("activity.json");
        assert!(
            log.record(ActivityLevel::Error, "Project", "Save failed")
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), previous_bytes);
        assert_eq!(
            log.entries_newest_first().cloned().collect::<Vec<_>>(),
            previous_entries
        );
    }
}
