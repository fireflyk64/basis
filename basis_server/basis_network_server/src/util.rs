//! Small shared helpers: calendar/time formatting the C# got from `DateTime`.

use std::time::{SystemTime, UNIX_EPOCH};

/// Proleptic Gregorian calendar from a Unix timestamp (Howard Hinnant's algorithm):
/// `(year, month, day, hour, minute, second)`.
pub fn civil_from_unix(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, (rem / 3600) as u32, ((rem % 3600) / 60) as u32, (rem % 60) as u32)
}

fn now_unix() -> (i64, u32) {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    (now.as_secs() as i64, now.subsec_nanos())
}

/// `DateTime.UtcNow.ToString("o")`: `2026-08-29T12:34:56.1234567Z`.
pub fn utc_now_iso8601() -> String {
    let (secs, nanos) = now_unix();
    utc_iso8601(secs, nanos)
}

pub fn utc_iso8601(secs: i64, nanos: u32) -> String {
    let (y, mo, d, h, mi, s) = civil_from_unix(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{:07}Z", nanos / 100)
}

/// `yyyy-MM-dd HH:mm:ss` in UTC.
pub fn utc_now_stamp() -> String {
    let (secs, _) = now_unix();
    let (y, mo, d, h, mi, s) = civil_from_unix(secs);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

/// `yyyyMMddHHmmssfff` in UTC.
pub fn utc_now_compact_stamp() -> String {
    let (secs, nanos) = now_unix();
    let (y, mo, d, h, mi, s) = civil_from_unix(secs);
    format!("{y:04}{mo:02}{d:02}{h:02}{mi:02}{s:02}{:03}", nanos / 1_000_000)
}

/// `yyyy-MM-dd` in UTC.
pub fn utc_today() -> String {
    let (secs, _) = now_unix();
    let (y, mo, d, _, _, _) = civil_from_unix(secs);
    format!("{y:04}-{mo:02}-{d:02}")
}

/// The local wall clock as `(hour, minute)`, via the C runtime so the host's time zone applies
/// as it did for `DateTime.Now`. Falls back to UTC if the C runtime cannot answer.
pub fn local_hour_minute() -> (u32, u32) {
    let (secs, _) = now_unix();
    #[cfg(unix)]
    {
        let time: libc::time_t = secs as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: localtime_r writes only into the `tm` we own and reads the `time` we pass.
        let ok = unsafe { !libc::localtime_r(&time, &mut tm).is_null() };
        if ok {
            return (tm.tm_hour as u32, tm.tm_min as u32);
        }
    }
    let (_, _, _, h, mi, _) = civil_from_unix(secs);
    (h, mi)
}

/// .NET `DateTime.UtcNow.Ticks` (100 ns since 0001-01-01).
pub fn utc_now_ticks() -> i64 {
    const EPOCH_TICKS: i64 = 621_355_968_000_000_000;
    let (secs, nanos) = now_unix();
    EPOCH_TICKS + secs * 10_000_000 + i64::from(nanos / 100)
}

/// Resident set size of this process in bytes (Linux `/proc/self/statm`), 0 elsewhere.
pub fn working_set_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(statm) = std::fs::read_to_string("/proc/self/statm")
            && let Some(resident_pages) = statm.split_whitespace().nth(1).and_then(|p| p.parse::<u64>().ok())
        {
            let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
            return resident_pages * u64::try_from(page).unwrap_or(4096);
        }
    }
    0
}

/// Minimal JSON string escaping (the C# hand-rolled the same set).
pub fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 16);
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// A finite number rendered with `decimals` places; NaN/inf become 0 as the C# `Num` did.
pub fn json_num(value: f64, decimals: usize) -> String {
    if value.is_finite() { format!("{value:.decimals$}") } else { "0".to_string() }
}
