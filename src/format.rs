//! Human-readable sizes, counts, percentages and dates.

/// Decimal (SI) units, matching Finder: 1 KB = 1000 bytes.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["KB", "MB", "GB", "TB", "PB", "EB"];
    if n < 1000 {
        return format!("{n} B");
    }
    let mut value = n as f64;
    let mut unit = 0;
    value /= 1000.0;
    while value >= 999.5 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if value < 9.995 {
        format!("{value:.2} {}", UNITS[unit])
    } else if value < 99.95 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// Thousands-separated integer: 1234567 -> "1,234,567".
pub fn count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub fn percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "-".to_owned();
    }
    format!("{:.1}%", fraction(part, whole) * 100.0)
}

pub fn fraction(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

/// Local date and time for a Unix timestamp in seconds.
pub fn date(secs: i64) -> String {
    use chrono::TimeZone;
    if secs <= 0 {
        return String::new();
    }
    match chrono::Local.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) => {
            t.format("%Y-%m-%d %H:%M").to_string()
        }
        chrono::LocalResult::None => String::new(),
    }
}

pub fn duration(secs: f64) -> String {
    if secs < 60.0 {
        format!("{secs:.1} s")
    } else {
        let total = secs.round() as u64;
        format!("{}:{:02}", total / 60, total % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_use_finder_decimal_units() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1000), "1.00 KB");
        assert_eq!(bytes(12_345_678), "12.3 MB");
        assert_eq!(bytes(123_456_789_012), "123 GB");
        assert_eq!(bytes(999_600), "1.00 MB");
        assert_eq!(bytes(2_000_000_000_000), "2.00 TB");
    }

    #[test]
    fn counts_have_thousands_separators() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1000), "1,000");
        assert_eq!(count(1_234_567), "1,234,567");
    }

    #[test]
    fn percent_handles_empty_totals() {
        assert_eq!(percent(1, 0), "-");
        assert_eq!(percent(1, 4), "25.0%");
    }

    #[test]
    fn durations_switch_to_minutes() {
        assert_eq!(duration(3.21), "3.2 s");
        assert_eq!(duration(125.0), "2:05");
    }
}
