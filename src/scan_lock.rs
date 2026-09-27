//! Cross-process scan lock, so the web "Scan now", the CLI `scan`, and the
//! 7 AM `digest` never run two scans at once.
//!
//! A lock file created with `create_new` (atomic "create if absent"). It's
//! removed when the [`ScanLock`] guard drops. If a process dies without
//! cleaning up (e.g. the service is restarted mid-scan), the lock is taken over
//! as soon as its recorded pid is no longer running. A lock whose holder can't
//! be checked is taken over once it's older than [`STALE_AFTER`]. A live holder
//! keeps it however long it runs (LLM rate-limit backoff can exceed 30 min).

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Result};

/// A lock this old is assumed to belong to a crashed/killed scan.
pub const STALE_AFTER: Duration = Duration::from_secs(30 * 60);

/// Holding this means you own the scan. Dropping it releases the lock.
#[derive(Debug)]
pub struct ScanLock {
    path: PathBuf,
}

impl ScanLock {
    /// Take the lock if it's free (or stale). `Ok(None)` means another scan
    /// holds it.
    pub fn try_acquire(path: impl AsRef<Path>) -> Result<Option<ScanLock>> {
        let path = path.as_ref();
        for _ in 0..2 {
            match OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(mut f) => {
                    // Informational only — who holds it and since when.
                    // "pid N at T" — the pid drives the dead-holder check.
                    let _ = writeln!(f, "pid {} at {}", std::process::id(), unix_now());
                    return Ok(Some(ScanLock { path: path.to_path_buf() }));
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    if !is_stale(path) {
                        return Ok(None);
                    }
                    eprintln!("Removing abandoned scan lock {}.", path.display());
                    match fs::remove_file(path) {
                        Ok(()) => continue,
                        Err(e) if e.kind() == ErrorKind::NotFound => continue,
                        Err(e) => return Err(anyhow!("couldn't remove stale scan lock {}: {e}", path.display())),
                    }
                }
                Err(e) => return Err(anyhow!("couldn't create scan lock {}: {e}", path.display())),
            }
        }
        // Lost a race to another process taking over the same stale lock.
        Ok(None)
    }

    /// Wait (polling) up to `timeout` for the lock. Errors with a clear message
    /// if another scan still holds it after that.
    pub async fn acquire_waiting(path: impl AsRef<Path>, timeout: Duration) -> Result<ScanLock> {
        let path = path.as_ref();
        let deadline = tokio::time::Instant::now() + timeout;
        let mut announced = false;
        loop {
            if let Some(lock) = Self::try_acquire(path)? {
                return Ok(lock);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(anyhow!(
                    "another scan is still running after waiting {} min (lock file {}). \
                     If you're sure no scan is running, delete that file and try again.",
                    timeout.as_secs() / 60,
                    path.display()
                ));
            }
            if !announced {
                println!("Another scan is running — waiting for it to finish (up to {} min)…", timeout.as_secs() / 60);
                announced = true;
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
}

impl Drop for ScanLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// The lock is abandoned: its holder's pid is dead, or (when the holder can't
/// be checked) it's older than [`STALE_AFTER`].
fn is_stale(path: &Path) -> bool {
    match holder_pid(path).map(pid_alive) {
        Some(Liveness::Alive) => false,
        Some(Liveness::Dead) => true,
        Some(Liveness::Unknown) | None => fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age > STALE_AFTER),
    }
}

enum Liveness {
    Alive,
    Dead,
    Unknown,
}

fn holder_pid(path: &Path) -> Option<u32> {
    let text = fs::read_to_string(path).ok()?;
    text.strip_prefix("pid ")?.split_whitespace().next()?.parse().ok()
}

/// Whether `pid` is a running process, via `kill -0` (macOS + Linux). Only a
/// definite "No such process" counts as dead; permission errors or a missing
/// `kill` are `Unknown` and fall back to the age check.
fn pid_alive(pid: u32) -> Liveness {
    if pid == std::process::id() {
        return Liveness::Alive;
    }
    match std::process::Command::new("kill").arg("-0").arg(pid.to_string()).output() {
        Ok(out) if out.status.success() => Liveness::Alive,
        Ok(out) if String::from_utf8_lossy(&out.stderr).contains("No such process") => Liveness::Dead,
        _ => Liveness::Unknown,
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_lock(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("job_hunter_{name}_{}.scan.lock", std::process::id()));
        let _ = fs::remove_file(&p);
        p
    }

    #[test]
    fn second_acquire_fails_until_released() {
        let p = temp_lock("exclusive");
        let first = ScanLock::try_acquire(&p).unwrap().expect("free lock");
        assert!(ScanLock::try_acquire(&p).unwrap().is_none(), "held lock must not be re-acquired");
        drop(first);
        assert!(!p.exists(), "dropping the guard removes the file");
        let again = ScanLock::try_acquire(&p).unwrap();
        assert!(again.is_some(), "released lock is free again");
    }

    #[test]
    fn lock_of_dead_process_is_taken_over() {
        let p = temp_lock("dead");
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead_pid = child.id();
        child.wait().unwrap();
        fs::write(&p, format!("pid {dead_pid} at 0\n")).unwrap();
        assert!(ScanLock::try_acquire(&p).unwrap().is_some(), "a dead holder's lock is abandoned");
    }

    #[test]
    fn live_holders_fresh_lock_is_respected() {
        let p = temp_lock("live");
        fs::write(&p, format!("pid {} at 0\n", std::process::id())).unwrap();
        assert!(ScanLock::try_acquire(&p).unwrap().is_none());
        fs::remove_file(&p).unwrap();
    }

    fn age_past_stale(p: &Path) {
        let old = SystemTime::now() - STALE_AFTER - Duration::from_secs(60);
        fs::File::options().write(true).open(p).unwrap().set_modified(old).unwrap();
    }

    #[test]
    fn old_lock_without_a_pid_is_taken_over() {
        let p = temp_lock("stale");
        fs::write(&p, "legacy\n").unwrap();
        age_past_stale(&p);
        assert!(ScanLock::try_acquire(&p).unwrap().is_some(), "an old unverifiable lock is abandoned");
    }

    #[test]
    fn old_lock_of_live_holder_is_kept() {
        let p = temp_lock("long");
        fs::write(&p, format!("pid {} at 0\n", std::process::id())).unwrap();
        age_past_stale(&p);
        assert!(ScanLock::try_acquire(&p).unwrap().is_none(), "a long-running live scan keeps its lock");
        fs::remove_file(&p).unwrap();
    }
}
