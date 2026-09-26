# Platforms: what differs, and where it lives

keepane began on Windows. Most of it never cared: the layout, the terminal
model (vt100), rendering, the command language, the options, the pane
messages and the event log, the dashboard, MCP, the phone page. What does
care is gathered under `src/platform/`, one module per platform with the
same names inside, chosen at compile time:

```rust
// src/platform/mod.rs
#[cfg(windows)] mod windows;
#[cfg(windows)] pub use windows::*;
#[cfg(unix)] mod unix;
#[cfg(unix)] pub use unix::*;
```

No traits: the rest of keepane calls `crate::platform::ipc::connect(..)`
and the like, and a function one platform lacks fails the build of that
platform. The step that moved the Windows code there changed no behaviour;
its test run is the same, test by test.

## The seams

| Seam | Windows | Linux / macOS |
|---|---|---|
| `ipc`: server listens, client connects | named pipe `\\.\pipe\keepane-<user>-<socket>`, DACL for the user and SYSTEM | Unix socket `<runtime>/keepane-<uid>/<socket>`, the directory 0700 (tmux's model) |
| `process`: start the server detached; run a shell command (`shell_command` for `run-shell`, `pipe_command` for `pipe-pane`) | `CreateProcessW`, no inherited handles, out of the caller's job when allowed; pwsh, else Windows PowerShell, else cmd | `setsid`, stdio to /dev/null (leaves a session's process group, so SSH hang-up does not reach it); `/bin/sh -c` |
| `process::Tree`: a pane's (or a `run-shell`'s) process tree ends with it | kill-on-close job object | the pane's process group (`setsid` by portable-pty): `SIGHUP` then `SIGKILL` to `-pgid` |
| `console`: the client's terminal | console API: raw mode, key and mouse records, resize events | termios raw mode, a VT input parser that makes the same key records, `SIGWINCH` |
| key records to a pane | win32-input-mode sequences (what ConPTY wants) | plain VT sequences (`encode_key`), text as UTF-8 |
| `shell`: the default shell and keepane's hook | pwsh, else Windows PowerShell; prompt hook through `-NoExit -Command` | `$SHELL`, else `/bin/sh`; bash through `--rcfile`, zsh through `ZDOTDIR`, each sourcing the user's own file first |
| per-pane shell history | `KEEPANE_SHELL_HISTORY` read by the hook (PSReadLine) | `HISTFILE` set per pane |
| `random`: the phone page's key | `BCryptGenRandom` | `/dev/urandom` |
| `clipboard` | Win32 clipboard | OSC 52 to the attached terminal; `wl-copy` / `xclip` / `pbcopy` when there |
| `notify` | toast with a "go to pane" button | `notify-send` / `osascript`, no button |
| `proccwd`, `sysinfo::program_of`: a pane's current directory and program | process environment block, toolhelp | `/proc/<pid>/cwd` and `/proc/<pid>/stat`; macOS `proc_pidinfo` |
| `sysinfo`: CPU, memory, battery, the home as `~` | Win32 | `/proc/stat`, `/proc/meminfo`, `/sys/class/power_supply`; macOS `sysctl` |
| `shutdown`: save before the machine goes | `WM_QUERYENDSESSION` | `SIGTERM` / `SIGHUP` to the server |
| `startup`: the server at logon | per-user `Run` value, `conhost --headless` | later: a systemd user unit / a launchd agent |

Directories need no module of their own: the `dirs` crate gives each
platform's (`%LOCALAPPDATA%\keepane` on Windows, `~/.local/share/keepane`
on Linux, `~/Library/Application Support/keepane` on macOS), and the home
directory for `~/.keepane.conf`.

Behind `ipc` and `process` on Windows sits `winsec`: the user's SID and the
pipe's security descriptor, and the kill-on-close job object.

Windows only, with nothing to port: the Windows Terminal profile (`wt`),
the MSI and Scoop updater (`update`), the move from wmux (`legacy`) and
the PowerShell tab completer (`completion`, until bash and zsh have one).

## Order

1. Move the Windows code under `src/platform/windows/`, the rest calling
   `crate::platform::…`. No behaviour changes; the same tests pass, the
   same number, the same names.
2. `src/platform/unix/`, built and tested on Linux (WSL): the unit tests,
   then the end-to-end tests, whose harness talks to the server through
   `platform::ipc` and starts `/bin/sh` panes there instead of `cmd.exe`.
3. macOS: the same Unix code, with `proc` and `sysinfo` from macOS; built
   for the target (`cargo check --target x86_64-apple-darwin`) here, run
   on a Mac later. What was not run on a Mac is said so.
