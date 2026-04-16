use chrono::{Local, Duration as ChronoDuration};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

static LOG_LOCK: Mutex<()> = Mutex::new(());

/// Directory for sigue-claude logs. Created on first write.
pub fn log_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".sigue-claude").join("logs")
}

/// Path to today's log file.
pub fn today_log_path() -> PathBuf {
    let date = Local::now().format("%Y-%m-%d").to_string();
    log_dir().join(format!("{date}.log"))
}

/// Write a line to today's log file. Silently fails if the log can't
/// be written — we never want logging to disrupt the user's session.
pub fn log(line: &str) {
    let _guard = LOG_LOCK.lock();
    let dir = log_dir();
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = today_log_path();
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let ts = Local::now().format("%Y-%m-%d %H:%M:%S");
        let _ = writeln!(f, "[{ts}] {line}");
    }
}

/// Remove log files older than `days`. Silently ignores errors.
pub fn cleanup_old_logs(days: i64) {
    let dir = log_dir();
    let cutoff = Local::now() - ChronoDuration::days(days);
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if let Ok(date) = chrono::NaiveDate::parse_from_str(stem, "%Y-%m-%d") {
            let file_time = date.and_hms_opt(0, 0, 0).unwrap().and_local_timezone(Local).unwrap();
            if file_time < cutoff {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

#[macro_export]
macro_rules! slog {
    ($($arg:tt)*) => {{
        $crate::logger::log(&format!($($arg)*));
    }};
}
