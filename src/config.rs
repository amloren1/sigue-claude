use regex::Regex;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Max retry attempts before giving up
    pub max_retries: u32,
    /// How often to poll the tmux pane (seconds)
    pub poll_interval_secs: u64,
    /// Extra wait after parsed reset time (seconds)
    pub margin_secs: u64,
    /// Fallback wait when reset time is unparseable (seconds)
    pub fallback_wait_secs: u64,
    /// Message to send on retry
    pub retry_message: String,
    /// Base backoff for server throttles (seconds) — doubles each retry
    pub throttle_base_secs: u64,
    /// Max backoff cap for server throttles (seconds)
    pub throttle_max_secs: u64,
    /// Extra regex patterns (case-insensitive) that count as rate limits.
    /// Use when Claude's messaging changes and the built-in patterns miss it.
    pub custom_patterns: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_retries: 20,
            poll_interval_secs: 5,
            margin_secs: 60,
            fallback_wait_secs: 300, // 5 minutes
            retry_message: "continue".to_string(),
            throttle_base_secs: 30,
            throttle_max_secs: 600, // 10 min cap
            custom_patterns: Vec::new(),
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|e| {
                eprintln!("[sigue] Bad config {}: {e}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Calculate backoff for server throttles: base * 2^(attempt-1), capped
    pub fn throttle_backoff(&self, attempt: u32) -> u64 {
        let backoff = self.throttle_base_secs * 2u64.pow(attempt.saturating_sub(1));
        backoff.min(self.throttle_max_secs)
    }

    /// Compile user-provided patterns once at startup. Bad patterns are
    /// logged and skipped — we don't want a typo to kill the monitor.
    pub fn compile_custom_patterns(&self) -> Vec<Regex> {
        self.custom_patterns
            .iter()
            .filter_map(|p| match Regex::new(&format!("(?i){p}")) {
                Ok(r) => Some(r),
                Err(e) => {
                    eprintln!("[sigue] Skipping bad custom pattern '{p}': {e}");
                    None
                }
            })
            .collect()
    }
}

fn config_path() -> PathBuf {
    dirs_or_home().join(".sigue-claude.json")
}

fn dirs_or_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}
