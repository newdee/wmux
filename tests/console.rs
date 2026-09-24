//! Runs the real `wmux.exe` client inside a ConPTY, so the console code path
//! (raw mode, ReadConsoleInputW, WriteConsoleW, alternate screen) is exercised
//! exactly as under Windows Terminal.

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
// NOTE: never drop a ConPTY master on the thread that also drains its output:
// ClosePseudoConsole waits for conhost, which may be blocked writing to us.
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn sessions_dir() -> String {
    std::env::temp_dir().join(format!("wmux-console-sessions-{}", std::process::id())).to_string_lossy().into_owned()
}

/// An empty config file for the servers these tests start, so what they
/// check does not depend on the machine's own `~/.wmux.conf` (a theme there
/// changes the status line they look for). The first config file that
/// exists wins, so this has to be a file, not a path to nothing.
fn empty_config() -> String {
    let p = std::env::temp_dir().join(format!("wmux-console-empty-{}.conf", std::process::id()));
    if !p.exists() {
        std::fs::write(&p, "").unwrap();
    }
    p.to_string_lossy().into_owned()
}

/// The real client binary with the test environment.
fn wmux() -> std::process::Command {
    let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_wmux"));
    c.env("WMUX_SESSIONS_DIR", sessions_dir());
    c.env("WMUX_CONFIG", empty_config());
    c
}

struct Term {
    parser: vt100::Parser,
    rx: mpsc::Receiver<Vec<u8>>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    raw: Vec<u8>,
    /// The `-L` socket the client was started with: its server is stopped
    /// when the test ends, passing or not, so a failed test does not leave
    /// a server holding target\debug\wmux.exe open (which fails the next
    /// build).
    socket: Option<String>,
}

impl Drop for Term {
    fn drop(&mut self) {
        if let Some(s) = &self.socket {
            let _ = wmux().args(["-L", s, "kill-server"]).output();
        }
    }
}

impl Term {
    fn spawn(args: &[&str], cols: u16, rows: u16) -> Term {
        Term::spawn_env(args, cols, rows, &[])
    }

    fn spawn_env(args: &[&str], cols: u16, rows: u16, env: &[(&str, &str)]) -> Term {
        let pty = native_pty_system();
        let pair = pty.openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }).unwrap();
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_wmux"));
        cmd.args(args);
        cmd.env_remove("WMUX_PANE"); // make sure we do not look nested
        cmd.env_remove("WMUX");
        cmd.env("WMUX_SESSIONS_DIR", sessions_dir()); // keep autosave out of the real directory
        cmd.env("WMUX_CONFIG", empty_config()); // and the machine's config out of the test
        for (k, v) in env {
            cmd.env(k, v);
        }
        let child = pair.slave.spawn_command(cmd).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let writer = pair.master.take_writer().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        // Keep the master alive for the life of the test.
        std::mem::forget(pair.master);
        let socket = args.windows(2).find(|w| w[0] == "-L").map(|w| w[1].to_string());
        Term { parser: vt100::Parser::new(rows, cols, 200), rx, writer, child, raw: Vec::new(), socket }
    }

    fn pump(&mut self, d: Duration) {
        let end = Instant::now() + d;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match self.rx.recv_timeout(left) {
                Ok(b) => {
                    self.raw.extend_from_slice(&b);
                    self.parser.process(&b);
                    // ConPTY (INHERIT_CURSOR) asks where the cursor is before
                    // it lets the child run; a real terminal answers, so do we.
                    let n = b.windows(4).filter(|w| w == b"\x1b[6n").count();
                    for _ in 0..n {
                        let (r, c) = self.parser.screen().cursor_position();
                        self.send(&format!("\x1b[{};{}R", r + 1, c + 1));
                    }
                }
                Err(_) => break,
            }
        }
    }

    fn wait_for(&mut self, what: &str, pred: impl Fn(&vt100::Screen) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !pred(self.parser.screen()) {
            assert!(
                Instant::now() < deadline,
                "timeout waiting for {what}\nscreen:\n{}\nraw ({} bytes): {:?}\nchild: {:?}",
                self.parser.screen().contents(),
                self.raw.len(),
                String::from_utf8_lossy(&self.raw[..self.raw.len().min(2000)]),
                self.child.try_wait()
            );
            self.pump(Duration::from_millis(100));
        }
    }

    fn send(&mut self, s: &str) {
        self.writer.write_all(s.as_bytes()).unwrap();
        self.writer.flush().unwrap();
    }

    fn wait_exit(&mut self) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(Some(st)) = self.child.try_wait() {
                self.pump(Duration::from_millis(200));
                return st.exit_code();
            }
            assert!(Instant::now() < deadline, "client did not exit\n{}", self.parser.screen().contents());
            self.pump(Duration::from_millis(100));
        }
    }

    fn row(&self, y: u16) -> String {
        self.parser.screen().rows(0, self.parser.screen().size().1).nth(y as usize).unwrap_or_default()
    }
}

#[test]
fn real_client_in_conpty() {
    let socket = format!("console-{}", std::process::id());
    let mut t = Term::spawn(&["-L", &socket, "new", "-s", "t", "cmd.exe", "/q", "/k", "prompt wmux$g"], 80, 24);

    // Attached: shell prompt in the pane, status line at the bottom.
    t.wait_for("prompt", |s| s.contents().contains("wmux>"));
    t.wait_for("status", |s| s.rows(0, 80).nth(23).unwrap().starts_with("[t] 0:cmd*"));
    assert!(t.parser.screen().alternate_screen(), "client should use the alternate screen");
    // The default right side: the pane's directory and the machine's
    // load, read in-process, and the clock.
    let row = t.row(23);
    assert!(row.contains("CPU ") && row.contains("MEM ") && row.contains(" | "), "machine on the right: {row:?}");

    // Keystrokes travel: pty -> conhost -> ReadConsoleInputW -> server -> pane.
    t.send("echo typed-in-conpty\r");
    t.wait_for("echo", |s| s.contents().matches("typed-in-conpty").count() >= 2);

    // A wmux command run inside a pane talks to the server that owns the pane,
    // without -L: $WMUX names the socket, as $TMUX does for tmux.
    t.send("wmux ls\r");
    t.wait_for("ls from inside the pane", |s| s.contents().contains("t: 1 windows"));

    // Prefix + % splits; a vertical border shows up in the middle.
    t.send("\x02%");
    t.wait_for("split", |s| (0..23).all(|y| s.cell(y, 40).is_some_and(|c| c.contents() == "│")));

    // Resize the console: the status line follows the new height and the
    // pane shells are told the new size.
    // (ResizePseudoConsole is not exposed through the forgotten master, so
    // this is covered by the pipe-level e2e test instead.)

    // A repeatable binding (`bind -r`, the default for pane movement) keeps
    // working without the prefix: one C-b, then two bare h's, walks left twice.
    let out = wmux().args(["-L", &socket, "split-window", "-h", "-t", "t"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let panes = || {
        let out = wmux().args(["-L", &socket, "list-panes", "-t", "t"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let active = || panes().lines().position(|l| l.contains("(active)")).unwrap();
    t.pump(Duration::from_millis(300));
    assert_eq!(panes().lines().count(), 3, "three panes now:\n{}", panes());
    assert_eq!(active(), 2, "the new pane is active");
    t.send("\x02h");
    t.send("h");
    t.pump(Duration::from_millis(300));
    assert_eq!(active(), 0, "the bare h repeated the binding");

    // Prefix + d detaches; the client restores the main screen and prints why.
    t.send("\x02d");
    let code = t.wait_exit();
    assert_eq!(code, 0);
    assert!(!t.parser.screen().alternate_screen(), "main screen should be restored");
    assert!(t.parser.screen().contents().contains("[detached (from session t)]"), "{}", t.parser.screen().contents());
    // Raw stream must contain the alt-screen exit and cursor restore.
    let raw = String::from_utf8_lossy(&t.raw);
    assert!(raw.contains("\x1b[?1049l"), "alternate screen never left");

    // The session survived the detach; kill it through the CLI.
    let out = wmux().args(["-L", &socket, "ls"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("t: 1 windows"), "{stdout}");
    let out = wmux().args(["-L", &socket, "kill-server"]).output().unwrap();
    assert!(out.status.success());
    let _ = std::fs::remove_dir_all(sessions_dir());
}

/// The two answers the binary gives without a server at all.
#[test]
fn version_and_help_answer_locally_and_reject_junk() {
    let out = wmux().arg("-V").output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(text.trim(), format!("wmux {}", env!("CARGO_PKG_VERSION")));

    let out = wmux().arg("-h").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("usage: wmux"));

    // A typo must not look like it worked.
    for args in [vec!["version", "-v"], vec!["help", "me"]] {
        let out = wmux().args(&args).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{args:?} should fail");
        assert!(String::from_utf8_lossy(&out.stderr).contains("takes no arguments"), "{args:?}");
    }
}

/// The picker driven through the real keyboard path: ConPTY -> conhost ->
/// ReadConsoleInputW in the client -> win32 input records -> server.
#[test]
fn choose_tree_through_the_real_keyboard() {
    let socket = format!("choose-{}", std::process::id());
    let mut t = Term::spawn(&["-L", &socket, "new", "-s", "t", "cmd.exe", "/q", "/k", "prompt wmux$g"], 80, 24);
    t.wait_for("prompt", |s| s.contents().contains("wmux>"));
    // Windows 0, 1, 2 with 0 still current (-d), all running the same shell.
    for _ in 0..2 {
        let out = wmux()
            .args(["-L", &socket, "new-window", "-d", "-t", "t", "cmd.exe", "/q", "/k", "prompt wmux$g"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    t.wait_for("three windows", |s| s.rows(0, 80).nth(23).unwrap().contains("2:cmd"));

    // prefix w opens the tree with the cursor on the current window (2 of 4).
    t.send("\x02w");
    t.wait_for("picker", |s| s.contents().contains("[2/4] j/k move"));
    assert!(t.row(0).starts_with("(0) - t: 3 windows (attached)"), "{:?}", t.row(0));
    assert!(t.row(1).starts_with("(1)   - 0: cmd*"), "{:?}", t.row(1));
    // g to the top, j down, k back up, G to the bottom: the hint tracks it.
    t.send("g");
    t.wait_for("g", |s| s.contents().contains("[1/4]"));
    t.send("j");
    t.wait_for("j", |s| s.contents().contains("[2/4]"));
    t.send("k");
    t.wait_for("k", |s| s.contents().contains("[1/4]"));
    t.send("G");
    t.wait_for("G", |s| s.contents().contains("[4/4]"));
    // Enter on the last window makes it current.
    t.send("\r");
    t.wait_for("selected", |s| !s.contents().contains("j/k move") && s.rows(0, 80).nth(23).unwrap().contains("2:cmd*"));
    // The shell never saw any of it.
    let out = wmux().args(["-L", &socket, "capture-pane", "-p", "-t", "t:0"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(text.trim(), "wmux>", "picker keys leaked into the pane: {text:?}");

    t.send("\x02d");
    assert_eq!(t.wait_exit(), 0);
    let out = wmux().args(["-L", &socket, "kill-server"]).output().unwrap();
    assert!(out.status.success());
    let _ = std::fs::remove_dir_all(sessions_dir());
}

#[test]
fn nested_new_is_refused_and_detached_flag_allowed() {
    let socket = format!("nested-{}", std::process::id());
    // Start a server with a session, then run a client pretending to be inside a pane.
    let out = wmux().args(["-L", &socket, "new", "-d", "-s", "outer", "cmd.exe", "/c", "pause"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    // Inside a pane (WMUX_PANE set), `new` without -d is refused like tmux does.
    let mut t = Term::spawn_env(&["-L", &socket, "new", "-s", "inner"], 80, 24, &[("WMUX_PANE", "1")]);
    let code = t.wait_exit();
    assert_eq!(code, 1);
    assert!(
        t.parser.screen().contents().contains("sessions should be nested with care"),
        "{}",
        t.parser.screen().contents()
    );
    // With -d it is allowed.
    let mut t = Term::spawn_env(&["-L", &socket, "new", "-d", "-s", "inner"], 80, 24, &[("WMUX_PANE", "1")]);
    assert_eq!(t.wait_exit(), 0);
    let out = wmux().args(["-L", &socket, "ls"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("outer:") && stdout.contains("inner:"), "{stdout}");
    let _ = wmux().args(["-L", &socket, "kill-server"]).output();
    let _ = std::fs::remove_dir_all(sessions_dir());
}

/// A sessions directory of its own: another test removes the shared one
/// when it ends, and these save and restore across a server restart.
fn own_dir(tag: &str) -> String {
    std::env::temp_dir().join(format!("wmux-console-{tag}-{}", std::process::id())).to_string_lossy().into_owned()
}

/// Stops a test's server when the test ends, failed assertions included:
/// a real server process outlives a panicking test otherwise, and holds
/// target\debug\wmux.exe so the next build cannot replace it.
struct StopServer(String, String);

impl Drop for StopServer {
    fn drop(&mut self) {
        let _ = run_in(&self.0, &self.1, &["kill-server"]);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run_in(dir: &str, socket: &str, args: &[&str]) -> (i32, String, String) {
    let out = wmux().env("WMUX_SESSIONS_DIR", dir).args(["-L", socket]).args(args).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < deadline, "timeout waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `restart-server` with real processes: the sessions that were running
/// come back in a new server (a new process) with their history, a session
/// that was saved and then killed does not, and a client attached in a
/// terminal is attached again by itself, without exiting.
#[test]
fn restart_server_moves_the_sessions_and_the_attached_client_follows() {
    let dir = own_dir("restart");
    let socket = format!("restart-{}", std::process::id());
    let _stop = StopServer(dir.clone(), socket.clone());
    let run = |args: &[&str]| run_in(&dir, &socket, args);
    run(&["new", "-d", "-s", "keep", "cmd.exe", "/q", "/k", "prompt keep$g"]);
    run(&["new", "-d", "-s", "gone"]);
    run(&["save-session", "-a"]);
    run(&["kill-session", "-t", "gone"]); // saved, but not running: must stay gone
    run(&["send-keys", "-t", "keep:0", "echo before-restart", "Enter"]);
    wait_until("the echo", || run(&["capture-pane", "-p", "-t", "keep:0"]).1.matches("before-restart").count() >= 2);
    let pid_before = run(&["display-message", "-p", "-t", "keep", "#{pid}"]).1.trim().to_string();

    let mut t = Term::spawn_env(&["-L", &socket, "attach", "-t", "keep"], 80, 24, &[("WMUX_SESSIONS_DIR", &dir)]);
    t.wait_for("attached", |s| s.rows(0, 80).nth(23).unwrap().starts_with("[keep]"));

    let (code, out, err) = run(&["restart-server"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("1 of 1 session(s) restored: keep"), "{out}");
    let pid_after = run(&["display-message", "-p", "-t", "keep", "#{pid}"]).1.trim().to_string();
    assert!(!pid_after.is_empty() && pid_after != pid_before, "a new server: {pid_before} -> {pid_after}");
    let (_, ls, _) = run(&["ls"]);
    assert!(ls.contains("keep:") && !ls.contains("gone:"), "{ls}");
    assert!(run(&["capture-pane", "-p", "-t", "keep:0"]).1.contains("before-restart"), "history came back");

    // The attached client never exited: it said the server was restarting
    // (so the screen below is a new attach, not the old one still up) and
    // it is on the new server now.
    let notice = b"server restarting: attaching to keep again";
    let deadline = Instant::now() + Duration::from_secs(20);
    while !t.raw.windows(notice.len()).any(|w| w == notice) {
        assert!(Instant::now() < deadline, "no restart notice:\n{}", t.parser.screen().contents());
        t.pump(Duration::from_millis(100));
    }
    t.wait_for("attached again", |s| {
        s.rows(0, 80).nth(23).unwrap().starts_with("[keep]") && s.contents().contains("before-restart")
    });
    // The pane's text was drawn again after the notice cleared the screen.
    let at = t.raw.windows(notice.len()).rposition(|w| w == notice).unwrap();
    let after = &t.raw[at + notice.len()..];
    let redrawn = b"before-restart";
    assert!(after.windows(redrawn.len()).any(|w| w == redrawn), "no redraw after the notice");
    assert!(t.child.try_wait().unwrap().is_none(), "the client is still running");
    t.send("echo after-restart\r");
    wait_until("typing reaches the new server", || {
        run(&["capture-pane", "-p", "-t", "keep:0"]).1.matches("after-restart").count() >= 2
    });
    // A plain kill-server is not a restart: the client leaves, as before.
    run(&["kill-server"]);
    t.wait_exit();
    assert!(t.parser.screen().contents().contains("[server exited"), "{}", t.parser.screen().contents());
    // Nothing to restart: says so.
    let (code, out, _) = run(&["restart-server"]);
    assert_eq!(code, 0);
    assert!(out.contains("nothing to restart"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Attached to a server of another version, the terminal's title says so,
/// and so does the client when it detaches. Needs an older wmux.exe to be
/// the server: set WMUX_OLD_EXE to one (the installed release, say) and run
/// with --ignored.
#[test]
#[ignore]
fn the_title_says_when_the_server_is_another_version() {
    let Some(old) = std::env::var_os("WMUX_OLD_EXE") else { return };
    let dir = own_dir("mismatch");
    let socket = format!("mismatch-{}", std::process::id());
    let old_wmux = || {
        let mut c = std::process::Command::new(&old);
        c.env("WMUX_SESSIONS_DIR", &dir).env("WMUX_CONFIG", empty_config()).args(["-L", &socket]);
        c
    };
    let out = old_wmux().args(["new", "-d", "-s", "m"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v = String::from_utf8_lossy(&std::process::Command::new(&old).arg("-V").output().unwrap().stdout)
        .trim()
        .trim_start_matches("wmux ")
        .to_string();
    assert_ne!(v, env!("CARGO_PKG_VERSION"), "WMUX_OLD_EXE must be another version");
    let mut t = Term::spawn_env(&["-L", &socket, "attach", "-t", "m"], 80, 24, &[("WMUX_SESSIONS_DIR", &dir)]);
    t.wait_for("attached", |s| s.rows(0, 80).nth(23).unwrap().starts_with("[m]"));
    let want = format!("wmux: m [server {v}: run wmux restart-server]");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !String::from_utf8_lossy(&t.raw).contains(&want) {
        assert!(Instant::now() < deadline, "no title {want:?} in {:?}", String::from_utf8_lossy(&t.raw));
        t.pump(Duration::from_millis(100));
    }
    let _ = old_wmux().arg("kill-server").output();
    t.wait_exit();
    let text = String::from_utf8_lossy(&t.raw);
    assert!(text.contains("restart-server` moves your sessions"), "{}", t.parser.screen().contents());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Run from inside a pane of the server it restarts, `restart-server`
/// would die with that pane halfway; it goes on outside the pane instead
/// and leaves its result in restart.log.
#[test]
fn restart_server_from_inside_a_pane_finishes_outside_it() {
    let dir = own_dir("restart-in-pane");
    let socket = format!("restartin-{}", std::process::id());
    let _stop = StopServer(dir.clone(), socket.clone());
    let run = |args: &[&str]| run_in(&dir, &socket, args);
    run(&["new", "-d", "-s", "inner", "cmd.exe", "/q", "/k", "prompt in$g"]);
    wait_until("prompt", || run(&["capture-pane", "-p", "-t", "inner:0"]).1.contains("in>"));
    let pid_before = run(&["display-message", "-p", "-t", "inner", "#{pid}"]).1.trim().to_string();
    // The pane's PATH starts with this wmux.exe's directory.
    run(&["send-keys", "-t", "inner:0", "wmux restart-server", "Enter"]);
    let log = std::path::Path::new(&dir).join("restart.log");
    wait_until("restart.log", || std::fs::read_to_string(&log).is_ok_and(|t| t.contains("restored")));
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("1 of 1 session(s) restored: inner"), "{text}");
    let pid_after = run(&["display-message", "-p", "-t", "inner", "#{pid}"]).1.trim().to_string();
    assert!(pid_after != pid_before, "a new server: {pid_before} -> {pid_after}");
    run(&["kill-server"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `wmux show-keys` through the real console path: the prefix shows as
/// C-b with the record it came in, and q ends it.
#[test]
fn show_keys_reports_the_prefix_and_quits_on_q() {
    let mut t = Term::spawn(&["show-keys"], 80, 24);
    t.wait_for("banner", |s| s.contents().contains("q quits"));
    t.send("\x02");
    t.wait_for("the prefix", |s| s.contents().contains("->  C-b"));
    t.send("q");
    assert_eq!(t.wait_exit(), 0);
    // The alternate screen is gone; what was pressed stays, to copy.
    let s = t.parser.screen();
    assert!(!s.alternate_screen(), "back on the main screen");
    assert!(s.contents().contains("->  C-b") && s.contents().contains("->  q"), "{}", s.contents());
}
