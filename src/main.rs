mod config;
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

fn run_print_mode(args: &[String]) -> ExitCode {
    let config = Config::load();
    let claude_bin = find_claude_binary();
    let mut retries = 0u32;

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
                eprintln!("[sigue] Failed to start claude: {e}");
                return ExitCode::from(1);
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{stdout}{stderr}");

        match detect_rate_limit(&combined) {
            None => {
                print!("{stdout}");
                eprint!("{stderr}");
                return ExitCode::from(output.status.code().unwrap_or(1) as u8);
            }
            Some(detection) => {
                retries += 1;
                if retries > config.max_retries {
                    eprintln!(
                        "[sigue] Max retries ({}) reached. Giving up.",
                        config.max_retries
                    );
                    print!("{stdout}");
                    eprint!("{stderr}");
                    return ExitCode::from(1);
                }

                let wait_secs = match detection.kind {
                    RateLimitKind::ServerThrottle => {
                        let backoff = config.throttle_backoff(retries);
                        eprintln!(
                            "[sigue] Server throttle detected. Backoff {backoff}s (attempt {retries}/{}).",
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
                        eprintln!(
                            "[sigue] Account limit hit: {msg}. Waiting {secs}s (attempt {retries}/{}).",
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

fn run_monitor(pane: &str, pid: u32) {
    let config = Config::load();
    let mut retries = 0u32;
    let mut consecutive_errors = 0u32;
    let mut waiting = false;

    loop {
        if !tmux::process_alive(pid) {
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
                    eprintln!("[sigue] Pane gone. Monitor exiting.");
                    return;
                }
                thread::sleep(Duration::from_secs(config.poll_interval_secs));
                continue;
            }
        };

        if waiting {
            if detect_rate_limit(&text).is_none() {
                waiting = false;
            }
            thread::sleep(Duration::from_secs(config.poll_interval_secs));
            continue;
        }

        match detect_rate_limit(&text) {
            None => {
                thread::sleep(Duration::from_secs(config.poll_interval_secs));
            }
            Some(detection) => {
                retries += 1;
                if retries > config.max_retries {
                    eprintln!(
                        "[sigue] Max retries ({}) reached. Monitor stopping.",
                        config.max_retries
                    );
                    return;
                }

                let wait_secs = match detection.kind {
                    RateLimitKind::ServerThrottle => {
                        let backoff = config.throttle_backoff(retries);
                        eprintln!("[sigue] Server throttle. Backoff {backoff}s.");
                        backoff
                    }
                    RateLimitKind::AccountLimit => {
                        let secs = time::parse_wait_seconds(
                            &text,
                            config.margin_secs,
                            config.fallback_wait_secs,
                        );
                        let msg = detection.message.as_deref().unwrap_or("unknown reset time");
                        eprintln!("[sigue] Account limit: {msg}. Waiting {secs}s.");
                        secs
                    }
                };

                thread::sleep(Duration::from_secs(wait_secs));

                if !tmux::process_alive(pid) {
                    return;
                }

                tmux::send_keys(pane, &config.retry_message);
                waiting = true;
                eprintln!(
                    "[sigue] Sent '{}' to pane {pane}.",
                    config.retry_message
                );
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
    let inner_cmd = format!(
        "CLAUDE_AUTO_RETRY_ACTIVE=1 {} {}; exec $SHELL",
        shell_escape(&self_exe.to_string_lossy()),
        escaped_args.join(" ")
    );

    if let Err(e) = tmux::create_session(&session_name, &inner_cmd) {
        eprintln!("[sigue] Failed to create tmux session: {e}");
        return ExitCode::from(1);
    }

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
    eprintln!("Config: ~/.sigue-claude.json (optional)");
    eprintln!("  max_retries          — max attempts (default: 10)");
    eprintln!("  poll_interval_secs   — tmux poll frequency (default: 5)");
    eprintln!("  margin_secs          — extra wait after reset (default: 60)");
    eprintln!("  fallback_wait_secs   — wait when time unparseable (default: 300)");
    eprintln!("  retry_message        — what to send (default: \"continue\")");
    eprintln!("  throttle_base_secs   — initial backoff for 2b errors (default: 30)");
    eprintln!("  throttle_max_secs    — max backoff cap (default: 600)");
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return ExitCode::SUCCESS;
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
