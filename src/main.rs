//! wmux: a tmux-like terminal multiplexer for Windows (ConPTY, PowerShell, WSL).

use anyhow::Result;
use wmux::{client, logger, server};

const USAGE: &str = "\
usage: wmux [-L socket-name] [command [flags]]

Sessions:
  new-session   (new)     [-s name] [-n window] [-c dir] [-d] [command...]
  attach-session (attach) [-t target] [-d]
  list-sessions (ls)
  kill-session  [-t target]      kill-server      has-session -t target
  rename-session [-t target] name
Windows / panes (from inside a session, or with -t):
  new-window (neww) [-n name] [-c dir] [command...]   kill-window   rename-window name
  select-window -t N   next-window   previous-window   last-window   list-windows
  split-window (splitw) [-h|-v] [-c dir] [command...]  kill-pane   select-pane -L|-R|-U|-D
  resize-pane -L|-R|-U|-D [n] | -Z   swap-pane -U|-D   break-pane   list-panes
  send-keys [-t target] keys...   copy-mode   paste-buffer   list-keys
Config: %USERPROFILE%\\.wmux.conf (tmux syntax: set -g prefix C-a, bind h select-pane -L, ...)
Default prefix: C-b.  Prefix ? lists key bindings.";

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut socket = "default".to_string();
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
            Some("-h" | "--help" | "help") => {
                println!("{USAGE}");
                return;
            }
            Some("-V" | "--version" | "version") => {
                println!("wmux {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            _ => break,
        }
    }
    if args.is_empty() {
        args.push("new-session".into());
    }
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    let code = if args[0] == "__server" {
        logger::init("server");
        match rt.block_on(server::run(socket)) {
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
