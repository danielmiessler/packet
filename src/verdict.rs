//! The verdict: which layer, if any, is making the connection bad. Layers are checked from the
//! Mac outward, and when two layers are equally bad the one closer to the Mac is named, because
//! a fault near the Mac shows up in everything past it.

use crate::probe::LinkKind;
use crate::round::Round;
use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Good = 0,
    Fair = 1,
    Poor = 2,
    Offline = 3,
}

impl Level {
    pub fn from_i64(v: i64) -> Level {
        match v {
            1 => Level::Fair,
            2 => Level::Poor,
            3 => Level::Offline,
            _ => Level::Good,
        }
    }
    pub fn slug(self) -> &'static str {
        match self {
            Level::Good => "good",
            Level::Fair => "fair",
            Level::Poor => "poor",
            Level::Offline => "offline",
        }
    }
    pub fn mark(self) -> &'static str {
        match self {
            Level::Good => "🟢",
            Level::Fair => "🟡",
            Level::Poor => "🔴",
            Level::Offline => "⚫",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Where {
    Nowhere = 0,
    WiFi = 1,
    Local = 2,
    Isp = 3,
    Dns = 4,
    Far = 5,
}

impl Where {
    pub fn from_i64(v: i64) -> Where {
        match v {
            1 => Where::WiFi,
            2 => Where::Local,
            3 => Where::Isp,
            4 => Where::Dns,
            5 => Where::Far,
            _ => Where::Nowhere,
        }
    }
    pub fn slug(self) -> &'static str {
        match self {
            Where::Nowhere => "none",
            Where::WiFi => "wifi",
            Where::Local => "local",
            Where::Isp => "isp",
            Where::Dns => "dns",
            Where::Far => "far",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Where::Nowhere => "Nothing",
            Where::WiFi => "Wi-Fi",
            Where::Local => "Local network",
            Where::Isp => "Internet provider",
            Where::Dns => "DNS",
            Where::Far => "Far internet",
        }
    }
}

/// Typical values learned from this Mac's own good rounds; None until there is enough history.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Baseline {
    pub edge_ms: Option<f64>,
    pub dns_ms: Option<f64>,
    pub regions: [Option<f64>; 8],
    pub down_mbps: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Verdict {
    pub level: Level,
    pub at: Where,
    pub sentence: String,
}

impl Verdict {
    pub fn to_json(&self) -> Value {
        json!({ "level": self.level.slug(), "layer": self.at.slug(), "layer_label": self.at.label(), "sentence": self.sentence })
    }
    /// The verdict a stored round was given.
    pub fn stored(level: i64, at: i64) -> Verdict {
        let (level, at) = (Level::from_i64(level), Where::from_i64(at));
        let sentence = match level {
            Level::Good => "Network was healthy.".to_string(),
            _ => format!("{} was {}.", at.label(), level.slug()),
        };
        Verdict { level, at, sentence }
    }
}

/// One layer's finding.
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub level: Level,
    pub at: Where,
    pub detail: String,
}

/// Loss is a share of five pings, so compare with room for floating-point error (1 - 4/5 is 0.1999…).
fn at_least(v: f64, t: f64) -> bool {
    v >= t - 1e-9
}

/// One bad round is often a single lost ping. The verdict changes to Fair or Poor only when the
/// round before was also bad; going offline, and getting better, show at once.
pub fn settle(raw: &Verdict, prev_raw: Option<&Verdict>) -> Verdict {
    let held = matches!(raw.level, Level::Fair | Level::Poor) && prev_raw.is_none_or(|p| p.level == Level::Good);
    if !held {
        return raw.clone();
    }
    Verdict {
        level: Level::Good,
        at: Where::Nowhere,
        sentence: format!(
            "Network is healthy. One round just now looked off ({}); watching the next.",
            raw.at.label().to_lowercase()
        ),
    }
}

fn ms(v: f64) -> String {
    if v < 10.0 { format!("{v:.1} ms") } else { format!("{v:.0} ms") }
}

fn pct(v: f64) -> String {
    format!("{:.0}%", v * 100.0)
}

/// Local link: the router, plus Wi-Fi signal when on Wi-Fi.
pub fn local_finding(r: &Round) -> Finding {
    let wifi = r.link == LinkKind::WiFi;
    let at = if wifi { Where::WiFi } else { Where::Local };
    let mut level = Level::Good;
    let mut why: Vec<String> = Vec::new();
    if let Some(w) = r.wifi.filter(|_| wifi) {
        // CoreWLAN reports noise 0 when it has no reading; then only the signal counts.
        let snr = (w.noise != 0).then(|| w.snr());
        let weak = |rssi_cut: i64, snr_cut: i64| w.rssi <= rssi_cut || snr.is_some_and(|s| s < snr_cut);
        let say = || match snr {
            Some(s) if w.rssi > -75 => format!("signal {} dBm but only {s} dB above the noise", w.rssi),
            _ => format!("signal {} dBm", w.rssi),
        };
        if weak(-82, 10) {
            level = Level::Poor;
            why.push(say());
        } else if weak(-75, 20) {
            level = Level::Fair;
            why.push(say());
        }
    }
    // A VPN's "router" is the far end of the tunnel, so it gets Wi-Fi-like slack on latency.
    let relaxed = wifi || r.link == LinkKind::Other;
    let (fair_ms, poor_ms) = if relaxed { (40.0, 100.0) } else { (10.0, 50.0) };
    // Five pings a round: 0.2 is one lost, 0.4 two.
    let (fair_loss, poor_loss) = (0.2, 0.4);
    // A router that answers no pings tells us nothing; only a router that answers badly is a finding.
    if r.gateway.ms.is_some() {
        let g = &r.gateway;
        let gms = g.ms.unwrap_or(0.0);
        let loss = g.loss.unwrap_or(0.0);
        let jitter = g.jitter.unwrap_or(0.0);
        let l = if gms > poor_ms || at_least(loss, poor_loss) {
            Level::Poor
        } else if gms > fair_ms || at_least(loss, fair_loss) || jitter > fair_ms {
            Level::Fair
        } else {
            Level::Good
        };
        if l > Level::Good {
            let via = if r.link == LinkKind::Other { "VPN gateway" } else { "router" };
            why.push(format!("{via} {} with {} loss", ms(gms), pct(loss)));
        }
        level = level.max(l);
    }
    Finding { level, at, detail: why.join(", ") }
}

pub fn isp_finding(r: &Round, b: &Baseline, last_down: Option<f64>) -> Finding {
    let at = Where::Isp;
    let Some(e) = r.edge.ms else {
        // Some networks block 1.1.1.1. If the far regions still answer, the line itself works.
        let level = if r.regions_up() * 2 >= r.regions.len() { Level::Fair } else { Level::Poor };
        let detail = if level == Level::Fair {
            "Cloudflare's edge is unreachable, though other regions answer"
        } else {
            "Cloudflare's edge is unreachable"
        };
        return Finding { level, at, detail: detail.into() };
    };
    let base = b.edge_ms.unwrap_or(20.0);
    let loss = r.edge.loss.unwrap_or(0.0);
    let jitter = r.edge.jitter.unwrap_or(0.0);
    let mut level = if at_least(loss, 0.4) || e > (4.0 * base).max(150.0) {
        Level::Poor
    } else if at_least(loss, 0.2) || e > (2.0 * base + 10.0).max(60.0) || jitter > 30.0 {
        Level::Fair
    } else {
        Level::Good
    };
    let mut detail = if level > Level::Good { format!("edge {} with {} loss", ms(e), pct(loss)) } else { String::new() };
    if let (Some(d), Some(bd)) = (last_down, b.down_mbps) {
        if d < 0.25 * bd {
            level = level.max(Level::Fair);
            if !detail.is_empty() {
                detail.push_str(", ");
            }
            detail.push_str(&format!("download {d:.0} Mbps against a usual {bd:.0}"));
        }
    }
    Finding { level, at, detail }
}

pub fn dns_finding(r: &Round, b: &Baseline) -> Finding {
    let at = Where::Dns;
    match r.dns_ms {
        None => Finding { level: Level::Poor, at, detail: "lookups are failing".into() },
        Some(d) if d > 300.0 => Finding { level: Level::Poor, at, detail: format!("lookups take {}", ms(d)) },
        Some(d) if d > 100.0 && d > 3.0 * b.dns_ms.unwrap_or(10.0) => {
            Finding { level: Level::Fair, at, detail: format!("lookups take {}", ms(d)) }
        }
        Some(_) => Finding { level: Level::Good, at, detail: String::new() },
    }
}

pub fn far_finding(r: &Round, b: &Baseline) -> Finding {
    let at = Where::Far;
    let n = r.regions.len();
    let down = n - r.regions_up();
    let slow = r
        .regions
        .iter()
        .zip(b.regions.iter())
        .filter(|(now, base)| matches!((now, base), (Some(x), Some(y)) if *x > y * 1.5 + 40.0))
        .count();
    if down * 2 >= n {
        Finding { level: Level::Poor, at, detail: format!("{down} of {n} regions unreachable") }
    } else if (down + slow) * 2 >= n {
        Finding { level: Level::Fair, at, detail: format!("{} of {n} regions slow or unreachable", down + slow) }
    } else {
        Finding { level: Level::Good, at, detail: String::new() }
    }
}

/// Each layer from the Mac outward: local link, internet provider, DNS, far internet.
pub fn findings(r: &Round, b: &Baseline, last_down: Option<f64>) -> [Finding; 4] {
    [local_finding(r), isp_finding(r, b, last_down), dns_finding(r, b), far_finding(r, b)]
}

pub fn judge(r: &Round, b: &Baseline, last_down: Option<f64>) -> Verdict {
    if r.link == LinkKind::None {
        return Verdict { level: Level::Offline, at: Where::Local, sentence: "Offline: no network connection.".into() };
    }
    let nothing_past_router = r.edge.ms.is_none() && r.regions_up() == 0;
    if nothing_past_router {
        let (at, sentence) = if r.gateway.ms.is_some() {
            (Where::Isp, "Offline: your router answers, but nothing past it does.".to_string())
        } else if r.link == LinkKind::WiFi {
            (Where::WiFi, "Offline: connected to Wi-Fi, but nothing answers.".to_string())
        } else {
            (Where::Local, "Offline: nothing answers past this Mac.".to_string())
        };
        return Verdict { level: Level::Offline, at, sentence };
    }
    let findings = findings(r, b, last_down);
    let worst = findings.iter().map(|f| f.level).max().unwrap_or(Level::Good);
    if worst == Level::Good {
        return Verdict {
            level: Level::Good,
            at: Where::Nowhere,
            sentence: "Network is healthy. If something feels slow, it isn't the network.".into(),
        };
    }
    let f = findings.iter().find(|f| f.level == worst).expect("worst level comes from a finding");
    let fine_past = match f.at {
        Where::WiFi | Where::Local if findings[1].level == Level::Good => " Past the router is fine.",
        Where::Isp if findings[0].level == Level::Good => " Your own network is fine.",
        Where::Dns | Where::Far if findings[0].level == Level::Good && findings[1].level == Level::Good => " The connection itself is fine.",
        _ => "",
    };
    let state = if worst == Level::Poor { "is bad" } else { "is struggling" };
    Verdict { level: worst, at: f.at, sentence: format!("{} {state}: {}.{fine_past}", f.at.label(), f.detail) }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::probe::WiFi;
    use crate::round::Layer;

    pub fn healthy() -> Round {
        let mut r = Round::empty(0);
        r.link = LinkKind::Ethernet;
        r.gateway = Layer { ms: Some(0.6), loss: Some(0.0), jitter: Some(0.1) };
        r.gateway_known = true;
        r.edge = Layer { ms: Some(12.0), loss: Some(0.0), jitter: Some(0.5) };
        r.colo = Some("SJC".into());
        r.dns_ms = Some(6.0);
        r.regions = [Some(28.0), Some(84.0), Some(164.0), Some(170.0), Some(150.0), Some(180.0), Some(210.0), Some(231.0)];
        r
    }

    fn base() -> Baseline {
        Baseline {
            edge_ms: Some(12.0),
            dns_ms: Some(6.0),
            regions: [Some(28.0), Some(84.0), Some(164.0), Some(170.0), Some(150.0), Some(180.0), Some(210.0), Some(231.0)],
            down_mbps: Some(370.0),
        }
    }

    #[test]
    fn verdict_layers() {
        let b = base();
        assert_eq!(judge(&healthy(), &b, Some(360.0)).at, Where::Nowhere);

        let mut local = healthy();
        local.gateway = Layer { ms: Some(70.0), loss: Some(0.2), jitter: Some(30.0) };
        local.edge = Layer { ms: Some(80.0), loss: Some(0.2), jitter: Some(30.0) };
        let v = judge(&local, &b, None);
        assert_eq!((v.level, v.at), (Level::Poor, Where::Local), "{}", v.sentence);

        let mut isp = healthy();
        isp.edge = Layer { ms: Some(90.0), loss: Some(0.04), jitter: Some(5.0) };
        assert_eq!(judge(&isp, &b, None).at, Where::Isp);

        let mut slow_bw = healthy();
        let v = judge(&slow_bw, &b, Some(40.0));
        assert_eq!((v.level, v.at), (Level::Fair, Where::Isp), "{}", v.sentence);
        slow_bw.dns_ms = Some(500.0);
        assert_eq!(judge(&slow_bw, &b, Some(40.0)).at, Where::Dns, "DNS poor outranks bandwidth fair");

        let mut dns = healthy();
        dns.dns_ms = None;
        assert_eq!(judge(&dns, &b, None).at, Where::Dns);

        let mut far = healthy();
        far.regions = [Some(28.0), Some(84.0), None, None, None, None, Some(210.0), Some(231.0)];
        assert_eq!(judge(&far, &b, None).at, Where::Far);
    }

    #[test]
    fn verdict_wifi_fault() {
        let mut r = healthy();
        r.link = LinkKind::WiFi;
        r.wifi = Some(WiFi { rssi: -84, noise: -92, tx_mbps: 24.0, channel: 36, band: 2 });
        r.gateway = Layer { ms: Some(60.0), loss: Some(0.3), jitter: Some(40.0) };
        r.edge = Layer { ms: Some(75.0), loss: Some(0.3), jitter: Some(40.0) };
        let v = judge(&r, &base(), None);
        assert_eq!((v.level, v.at), (Level::Poor, Where::WiFi), "{}", v.sentence);
        assert!(v.sentence.contains("-84 dBm"));
    }

    #[test]
    fn weak_wifi_alone_is_fair() {
        let mut r = healthy();
        r.link = LinkKind::WiFi;
        r.wifi = Some(WiFi { rssi: -77, noise: -95, tx_mbps: 144.0, channel: 36, band: 2 });
        r.gateway = Layer { ms: Some(4.0), loss: Some(0.0), jitter: Some(1.0) };
        let v = judge(&r, &base(), None);
        assert_eq!((v.level, v.at), (Level::Fair, Where::WiFi));
        assert!(v.sentence.contains("Past the router is fine"));
    }

    #[test]
    fn silent_router_is_not_a_fault() {
        let mut r = healthy();
        r.gateway = Layer { ms: None, loss: Some(1.0), jitter: None };
        assert_eq!(judge(&r, &base(), None).level, Level::Good);
    }

    #[test]
    fn icmp_blocked_edge_uses_tcp() {
        let mut r = healthy();
        r.edge = Layer { ms: Some(14.0), loss: None, jitter: None };
        assert_eq!(judge(&r, &base(), None).level, Level::Good);
    }

    #[test]
    fn verdict_offline() {
        let b = base();
        let mut r = Round::empty(0);
        assert_eq!(judge(&r, &b, None).level, Level::Offline, "no route");
        r.link = LinkKind::WiFi;
        assert_eq!(judge(&r, &b, None).level, Level::Offline, "associated, nothing answers");
        r.gateway = Layer { ms: Some(2.0), loss: Some(0.0), jitter: Some(0.1) };
        let v = judge(&r, &b, None);
        assert_eq!((v.level, v.at), (Level::Offline, Where::Isp));
    }

    #[test]
    fn fine_only_when_measured_fine() {
        let mut r = healthy();
        r.gateway.ms = Some(20.0);
        r.edge.loss = Some(0.4);
        let f = findings(&r, &base(), None);
        let v = judge(&r, &base(), None);
        assert_ne!(f[0].level, Level::Good, "slow router is a local fault");
        assert!(!v.sentence.contains("Your own network is fine"), "{}", v.sentence);
    }

    #[test]
    fn settle_holds_one_blip() {
        let good = judge(&healthy(), &base(), None);
        let mut lossy = healthy();
        lossy.edge.loss = Some(0.4);
        let bad = judge(&lossy, &base(), None);
        assert_eq!(bad.level, Level::Poor);
        assert_eq!(settle(&bad, None).level, Level::Good);
        assert_eq!(settle(&bad, Some(&good)).level, Level::Good);
        assert_eq!(settle(&bad, Some(&bad)).level, Level::Poor);
        let off = judge(&Round::empty(0), &base(), None);
        assert_eq!(settle(&off, Some(&good)).level, Level::Offline, "offline shows at once");
        assert_eq!(settle(&good, Some(&bad)).level, Level::Good, "recovery shows at once");
    }

    #[test]
    fn one_lost_ping_is_fair_not_poor() {
        for wifi in [false, true] {
            let mut r = healthy();
            if wifi {
                r.link = LinkKind::WiFi;
                r.wifi = Some(WiFi { rssi: -55, noise: -95, tx_mbps: 866.0, channel: 149, band: 2 });
                r.gateway = Layer { ms: Some(4.0), loss: Some(1.0 - 4.0 / 5.0), jitter: Some(1.0) };
            } else {
                r.edge.loss = Some(1.0 - 4.0 / 5.0);
            }
            assert_eq!(judge(&r, &base(), None).level, Level::Fair, "wifi={wifi}");
        }
    }

    #[test]
    fn blocked_edge_with_regions_is_fair() {
        let mut r = healthy();
        r.edge = Layer::default();
        assert_eq!(judge(&r, &base(), None).level, Level::Fair);
        r.regions = [None; 8];
        r.regions[0] = Some(30.0);
        assert_eq!(judge(&r, &base(), None).level, Level::Poor);
    }

    #[test]
    fn unknown_noise_is_not_bad_signal() {
        let mut r = healthy();
        r.link = LinkKind::WiFi;
        r.wifi = Some(WiFi { rssi: -55, noise: 0, tx_mbps: 866.0, channel: 149, band: 2 });
        r.gateway = Layer { ms: Some(4.0), loss: Some(0.0), jitter: Some(1.0) };
        assert_eq!(judge(&r, &base(), None).level, Level::Good);
    }

    #[test]
    fn vpn_gateway_gets_slack() {
        let mut r = healthy();
        r.link = LinkKind::Other;
        r.gateway = Layer { ms: Some(30.0), loss: Some(0.0), jitter: Some(2.0) };
        assert_eq!(judge(&r, &base(), None).level, Level::Good);
    }

    #[test]
    fn no_baseline_still_judges() {
        let v = judge(&healthy(), &Baseline::default(), None);
        assert_eq!(v.level, Level::Good);
    }
}
