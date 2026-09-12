# wmux

[![CI](https://github.com/newdee/wmux/actions/workflows/ci.yml/badge.svg)](https://github.com/newdee/wmux/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/newdee/wmux)](https://github.com/newdee/wmux/releases)

[中文说明](README.zh-CN.md)

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

Grab `wmux-<version>-windows-x86_64.zip` from the
[releases page](https://github.com/newdee/wmux/releases), unzip, and put
`wmux.exe` somewhere on your `PATH`. Or build it yourself:

```powershell
cargo install --path .
```

Requires Windows 10 1809 or newer (ConPTY). Rust 1.88+ to build.

## Use

```powershell
wmux                      # new session, attached
wmux new -s work          # named session
wmux new -d -s bg wsl.exe # detached session running WSL
wmux ls                   # list sessions
wmux attach -t work       # re-attach (works from a different terminal window)
wmux send-keys -t work "git status" Enter
wmux capture-pane -p -t work   # print what the pane shows (-S -200 adds scrollback)
wmux kill-server
```

Inside a session, press the prefix (`Ctrl+b`) and then:

| Key | Action |
| --- | --- |
| `c` / `n` / `p` / `Tab` / `0-9` | new window / next / previous / last / select by index |
| `,` / `&` | rename / kill window |
| `%` / `"` | split left-right / top-bottom |
| `h` `j` `k` `l` or arrows / `o` / `;` | move between panes (vim keys) / next pane / last pane |
| `H` `J` `K` `L`, `Alt`+arrows / `Ctrl`+arrows | resize the current pane by 5 / by 1 |
| `S` | toggle `synchronize-panes` (type into every pane of the window; `S` flag on the status line) |
| `C-s` / `C-r` | save the session / restore saved sessions (see Resume) |
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

## Resume after a reboot

Every session is saved to its own file under `%LOCALAPPDATA%\wmux\sessions`
whenever its shape changes (windows, panes, layout, names, start commands
and directories) and when the server exits. A reboot, a crash or a
`kill-session` does not lose that file, so:

```powershell
wmux resume              # bring back every saved session, attach to the first
wmux resume work         # bring back (or just attach to) the session "work"
wmux list-saved          # what can be resumed, newest first
wmux delete-saved old    # forget one
wmux save-session -a     # save everything right now (prefix C-s saves the current one)
```

Resuming recreates the pane tree and starts each pane's original command
in the pane's last known directory. Like tmux-resurrect, it does not bring
back what those programs were doing or what was on screen. `set -g
restore-on-start on` makes a fresh server restore everything by itself;
`set -g autosave off` turns saving off; `sessions-dir` moves the files.

A pane's directory starts as the directory it was created in. Keep it
current the way terminals do, by letting the shell announce `cd`s (OSC 9;9,
which Windows Terminal understands too, or OSC 7 from bash/zsh), or set it
by hand with `wmux set-cwd` (no argument: the directory you run it from):

```powershell
# PowerShell profile: announce the directory at every prompt
function prompt { "`e]9;9;$PWD`e\" + "PS $PWD> " }
```

```bash
# WSL bash: /mnt/<drive>/... paths map back to Windows drives
PROMPT_COMMAND='printf "\e]7;file://%s%s\e\\" "$HOSTNAME" "$PWD"'
```

`list-panes` shows the recorded directory, and `#{pane_current_path}` puts
it on the status line.

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

### Status line

`status-left`, `status-right`, `window-status-format` and
`window-status-current-format` take tmux format strings: `#S` session, `#W`
window name, `#I` window index, `#P` pane index, `#T` pane title, `#H` host,
`#F` flags, `#{session_name}`-style names, `%H:%M` time fields,
`#[fg=colour39,bg=black,bold]` style changes and `#(command)`, which runs the
command every `status-interval` seconds (default 15) and shows its first
line. `status-left-length` / `status-right-length` clip.

```tmux
set -g status-right "#[fg=yellow]#(pwsh -NoProfile -c (Get-Date).ToString('HH:mm'))#[default] #H"
```

## Plugins

Plugins work the way tmux plugins do: a plugin is a directory with a
`<name>.wmux` (or `plugin.wmux`) file of wmux commands, plus any scripts it
needs, in any language. Scripts call back through the CLI: `WMUX` holds the
socket name and `WMUX_PANE` the pane, so `wmux -L $env:WMUX display-message
...` from a script reaches the right server.

```tmux
# ~/.wmux.conf
set -g plugin-path ~/.wmux/plugins       # default
set -g @plugin demo                       # ~/.wmux/plugins/demo/demo.wmux
set -g @plugin C:\src\my-plugin           # or a path (dir or file)
```

Inside a plugin file you have everything the config has, plus:

- `run-shell [-b] [-t target] command` runs a command (through pwsh) with
  the wmux environment; its output is shown when it finishes (`-b`: ignore).
- `set-hook -g <hook> <command>` runs a command when something happens:
  `after-new-session`, `after-new-window`, `after-split-window`,
  `after-select-window`, `after-select-pane`, `after-kill-pane`,
  `client-attached`, `client-detached`, `pane-exited`. `set-hook -gu <hook>`
  removes it; `show-hooks` lists them.
- `set -g @anything value` stores a user option; `show-options -gqv @anything`
  reads it back (from a script: `wmux -L $env:WMUX show-options -gqv @anything`).
- `#(command)` pieces on the status line (above).
- `load-plugin name-or-path` and `list-plugins` at runtime.

A minimal plugin that shows an agent's progress file on the status line and
pops the full log with `prefix A`:

```tmux
# ~/.wmux/plugins/agent-status/agent-status.wmux
set -g status-right "#[fg=cyan]#(pwsh -NoProfile -File ~/.wmux/plugins/agent-status/summary.ps1)#[default] %H:%M"
set -g status-interval 5
bind A run-shell "pwsh -NoProfile -Command Get-Content $env:TEMP\agent.log -Tail 30"
```

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

The pipe carries a DACL that admits only the creating user (and SYSTEM), the
Windows equivalent of tmux's mode-0700 socket directory. Every pane runs in a
kill-on-close job object, so `kill-pane`, `kill-session` and a server exit
take the whole process tree down (the equivalent of tmux hanging up the
process group), and a slow client console never makes the server buffer
frames without bound: it drops to a full redraw instead.

`vendor/vt100` is vt100 0.16.2 with a one-function fix for a panic when a
pane shrinks through a wide (CJK) character; see `vendor/vt100/WMUX-PATCH.md`.
The server also logs and survives any panic in a command (`server.log`).

## Development

```powershell
cargo test              # unit + pipe-level e2e + real-console tests (spawns cmd.exe panes)
cargo clippy --all-targets
```

The `tests/console.rs` suite runs the real `wmux.exe` inside a ConPTY, so the
console code path (raw input mode, alternate screen, detach cleanup) is
covered without a human at the keyboard.

## Not (yet) implemented

Relative to tmux: `synchronize-panes` applies to the current window (no
`-t`), multiple clients on the same session see the same size
(last attach wins, no per-client viewport), only the hooks listed above, no
`#{?cond,a,b}` conditionals in formats, no named paste buffers (the Windows
clipboard is the only buffer), no `choose-tree` UI, no window layout presets
(`select-layout`), no repeatable bindings (`bind -r` is accepted, the repeat
is ignored), and `list-panes -a`/`-s` always list the target window only.
