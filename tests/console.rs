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

/// The real client binary with the test environment.
fn wmux() -> std::process::Command {
    let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_wmux"));
    c.env("WMUX_SESSIONS_DIR", sessions_dir());
    c
}

struct Term {
    parser: vt100::Parser,
    rx: mpsc::Receiver<Vec<u8>>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    raw: Vec<u8>,
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
        Term { parser: vt100::Parser::new(rows, cols, 200), rx, writer, child, raw: Vec::new() }
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
    // cmd.exe sets its window title via OSC; it lands on the right of the status line.
    assert!(t.row(23).contains("cmd.exe\""), "pane title on the right: {:?}", t.row(23));

    // Keystrokes travel: pty -> conhost -> ReadConsoleInputW -> server -> pane.
    t.send("echo typed-in-conpty\r");
    t.wait_for("echo", |s| s.contents().matches("typed-in-conpty").count() >= 2);

    // Prefix + % splits; a vertical border shows up in the middle.
    t.send("\x02%");
    t.wait_for("split", |s| (0..23).all(|y| s.cell(y, 40).is_some_and(|c| c.contents() == "│")));

    // Resize the console: the status line follows the new height and the
    // pane shells are told the new size.
    // (ResizePseudoConsole is not exposed through the forgotten master, so
    // this is covered by the pipe-level e2e test instead.)

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
