//! When a bandwidth test runs. Test Now always runs; these decide the automatic ones.

use crate::probe::Trigger;
use crate::store;
use crate::verdict::Level;
use std::path::Path;

pub const ROUND_EVERY_MS: i64 = 60_000;
pub const QUICK_EVERY_MS: i64 = 10 * 60_000;
pub const FULL_EVERY_MS: i64 = 60 * 60_000;
/// A round that turns bad pulls a quick check forward, but never more often than this.
pub const BAD_ROUND_MIN_GAP_MS: i64 = 3 * 60_000;
/// The hourly full test moves about 3.4 GB a day, so it runs only after the person turns it on.
/// The setting is a marker file in the data folder, as `login-initialized` is.
const HOURLY_MARKER: &str = "hourly-test";

pub fn hourly_enabled() -> bool {
    hourly_enabled_in(&store::data_dir())
}

pub fn set_hourly(on: bool) -> std::io::Result<()> {
    set_hourly_in(&store::data_dir(), on)
}

fn hourly_enabled_in(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(HOURLY_MARKER)).is_ok_and(|m| m.is_file())
}

fn set_hourly_in(dir: &Path, on: bool) -> std::io::Result<()> {
    let path = dir.join(HOURLY_MARKER);
    if on {
        use std::io::Write;
        store::private_dir(dir)?;
        // Whatever sits at the marker's name is replaced, never written through, so a planted
        // link cannot aim the write at another file.
        if std::fs::symlink_metadata(&path).is_ok() {
            std::fs::remove_file(&path)?;
        }
        std::fs::OpenOptions::new().write(true).create_new(true).open(&path)?.write_all(b"on\n")
    } else if std::fs::symlink_metadata(&path).is_ok() {
        std::fs::remove_file(path)
    } else {
        Ok(())
    }
}

/// `last_quick` is the last test of any kind (a full test counts as a check too); `last_full`
/// is the last hourly or Test Now run; `hourly` is the person's opt-in to the hourly test. A bad
/// round pulls a check forward only when the network has just turned bad (`prev` was Good): a
/// network that stays weak is not tested every few minutes, which would load it exactly when it
/// is struggling.
pub fn bandwidth_due(
    now_ms: i64,
    last_quick: Option<i64>,
    last_full: Option<i64>,
    level: Level,
    prev: Level,
    expensive_path: bool,
    hourly: bool,
) -> Option<Trigger> {
    if expensive_path || level == Level::Offline {
        return None;
    }
    let since = |t: Option<i64>| t.map_or(i64::MAX, |t| now_ms - t);
    if hourly && since(last_full) >= FULL_EVERY_MS {
        Some(Trigger::Hourly)
    } else if since(last_quick) >= QUICK_EVERY_MS {
        Some(Trigger::Scheduled)
    } else if level >= Level::Fair && prev == Level::Good && since(last_quick) >= BAD_ROUND_MIN_GAP_MS {
        Some(Trigger::BadRound)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bandwidth_schedule() {
        let m = 60_000;
        let now = 1000 * m;
        let full = Some(now - 20 * m);
        let g = Level::Good;
        assert_eq!(bandwidth_due(now, None, None, g, g, false, true), Some(Trigger::Hourly), "first round runs a full test");
        assert_eq!(bandwidth_due(now, Some(now - 5 * m), full, g, g, false, true), None);
        assert_eq!(bandwidth_due(now, Some(now - 10 * m), full, g, g, false, true), Some(Trigger::Scheduled));
        assert_eq!(bandwidth_due(now, Some(now - 5 * m), Some(now - 60 * m), g, g, false, true), Some(Trigger::Hourly));
        assert_eq!(bandwidth_due(now, Some(now - 4 * m), full, Level::Fair, g, false, true), Some(Trigger::BadRound));
        assert_eq!(bandwidth_due(now, Some(now - 2 * m), full, Level::Poor, g, false, true), None, "rate limited");
        assert_eq!(bandwidth_due(now, None, None, Level::Offline, g, false, true), None);
    }

    #[test]
    fn staying_bad_does_not_retest() {
        let m = 60_000;
        let now = 1000 * m;
        let full = Some(now - 20 * m);
        assert_eq!(bandwidth_due(now, Some(now - 4 * m), full, Level::Fair, Level::Fair, false, true), None);
        assert_eq!(bandwidth_due(now, Some(now - 4 * m), full, Level::Poor, Level::Fair, false, true), None);
        // The ordinary 10-minute check still runs.
        assert_eq!(bandwidth_due(now, Some(now - 10 * m), full, Level::Poor, Level::Poor, false, true), Some(Trigger::Scheduled));
    }

    #[test]
    fn hourly_is_opt_in() {
        let m = 60_000;
        let now = 1000 * m;
        let g = Level::Good;
        assert_eq!(bandwidth_due(now, None, None, g, g, false, false), Some(Trigger::Scheduled), "first round runs a quick check");
        assert_eq!(bandwidth_due(now, Some(now - 5 * m), Some(now - 600 * m), g, g, false, false), None, "no hourly test while off");
        assert_eq!(bandwidth_due(now, Some(now - 10 * m), None, g, g, false, false), Some(Trigger::Scheduled));
    }

    #[test]
    fn hourly_setting() {
        let dir = std::env::temp_dir().join(format!("packet-hourly-{}", std::process::id()));
        assert!(!hourly_enabled_in(&dir), "off by default");
        set_hourly_in(&dir, true).unwrap();
        assert!(hourly_enabled_in(&dir));
        set_hourly_in(&dir, false).unwrap();
        assert!(!hourly_enabled_in(&dir));
        set_hourly_in(&dir, false).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hourly_marker_ignores_links() {
        let dir = std::env::temp_dir().join(format!("packet-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("target");
        std::fs::write(&target, b"keep").unwrap();
        std::os::unix::fs::symlink(&target, dir.join(HOURLY_MARKER)).unwrap();
        let planted = hourly_enabled_in(&dir);
        set_hourly_in(&dir, true).unwrap();
        let kept = std::fs::read(&target).unwrap();
        let is_file = std::fs::symlink_metadata(dir.join(HOURLY_MARKER)).unwrap().is_file();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!planted, "a planted link does not turn the hourly test on");
        assert_eq!(kept, b"keep", "the link's target is untouched");
        assert!(is_file, "the marker is a plain file");
    }

    #[test]
    fn expensive_skips() {
        let (g, p) = (Level::Good, Level::Poor);
        assert_eq!(bandwidth_due(1_000_000_000, None, None, g, g, true, true), None);
        assert_eq!(bandwidth_due(1_000_000_000, None, None, p, g, true, true), None);
    }
}
