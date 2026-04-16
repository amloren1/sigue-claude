use regex::Regex;
use std::sync::LazyLock;

/// Patterns that indicate a rate limit on their own — no reset time needed.
static STANDALONE_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"(?i)type\s*2b\s*rate\s*limit").unwrap(),
        Regex::new(r"(?i)temporarily limiting requests").unwrap(),
        Regex::new(r"(?i)server is temporarily").unwrap(),
        Regex::new(r"(?i)too many requests").unwrap(),
        Regex::new(r"(?i)429\s*(?:too many requests|rate limit)").unwrap(),
    ]
});

/// Patterns indicating the user has actually hit their account/usage limit.
/// Every pattern requires an action word (hit/exceeded/reached/over) — the
/// mere mention of "rate limit" or a "5-hour" window in claude's progress
/// bar is NOT enough to trigger a retry. Something has to have happened.
static LIMIT_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // "you've hit your 5-hour limit", "exceeded the limit", etc.
        Regex::new(r"(?i)(?:hit|exceeded|reached|over)\s+(?:your|the)\s+(?:\d+-hour\s+)?(?:usage\s+|rate\s+)?limit")
            .unwrap(),
        // "rate limit reached", "usage limit exceeded", "limit reached"
        Regex::new(r"(?i)(?:usage|rate|request)?\s*limit\s+(?:reached|exceeded|hit)").unwrap(),
        // "you've reached ..." / "you have hit ..."
        Regex::new(r"(?i)(?:you've|you\s+have)\s+(?:hit|exceeded|reached)\s+(?:your|the)").unwrap(),
        // "out of usage", "out of requests"
        Regex::new(r"(?i)out of\s+(?:usage|requests|quota|tokens)").unwrap(),
    ]
});

/// Patterns indicating a reset time is mentioned nearby.
static RESET_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"(?i)resets?\s+(?:at\s+)?\d{1,2}(?::\d{2})?\s*(?:am|pm)?").unwrap(),
        // Relative shorthand — "resets in 2h", "resets 1h27m", "resets 5m"
        Regex::new(r"(?i)resets?\s+(?:in\s+)?\d+\s*(?:hours?|minutes?|h|m)").unwrap(),
        Regex::new(r"(?i)try again in \d+\s*(?:hours?|minutes?|h|m)").unwrap(),
    ]
});

/// Strip ANSI escape sequences from terminal output.
pub fn strip_ansi(text: &str) -> String {
    static ANSI_RE: LazyLock<Regex> = LazyLock::new(|| {
        // CSI sequences, OSC sequences, and other escape sequences
        Regex::new(r"\x1b\[[\x20-\x3f]*[\x40-\x7e]|\x1b\][\s\S]*?(?:\x07|\x1b\\)|\x1b[PX_^][\s\S]*?(?:\x07|\x1b\\)").unwrap()
    });
    ANSI_RE.replace_all(text, "").into_owned()
}

#[derive(Debug, PartialEq)]
pub enum RateLimitKind {
    /// Server-side throttle (Type 2b) — no reset time, use exponential backoff
    ServerThrottle,
    /// Account usage limit — has a known reset time
    AccountLimit,
}

#[derive(Debug)]
pub struct RateLimitDetection {
    pub kind: RateLimitKind,
    pub message: Option<String>,
}

/// Check if text indicates a rate limit. Custom patterns (user-provided
/// via config) are evaluated first and treated as account limits — if a
/// reset time is nearby, it's used; otherwise the fallback wait applies.
pub fn detect_rate_limit(text: &str, custom: &[Regex]) -> Option<RateLimitDetection> {
    let clean = strip_ansi(text);
    let lines: Vec<&str> = clean.lines().collect();
    let full = lines.join("\n");

    // Custom user patterns — highest priority so users can override behavior
    for pat in custom.iter() {
        if pat.is_match(&full) {
            let message = lines
                .iter()
                .find(|l| pat.is_match(l))
                .map(|l| l.trim().to_string());
            return Some(RateLimitDetection {
                kind: RateLimitKind::AccountLimit,
                message,
            });
        }
    }

    // Check standalone patterns (Type 2b, 429, etc.)
    for pat in STANDALONE_PATTERNS.iter() {
        if pat.is_match(&full) {
            let message = lines.iter().find(|l| pat.is_match(l)).map(|l| l.trim().to_string());
            return Some(RateLimitDetection {
                kind: RateLimitKind::ServerThrottle,
                message,
            });
        }
    }

    // Check account limit patterns — require a nearby reset time
    let window: usize = 6;
    for (i, line) in lines.iter().enumerate() {
        if LIMIT_PATTERNS.iter().any(|p| p.is_match(line)) {
            let start = i.saturating_sub(window);
            let end = (i + window + 1).min(lines.len());
            let has_reset = lines[start..end]
                .iter()
                .any(|l| RESET_PATTERNS.iter().any(|p| p.is_match(l)));

            if has_reset {
                let message = lines[start..end]
                    .iter()
                    .find(|l| RESET_PATTERNS.iter().any(|p| p.is_match(l)))
                    .map(|l| l.trim().to_string());
                return Some(RateLimitDetection {
                    kind: RateLimitKind::AccountLimit,
                    message,
                });
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_type_2b() {
        let text = "API Error: Server is temporarily limiting requests (not your usage limit) · Type 2b rate limited. Please try again later.";
        let result = detect_rate_limit(text, &[]).unwrap();
        assert_eq!(result.kind, RateLimitKind::ServerThrottle);
    }

    #[test]
    fn detects_type_2b_multiline() {
        let text = "Some output\n⚠ API Error: Server is temporarily limiting requests\n· Type 2b rate limited. Please try again later.\nMore text";
        let result = detect_rate_limit(text, &[]).unwrap();
        assert_eq!(result.kind, RateLimitKind::ServerThrottle);
    }

    #[test]
    fn detects_account_limit_with_reset() {
        let text = "⚠ You've hit your 5-hour limit\n· resets 3pm (UTC)";
        let result = detect_rate_limit(text, &[]).unwrap();
        assert_eq!(result.kind, RateLimitKind::AccountLimit);
        assert!(result.message.unwrap().contains("resets 3pm"));
    }

    #[test]
    fn detects_limit_with_try_again() {
        let text = "Usage limit reached\nTry again in 2 hours";
        let result = detect_rate_limit(text, &[]).unwrap();
        assert_eq!(result.kind, RateLimitKind::AccountLimit);
    }

    #[test]
    fn no_false_positive_on_normal_text() {
        let text = "Here is some code that processes rate calculations\nand limits the output to 100 rows.";
        assert!(detect_rate_limit(text, &[]).is_none());
    }

    #[test]
    fn strips_ansi() {
        let text = "\x1b[31mType 2b rate limited\x1b[0m";
        let result = detect_rate_limit(text, &[]).unwrap();
        assert_eq!(result.kind, RateLimitKind::ServerThrottle);
    }

    #[test]
    fn detects_429() {
        let text = "Error 429 Too Many Requests";
        let result = detect_rate_limit(text, &[]).unwrap();
        assert_eq!(result.kind, RateLimitKind::ServerThrottle);
    }

    #[test]
    fn detects_custom_pattern() {
        let text = "Some output\nquota exhausted for this tier\nmore text";
        let custom = vec![Regex::new(r"(?i)quota exhausted").unwrap()];
        let result = detect_rate_limit(text, &custom).unwrap();
        assert_eq!(result.kind, RateLimitKind::AccountLimit);
        assert!(result.message.unwrap().contains("quota exhausted"));
    }

    #[test]
    fn progress_bar_alone_is_not_a_rate_limit() {
        // Regression: claude's normal TUI shows a progress bar like
        // "5h [####-----] 32% resets 1h27m" while you're well within
        // the window. This is NOT a rate limit — detection must not fire.
        let text = "5h [######--------------] 32% resets 1h27m  7d [##------------------] 12%";
        assert!(detect_rate_limit(text, &[]).is_none());
    }

    #[test]
    fn text_mentioning_limit_without_action_is_not_a_rate_limit() {
        // "rate limit" or "usage limit" as descriptive phrases (e.g. in
        // docs, code, or conversational text) should not trigger.
        let text = "The rate limit for this endpoint is 5 requests per second.";
        assert!(detect_rate_limit(text, &[]).is_none());
    }

    #[test]
    fn custom_pattern_takes_precedence() {
        // Text matches a standalone server-throttle pattern, but also the
        // custom pattern. Custom should win.
        let text = "Type 2b rate limited — this is our override case";
        let custom = vec![Regex::new(r"(?i)override case").unwrap()];
        let result = detect_rate_limit(text, &custom).unwrap();
        assert_eq!(result.kind, RateLimitKind::AccountLimit);
    }
}
