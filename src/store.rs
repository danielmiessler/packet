//! Packet's one database. Numbers about the path only: no hostnames visited, no apps, no traffic.

use crate::probe::{Bandwidth, LinkKind, Trigger, WiFi};
use crate::round::{Layer, Round};
use crate::verdict::{Baseline, Verdict};
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::path::{Path, PathBuf};

pub const RETAIN_DAYS: i64 = 90;
const SCHEMA_VERSION: i64 = 2;
/// Good rounds needed before the baseline replaces fixed thresholds.
const BASELINE_MIN_ROUNDS: i64 = 30;

fn invariant(cond: bool, msg: &str) {
    if !cond {
        panic!("invariant violated: {msg}");
    }
}

/// The one place Packet decides where it writes. `PACKET_DATA_DIR` overrides it for tests.
pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("PACKET_DATA_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("HOME").expect("HOME is set");
    PathBuf::from(home).join("Library/Application Support/Packet")
}

/// Creates the data directory if needed and makes it owner-only: the history is nobody else's
/// business on a shared Mac.
pub fn private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

pub fn open() -> rusqlite::Result<Connection> {
    let dir = data_dir();
    private_dir(&dir).expect("create data dir");
    open_at(&dir.join("packet.sqlite"))
}

/// The database and its WAL files are owner-only. SQLite gives new WAL files the database's mode,
/// so setting it here covers installs made before this rule too.
fn owner_only(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    for suffix in ["", "-wal", "-shm"] {
        let mut p = path.as_os_str().to_owned();
        p.push(suffix);
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
}

pub fn open_at(path: &Path) -> rusqlite::Result<Connection> {
    let c = Connection::open(path)?;
    owner_only(path);
    c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=2000;")?;
    let version: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version < SCHEMA_VERSION {
        c.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS rounds(
               at_ms INTEGER PRIMARY KEY,
               link INTEGER NOT NULL,
               rssi INTEGER, noise INTEGER, tx_mbps REAL, channel INTEGER, band INTEGER,
               gw_ms REAL, gw_loss REAL, gw_jitter REAL,
               edge_ms REAL, edge_loss REAL, edge_jitter REAL, edge_colo TEXT,
               dns_ms REAL,
               r0 REAL, r1 REAL, r2 REAL, r3 REAL, r4 REAL, r5 REAL, r6 REAL, r7 REAL,
               expensive INTEGER NOT NULL,
               level INTEGER NOT NULL,
               layer INTEGER NOT NULL
             ) STRICT;
             CREATE TABLE IF NOT EXISTS bandwidth(
               at_ms INTEGER PRIMARY KEY,
               trigger INTEGER NOT NULL,
               down_mbps REAL, up_mbps REAL,
               down_bytes INTEGER NOT NULL, up_bytes INTEGER NOT NULL
             ) STRICT;
             PRAGMA user_version = 1;
             COMMIT;",
        )?;
    }
    if version < 2 {
        // Version 2: responsiveness under load, from Test Now.
        c.execute_batch("BEGIN; ALTER TABLE bandwidth ADD COLUMN rpm REAL; PRAGMA user_version = 2; COMMIT;")?;
    }
    Ok(c)
}

pub fn insert_round(c: &Connection, r: &Round, v: &Verdict) -> rusqlite::Result<()> {
    invariant(r.at_ms > 0, "round has a timestamp");
    invariant(r.colo.as_deref().is_none_or(|s| s.len() <= 5), "colo is a data-center code");
    let w = r.wifi.unwrap_or_default();
    let wifi = r.wifi.is_some();
    c.execute(
        "INSERT OR REPLACE INTO rounds VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26)",
        params![
            r.at_ms,
            r.link as i64,
            wifi.then_some(w.rssi),
            wifi.then_some(w.noise),
            wifi.then_some(w.tx_mbps),
            wifi.then_some(w.channel),
            wifi.then_some(w.band),
            r.gateway.ms,
            r.gateway.loss,
            r.gateway.jitter,
            r.edge.ms,
            r.edge.loss,
            r.edge.jitter,
            r.colo,
            r.dns_ms,
            r.regions[0],
            r.regions[1],
            r.regions[2],
            r.regions[3],
            r.regions[4],
            r.regions[5],
            r.regions[6],
            r.regions[7],
            r.expensive as i64,
            v.level as i64,
            v.at as i64,
        ],
    )?;
    Ok(())
}

pub fn insert_bandwidth(c: &Connection, b: &Bandwidth) -> rusqlite::Result<()> {
    invariant(b.at_ms > 0, "bandwidth has a timestamp");
    invariant(b.down_bytes >= 0 && b.up_bytes >= 0, "byte counts are non-negative");
    c.execute(
        "INSERT OR REPLACE INTO bandwidth VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![b.at_ms, b.trigger as i64, b.down_mbps, b.up_mbps, b.down_bytes, b.up_bytes, b.rpm],
    )?;
    Ok(())
}

pub fn prune(c: &Connection, now_ms: i64) -> rusqlite::Result<()> {
    let cutoff = now_ms - RETAIN_DAYS * 86_400_000;
    c.execute("DELETE FROM rounds WHERE at_ms < ?1", [cutoff])?;
    c.execute("DELETE FROM bandwidth WHERE at_ms < ?1", [cutoff])?;
    Ok(())
}

pub struct Stored {
    pub round: Round,
    pub verdict: Verdict,
}

pub const ROUND_COLS: &str = "at_ms,link,rssi,noise,tx_mbps,channel,band,gw_ms,gw_loss,gw_jitter,edge_ms,edge_loss,edge_jitter,edge_colo,dns_ms,r0,r1,r2,r3,r4,r5,r6,r7,expensive,level,layer";

pub fn row_to_stored(row: &Row) -> rusqlite::Result<Stored> {
    let rssi: Option<i64> = row.get(2)?;
    let wifi = rssi.map(|rssi| -> rusqlite::Result<WiFi> {
        Ok(WiFi {
            rssi,
            noise: row.get::<_, Option<i64>>(3)?.unwrap_or(0),
            tx_mbps: row.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
            channel: row.get::<_, Option<i64>>(5)?.unwrap_or(0),
            band: row.get::<_, Option<i64>>(6)?.unwrap_or(0),
        })
    });
    let mut regions = [None; 8];
    for (i, slot) in regions.iter_mut().enumerate() {
        *slot = row.get(15 + i)?;
    }
    let gw_ms: Option<f64> = row.get(7)?;
    // A router that was pinged but never answered is stored with loss and no latency.
    let round = Round {
        at_ms: row.get(0)?,
        link: LinkKind::from_i64(row.get(1)?),
        wifi: wifi.transpose()?,
        gateway: Layer { ms: gw_ms, loss: row.get(8)?, jitter: row.get(9)? },
        gateway_known: gw_ms.is_some() || row.get::<_, Option<f64>>(8)?.is_some(),
        edge: Layer { ms: row.get(10)?, loss: row.get(11)?, jitter: row.get(12)? },
        colo: row.get(13)?,
        dns_ms: row.get(14)?,
        regions,
        expensive: row.get::<_, i64>(23)? != 0,
    };
    Ok(Stored { round, verdict: Verdict::stored(row.get(24)?, row.get(25)?) })
}

#[cfg(test)]
pub fn latest_round(c: &Connection) -> rusqlite::Result<Option<Stored>> {
    c.query_row(&format!("SELECT {ROUND_COLS} FROM rounds ORDER BY at_ms DESC LIMIT 1"), [], row_to_stored).optional()
}

/// The stored round closest to `at_ms`, within `window_ms`.
pub fn nearest_round(c: &Connection, at_ms: i64, window_ms: i64) -> rusqlite::Result<Option<Stored>> {
    c.query_row(
        &format!(
            "SELECT {ROUND_COLS} FROM rounds WHERE at_ms BETWEEN ?1 AND ?2 ORDER BY ABS(at_ms - ?3) LIMIT 1"
        ),
        params![at_ms - window_ms, at_ms + window_ms, at_ms],
        row_to_stored,
    )
    .optional()
}

pub fn rounds_between(c: &Connection, from: i64, to: i64) -> rusqlite::Result<Vec<Stored>> {
    let mut s = c.prepare(&format!("SELECT {ROUND_COLS} FROM rounds WHERE at_ms BETWEEN ?1 AND ?2 ORDER BY at_ms"))?;
    let rows = s.query_map(params![from, to], row_to_stored)?;
    rows.collect()
}

fn row_to_bw(row: &Row) -> rusqlite::Result<Bandwidth> {
    Ok(Bandwidth {
        at_ms: row.get(0)?,
        trigger: Trigger::from_i64(row.get(1)?),
        down_mbps: row.get(2)?,
        up_mbps: row.get(3)?,
        down_bytes: row.get(4)?,
        up_bytes: row.get(5)?,
        rpm: row.get(6)?,
    })
}

/// The last hourly or Test Now run: the line's real speed.
pub fn latest_full(c: &Connection) -> rusqlite::Result<Option<Bandwidth>> {
    c.query_row("SELECT * FROM bandwidth WHERE trigger IN (2,3) ORDER BY at_ms DESC LIMIT 1", [], row_to_bw).optional()
}

pub fn latest_quick(c: &Connection) -> rusqlite::Result<Option<Bandwidth>> {
    c.query_row("SELECT * FROM bandwidth WHERE trigger IN (0,1) ORDER BY at_ms DESC LIMIT 1", [], row_to_bw).optional()
}

pub fn nearest_bandwidth(c: &Connection, at_ms: i64, window_ms: i64) -> rusqlite::Result<Option<Bandwidth>> {
    c.query_row(
        "SELECT * FROM bandwidth WHERE at_ms BETWEEN ?1 AND ?2 ORDER BY ABS(at_ms - ?3) LIMIT 1",
        params![at_ms - window_ms, at_ms + window_ms, at_ms],
        row_to_bw,
    )
    .optional()
}

pub fn bandwidth_between(c: &Connection, from: i64, to: i64) -> rusqlite::Result<Vec<Bandwidth>> {
    let mut s = c.prepare("SELECT * FROM bandwidth WHERE at_ms BETWEEN ?1 AND ?2 ORDER BY at_ms")?;
    let rows = s.query_map(params![from, to], row_to_bw)?;
    rows.collect()
}

fn median_of(c: &Connection, col: &str, since: i64, extra: &str) -> rusqlite::Result<Option<f64>> {
    let mut s = c.prepare(&format!("SELECT {col} FROM rounds WHERE at_ms >= ?1 AND level = 0 AND {col} IS NOT NULL {extra}"))?;
    let v: Vec<f64> = s.query_map([since], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(crate::probe::median(&v))
}

/// Medians of the last 7 days of good rounds, and of quick-check downloads (only quick checks are
/// compared with quick checks).
pub fn baseline(c: &Connection, now_ms: i64) -> rusqlite::Result<Baseline> {
    let since = now_ms - 7 * 86_400_000;
    let good: i64 = c.query_row("SELECT COUNT(*) FROM rounds WHERE at_ms >= ?1 AND level = 0", [since], |r| r.get(0))?;
    if good < BASELINE_MIN_ROUNDS {
        return Ok(Baseline::default());
    }
    let mut regions = [None; 8];
    for (i, slot) in regions.iter_mut().enumerate() {
        *slot = median_of(c, &format!("r{i}"), since, "")?;
    }
    let mut s = c.prepare("SELECT down_mbps FROM bandwidth WHERE at_ms >= ?1 AND trigger IN (0,1) AND down_mbps IS NOT NULL")?;
    let downs: Vec<f64> = s.query_map([since], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(Baseline {
        edge_ms: median_of(c, "edge_ms", since, "")?,
        dns_ms: median_of(c, "dns_ms", since, "")?,
        regions,
        down_mbps: (downs.len() >= 6).then(|| crate::probe::median(&downs)).flatten(),
    })
}

/// A quick-check download from the last 15 minutes, for the verdict's bandwidth check.
pub fn recent_scheduled_down(c: &Connection, now_ms: i64) -> rusqlite::Result<Option<f64>> {
    c.query_row(
        "SELECT down_mbps FROM bandwidth WHERE at_ms >= ?1 AND trigger IN (0,1) ORDER BY at_ms DESC LIMIT 1",
        [now_ms - 15 * 60_000],
        |r| r.get(0),
    )
    .optional()
    .map(Option::flatten)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verdict::{judge, tests::healthy};

    fn temp_db() -> (tempdir::Dir, Connection) {
        let d = tempdir::Dir::new();
        let c = open_at(&d.0.join("p.sqlite")).unwrap();
        (d, c)
    }

    pub mod tempdir {
        pub struct Dir(pub std::path::PathBuf);
        impl Dir {
            pub fn new() -> Dir {
                let p = std::env::temp_dir().join(format!("packet-test-{}-{}", std::process::id(), crate::time::now_ms() ^ rand_u64()));
                std::fs::create_dir_all(&p).unwrap();
                Dir(p)
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        fn rand_u64() -> i64 {
            use std::sync::atomic::{AtomicI64, Ordering};
            static N: AtomicI64 = AtomicI64::new(0);
            N.fetch_add(1, Ordering::Relaxed) << 40
        }
    }

    #[test]
    fn round_roundtrip() {
        let (_d, c) = temp_db();
        let mut r = healthy();
        r.at_ms = 1_000;
        r.link = LinkKind::WiFi;
        r.wifi = Some(WiFi { rssi: -60, noise: -92, tx_mbps: 866.0, channel: 149, band: 2 });
        let v = judge(&r, &Baseline::default(), None);
        insert_round(&c, &r, &v).unwrap();
        let s = latest_round(&c).unwrap().unwrap();
        assert_eq!(s.round, r);
        assert_eq!(s.verdict.level, v.level);
    }

    #[test]
    fn at_time_lookup() {
        let (_d, c) = temp_db();
        for t in [60_000, 120_000, 180_000] {
            let mut r = healthy();
            r.at_ms = t;
            insert_round(&c, &r, &judge(&r, &Baseline::default(), None)).unwrap();
        }
        assert_eq!(nearest_round(&c, 130_000, 300_000).unwrap().unwrap().round.at_ms, 120_000);
        assert_eq!(nearest_round(&c, 170_000, 300_000).unwrap().unwrap().round.at_ms, 180_000);
        assert!(nearest_round(&c, 10_000_000, 300_000).unwrap().is_none(), "outside the window");
    }

    #[test]
    fn strict_schema_refuses_text_in_numbers() {
        let (_d, c) = temp_db();
        let bad = c.execute("INSERT INTO bandwidth VALUES (1, 0, 'fast', NULL, 0, 0, NULL)", []);
        assert!(bad.is_err());
    }

    #[test]
    fn baseline_needs_history() {
        let (_d, c) = temp_db();
        assert_eq!(baseline(&c, 10_000_000).unwrap(), Baseline::default());
        for i in 0..40 {
            let mut r = healthy();
            r.at_ms = 1_000_000 + i * 60_000;
            insert_round(&c, &r, &judge(&r, &Baseline::default(), None)).unwrap();
        }
        let b = baseline(&c, 4_000_000).unwrap();
        assert_eq!(b.edge_ms, Some(12.0));
        assert_eq!(b.regions[7], Some(231.0));
    }

    #[test]
    fn prune_drops_old() {
        let (_d, c) = temp_db();
        let now = 200 * 86_400_000;
        for t in [now - 100 * 86_400_000, now - 1_000] {
            let mut r = healthy();
            r.at_ms = t;
            insert_round(&c, &r, &judge(&r, &Baseline::default(), None)).unwrap();
        }
        prune(&c, now).unwrap();
        assert_eq!(rounds_between(&c, 0, now).unwrap().len(), 1);
    }
}

#[cfg(test)]
mod permission_tests {
    #[test]
    fn store_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let dir = std::env::temp_dir().join(format!("packet-perm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = dir.join("packet.sqlite");
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        super::private_dir(&dir).unwrap();
        let c = super::open_at(&path).unwrap();
        let wal = dir.join("packet.sqlite-wal");
        let (d, f, w) = (mode(&dir), mode(&path), wal.exists().then(|| mode(&wal)));
        drop(c);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(d, 0o700, "data directory");
        assert_eq!(f, 0o600, "an existing store is tightened");
        assert!(w.is_none_or(|w| w == 0o600), "WAL file");
    }
}
