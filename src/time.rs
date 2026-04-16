use chrono::{Local, NaiveTime, TimeDelta, Utc};
use regex::Regex;
use std::sync::LazyLock;

static RESET_AT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)resets?\s+(?:at\s+)?(\d{1,2})(?::(\d{2}))?\s*(am|pm)?\s*\(?(UTC|local)?\)?")
        .unwrap()
});

static RESET_IN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:resets?\s+in|try again in)[:\s]\s*(\d+)\s*(hours?|minutes?|h|m)")
        .unwrap()
});

/// Parse a reset time from screen text and return wait duration in seconds.
/// Falls back to `fallback_secs` if unparseable.
pub fn parse_wait_seconds(text: &str, margin_secs: u64, fallback_secs: u64) -> u64 {
    if let Some(secs) = try_parse_relative(text) {
        return secs + margin_secs;
    }
    if let Some(secs) = try_parse_absolute(text) {
        return secs + margin_secs;
    }
    fallback_secs
}

fn try_parse_relative(text: &str) -> Option<u64> {
    let caps = RESET_IN_RE.captures(text)?;
    let amount: u64 = caps[1].parse().ok()?;
    let unit = &caps[2];
    let secs = if unit.starts_with('h') {
        amount * 3600
    } else {
        amount * 60
    };
    Some(secs)
}

fn try_parse_absolute(text: &str) -> Option<u64> {
    let caps = RESET_AT_RE.captures(text)?;
    let mut hour: u32 = caps[1].parse().ok()?;
    let minute: u32 = caps.get(2).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);

    if let Some(ampm) = caps.get(3) {
        let ampm = ampm.as_str().to_lowercase();
        if ampm == "pm" && hour != 12 {
            hour += 12;
        } else if ampm == "am" && hour == 12 {
            hour = 0;
        }
    }

    let reset_time = NaiveTime::from_hms_opt(hour, minute, 0)?;

    let is_utc = caps
        .get(4)
        .map(|m| m.as_str().eq_ignore_ascii_case("utc"))
        .unwrap_or(false);

    let now = if is_utc {
        Utc::now().time()
    } else {
        Local::now().time()
    };

    let mut diff = reset_time - now;
    // If the time is in the past, it means tomorrow
    if diff < TimeDelta::zero() {
        diff += TimeDelta::hours(24);
    }

    Some(diff.num_seconds().max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_relative_hours() {
        let secs = try_parse_relative("Try again in 2 hours").unwrap();
        assert_eq!(secs, 7200);
    }

    #[test]
    fn parse_relative_minutes() {
        let secs = try_parse_relative("resets in 30 minutes").unwrap();
        assert_eq!(secs, 1800);
    }

    #[test]
    fn parse_relative_shorthand() {
        let secs = try_parse_relative("resets in: 5m").unwrap();
        assert_eq!(secs, 300);
    }

    #[test]
    fn parse_absolute_returns_something() {
        // Can't assert exact value since it depends on current time,
        // but it should parse without panicking
        let result = try_parse_absolute("resets 3pm (UTC)");
        assert!(result.is_some());
    }

    #[test]
    fn fallback_when_unparseable() {
        let secs = parse_wait_seconds("no time info here", 60, 18000);
        assert_eq!(secs, 18000); // 5 hour fallback
    }
}
