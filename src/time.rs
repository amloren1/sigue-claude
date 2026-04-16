use chrono::{DateTime, Duration as ChronoDuration, Local, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use regex::Regex;
use std::sync::LazyLock;

// Capture group semantics:
//   1: hour
//   2: minutes (optional)
//   3: am/pm (optional)
//   4: timezone name inside parens — e.g. UTC, local, Europe/Dublin, EST (optional)
static RESET_AT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)resets?\s+(?:at\s+)?(\d{1,2})(?::(\d{2}))?\s*(am|pm)?\s*(?:\(([A-Za-z][A-Za-z0-9_/+\-]*)\))?"
    )
    .unwrap()
});

// Matches relative reset times in several forms:
//   resets in 2 hours
//   resets 1h27m          (claude's progress bar shorthand)
//   try again in 30 minutes
//   resets 5m
//   resets 2h
// Captures:
//   1: leading number
//   2: hour unit (hours|hour|h)        — present if hour form
//   3: optional trailing minutes after "Xh"
//   4: minute unit (minutes|minute|m)  — present if minute-only form
static RESET_IN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:resets?|try again)(?:\s+in)?\s*[:\s]?\s*(\d+)\s*(?:(hours?|h)(?:\s*(\d+)\s*(?:minutes?|m))?|(minutes?|m))",
    )
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
    let n: u64 = caps[1].parse().ok()?;
    // Hour form (with optional trailing minutes): "1h27m", "2 hours", "1h"
    if caps.get(2).is_some() {
        let mins: u64 = caps
            .get(3)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(0);
        return Some(n * 3600 + mins * 60);
    }
    // Minute-only form: "30 minutes", "5m"
    if caps.get(4).is_some() {
        return Some(n * 60);
    }
    None
}

/// A timezone spec — either a named IANA zone, UTC, or the user's local time.
enum TzSpec {
    Named(Tz),
    Utc,
    Local,
}

fn parse_timezone(s: &str) -> TzSpec {
    let lower = s.to_lowercase();
    if lower == "utc" || lower == "gmt" || lower == "z" {
        return TzSpec::Utc;
    }
    if lower == "local" {
        return TzSpec::Local;
    }
    // Try parsing as IANA name: "Europe/Dublin", "America/New_York", etc.
    if let Ok(tz) = s.parse::<Tz>() {
        return TzSpec::Named(tz);
    }
    // Common US abbreviations that chrono-tz doesn't resolve directly
    let mapped = match s.to_uppercase().as_str() {
        "EST" | "EDT" => Some(chrono_tz::US::Eastern),
        "CST" | "CDT" => Some(chrono_tz::US::Central),
        "MST" | "MDT" => Some(chrono_tz::US::Mountain),
        "PST" | "PDT" => Some(chrono_tz::US::Pacific),
        "BST" => Some(chrono_tz::Europe::London),
        "CET" | "CEST" => Some(chrono_tz::Europe::Paris),
        _ => None,
    };
    mapped.map(TzSpec::Named).unwrap_or(TzSpec::Local)
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

    let tz_spec = caps
        .get(4)
        .map(|m| parse_timezone(m.as_str()))
        .unwrap_or(TzSpec::Local);

    // Build a full DateTime in the reset's timezone for today. If that time
    // has already passed today, roll to tomorrow. Then compute the absolute
    // duration until that moment from `now` — this handles DST transitions
    // and half-hour offsets naturally because chrono works in real instants.
    let now_utc: DateTime<Utc> = Utc::now();
    let target_utc: DateTime<Utc> = match tz_spec {
        TzSpec::Utc => {
            let today_naive = now_utc.date_naive().and_time(reset_time);
            let today = Utc.from_utc_datetime(&today_naive);
            if today <= now_utc {
                today + ChronoDuration::days(1)
            } else {
                today
            }
        }
        TzSpec::Local => {
            let now_local = Local::now();
            let today_naive = now_local.date_naive().and_time(reset_time);
            // `from_local_datetime` returns Ambiguous on DST fall-back
            // (two instants match) or None on DST spring-forward (no instant
            // matches — the wall-clock time doesn't exist). Prefer the
            // later/earlier one over erroring out.
            let today = Local
                .from_local_datetime(&today_naive)
                .earliest()
                .or_else(|| Local.from_local_datetime(&today_naive).latest())?;
            let target_local = if today <= now_local {
                today + ChronoDuration::days(1)
            } else {
                today
            };
            target_local.with_timezone(&Utc)
        }
        TzSpec::Named(tz) => {
            let now_in_tz = now_utc.with_timezone(&tz);
            let today_naive = now_in_tz.date_naive().and_time(reset_time);
            let today = tz
                .from_local_datetime(&today_naive)
                .earliest()
                .or_else(|| tz.from_local_datetime(&today_naive).latest())?;
            let target_in_tz = if today <= now_in_tz {
                today + ChronoDuration::days(1)
            } else {
                today
            };
            target_in_tz.with_timezone(&Utc)
        }
    };

    let diff = (target_utc - now_utc).num_seconds().max(0) as u64;
    Some(diff)
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
    fn parse_compound_shorthand() {
        // Claude's progress-bar form: "resets 1h27m"
        let secs = try_parse_relative("5h [####-----] 32% resets 1h27m  7d").unwrap();
        assert_eq!(secs, 3600 + 27 * 60);
    }

    #[test]
    fn parse_bare_hour_shorthand() {
        let secs = try_parse_relative("resets 2h").unwrap();
        assert_eq!(secs, 7200);
    }

    #[test]
    fn parse_bare_minute_shorthand() {
        let secs = try_parse_relative("resets 45m").unwrap();
        assert_eq!(secs, 45 * 60);
    }

    #[test]
    fn compound_shorthand_beats_absolute_parse() {
        // Regression: "resets 1h27m" used to fall through to the
        // absolute-time parser and get interpreted as 1:00, which then
        // rolled to 1 AM tomorrow — a ~15h wait instead of the real 1h27m.
        let secs = parse_wait_seconds("limit, resets 1h27m", 0, 99999);
        assert_eq!(secs, 3600 + 27 * 60);
    }

    #[test]
    fn parse_absolute_utc_returns_something() {
        let result = try_parse_absolute("resets 3pm (UTC)");
        assert!(result.is_some());
        // Must always be in [0, 24h]
        let secs = result.unwrap();
        assert!(secs <= 24 * 3600);
    }

    #[test]
    fn parse_absolute_named_tz() {
        let result = try_parse_absolute("resets 3pm (Europe/Dublin)");
        assert!(result.is_some());
        assert!(result.unwrap() <= 24 * 3600);
    }

    #[test]
    fn parse_absolute_half_hour_offset_tz() {
        // India is UTC+5:30 — tests the non-integer-hour-offset path
        let result = try_parse_absolute("resets 3pm (Asia/Kolkata)");
        assert!(result.is_some());
        assert!(result.unwrap() <= 24 * 3600);
    }

    #[test]
    fn parse_absolute_us_abbreviation() {
        let result = try_parse_absolute("resets 3pm (PST)");
        assert!(result.is_some());
    }

    #[test]
    fn parse_absolute_no_tz_uses_local() {
        let result = try_parse_absolute("resets 3pm");
        assert!(result.is_some());
    }

    #[test]
    fn fallback_when_unparseable() {
        let secs = parse_wait_seconds("no time info here", 60, 18000);
        assert_eq!(secs, 18000);
    }
}
