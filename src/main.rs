//! wmux: a tmux-like terminal multiplexer for Windows (ConPTY, PowerShell, WSL).

use anyhow::Result;
use wmux::{client, logger, server};

const USAGE: &str = "\
usage: wmux [-L socket-name] [command [flags]]

Sessions:
  new-session   (new)     [-s name] [-n window] [-c dir] [-d [-x cols] [-y rows]] [command...]
  attach-session (attach) [-t target] [-d]
  list-sessions (ls)
  kill-session  [-t target]      kill-server      has-session -t target
  rename-session [-t target] name
Windows / panes (from inside a session, or with -t):
  new-window (neww) [-n name] [-c dir] [command...]   kill-window   rename-window name
  select-window -t N   next-window   previous-window   last-window   list-windows
  swap-window -s A -t B   move-window -s A -t B   select-layout [-n|-p] [name]
  split-window (splitw) [-h|-v] [-c dir] [command...]  kill-pane   select-pane -L|-R|-U|-D
  resize-pane -L|-R|-U|-D [n] | -Z   swap-pane -U|-D   break-pane   list-panes
  send-keys [-t target] keys... | -X copy-command   capture-pane -p [-e] [-S -N]   copy-mode
  join-pane [-h|-v] -s A -t B   rotate-window   respawn-pane [-k]   display-panes   list-keys
  choose-tree [-s|-w]  (prefix s / w: pick a session / window with j k g G Enter q)
  pipe-pane [-o] [-t target] [command]   (no command stops the pipe)
Paste buffers:  set-buffer   list-buffers   show-buffer   delete-buffer   choose-buffer
  load-buffer file   save-buffer file   paste-buffer [-b name]
Other:  clock-mode   show-messages   list-clients   list-commands   if-shell   find-window
  set-environment name value   show-environment   refresh-client   select-layout [-n|-p|-E]
  choose-client (prefix D)   display-menu [-T title] name key command ...   (prefix > / <)
  display-popup [-E] [-C] [-w W] [-h H] [-d dir] [command...]   wait-for [-L|-U|-S] channel
  find-text [-C] [-n hits] [-t target] pattern   (search what every pane printed)
  jobs [-t session] [-F format]   (every pane: running or exited, up for how long, idle since when)
  choose-jobs   (prefix B: the same board as a picker; Enter goes there, x kills, r restarts)
  record [-t target] [out.cast]   (write the pane's output as asciinema v2; no path stops)
  notify [-T title] message   (a desktop notification; `set -g notify on` for alerts, with a Go-to-pane button)
  focus-pane %N   (every attached client switches to that pane and comes to the front)
Resume after a reboot (sessions autosave to %LOCALAPPDATA%\\wmux\\sessions):
  resume [name]   list-saved   save-session [-t target|-a]   restore-session [-a] [name]   delete-saved name
  set-cwd [-t target] [dir]   (record the directory a pane resumes in; default: caller's cwd)
  startup on|off|status   (start the server at logon and restore every saved session; no admin needed)
  windows-terminal install|remove|status   (a wmux profile in the Windows Terminal dropdown)
Upgrading:  version (this wmux and the server's)   update [--check]   restart-server (sessions move to this version)
On a phone:  web [--port N] [--bind IP] [--read-only] [--keep-key]   (prints a QR code; scan it on the same network)
History (what panes printed, a file a day, 30 days):  choose-history (prefix /)   view FILE
  list-marks [-t pane]   (the commands a pane ran, with their times; prefix C-t shows them on the lines)
  undo-kill   (prefix u: the pane or window killed in the last 10 seconds comes back; undo-kill-time)
Keys not arriving?  show-keys   (prints each key as the console hands it over and as wmux reads it; q quits)
Plugins / scripting:
  run-shell [-b] command   set-hook -g hook command   show-hooks   load-plugin name   list-plugins
  show-options [-gqv] [name]
Config: %USERPROFILE%\\.wmux.conf (tmux syntax: set -g prefix C-a, bind h select-pane -L, set -g @plugin name);
  with none, ~/.tmux.conf is read and whatever wmux cannot use is skipped.
Any unambiguous prefix of a command name works: `wmux att`, `wmux lsp`, `wmux splitw -h`.
Option names too (`set sync`, `set mon-act on`); an on/off option with no value flips it.
Default prefix: C-b.  Prefix ? lists key bindings, prefix q shows pane numbers.";

fn main() {
    // args() panics on non-UTF-8 (unpaired surrogates in a path); be lossy instead.
    let mut args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    // Inside a pane, talk to the server that owns it (tmux does the same with
    // $TMUX), so `wmux ls` from a script or a plugin means "this server".
    let mut socket = std::env::var("WMUX").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "default".into());
    // Global flags.
    loop {
        match args.first().map(String::as_str) {
            Some("-L") => {
                args.remove(0);
                if args.is_empty() {
                    eprintln!("-L: missing socket name");
                    std::process::exit(1);
                }
                socket = args.remove(0);
            }
            Some(s) if s.starts_with("-L") => {
                socket = args.remove(0)[2..].to_string();
            }
            // These answer locally, so they take nothing else: a typo must not
            // look like it worked.
            Some("-h" | "--help" | "help") => {
                if args.len() > 1 {
                    eprintln!("{}: takes no arguments", args[0]);
                    std::process::exit(1);
                }
                println!("{USAGE}");
                return;
            }
            // tmux's -V: this program's version and nothing else.
            Some("-V" | "--version") => {
                if args.len() > 1 {
                    eprintln!("{}: takes no arguments", args[0]);
                    std::process::exit(1);
                }
                println!("wmux {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            _ => break,
        }
    }
    // `wmux __replay file`: the helper a resumed pane runs first. It prints
    // the pane's saved output into the console it was started in (the
    // pane's ConPTY) and removes the file, so the text is in the console's
    // own buffer before the shell starts.
    if args.first().map(String::as_str) == Some("__replay") {
        let Some(path) = args.get(1) else {
            eprintln!("__replay: file required");
            std::process::exit(2);
        };
        let code = match std::fs::read_to_string(path) {
            Ok(text) => match wmux::console::write_to_console(&text)
                .and_then(|()| wmux::console::write_to_console(wmux::server::pane::REPLAY_MARKER))
            {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("__replay: {e:#}");
                    1
                }
            },
            Err(e) => {
                eprintln!("__replay: {path}: {e}");
                1
            }
        };
        // The console lives as long as its first process: stay until the
        // server has started the pane's program in it, which it signals by
        // removing the file. A server that never does is not waited on
        // forever.
        if code == 0 && std::env::var_os("WMUX_REPLAY_NO_WAIT").is_none() {
            let started = std::time::Instant::now();
            while std::path::Path::new(path).exists() && started.elapsed() < std::time::Duration::from_secs(60) {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
        let _ = std::fs::remove_file(path);
        std::process::exit(code);
    }
    // A notification's "Go to pane" button opens a wmux:// URL, which the
    // protocol registration hands to us as the one argument.
    if let Some(url) = args.first().filter(|a| a.starts_with("wmux://")).cloned() {
        match wmux::notify::parse_go_url(&url) {
            Some((sock, pane)) => {
                socket = sock;
                args = vec!["focus-pane".into(), format!("%{pane}")];
            }
            None => {
                eprintln!("wmux: not a wmux://go/<socket>/<pane> link: {url}");
                std::process::exit(1);
            }
        }
    }
    if args.is_empty() {
        args.push("new-session".into());
    }
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    // Starting at logon is about the server, and the Windows Terminal
    // profile is a file of ours: both are settled without a server.
    let local = match args[0].as_str() {
        "startup" => Some(wmux::startup::run(&socket, &args[1..])),
        "windows-terminal" | "wt" => Some(wmux::wt::run(&socket, &args[1..])),
        "completion" => Some(wmux::completion::run(&args[1..])),
        "version" | "restart-server" | "show-keys" if args.len() > 1 => {
            Some(Err(anyhow::anyhow!("{}: takes no arguments", args[0])))
        }
        "show-keys" => Some(wmux::console::show_keys()),
        "view" => match args.get(1..) {
            Some([file]) => Some(wmux::pager::run(file)),
            _ => Some(Err(anyhow::anyhow!("usage: wmux view FILE"))),
        },
        // These talk to the server, but as a client of their own: the
        // version of this program next to the server's, and moving the
        // sessions to a server of this version.
        "version" => Some(rt.block_on(client::version(&socket))),
        "restart-server" => Some(rt.block_on(client::restart_server(&socket))),
        "update" => Some(rt.block_on(wmux::update::run(&socket, &args[1..]))),
        // The panes on a phone: a client of the server that also answers
        // HTTP on the local network, until Ctrl+C.
        "web" => Some(rt.block_on(wmux::web::run(&socket, &args[1..]))),
        _ => None,
    };
    if let Some(result) = local {
        let code = match result {
            Ok(c) => c,
            Err(e) => {
                eprintln!("wmux: {e:#}");
                1
            }
        };
        std::process::exit(code);
    }
    let code = if args[0] == "__server" {
        logger::init("server");
        // The server has no console; a panic would otherwise vanish.
        std::panic::set_hook(Box::new(|info| {
            log::error!("panic: {info}");
        }));
        let restore = args.iter().skip(1).any(|a| a == "--restore");
        let options = server::RunOptions { force_restore: restore, config: None };
        match rt.block_on(server::run_with(socket, options)) {
            Ok(()) => 0,
            Err(e) => {
                log::error!("{e:#}");
                1
            }
        }
    } else {
        match rt.block_on(run_client(socket, args)) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("wmux: {e:#}");
                1
            }
        }
    };
    // Do not wait for blocked console-input threads.
    std::process::exit(code);
}

async fn run_client(socket: String, args: Vec<String>) -> Result<i32> {
    client::run(socket, args).await
}
