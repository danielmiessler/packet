//! Measurements. Every host Packet talks to is named in this file (see `scripts/probe-hosts.sh`).

use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const EDGE_IP: Ipv4Addr = Ipv4Addr::new(1, 1, 1, 1);
pub const TRACE_URL: &str = "https://1.1.1.1/cdn-cgi/trace";
pub const DOWN_URL: &str = "https://speed.cloudflare.com/__down?bytes=";
pub const UP_URL: &str = "https://speed.cloudflare.com/__up";
const CURL: &str = "/usr/bin/curl";

/// Far regions. Cloudflare is anycast and always answers from the nearest edge, so far latency
/// comes from AWS regional endpoints, whose location is fixed. Order is the storage order (r0..r7).
pub const REGIONS: [Region; 8] = [
    Region { code: "USW", name: "US West (N. California)", host: "dynamodb.us-west-1.amazonaws.com", continent: "North America" },
    Region { code: "USE", name: "US East (Virginia)", host: "dynamodb.us-east-1.amazonaws.com", continent: "North America" },
    Region { code: "LON", name: "London", host: "dynamodb.eu-west-2.amazonaws.com", continent: "Europe" },
    Region { code: "FRA", name: "Frankfurt", host: "dynamodb.eu-central-1.amazonaws.com", continent: "Europe" },
    Region { code: "TYO", name: "Tokyo", host: "dynamodb.ap-northeast-1.amazonaws.com", continent: "Asia" },
    Region { code: "SIN", name: "Singapore", host: "dynamodb.ap-southeast-1.amazonaws.com", continent: "Asia" },
    Region { code: "SAO", name: "São Paulo", host: "dynamodb.sa-east-1.amazonaws.com", continent: "South America" },
    Region { code: "SYD", name: "Sydney", host: "dynamodb.ap-southeast-2.amazonaws.com", continent: "Oceania" },
];

/// Popular names for the DNS timing, rotated each round. The system resolver usually has them
/// cached, so the time is what every app waits for on a lookup.
const DNS_NAMES: [&str; 4] = ["www.apple.com", "www.google.com", "www.cloudflare.com", "github.com"];

#[derive(Clone, Copy, Debug)]
pub struct Region {
    pub code: &'static str,
    pub name: &'static str,
    pub host: &'static str,
    pub continent: &'static str,
}

// ---------- link ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    None = 0,
    Ethernet = 1,
    WiFi = 2,
    Other = 3,
}

impl LinkKind {
    pub fn from_i64(v: i64) -> LinkKind {
        match v {
            1 => LinkKind::Ethernet,
            2 => LinkKind::WiFi,
            3 => LinkKind::Other,
            _ => LinkKind::None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            LinkKind::None => "No connection",
            LinkKind::Ethernet => "Ethernet",
            LinkKind::WiFi => "Wi-Fi",
            LinkKind::Other => "VPN or other",
        }
    }
    pub fn slug(self) -> &'static str {
        match self {
            LinkKind::None => "none",
            LinkKind::Ethernet => "ethernet",
            LinkKind::WiFi => "wifi",
            LinkKind::Other => "other",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WiFi {
    pub rssi: i64,
    pub noise: i64,
    pub tx_mbps: f64,
    pub channel: i64,
    /// 1 = 2.4 GHz, 2 = 5 GHz, 3 = 6 GHz (CWChannelBand).
    pub band: i64,
}

impl WiFi {
    pub fn snr(&self) -> i64 {
        self.rssi - self.noise
    }
    pub fn band_label(&self) -> &'static str {
        match self.band {
            1 => "2.4 GHz",
            2 => "5 GHz",
            3 => "6 GHz",
            _ => "",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Link {
    pub kind: LinkKind,
    pub gateway: Option<Ipv4Addr>,
    pub wifi: Option<WiFi>,
}

/// Parses `route -n get default`.
pub fn parse_route(out: &str) -> (Option<Ipv4Addr>, Option<String>) {
    let mut gw = None;
    let mut iface = None;
    for line in out.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("gateway:") {
            gw = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("interface:") {
            iface = Some(v.trim().to_string());
        }
    }
    (gw, iface)
}

pub fn classify(iface: Option<&str>, wifi_iface: Option<&str>) -> LinkKind {
    match iface {
        None => LinkKind::None,
        Some(i) if Some(i) == wifi_iface => LinkKind::WiFi,
        Some(i) if i.starts_with("en") || i.starts_with("bridge") => LinkKind::Ethernet,
        Some(_) => LinkKind::Other,
    }
}

pub fn link() -> Link {
    let out = Command::new("/sbin/route")
        .args(["-n", "get", "default"])
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let (gateway, iface) = parse_route(&out);
    let (wifi_iface, wifi) = crate::wlan::current();
    let kind = classify(iface.as_deref(), wifi_iface.as_deref());
    Link { kind, gateway, wifi: if kind == LinkKind::WiFi { wifi } else { None } }
}

// ---------- ICMP ----------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PingStats {
    pub sent: u32,
    pub rtts: Vec<f64>,
}

impl PingStats {
    pub fn recv(&self) -> u32 {
        self.rtts.len() as u32
    }
    pub fn loss(&self) -> Option<f64> {
        (self.sent > 0).then(|| 1.0 - self.recv() as f64 / self.sent as f64)
    }
    pub fn median(&self) -> Option<f64> {
        median(&self.rtts)
    }
    /// Mean absolute difference between consecutive replies.
    pub fn jitter(&self) -> Option<f64> {
        if self.rtts.len() < 2 {
            return None;
        }
        let d: f64 = self.rtts.windows(2).map(|w| (w[1] - w[0]).abs()).sum();
        Some(d / (self.rtts.len() - 1) as f64)
    }
    pub fn answered(&self) -> bool {
        !self.rtts.is_empty()
    }
}

pub fn median(v: &[f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let n = s.len();
    Some(if n % 2 == 1 { s[n / 2] } else { (s[n / 2 - 1] + s[n / 2]) / 2.0 })
}

fn checksum(b: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    for c in b.chunks(2) {
        let w = if c.len() == 2 { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], 0]) };
        sum += w as u32;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

pub fn echo_request(id: u16, seq: u16, token: [u8; 8]) -> Vec<u8> {
    let mut p = vec![8u8, 0, 0, 0];
    p.extend_from_slice(&id.to_be_bytes());
    p.extend_from_slice(&seq.to_be_bytes());
    p.extend_from_slice(&token);
    let c = checksum(&p);
    p[2..4].copy_from_slice(&c.to_be_bytes());
    p
}

/// True when `buf` is the echo reply to `seq` carrying `token`. macOS hands datagram ICMP
/// sockets the IP header too, so it is skipped when present.
pub fn is_reply(buf: &[u8], seq: u16, token: [u8; 8]) -> bool {
    let icmp = if !buf.is_empty() && buf[0] >> 4 == 4 {
        let ihl = (buf[0] & 0x0f) as usize * 4;
        if buf.len() < ihl {
            return false;
        }
        &buf[ihl..]
    } else {
        buf
    };
    icmp.len() >= 16 && icmp[0] == 0 && u16::from_be_bytes([icmp[6], icmp[7]]) == seq && icmp[8..16] == token
}

/// A per-call nonce. The clock alone is not enough: macOS reads it in microseconds, so the router
/// and edge pings, started together, could share a token and take each other's replies.
fn token() -> [u8; 8] {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos() as u64;
    (t ^ ((std::process::id() as u64) << 32) ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15)).to_be_bytes()
}

/// Unprivileged ICMP echo (SOCK_DGRAM), one request at a time.
pub fn ping(addr: Ipv4Addr, count: u32, gap: Duration, timeout: Duration) -> PingStats {
    let mut stats = PingStats::default();
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, libc::IPPROTO_ICMP) };
    if fd < 0 {
        return stats;
    }
    let mut sa: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    sa.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
    sa.sin_family = libc::AF_INET as u8;
    sa.sin_addr = libc::in_addr { s_addr: u32::from(addr).to_be() };
    let tok = token();
    let id = (std::process::id() & 0xffff) as u16;
    for seq in 0..count as u16 {
        let started = Instant::now();
        let pkt = echo_request(id, seq, tok);
        let sent = unsafe {
            libc::sendto(
                fd,
                pkt.as_ptr() as *const _,
                pkt.len(),
                0,
                &sa as *const libc::sockaddr_in as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_in>() as u32,
            )
        };
        stats.sent += 1;
        if sent >= 0 {
            let mut buf = [0u8; 1500];
            loop {
                let left = timeout.saturating_sub(started.elapsed());
                if left.is_zero() {
                    break;
                }
                let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
                let r = unsafe { libc::poll(&mut pfd, 1, left.as_millis().max(1) as i32) };
                if r <= 0 {
                    break;
                }
                let mut from: libc::sockaddr_in = unsafe { std::mem::zeroed() };
                let mut flen = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
                let n = unsafe {
                    libc::recvfrom(fd, buf.as_mut_ptr() as *mut _, buf.len(), 0, &mut from as *mut libc::sockaddr_in as *mut libc::sockaddr, &mut flen)
                };
                let from_target = from.sin_addr.s_addr == sa.sin_addr.s_addr;
                if n > 0 && from_target && is_reply(&buf[..n as usize], seq, tok) {
                    stats.rtts.push(started.elapsed().as_secs_f64() * 1000.0);
                    break;
                }
            }
        }
        if (seq as u32) + 1 < count {
            std::thread::sleep(gap.saturating_sub(started.elapsed()));
        }
    }
    unsafe { libc::close(fd) };
    stats
}

// ---------- TCP, DNS, edge ----------

/// A system lookup with a deadline: getaddrinfo has none, and a stuck resolver would otherwise
/// hold the whole round. The lookup thread is left to finish on its own.
pub fn resolve_v4(host: &str, timeout: Duration) -> Option<SocketAddr> {
    let (tx, rx) = std::sync::mpsc::channel();
    let host = host.to_string();
    std::thread::Builder::new()
        .name("packet-resolve".into())
        .spawn(move || {
            let addr = (host.as_str(), 443).to_socket_addrs().ok().and_then(|mut a| a.find(|a| a.is_ipv4()));
            let _ = tx.send(addr);
        })
        .ok()?;
    rx.recv_timeout(timeout).ok().flatten()
}

/// TCP handshake time to `host:443`, best of `tries`. DNS time is excluded.
pub fn connect_ms(host: &str, tries: u32, timeout: Duration) -> Option<f64> {
    let addr = resolve_v4(host, Duration::from_secs(2))?;
    connect_addr_ms(addr, tries, timeout)
}

pub fn connect_addr_ms(addr: SocketAddr, tries: u32, timeout: Duration) -> Option<f64> {
    let mut best: Option<f64> = None;
    for _ in 0..tries {
        let t = Instant::now();
        if TcpStream::connect_timeout(&addr, timeout).is_ok() {
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            best = Some(best.map_or(ms, |b: f64| b.min(ms)));
        }
    }
    best
}

/// The first IPv4 nameserver in /etc/resolv.conf (macOS writes it from the active resolver).
pub fn parse_resolver(conf: &str) -> Option<Ipv4Addr> {
    conf.lines()
        .filter_map(|l| l.trim().strip_prefix("nameserver"))
        .find_map(|v| v.trim().parse::<Ipv4Addr>().ok())
}

pub fn dns_query(id: u16, name: &str) -> Vec<u8> {
    let mut q = Vec::with_capacity(32 + name.len());
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in name.split('.') {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.extend_from_slice(&[0, 0, 1, 0, 1]);
    q
}

/// Our reply: Some(true) answered, Some(false) the resolver refused or failed, None not ours.
/// Ours means a standard-query response with our ID that repeats our question.
pub fn dns_reply(buf: &[u8], query: &[u8]) -> Option<bool> {
    let ours = buf.len() >= query.len()
        && buf[..2] == query[..2]
        && buf[2] & 0x80 != 0
        && buf[2] & 0x78 == 0
        && buf[4..6] == query[4..6]
        && buf[12..query.len()] == query[12..];
    ours.then(|| buf[3] & 0x0f == 0)
}

/// Time for the system's resolver to answer a lookup, sent straight to it so the local cache is skipped.
/// With no IPv4 resolver to ask directly (IPv6-only, encrypted DNS), it times a system lookup.
pub fn dns_ms(round: u64) -> Option<f64> {
    let name = DNS_NAMES[(round % DNS_NAMES.len() as u64) as usize];
    let conf = std::fs::read_to_string("/etc/resolv.conf").unwrap_or_default();
    let Some(server) = parse_resolver(&conf) else {
        let t = Instant::now();
        return resolve_v4(name, Duration::from_secs(2)).map(|_| t.elapsed().as_secs_f64() * 1000.0);
    };
    // Connected, so only the resolver's address can answer; a random ID, so nobody can guess it.
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect((IpAddr::V4(server), 53)).ok()?;
    sock.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let query = dns_query(unsafe { libc::arc4random() } as u16, name);
    let t = Instant::now();
    sock.send(&query).ok()?;
    let mut buf = [0u8; 1500];
    loop {
        let n = sock.recv(&mut buf).ok()?;
        match dns_reply(&buf[..n], &query) {
            Some(true) => return Some(t.elapsed().as_secs_f64() * 1000.0),
            Some(false) => return None,
            None => {}
        }
        if t.elapsed() > Duration::from_secs(2) {
            return None;
        }
    }
}

pub fn parse_colo(trace: &str) -> Option<String> {
    trace
        .lines()
        .find_map(|l| l.strip_prefix("colo="))
        .map(|c| c.trim().to_string())
        .filter(|c| (2..=5).contains(&c.len()) && c.chars().all(|ch| ch.is_ascii_alphanumeric()))
}

/// The trace page is a few hundred bytes; reading at most this much keeps a hostile endpoint
/// from filling memory.
const TRACE_CAP: u64 = 4096;

fn capped(r: impl std::io::Read) -> Vec<u8> {
    use std::io::Read;
    let mut body = Vec::new();
    let _ = r.take(TRACE_CAP).read_to_end(&mut body);
    body
}

/// The Cloudflare data center answering for this connection.
pub fn edge_colo() -> Option<String> {
    let mut child = Command::new(CURL)
        .args(["-q", "-s", "--max-time", "3", TRACE_URL])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let body = child.stdout.take().map(capped).unwrap_or_default();
    let _ = child.kill();
    let _ = child.wait();
    parse_colo(&String::from_utf8_lossy(&body))
}

// ---------- bandwidth ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    Scheduled = 0,
    BadRound = 1,
    Manual = 2,
    Hourly = 3,
}

impl Trigger {
    pub fn from_i64(v: i64) -> Trigger {
        match v {
            1 => Trigger::BadRound,
            2 => Trigger::Manual,
            3 => Trigger::Hourly,
            _ => Trigger::Scheduled,
        }
    }
    pub fn slug(self) -> &'static str {
        match self {
            Trigger::Scheduled => "scheduled",
            Trigger::BadRound => "bad-round",
            Trigger::Manual => "manual",
            Trigger::Hourly => "hourly",
        }
    }
}

impl Trigger {
    /// Full tests read the line's real speed; quick checks are only compared with their own normal.
    pub fn is_full(self) -> bool {
        matches!(self, Trigger::Manual | Trigger::Hourly)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Caps {
    pub streams: u32,
    pub down_bytes: u64,
    pub up_bytes: u64,
    pub max_secs: u32,
}

impl Caps {
    /// Quick checks (every 10 minutes, and after a bad round) stay small, about 14 MB. A fresh
    /// connection never reaches full speed in that little data, so they read low on a fast line
    /// and are only compared with their own normal. Full tests (hourly and Test Now) fill the line.
    pub fn for_trigger(t: Trigger) -> Caps {
        match t {
            Trigger::Manual | Trigger::Hourly => Caps { streams: 4, down_bytes: 25_000_000, up_bytes: 10_000_000, max_secs: 8 },
            _ => Caps { streams: 4, down_bytes: 2_500_000, up_bytes: 1_000_000, max_secs: 4 },
        }
    }
    #[cfg(test)]
    pub fn total_bytes(&self) -> u64 {
        self.streams as u64 * (self.down_bytes + self.up_bytes)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bandwidth {
    pub at_ms: i64,
    pub trigger: Trigger,
    pub down_mbps: Option<f64>,
    pub up_mbps: Option<f64>,
    pub down_bytes: i64,
    pub up_bytes: i64,
    /// Responsiveness under load in round-trips per minute (Test Now only, from networkQuality).
    pub rpm: Option<f64>,
}

/// One curl stream's `-w` line: HTTP status, bytes, seconds to first byte (or to upload start),
/// total seconds. Anything but a 2xx answer is an error page, not a measurement.
pub fn parse_curl_w(line: &str) -> Option<(u64, f64, f64)> {
    let mut it = line.split_whitespace();
    let status: u16 = it.next()?.parse().ok()?;
    if !(200..300).contains(&status) {
        return None;
    }
    let bytes = it.next()?.parse::<f64>().ok()? as u64;
    let start = it.next()?.parse().ok()?;
    let total = it.next()?.parse().ok()?;
    Some((bytes, start, total))
}

/// Aggregate rate of parallel streams: all bytes over the span from the first stream starting
/// to move data to the last one finishing.
pub fn aggregate_mbps(streams: &[(u64, f64, f64)]) -> Option<f64> {
    let bytes: u64 = streams.iter().map(|s| s.0).sum();
    let start = streams.iter().map(|s| s.1).fold(f64::INFINITY, f64::min);
    let end = streams.iter().map(|s| s.2).fold(0.0, f64::max);
    let span = end - start;
    (bytes > 0 && span > 0.0).then(|| bytes as f64 * 8.0 / span / 1e6)
}

fn run_streams(caps: &Caps, upload: bool) -> (Vec<(u64, f64, f64)>, i64) {
    let max = caps.max_secs.to_string();
    let mut children = Vec::new();
    let t0 = std::time::Instant::now();
    for _ in 0..caps.streams {
        let mut cmd = Command::new(CURL);
        cmd.args(["-q", "-s", "-o", "/dev/null", "--max-time", &max]).stderr(Stdio::null()).stdout(Stdio::piped());
        if upload {
            cmd.args(["-X", "POST", "-H", "Content-Type: application/octet-stream", "--data-binary", "@-"])
                .args(["-w", "%{http_code} %{size_upload} %{time_pretransfer} %{time_total}", UP_URL])
                .stdin(Stdio::piped());
        } else {
            let url = format!("{DOWN_URL}{}", caps.down_bytes);
            cmd.args(["-w", "%{http_code} %{size_download} %{time_starttransfer} %{time_total}"]).arg(url).stdin(Stdio::null());
        }
        if let Ok(mut child) = cmd.spawn() {
            if upload {
                if let Some(mut stdin) = child.stdin.take() {
                    let n = caps.up_bytes as usize;
                    std::thread::spawn(move || {
                        let chunk = noise_chunk();
                        let mut left = n;
                        while left > 0 {
                            let k = left.min(chunk.len());
                            if stdin.write_all(&chunk[..k]).is_err() {
                                break;
                            }
                            left -= k;
                        }
                    });
                }
            }
            children.push((child, t0.elapsed().as_secs_f64()));
        }
    }
    let mut out = Vec::new();
    let mut bytes = 0i64;
    for (c, offset) in children {
        if let Ok(o) = c.wait_with_output() {
            if let Some((n, start, total)) = parse_curl_w(&String::from_utf8_lossy(&o.stdout)) {
                bytes += n as i64;
                out.push((n, offset + start, offset + total));
            }
        }
    }
    (out, bytes)
}

/// Incompressible filler for uploads (xorshift), so no middlebox can shrink it.
fn noise_chunk() -> Vec<u8> {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15 ^ token()[0] as u64;
    (0..65_536)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// Test Now asks macOS's own `networkQuality`, which opens many flows to Apple's CDN (hosts it
/// picks itself from Apple's configuration, the one set of hosts this file does not name) and
/// reads a multi-gig line's real capacity plus responsiveness under load. It is capped at
/// NQ_MAX_SECS and killed at NQ_KILL_SECS, and on an expensive or Low Data path Test Now uses the
/// capped Cloudflare test instead. Falls back to Cloudflare on any failure.
pub fn bandwidth(trigger: Trigger, now_ms: i64) -> Bandwidth {
    if trigger == Trigger::Manual {
        let (expensive, constrained) = crate::netpath::state();
        if !(expensive || constrained) {
            if let Some(b) = network_quality(now_ms) {
                return b;
            }
        }
    }
    cloudflare(trigger, now_ms)
}

const NETWORK_QUALITY: &str = "/usr/bin/networkQuality";
pub const NQ_MAX_SECS: u32 = 12;
const NQ_KILL_SECS: u64 = 30;

pub fn parse_network_quality(json: &str, at_ms: i64) -> Option<Bandwidth> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let mbps = |k: &str| v.get(k).and_then(|x| x.as_f64()).map(|bits| bits / 1e6);
    let bytes = |k: &str| v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0).max(0.0) as i64;
    let down = mbps("dl_throughput")?;
    Some(Bandwidth {
        at_ms,
        trigger: Trigger::Manual,
        down_mbps: Some(down),
        up_mbps: mbps("ul_throughput"),
        down_bytes: bytes("dl_bytes_transferred"),
        up_bytes: bytes("ul_bytes_transferred"),
        rpm: v.get("responsiveness").and_then(|x| x.as_f64()),
    })
}

fn network_quality(now_ms: i64) -> Option<Bandwidth> {
    let mut child = Command::new(NETWORK_QUALITY)
        .args(["-c", "-M", &NQ_MAX_SECS.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < Duration::from_secs(NQ_KILL_SECS) => std::thread::sleep(Duration::from_millis(200)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let out = child.wait_with_output().ok()?;
    parse_network_quality(&String::from_utf8_lossy(&out.stdout), now_ms)
}

/// Responsiveness in words. The cut-offs are Packet's own, not Apple's.
pub fn rpm_label(rpm: f64) -> &'static str {
    if rpm >= 800.0 {
        "High"
    } else if rpm >= 200.0 {
        "Medium"
    } else {
        "Low"
    }
}

fn cloudflare(trigger: Trigger, now_ms: i64) -> Bandwidth {
    let caps = Caps::for_trigger(trigger);
    let (down, down_bytes) = run_streams(&caps, false);
    let (up, up_bytes) = run_streams(&caps, true);
    Bandwidth {
        at_ms: now_ms,
        trigger,
        down_mbps: aggregate_mbps(&down),
        up_mbps: aggregate_mbps(&up),
        down_bytes,
        up_bytes,
        rpm: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_parse() {
        let out = "   route to: default\ndestination: default\n    gateway: 192.168.1.1\n  interface: en0\n";
        assert_eq!(parse_route(out), (Some(Ipv4Addr::new(192, 168, 1, 1)), Some("en0".into())));
        assert_eq!(parse_route(""), (None, None));
    }

    #[test]
    fn link_classes() {
        assert_eq!(classify(Some("en1"), Some("en1")), LinkKind::WiFi);
        assert_eq!(classify(Some("en0"), Some("en1")), LinkKind::Ethernet);
        assert_eq!(classify(Some("utun4"), Some("en1")), LinkKind::Other);
        assert_eq!(classify(None, Some("en1")), LinkKind::None);
    }

    #[test]
    fn gateway_stats() {
        let s = PingStats { sent: 5, rtts: vec![2.0, 4.0, 3.0, 5.0] };
        assert!((s.loss().unwrap() - 0.2).abs() < 1e-9);
        assert_eq!(s.median(), Some(3.5));
        // |4-2| + |3-4| + |5-3| = 5 over 3 gaps
        assert!((s.jitter().unwrap() - 5.0 / 3.0).abs() < 1e-9);
        let none = PingStats { sent: 5, rtts: vec![] };
        assert_eq!(none.loss(), Some(1.0));
        assert_eq!(none.median(), None);
        assert!(!none.answered());
    }

    #[test]
    fn icmp_roundtrip() {
        let tok = [1, 2, 3, 4, 5, 6, 7, 8];
        let req = echo_request(7, 3, tok);
        assert_eq!(checksum(&req), 0, "checksum folds to zero over a valid packet");
        let mut reply = req.clone();
        reply[0] = 0;
        assert!(is_reply(&reply, 3, tok));
        assert!(!is_reply(&reply, 4, tok));
        let mut with_ip = vec![0x45u8; 1];
        with_ip.extend_from_slice(&[0u8; 19]);
        with_ip.extend_from_slice(&reply);
        assert!(is_reply(&with_ip, 3, tok));
        assert!(!is_reply(&req, 3, tok), "a request is not a reply");
    }

    #[test]
    fn resolve_has_a_deadline() {
        let t = Instant::now();
        let _ = resolve_v4("packet-test.invalid", Duration::from_millis(300));
        assert!(t.elapsed() < Duration::from_millis(600));
    }

    #[test]
    fn loopback_ping() {
        let p = ping(Ipv4Addr::LOCALHOST, 3, Duration::from_millis(20), Duration::from_millis(500));
        assert_eq!(p.sent, 3);
        assert_eq!(p.recv(), 3, "the socket send and receive path works end to end");
        assert!(p.median().unwrap() < 50.0);
    }

    #[test]
    fn tokens_differ_back_to_back() {
        let a: Vec<[u8; 8]> = (0..1000).map(|_| token()).collect();
        let mut b = a.clone();
        b.sort();
        b.dedup();
        assert_eq!(b.len(), a.len());
    }

    #[test]
    fn dns_packets() {
        let q = dns_query(0xabcd, "github.com");
        assert_eq!(&q[..2], &[0xab, 0xcd]);
        assert_eq!(&q[12..], &[6, b'g', b'i', b't', b'h', b'u', b'b', 3, b'c', b'o', b'm', 0, 0, 1, 0, 1]);
        let mut r = q.clone();
        r[2] |= 0x80;
        assert_eq!(dns_reply(&r, &q), Some(true));
        let mut other = q.clone();
        other[1] ^= 1;
        assert_eq!(dns_reply(&r, &other), None, "another ID is not ours");
        assert_eq!(dns_reply(&q, &q), None, "our own query is not a reply");
        let mut forged = r.clone();
        forged[13] = b'x';
        assert_eq!(dns_reply(&forged, &q), None, "an answer to another question is not ours");
        assert_eq!(dns_reply(&r[..12], &q), None, "a bare header is not ours");
        let mut notify = r.clone();
        notify[2] |= 0x20;
        assert_eq!(dns_reply(&notify, &q), None, "another opcode is not ours");
        r[3] = 0x02; // SERVFAIL
        assert_eq!(dns_reply(&r, &q), Some(false));
        assert_eq!(parse_resolver("# x\nnameserver fe80::1\nnameserver 192.168.1.1\n"), Some(Ipv4Addr::new(192, 168, 1, 1)));
    }

    #[test]
    fn trace_read_is_capped() {
        assert_eq!(capped(std::io::repeat(b'x')).len() as u64, TRACE_CAP);
        assert_eq!(capped(&b"colo=SJC\n"[..]), b"colo=SJC\n");
    }

    #[test]
    fn colo_parse() {
        assert_eq!(parse_colo("fl=1\ncolo=SJC\nloc=US\n"), Some("SJC".into()));
        assert_eq!(parse_colo("colo=<script>"), None);
        assert_eq!(parse_colo(""), None);
    }

    #[test]
    fn regions_span_continents() {
        let mut c: Vec<_> = REGIONS.iter().map(|r| r.continent).collect();
        c.sort();
        c.dedup();
        assert!(REGIONS.len() >= 6 && c.len() >= 4);
    }

    #[test]
    fn bandwidth_caps() {
        let s = Caps::for_trigger(Trigger::Scheduled);
        assert!(s.total_bytes() <= 15_000_000, "scheduled test stays near 14 MB");
        assert!(s.max_secs <= 4);
        assert_eq!(Caps::for_trigger(Trigger::BadRound), s);
        let m = Caps::for_trigger(Trigger::Manual);
        assert!(m.total_bytes() <= 150_000_000 && m.max_secs <= 8);
        assert_eq!(Caps::for_trigger(Trigger::Hourly), m);
        // Daily automatic volume: a quick check every 10 minutes plus a full test every hour.
        assert!(s.total_bytes() * 144 + m.total_bytes() * 24 <= 5_500_000_000);
    }

    #[test]
    fn network_quality_parse() {
        let j = r#"{"dl_throughput":1578590976,"ul_throughput":2053028864,"responsiveness":2527.9,"dl_bytes_transferred":2796573675,"ul_bytes_transferred":3543138304}"#;
        let b = parse_network_quality(j, 5).unwrap();
        assert_eq!(b.trigger, Trigger::Manual);
        assert!((b.down_mbps.unwrap() - 1578.59).abs() < 0.01);
        assert!((b.up_mbps.unwrap() - 2053.03).abs() < 0.01);
        assert_eq!(b.rpm, Some(2527.9));
        assert_eq!(b.down_bytes + b.up_bytes, 6_339_711_979);
        assert!(parse_network_quality("{}", 5).is_none());
        assert!(parse_network_quality("not json", 5).is_none());
        assert_eq!(rpm_label(2527.9), "High");
        assert_eq!(rpm_label(150.0), "Low");
    }

    #[test]
    fn rates() {
        assert_eq!(parse_curl_w("200 2500000 0.05 0.25"), Some((2_500_000, 0.05, 0.25)));
        assert_eq!(parse_curl_w("429 2500 0.05 0.25"), None, "an error page is not a measurement");
        assert_eq!(parse_curl_w("000 0 0 0.25"), None, "no answer at all");
        assert_eq!(parse_curl_w("garbage"), None);
        // 4 × 2.5 MB over 0.2 s = 400 Mbps
        let s = vec![(2_500_000, 0.05, 0.25); 4];
        assert!((aggregate_mbps(&s).unwrap() - 400.0).abs() < 1e-6);
        assert_eq!(aggregate_mbps(&[]), None);
    }
}
