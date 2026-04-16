mod config;
mod logger;
mod patterns;
mod time;
mod tmux;

use config::Config;
use patterns::{RateLimitKind, detect_rate_limit};
use std::process::{Command, ExitCode, Stdio, exit};
use std::thread;
use std::time::Duration;

fn find_claude_binary() -> String {
    Command::new("which")
        .arg("claude")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "claude".to_string())
}

fn is_print_mode(args: &[String]) -> bool {
    args.iter().any(|a| a == "-p" || a == "--print")
}

// ── Print mode: capture output, detect limits, retry ──

// Log to file AND stderr (for print mode, where stderr doesn't interfere
// with a TUI). Interactive monitor uses slog! (file only) instead.
macro_rules! elog {
    ($($arg:tt)*) => {{
        let msg = format!($($arg)*);
        eprintln!("[sigue] {msg}");
        $crate::logger::log(&msg);
    }};
}

fn run_print_mode(args: &[String]) -> ExitCode {
    let config = Config::load();
    let custom_patterns = config.compile_custom_patterns();
    let claude_bin = find_claude_binary();
    let mut retries = 0u32;

    logger::cleanup_old_logs(7);

    loop {
        let result = Command::new(&claude_bin)
            .args(args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();

        let output = match result {
            Ok(o) => o,
            Err(e) => {
                elog!("Failed to start claude: {e}");
                return ExitCode::from(1);
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{stdout}{stderr}");

        match detect_rate_limit(&combined, &custom_patterns) {
            None => {
                print!("{stdout}");
                eprint!("{stderr}");
                return ExitCode::from(output.status.code().unwrap_or(1) as u8);
            }
            Some(detection) => {
                retries += 1;
                if retries > config.max_retries {
                    elog!("Max retries ({}) reached. Giving up.", config.max_retries);
                    print!("{stdout}");
                    eprint!("{stderr}");
                    return ExitCode::from(1);
                }

                let wait_secs = match detection.kind {
                    RateLimitKind::ServerThrottle => {
                        let backoff = config.throttle_backoff(retries);
                        elog!(
                            "Server throttle detected. Backoff {backoff}s (attempt {retries}/{}).",
                            config.max_retries
                        );
                        backoff
                    }
                    RateLimitKind::AccountLimit => {
                        let secs = time::parse_wait_seconds(
                            &combined,
                            config.margin_secs,
                            config.fallback_wait_secs,
                        );
                        let msg = detection.message.as_deref().unwrap_or("unknown reset time");
                        elog!(
                            "Account limit hit: {msg}. Waiting {secs}s (attempt {retries}/{}).",
                            config.max_retries
                        );
                        secs
                    }
                };

                thread::sleep(Duration::from_secs(wait_secs));
            }
        }
    }
}

// ── Interactive mode: monitor tmux pane in background ──

/// Sleep for `total_secs`, updating the tmux status bar countdown every
/// second (short waits) or every 10s (long waits >2min, to avoid churn).
/// Also checks `pid` periodically — bails early if Claude exits.
fn countdown_sleep(total_secs: u64, session: &Option<String>, label: &str, pid: u32) {
    if total_secs == 0 {
        return;
    }
    let start = std::time::Instant::now();
    loop {
        let elapsed = start.elapsed().as_secs();
        if elapsed >= total_secs {
            break;
        }
        let remaining = total_secs - elapsed;
        if let Some(s) = session {
            tmux::set_sigue_state(s, &format!("{label} {}", format_duration(remaining)));
        }
        if !tmux::process_alive(pid) {
            return;
        }
        // Short remaining → tick every 1s (snappy countdown in status bar).
        // Long remaining → tick every 10s (less churn during hour-long waits).
        let tick = if remaining > 120 { 10 } else { 1 };
        let tick = (tick as u64).min(total_secs - elapsed);
        thread::sleep(Duration::from_secs(tick));
    }
}

fn format_duration(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

fn run_monitor(pane: &str, pid: u32) {
    let config = Config::load();
    let custom_patterns = config.compile_custom_patterns();
    let session = tmux::session_for_pane(pane);
    let mut consecutive_retries = 0u32;
    let mut consecutive_errors = 0u32;
    let mut clean_polls = 0u32;
    let mut waiting = false;
    let mut wait_polls = 0u32;
    // After sending retry, allow this many polls before giving up waiting
    // for the rate limit text to clear (prevents getting stuck when the
    // old message stays visible on screen).
    let max_wait_polls = 60; // 60 * poll_interval = 5 min at default
    // Number of consecutive clean polls needed to consider Claude "recovered"
    // and reset the backoff. At default 5s poll interval, 6 polls = 30s of
    // clean output means Claude is working again.
    let clean_polls_to_reset = 6u32;

    let set_state = |state: &str| {
        if let Some(s) = &session {
            tmux::set_sigue_state(s, state);
        }
    };

    logger::cleanup_old_logs(7);
    slog!("Monitor started (pane={pane}, pid={pid})");

    loop {
        if !tmux::process_alive(pid) {
            slog!("Claude process exited. Monitor stopping.");
            return;
        }

        let text = match tmux::capture_pane(pane) {
            Some(t) => {
                consecutive_errors = 0;
                t
            }
            None => {
                consecutive_errors += 1;
                if consecutive_errors >= 10 {
                    slog!("Pane gone. Monitor exiting.");
                    return;
                }
                thread::sleep(Duration::from_secs(config.poll_interval_secs));
                continue;
            }
        };

        if waiting {
            wait_polls += 1;
            if detect_rate_limit(&text, &custom_patterns).is_none() || wait_polls >= max_wait_polls {
                waiting = false;
                wait_polls = 0;
                set_state("");
            }
            thread::sleep(Duration::from_secs(config.poll_interval_secs));
            continue;
        }

        match detect_rate_limit(&text, &custom_patterns) {
            None => {
                clean_polls += 1;
                if clean_polls >= clean_polls_to_reset && consecutive_retries > 0 {
                    slog!(
                        "Claude recovered. Resetting backoff (was at attempt {consecutive_retries})."
                    );
                    consecutive_retries = 0;
                    set_state("");
                }
                thread::sleep(Duration::from_secs(config.poll_interval_secs));
            }
            Some(detection) => {
                clean_polls = 0;
                consecutive_retries += 1;
                if consecutive_retries > config.max_retries {
                    slog!(
                        "Max consecutive retries ({}) reached. Monitor stopping.",
                        config.max_retries
                    );
                    set_state("sigue: max retries reached");
                    return;
                }

                let max = config.max_retries;
                let (wait_secs, label) = match detection.kind {
                    RateLimitKind::ServerThrottle => {
                        let backoff = config.throttle_backoff(consecutive_retries);
                        slog!(
                            "Server throttle. Backoff {backoff}s (attempt {consecutive_retries}/{max})."
                        );
                        (
                            backoff,
                            format!("sigue: throttle retry {consecutive_retries}/{max} in"),
                        )
                    }
                    RateLimitKind::AccountLimit => {
                        let secs = time::parse_wait_seconds(
                            &text,
                            config.margin_secs,
                            config.fallback_wait_secs,
                        );
                        let msg = detection.message.as_deref().unwrap_or("unknown reset time");
                        slog!(
                            "Account limit: {msg}. Waiting {secs}s (attempt {consecutive_retries}/{max})."
                        );
                        (
                            secs,
                            format!("sigue: limit retry {consecutive_retries}/{max} in"),
                        )
                    }
                };

                countdown_sleep(wait_secs, &session, &label, pid);

                if !tmux::process_alive(pid) {
                    return;
                }

                // Re-check the pane: while we were waiting, claude may have
                // recovered on its own, or the user may have resumed the
                // session manually. Sending "continue" now would be spurious
                // input into whatever claude is currently doing.
                let fresh = tmux::capture_pane(pane).unwrap_or_default();
                if detect_rate_limit(&fresh, &custom_patterns).is_none() {
                    slog!(
                        "Rate limit gone after wait — claude recovered on its own, skipping retry."
                    );
                    set_state("");
                    // Don't set waiting=true — nothing to wait for. Treat
                    // this as a normal recovery so the next detection
                    // starts fresh.
                    continue;
                }

                // Safety: only send retry keys if claude is still the
                // foreground process in the pane. If the user suspended
                // claude (Ctrl-Z) or switched tasks, sending "continue"
                // would go to a shell instead — likely harmless but wrong.
                let fg = tmux::pane_current_command(pane);
                let looks_like_claude = matches!(
                    fg.as_deref(),
                    Some("claude" | "node" | "sigue-claude")
                );
                if !looks_like_claude {
                    let got = fg.as_deref().unwrap_or("<unknown>");
                    slog!(
                        "Foreground is '{got}', not claude — skipping retry. Will re-check on next poll."
                    );
                    set_state(&format!("sigue: paused (fg={got})"));
                    // Don't set waiting=true — stay in active detection
                    // mode so we try again once claude is back in focus.
                    thread::sleep(Duration::from_secs(config.poll_interval_secs));
                    continue;
                }

                set_state(&format!(
                    "sigue: retrying {consecutive_retries}/{max}..."
                ));
                tmux::send_keys(pane, &config.retry_message);
                waiting = true;
                wait_polls = 0;
                slog!("Sent '{}' to pane {pane}.", config.retry_message);
            }
        }
    }
}

fn run_interactive(args: &[String]) -> ExitCode {
    let claude_bin = find_claude_binary();
    let pane = tmux::current_pane();

    let mut child = match Command::new(&claude_bin)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[sigue] Failed to start claude: {e}");
            return ExitCode::from(1);
        }
    };

    let child_pid = child.id();

    if let Some(pane_id) = pane {
        thread::spawn(move || {
            run_monitor(&pane_id, child_pid);
        });
    }

    let status = child.wait().unwrap_or_else(|_| exit(1));
    ExitCode::from(status.code().unwrap_or(1) as u8)
}

fn run_in_new_tmux_session(args: &[String]) -> ExitCode {
    let session_name = format!("sigue-{}", std::process::id());

    let self_exe = std::env::current_exe().unwrap_or_else(|_| "sigue-claude".into());
    let escaped_args: Vec<String> = args.iter().map(|a| shell_escape(a)).collect();
    // When the inner sigue-claude exits (because claude exited), tmux will
    // automatically destroy the session. No `exec $SHELL` — that would keep
    // the session alive and create orphans.
    let inner_cmd = format!(
        "CLAUDE_AUTO_RETRY_ACTIVE=1 {} {}",
        shell_escape(&self_exe.to_string_lossy()),
        escaped_args.join(" ")
    );

    if let Err(e) = tmux::create_session(&session_name, &inner_cmd) {
        eprintln!("[sigue] Failed to create tmux session: {e}");
        return ExitCode::from(1);
    }

    tmux::configure_status_bar(&session_name);

    match tmux::attach_session(&session_name) {
        Ok(status) => ExitCode::from(status.code().unwrap_or(0) as u8),
        Err(e) => {
            eprintln!("[sigue] Failed to attach: {e}");
            ExitCode::from(1)
        }
    }
}

fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn print_help() {
    eprintln!("sigue-claude — auto-retry wrapper for Claude Code rate limits");
    eprintln!();
    eprintln!("Usage: sigue-claude [claude args...]");
    eprintln!();
    eprintln!("Wraps the `claude` CLI. Detects rate limits (server throttles");
    eprintln!("and account limits) and automatically retries with backoff.");
    eprintln!();
    eprintln!("Modes:");
    eprintln!("  Interactive (default) — runs inside tmux, monitors pane text");
    eprintln!("  Print (-p/--print)    — captures output, retries on limit");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  --list-sessions       list all sigue-claude tmux sessions");
    eprintln!("  --cleanup             kill all sigue-claude tmux sessions");
    eprintln!("  --status              show version, active sessions, log path");
    eprintln!("  --logs                print today's log file");
    eprintln!("  --version, -V         print version");
    eprintln!("  --help, -h            show this help");
    eprintln!();
    eprintln!("Logs: ~/.sigue-claude/logs/YYYY-MM-DD.log (auto-rotates, 7-day retention)");
    eprintln!();
    eprintln!("Config: ~/.sigue-claude.json (optional)");
    eprintln!("  max_retries          — max attempts (default: 10)");
    eprintln!("  poll_interval_secs   — tmux poll frequency (default: 5)");
    eprintln!("  margin_secs          — extra wait after reset (default: 60)");
    eprintln!("  fallback_wait_secs   — wait when time unparseable (default: 300)");
    eprintln!("  retry_message        — what to send (default: \"continue\")");
    eprintln!("  throttle_base_secs   — initial backoff for 2b errors (default: 30)");
    eprintln!("  throttle_max_secs    — max backoff cap (default: 600)");
    eprintln!("  custom_patterns      — extra regex patterns to detect (default: [])");
}

fn list_sigue_sessions() -> Vec<String> {
    tmux::list_sessions()
        .into_iter()
        .filter(|s| s.starts_with("sigue-"))
        .collect()
}

fn run_list_sessions() -> ExitCode {
    let sessions = list_sigue_sessions();
    if sessions.is_empty() {
        println!("No sigue-claude sessions running.");
    } else {
        println!("Active sigue-claude sessions:");
        for s in &sessions {
            println!("  {s}");
        }
        println!();
        println!("Attach:  tmux attach -t <name>");
        println!("Kill:    tmux kill-session -t <name>");
        println!("Kill all: sigue-claude --cleanup");
    }
    ExitCode::SUCCESS
}

fn run_cleanup() -> ExitCode {
    let sessions = list_sigue_sessions();
    if sessions.is_empty() {
        println!("No sigue-claude sessions to clean up.");
        return ExitCode::SUCCESS;
    }
    let mut killed = 0;
    for s in &sessions {
        if tmux::kill_session(s) {
            println!("Killed: {s}");
            killed += 1;
        } else {
            eprintln!("Failed to kill: {s}");
        }
    }
    println!("Cleaned up {killed}/{} session(s).", sessions.len());
    ExitCode::SUCCESS
}

fn run_logs() -> ExitCode {
    let path = logger::today_log_path();
    if !path.exists() {
        println!("No logs for today yet.");
        println!("Log dir: {}", logger::log_dir().display());
        return ExitCode::SUCCESS;
    }
    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            print!("{contents}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Failed to read log: {e}");
            ExitCode::from(1)
        }
    }
}

fn run_status() -> ExitCode {
    println!("sigue-claude v{}", env!("CARGO_PKG_VERSION"));
    println!();
    let sessions = list_sigue_sessions();
    if sessions.is_empty() {
        println!("Active sessions: none");
    } else {
        println!("Active sessions:");
        for s in &sessions {
            println!("  {s}");
        }
    }
    println!();
    println!("Log dir: {}", logger::log_dir().display());
    let path = logger::today_log_path();
    if path.exists() {
        println!("Today's log: {}", path.display());
    } else {
        println!("Today's log: (none yet)");
    }
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return ExitCode::SUCCESS;
    }

    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("sigue-claude {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    if args.iter().any(|a| a == "--list-sessions") {
        return run_list_sessions();
    }

    if args.iter().any(|a| a == "--cleanup") {
        return run_cleanup();
    }

    if args.iter().any(|a| a == "--logs") {
        return run_logs();
    }

    if args.iter().any(|a| a == "--status") {
        return run_status();
    }

    // Internal: monitor subprocess mode
    if args.first().map(|s| s.as_str()) == Some("__monitor") {
        if let (Some(pane), Some(pid)) = (args.get(1), args.get(2)) {
            if let Ok(pid) = pid.parse() {
                run_monitor(pane, pid);
                return ExitCode::SUCCESS;
            }
        }
        eprintln!("[sigue] Monitor: bad args");
        return ExitCode::from(1);
    }

    if is_print_mode(&args) {
        run_print_mode(&args)
    } else if tmux::is_inside_tmux() {
        run_interactive(&args)
    } else {
        run_in_new_tmux_session(&args)
    }
}
