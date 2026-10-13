//! The measuring thread: a round every minute, bandwidth on its schedule, results to the store
//! and to whoever is listening.

use crate::probe::{self, Bandwidth, Trigger};
use crate::round::{self, Round};
use crate::verdict::{self, Baseline, Finding, Level, Verdict};
use crate::{sched, store, time};
use rusqlite::Connection;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const BASELINE_REFRESH_MS: i64 = 30 * 60_000;

pub enum Cmd {
    TestNow,
    Shutdown,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub round: Round,
    /// The settled verdict: a single bad round is not enough to change it (see `verdict::settle`).
    pub verdict: Verdict,
    /// Each layer as this round measured it, before settling.
    pub findings: [Finding; 4],
    /// The last full test: the line's real speed.
    pub full: Option<Bandwidth>,
    /// The last quick check, as a share of its usual download (None until there is a usual).
    pub quick: Option<Bandwidth>,
    pub quick_share: Option<f64>,
    /// A bandwidth test is running right after this round.
    pub testing: bool,
    /// This snapshot ends a Test Now.
    pub manual_done: bool,
}

pub struct State {
    pub baseline: Baseline,
    pub baseline_at: i64,
    pub last_quick: Option<Bandwidth>,
    pub last_full: Option<Bandwidth>,
    pub prev_raw: Option<Verdict>,
    /// The settled level of the round before, for spotting a turn for the worse.
    pub prev_settled: Level,
    /// When the last test of any kind, and the last full one, ran, measured or not: a failed test
    /// waits for its schedule like a good one instead of retrying every minute.
    pub tried_any: Option<i64>,
    pub tried_full: Option<i64>,
    pub seq: u64,
    pub pruned_at: i64,
}

impl State {
    pub fn load(c: &Connection, now: i64) -> State {
        State {
            baseline: store::baseline(c, now).unwrap_or_default(),
            baseline_at: now,
            last_quick: store::latest_quick(c).ok().flatten(),
            last_full: store::latest_full(c).ok().flatten(),
            prev_raw: None,
            prev_settled: Level::Good,
            tried_any: None,
            tried_full: None,
            seq: 0,
            pruned_at: 0,
        }
    }
}

fn measured(b: &Bandwidth) -> bool {
    b.down_mbps.is_some() || b.up_mbps.is_some()
}

/// One round, and the bandwidth test it calls for. `manual` is Test Now.
pub fn step(
    c: &Connection,
    st: &mut State,
    manual: bool,
    measure: &dyn Fn(i64, u64) -> Round,
    bandwidth: &dyn Fn(Trigger, i64) -> Bandwidth,
    notify: &dyn Fn(Snapshot),
) {
    let now = time::now_ms();
    if now - st.baseline_at > BASELINE_REFRESH_MS {
        st.baseline = store::baseline(c, now).unwrap_or_default();
        st.baseline_at = now;
    }
    if now - st.pruned_at > 86_400_000 {
        let _ = store::prune(c, now);
        st.pruned_at = now;
    }
    let round = measure(now, st.seq);
    st.seq += 1;
    let recent_down = store::recent_scheduled_down(c, now).ok().flatten();
    let findings = verdict::findings(&round, &st.baseline, recent_down);
    let raw = verdict::judge(&round, &st.baseline, recent_down);
    let prev_level = st.prev_settled;
    let v = verdict::settle(&raw, st.prev_raw.as_ref());
    st.prev_raw = Some(raw);
    st.prev_settled = v.level;
    if store::insert_round(c, &round, &v).is_err() {
        std::thread::sleep(Duration::from_millis(250));
        let _ = store::insert_round(c, &round, &v);
    }
    let due = if manual {
        Some(Trigger::Manual)
    } else {
        let last_any = [st.last_quick.map(|b| b.at_ms), st.last_full.map(|b| b.at_ms), st.tried_any].into_iter().flatten().max();
        let last_full = [st.last_full.map(|b| b.at_ms), st.tried_full].into_iter().flatten().max();
        sched::bandwidth_due(now, last_any, last_full, v.level, prev_level, round.expensive, sched::hourly_enabled())
    };
    let mut snap = Snapshot {
        round,
        verdict: v,
        findings,
        full: st.last_full,
        quick: st.last_quick,
        quick_share: share(st.last_quick, &st.baseline),
        testing: due.is_some(),
        manual_done: false,
    };
    notify(snap.clone());
    if let Some(t) = due {
        let bw = bandwidth(t, time::now_ms());
        st.tried_any = Some(bw.at_ms);
        if t.is_full() {
            st.tried_full = Some(bw.at_ms);
        }
        let _ = store::insert_bandwidth(c, &bw);
        // A test that measured nothing never hides the last good result.
        if measured(&bw) {
            if bw.trigger.is_full() {
                st.last_full = Some(bw);
                snap.full = Some(bw);
            } else {
                st.last_quick = Some(bw);
                snap.quick = Some(bw);
                snap.quick_share = share(Some(bw), &st.baseline);
            }
        }
        snap.testing = false;
        snap.manual_done = manual;
        notify(snap);
    }
}

fn share(quick: Option<Bandwidth>, b: &Baseline) -> Option<f64> {
    match (quick.and_then(|q| q.down_mbps), b.down_mbps) {
        (Some(q), Some(u)) if u > 0.0 => Some(q / u),
        _ => None,
    }
}

pub fn spawn(notify: impl Fn(Snapshot) + Send + 'static) -> (Sender<Cmd>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let h = std::thread::Builder::new()
        .name("packet-sampler".into())
        .spawn(move || {
            let Ok(c) = store::open() else { return };
            let mut st = State::load(&c, time::now_ms());
            let every = Duration::from_millis(sched::ROUND_EVERY_MS as u64);
            let mut next = Instant::now();
            let mut queued = false;
            loop {
                let manual = if std::mem::take(&mut queued) {
                    true
                } else {
                    match rx.recv_timeout(next.saturating_duration_since(Instant::now())) {
                        Ok(Cmd::TestNow) => true,
                        Ok(Cmd::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
                        Err(RecvTimeoutError::Timeout) => false,
                    }
                };
                if !manual {
                    next += every;
                    if next < Instant::now() {
                        next = Instant::now() + every;
                    }
                }
                step(&c, &mut st, manual, &round::run, &probe::bandwidth, &notify);
                // Clicks that queued while this round ran are one request, not several. After an
                // automatic round that request still runs; after a Test Now it was this one.
                loop {
                    match rx.try_recv() {
                        Ok(Cmd::TestNow) => queued |= !manual,
                        Ok(Cmd::Shutdown) | Err(mpsc::TryRecvError::Disconnected) => return,
                        Err(mpsc::TryRecvError::Empty) => break,
                    }
                }
            }
        })
        .expect("spawn sampler thread");
    (tx, h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verdict::tests::healthy;
    use std::cell::RefCell;

    fn fake_bw(t: Trigger, at: i64) -> Bandwidth {
        Bandwidth { at_ms: at, trigger: t, down_mbps: Some(300.0), up_mbps: Some(200.0), down_bytes: 1, up_bytes: 1, rpm: None }
    }

    fn failed_bw(t: Trigger, at: i64) -> Bandwidth {
        Bandwidth { at_ms: at, trigger: t, down_mbps: None, up_mbps: None, down_bytes: 0, up_bytes: 0, rpm: None }
    }

    fn fake_round(at: i64, _seq: u64) -> Round {
        let mut r = healthy();
        r.at_ms = at;
        r
    }

    fn temp_conn(tag: &str) -> (std::path::PathBuf, Connection) {
        let dir = std::env::temp_dir().join(format!("packet-step-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.sqlite");
        let _ = std::fs::remove_file(&path);
        (dir, store::open_at(&path).unwrap())
    }

    #[test]
    fn test_now_runs_both() {
        let (dir, c) = temp_conn("both");
        let mut st = State::load(&c, time::now_ms());
        // Fresh automatic tests just ran, so only Test Now can start another.
        st.last_quick = Some(fake_bw(Trigger::Scheduled, time::now_ms()));
        st.last_full = Some(fake_bw(Trigger::Hourly, time::now_ms()));
        let seen = RefCell::new(Vec::new());
        step(&c, &mut st, false, &fake_round, &fake_bw, &|s| seen.borrow_mut().push((s.testing, s.manual_done)));
        assert_eq!(*seen.borrow(), vec![(false, false)], "automatic round with fresh tests: no bandwidth");
        std::thread::sleep(Duration::from_millis(5));
        seen.borrow_mut().clear();
        step(&c, &mut st, true, &fake_round, &fake_bw, &|s| seen.borrow_mut().push((s.testing, s.manual_done)));
        assert_eq!(*seen.borrow(), vec![(true, false), (false, true)], "Test Now reports testing, then the result");
        let rounds: i64 = c.query_row("SELECT COUNT(*) FROM rounds", [], |r| r.get(0)).unwrap();
        let manual: i64 = c.query_row("SELECT COUNT(*) FROM bandwidth WHERE trigger = 2", [], |r| r.get(0)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!((rounds, manual), (2, 1));
    }

    #[test]
    fn turning_bad_pulls_a_check_forward() {
        let (dir, c) = temp_conn("turn");
        let mut st = State::load(&c, time::now_ms());
        st.last_quick = Some(fake_bw(Trigger::Scheduled, time::now_ms() - 4 * 60_000));
        st.last_full = Some(fake_bw(Trigger::Hourly, time::now_ms() - 30 * 60_000));
        let lossy = |at: i64, _s: u64| {
            let mut r = healthy();
            r.at_ms = at;
            r.edge.loss = Some(0.4);
            r
        };
        let ran = RefCell::new(Vec::new());
        let bw = |t: Trigger, at: i64| {
            ran.borrow_mut().push(t);
            fake_bw(t, at)
        };
        step(&c, &mut st, false, &lossy, &bw, &|_| {});
        assert!(ran.borrow().is_empty(), "the first bad round is held, so no check yet");
        std::thread::sleep(Duration::from_millis(5));
        step(&c, &mut st, false, &lossy, &bw, &|_| {});
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(*ran.borrow(), vec![Trigger::BadRound], "the settled turn to bad pulls a check forward");
    }

    #[test]
    fn failed_test_waits_for_its_schedule() {
        let (dir, c) = temp_conn("retry");
        let mut st = State::load(&c, time::now_ms());
        st.last_quick = Some(fake_bw(Trigger::Scheduled, time::now_ms() - 20 * 60_000));
        st.last_full = Some(fake_bw(Trigger::Hourly, time::now_ms() - 30 * 60_000));
        let ran = RefCell::new(Vec::new());
        let bw = |t: Trigger, at: i64| {
            ran.borrow_mut().push(t);
            failed_bw(t, at)
        };
        step(&c, &mut st, false, &fake_round, &bw, &|_| {});
        std::thread::sleep(Duration::from_millis(5));
        step(&c, &mut st, false, &fake_round, &bw, &|_| {});
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(*ran.borrow(), vec![Trigger::Scheduled], "one failed check, then the schedule, not a retry every round");
    }

    #[test]
    fn failed_test_keeps_last_good() {
        let (dir, c) = temp_conn("failed");
        let mut st = State::load(&c, time::now_ms());
        let good = fake_bw(Trigger::Manual, time::now_ms() - 1000);
        st.last_full = Some(good);
        st.last_quick = Some(fake_bw(Trigger::Scheduled, time::now_ms()));
        let last = RefCell::new(None);
        step(&c, &mut st, true, &fake_round, &failed_bw, &|s| *last.borrow_mut() = s.full);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(*last.borrow(), Some(good));
        assert_eq!(st.last_full, Some(good));
    }

    #[test]
    fn one_blip_does_not_change_the_verdict() {
        let (dir, c) = temp_conn("blip");
        let mut st = State::load(&c, time::now_ms());
        st.last_quick = Some(fake_bw(Trigger::Scheduled, time::now_ms()));
        st.last_full = Some(fake_bw(Trigger::Hourly, time::now_ms()));
        let blip = |at: i64, _s: u64| {
            let mut r = fake_round(at, 0);
            r.edge.loss = Some(0.6);
            r
        };
        let levels = RefCell::new(Vec::new());
        step(&c, &mut st, false, &blip, &fake_bw, &|s| levels.borrow_mut().push(s.verdict.level));
        std::thread::sleep(Duration::from_millis(5));
        step(&c, &mut st, false, &blip, &fake_bw, &|s| levels.borrow_mut().push(s.verdict.level));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(*levels.borrow(), vec![Level::Good, Level::Poor], "first bad round is held, the second shows");
    }
}
