//! Packet: is it the network? Measures each layer of the connection every minute, in the menu bar.
//! `packet` runs the app; the subcommands answer from the command line.

mod app;
mod history;
mod login;
mod netpath;
mod probe;
mod report;
mod round;
mod sampler;
mod sched;
mod store;
mod time;
mod verdict;
mod wlan;

use probe::Trigger;
use serde_json::json;

const HELP: &str = "packet — is it the network?

USAGE
  packet                          run the menu-bar app
  packet now [--json]             measure every layer now
  packet test [--json]            measure now plus a full bandwidth test
  packet at <WHEN> [--json]       what the network was like at a past moment
  packet history [--range R] [--json]
  packet help

WHEN   14:30 · 2026-10-12 14:30 · 30m · 2h · 3d · epoch ms
RANGE  1h · 24h (default) · 7d · 30d

EXIT
  0 ok · 1 no data for that time · 2 bad arguments";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with("-psn_")).collect();
    let json = args.iter().any(|a| a == "--json");
    let rest: Vec<&str> = args.iter().map(String::as_str).filter(|a| *a != "--json").collect();
    let code = match rest.first().copied() {
        None => {
            app::run();
            0
        }
        Some("now") => now(json, false),
        Some("test") => now(json, true),
        Some("at") => match rest.get(1) {
            Some(w) => at(&rest[1..].join(" "), json).unwrap_or_else(|| {
                eprintln!("can't read the time {w:?}\n\n{HELP}");
                2
            }),
            None => {
                eprintln!("at needs a time\n\n{HELP}");
                2
            }
        },
        Some("history") => {
            let range = rest.iter().position(|a| *a == "--range").and_then(|i| rest.get(i + 1)).copied().unwrap_or("24h");
            if history::range_ms(range).is_none() {
                eprintln!("unknown range {range:?}\n\n{HELP}");
                2
            } else {
                hist(range, json)
            }
        }
        Some("help" | "--help" | "-h") => {
            println!("{HELP}");
            0
        }
        Some(other) => {
            eprintln!("unknown command: {other}\n\n{HELP}");
            2
        }
    };
    std::process::exit(code);
}

fn print_text(r: &round::Round, v: &verdict::Verdict, f: &[verdict::Finding; 4], bw: Option<&probe::Bandwidth>, now: i64) {
    println!("{} {}\n", v.level.mark(), v.sentence);
    println!("  {} {}", f[0].level.mark(), report::link_row(r));
    println!("  {} {}", f[1].level.mark(), report::edge_row(r));
    println!("  {} {}", f[2].level.mark(), report::dns_row(r));
    println!("  {} {}", f[3].level.mark(), report::world_row(r));
    println!("       {}", report::region_line(r));
    println!("  {}", report::bandwidth_row(bw, now));
}

fn now(json: bool, full: bool) -> i32 {
    let at = time::now_ms();
    let c = store::open().ok();
    let base = c.as_ref().and_then(|c| store::baseline(c, at).ok()).unwrap_or_default();
    let recent = c.as_ref().and_then(|c| store::recent_scheduled_down(c, at).ok().flatten());
    let r = round::run(at, at as u64 / 60_000);
    let bw = if full {
        Some(probe::bandwidth(Trigger::Manual, time::now_ms()))
    } else {
        c.as_ref().and_then(|c| store::latest_full(c).ok().flatten())
    };
    let v = verdict::judge(&r, &base, recent);
    let f = verdict::findings(&r, &base, recent);
    if json {
        let mut out = r.to_json(&v);
        out["layers"] = json!(f.iter().map(|x| json!({ "layer": x.at.slug(), "level": x.level.slug(), "detail": x.detail })).collect::<Vec<_>>());
        out["bandwidth"] = bw.map(bw_json).unwrap_or(serde_json::Value::Null);
        if full {
            if let Some(b) = bw {
                out["down_mbps"] = json!(round::round1(b.down_mbps));
                out["up_mbps"] = json!(round::round1(b.up_mbps));
            }
        }
        println!("{}", serde_json::to_string_pretty(&out).expect("json"));
    } else {
        print_text(&r, &v, &f, bw.as_ref(), time::now_ms());
    }
    0
}

fn bw_json(b: probe::Bandwidth) -> serde_json::Value {
    json!({
        "at": time::iso_local(b.at_ms),
        "down_mbps": round::round1(b.down_mbps),
        "up_mbps": round::round1(b.up_mbps),
        "trigger": b.trigger.slug(),
        "responsiveness_rpm": b.rpm.map(f64::round),
        "bytes": b.down_bytes + b.up_bytes,
    })
}

/// None when the time can't be parsed; Some(exit code) otherwise.
fn at(when: &str, json: bool) -> Option<i32> {
    let now = time::now_ms();
    let t = time::parse_when(when, now)?;
    let Ok(c) = store::open() else { return Some(1) };
    let Some(s) = store::nearest_round(&c, t, 5 * 60_000).ok().flatten() else {
        if json {
            println!("{}", json!({ "asked": time::iso_local(t), "found": false }));
        } else {
            println!("No measurement within 5 minutes of {}.", time::iso_local(t));
        }
        return Some(1);
    };
    let around = store::rounds_between(&c, t - 10 * 60_000, t + 10 * 60_000).unwrap_or_default();
    let bad = around.iter().filter(|x| x.verdict.level >= verdict::Level::Fair).count();
    let worst = around.iter().map(|x| x.verdict.level).max().unwrap_or(verdict::Level::Good);
    let bw = store::nearest_bandwidth(&c, t, 15 * 60_000).ok().flatten();
    let f = verdict::findings(&s.round, &store::baseline(&c, t).unwrap_or_default(), None);
    if json {
        let mut out = s.round.to_json(&s.verdict);
        out["asked"] = json!(time::iso_local(t));
        out["found"] = json!(true);
        out["window"] = json!({ "minutes": 20, "rounds": around.len(), "bad_rounds": bad, "worst": worst.slug() });
        out["bandwidth"] = bw.map(bw_json).unwrap_or(serde_json::Value::Null);
        println!("{}", serde_json::to_string_pretty(&out).expect("json"));
    } else {
        println!("At {} (nearest measurement {}):", time::hhmm(t), time::hhmm(s.round.at_ms));
        print_text(&s.round, &s.verdict, &f, bw.as_ref(), now);
        println!("\n  ±10 min: {bad} of {} rounds were fair or worse (worst: {}).", around.len(), worst.slug());
    }
    Some(0)
}

fn hist(range: &str, json: bool) -> i32 {
    let Ok(c) = store::open() else { return 1 };
    let Ok(h) = history::build(&c, range, time::now_ms()) else { return 1 };
    if json {
        println!("{}", serde_json::to_string(&h).expect("json"));
        return 0;
    }
    println!("Last {range}: {} rounds, {} fair or worse.", h["rounds"], h["bad_rounds"]);
    for b in h["buckets"].as_array().into_iter().flatten() {
        if b["level"] != "good" {
            let t = b["t"].as_i64().unwrap_or(0);
            println!("  {}  {:<8} {}", time::hhmm(t), b["level"].as_str().unwrap_or(""), b["layer"].as_str().unwrap_or(""));
        }
    }
    0
}
