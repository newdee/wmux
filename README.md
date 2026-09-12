# wmux

A tmux-style terminal multiplexer for Windows. Sessions survive closing the
terminal, panes and windows split the screen, and PowerShell, WSL and cmd all
run inside panes with full key fidelity.

- **Native**: built on ConPTY and the Win32 console API. No Cygwin, no MSYS,
  no WSL requirement. Works in Windows Terminal, the classic console host,
  VS Code's terminal, and anything else that hosts a Windows console.
- **PowerShell and WSL first-class**: keystrokes are forwarded to panes as raw
  Windows key events (the same win32-input-mode protocol Windows Terminal
  uses), so PSReadLine chords, `Ctrl+Space`, `Shift+Enter`, arrows with
  modifiers, IME input and WSL/Linux TUIs behave exactly as they do outside
  wmux.
- **tmux muscle memory**: `C-b` prefix, `%` / `"` to split, `c` for a new
  window, `d` to detach, `[` for copy mode, `:` for a command prompt, the same
  command names on the CLI (`new-session`, `attach`, `ls`, `send-keys`, ...),
  and a `.tmux.conf`-style config file.

## Install

```powershell
cargo install --path .
```

Requires Windows 10 1809 or newer (ConPTY). Rust 1.85+ to build.

## Use

```powershell
wmux                      # new session, attached
wmux new -s work          # named session
wmux new -d -s bg wsl.exe # detached session running WSL
wmux ls                   # list sessions
wmux attach -t work       # re-attach (works from a different terminal window)
wmux send-keys -t work "git status" Enter
wmux kill-server
```

Inside a session, press the prefix (`Ctrl+b`) and then:

| Key | Action |
| --- | --- |
| `c` / `n` / `p` / `l` / `0-9` | new window / next / previous / last / select by index |
| `,` / `&` | rename / kill window |
| `%` / `"` | split left-right / top-bottom |
| arrows / `o` / `;` | move between panes / next pane / last pane |
| `Ctrl`+arrows, `Alt`+arrows | resize the current pane by 1 / by 5 |
| `z` | zoom (toggle) the current pane |
| `x` | kill the current pane |
| `{` / `}` | swap pane with previous / next |
| `!` | break the pane out into its own window |
| `[` / `PgUp` | copy mode (scroll back; `Space` starts a selection, `Enter` copies) |
| `]` | paste the clipboard |
| `:` | command prompt (`:split-window -h -c C:\src`, `:set mouse off`, ...) |
| `d` | detach |
| `?` | list key bindings |
| `s` / `w` | list sessions / windows |
| `(` / `)` | switch the client to the previous / next session |

Mouse: click selects a pane, drag a border to resize, click a window name on
the status line to select it, wheel scrolls (enters copy mode on the normal
screen, sends arrow keys to full-screen programs, and is passed through to
programs that ask for mouse events). Drag to select text; the selection is
copied to the Windows clipboard on release.

## Configuration

`%USERPROFILE%\.wmux.conf` (or `%USERPROFILE%\.config\wmux\wmux.conf`, or
the file named by `WMUX_CONFIG`) holds one command per line, tmux syntax:

```tmux
set -g prefix C-a
set -g default-shell wsl          # pwsh (default), powershell, wsl, cmd, or a path
# set -g default-command "wsl.exe -d Ubuntu"
set -g mouse on
set -g history-limit 10000
set -g status-position top
set -g status-style fg=black,bg=colour39
set -g pane-active-border-style fg=colour39
set -g base-index 1

bind h select-pane -L
bind j select-pane -D
bind k select-pane -U
bind l select-pane -R
bind | split-window -h
bind - split-window -v
bind -n M-Left previous-window     # -n: no prefix
bind -n M-Right next-window
bind r source-file ~/.wmux.conf
```

Options unknown to wmux but common in `.tmux.conf` (`escape-time`,
`default-terminal`, `status-left`, ...) are accepted and ignored, so an
existing tmux config can be reused as a starting point.

Every window and pane command accepts `-t target` as in tmux:
`session`, `session:window`, `:window`, `session:window.pane`, and the window
part may be an index, a name, `+`, `-` or `!`.

## How it works

`wmux` is a client. The first invocation starts a detached server process
(`wmux __server`) that owns every session; clients talk to it over a per-user
named pipe (`\\.\pipe\wmux-<user>-<socket>`, choose the socket with `-L`).
Each pane is a ConPTY with a `vt100` terminal model on the server side; the
server composites the visible panes, borders and status line into a frame and
sends only the cells that changed to the attached client, which writes them to
the console with VT sequences. The server exits when its last session ends.

Environment inside panes: `WMUX` (socket name) and `WMUX_PANE` (pane id).
Server log: `%LOCALAPPDATA%\wmux\server.log` (`WMUX_LOG=debug` for more).

## Development

```powershell
cargo test              # unit + pipe-level e2e + real-console tests (spawns cmd.exe panes)
cargo clippy --all-targets
```

The `tests/console.rs` suite runs the real `wmux.exe` inside a ConPTY, so the
console code path (raw input mode, alternate screen, detach cleanup) is
covered without a human at the keyboard.

## Not (yet) implemented

Relative to tmux: multiple clients on the same session see the same size
(last attach wins, no per-client viewport), no `status-left`/`status-right`
formats, no hooks, no named paste buffers (the Windows clipboard is the only
buffer), no `choose-tree` UI, no window layouts presets (`select-layout`).
