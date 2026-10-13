//! Wall-clock helpers in the Mac's local zone, 24-hour, through libc.

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn local_tm(ms: i64) -> libc::tm {
    let t: libc::time_t = (ms / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    tm
}

/// `2026-10-12T14:30:05-07:00`
pub fn iso_local(ms: i64) -> String {
    let tm = local_tm(ms);
    let off = tm.tm_gmtoff / 60;
    let sign = if off < 0 { '-' } else { '+' };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{}{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        sign,
        off.abs() / 60,
        off.abs() % 60
    )
}

/// `14:30`
pub fn hhmm(ms: i64) -> String {
    let tm = local_tm(ms);
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}

/// "just now", "4 min ago", "2 h ago".
pub fn ago(now: i64, then: i64) -> String {
    let s = (now - then).max(0) / 1000;
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=172_799 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86_400),
    }
}

fn local_ms(y: i32, mo: i32, d: i32, h: i32, mi: i32) -> Option<i64> {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = y - 1900;
    tm.tm_mon = mo - 1;
    tm.tm_mday = d;
    tm.tm_hour = h;
    tm.tm_min = mi;
    tm.tm_isdst = -1;
    let t = unsafe { libc::mktime(&mut tm) };
    (t != -1).then_some(t as i64 * 1000)
}

fn hm(s: &str) -> Option<(i32, i32)> {
    let (h, m) = s.split_once(':')?;
    let (h, m) = (h.parse().ok()?, m.parse().ok()?);
    ((0..24).contains(&h) && (0..60).contains(&m)).then_some((h, m))
}

/// Parses `14:30` (the most recent past 14:30), `2026-10-12 14:30`, `2026-10-12T14:30`,
/// `30m` / `2h` / `3d` (ago), or epoch milliseconds.
pub fn parse_when(s: &str, now: i64) -> Option<i64> {
    let s = s.trim().trim_end_matches(" ago");
    if let Some(n) = s.strip_suffix('m').and_then(|n| n.parse::<i64>().ok()) {
        return Some(now - n * 60_000);
    }
    if let Some(n) = s.strip_suffix('h').and_then(|n| n.parse::<i64>().ok()) {
        return Some(now - n * 3_600_000);
    }
    if let Some(n) = s.strip_suffix('d').and_then(|n| n.parse::<i64>().ok()) {
        return Some(now - n * 86_400_000);
    }
    if s.len() >= 12 && s.chars().all(|c| c.is_ascii_digit()) {
        return s.parse().ok();
    }
    if let Some((h, m)) = hm(s) {
        let today = local_tm(now);
        let (y, mo, d) = (today.tm_year + 1900, today.tm_mon + 1, today.tm_mday);
        let t = local_ms(y, mo, d, h, m)?;
        // mktime rolls day 0 back to the last day of the previous month.
        return if t > now { local_ms(y, mo, d - 1, h, m) } else { Some(t) };
    }
    let (date, time) = s.split_once(['T', ' '])?;
    let mut parts = date.split('-').map(|p| p.parse::<i32>().ok());
    let (y, mo, d) = (parts.next()??, parts.next()??, parts.next()??);
    let hhmm: String = time.chars().take(5).collect();
    let (h, m) = hm(&hhmm)?;
    let t = local_ms(y, mo, d, h, m)?;
    // mktime rolls 2026-02-30 into March; a date that moved was never a date.
    let back = local_tm(t);
    (back.tm_year + 1900 == y && back.tm_mon + 1 == mo && back.tm_mday == d).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn when_forms() {
        let now = local_ms(2026, 10, 12, 15, 0).unwrap();
        assert_eq!(parse_when("30m", now), Some(now - 1_800_000));
        assert_eq!(parse_when("2h ago", now), Some(now - 7_200_000));
        assert_eq!(parse_when("14:30", now), Some(now - 1_800_000));
        assert_eq!(parse_when("16:00", now), Some(now + 3_600_000 - 86_400_000), "future clock time means yesterday");
        assert_eq!(parse_when("2026-10-12 14:30", now), Some(now - 1_800_000));
        assert_eq!(parse_when("2026-10-12T14:30", now), Some(now - 1_800_000));
        assert_eq!(parse_when("25:00", now), None);
        assert_eq!(parse_when("2026-02-30 14:30", now), None, "not a calendar date");
        assert_eq!(parse_when("soon", now), None);
        assert_eq!(parse_when("2026-10-12 1é:30", now), None, "non-ASCII never panics");
        let after_dst = local_ms(2026, 11, 1, 0, 30).unwrap();
        assert_eq!(parse_when("12:00", after_dst), local_ms(2026, 10, 31, 12, 0), "yesterday across a DST change");
        let first = local_ms(2026, 11, 1, 9, 0).unwrap();
        assert_eq!(parse_when("10:00", first), local_ms(2026, 10, 31, 10, 0), "yesterday across a month end");
        assert_eq!(hhmm(now), "15:00");
        assert_eq!(ago(now, now - 240_000), "4 min ago");
    }
}
