//! One measurement round: every layer, measured in parallel, in about three seconds.

use crate::probe::{self, Link, LinkKind, PingStats, REGIONS, WiFi};
use crate::verdict::Verdict;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::time::Duration;

const PINGS: u32 = 5;
const PING_GAP: Duration = Duration::from_millis(200);
const PING_TIMEOUT: Duration = Duration::from_millis(1000);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layer {
    pub ms: Option<f64>,
    pub loss: Option<f64>,
    pub jitter: Option<f64>,
}

impl Layer {
    pub fn from_ping(p: &PingStats) -> Layer {
        Layer { ms: p.median(), loss: p.loss(), jitter: p.jitter() }
    }
    fn to_json(&self) -> Value {
        json!({ "ms": round1(self.ms), "loss": self.loss.map(|l| (l * 1000.0).round() / 1000.0), "jitter_ms": round1(self.jitter) })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Round {
    pub at_ms: i64,
    pub link: LinkKind,
    pub wifi: Option<WiFi>,
    /// Router. `ms` is None when the router does not answer pings (many don't); that alone is no fault.
    pub gateway: Layer,
    pub gateway_known: bool,
    /// Nearest Cloudflare edge. `loss` is None when ICMP is blocked and `ms` came from a TCP handshake.
    pub edge: Layer,
    pub colo: Option<String>,
    pub dns_ms: Option<f64>,
    pub regions: [Option<f64>; 8],
    pub expensive: bool,
}

impl Round {
    pub fn empty(at_ms: i64) -> Round {
        Round {
            at_ms,
            link: LinkKind::None,
            wifi: None,
            gateway: Layer::default(),
            gateway_known: false,
            edge: Layer::default(),
            colo: None,
            dns_ms: None,
            regions: [None; 8],
            expensive: false,
        }
    }

    pub fn far_median(&self) -> Option<f64> {
        let v: Vec<f64> = self.regions.iter().flatten().copied().collect();
        probe::median(&v)
    }

    pub fn regions_up(&self) -> usize {
        self.regions.iter().filter(|r| r.is_some()).count()
    }

    pub fn to_json(&self, verdict: &Verdict) -> Value {
        let regions: Vec<Value> = REGIONS
            .iter()
            .zip(self.regions.iter())
            .map(|(r, ms)| json!({ "code": r.code, "name": r.name, "continent": r.continent, "ms": round1(*ms) }))
            .collect();
        json!({
            "at": crate::time::iso_local(self.at_ms),
            "at_ms": self.at_ms,
            "verdict": verdict.to_json(),
            "link": {
                "kind": self.link.slug(),
                "label": self.link.label(),
                "wifi": self.wifi.map(|w| json!({
                    "rssi_dbm": w.rssi, "noise_dbm": w.noise, "snr_db": w.snr(),
                    "tx_mbps": w.tx_mbps, "channel": w.channel, "band": w.band_label(),
                })),
            },
            "gateway": self.gateway.to_json(),
            "edge": { "colo": self.colo, "ms": round1(self.edge.ms), "loss": self.edge.loss, "jitter_ms": round1(self.edge.jitter) },
            "dns": { "ms": round1(self.dns_ms) },
            "regions": regions,
            "path_expensive": self.expensive,
        })
    }
}

pub fn round1(v: Option<f64>) -> Option<f64> {
    v.map(|x| (x * 10.0).round() / 10.0)
}

/// Measures every layer in parallel.
pub fn run(at_ms: i64, seq: u64) -> Round {
    let link: Link = probe::link();
    let (expensive, constrained) = crate::netpath::state();
    let mut r = Round::empty(at_ms);
    r.link = link.kind;
    r.wifi = link.wifi;
    r.expensive = expensive || constrained;
    if link.kind == LinkKind::None {
        return r;
    }
    std::thread::scope(|s| {
        let gw = link.gateway.map(|g| s.spawn(move || probe::ping(g, PINGS, PING_GAP, PING_TIMEOUT)));
        let edge = s.spawn(|| probe::ping(probe::EDGE_IP, PINGS, PING_GAP, PING_TIMEOUT));
        let colo = s.spawn(probe::edge_colo);
        let dns = s.spawn(move || probe::dns_ms(seq));
        let regions: Vec<_> = REGIONS
            .iter()
            .map(|reg| s.spawn(move || probe::connect_ms(reg.host, 2, Duration::from_secs(2))))
            .collect();

        if let Some(h) = gw {
            r.gateway_known = true;
            r.gateway = Layer::from_ping(&h.join().unwrap_or_default());
        }
        let e = edge.join().unwrap_or_default();
        r.edge = if e.answered() {
            Layer::from_ping(&e)
        } else {
            // ICMP may be filtered; a TCP handshake still shows whether the edge is reachable.
            // Reachable over TCP means ICMP is filtered and loss is unknown; unreachable is total loss.
            let addr = SocketAddr::from((probe::EDGE_IP, 443));
            let ms = probe::connect_addr_ms(addr, 2, Duration::from_secs(2));
            Layer { ms, loss: if ms.is_some() { None } else { Some(1.0) }, jitter: None }
        };
        r.colo = colo.join().ok().flatten();
        r.dns_ms = dns.join().ok().flatten();
        for (slot, h) in r.regions.iter_mut().zip(regions) {
            *slot = h.join().ok().flatten();
        }
    });
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verdict::{judge, Baseline};

    #[test]
    fn wifi_fields() {
        let mut r = Round::empty(1);
        r.link = LinkKind::WiFi;
        r.wifi = Some(WiFi { rssi: -61, noise: -94, tx_mbps: 866.0, channel: 149, band: 2 });
        let j = r.to_json(&judge(&r, &Baseline::default(), None));
        let w = &j["link"]["wifi"];
        assert_eq!(j["link"]["kind"], "wifi");
        assert_eq!((w["rssi_dbm"].as_i64(), w["noise_dbm"].as_i64(), w["snr_db"].as_i64()), (Some(-61), Some(-94), Some(33)));
        assert_eq!((w["tx_mbps"].as_f64(), w["channel"].as_i64(), w["band"].as_str()), (Some(866.0), Some(149), Some("5 GHz")));
        // Ethernet rounds carry no Wi-Fi block.
        r.link = LinkKind::Ethernet;
        r.wifi = None;
        assert!(r.to_json(&judge(&r, &Baseline::default(), None))["link"]["wifi"].is_null());
    }
}
