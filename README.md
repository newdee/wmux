# wmux

[![CI](https://github.com/newdee/wmux/actions/workflows/ci.yml/badge.svg)](https://github.com/newdee/wmux/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/newdee/wmux)](https://github.com/newdee/wmux/releases)

[中文说明](README.zh-CN.md) · **[Feature tour →](https://dfine.tech/wmux/)**

A tmux-style terminal multiplexer for Windows. Sessions survive closing the
terminal, panes and windows split the screen, and PowerShell, WSL and cmd all
run inside panes with full key fidelity.

<p align="center">
  <img src="docs/img/wmux-demo.gif" width="880"
       alt="Splitting a shell into panes, moving with h/j/k/l, zooming, the pane menu, the window picker, detaching and attaching again">
</p>

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
- **Background jobs that report back**: a window you are not looking at is
  marked when it prints, rings or goes quiet (`monitor-activity`,
  `C-b M-n` jumps to it), a pane whose program dies can keep its output and
  exit code (`remain-on-exit`), and a resumed session comes back with what
  each pane had on screen (`save-history`).

<p align="center">
  <img src="docs/img/wmux-alerts.gif" width="880"
       alt="A deploy finishes in a window nobody is looking at, the status line marks it with #, C-b M-n jumps there, a failing command leaves its pane and exit code behind, and a popup shows the window list">
</p>

## Install

From the [releases page](https://github.com/newdee/wmux/releases):

- `wmux-<version>-windows-x86_64.msi` installs into `Program Files`, puts
  `wmux` on the system `PATH` and uninstalls from "Apps & features"
  (`msiexec /i wmux-<version>-windows-x86_64.msi /qn` for an unattended
  install).
- `wmux-<version>-windows-x86_64.zip` is the same `wmux.exe` to unzip
  wherever you like.

Or build from source, which needs Rust 1.88+:

```powershell
cargo install --git https://github.com/newdee/wmux --locked   # latest master
cargo install --path .                                         # a local clone
```

Requires Windows 10 1809 or newer (ConPTY).

Building the installer yourself needs nothing but the repository; WiX is
downloaded on demand if it is not already installed:

```powershell
cargo build --release
pwsh -File installer/build-msi.ps1        # target\wmux-<version>-windows-x86_64.msi
```

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

Any unambiguous prefix of a command name works, as in tmux: `wmux att`,
`wmux lsp`, `wmux splitw -h`. `wmux kill` is refused, because four commands
start that way. `wmux list-commands` prints them all, and
[docs/tmux-parity.md](docs/tmux-parity.md) tracks them against tmux's own
list, command by command and key by key.

Inside a session, press the prefix (`Ctrl+b`) and then:

| Key | Action |
| --- | --- |
| `c` / `n` / `p` / `Tab` / `0-9` | new window / next / previous / last / select by index |
| `,` / `&` | rename / kill window |
| `%` / `"` | split left-right / top-bottom |
| `h` `j` `k` `l` or arrows / `o` / `;` | move between panes (vim keys) / next pane / last pane |
| `H` `J` `K` `L`, `Alt`+arrows / `Ctrl`+arrows | resize the current pane by 5 / by 1 |
| (moving, resizing, `n` / `p` and `{` / `}` repeat: after the prefix, keep pressing the key for half a second, `repeat-time`) | |
| `S` | toggle `synchronize-panes` (type into every pane of the window; `S` flag on the status line) |
| `C-s` / `C-r` | save the session / restore saved sessions (see Resume) |
| `z` | zoom (toggle) the current pane |
| `x` | kill the current pane |
| `{` / `}` | swap pane with previous / next |
| `q` | show the pane numbers; press one to go there |
| `Space` / `M-1`…`M-5` / `E` | cycle the layout / pick one (even-horizontal, even-vertical, main-horizontal, main-vertical, tiled) / even out the panes next to this one |
| `C-o` / `M-o` | rotate the panes through the layout |
| `!` | break the pane out into its own window |
| `m` / `M` | mark this pane / clear the mark (`join-pane` takes the marked one) |
| `T` / `f` | name this pane / find a window by name or title |
| `[` / `PgUp` | copy mode (see below) |
| `#` / `-` / `=` | list paste buffers / delete the newest / pick one to paste |
| `t` / `~` / `r` | clock / recent messages / redraw |
| `]` | paste the clipboard |
| `:` | command prompt (`:split-window -h -c C:\src`, `:set mouse off`, ...) |
| `d` | detach |
| `?` | list key bindings |
| `s` / `w` | pick a session / a window from a list (`j` `k` or arrows move, `g` `G` top/bottom, `0-9` jump, `Enter` selects, `q` cancels) |
| `(` / `)` | switch the client to the previous / next session |
| `D` | pick a client from a list and detach it |
| `>` / `<` | pane menu / window menu (the letter in brackets runs the entry, `Enter` runs the highlighted one) |
| `M-n` / `M-p` | next / previous window with an alert (see `monitor-activity`) |

In copy mode: `h` `j` `k` `l` and the arrows move, `w` `b` `e` walk words,
`0` `^` `$` and `H` `M` `L` and `{` `}` and `g` `G` jump, a count repeats
(`3j`), `Space` or `v` starts a selection, `C-v` makes it a rectangle,
`Enter` or `y` copies (to a paste buffer and the Windows clipboard), `/`
searches forward and `?` back through the scrollback with `n` / `N` to
repeat, `q` leaves. A script can drive all of it with
`send-keys -X <command>`, the same command names tmux uses.

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

Resuming recreates the pane tree, puts back the last `save-history` lines
each pane had on screen (500 by default; `set -g save-history all` keeps
the whole scrollback, colours included) and starts each pane's original
command in the pane's last known directory. What the programs themselves
were doing does not come back; no multiplexer can do that. `set -g
restore-on-start on` makes a fresh server restore everything by itself;
`set -g autosave off` turns saving off; `sessions-dir` moves the files.

To have all of that happen by itself when you log on:

```powershell
wmux startup on          # start the server at logon and restore every saved session
wmux startup status      # what is registered
wmux startup off         # stop doing that
```

This writes one value under the per-user `Run` registry key (no
administrator rights, nothing in Task Scheduler), running the server
through `conhost --headless` so no console window appears at logon. After a
reboot, `wmux attach` finds everything as it was.

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
the file named by `WMUX_CONFIG`) holds one command per line, tmux syntax.
With no wmux config at all, an existing `~/.tmux.conf` (or
`~/.config/tmux/tmux.conf`) is read instead: what wmux understands is
applied, and what it cannot use (`bind -T copy-mode-vi`, TPM's `@plugin`
lines, `%if` blocks) is skipped and listed in `show-messages` rather than
thrown at you on every attach.

```tmux
set -g prefix C-a
set -g default-shell wsl          # pwsh (default), powershell, wsl, cmd, or a path
# set -g default-command "wsl.exe -d Ubuntu"
set -g mouse on
set -g history-limit 10000
set -g status-position top
set -g status-style fg=black,bg=colour39
set -g pane-active-border-style fg=colour39
set -g base-index 1               # windows and panes counted from 1
set -g pane-base-index 1

set -g repeat-time 500            # how long a `bind -r` key keeps working; 0 disables

set -g remain-on-exit on          # keep a pane whose program exited, showing why
set -g save-history 500           # lines of each pane saved for `resume`; 0 saves none, all saves the whole scrollback
set -g monitor-activity on        # flag a background window that prints (`#` on the status line)
set -g monitor-bell on            # and one that rings the bell (`!`); on by default
set -g monitor-silence 60         # flag one that has said nothing for 60s (`~`); 0 disables
set -g visual-bell on             # say it on the status line instead of ringing

bind -r h select-pane -L          # -r: press h h h after one prefix
bind -r j select-pane -D
bind -r k select-pane -U
bind -r l select-pane -R
bind | split-window -h
bind - split-window -v
bind -n M-Left previous-window     # -n: no prefix
bind -n M-Right next-window
bind r source-file ~/.wmux.conf

set -ag status-right " | wmux"    # -a adds to what the option already holds
source-file ~/.wmux/themes/nord.conf
```

Option names take an unambiguous abbreviation, the way command names do:
`set sync` is `set synchronize-panes`, `set mon-act on` is
`set monitor-activity on`, and each dash-separated word can be shortened
(`set w-s-f "#I:#W"`). An on/off option with no value flips: `set mouse`,
`set sync`. Something that could mean several options says which
(`set mon` lists the three `monitor-*` ones).

Options unknown to wmux but common in `.tmux.conf` (`escape-time`,
`default-terminal`, ...) are accepted and ignored, so an existing tmux config
can be reused as a starting point.

### Themes

`themes/` in this repository holds ready-made colour schemes (Nord, Gruvbox
dark, Dracula, Catppuccin Mocha). They are ordinary wmux commands, so a theme
is just a file to source and an easy thing to copy and edit:

```tmux
source-file ~/.wmux/themes/dracula.conf
```

### Status line

`status-left`, `status-right`, `window-status-format`,
`window-status-current-format` and `pane-border-format` take tmux format
strings: `#S` session, `#W` window name, `#I` window index, `#P` pane
index, `#T` pane title, `#H` host, `#F` flags, `#{session_name}`-style
names, `#{?flag,then,else}` conditionals (`#{?window_flags,busy,idle}`,
`#{?session_name==work,…,…}`), `%H:%M` time fields,
`#[fg=colour39,bg=black,bold]` style changes and `#(command)`, which runs the
command every `status-interval` seconds (default 15) and shows its first
line. `status-left-length` / `status-right-length` clip.

Variables: `session_name` `session_id` `session_windows` `session_attached`
`session_created`, `window_name` `window_id` `window_index` `window_panes`
`window_active` `window_last_flag` `window_zoomed_flag` `window_width`
`window_height` `window_bell_flag` `window_activity_flag`
`window_silence_flag` `window_flags`, `pane_index` `pane_id` `pane_title`
`pane_current_command` `pane_start_command` `pane_current_path` `pane_width`
`pane_height` `pane_active` `pane_dead` `pane_dead_status`
`pane_synchronized` `pane_in_mode` `pane_pid`, `client_width`
`client_height`, `host` `host_short` `socket_path` `version` `pid`.
Modifiers, as in tmux: `#{=10:pane_title}` (first 10 characters),
`#{=-10:…}` (last 10), `#{b:pane_current_path}` (basename), `#{d:…}`
(dirname), `#{t:session_created}` (a time as a clock), `#{s/foo/bar/:…}`
(substitution); they nest (`#{=8:b:pane_current_path}`).

`set -g pane-border-status top` (or `bottom`) puts a line of
`pane-border-format` on every pane's border — by default the pane's number
and title, the active pane's in bold.

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

Commands a script or a binding reaches for, beyond the obvious ones
(`wmux list-commands` prints all 85, and any unambiguous prefix works):

- `pipe-pane [-o] [-t target] [command]` copies everything a pane prints into
  a command's standard input; with no command it stops. `wmux pipe-pane
  "$input | Add-Content build.log"` keeps a build log without touching the
  build.
- `wait-for [-L|-U|-S] channel` blocks until another client signals the
  channel (or unlocks it), so two scripts can take turns: `wmux wait-for
  ready` waits, `wmux wait-for -S ready` releases it.
- `display-menu [-T title] name key command ...` opens a menu over the
  window; an empty name is a separator. `display-popup [-E] [-w W] [-h H]
  [-d dir] [command]` runs a program in a box over the window (`-E` closes
  it when the program exits, `-C` closes it from outside).
- `choose-client` lists the attached clients and detaches the one picked.
- `send-keys -X <copy-command>` drives copy mode (`search-backward`,
  `begin-selection`, `copy-selection`, ... — the tmux names).
- `capture-pane -p [-e]` prints a pane, with the colours if asked.
- `find-text pattern` looks through what **every pane has printed**, not
  just the window names, and says where each hit is and how far back:
  `ft:0.0  -8  REDIS-TIMEOUT-here`. `-C` matches case, `-t` narrows to a
  session or window, `-n` caps the hits per pane. Neither tmux nor
  `find-window` can do this: `find-window` searches names and titles.
- `record [-t target] out.cast` writes everything a pane prints from now
  on as an [asciinema](https://asciinema.org) v2 file (resizes included);
  `record -t target` with no path stops. Play it with `asciinema play`, or
  upload it, or embed it in a page: a terminal session someone else can
  watch, not a screenshot.
- `notify [-T title] message` raises a desktop notification, and
  `set -g notify on` sends one for every alert, so a job that ends while
  the terminal is behind other windows still reaches you.

Every window and pane command accepts `-t target` as in tmux:
`session`, `session:window`, `:window`, `session:window.pane`, and the window
part may be an index, a name, `+`, `-` or `!`. The one exception is
`select-pane`, whose `-t` takes the pane to move to (`next`, `last` or an
index) rather than a window target, next to `-L` `-R` `-U` `-D`.

## How it works

`wmux` is a client. The first invocation starts a detached server process
(`wmux __server`) that owns every session; clients talk to it over a per-user
named pipe (`\\.\pipe\wmux-<user>-<socket>`, choose the socket with `-L`).
Each pane is a ConPTY with a `vt100` terminal model on the server side; the
server composites the visible panes, borders and status line into a frame and
sends only the cells that changed to the attached client, which writes them to
the console with VT sequences. The server exits when its last session ends.

Environment inside panes: `WMUX` (socket name) and `WMUX_PANE` (pane id). A
`wmux` command run inside a pane talks to the server that owns it, the way
`$TMUX` works for tmux, so `wmux ls` from a script or a plugin needs no `-L`.
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
`-t`), multiple clients on the same session see the same size (last attach
wins, no per-client viewport, so `refresh-client -U`/`-D` do nothing), only
the hooks listed above, `swap-window` works inside one session (`move-window`
crosses sessions), `bind -r` has no per-key repeat count, `choose-tree`
without per-session collapsing, tagging or a filter, `swap-pane` swaps with
the previous or next pane (`-U` / `-D`; `-s` and `-t` both name the pane to
swap, there is no pair form), `select-layout` has the five named layouts and
`-E` but not tmux's layout strings, `list-panes -a`/`-s` always list the
target window only, `pipe-pane` copies pane output into a command but has no
`-I` the other way, and `display-popup` is always centred (no `-x`/`-y`) and
gives the prefix key to wmux rather than to the program in the box.

`docs/tmux-parity.md` has the command-by-command and key-by-key list.
