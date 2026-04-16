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

/// Patterns indicating the user hit their account/usage limit.
static LIMIT_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"(?i)(?:hit|exceeded|reached).*(?:your|the)\s*(?:\d+-hour\s+)?limit").unwrap(),
        Regex::new(r"(?i)\d+-hour limit").unwrap(),
        Regex::new(r"(?i)limit reached").unwrap(),
        Regex::new(r"(?i)usage limit").unwrap(),
        Regex::new(r"(?i)out of.*usage").unwrap(),
        Regex::new(r"(?i)rate limit").unwrap(),
    ]
});

/// Patterns indicating a reset time is mentioned nearby.
static RESET_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"(?i)resets?\s+(?:at\s+)?\d{1,2}(?::\d{2})?\s*(?:am|pm)?").unwrap(),
        Regex::new(r"(?i)resets?\s+in[:\s]\s*\d").unwrap(),
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

/// Check if text indicates a rate limit. Returns detection info or None.
pub fn detect_rate_limit(text: &str) -> Option<RateLimitDetection> {
    let clean = strip_ansi(text);
    let lines: Vec<&str> = clean.lines().collect();
    let full = lines.join("\n");

    // Check standalone patterns first (Type 2b, 429, etc.)
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
        let result = detect_rate_limit(text).unwrap();
        assert_eq!(result.kind, RateLimitKind::ServerThrottle);
    }

    #[test]
    fn detects_type_2b_multiline() {
        let text = "Some output\n⚠ API Error: Server is temporarily limiting requests\n· Type 2b rate limited. Please try again later.\nMore text";
        let result = detect_rate_limit(text).unwrap();
        assert_eq!(result.kind, RateLimitKind::ServerThrottle);
    }

    #[test]
    fn detects_account_limit_with_reset() {
        let text = "⚠ You've hit your 5-hour limit\n· resets 3pm (UTC)";
        let result = detect_rate_limit(text).unwrap();
        assert_eq!(result.kind, RateLimitKind::AccountLimit);
        assert!(result.message.unwrap().contains("resets 3pm"));
    }

    #[test]
    fn detects_limit_with_try_again() {
        let text = "Usage limit reached\nTry again in 2 hours";
        let result = detect_rate_limit(text).unwrap();
        assert_eq!(result.kind, RateLimitKind::AccountLimit);
    }

    #[test]
    fn no_false_positive_on_normal_text() {
        let text = "Here is some code that processes rate calculations\nand limits the output to 100 rows.";
        assert!(detect_rate_limit(text).is_none());
    }

    #[test]
    fn strips_ansi() {
        let text = "\x1b[31mType 2b rate limited\x1b[0m";
        let result = detect_rate_limit(text).unwrap();
        assert_eq!(result.kind, RateLimitKind::ServerThrottle);
    }

    #[test]
    fn detects_429() {
        let text = "Error 429 Too Many Requests";
        let result = detect_rate_limit(text).unwrap();
        assert_eq!(result.kind, RateLimitKind::ServerThrottle);
    }
}
