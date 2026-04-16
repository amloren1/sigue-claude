use std::process::Command;

/// Check if we're currently inside a tmux session.
pub fn is_inside_tmux() -> bool {
    std::env::var("TMUX").is_ok()
}

/// Get the current tmux pane identifier (e.g. "%3").
pub fn current_pane() -> Option<String> {
    let output = Command::new("tmux")
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
    let output = Command::new("tmux")
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
    let _ = Command::new("tmux")
        .args(["send-keys", "-t", pane, text, "Enter"])
        .status();
}

/// Check if a process is still running.
pub fn process_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
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
