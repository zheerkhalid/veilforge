//! Small pure helpers: time, dates, sizes, hex, base64, URL hosts.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

static LOCAL_OFFSET: AtomicI64 = AtomicI64::new(0);

/// Sets the offset of local time from UTC, in seconds (done once at startup).
pub fn set_local_offset(seconds: i64) {
    LOCAL_OFFSET.store(seconds, Ordering::Relaxed);
}

pub const DAY: i64 = 86_400;

/// Days since 1970-01-01 for a civil date (proleptic Gregorian).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date for a number of days since 1970-01-01.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn local(ts: i64) -> i64 {
    ts + LOCAL_OFFSET.load(Ordering::Relaxed)
}

/// `16 Nov 2026` in local time.
pub fn fmt_date(ts: i64) -> String {
    let (y, m, d) = civil_from_days(local(ts).div_euclid(DAY));
    format!("{d} {} {y}", MONTHS[(m - 1) as usize])
}

/// `16 Nov 2026, 14:05` in local time.
pub fn fmt_datetime(ts: i64) -> String {
    let t = local(ts);
    let secs = t.rem_euclid(DAY);
    format!(
        "{}, {:02}:{:02}",
        fmt_date(ts),
        secs / 3600,
        secs % 3600 / 60
    )
}

/// `2026-11-16` (UTC), the form used in date fields.
pub fn fmt_iso_date(ts: i64) -> String {
    let (y, m, d) = civil_from_days(ts.div_euclid(DAY));
    format!("{y:04}-{m:02}-{d:02}")
}

/// Parses `YYYY-MM-DD` into the Unix time of that day's start (UTC).
pub fn parse_iso_date(text: &str) -> Option<i64> {
    let mut parts = text.trim().split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1970..=9999).contains(&y) || !(1..=12).contains(&m) {
        return None;
    }
    if d < 1 || d > days_in_month(y, m) {
        return None;
    }
    Some(days_from_civil(y, m, d) * DAY)
}

pub fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
    }
}

/// Parses a card expiry (`MM/YY`, `MM/YYYY`, `MM-YY`, `MMYY`) into the Unix
/// time at which the card stops being valid (start of the following month).
pub fn parse_card_expiry(text: &str) -> Option<i64> {
    let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
    let has_sep = text.contains(['/', '-', ' ', '.']);
    let (m, y): (u32, i64) = if has_sep {
        let mut parts = text
            .split(['/', '-', ' ', '.'])
            .filter(|p| !p.trim().is_empty());
        let m = parts.next()?.trim().parse().ok()?;
        let y: i64 = parts.next()?.trim().parse().ok()?;
        (m, y)
    } else if digits.len() == 4 || digits.len() == 6 {
        (digits[..2].parse().ok()?, digits[2..].parse().ok()?)
    } else {
        return None;
    };
    let y = if y < 100 { 2000 + y } else { y };
    if !(1..=12).contains(&m) || !(2000..=2999).contains(&y) {
        return None;
    }
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    Some(days_from_civil(ny, nm, 1) * DAY)
}

/// "today", "3 days ago", "5 months ago", "2 years ago".
pub fn ago(ts: i64, now: i64) -> String {
    let days = (now - ts).max(0) / DAY;
    match days {
        0 => "today".into(),
        1 => "yesterday".into(),
        2..=59 => format!("{days} days ago"),
        60..=729 => format!("{} months ago", days / 30),
        _ => format!("{} years ago", days / 365),
    }
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if v >= 100.0 {
        format!("{v:.0} {}", UNITS[u])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// `1234567` → `1,234,567`.
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn hex(bytes: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(D[(b >> 4) as usize] as char);
        s.push(D[(b & 15) as usize] as char);
    }
    s
}

pub fn unhex(text: &str) -> Option<Vec<u8>> {
    let t = text.trim();
    if !t.len().is_multiple_of(2) || !t.is_ascii() {
        return None;
    }
    (0..t.len() / 2)
        .map(|i| u8::from_str_radix(&t[2 * i..2 * i + 2], 16).ok())
        .collect()
}

/// Standard base64 with padding.
pub fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            A[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// The host part of a URL or bare domain, lower case, without `www.`.
pub fn host_of(url: &str) -> String {
    let u = url.trim();
    let rest = match u.find("://") {
        Some(i) => &u[i + 3..],
        None => u,
    };
    let rest = rest.split(['/', '?', '#']).next().unwrap_or("");
    let rest = rest.rsplit('@').next().unwrap_or("");
    let host = if rest.starts_with('[') {
        rest.split(']').next().unwrap_or("").trim_start_matches('[')
    } else {
        rest.split(':').next().unwrap_or("")
    };
    host.trim_start_matches("www.").to_lowercase()
}

/// The registrable-looking part of a host: its last two labels
/// (`mail.example.com` → `example.com`). Good enough for matching.
pub fn base_domain(host: &str) -> String {
    let labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    if labels.len() <= 2 {
        return labels.join(".");
    }
    let n = labels.len();
    // Two-level public suffixes such as co.uk, com.au.
    let second = labels[n - 2];
    let take = if labels[n - 1].len() == 2
        && matches!(second, "co" | "com" | "org" | "net" | "ac" | "gov" | "edu")
    {
        3
    } else {
        2
    };
    labels[n - take.min(n)..].join(".")
}

/// Cuts text to `max` characters, adding an ellipsis when something was cut.
pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut s: String = text.chars().take(max.saturating_sub(1)).collect();
    s.push('…');
    s
}

/// Removes control characters from untrusted remote text and limits its length.
pub fn clean_remote(text: &str, max: usize) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    truncate(cleaned.trim(), max)
}

/// Creates a fresh, uniquely named folder under the system temp folder.
pub fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "veilforge-{tag}-{}-{}",
        std::process::id(),
        hex(&crate::secret::random_array::<6>())
    ));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        assert_eq!(fmt_iso_date(1_700_000_000), "2023-11-14");
        for d in [-800_000, -1, 0, 1, 19_000, 40_000, 900_000] {
            let (y, m, dd) = civil_from_days(d);
            assert_eq!(days_from_civil(y, m, dd), d);
        }
        assert_eq!(parse_iso_date("2023-11-14"), Some(1_699_920_000));
        assert_eq!(parse_iso_date("2023-02-30"), None);
        assert_eq!(parse_iso_date("x"), None);
    }

    #[test]
    fn card_expiry_forms() {
        let dec = days_from_civil(2028, 1, 1) * DAY;
        assert_eq!(parse_card_expiry("12/27"), Some(dec));
        assert_eq!(parse_card_expiry("12/2027"), Some(dec));
        assert_eq!(parse_card_expiry("1227"), Some(dec));
        assert_eq!(parse_card_expiry("12-27"), Some(dec));
        assert_eq!(parse_card_expiry("13/27"), None);
        assert_eq!(parse_card_expiry(""), None);
    }

    #[test]
    fn hosts_and_domains() {
        assert_eq!(host_of("https://www.Example.com/login?x=1"), "example.com");
        assert_eq!(host_of("example.org:8443/path"), "example.org");
        assert_eq!(host_of("https://user@mail.example.com"), "mail.example.com");
        assert_eq!(base_domain("mail.example.com"), "example.com");
        assert_eq!(base_domain("shop.example.co.uk"), "example.co.uk");
        assert_eq!(base_domain("localhost"), "localhost");
    }

    #[test]
    fn encodings() {
        assert_eq!(hex(&[0, 255, 16]), "00ff10");
        assert_eq!(unhex("00FF10"), Some(vec![0, 255, 16]));
        assert_eq!(unhex("0g"), None);
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(clean_remote("a\u{7}b\n", 10), "a b");
    }
}
