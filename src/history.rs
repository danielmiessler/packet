//! History for the chart window and `packet history`: rounds bucketed to a few hundred points.

use crate::probe::LinkKind;
use crate::round::round1;
use crate::store::{self, Stored};
use crate::verdict::Level;
use rusqlite::Connection;
use serde_json::{json, Value};

pub const RANGES: [(&str, i64); 4] = [("1h", 3_600_000), ("24h", 86_400_000), ("7d", 7 * 86_400_000), ("30d", 30 * 86_400_000)];
const TARGET_POINTS: i64 = 360;

pub fn range_ms(name: &str) -> Option<i64> {
    RANGES.iter().find(|(n, _)| *n == name).map(|(_, ms)| *ms)
}

fn mean(v: &[f64]) -> Option<f64> {
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

/// Rounds grouped into equal time buckets: mean latencies, worst loss and verdict, and the
/// link kind most of the bucket was on.
pub fn bucket(rounds: &[Stored], from: i64, to: i64) -> Vec<Value> {
    let width = ((to - from) / TARGET_POINTS).max(60_000);
    let mut out = Vec::new();
    let mut i = 0;
    let mut start = from;
    while start < to && i <= rounds.len() {
        let end = start + width;
        let mut j = i;
        while j < rounds.len() && rounds[j].round.at_ms < end {
            j += 1;
        }
        let b = &rounds[i..j];
        if !b.is_empty() {
            let col = |f: &dyn Fn(&Stored) -> Option<f64>| -> Vec<f64> { b.iter().filter_map(f).collect() };
            let loss = b
                .iter()
                .filter_map(|s| s.round.edge.loss.into_iter().chain(s.round.gateway.loss.filter(|_| s.round.gateway.ms.is_some())).reduce(f64::max))
                .fold(0.0, f64::max);
            let level = b.iter().map(|s| s.verdict.level).max().unwrap_or(Level::Good);
            let link = predominant(&b.iter().map(|s| s.round.link).collect::<Vec<_>>());
            let worst = b.iter().max_by_key(|s| s.verdict.level).expect("bucket not empty");
            let rssi = col(&|s| s.round.wifi.map(|w| w.rssi as f64));
            out.push(json!({
                "t": start + width / 2,
                "n": b.len(),
                "gw": round1(mean(&col(&|s| s.round.gateway.ms))),
                "edge": round1(mean(&col(&|s| s.round.edge.ms))),
                "dns": round1(mean(&col(&|s| s.round.dns_ms))),
                "far": round1(mean(&col(&|s| s.round.far_median()))),
                "loss": (loss * 1000.0).round() / 1000.0,
                "level": level.slug(),
                "layer": worst.verdict.at.slug(),
                "link": link.slug(),
                "rssi": round1(mean(&rssi)),
            }));
        }
        i = j;
        start = end;
    }
    out
}

/// The link most of a bucket's rounds were on; a tie goes to the later one.
fn predominant(links: &[LinkKind]) -> LinkKind {
    let count = |k: LinkKind| links.iter().filter(|&&l| l == k).count();
    links.iter().copied().max_by_key(|&k| count(k)).expect("bucket not empty")
}

/// Every switch between wired and wireless, at full resolution. Drops to no connection or a VPN
/// are not link changes; the verdict strip shows them.
pub fn link_changes(rounds: &[Stored]) -> Vec<Value> {
    let physical: Vec<&Stored> =
        rounds.iter().filter(|s| matches!(s.round.link, LinkKind::Ethernet | LinkKind::WiFi)).collect();
    physical
        .windows(2)
        .filter(|w| w[0].round.link != w[1].round.link)
        .map(|w| json!({ "t": w[1].round.at_ms, "from": w[0].round.link.slug(), "to": w[1].round.link.slug() }))
        .collect()
}

pub fn build(c: &Connection, range: &str, now_ms: i64) -> rusqlite::Result<Value> {
    let span = range_ms(range).unwrap_or(86_400_000);
    let width = (span / TARGET_POINTS).max(60_000);
    let from = now_ms - span;
    let rounds = store::rounds_between(c, from, now_ms)?;
    // Only full tests carry the line's real speed; quick checks show up through the verdict.
    let bw: Vec<Value> = store::bandwidth_between(c, from, now_ms)?
        .iter()
        .filter(|b| b.trigger.is_full())
        .map(|b| json!({ "t": b.at_ms, "down": round1(b.down_mbps), "up": round1(b.up_mbps), "rpm": b.rpm.map(f64::round), "trigger": b.trigger.slug() }))
        .collect();
    let bad = rounds.iter().filter(|s| s.verdict.level >= Level::Fair).count();
    Ok(json!({
        "range": range,
        "from": from,
        "to": now_ms,
        "rounds": rounds.len(),
        "width": width,
        "bad_rounds": bad,
        "buckets": bucket(&rounds, from, now_ms),
        "changes": link_changes(&rounds),
        "bandwidth": bw,
    }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn bucket_link_is_the_majority() {
        use super::{predominant, LinkKind::*};
        assert_eq!(predominant(&[Ethernet, Ethernet, Ethernet, WiFi]), Ethernet);
        assert_eq!(predominant(&[WiFi, WiFi, Ethernet]), WiFi);
        assert_eq!(predominant(&[WiFi, Ethernet]), Ethernet, "a tie goes to the later link");
    }

    use super::*;
    use crate::verdict::{judge, tests::healthy, Baseline};

    /// Fills a store with a made-up day for screenshots and UI work: a DNS stall, an evening on
    /// weak Wi-Fi, and an hour of provider trouble. Every number is synthetic.
    /// `PACKET_DATA_DIR=/tmp/packet-demo cargo test -- --ignored write_demo_store`
    #[test]
    #[ignore]
    fn write_demo_store() {
        use crate::probe::{Bandwidth, LinkKind, Trigger, WiFi};
        use crate::round::Layer;
        let dir = std::env::var("PACKET_DATA_DIR").expect("set PACKET_DATA_DIR to an empty scratch directory");
        assert!(!dir.contains("Application Support"), "refusing to write demo data into the real store");
        let path = std::path::Path::new(&dir).join("packet.sqlite");
        let _ = std::fs::remove_file(&path);
        let c = store::open_at(&path).unwrap();
        let now = crate::time::now_ms();
        let mut seed: u64 = 0x5eed;
        let mut rnd = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as f64 / (1u64 << 31) as f64
        };
        let regions = [12.0, 64.0, 138.0, 146.0, 112.0, 172.0, 182.0, 148.0];
        let base = Baseline { edge_ms: Some(8.0), dns_ms: Some(2.0), regions: regions.map(Some), down_mbps: Some(930.0) };
        c.execute_batch("BEGIN").unwrap();
        for i in 0..1440i64 {
            let mut r = healthy();
            r.at_ms = now - (1440 - i) * 60_000;
            r.colo = Some("LAX".into());
            r.gateway = Layer { ms: Some(0.5 + rnd() * 0.4), loss: Some(0.0), jitter: Some(0.1) };
            r.edge = Layer { ms: Some(7.0 + rnd() * 2.0), loss: Some(0.0), jitter: Some(0.4 + rnd() * 0.3) };
            r.dns_ms = Some(1.5 + rnd() * 1.5);
            r.regions = regions.map(|m| Some(m * (0.97 + rnd() * 0.06)));
            if (380..392).contains(&i) {
                r.dns_ms = Some(180.0 + rnd() * 90.0);
            }
            if (760..980).contains(&i) {
                r.link = LinkKind::WiFi;
                r.wifi = Some(WiFi { rssi: -71, noise: -92, tx_mbps: 286.0, channel: 44, band: 2 });
                r.gateway = Layer { ms: Some(4.0 + rnd() * 3.0), loss: Some(0.0), jitter: Some(1.5) };
                r.edge.ms = r.edge.ms.map(|m| m + 5.0);
                if (840..900).contains(&i) {
                    r.wifi = Some(WiFi { rssi: -82, noise: -90, tx_mbps: 52.0, channel: 44, band: 2 });
                    r.gateway = Layer { ms: Some(60.0 + rnd() * 60.0), loss: Some(0.2), jitter: Some(25.0) };
                    r.edge = Layer { ms: Some(90.0 + rnd() * 50.0), loss: Some(0.2), jitter: Some(30.0) };
                }
            }
            if (1180..1240).contains(&i) {
                r.edge = Layer { ms: Some(140.0 + rnd() * 60.0), loss: Some(0.15), jitter: Some(40.0) };
                r.regions = regions.map(|m| Some(m + 150.0 + rnd() * 40.0));
            }
            let v = judge(&r, &base, Some(930.0));
            store::insert_round(&c, &r, &v).unwrap();
        }
        for h in 0..24i64 {
            let trouble = h == 20;
            let b = Bandwidth {
                at_ms: now - (24 - h) * 3_600_000 + 1_800_000,
                trigger: Trigger::Hourly,
                down_mbps: Some(if trouble { 140.0 } else { 900.0 + rnd() * 60.0 }),
                up_mbps: Some(if trouble { 90.0 } else { 860.0 + rnd() * 50.0 }),
                down_bytes: 100_000_000,
                up_bytes: 40_000_000,
                rpm: None,
            };
            store::insert_bandwidth(&c, &b).unwrap();
        }
        let manual = Bandwidth {
            at_ms: now - 25 * 60_000,
            trigger: Trigger::Manual,
            down_mbps: Some(1840.0),
            up_mbps: Some(1910.0),
            down_bytes: 0,
            up_bytes: 0,
            rpm: Some(2400.0),
        };
        store::insert_bandwidth(&c, &manual).unwrap();
        c.execute_batch("COMMIT").unwrap();
    }

    #[test]
    fn history_render_time() {
        let dir = std::env::temp_dir().join(format!("packet-hist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("h.sqlite");
        let _ = std::fs::remove_file(&path);
        let c = store::open_at(&path).unwrap();
        let now = 100 * 86_400_000i64;
        let n = 30 * 1440; // 30 days of rounds
        c.execute_batch("BEGIN").unwrap();
        for i in 0..n {
            let mut r = healthy();
            r.at_ms = now - (n - i) * 60_000;
            if i % 5000 < 30 {
                r.link = LinkKind::WiFi;
            }
            let v = judge(&r, &Baseline::default(), None);
            store::insert_round(&c, &r, &v).unwrap();
        }
        c.execute_batch("COMMIT").unwrap();
        let t = std::time::Instant::now();
        let h = build(&c, "30d", now).unwrap();
        let took = t.elapsed();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(h["rounds"], n);
        let buckets = h["buckets"].as_array().unwrap().len();
        assert!((300..=400).contains(&buckets), "{buckets} buckets");
        assert!(!h["changes"].as_array().unwrap().is_empty());
        assert!(took.as_millis() < 1000, "30-day history built in {took:?}");
    }
}
