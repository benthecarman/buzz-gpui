//! Media kept on disk between launches, standing in for the webview's HTTP
//! cache: without it every launch downloaded every avatar again.
//!
//! One file per URL, named by the URL's SHA-256, written to a temporary
//! name and renamed so a reader never sees half a body. Relay media is
//! content-addressed, so it never goes stale; other hosts' images are
//! shown from here and refreshed once they are older than
//! [`PUBLIC_FRESH`]. The folder is trimmed to [`BUDGET`], oldest first.
//! It lives in the app's cache folder, which signing out erases.

use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// Other hosts' images older than this are refreshed after showing
/// (the reference's `max-age=3600`).
pub const PUBLIC_FRESH: Duration = Duration::from_secs(3600);
/// Bytes kept on disk; a trim goes down to three quarters of it.
const BUDGET: u64 = 512 * 1024 * 1024;
/// Writes between trims.
const TRIM_EVERY: u64 = 256;

static DISK: OnceLock<Disk> = OnceLock::new();

/// Keeps fetched media under `dir` from now on. Set once, at startup.
pub fn set_disk_cache(dir: PathBuf) {
    let disk = Disk::new(dir);
    if DISK.set(disk).is_ok() {
        std::thread::spawn(|| {
            if let Some(disk) = DISK.get() {
                disk.trim();
            }
        });
    }
}

pub(crate) fn disk() -> Option<&'static Disk> {
    DISK.get()
}

pub(crate) struct Disk {
    dir: PathBuf,
    writes: AtomicU64,
}

/// A body read from disk, and how long ago it was fetched.
pub(crate) struct Cached {
    pub bytes: Vec<u8>,
    pub age: Duration,
}

impl Disk {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            writes: AtomicU64::new(0),
        }
    }

    fn path(&self, url: &str) -> PathBuf {
        let name = format!("{:x}", Sha256::digest(url.as_bytes()));
        self.dir.join(&name[..2]).join(name)
    }

    pub(crate) fn read(&self, url: &str) -> Option<Cached> {
        let path = self.path(url);
        let bytes = fs::read(&path).ok()?;
        let fetched = fs::metadata(&path).and_then(|meta| meta.modified()).ok()?;
        Some(Cached {
            bytes,
            age: SystemTime::now()
                .duration_since(fetched)
                .unwrap_or_default(),
        })
    }

    /// Keeps `bytes` for `url`; failures only cost a download next time.
    pub(crate) fn write(&self, url: &str, bytes: &[u8]) {
        let path = self.path(url);
        if let Err(error) = write_atomic(&path, bytes) {
            tracing::debug!(%error, "could not cache media on disk");
            return;
        }
        if self.writes.fetch_add(1, Ordering::Relaxed) % TRIM_EVERY == TRIM_EVERY - 1 {
            self.trim();
        }
    }

    /// Deletes the oldest files until the folder fits the budget.
    pub(crate) fn trim(&self) {
        self.trim_to(BUDGET);
    }

    fn trim_to(&self, budget: u64) {
        let mut files = Vec::new();
        let mut total = 0;
        for shard in fs::read_dir(&self.dir).into_iter().flatten().flatten() {
            for file in fs::read_dir(shard.path()).into_iter().flatten().flatten() {
                let Ok(meta) = file.metadata() else { continue };
                if !meta.is_file() {
                    continue;
                }
                let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                total += meta.len();
                files.push((modified, meta.len(), file.path()));
            }
        }
        if total <= budget {
            return;
        }
        files.sort_by_key(|(modified, _, _)| *modified);
        let target = budget / 4 * 3;
        for (_, len, path) in files {
            if total <= target {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total -= len;
            }
        }
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().expect("cache files sit in a shard");
    private_dir(dir)?;
    let temp = dir.join(format!(
        ".{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let written = fs::File::create(&temp)
        .and_then(|mut file| file.write_all(bytes))
        .and_then(|()| fs::rename(&temp, path));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

/// Creates the folder readable by this user alone: it holds private
/// channels' media.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    fs::create_dir_all(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_bodies_between_instances() {
        let dir = tempfile::tempdir().unwrap();
        let url = "https://example.com/a.png";
        assert!(Disk::new(dir.path().into()).read(url).is_none());
        Disk::new(dir.path().into()).write(url, b"picture");
        let cached = Disk::new(dir.path().into()).read(url).unwrap();
        assert_eq!(cached.bytes, b"picture");
        assert!(cached.age < PUBLIC_FRESH);
        assert!(
            Disk::new(dir.path().into())
                .read("https://example.com/b.png")
                .is_none()
        );
    }

    #[test]
    fn trims_the_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let disk = Disk::new(dir.path().into());
        for (n, age) in [(0, 300), (1, 200), (2, 100)] {
            let url = format!("https://example.com/{n}");
            disk.write(&url, &[0; 100]);
            let file = fs::File::options()
                .write(true)
                .open(disk.path(&url))
                .unwrap();
            file.set_modified(SystemTime::now() - Duration::from_secs(age))
                .unwrap();
        }
        disk.trim_to(250);
        assert!(disk.read("https://example.com/0").is_none());
        assert!(disk.read("https://example.com/1").is_none());
        assert!(disk.read("https://example.com/2").is_some());
    }
}
