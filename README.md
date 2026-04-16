# sigue-claude

Auto-retry wrapper for [Claude Code](https://docs.anthropic.com/en/docs/claude-code) rate limits. Detects server throttles (Type 2b) and account usage limits, waits for the reset window, and retries automatically.

*"Sigue"* — Spanish for *"keep going."*

## Prerequisites

- [Claude Code](https://docs.anthropic.com/en/docs/claude-code) CLI (`claude`)
- [tmux](https://github.com/tmux/tmux) (for interactive mode)
- [Rust toolchain](https://rustup.rs/) (to build from source)

### Install tmux

```bash
# macOS
brew install tmux

# Ubuntu/Debian
sudo apt install tmux

# Arch
sudo pacman -S tmux
```

## Install

```bash
cargo install --git https://github.com/amloren1/sigue-claude
```

Or build from source:

```bash
git clone https://github.com/amloren1/sigue-claude.git
cd sigue-claude
cargo install --path .
```

### Recommended tmux config

Add to `~/.tmux.conf` for a better experience:

```bash
set -g mouse on
set -g history-limit 50000
set -g default-terminal "screen-256color"
set -sg escape-time 10

# Forward terminal focus events so claude-code can re-render cleanly
# (silences the "tmux focus events off" warning at startup).
set -g focus-events on

# Mouse drag selection copies to system clipboard (macOS)
# Tip: hold Option (⌥) while dragging for native terminal selection instead
bind -T copy-mode-vi MouseDragEnd1Pane send-keys -X copy-pipe-and-cancel "pbcopy"
bind -T copy-mode MouseDragEnd1Pane send-keys -X copy-pipe-and-cancel "pbcopy"
```

Reload with `tmux source-file ~/.tmux.conf`.

On Linux, replace `pbcopy` with `xclip -selection clipboard` (X11) or `wl-copy` (Wayland).

## Usage

Use `sigue-claude` as a drop-in replacement for `claude`:

```bash
# Interactive mode (launches inside tmux)
sigue-claude

# Pass any claude flags through
sigue-claude --allowedTools "Bash,Read,Write"

# Print mode (captures output, retries on limit)
sigue-claude -p "explain this codebase"
```

### How it works

**Interactive mode** (default): Launches Claude inside a tmux session. A background monitor polls the terminal output every 5 seconds. When a rate limit is detected, it waits for the reset window and sends a retry message to resume the session. When Claude exits, the tmux session is destroyed automatically — no orphans.

**Print mode** (`-p`/`--print`): Captures Claude's output directly. If a rate limit is detected in stdout/stderr, it waits and re-runs the command.

### Live status in the tmux status bar

During a wait, sigue shows a yellow badge in the bottom-right of the tmux status bar with a live countdown:

- `sigue: throttle retry 1/10 in 28s` — server throttle backoff
- `sigue: limit retry 1/10 in 4h30m` — account limit wait
- `sigue: retrying 1/10...` — briefly after sending "continue"
- `sigue: paused (fg=zsh)` — claude isn't the foreground process (you suspended it)

When claude is working normally, the badge disappears.

### Smart behavior

- **Backoff resets on recovery**: after ~30s of clean output, the retry counter goes back to 1. Unrelated rate limit events don't escalate each other's backoff.
- **Skips spurious retries**: if claude recovers during the wait — either naturally or because you manually typed "continue" — sigue re-checks the pane when the timer ends and skips the retry.
- **Foreground-safe**: won't send "continue" if claude isn't the active process in the pane (e.g. you Ctrl-Z'd it). It pauses and waits for claude to come back.
- **Background-friendly**: detaching from tmux (`Ctrl-b d`) doesn't affect monitoring. Sigue keeps retrying whether or not you're watching.

### Session management

If you detach from a session with `Ctrl-b d` (or it gets orphaned from an older version), use these commands:

```bash
sigue-claude --list-sessions   # List active sigue-claude sessions
sigue-claude --cleanup         # Kill all sigue-claude sessions
tmux attach -t sigue-<pid>     # Re-attach to a detached session
```

### Logs and status

Monitor activity is written to `~/.sigue-claude/logs/YYYY-MM-DD.log` (not stderr, to avoid interfering with Claude's TUI). Logs auto-rotate daily and clean up after 7 days.

```bash
sigue-claude --logs      # Print today's log
sigue-claude --status    # Show version, active sessions, log path
sigue-claude --version   # Print version
```

### What it detects

| Type | Example | Strategy |
|------|---------|----------|
| Server throttle (Type 2b) | *"Server is temporarily limiting requests"* | Exponential backoff (30s base, 10m cap) |
| Account limit | *"You've hit your 5-hour limit, resets 3pm"* | Parses reset time, waits + 60s margin |
| HTTP 429 | *"Error 429 Too Many Requests"* | Exponential backoff |

Reset times are parsed with full timezone awareness:

- Relative: `try again in 2 hours`, `resets in 30 minutes`
- Compound shorthand: `resets 1h27m`, `resets 2h`, `resets 45m` (the form claude shows in its progress-bar overlay)
- Absolute UTC: `resets 3pm (UTC)`
- Named IANA zones: `resets 3pm (Europe/Dublin)`, `(Asia/Kolkata)`, `(America/New_York)`
- US abbreviations: `(EST)`, `(PST)`, `(CST)`, `(MST)` and daylight variants
- Half-hour offsets (India, Iran) and DST transitions are handled correctly via real `DateTime` arithmetic.

## Configuration

Optional. Create `~/.sigue-claude.json`:

```json
{
  "max_retries": 20,
  "poll_interval_secs": 5,
  "margin_secs": 60,
  "fallback_wait_secs": 300,
  "retry_message": "continue",
  "throttle_base_secs": 30,
  "throttle_max_secs": 600
}
```

| Key | Default | Description |
|-----|---------|-------------|
| `max_retries` | `20` | Max consecutive retry attempts before giving up |
| `poll_interval_secs` | `5` | How often to check tmux pane for rate limits (seconds) |
| `margin_secs` | `60` | Extra wait after parsed reset time (seconds) |
| `fallback_wait_secs` | `300` | Wait when reset time can't be parsed (seconds) |
| `retry_message` | `"continue"` | Message sent to resume the session |
| `throttle_base_secs` | `30` | Initial backoff for server throttles (doubles each retry) |
| `throttle_max_secs` | `600` | Maximum backoff cap (seconds) |
| `custom_patterns` | `[]` | Extra regex patterns (case-insensitive) treated as rate limits |

### Custom patterns

If Claude changes its error wording and the built-in patterns miss it, add your own. Each entry is a regex applied case-insensitively to the pane output. Matches are treated as account-limit detections — sigue parses any nearby reset time, or falls back to `fallback_wait_secs`.

```json
{
  "custom_patterns": [
    "quota.*exhausted",
    "too many tokens consumed today"
  ]
}
```

Invalid regexes are logged and skipped — a typo won't kill the monitor.

## License

[MIT](LICENSE)
