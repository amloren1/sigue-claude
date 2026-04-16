use std::process::{Command, Stdio};

/// Build a `tmux` command with stdout/stderr silenced. Every helper in
/// this module uses this so tmux chatter (warnings, "no such client",
/// etc.) never leaks into claude's TUI pane.
fn tmux() -> Command {
    let mut cmd = Command::new("tmux");
    cmd.stderr(Stdio::null());
    cmd
}

/// Check if we're currently inside a tmux session.
pub fn is_inside_tmux() -> bool {
    std::env::var("TMUX").is_ok()
}

/// Get the current tmux pane identifier (e.g. "%3").
pub fn current_pane() -> Option<String> {
    let output = tmux()
        .args(["display-message", "-p", "#{pane_id}"])
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

/// Capture the visible text of a tmux pane.
pub fn capture_pane(pane: &str) -> Option<String> {
    let output = tmux()
        .args(["capture-pane", "-t", pane, "-p"])
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        None
    }
}

/// Send keystrokes to a tmux pane.
pub fn send_keys(pane: &str, text: &str) {
    let _ = tmux()
        .args(["send-keys", "-t", pane, text, "Enter"])
        .status();
}

/// Check if a process is still running.
pub fn process_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Create a new tmux session and run a command inside it.
pub fn create_session(session_name: &str, command: &str) -> std::io::Result<()> {
    Command::new("tmux")
        .args(["new-session", "-d", "-s", session_name, command])
        .status()?;
    Ok(())
}

/// Attach to an existing tmux session (blocking — takes over the terminal).
pub fn attach_session(session_name: &str) -> std::io::Result<std::process::ExitStatus> {
    Command::new("tmux")
        .args(["attach-session", "-t", session_name])
        .status()
}

/// List all tmux session names. Returns empty vec if tmux not running.
pub fn list_sessions() -> Vec<String> {
    let output = tmux()
        .args(["list-sessions", "-F", "#{session_name}"])
        .output();
    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|s| s.to_string())
            .collect(),
        _ => Vec::new(),
    }
}

/// Kill a tmux session by name. Returns true on success.
pub fn kill_session(session_name: &str) -> bool {
    tmux()
        .args(["kill-session", "-t", session_name])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Get the session name that owns a given pane.
pub fn session_for_pane(pane: &str) -> Option<String> {
    let output = tmux()
        .args(["display-message", "-p", "-t", pane, "#{session_name}"])
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

/// Get the name of the command currently running in the foreground of a pane.
/// Used to verify claude is still in focus before sending retry keys
/// (prevents sending "continue" to a shell if the user suspended claude).
pub fn pane_current_command(pane: &str) -> Option<String> {
    let output = tmux()
        .args(["display-message", "-p", "-t", pane, "#{pane_current_command}"])
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

/// Set the @sigue_state user option on a session. The status bar is
/// configured with `status-interval=1`, so tmux will pick up the change
/// on the next tick (≤1s). We don't call `refresh-client` because it
/// targets clients (not sessions) and errors out when no client is attached
/// — polluting the TUI with "can't find client" messages.
pub fn set_sigue_state(session: &str, state: &str) {
    let _ = tmux()
        .args(["set-option", "-t", session, "@sigue_state", state])
        .status();
}

/// Configure a session's status bar to show sigue state.
/// Shows state (with a visible prefix when active) + time on the right.
pub fn configure_status_bar(session: &str) {
    // status-right shows sigue state (yellow when active) then time
    let status_right = "#{?#{==:#{@sigue_state},},,#[fg=black,bg=yellow,bold] #{@sigue_state} #[default] }%H:%M";
    let cmds: &[&[&str]] = &[
        &["set-option", "-t", session, "status", "on"],
        &["set-option", "-t", session, "status-right-length", "120"],
        &["set-option", "-t", session, "status-right", status_right],
        // 1s interval so the %H:%M clock and the @sigue_state countdown
        // tick visibly without any explicit refresh-client calls.
        &["set-option", "-t", session, "status-interval", "1"],
        &["set-option", "-t", session, "@sigue_state", ""],
    ];
    for args in cmds {
        let _ = tmux().args(*args).status();
    }
}
