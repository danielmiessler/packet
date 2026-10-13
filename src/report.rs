//! Plain-words rows shared by the menu and the CLI.

use crate::probe::{Bandwidth, LinkKind, REGIONS};
use crate::round::Round;
use crate::verdict::Finding;

fn ms(v: f64) -> String {
    if v < 10.0 { format!("{v:.1} ms") } else { format!("{v:.0} ms") }
}

fn loss(v: Option<f64>) -> String {
    v.map(|l| format!(" · {:.0}% loss", l * 100.0)).unwrap_or_default()
}

pub fn link_row(r: &Round) -> String {
    let router = match r.gateway.ms {
        Some(g) => format!("router {}{}", ms(g), loss(r.gateway.loss)),
        None if r.gateway_known => "router doesn't answer pings".into(),
        None => "no router".into(),
    };
    match (r.link, r.wifi) {
        (LinkKind::WiFi, Some(w)) => {
            let band = w.band_label();
            let band = if band.is_empty() { String::new() } else { format!(" · {band} ch {}", w.channel) };
            format!("Wi-Fi {} dBm · {:.0} Mbps{band} · {router}", w.rssi, w.tx_mbps)
        }
        (LinkKind::None, _) => "No network connection".into(),
        (k, _) => format!("{} · {router}", k.label()),
    }
}

pub fn edge_row(r: &Round) -> String {
    let colo = r.colo.as_deref().map(|c| format!(" {c}")).unwrap_or_default();
    match r.edge.ms {
        Some(e) => {
            let jitter = r.edge.jitter.map(|j| format!(" · jitter {}", ms(j))).unwrap_or_default();
            format!("Internet · Cloudflare{colo} {}{}{jitter}", ms(e), loss(r.edge.loss))
        }
        None => "Internet · Cloudflare unreachable".into(),
    }
}

pub fn dns_row(r: &Round) -> String {
    match r.dns_ms {
        Some(d) => format!("DNS lookups {}", ms(d)),
        None => "DNS lookups failing".into(),
    }
}

pub fn world_row(r: &Round) -> String {
    match r.far_median() {
        Some(m) => format!("World · {} of {} regions · median {}", r.regions_up(), REGIONS.len(), ms(m)),
        None => "World · no region reachable".into(),
    }
}

pub fn region_rows(r: &Round) -> Vec<String> {
    REGIONS
        .iter()
        .zip(r.regions.iter())
        .map(|(g, v)| match v {
            Some(m) => format!("{}  {}", ms(*m), g.name),
            None => format!("—  {} unreachable", g.name),
        })
        .collect()
}

pub fn region_line(r: &Round) -> String {
    REGIONS
        .iter()
        .zip(r.regions.iter())
        .map(|(g, v)| format!("{} {}", g.code, v.map_or("—".to_string(), |m| format!("{m:.0}"))))
        .collect::<Vec<_>>()
        .join(" · ")
        + " ms"
}

fn mbps(v: Option<f64>) -> String {
    v.map_or("—".to_string(), |x| format!("{x:.0}"))
}

/// Any test, labelled with its kind (used by `packet at`).
pub fn bandwidth_row(b: Option<&Bandwidth>, now_ms: i64) -> String {
    match b {
        None => "Bandwidth · not measured yet".into(),
        Some(b) if b.trigger.is_full() => line_speed_row(Some(b), now_ms),
        Some(b) => format!("Quick check ⬇ {} ⬆ {} Mbps (reads low on fast lines) · {}", mbps(b.down_mbps), mbps(b.up_mbps), crate::time::ago(now_ms, b.at_ms)),
    }
}

pub fn line_speed_row(b: Option<&Bandwidth>, now_ms: i64) -> String {
    let ago = |b: &Bandwidth| crate::time::ago(now_ms, b.at_ms);
    match b {
        None => "Line speed · not measured yet: use Test Now".into(),
        Some(b) if b.rpm.is_some() => format!(
            "Capacity ⬇ {} ⬆ {} Mbps · responsiveness {} · Test Now {}",
            mbps(b.down_mbps),
            mbps(b.up_mbps),
            b.rpm.map(|r| format!("{} ({r:.0} RPM)", crate::probe::rpm_label(r))).unwrap_or_default(),
            ago(b)
        ),
        Some(b) => {
            let kind = if b.trigger == crate::probe::Trigger::Manual { "Test Now" } else { "hourly test" };
            format!("Cloudflare ⬇ {} ⬆ {} Mbps · {kind} {}", mbps(b.down_mbps), mbps(b.up_mbps), ago(b))
        }
    }
}

/// The quick check against its own normal.
pub fn quick_row(q: Option<&Bandwidth>, share: Option<f64>, now_ms: i64) -> String {
    match (q, share) {
        (None, _) => "Quick check · every 10 minutes".into(),
        (Some(q), None) => format!("Quick check · learning what's normal · {}", crate::time::ago(now_ms, q.at_ms)),
        (Some(q), Some(s)) if s < 0.25 => format!("Quick check · download {:.0}% of usual · {}", s * 100.0, crate::time::ago(now_ms, q.at_ms)),
        (Some(q), Some(_)) => format!("Quick check · normal · {}", crate::time::ago(now_ms, q.at_ms)),
    }
}

pub fn mark(f: &Finding) -> &'static str {
    f.level.mark()
}

