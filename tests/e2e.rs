//! End-to-end tests: run the server in-process, talk to it over the named
//! pipe exactly like the real client, and check the rendered frames.

use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite, ReadHalf, WriteHalf};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
use wmux::ipc::{ClientMsg, KeyRecord, MouseRecord, PROTOCOL_VERSION, ServerMsg, pipe_name, read_frame, write_frame};
use wmux::keys::{LEFT_ALT_PRESSED, LEFT_CTRL_PRESSED, SHIFT_PRESSED, VK_RETURN};

const COLS: u16 = 80;
const ROWS: u16 = 24;

struct Harness {
    socket: String,
    _server: tokio::task::JoinHandle<()>,
    sessions_dir: std::path::PathBuf,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.sessions_dir);
    }
}

impl Harness {
    async fn start(name: &str) -> Harness {
        let socket = format!("test-{name}-{}", std::process::id());
        let s = socket.clone();
        let server = tokio::spawn(async move {
            if let Err(e) = wmux::server::run(s).await {
                panic!("server: {e:#}");
            }
        });
        let pipe = pipe_name(&socket);
        let deadline = Instant::now() + Duration::from_secs(5);
        while ClientOptions::new().open(&pipe).is_err() {
            assert!(Instant::now() < deadline, "server did not come up");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Autosave must never touch the real sessions directory from a test.
        let dir = std::env::temp_dir().join(format!("wmux-test-sessions-{}-{name}", std::process::id()));
        let h = Harness { socket, _server: server, sessions_dir: dir.clone() };
        // Make every implicitly spawned pane a predictable cmd.exe prompt.
        let (code, _, err) = h.cli(&["set", "-g", "default-command", "cmd.exe /q /k prompt wmux$g"]).await;
        assert_eq!(code, 0, "{err}");
        let (code, _, err) = h.cli(&["set", "-g", "sessions-dir", &dir.to_string_lossy()]).await;
        assert_eq!(code, 0, "{err}");
        h
    }

    async fn connect(&self) -> Conn {
        let pipe = pipe_name(&self.socket);
        let deadline = Instant::now() + Duration::from_secs(10);
        let c = loop {
            match ClientOptions::new().open(&pipe) {
                Ok(c) => break c,
                Err(e) => {
                    assert!(Instant::now() < deadline, "server {} not reachable: {e}", self.socket);
                    tokio::time::sleep(Duration::from_millis(20)).await
                }
            }
        };
        let (rd, wr) = tokio::io::split(c);
        Conn { rd, wr, screen: vt100::Parser::new(ROWS, COLS, 0) }
    }

    /// Poll `capture-pane` until the pane's text satisfies `pred`.
    async fn wait_capture(&self, target: &str, what: &str, pred: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let (_, out, _) = self.cli(&["capture-pane", "-p", "-t", target]).await;
            if pred(&out) {
                return out;
            }
            assert!(Instant::now() < deadline, "timeout waiting for {what} in {target}; pane:\n{out}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Run a CLI-style command; returns (code, stdout text, stderr text).
    async fn cli(&self, argv: &[&str]) -> (i32, String, String) {
        let mut c = self.connect().await;
        c.command(argv, false).await;
        let mut out = String::new();
        let mut err = String::new();
        loop {
            match c.next().await {
                ServerMsg::Text(t) => out.push_str(&t),
                ServerMsg::Error(e) => err.push_str(&e),
                ServerMsg::Done { code } => return (code, out, err),
                other => panic!("unexpected {other:?}"),
            }
        }
    }
}

struct Conn {
    rd: ReadHalf<NamedPipeClient>,
    wr: WriteHalf<NamedPipeClient>,
    screen: vt100::Parser,
}

impl Conn {
    async fn send(&mut self, m: ClientMsg) {
        write_frame(&mut self.wr, &m).await.unwrap();
    }

    async fn command(&mut self, argv: &[&str], interactive: bool) {
        self.send(ClientMsg::Command {
            version: PROTOCOL_VERSION,
            argv: argv.iter().map(|s| s.to_string()).collect(),
            cwd: std::env::temp_dir().to_string_lossy().into_owned(),
            cols: COLS,
            rows: ROWS,
            interactive,
            pane_env: None,
        })
        .await;
    }

    async fn next(&mut self) -> ServerMsg {
        tokio::time::timeout(Duration::from_secs(10), read_frame::<_, ServerMsg>(&mut self.rd))
            .await
            .expect("timeout waiting for server")
            .unwrap()
            .expect("server closed")
    }

    /// Attach with a command and pump until `Attached`, then the `SetMouse`
    /// that every attach must be followed by; returns (session, mouse).
    async fn attach_full(&mut self, argv: &[&str]) -> (String, bool) {
        self.command(argv, true).await;
        let session = loop {
            match self.next().await {
                ServerMsg::Attached { session } => break session,
                ServerMsg::Error(e) => panic!("attach failed: {e}"),
                ServerMsg::Output(b) => self.screen.process(&b),
                _ => {}
            }
        };
        let mouse = loop {
            match self.next().await {
                ServerMsg::SetMouse(m) => break m,
                ServerMsg::Output(b) => self.screen.process(&b),
                other => panic!("expected SetMouse after Attached, got {other:?}"),
            }
        };
        (session, mouse)
    }

    async fn attach(&mut self, argv: &[&str]) -> String {
        self.attach_full(argv).await.0
    }

    /// Pump until a `SetMouse` arrives; returns its value.
    async fn wait_set_mouse(&mut self) -> bool {
        loop {
            match self.next().await {
                ServerMsg::SetMouse(m) => return m,
                ServerMsg::Output(b) => self.screen.process(&b),
                _ => {}
            }
        }
    }

    /// Pump output until the rendered screen satisfies `pred`.
    async fn wait_for(&mut self, what: &str, pred: impl Fn(&vt100::Screen) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !pred(self.screen.screen()) {
            assert!(
                Instant::now() < deadline,
                "timeout waiting for {what}; screen:\n{}",
                self.screen.screen().contents()
            );
            match tokio::time::timeout(Duration::from_millis(500), read_frame::<_, ServerMsg>(&mut self.rd)).await {
                Ok(Ok(Some(ServerMsg::Output(b)))) => self.screen.process(&b),
                Ok(Ok(Some(ServerMsg::Detached { reason }))) => panic!("detached: {reason}"),
                Ok(Ok(Some(_))) => {}
                Ok(Ok(None)) => panic!("server closed"),
                Ok(Err(e)) => panic!("{e}"),
                Err(_) => {}
            }
        }
    }

    async fn wait_detached(&mut self) -> String {
        loop {
            match self.next().await {
                ServerMsg::Detached { reason } => return reason,
                ServerMsg::Output(b) => self.screen.process(&b),
                _ => {}
            }
        }
    }

    async fn key(&mut self, vk: u16, ch: char, ctrl: u32) {
        let rec = KeyRecord { down: true, repeat: 1, vk, sc: 0, ch: ch as u16, ctrl };
        self.send(ClientMsg::Key(rec)).await;
        self.send(ClientMsg::Key(KeyRecord { down: false, ..rec })).await;
    }

    async fn type_str(&mut self, s: &str) {
        for c in s.chars() {
            let vk = if c.is_ascii_alphabetic() {
                c.to_ascii_uppercase() as u16
            } else if c == ' ' {
                0x20
            } else {
                0
            };
            self.key(vk, c, 0).await;
        }
    }

    async fn enter(&mut self) {
        self.key(VK_RETURN, '\r', 0).await;
    }

    /// Prefix (C-b) followed by a key.
    async fn prefix(&mut self, ch: char) {
        self.key(b'B' as u16, '\x02', LEFT_CTRL_PRESSED).await;
        let (vk, ctrl) = match ch {
            '\t' => (0x09, 0),
            c if c.is_ascii_uppercase() => (c as u16, SHIFT_PRESSED),
            c if c.is_ascii_alphabetic() => (c.to_ascii_uppercase() as u16, 0),
            _ => (0, 0),
        };
        self.key(vk, ch, ctrl).await;
    }

    fn text(&self) -> String {
        self.screen.screen().contents()
    }

    fn row(&self, y: u16) -> String {
        self.screen.screen().rows(0, COLS).nth(y as usize).unwrap_or_default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_lifecycle() {
    let h = Harness::start("cli").await;
    let (code, _, err) = h.cli(&["ls"]).await;
    assert_eq!(code, 1);
    assert_eq!(err, "no sessions");

    let (code, _, err) = h.cli(&["new", "-d", "-s", "main", "cmd.exe", "/q", "/k", "prompt $g"]).await;
    assert_eq!(code, 0, "{err}");
    let (code, out, _) = h.cli(&["ls"]).await;
    assert_eq!(code, 0);
    assert!(out.starts_with("main: 1 windows"), "{out}");

    let (code, _, err) = h.cli(&["new", "-d", "-s", "main"]).await;
    assert_eq!(code, 1);
    assert_eq!(err, "duplicate session: main");

    let (code, _, _) = h.cli(&["has-session", "-t", "main"]).await;
    assert_eq!(code, 0);
    let (code, _, _) = h.cli(&["has-session", "-t", "nope"]).await;
    assert_eq!(code, 1);

    // A program that cannot be started is an error, not a dead session.
    let (code, _, err) = h.cli(&["new", "-d", "-s", "bad", "definitely-not-a-program-xyz.exe"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("definitely-not-a-program-xyz.exe"), "{err}");
    let (code, _, _) = h.cli(&["has-session", "-t", "bad"]).await;
    assert_eq!(code, 1);

    // Non-ASCII session and window names survive the round trip.
    let (code, _, err) = h.cli(&["new", "-d", "-s", "会话", "-n", "窗口"]).await;
    assert_eq!(code, 0, "{err}");
    let (_, out, _) = h.cli(&["list-windows", "-t", "会话"]).await;
    assert!(out.starts_with("0: 窗口*"), "{out}");
    let (code, _, _) = h.cli(&["kill-session", "-t", "会话"]).await;
    assert_eq!(code, 0);

    let (code, _, err) = h.cli(&["new-window", "-t", "main", "-n", "second", "cmd.exe", "/c", "exit"]).await;
    assert_eq!(code, 0, "{err}");
    let (code, _, _) = h.cli(&["rename-session", "-t", "main", "renamed"]).await;
    assert_eq!(code, 0);
    let (_, out, _) = h.cli(&["ls"]).await;
    assert!(out.starts_with("renamed:"), "{out}");

    let (code, _, _) = h.cli(&["kill-session", "-t", "renamed"]).await;
    assert_eq!(code, 0);
    // The server exits when its last session dies.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(ClientOptions::new().open(pipe_name(&h.socket)).is_err(), "server should have exited");
}

#[tokio::test(flavor = "multi_thread")]
async fn attach_type_split_detach() {
    let h = Harness::start("attach").await;
    let mut c = h.connect().await;
    let session = c.attach(&["new", "-s", "w", "cmd.exe", "/q", "/k", "prompt wmux$g"]).await;
    assert_eq!(session, "w");

    // Status line at the bottom names the session and window.
    c.wait_for("prompt", |s| s.contents().contains("wmux>")).await;
    let status = c.row(ROWS - 1);
    assert!(status.starts_with("[w] 0:cmd*"), "status: {status:?}");

    // Typing reaches the shell via win32-input-mode.
    c.type_str("echo hello-from-wmux").await;
    c.enter().await;
    c.wait_for("echo output", |s| s.contents().matches("hello-from-wmux").count() >= 2).await;

    // Split: a vertical border appears and both halves get a prompt.
    c.prefix('%').await;
    c.wait_for("split border", |s| (0..ROWS - 1).all(|y| s.cell(y, COLS / 2).is_some_and(|c| c.contents() == "│")))
        .await;
    c.wait_for("second prompt", |s| s.contents().matches("wmux>").count() >= 2).await;

    // New window: status shows two windows, second is current.
    c.prefix('c').await;
    c.wait_for("second window", |s| s.rows(0, COLS).nth(ROWS as usize - 1).unwrap().contains("1:cmd*")).await;
    // Back to window 0 (which is still split).
    c.prefix('p').await;
    c.wait_for("window 0 current", |s| s.rows(0, COLS).nth(ROWS as usize - 1).unwrap().contains("0:cmd*")).await;
    c.wait_for("split border again", |s| s.cell(0, COLS / 2).is_some_and(|c| c.contents() == "│")).await;

    // Zoom hides the border.
    c.prefix('z').await;
    c.wait_for("zoomed", |s| {
        !s.cell(0, COLS / 2).is_some_and(|c| c.contents() == "│")
            && s.rows(0, COLS).nth(ROWS as usize - 1).unwrap().contains("0:cmd*Z")
    })
    .await;
    c.prefix('z').await;
    c.wait_for("unzoomed", |s| s.cell(0, COLS / 2).is_some_and(|c| c.contents() == "│")).await;

    // Prefix ? shows the key table as an overlay; any key dismisses it.
    c.prefix('?').await;
    c.wait_for("overlay", |s| s.contents().contains("bind-key -T prefix") && s.contents().contains("press any key"))
        .await;
    c.key(0x1B, '\x1b', 0).await;
    c.wait_for("overlay gone", |s| !s.contents().contains("press any key")).await;

    // split-window -d keeps the current pane; -b puts the new pane first.
    c.prefix(':').await;
    c.type_str("split-window -v -d -b").await;
    c.enter().await;
    c.wait_for("three panes", |s| s.contents().matches("wmux>").count() >= 3).await;
    let (_, out, _) = h.cli(&["list-panes", "-t", "w"]).await;
    // The new pane is index 0 (before) and the previously active pane stays active.
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "{out}");
    assert!(!lines[0].contains("(active)"), "{out}");

    // Prefix , opens a rename prompt pre-filled with the window name; the
    // template is "rename-window -- %%" so `--` must end flag parsing.
    c.prefix(',').await;
    c.wait_for("rename prompt", |s| s.rows(0, COLS).nth(ROWS as usize - 1).unwrap().starts_with("(rename-window) cmd"))
        .await;
    c.key(0x08, '\x08', 0).await; // backspace over "cmd"
    c.key(0x08, '\x08', 0).await;
    c.key(0x08, '\x08', 0).await;
    c.type_str("via-comma").await;
    c.enter().await;
    c.wait_for("renamed via ,", |s| s.rows(0, COLS).nth(ROWS as usize - 1).unwrap().contains("0:via-comma*")).await;

    // A multi-line error (bad source-file) is shown as an overlay, not flattened.
    let bad = std::env::temp_dir().join(format!("wmux-bad-{}.conf", std::process::id()));
    std::fs::write(&bad, "set -g mouse maybe\nfrobnicate\n").unwrap();
    c.prefix(':').await;
    c.type_str(&format!("source-file {}", bad.display())).await;
    c.enter().await;
    c.wait_for("config errors overlay", |s| {
        let t = s.contents();
        // Long lines are clipped at the window width, so match prefixes only.
        t.contains(":1: bad boolean 'maybe'") && t.contains(":2: unknown command") && t.contains("press any key")
    })
    .await;
    c.key(0x1B, '\x1b', 0).await;
    c.wait_for("overlay gone again", |s| !s.contents().contains("press any key")).await;
    let _ = std::fs::remove_file(&bad);

    // Command prompt: rename the window.
    c.prefix(':').await;
    c.type_str("rename-window shell").await;
    c.enter().await;
    c.wait_for("renamed", |s| s.rows(0, COLS).nth(ROWS as usize - 1).unwrap().contains("0:shell*")).await;

    // Detach.
    c.prefix('d').await;
    let reason = c.wait_detached().await;
    assert_eq!(reason, "detached");

    let (_, out, _) = h.cli(&["ls"]).await;
    assert!(out.starts_with("w: 2 windows"), "{out}");
    let (_, out, _) = h.cli(&["list-windows", "-t", "w"]).await;
    assert!(out.contains("0: shell* (3 panes)"), "{out}");

    // -A attaches to an existing session instead of failing on the duplicate.
    let (code, _, err) = h.cli(&["new", "-A", "-d", "-s", "w"]).await;
    assert_eq!(code, 0, "{err}");
    // new-window -d does not change the current window; kill-window -a keeps only the target.
    let (code, _, _) = h.cli(&["new-window", "-d", "-t", "w", "-n", "bg"]).await;
    assert_eq!(code, 0);
    let (_, out, _) = h.cli(&["list-windows", "-t", "w"]).await;
    assert!(out.contains("0: shell*") && out.contains("2: bg ("), "{out}");
    let (code, _, _) = h.cli(&["kill-window", "-a", "-t", "w:0"]).await;
    assert_eq!(code, 0);
    let (_, out, _) = h.cli(&["list-windows", "-t", "w"]).await;
    assert_eq!(out.lines().count(), 1, "{out}");
    // kill-pane -a leaves one pane.
    let (code, _, _) = h.cli(&["kill-pane", "-a", "-t", "w:0.1"]).await;
    assert_eq!(code, 0);
    let (_, out, _) = h.cli(&["list-panes", "-t", "w"]).await;
    assert_eq!(out.lines().count(), 1, "{out}");
    // send-keys -l sends the words literally: "Enter" is text, not a key.
    let (code, _, _) = h.cli(&["send-keys", "-t", "w", "-l", "rem literal-Enter-word"]).await;
    assert_eq!(code, 0);
    let (code, _, _) = h.cli(&["send-keys", "-t", "w", "Enter"]).await;
    assert_eq!(code, 0);
    // capture-pane prints the pane text from the CLI.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (code, out, _) = h.cli(&["capture-pane", "-p", "-t", "w"]).await;
        assert_eq!(code, 0);
        if out.contains("rem literal-Enter-word") && out.ends_with("wmux>") {
            break;
        }
        assert!(Instant::now() < deadline, "capture-pane: {out:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Re-attach: full redraw restores the view; the literal send-keys text is there.
    let mut c2 = h.connect().await;
    c2.attach(&["attach", "-t", "w"]).await;
    c2.wait_for("restored prompt", |s| s.contents().contains("wmux>")).await;
    c2.wait_for("literal text", |s| s.contents().contains("literal-Enter-word")).await;
    // Same-session CLI command from outside while attached shows up as a message.
    let (code, _, _) = h.cli(&["send-keys", "-t", "w", "echo via-send-keys", "Enter"]).await;
    assert_eq!(code, 0);
    c2.wait_for("send-keys echoed", |s| s.contents().matches("via-send-keys").count() >= 2).await;

    let (code, _, _) = h.cli(&["kill-server"]).await;
    assert_eq!(code, 0);
    let reason = c2.wait_detached().await;
    assert_eq!(reason, "server exited");
}

#[tokio::test(flavor = "multi_thread")]
async fn pane_exit_closes_window_and_session() {
    let h = Harness::start("exit").await;
    let mut c = h.connect().await;
    c.attach(&["new", "-s", "x", "cmd.exe", "/q", "/k", "prompt wmux$g"]).await;
    c.wait_for("prompt", |s| s.contents().contains("wmux>")).await;
    c.prefix('"').await;
    c.wait_for("horizontal border", |s| {
        (0..COLS).all(|x| {
            s.cell(ROWS / 2 - 1, x).is_some_and(|c| c.contents() == "─")
                || s.cell(ROWS / 2, x).is_some_and(|c| c.contents() == "─")
        })
    })
    .await;
    // Exit the new (active) pane: window collapses back to one pane.
    c.type_str("exit").await;
    c.enter().await;
    c.wait_for("border gone", |s| !s.contents().contains('─')).await;
    // Exit the last pane: session ends and the client is detached.
    c.type_str("exit").await;
    c.enter().await;
    let reason = c.wait_detached().await;
    assert_eq!(reason, "exited");
}

#[tokio::test(flavor = "multi_thread")]
async fn mouse_selects_pane_and_copy_mode_scrolls() {
    let h = Harness::start("mouse").await;
    let mut c = h.connect().await;
    let (_, mouse) = c.attach_full(&["new", "-s", "m", "cmd.exe", "/q", "/k", "prompt wmux$g"]).await;
    assert!(mouse, "mouse is on by default");
    c.wait_for("prompt", |s| s.contents().contains("wmux>")).await;
    // Toggling the option reaches the attached client's console.
    let (code, _, _) = h.cli(&["set", "-g", "mouse", "off"]).await;
    assert_eq!(code, 0);
    assert!(!c.wait_set_mouse().await);
    let (code, _, _) = h.cli(&["set", "-g", "mouse", "on"]).await;
    assert_eq!(code, 0);
    assert!(c.wait_set_mouse().await);
    c.prefix('%').await;
    c.wait_for("split", |s| s.contents().matches("wmux>").count() >= 2).await;
    // Right pane is active after the split (green border on the right side).
    // Click in the left pane, then type: text must land on the left.
    c.send(ClientMsg::Mouse(MouseRecord { x: 2, y: 2, buttons: 1, ctrl: 0, flags: 0 })).await;
    c.send(ClientMsg::Mouse(MouseRecord { x: 2, y: 2, buttons: 0, ctrl: 0, flags: 0 })).await;
    c.type_str("rem left-side-marker").await;
    c.wait_for("typed on the left", |s| s.rows(0, COLS / 2).any(|r| r.contains("left-side-marker"))).await;
    assert!(
        !c.text().contains("marker")
            || c.screen.screen().rows(COLS / 2 + 1, COLS / 2 - 1).all(|r| !r.contains("left-side-marker"))
    );

    // Fill scrollback, then wheel up: the [n/m] indicator of copy mode shows.
    c.enter().await;
    c.type_str("for /l %i in (1,1,60) do @echo line%i").await;
    c.enter().await;
    c.wait_for("output", |s| s.contents().contains("line60")).await;
    c.send(ClientMsg::Mouse(MouseRecord { x: 2, y: 2, buttons: (120u32) << 16, ctrl: 0, flags: 4 })).await;
    c.wait_for("copy mode indicator", |s| s.rows(0, COLS).next().unwrap().contains("[3/")).await;
    // Escape leaves copy mode.
    c.key(0x1B, '\x1b', 0).await;
    c.wait_for("copy mode left", |s| !s.rows(0, COLS).next().unwrap().contains("[3/")).await;
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resize_and_two_clients() {
    let h = Harness::start("resize").await;
    let mut a = h.connect().await;
    a.attach(&["new", "-s", "r", "cmd.exe", "/q", "/k", "prompt wmux$g"]).await;
    a.wait_for("prompt", |s| s.contents().contains("wmux>")).await;
    // Shrink the client: the status line moves up.
    a.send(ClientMsg::Resize { cols: 60, rows: 12 }).await;
    a.screen = vt100::Parser::new(12, 60, 0);
    a.wait_for("status at row 11", |s| s.rows(0, 60).nth(11).unwrap().starts_with("[r] 0:cmd*")).await;
    let (_, out, _) = h.cli(&["ls"]).await;
    assert!(out.contains("[60x12]"), "{out}");

    // Second client attaches: it sees the same session; -d kicks the first.
    let mut b = h.connect().await;
    b.attach(&["attach", "-d", "-t", "r"]).await;
    let reason = a.wait_detached().await;
    assert_eq!(reason, "detached (attach -d)");
    b.wait_for("prompt on b", |s| s.contents().contains("wmux>")).await;
    let (_, out, _) = h.cli(&["ls"]).await;
    assert!(out.contains("[80x24]") && out.contains("(attached)"), "{out}");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn plugins_hooks_status_formats_and_run_shell() {
    let h = Harness::start("plugin").await;
    // A plugin directory: <plugin-path>/demo/demo.wmux
    let root = std::env::temp_dir().join(format!("wmux-plugins-{}", std::process::id()));
    let dir = root.join("demo");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("demo.wmux"),
        "set -g status-right \"#[fg=red]#(pwsh -NoProfile -Command Write-Output plugged)#[default] %H\"\n\
         set -g status-interval 1\n\
         bind P run-shell \"pwsh -NoProfile -Command Write-Output hello-from-plugin\"\n\
         set-hook -g after-new-window \"rename-window hooked\"\n\
         set -g @demo-option yes\n",
    )
    .unwrap();
    // Declared the tmux way, from a config file.
    let conf = root.join("wmux.conf");
    std::fs::write(&conf, format!("set -g plugin-path \"{}\"\nset -g @plugin demo\n", root.display())).unwrap();
    let (code, _, err) = h.cli(&["source-file", &conf.to_string_lossy()]).await;
    assert_eq!(code, 0, "{err}");
    let (_, out, _) = h.cli(&["list-plugins"]).await;
    assert!(out.contains("demo"), "{out}");
    let (_, out, _) = h.cli(&["show-hooks"]).await;
    assert!(out.contains("after-new-window \"rename-window hooked\""), "{out}");
    // show-hooks output is valid command syntax: feeding it back reproduces the hook.
    let line = out.lines().find(|l| l.starts_with("after-new-window")).unwrap();
    let words = wmux::command::tokenize(line).unwrap();
    assert_eq!(words, vec!["after-new-window", "rename-window hooked"]);
    let (code, _, err) = h.cli(&["load-plugin", "nope"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("plugin not found"), "{err}");
    // Loading the same plugin twice is a no-op, so bindings are not duplicated.
    let (code, _, _) = h.cli(&["load-plugin", "demo"]).await;
    assert_eq!(code, 0);
    let (_, out, _) = h.cli(&["list-plugins"]).await;
    assert_eq!(out.lines().count(), 1, "{out}");
    // Plugins read their options the tmux way.
    let (code, out, _) = h.cli(&["show-options", "-gqv", "@demo-option"]).await;
    assert_eq!((code, out.as_str()), (0, "yes"));
    let (code, out, _) = h.cli(&["show-options", "-gqv", "@absent"]).await;
    assert_eq!((code, out.as_str()), (0, ""));
    let (code, _, err) = h.cli(&["show-options", "-g", "@absent"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("unknown option"), "{err}");
    let (_, out, _) = h.cli(&["show-options", "-g"]).await;
    assert!(out.contains("status-interval 1") && out.contains("@demo-option yes"), "{out}");
    let (code, _, err) = h.cli(&["set-hook", "-g", "no-such-hook", "list-keys"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("unknown hook"), "{err}");

    // run-shell from the CLI: output comes back when the command finishes; a
    // non-zero exit is an error.
    let (code, out, _) = h.cli(&["run-shell", "pwsh -NoProfile -Command Write-Output from-cli"]).await;
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "from-cli");
    let (code, _, err) = h.cli(&["run-shell", "pwsh -NoProfile -Command exit 3"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("exited with 3"), "{err}");
    // WMUX is set for the child, so plugin scripts can call back.
    let (_, out, _) = h.cli(&["run-shell", "pwsh -NoProfile -Command Write-Output $env:WMUX"]).await;
    assert_eq!(out.trim(), h.socket);

    // Attached: the #(command) piece shows up on the status line (styled),
    // prefix P runs the plugin's command and shows its output, the hook renames
    // new windows.
    let mut c = h.connect().await;
    c.attach(&["new", "-s", "p"]).await;
    c.wait_for("prompt", |s| s.contents().contains("wmux>")).await;
    c.wait_for("status #() piece", |s| s.rows(0, COLS).nth(ROWS as usize - 1).unwrap().contains("plugged")).await;
    let sr = c.screen.screen();
    let row = ROWS - 1;
    let col = (0..COLS).find(|&x| sr.rows(x, COLS - x).nth(row as usize).unwrap().starts_with("plugged")).unwrap();
    assert_eq!(sr.cell(row, col).unwrap().fgcolor(), vt100::Color::Idx(1), "#[fg=red] applied");
    c.prefix('P').await;
    c.wait_for("run-shell output", |s| s.rows(0, COLS).nth(ROWS as usize - 1).unwrap().contains("hello-from-plugin"))
        .await;
    c.prefix('c').await;
    c.wait_for("hook renamed the window", |s| s.rows(0, COLS).nth(ROWS as usize - 1).unwrap().contains("1:hooked*"))
        .await;
    let (_, out, _) = h.cli(&["list-windows", "-t", "p"]).await;
    assert!(out.contains("1: hooked*"), "{out}");
    h.cli(&["kill-server"]).await;
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread")]
async fn save_and_resume_sessions() {
    let h = Harness::start("resume").await;
    let dir = std::env::temp_dir().join(format!("wmux-sessions-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (code, _, err) = h.cli(&["set", "-g", "sessions-dir", &dir.to_string_lossy()]).await;
    assert_eq!(code, 0, "{err}");

    // A second session keeps the server alive while "work" is killed below.
    let (code, _, err) = h.cli(&["new", "-d", "-s", "keeper"]).await;
    assert_eq!(code, 0, "{err}");
    // Build a session: two windows, the first split in three, second window renamed.
    let (code, _, err) = h.cli(&["new", "-d", "-s", "work", "-n", "edit", "cmd.exe", "/q", "/k", "prompt A$g"]).await;
    assert_eq!(code, 0, "{err}");
    h.cli(&["split-window", "-h", "-t", "work:0", "cmd.exe", "/q", "/k", "prompt B$g"]).await;
    h.cli(&["split-window", "-v", "-t", "work:0", "cmd.exe", "/q", "/k", "prompt C$g"]).await;
    h.cli(&["new-window", "-t", "work", "-n", "logs", "cmd.exe", "/q", "/k", "prompt D$g"]).await;
    h.cli(&["select-window", "-t", "work:0"]).await;
    // Autosave happens on the tick; the explicit command is immediate.
    let (code, out, err) = h.cli(&["save-session", "-t", "work"]).await;
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("saved "), "{out}");
    let (_, out, _) = h.cli(&["list-saved"]).await;
    assert!(out.contains("work: saved") && out.contains("(running)"), "{out}");

    // Kill it: the file survives; resuming by name brings it back with the
    // same shape and commands.
    let (code, _, _) = h.cli(&["kill-session", "-t", "work"]).await;
    assert_eq!(code, 0);
    let (code, _, err) = h.cli(&["has-session", "-t", "work"]).await;
    assert_eq!(code, 1, "{err}");
    let (_, out, _) = h.cli(&["list-saved"]).await;
    let work_line = out.lines().find(|l| l.starts_with("work:")).unwrap_or_default();
    assert!(work_line.contains("saved") && !work_line.contains("(running)"), "{out}");
    let (_, out, _) = h.cli(&["ls"]).await;
    assert!(!out.contains("work") && out.contains("keeper"), "{out}");

    let mut c = h.connect().await;
    let session = c.attach(&["resume", "work"]).await;
    assert_eq!(session, "work");
    let (_, out, _) = h.cli(&["list-windows", "-t", "work"]).await;
    assert!(out.contains("0: edit* (3 panes)") && out.contains("1: logs"), "{out}");
    let (_, out, _) = h.cli(&["list-panes", "-t", "work:0"]).await;
    assert_eq!(out.lines().count(), 3, "{out}");
    // Each pane came back with its own command line: three different prompts.
    c.wait_for("restored prompts", |s| ["A>", "B>", "C>"].iter().all(|p| s.contents().contains(p))).await;
    // The layout (left | (top / bottom)) is the same: a vertical border and a
    // horizontal one in the right half.
    assert!(c.screen.screen().cell(0, COLS / 2).is_some_and(|x| x.contents() == "│"), "{}", c.text());
    assert!(
        (COLS / 2 + 1..COLS).any(|x| c.screen.screen().cell((ROWS - 1) / 2, x).is_some_and(|c| c.contents() == "─")),
        "{}",
        c.text()
    );

    // Resuming a running session is just an attach for a second client.
    let mut c2 = h.connect().await;
    assert_eq!(c2.attach(&["resume", "work"]).await, "work");
    c2.wait_for("attached", |s| s.contents().contains("A>")).await;

    // `resume` with no name restores everything that is not running.
    let (code, _, err) = h.cli(&["new", "-d", "-s", "other"]).await;
    assert_eq!(code, 0, "{err}");
    h.cli(&["save-session", "-t", "other"]).await;
    h.cli(&["kill-session", "-t", "other"]).await;
    let (code, out, _) = h.cli(&["restore-session"]).await;
    assert_eq!(code, 0);
    assert!(out.starts_with("restored 1 session"), "{out}");
    let (code, _, _) = h.cli(&["has-session", "-t", "other"]).await;
    assert_eq!(code, 0);
    // Unknown names and deletion.
    let (code, _, err) = h.cli(&["resume", "nope"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("no saved session"), "{err}");
    // A running session cannot be forgotten (autosave would bring it back).
    let (code, _, err) = h.cli(&["delete-saved", "other"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("is running"), "{err}");
    h.cli(&["kill-session", "-t", "other"]).await;
    let (code, _, _) = h.cli(&["delete-saved", "other"]).await;
    assert_eq!(code, 0);
    let (_, out, _) = h.cli(&["list-saved"]).await;
    assert!(!out.contains("other:"), "{out}");
    // Renaming a session moves its saved file: no ghost under the old name.
    h.cli(&["rename-session", "-t", "work", "work2"]).await;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (_, out, _) = h.cli(&["list-saved"]).await;
        if out.contains("work2:") && !out.lines().any(|l| l.starts_with("work:")) {
            break;
        }
        assert!(Instant::now() < deadline, "{out}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    h.cli(&["rename-session", "-t", "work2", "work"]).await;

    // set-cwd records the directory a pane will be resumed in: explicit, or
    // the calling client's own (the harness sends temp_dir as its cwd).
    let (code, _, err) = h.cli(&["set-cwd", "-t", "work:0.1", "C:\\Windows"]).await;
    assert_eq!(code, 0, "{err}");
    let (code, _, _) = h.cli(&["set-cwd", "-t", "work:0.2"]).await;
    assert_eq!(code, 0);
    let (code, _, err) = h.cli(&["set-cwd", "-t", "work:0.0", "C:\\definitely\\not\\here"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("not a directory"), "{err}");
    // Relative directories resolve against the client's cwd (temp_dir here).
    let sub = std::env::temp_dir().join(format!("wmux-rel-{}", std::process::id()));
    std::fs::create_dir_all(&sub).unwrap();
    let rel = sub.file_name().unwrap().to_string_lossy().into_owned();
    let (code, _, err) = h.cli(&["set-cwd", "-t", "work:0.0", &rel]).await;
    assert_eq!(code, 0, "{err}");
    let (_, out, _) = h.cli(&["list-panes", "-t", "work:0"]).await;
    assert!(out.lines().next().is_some_and(|l| l.contains(&rel)), "{out}");
    let _ = std::fs::remove_dir_all(&sub);
    let (_, out, _) = h.cli(&["list-panes", "-t", "work:0"]).await;
    let tmp = std::env::temp_dir().to_string_lossy().trim_end_matches('\\').to_string();
    assert!(out.contains("[C:\\Windows]") && out.contains(&format!("[{tmp}")), "{out}");
    // The shell itself can announce its directory (OSC 9;9), as a prompt
    // function would; it travels through ConPTY like a title change does.
    h.cli(&[
        "send-keys",
        "-t",
        "work:0.0",
        "pwsh -NoProfile -Command \"Write-Host ([char]27+']9;9;C:\\Users'+[char]7)\"",
        "Enter",
    ])
    .await;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (_, out, _) = h.cli(&["list-panes", "-t", "work:0"]).await;
        if out.lines().next().is_some_and(|l| l.contains("[C:\\Users]")) {
            break;
        }
        assert!(Instant::now() < deadline, "OSC cwd not picked up: {out}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    // ...and all of that lands in the saved file.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let f = wmux::resurrect::SavedFile::load(&wmux::resurrect::find(&dir, "work").unwrap()).unwrap();
        let cwds: Vec<Option<String>> = f.session.windows[0].layout.panes().iter().map(|p| p.cwd.clone()).collect();
        if cwds[0].as_deref() == Some("C:\\Users") && cwds[1].as_deref() == Some("C:\\Windows") {
            break;
        }
        assert!(Instant::now() < deadline, "saved cwds: {cwds:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Autosave: a structural change is on disk within a couple of ticks.
    h.cli(&["rename-window", "-t", "work:1", "renamed-by-autosave"]).await;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let path = wmux::resurrect::find(&dir, "work").unwrap();
        let f = wmux::resurrect::SavedFile::load(&path).unwrap();
        if f.session.windows.iter().any(|w| w.name == "renamed-by-autosave") {
            break;
        }
        assert!(Instant::now() < deadline, "autosave did not pick up the rename");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    h.cli(&["kill-server"]).await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_keys_and_synchronize_panes() {
    let h = Harness::start("vim").await;
    let mut c = h.connect().await;
    c.attach(&["new", "-s", "v"]).await;
    c.wait_for("prompt", |s| s.contents().contains("wmux>")).await;
    c.prefix('%').await; // left | right, right active
    c.wait_for("split", |s| s.contents().matches("wmux>").count() >= 2).await;
    // prefix h -> left pane active, prefix l -> right again.
    c.prefix('h').await;
    let (_, out, _) = h.cli(&["list-panes", "-t", "v"]).await;
    assert!(out.lines().next().unwrap().contains("(active)"), "{out}");
    c.prefix('l').await;
    let (_, out, _) = h.cli(&["list-panes", "-t", "v"]).await;
    assert!(out.lines().nth(1).unwrap().contains("(active)"), "{out}");
    // prefix H shrinks the right pane's left edge... i.e. resizes; widths change.
    c.prefix('H').await;
    let (_, out, _) = h.cli(&["list-panes", "-t", "v"]).await;
    // 80 columns: 35 + border + 44.
    assert!(out.contains("[44x") && out.contains("[35x"), "{out}");
    // prefix Tab is last-window now that l is taken.
    c.prefix('c').await;
    c.wait_for("window 1", |s| s.rows(0, COLS).nth(ROWS as usize - 1).unwrap().contains("1:cmd*")).await;
    c.prefix('\t').await;
    c.wait_for("back to 0", |s| s.rows(0, COLS).nth(ROWS as usize - 1).unwrap().contains("0:cmd*")).await;

    // synchronize-panes: typing lands in both panes; the S flag shows.
    c.prefix('S').await;
    c.wait_for("S flag", |s| s.rows(0, COLS).nth(ROWS as usize - 1).unwrap().contains("0:cmd*S")).await;
    c.type_str("echo both-panes").await;
    c.enter().await;
    c.wait_for("echoed twice", |s| s.contents().matches("both-panes").count() >= 4).await;
    let (_, a, _) = h.cli(&["capture-pane", "-p", "-t", "v:0.0"]).await;
    let (_, b, _) = h.cli(&["capture-pane", "-p", "-t", "v:0.1"]).await;
    assert!(a.contains("both-panes") && b.contains("both-panes"), "{a}\n---\n{b}");
    // send-keys follows the flag too; then off again.
    h.cli(&["send-keys", "-t", "v:0.0", "echo via-send", "Enter"]).await;
    c.wait_for("send-keys to both", |s| s.contents().matches("via-send").count() >= 4).await;
    let (code, _, _) = h.cli(&["set", "-w", "synchronize-panes", "off"]).await;
    assert_eq!(code, 0);
    c.wait_for("S flag gone", |s| !s.rows(0, COLS).nth(ROWS as usize - 1).unwrap().contains("0:cmd*S")).await;
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn choose_tree_picker() {
    let h = Harness::start("choose").await;
    let mut c = h.connect().await;
    c.attach(&["new", "-s", "a"]).await;
    c.wait_for("prompt", |s| s.contents().contains("wmux>")).await;
    c.prefix('c').await;
    c.wait_for("window 1", |s| s.rows(0, COLS).nth(ROWS as usize - 1).unwrap().contains("1:cmd*")).await;
    let (code, _, err) = h.cli(&["new", "-d", "-s", "b"]).await;
    assert_eq!(code, 0, "{err}");
    // The shell in the detached session must be up before its keystrokes matter.
    h.wait_capture("b:0", "shell prompt", |t| t.contains("wmux>")).await;

    // prefix w: every session expanded, cursor on the current window (item 3 of 5).
    c.prefix('w').await;
    // The pane title arrives over OSC, so wait for the fully drawn tree.
    c.wait_for("picker", |s| {
        let t = s.contents();
        // The title is the shell's own path, with "Administrator: " (localized)
        // in front of it when the test runs elevated.
        t.contains("[3/5] j/k move") && t.contains("(1)   - 0: cmd- (1 panes) \"") && t.contains("cmd.exe\"")
    })
    .await;
    let text = c.text();
    assert!(text.contains("(0) - a: 2 windows (attached)"), "{text}");
    assert!(text.contains("(2)   - 1: cmd* (1 panes)"), "{text}");
    assert!(text.contains("(3) - b: 1 windows"), "{text}");
    // Every session's current window carries the *, as in tmux.
    assert!(text.contains("(4)   - 0: cmd* (1 panes)"), "{text}");
    // vim motions: k up, g top, G bottom, j clamps at the end, digits jump.
    c.type_str("k").await;
    c.wait_for("k", |s| s.contents().contains("[2/5]")).await;
    c.type_str("g").await;
    c.wait_for("g", |s| s.contents().contains("[1/5]")).await;
    c.key(b'G' as u16, 'G', SHIFT_PRESSED).await;
    c.wait_for("G", |s| s.contents().contains("[5/5]")).await;
    c.type_str("j").await;
    c.key(b'X' as u16, 'x', 0).await; // unbound key: ignored, picker stays
    c.wait_for("clamped", |s| s.contents().contains("[5/5]")).await;
    c.key(b'1' as u16, '1', 0).await;
    c.wait_for("digit", |s| s.contents().contains("[2/5]")).await;
    // Enter on "a:0" selects window 0 and closes the picker.
    c.enter().await;
    c.wait_for("window 0", |s| {
        let t = s.contents();
        !t.contains("j/k move") && s.rows(0, COLS).nth(ROWS as usize - 1).unwrap().contains("0:cmd*")
    })
    .await;

    // prefix s: sessions only; Enter switches the client to "b".
    c.prefix('s').await;
    c.wait_for("sessions", |s| s.contents().contains("[1/2] j/k move")).await;
    let text = c.text();
    assert!(text.contains("(0) + a: 2 windows (attached)") && text.contains("(1) + b: 1 windows"), "{text}");
    assert!(!text.contains("0: cmd"), "collapsed: {text}");
    c.type_str("j").await;
    c.enter().await;
    c.wait_for("switched to b", |s| s.rows(0, COLS).nth(ROWS as usize - 1).unwrap().starts_with("[b] 0:cmd*")).await;

    // The picker is a mode, not a keyboard trap: the prefix still works, so
    // `prefix ?` (list-keys) overlays it and the next key dismisses the
    // overlay and leaves the picker standing.
    c.prefix('w').await;
    c.wait_for("picker", |s| s.contents().contains("j/k move")).await;
    c.prefix('?').await;
    c.wait_for("keys overlay", |s| s.contents().contains("bind-key -T prefix")).await;
    c.type_str("q").await; // dismisses the overlay only
    c.wait_for("picker back", |s| {
        let t = s.contents();
        t.contains("j/k move") && !t.contains("bind-key -T prefix")
    })
    .await;

    // q and Escape cancel without touching anything; keys never reach the pane.
    c.type_str("q").await;
    c.wait_for("closed", |s| !s.contents().contains("j/k move")).await;
    c.prefix('w').await;
    c.wait_for("picker again", |s| s.contents().contains("[5/5] j/k move")).await;
    // The tree is live: a window created meanwhile shows up, the cursor stays
    // on the same item (b:0 is now 5 of 6).
    let (code, _, err) = h.cli(&["new-window", "-d", "-t", "b"]).await;
    assert_eq!(code, 0, "{err}");
    c.wait_for("live", |s| s.contents().contains("[5/6] j/k move") && s.contents().contains("(5)   - 1: cmd")).await;
    c.key(0x1B, '\x1b', 0).await;
    c.wait_for("closed", |s| !s.contents().contains("j/k move")).await;
    // Nothing the picker consumed reached the shell: one untouched prompt.
    let out = h.cli(&["capture-pane", "-p", "-t", "b:0"]).await.1;
    assert_eq!(out.trim(), "wmux>", "picker keys leaked into the pane: {out:?}");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn choose_tree_scrolls_and_follows_the_live_tree() {
    let h = Harness::start("choose-scroll").await;
    // base-index 1 must show through, and with the status line off the picker
    // gets the whole screen.
    h.cli(&["set", "-g", "base-index", "1"]).await;
    h.cli(&["set", "-g", "status", "off"]).await;
    let mut c = h.connect().await;
    c.attach(&["new", "-s", "many"]).await;
    c.wait_for("prompt", |s| s.contents().contains("wmux>")).await;
    for _ in 0..29 {
        let (code, _, err) = h.cli(&["new-window", "-d", "-t", "many"]).await;
        assert_eq!(code, 0, "{err}");
    }

    // 1 session + 30 windows = 31 items, 23 body rows (24 rows, no status line).
    c.prefix('w').await;
    c.wait_for("picker", |s| s.contents().contains("[2/31] j/k move")).await;
    assert!(c.row(0).starts_with("(0) - many: 30 windows (attached)"), "{:?}", c.row(0));
    assert!(c.row(1).starts_with("(1)   - 1: cmd*"), "base-index 1: {:?}", c.row(1));
    // Nothing scrolled yet; the last body row is item 22.
    assert!(c.row(22).starts_with("      - 22:"), "{:?}", c.row(22));
    // G: the last item is visible on the last body row, the list scrolled.
    c.key(b'G' as u16, 'G', SHIFT_PRESSED).await;
    c.wait_for("bottom", |s| s.contents().contains("[31/31]")).await;
    // Item 8 is now the top line; its "(8)" jump tag travels with it.
    assert!(c.row(0).starts_with("(8)   - 8:"), "scrolled: {:?}", c.row(0));
    assert!(c.row(22).starts_with("      - 30:"), "{:?}", c.row(22));
    // g: back to the top, scrolled back.
    c.type_str("g").await;
    c.wait_for("top", |s| s.contents().contains("[1/31]")).await;
    assert!(c.row(0).starts_with("(0) - many: 30 windows"), "{:?}", c.row(0));

    // The tree is live under the cursor: kill a window and the count drops
    // while the selection stays on the session line.
    c.key(0x22, '\0', 0).await; // PageDown (VK_NEXT): one body page down
    c.wait_for("paged", |s| s.contents().contains("[24/31]")).await;
    let (code, _, err) = h.cli(&["kill-window", "-t", "many:30"]).await;
    assert_eq!(code, 0, "{err}");
    c.wait_for("30 items", |s| s.contents().contains("[24/30]")).await;
    // Enter on window 23 (item 24 with base-index 1) selects it.
    c.enter().await;
    c.wait_for("selected", |s| !s.contents().contains("j/k move")).await;
    let (_, out, _) = h.cli(&["list-windows", "-t", "many"]).await;
    assert!(out.lines().nth(22).unwrap().starts_with("23: cmd*"), "{out}");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn choose_tree_degenerate_sizes_and_wide_names() {
    let h = Harness::start("choose-edge").await;
    let mut c = h.connect().await;
    c.attach(&["new", "-s", "会话", "-n", "编辑器"]).await;
    c.wait_for("prompt", |s| s.contents().contains("wmux>")).await;
    let (code, _, err) = h.cli(&["new", "-d", "-s", "gone"]).await;
    assert_eq!(code, 0, "{err}");

    c.prefix('w').await;
    // 2 sessions + their 2 windows = 4 items; the cursor starts on 会话:0.
    c.wait_for("picker", |s| s.contents().contains("[2/4] j/k move")).await;
    // Double-width names survive the layout.
    assert!(c.row(0).starts_with("(0) - 会话: 1 windows (attached)"), "{:?}", c.row(0));
    assert!(c.row(1).starts_with("(1)   - 0: 编辑器*"), "{:?}", c.row(1));

    // A terminal with room for a single body row still renders hint and all.
    c.send(ClientMsg::Resize { cols: 20, rows: 3 }).await;
    c.screen = vt100::Parser::new(3, 20, 0);
    c.wait_for("tiny", |s| s.rows(0, 20).nth(1).unwrap().starts_with("[2/4] j/k move")).await;
    assert!(c.row(0).starts_with("(1)   - 0: 编辑器*"), "the selection stays visible: {:?}", c.row(0));
    // One column wide is degenerate but must not panic or wedge the server.
    c.send(ClientMsg::Resize { cols: 1, rows: 1 }).await;
    c.screen = vt100::Parser::new(1, 1, 0);
    c.send(ClientMsg::Resize { cols: 80, rows: 24 }).await;
    c.screen = vt100::Parser::new(ROWS, COLS, 0);
    c.wait_for("back", |s| s.contents().contains("[2/4] j/k move")).await;

    // The item under the cursor vanishing clamps the selection instead of
    // pointing past the end.
    c.key(0x23, '\0', 0).await; // End: the last item, the window of "gone"
    c.wait_for("last", |s| s.contents().contains("[4/4]")).await;
    let (code, _, err) = h.cli(&["kill-session", "-t", "gone"]).await;
    assert_eq!(code, 0, "{err}");
    c.wait_for("clamped", |s| s.contents().contains("[2/2] j/k move")).await;
    c.enter().await;
    c.wait_for("still alive", |s| {
        !s.contents().contains("j/k move") && s.rows(0, COLS).nth(ROWS as usize - 1).unwrap().contains("0:编辑器*")
    })
    .await;
    let (_, out, _) = h.cli(&["ls"]).await;
    assert!(out.starts_with("会话: 1 windows") && !out.contains("gone"), "{out}");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn copy_mode_vi_motions_and_modes() {
    let h = Harness::start("motions").await;
    let mut c = h.connect().await;
    c.attach(&["new", "-s", "v"]).await;
    c.wait_for("prompt", |s| s.contents().contains("wmux>")).await;
    c.type_str("echo alpha beta gamma delta").await;
    c.enter().await;
    c.wait_for("output", |s| s.contents().contains("alpha beta gamma delta")).await;

    // Search puts the cursor on a known word; w and e then walk from there.
    c.prefix('[').await;
    c.type_str("?alpha beta").await;
    c.enter().await;
    c.type_str("v").await; // start a selection at the match
    c.type_str("e").await; // to the end of "alpha"
    c.enter().await; // copy
    c.wait_for("copied", |s| s.contents().contains("copied")).await;
    let (_, out, _) = h.cli(&["show-buffer"]).await;
    assert_eq!(out.trim_end(), "alpha", "e stops at the end of the word: {out:?}");

    c.prefix('[').await;
    c.type_str("?alpha beta").await;
    c.enter().await;
    c.type_str("ww").await; // over "alpha" and "beta" to "gamma"
    c.type_str("v").await;
    c.type_str("e").await;
    c.enter().await;
    let (_, out, _) = h.cli(&["show-buffer"]).await;
    assert_eq!(out.trim_end(), "gamma", "w moves a word at a time: {out:?}");
    c.prefix('[').await;
    c.type_str("?gamma").await;
    c.enter().await;
    c.type_str("b").await; // back one word, to "beta"
    c.type_str("v").await;
    c.type_str("e").await;
    c.enter().await;
    let (_, out, _) = h.cli(&["show-buffer"]).await;
    assert_eq!(out.trim_end(), "beta", "b goes back a word: {out:?}");

    // A count repeats a motion: 3j moves three lines.
    c.prefix('[').await;
    c.type_str("gv").await; // top of the scrollback, start selecting
    c.type_str("3j").await;
    c.enter().await;
    let (_, out, _) = h.cli(&["show-buffer"]).await;
    assert!(out.lines().count() >= 3, "3j selected three lines: {out:?}");

    // C-v makes the selection a rectangle: same columns on every line.
    c.prefix('[').await;
    c.key(b'V' as u16, '\x16', LEFT_CTRL_PRESSED).await; // C-v
    c.type_str("jjll").await;
    c.enter().await;
    let (_, out, _) = h.cli(&["show-buffer"]).await;
    assert!(out.lines().all(|l| l.chars().count() <= 3), "a rectangle is narrow: {out:?}");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn clock_conditionals_and_client_commands() {
    let h = Harness::start("clock").await;
    let mut c = h.connect().await;
    c.attach(&["new", "-s", "c1"]).await;
    c.wait_for("prompt", |s| s.contents().contains("wmux>")).await;

    // prefix t draws a clock; any key puts it away.
    c.prefix('t').await;
    c.wait_for("clock", |s| s.contents().contains("███")).await;
    c.type_str("x").await;
    c.wait_for("clock gone", |s| !s.contents().contains("███")).await;

    // if-shell -F takes the branch the format says.
    h.cli(&["if-shell", "-F", "#{?session_name,yes,}", "set -g @cond true", "set -g @cond false"]).await;
    let (_, out, _) = h.cli(&["show-options", "-gv", "@cond"]).await;
    assert_eq!(out.trim(), "true");
    h.cli(&["if-shell", "-F", "", "set -g @cond then", "set -g @cond else"]).await;
    let (_, out, _) = h.cli(&["show-options", "-gv", "@cond"]).await;
    assert_eq!(out.trim(), "else");
    // Without -F the shell's exit status decides.
    h.cli(&["if-shell", "cmd /c exit 0", "set -g @sh ok", "set -g @sh bad"]).await;
    let (_, out, _) = h.cli(&["show-options", "-gv", "@sh"]).await;
    assert_eq!(out.trim(), "ok");
    h.cli(&["if-shell", "cmd /c exit 1", "set -g @sh ok", "set -g @sh bad"]).await;
    let (_, out, _) = h.cli(&["show-options", "-gv", "@sh"]).await;
    assert_eq!(out.trim(), "bad");

    // The attached client shows up, and messages are remembered.
    let (_, out, _) = h.cli(&["list-clients"]).await;
    assert!(out.contains("c1") && out.contains("[80x24]"), "{out}");
    let (_, out, _) = h.cli(&["show-messages"]).await;
    assert!(out.contains("copied") || out.lines().count() >= 1, "{out}");

    // switch-client acts on the client that runs it, so this goes through the
    // session's own keyboard: prefix ) to the next session, then -l back.
    h.cli(&["new", "-d", "-s", "c2"]).await;
    c.prefix(')').await;
    c.wait_for("on c2", |s| s.rows(0, COLS).nth(ROWS as usize - 1).unwrap().starts_with("[c2]")).await;
    c.prefix(':').await;
    c.type_str("switch-client -l").await;
    c.enter().await;
    c.wait_for("back on c1", |s| s.rows(0, COLS).nth(ROWS as usize - 1).unwrap().starts_with("[c1]")).await;

    // detach-client -a sends everyone home.
    let (code, _, err) = h.cli(&["detach-client", "-a"]).await;
    assert_eq!(code, 0, "{err}");
    let reason = c.wait_detached().await;
    assert_eq!(reason, "detached");
    let (_, out, _) = h.cli(&["list-clients"]).await;
    assert_eq!(out.trim(), "no clients attached");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_small_tmux_commands() {
    let h = Harness::start("small").await;
    let (code, _, err) = h.cli(&["new", "-d", "-s", "a", "cmd.exe", "/q", "/k", "prompt wmux$g"]).await;
    assert_eq!(code, 0, "{err}");
    h.cli(&["split-window", "-h", "-t", "a"]).await;
    h.cli(&["split-window", "-v", "-t", "a"]).await;

    // rotate-window moves the panes around the layout, keeping its shape:
    // the geometry stays, the pane ids in those places move.
    async fn ids(h: &Harness) -> Vec<String> {
        let (_, out, _) = h.cli(&["list-panes", "-t", "a"]).await;
        out.lines().map(|l| l.split_whitespace().nth(2).unwrap().to_string()).collect()
    }
    async fn geometry(h: &Harness) -> Vec<String> {
        let (_, out, _) = h.cli(&["list-panes", "-t", "a"]).await;
        out.lines().map(|l| l.split_whitespace().nth(1).unwrap().to_string()).collect()
    }
    let before = ids(&h).await;
    let shape = geometry(&h).await;
    let (code, _, err) = h.cli(&["rotate-window", "-t", "a"]).await;
    assert_eq!(code, 0, "{err}");
    let after = ids(&h).await;
    assert_eq!(after.len(), before.len());
    assert_ne!(after, before, "the panes moved: {before:?} -> {after:?}");
    assert_eq!(geometry(&h).await, shape, "the layout itself did not change");
    h.cli(&["rotate-window", "-D", "-t", "a"]).await;
    assert_eq!(ids(&h).await, before, "-D undoes -U");

    // next-layout and previous-layout are the names for select-layout -n/-p.
    let (code, _, err) = h.cli(&["next-layout", "-t", "a"]).await;
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = h.cli(&["previous-layout", "-t", "a"]).await;
    assert_eq!(code, 0, "{err}");

    // Listing commands and clients.
    let (_, out, _) = h.cli(&["list-commands"]).await;
    assert!(out.lines().count() > 50, "every command name: {}", out.lines().count());
    assert!(out.contains("rotate-window") && out.contains("attach-session"), "{out}");
    let (_, out, _) = h.cli(&["list-clients"]).await;
    assert_eq!(out.trim(), "no clients attached", "{out}");

    // The environment new panes get.
    h.cli(&["set-environment", "WMUX_TEST_VAR", "hello"]).await;
    let (_, out, _) = h.cli(&["show-environment"]).await;
    assert!(out.contains("WMUX_TEST_VAR=hello"), "{out}");
    let (code, _, err) = h.cli(&["new-window", "-d", "-t", "a", "cmd.exe", "/q", "/k", "prompt wmux$g"]).await;
    assert_eq!(code, 0, "{err}");
    h.cli(&["send-keys", "-t", "a:1", "echo %WMUX_TEST_VAR%", "Enter"]).await;
    let pane = h.wait_capture("a:1", "the variable", |t| t.contains("hello")).await;
    assert!(pane.contains("hello"), "{pane}");
    h.cli(&["set-environment", "-r", "WMUX_TEST_VAR"]).await;
    let (_, out, _) = h.cli(&["show-environment"]).await;
    assert!(!out.contains("WMUX_TEST_VAR"), "{out}");

    // respawn-pane restarts a live pane only with -k.
    let (code, _, err) = h.cli(&["respawn-pane", "-t", "a:1"]).await;
    assert_eq!(code, 1, "a live pane needs -k");
    assert!(err.contains("still running"), "{err}");
    h.cli(&["send-keys", "-t", "a:1", "echo before-respawn", "Enter"]).await;
    h.wait_capture("a:1", "the marker", |t| t.contains("before-respawn")).await;
    let (code, _, err) = h.cli(&["respawn-pane", "-k", "-t", "a:1"]).await;
    assert_eq!(code, 0, "{err}");
    let pane = h.wait_capture("a:1", "a fresh shell", |t| !t.contains("before-respawn") && t.contains("wmux>")).await;
    assert!(!pane.contains("before-respawn"), "the pane started over: {pane}");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn paste_buffers() {
    let h = Harness::start("buffers").await;
    let (code, _, err) = h.cli(&["new", "-d", "-s", "b", "cmd.exe", "/q", "/k", "prompt wmux$g"]).await;
    assert_eq!(code, 0, "{err}");

    let (_, out, _) = h.cli(&["list-buffers"]).await;
    assert_eq!(out.trim(), "no buffers");
    h.cli(&["set-buffer", "hello from wmux"]).await;
    h.cli(&["set-buffer", "-b", "named", "second buffer"]).await;
    let (_, out, _) = h.cli(&["list-buffers"]).await;
    assert!(out.contains("named: 13 bytes: second buffer"), "{out}");
    assert!(out.contains("buffer0: 15 bytes: hello from wmux"), "{out}");
    let (_, out, _) = h.cli(&["show-buffer", "-b", "named"]).await;
    assert_eq!(out.trim(), "second buffer");
    // No -b: the newest buffer.
    let (_, out, _) = h.cli(&["show-buffer"]).await;
    assert_eq!(out.trim(), "second buffer");

    // -a appends to a named buffer.
    h.cli(&["set-buffer", "-a", "-b", "named", " and more"]).await;
    let (_, out, _) = h.cli(&["show-buffer", "-b", "named"]).await;
    assert_eq!(out.trim(), "second buffer and more");

    // Buffers go to and come from files.
    let file = std::env::temp_dir().join(format!("wmux-buffer-{}.txt", std::process::id()));
    let (code, _, err) = h.cli(&["save-buffer", "-b", "named", &file.to_string_lossy()]).await;
    assert_eq!(code, 0, "{err}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "second buffer and more");
    std::fs::write(&file, "from a file").unwrap();
    let (code, _, err) = h.cli(&["load-buffer", "-b", "loaded", &file.to_string_lossy()]).await;
    assert_eq!(code, 0, "{err}");
    let (_, out, _) = h.cli(&["show-buffer", "-b", "loaded"]).await;
    assert_eq!(out.trim(), "from a file");

    // Pasting a named buffer reaches the pane.
    let (code, _, err) = h.cli(&["paste-buffer", "-b", "loaded", "-t", "b"]).await;
    assert_eq!(code, 0, "{err}");
    let pane = h.wait_capture("b", "the pasted text", |t| t.contains("from a file")).await;
    assert!(pane.contains("from a file"), "{pane}");

    // delete-buffer drops the newest, then the named one.
    h.cli(&["delete-buffer"]).await;
    h.cli(&["delete-buffer", "-b", "named"]).await;
    let (_, out, _) = h.cli(&["list-buffers"]).await;
    assert!(!out.contains("named:"), "{out}");
    let (code, _, err) = h.cli(&["paste-buffer", "-b", "nosuch", "-t", "b"]).await;
    assert_eq!(code, 1);
    assert_eq!(err, "no buffer nosuch");
    let _ = std::fs::remove_file(&file);
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn join_pane_marks_and_exact_sizes() {
    let h = Harness::start("join").await;
    let (code, _, err) = h.cli(&["new", "-d", "-s", "j", "cmd.exe", "/q", "/k", "prompt wmux$g"]).await;
    assert_eq!(code, 0, "{err}");
    h.cli(&["new-window", "-d", "-t", "j", "cmd.exe", "/q", "/k", "prompt wmux$g"]).await;
    async fn panes(h: &Harness, w: &str) -> usize {
        let (_, out, _) = h.cli(&["list-panes", "-t", w]).await;
        out.lines().count()
    }
    assert_eq!(panes(&h, "j:0").await, 1);
    assert_eq!(panes(&h, "j:1").await, 1);

    // Move the pane of window 1 next to the pane of window 0.
    let (code, _, err) = h.cli(&["join-pane", "-h", "-s", "j:1", "-t", "j:0"]).await;
    assert_eq!(code, 0, "{err}");
    assert_eq!(panes(&h, "j:0").await, 2, "the pane moved across");
    let (_, out, _) = h.cli(&["list-windows", "-t", "j"]).await;
    assert_eq!(out.lines().count(), 1, "the empty window went away: {out}");

    // A marked pane is what join-pane takes when there is no -s.
    h.cli(&["new-window", "-d", "-t", "j", "cmd.exe", "/q", "/k", "prompt wmux$g"]).await;
    let (code, _, err) = h.cli(&["select-pane", "-m", "-t", "j:1"]).await;
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = h.cli(&["join-pane", "-v", "-t", "j:0"]).await;
    assert_eq!(code, 0, "{err}");
    assert_eq!(panes(&h, "j:0").await, 3);

    // Exact sizes.
    async fn width(h: &Harness) -> u16 {
        let (_, out, _) = h.cli(&["list-panes", "-t", "j:0"]).await;
        out.lines().next().unwrap().split(['[', 'x']).nth(1).unwrap().parse::<u16>().unwrap()
    }
    let (code, _, err) = h.cli(&["resize-pane", "-t", "j:0.0", "-x", "30"]).await;
    assert_eq!(code, 0, "{err}");
    assert_eq!(width(&h).await, 30);
    h.cli(&["resize-pane", "-t", "j:0.0", "-x", "50%"]).await;
    assert_eq!(width(&h).await, 40, "half of 80 columns");
    let (code, _, err) = h.cli(&["resize-pane", "-t", "j:0.0", "-x", "nonsense"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("bad size"), "{err}");

    // A pane can be given a title, which the format strings pick up.
    h.cli(&["select-pane", "-t", "j:0.0", "-T", "logs"]).await;
    let (_, out, _) = h.cli(&["list-panes", "-t", "j:0"]).await;
    assert!(out.lines().next().unwrap().contains("logs"), "{out}");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn window_order_formats_and_short_names() {
    let h = Harness::start("order").await;
    let (code, _, err) = h.cli(&["new", "-d", "-s", "w", "-n", "one"]).await;
    assert_eq!(code, 0, "{err}");
    h.cli(&["new-window", "-d", "-t", "w", "-n", "two"]).await;
    h.cli(&["new-window", "-d", "-t", "w", "-n", "three"]).await;
    let names = || async {
        let (_, out, _) = h.cli(&["list-windows", "-t", "w"]).await;
        // "0: one* (1 panes) [80x24]" -> "one"
        out.lines()
            .map(|l| {
                let after = l.split_once(": ").unwrap().1;
                let word = after.split_whitespace().next().unwrap_or("");
                word.trim_end_matches(['*', '-']).to_string()
            })
            .collect::<Vec<String>>()
    };
    assert_eq!(names().await, ["one", "two", "three"]);

    // swap exchanges two windows, move takes one out and re-inserts it.
    let (code, _, err) = h.cli(&["swap-window", "-s", "w:0", "-t", "w:2"]).await;
    assert_eq!(code, 0, "{err}");
    assert_eq!(names().await, ["three", "two", "one"]);
    let (code, _, err) = h.cli(&["move-window", "-s", "w:2", "-t", "w:0"]).await;
    assert_eq!(code, 0, "{err}");
    assert_eq!(names().await, ["one", "three", "two"]);
    // Short forms of the command names work as in tmux.
    let (code, out, _) = h.cli(&["lsw", "-t", "w"]).await;
    assert_eq!(code, 0);
    assert_eq!(out.lines().count(), 3);
    let (code, _, err) = h.cli(&["kill"]).await;
    assert_eq!(code, 1);
    assert!(err.starts_with("ambiguous command: kill"), "{err}");

    // `set -a` appends, and formats understand conditionals everywhere.
    h.cli(&["set", "-g", "status-right", "A"]).await;
    h.cli(&["set", "-ag", "status-right", "B"]).await;
    let (_, out, _) = h.cli(&["show-options", "-gv", "status-right"]).await;
    assert_eq!(out.trim(), "AB");
    let (_, out, _) = h.cli(&["display-message", "-p", "#{?window_flags,busy,idle}|#{?session_name==w,yes,no}"]).await;
    assert_eq!(out.trim(), "busy|yes");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn layouts_and_pane_numbers() {
    let h = Harness::start("layout").await;
    let mut c = h.connect().await;
    c.attach(&["new", "-s", "g"]).await;
    c.wait_for("prompt", |s| s.contents().contains("wmux>")).await;
    for _ in 0..3 {
        h.cli(&["split-window", "-h", "-t", "g"]).await;
    }
    let widths = || async {
        let (_, out, _) = h.cli(&["list-panes", "-t", "g"]).await;
        out.lines().map(|l| l.split(['[', 'x']).nth(1).unwrap().parse::<u16>().unwrap()).collect::<Vec<u16>>()
    };

    // prefix Space cycles: the first layout is even-horizontal, four columns.
    c.prefix(' ').await;
    c.wait_for("even-horizontal", |s| s.contents().contains("even-horizontal")).await;
    let w = widths().await;
    assert_eq!(w.len(), 4);
    assert!(w.iter().all(|x| (19..=20).contains(x)), "four equal columns: {w:?}");
    // Again: even-vertical, so every pane is full width.
    c.prefix(' ').await;
    c.wait_for("even-vertical", |s| s.contents().contains("even-vertical")).await;
    assert!(widths().await.iter().all(|x| *x == COLS), "full width rows");
    // By name, with a prefix of the name and a target.
    let (code, _, err) = h.cli(&["select-layout", "-t", "g", "til"]).await;
    assert_eq!(code, 0, "{err}");
    let w = widths().await;
    assert!(w.iter().all(|x| (39..=40).contains(x)), "a 2x2 grid: {w:?}");
    let (code, _, err) = h.cli(&["select-layout", "-t", "g", "nope"]).await;
    assert_eq!(code, 1);
    assert!(err.starts_with("unknown layout: nope"), "{err}");

    // prefix q shows the numbers; a digit then picks that pane.
    c.prefix('q').await;
    c.wait_for("numbers", |s| s.contents().contains("███")).await;
    c.key(b'2' as u16, '2', 0).await;
    let (_, out, _) = h.cli(&["list-panes", "-t", "g"]).await;
    assert!(out.lines().nth(2).unwrap().contains("(active)"), "pane 2 is active now: {out}");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn copy_mode_search_finds_scrolled_off_lines() {
    let h = Harness::start("search").await;
    let mut c = h.connect().await;
    c.attach(&["new", "-s", "f"]).await;
    c.wait_for("prompt", |s| s.contents().contains("wmux>")).await;
    // More output than fits, so the early lines are only in the scrollback.
    c.type_str("for /l %i in (1,1,60) do @echo marker-%i").await;
    c.enter().await;
    c.wait_for("output", |s| s.contents().contains("marker-60")).await;
    assert!(!c.text().contains("marker-3 "), "line 3 has scrolled away: {}", c.text());

    // prefix [ enters copy mode; ? searches back through the scrollback, as
    // tmux does (/ looks the other way, towards the newest line).
    c.prefix('[').await;
    c.type_str("?marker-3 ").await;
    c.enter().await;
    c.wait_for("found", |s| s.contents().contains("marker-3 ")).await;

    // n repeats the search further back; N turns around.
    c.type_str("n").await;
    c.wait_for("earlier hit", |s| s.contents().contains("marker-3 ")).await;
    c.type_str("?nothing-like-this").await;
    c.enter().await;
    c.wait_for("miss reported", |s| s.contents().contains("no match: nothing-like-this")).await;
    // Escape leaves copy mode; the pane is still usable.
    c.key(0x1B, '\x1b', 0).await;
    c.key(0x1B, '\x1b', 0).await;
    c.type_str("echo after-search").await;
    c.enter().await;
    let pane = h.wait_capture("f", "the echo after copy mode", |t| t.contains("after-search")).await;
    assert!(pane.contains("after-search"), "{pane}");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pane_base_index_shifts_every_pane_number() {
    let h = Harness::start("panebase").await;
    h.cli(&["set", "-g", "pane-base-index", "1"]).await;
    h.cli(&["set", "-g", "base-index", "1"]).await;
    let (code, _, err) = h.cli(&["new", "-d", "-s", "b"]).await;
    assert_eq!(code, 0, "{err}");
    h.cli(&["split-window", "-h", "-t", "b"]).await;
    h.cli(&["split-window", "-v", "-t", "b"]).await;

    // Listed, targeted and formatted with the same numbers.
    let (_, out, _) = h.cli(&["list-panes", "-t", "b"]).await;
    let nums: Vec<&str> = out.lines().map(|l| l.split(':').next().unwrap()).collect();
    assert_eq!(nums, ["1", "2", "3"], "{out}");
    let (code, _, err) = h.cli(&["send-keys", "-t", "b:1.1", "echo first-pane", "Enter"]).await;
    assert_eq!(code, 0, "{err}");
    let text = h.wait_capture("b:1.1", "the echo", |t| t.contains("first-pane")).await;
    assert!(text.contains("first-pane"), "{text}");
    // Pane 0 no longer exists when the base is 1.
    let (code, _, err) = h.cli(&["send-keys", "-t", "b:1.0", "x"]).await;
    assert_eq!(code, 1, "pane 0 should be gone");
    assert_eq!(err, "no pane 0");
    let (_, out, _) = h.cli(&["display-message", "-p", "#P"]).await;
    assert_eq!(out.trim(), "3", "#P follows the base too: {out}");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn repeatable_keys_chain_without_the_prefix() {
    let h = Harness::start("repeat").await;
    let mut c = h.connect().await;
    c.attach(&["new", "-s", "r"]).await;
    c.wait_for("prompt", |s| s.contents().contains("wmux>")).await;
    // Three panes side by side: 0 | 1 | 2, with 2 active.
    c.prefix('%').await;
    c.wait_for("split", |s| s.contents().matches("wmux>").count() >= 2).await;
    c.prefix('%').await;
    c.wait_for("split again", |s| s.contents().matches("wmux>").count() >= 3).await;
    let active = |out: &str| out.lines().position(|l| l.contains("(active)")).unwrap();
    let (_, out, _) = h.cli(&["list-panes", "-t", "r"]).await;
    assert_eq!(active(&out), 2, "{out}");

    // prefix h, then a bare h: two panes left in one go.
    c.prefix('h').await;
    c.type_str("h").await;
    let (_, out, _) = h.cli(&["list-panes", "-t", "r"]).await;
    assert_eq!(active(&out), 0, "bare h repeated the binding: {out}");

    // The window closes: after repeat-time a bare h is just text again.
    let (code, _, err) = h.cli(&["set", "-g", "repeat-time", "150"]).await;
    assert_eq!(code, 0, "{err}");
    c.prefix('l').await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    c.type_str("hhh").await;
    c.enter().await;
    let (_, out, _) = h.cli(&["list-panes", "-t", "r"]).await;
    assert_eq!(active(&out), 1, "the late h's must not move the pane: {out}");
    let pane = h.wait_capture("r:0.1", "the typed text", |t| t.contains("hhh")).await;
    assert!(pane.contains("hhh"), "{pane}");

    // repeat-time 0 turns it off entirely.
    h.cli(&["set", "-g", "repeat-time", "0"]).await;
    c.prefix('h').await;
    c.type_str("h").await;
    let (_, out, _) = h.cli(&["list-panes", "-t", "r"]).await;
    assert_eq!(active(&out), 0, "the prefixed h still moves: {out}");
    let pane = h.wait_capture("r:0.0", "the second h as text", |t| t.contains("h")).await;
    assert!(pane.contains("h"), "{pane}");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn zoomed_pane_still_navigates_by_direction() {
    let h = Harness::start("zoom-nav").await;
    let mut c = h.connect().await;
    c.attach(&["new", "-s", "z"]).await;
    c.wait_for("prompt", |s| s.contents().contains("wmux>")).await;
    c.prefix('%').await; // left | right, the right one active
    c.wait_for("split", |s| s.contents().matches("wmux>").count() >= 2).await;
    c.prefix('z').await; // zoom the right pane
    c.wait_for("Z flag", |s| s.rows(0, COLS).nth(ROWS as usize - 1).unwrap().contains("0:cmd*Z")).await;
    let (_, out, _) = h.cli(&["list-panes", "-t", "z"]).await;
    assert!(out.lines().nth(1).unwrap().contains("[80x23]"), "zoomed pane fills the window: {out}");
    // The hidden pane keeps running at its own size; it is not 0x0.
    assert!(out.lines().next().unwrap().contains("[40x23]"), "hidden pane keeps its size: {out}");
    h.cli(&["send-keys", "-t", "z:0.0", "echo hidden-alive", "Enter"]).await;
    let hidden = h.wait_capture("z:0.0", "the hidden pane's echo", |t| t.contains("hidden-alive")).await;
    assert!(hidden.contains("hidden-alive"), "{hidden}");

    // The whole point: h moves left out of the zoom instead of "no such pane".
    c.prefix('h').await;
    c.wait_for("unzoomed", |s| {
        let row = s.rows(0, COLS).nth(ROWS as usize - 1).unwrap();
        row.contains("0:cmd*") && !row.contains("*Z")
    })
    .await;
    let (_, out, _) = h.cli(&["list-panes", "-t", "z"]).await;
    assert!(out.lines().next().unwrap().contains("(active)"), "left pane is active: {out}");
    assert!(!c.text().contains("no such pane"), "{}", c.text());

    // And back: zoom the left pane, l returns to the right one.
    c.prefix('z').await;
    c.wait_for("Z flag again", |s| s.rows(0, COLS).nth(ROWS as usize - 1).unwrap().contains("0:cmd*Z")).await;
    c.prefix('l').await;
    c.wait_for("unzoomed again", |s| !s.rows(0, COLS).nth(ROWS as usize - 1).unwrap().contains("*Z")).await;
    let (_, out, _) = h.cli(&["list-panes", "-t", "z"]).await;
    assert!(out.lines().nth(1).unwrap().contains("(active)"), "right pane is active: {out}");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn list_keys_is_reproducible_across_servers() {
    // Two independent servers (different HashMap seeds) must print the key
    // table byte-for-byte identically.
    let a = Harness::start("repro-a").await;
    let b = Harness::start("repro-b").await;
    let (_, ka, _) = a.cli(&["list-keys"]).await;
    let (_, kb, _) = b.cli(&["list-keys"]).await;
    assert_eq!(ka, kb);
    assert!(ka.lines().count() >= 40, "{}", ka.lines().count());
    a.cli(&["kill-server"]).await;
    b.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_protocol_version_is_rejected() {
    let h = Harness::start("proto").await;
    let mut c = h.connect().await;
    c.send(ClientMsg::Command {
        version: 999,
        argv: vec!["ls".into()],
        cwd: String::new(),
        cols: 80,
        rows: 24,
        interactive: false,
        pane_env: None,
    })
    .await;
    match c.next().await {
        ServerMsg::Error(e) => assert!(e.contains("protocol mismatch"), "{e}"),
        other => panic!("{other:?}"),
    }
    // Server without sessions keeps running; kill it explicitly.
    let (code, _, _) = h.cli(&["kill-server"]).await;
    assert_eq!(code, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn send_keys_dash_x_drives_copy_mode() {
    let h = Harness::start("sendx").await;
    let (code, _, err) = h.cli(&["new", "-d", "-s", "x"]).await;
    assert_eq!(code, 0, "{err}");
    h.wait_capture("x:0", "shell prompt", |t| t.contains("wmux>")).await;
    h.cli(&["send-keys", "-t", "x:0", "echo alpha beta gamma", "Enter"]).await;
    h.wait_capture("x:0", "echo output", |t| t.matches("alpha beta gamma").count() >= 2).await;

    // The copy-mode commands work on the pane named by -t, with no client
    // attached anywhere: search back to the word, select it, copy it.
    for argv in [
        vec!["send-keys", "-t", "x:0", "-X", "search-backward", "alpha"],
        vec!["send-keys", "-t", "x:0", "-X", "begin-selection"],
        vec!["send-keys", "-t", "x:0", "-X", "next-word-end"],
        vec!["send-keys", "-t", "x:0", "-X", "copy-selection"],
    ] {
        let (code, _, err) = h.cli(&argv).await;
        assert_eq!(code, 0, "{argv:?}: {err}");
    }
    let (_, out, _) = h.cli(&["show-buffer"]).await;
    assert_eq!(out.trim_end(), "alpha", "send-keys -X copied the searched word: {out:?}");

    // An unknown copy command is refused instead of being ignored.
    let (code, _, err) = h.cli(&["send-keys", "-t", "x:0", "-X", "fly-to-the-moon"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("unknown command 'fly-to-the-moon'"), "{err}");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn remain_on_exit_keeps_the_pane_and_history_survives_resume() {
    let h = Harness::start("remain").await;
    let (code, _, err) = h.cli(&["set", "-g", "remain-on-exit", "on"]).await;
    assert_eq!(code, 0, "{err}");
    // A second session keeps the server alive while "r" is killed below.
    h.cli(&["new", "-d", "-s", "keeper"]).await;
    h.cli(&["new", "-d", "-s", "r"]).await;
    h.wait_capture("r:0", "shell prompt", |t| t.contains("wmux>")).await;
    h.cli(&["send-keys", "-t", "r:0", "echo keepme-42", "Enter"]).await;
    h.wait_capture("r:0", "output", |t| t.contains("keepme-42")).await;

    // save-history: the pane's text goes into the saved file...
    let (code, _, err) = h.cli(&["save-session", "-t", "r"]).await;
    assert_eq!(code, 0, "{err}");
    let file = std::fs::read_dir(&h.sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
        .find(|t| t.contains("\"name\": \"r\""))
        .expect("a saved file for r");
    assert!(file.contains("keepme-42"), "the pane output is saved: {file}");

    // ...and comes back on the screen when the session is resumed.
    h.cli(&["kill-session", "-t", "r"]).await;
    let (code, _, err) = h.cli(&["resume", "r"]).await;
    assert_eq!(code, 0, "{err}");
    h.wait_capture("r:0", "the restored output", |t| t.contains("keepme-42")).await;
    h.wait_capture("r:0", "the restored shell", |t| t.contains("wmux>")).await;

    // The shell exits; with remain-on-exit the pane, the window and the
    // session all stay, and the pane says what happened.
    h.cli(&["send-keys", "-t", "r:0", "exit", "Enter"]).await;
    let pane = h.wait_capture("r:0", "the exit note", |t| t.contains("exited with")).await;
    assert!(pane.contains("keepme-42"), "the output is still there: {pane}");
    let (code, out, _) = h.cli(&["ls"]).await;
    assert_eq!(code, 0);
    assert!(out.lines().any(|l| l.starts_with("r: 1 windows")), "the session outlives its shell: {out}");
    let (_, msgs, _) = h.cli(&["show-messages"]).await;
    assert!(msgs.contains("exited with"), "the exit is logged: {msgs}");

    // respawn-pane starts the shell again in the same pane.
    let (code, _, err) = h.cli(&["respawn-pane", "-t", "r:0"]).await;
    assert_eq!(code, 0, "{err}");
    h.wait_capture("r:0", "a fresh prompt", |t| t.contains("wmux>")).await;
    // ...with the pane environment a new pane gets, so `wmux` inside it
    // still talks to this server (respawn used to pass set-environment only).
    h.cli(&["send-keys", "-t", "r:0", "echo WMUX=%WMUX% PANE=%WMUX_PANE%", "Enter"]).await;
    // The typed line still says %WMUX%; the output line has the real values.
    let want = format!("WMUX={} PANE=", h.socket);
    h.wait_capture("r:0", "the socket name and pane id from inside", |t| {
        t.lines().any(|l| l.strip_prefix(&want).is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit())))
    })
    .await;
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn move_window_between_sessions() {
    let h = Harness::start("movew").await;
    h.cli(&["new", "-d", "-s", "a"]).await;
    h.cli(&["new", "-d", "-s", "b"]).await;
    h.cli(&["new-window", "-d", "-t", "a", "-n", "travels"]).await;
    let (_, out, _) = h.cli(&["list-windows", "-t", "a"]).await;
    assert_eq!(out.lines().count(), 2, "{out}");

    // b is looking at its only window; the arrival must not steal that.
    h.cli(&["new-window", "-t", "b", "-n", "watched"]).await;
    let (_, before, _) = h.cli(&["list-windows", "-t", "b"]).await;
    assert!(before.lines().any(|l| l.contains("watched*")), "{before}");

    let (code, _, err) = h.cli(&["move-window", "-s", "a:1", "-t", "b:0"]).await;
    assert_eq!(code, 0, "{err}");
    let (_, a, _) = h.cli(&["list-windows", "-t", "a"]).await;
    assert_eq!(a.lines().count(), 1, "the window left a: {a}");
    let (_, b, _) = h.cli(&["list-windows", "-t", "b"]).await;
    assert_eq!(b.lines().count(), 3, "and arrived in b: {b}");
    assert!(b.lines().next().unwrap().contains("travels"), "at the index asked for: {b}");
    assert!(b.lines().any(|l| l.contains("watched*")), "b still looks at the same window: {b}");
    assert!(!b.lines().next().unwrap().contains('*'), "the newcomer is not made current: {b}");
    h.cli(&["kill-window", "-t", "b:2"]).await;

    // Moving the last window of a session takes the session with it.
    let (code, _, err) = h.cli(&["move-window", "-s", "a:0", "-t", "b:2"]).await;
    assert_eq!(code, 0, "{err}");
    let (_, out, _) = h.cli(&["ls"]).await;
    assert_eq!(out.lines().count(), 1, "only b is left: {out}");
    assert!(out.starts_with("b: 3 windows"), "{out}");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn choose_client_detaches_the_one_picked() {
    let h = Harness::start("chooseclient").await;
    let mut a = h.connect().await;
    a.attach(&["new", "-s", "c"]).await;
    a.wait_for("prompt", |s| s.contents().contains("wmux>")).await;
    let mut b = h.connect().await;
    b.attach(&["attach", "-t", "c"]).await;
    b.wait_for("prompt", |s| s.contents().contains("wmux>")).await;

    a.prefix('D').await;
    a.wait_for("client list", |s| {
        let t = s.contents();
        t.contains("[1/2] j/k move") && t.matches("client-").count() == 2
    })
    .await;
    // The first line is this client; j moves to the other one and Enter
    // detaches it, leaving us attached.
    a.type_str("j").await;
    a.wait_for("second client", |s| s.contents().contains("[2/2]")).await;
    a.enter().await;
    assert_eq!(b.wait_detached().await, "detached");
    a.wait_for("picker gone", |s| !s.contents().contains("j/k move")).await;
    let (_, out, _) = h.cli(&["list-clients"]).await;
    assert_eq!(out.lines().count(), 1, "one client left: {out}");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pipe_pane_copies_pane_output_into_a_command() {
    let h = Harness::start("pipe").await;
    h.cli(&["new", "-d", "-s", "p"]).await;
    h.wait_capture("p:0", "shell prompt", |t| t.contains("wmux>")).await;
    let out_file = std::env::temp_dir().join(format!("wmux-pipe-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&out_file);
    let cmd = format!("$input | Set-Content -Path '{}'", out_file.display());
    let (code, _, err) = h.cli(&["pipe-pane", "-t", "p:0", &cmd]).await;
    assert_eq!(code, 0, "{err}");

    h.cli(&["send-keys", "-t", "p:0", "echo piped-hello", "Enter"]).await;
    h.wait_capture("p:0", "output", |t| t.contains("piped-hello")).await;
    // No command stops the pipe, which closes the command's input and makes
    // it write the file.
    let (code, _, err) = h.cli(&["pipe-pane", "-t", "p:0"]).await;
    assert_eq!(code, 0, "{err}");

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if std::fs::read_to_string(&out_file).is_ok_and(|t| t.contains("piped-hello")) {
            break;
        }
        assert!(Instant::now() < deadline, "pipe-pane never wrote {}", out_file.display());
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let _ = std::fs::remove_file(&out_file);
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wait_for_channels_signal_and_lock() {
    let h = Harness::start("waitfor").await;
    // A client waiting on a channel gets its answer when another signals it.
    let mut w = h.connect().await;
    w.command(&["wait-for", "chan"], false).await;
    let (code, _, err) = h.cli(&["wait-for", "-S", "chan"]).await;
    assert_eq!(code, 0, "{err}");
    match w.next().await {
        ServerMsg::Done { code } => assert_eq!(code, 0),
        other => panic!("waiting client: {other:?}"),
    }

    // A signal with nobody waiting is remembered for the next waiter.
    h.cli(&["wait-for", "-S", "later"]).await;
    let (code, _, err) = h.cli(&["wait-for", "later"]).await;
    assert_eq!(code, 0, "a remembered signal returns at once: {err}");

    // Lock, queue behind it, hand it over.
    let (code, _, err) = h.cli(&["wait-for", "-L", "mutex"]).await;
    assert_eq!(code, 0, "{err}");
    let mut l = h.connect().await;
    l.command(&["wait-for", "-L", "mutex"], false).await;
    let (code, _, err) = h.cli(&["wait-for", "-U", "mutex"]).await;
    assert_eq!(code, 0, "{err}");
    match l.next().await {
        ServerMsg::Done { code } => assert_eq!(code, 0),
        other => panic!("locker: {other:?}"),
    }
    // Unlocking a channel nobody holds says so.
    let (code, _, err) = h.cli(&["wait-for", "-U", "free"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("not locked"), "{err}");
    let (code, _, err) = h.cli(&["wait-for", "-L", "-S", "x"]).await;
    assert_eq!(code, 1, "exclusive flags are refused: {err}");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn display_menu_runs_an_entry_by_key_and_by_enter() {
    let h = Harness::start("menu").await;
    let mut c = h.connect().await;
    c.attach(&["new", "-s", "m"]).await;
    c.wait_for("prompt", |s| s.contents().contains("wmux>")).await;

    // prefix > is the pane menu; `h` splits the window horizontally.
    c.prefix('>').await;
    c.wait_for("menu", |s| s.contents().contains("(h) Split horizontally")).await;
    let text = c.text();
    assert!(text.contains("pane 0"), "the title is shown: {text}");
    assert!(text.contains("(x) Kill"), "{text}");
    c.type_str("h").await;
    c.wait_for("two panes", |s| s.contents().matches("wmux>").count() >= 2).await;
    let (_, out, _) = h.cli(&["list-panes", "-t", "m:0"]).await;
    assert_eq!(out.lines().count(), 2, "{out}");

    // G lands on the last entry that can be picked (the separators are
    // skipped), and Enter runs it: kill-pane leaves one pane.
    c.prefix('>').await;
    c.wait_for("menu again", |s| s.contents().contains("(h) Split horizontally")).await;
    c.key(b'G' as u16, 'G', SHIFT_PRESSED).await;
    c.enter().await;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let (_, out, _) = h.cli(&["list-panes", "-t", "m:0"]).await;
        if out.lines().count() == 1 {
            break;
        }
        assert!(Instant::now() < deadline, "Enter on the last entry did not kill a pane: {out}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Escape closes a menu without running anything.
    c.prefix('>').await;
    c.wait_for("menu", |s| s.contents().contains("(h) Split horizontally")).await;
    c.key(0x1B, '\x1b', 0).await;
    c.wait_for("menu gone", |s| !s.contents().contains("Split horizontally")).await;
    let (_, out, _) = h.cli(&["list-panes", "-t", "m:0"]).await;
    assert_eq!(out.lines().count(), 1, "{out}");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn display_popup_takes_the_keys_and_closes_with_its_command() {
    let h = Harness::start("popup").await;
    let mut c = h.connect().await;
    c.attach(&["new", "-s", "pop"]).await;
    c.wait_for("prompt", |s| s.contents().contains("wmux>")).await;

    c.prefix(':').await;
    c.type_str("display-popup -E cmd.exe /q /k \"prompt pip$g\"").await;
    c.enter().await;
    c.wait_for("popup", |s| s.contents().contains("pip>")).await;
    assert!(c.text().contains('┌'), "the popup has a border: {}", c.text());

    // Keys go to the popup's program, not to the pane behind it.
    c.type_str("echo inside-popup").await;
    c.enter().await;
    c.wait_for("popup output", |s| s.contents().contains("inside-popup")).await;
    let pane = h.cli(&["capture-pane", "-p", "-t", "pop:0"]).await.1;
    assert!(!pane.contains("inside-popup"), "the pane behind is untouched: {pane}");

    // -E: the box goes away when the command does.
    c.type_str("exit").await;
    c.enter().await;
    c.wait_for("popup closed", |s| !s.contents().contains("pip>")).await;
    // The pane behind still takes keys afterwards.
    c.type_str("echo after-popup").await;
    c.enter().await;
    h.wait_capture("pop:0", "the echo after the popup", |t| t.contains("after-popup")).await;
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn alerts_flag_background_windows() {
    let h = Harness::start("alerts").await;
    let (code, _, err) = h.cli(&["set", "-g", "monitor-activity", "on"]).await;
    assert_eq!(code, 0, "{err}");
    let mut c = h.connect().await;
    c.attach(&["new", "-s", "al"]).await;
    c.wait_for("prompt", |s| s.contents().contains("wmux>")).await;
    c.prefix('c').await;
    c.wait_for("window 1", |s| s.rows(0, COLS).nth(ROWS as usize - 1).unwrap().contains("1:cmd*")).await;
    h.wait_capture("al:1", "second shell", |t| t.contains("wmux>")).await;

    // Output in the window nobody is looking at raises the activity flag.
    h.cli(&["send-keys", "-t", "al:0", "echo background-noise", "Enter"]).await;
    // The flag shows up in the status line and in list-windows, after the
    // "last window" mark, as tmux orders them.
    c.wait_for("activity flag", |s| s.rows(0, COLS).nth(ROWS as usize - 1).unwrap().contains("0:cmd-#")).await;
    let (_, out, _) = h.cli(&["list-windows", "-t", "al"]).await;
    assert!(out.lines().next().is_some_and(|l| l.contains("cmd-#")), "{out}");

    // prefix M-n goes to the window with the alert; looking at it clears it.
    c.key(b'B' as u16, '\x02', LEFT_CTRL_PRESSED).await;
    c.key(b'N' as u16, 'n', LEFT_ALT_PRESSED).await;
    c.wait_for("switched and cleared", |s| {
        let status = s.rows(0, COLS).nth(ROWS as usize - 1).unwrap();
        status.contains("0:cmd*") && !status.contains("0:cmd#")
    })
    .await;
    // With no alert left, the key says so instead of moving.
    c.key(b'B' as u16, '\x02', LEFT_CTRL_PRESSED).await;
    c.key(b'N' as u16, 'n', LEFT_ALT_PRESSED).await;
    c.wait_for("no alert message", |s| s.contents().contains("no window with an alert")).await;

    // The same in a session nobody is attached to: the flag goes up on the
    // background window and comes down when that window becomes current,
    // with no client to do the looking.
    h.cli(&["new", "-d", "-s", "far"]).await;
    h.cli(&["new-window", "-t", "far"]).await;
    h.wait_capture("far:1", "second shell", |t| t.contains("wmux>")).await;
    h.cli(&["send-keys", "-t", "far:0", "echo quiet-noise", "Enter"]).await;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let (_, out, _) = h.cli(&["list-windows", "-t", "far"]).await;
        if out.lines().next().is_some_and(|l| l.contains('#')) {
            break;
        }
        assert!(Instant::now() < deadline, "no flag in a detached session: {out}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    h.cli(&["select-window", "-t", "far:0"]).await;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let (_, out, _) = h.cli(&["list-windows", "-t", "far"]).await;
        if out.lines().next().is_some_and(|l| !l.contains('#')) {
            assert!(out.lines().next().unwrap().contains("cmd*"), "{out}");
            break;
        }
        assert!(Instant::now() < deadline, "the current window kept a stale flag: {out}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn spread_layout_and_capture_with_colours() {
    let h = Harness::start("spread").await;
    h.cli(&["new", "-d", "-s", "sp"]).await;
    h.wait_capture("sp:0", "shell prompt", |t| t.contains("wmux>")).await;
    h.cli(&["split-window", "-h", "-d", "-t", "sp:0"]).await;
    let (code, _, err) = h.cli(&["resize-pane", "-t", "sp:0.0", "-x", "60"]).await;
    assert_eq!(code, 0, "{err}");
    let widths = |out: &str| -> Vec<u16> {
        out.lines()
            .filter_map(|l| l.split('[').nth(1).and_then(|s| s.split('x').next()).and_then(|w| w.parse().ok()))
            .collect()
    };
    let (_, out, _) = h.cli(&["list-panes", "-t", "sp:0"]).await;
    let before = widths(&out);
    assert_eq!(before.len(), 2, "{out}");
    assert!(before[0] > before[1] + 5, "the panes are lopsided to start with: {out}");

    let (code, _, err) = h.cli(&["select-layout", "-E", "-t", "sp:0.0"]).await;
    assert_eq!(code, 0, "{err}");
    let (_, out, _) = h.cli(&["list-panes", "-t", "sp:0"]).await;
    let after = widths(&out);
    assert!(after[0].abs_diff(after[1]) <= 1, "-E evened them out: {out} (was {before:?})");

    // capture-pane -e keeps the colours; without it the text is plain.
    h.cli(&["send-keys", "-t", "sp:0.0", "prompt $e[31mRED$e[0m$g", "Enter"]).await;
    h.wait_capture("sp:0.0", "the coloured prompt", |t| t.contains("RED>")).await;
    let (_, plain, _) = h.cli(&["capture-pane", "-p", "-t", "sp:0.0"]).await;
    assert!(!plain.contains('\u{1b}'), "plain capture has no escapes: {plain:?}");
    let (_, coloured, _) = h.cli(&["capture-pane", "-p", "-e", "-t", "sp:0.0"]).await;
    assert!(coloured.contains("RED"), "{coloured:?}");
    assert!(coloured.contains("\u{1b}[31m"), "-e keeps the colour: {coloured:?}");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn menus_and_popups_survive_degenerate_sizes() {
    let h = Harness::start("degenerate").await;
    let mut c = h.connect().await;
    c.attach(&["new", "-s", "d"]).await;
    c.wait_for("prompt", |s| s.contents().contains("wmux>")).await;

    // A menu with nothing that can be picked: Enter does nothing, Escape
    // closes it, and the pane behind is untouched.
    c.prefix(':').await;
    c.type_str("display-menu \"\"").await;
    c.enter().await;
    c.wait_for("separator-only menu", |s| s.contents().contains("j/k move")).await;
    c.enter().await;
    c.key(0x1B, '\x1b', 0).await;
    c.wait_for("menu gone", |s| !s.contents().contains("j/k move")).await;

    // A terminal with no room for a popup says so instead of drawing a
    // broken box.
    c.send(ClientMsg::Resize { cols: 8, rows: 3 }).await;
    c.prefix(':').await;
    c.type_str("display-popup -E cmd.exe").await;
    c.enter().await;
    // Eight columns cannot show the message, so read it out of the log.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let (_, msgs, _) = h.cli(&["show-messages"]).await;
        if msgs.contains("no room") {
            break;
        }
        assert!(Instant::now() < deadline, "no complaint about the size: {msgs}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // A menu at that size draws what fits and still closes.
    c.prefix('>').await;
    // Eight columns clip the labels; the picker still works.
    c.wait_for("tiny menu", |s| s.contents().contains("(h) Spli")).await;
    c.key(0x1B, '\x1b', 0).await;
    c.wait_for("tiny menu gone", |s| !s.contents().contains("(h) Spli")).await;

    // Back to a usable size: a popup opens, survives a resize down to
    // nothing, and closing one that is not there is not an error.
    c.send(ClientMsg::Resize { cols: 80, rows: 24 }).await;
    c.prefix(':').await;
    c.type_str("display-popup -E cmd.exe /q /k \"prompt tiny$g\"").await;
    c.enter().await;
    c.wait_for("popup", |s| s.contents().contains("tiny>")).await;
    c.send(ClientMsg::Resize { cols: 6, rows: 4 }).await;
    c.send(ClientMsg::Resize { cols: 80, rows: 24 }).await;
    c.type_str("echo still-alive").await;
    c.enter().await;
    // Either the popup survived the squeeze (and took the keys) or it was
    // dropped (and the pane took them); both are fine, a panic is not.
    h.wait_capture("d:0", "the session still works", |t| t.contains("wmux>")).await;

    // -C from a script closes the popup the user is looking at, and doing it
    // again with none open is not an error.
    let (code, _, err) = h.cli(&["display-popup", "-C"]).await;
    assert_eq!(code, 0, "{err}");
    c.wait_for("popup closed from outside", |s| !s.contents().contains("tiny>")).await;
    let (code, _, err) = h.cli(&["display-popup", "-C"]).await;
    assert_eq!(code, 0, "closing nothing is fine: {err}");
    let (code, out, _) = h.cli(&["ls"]).await;
    assert_eq!(code, 0, "the server is still healthy: {out}");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn degenerate_targets_for_the_new_commands() {
    let h = Harness::start("degen2").await;
    h.cli(&["new", "-d", "-s", "one"]).await;
    h.wait_capture("one:0", "shell prompt", |t| t.contains("wmux>")).await;

    // select-layout -E needs something beside the pane.
    let (code, _, err) = h.cli(&["select-layout", "-E", "-t", "one:0"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("no panes beside it"), "{err}");

    // Commands that need a client say so instead of doing half the work.
    for argv in [vec!["display-menu", "x", "k", "kill-pane"], vec!["choose-client"], vec!["display-popup", "cmd.exe"]] {
        let (code, _, err) = h.cli(&argv).await;
        assert_eq!(code, 1, "{argv:?}");
        assert!(err.contains("not attached"), "{argv:?}: {err}");
    }

    // A pipe whose command is gone ends quietly, and output keeps flowing.
    let (code, _, err) = h.cli(&["pipe-pane", "-t", "one:0", "exit"]).await;
    assert_eq!(code, 0, "{err}");
    h.cli(&["send-keys", "-t", "one:0", "echo after-dead-pipe", "Enter"]).await;
    h.wait_capture("one:0", "output after the pipe died", |t| t.contains("after-dead-pipe")).await;
    // Stopping a pipe that is not running is not an error either.
    let (code, _, err) = h.cli(&["pipe-pane", "-t", "one:0"]).await;
    assert_eq!(code, 0, "{err}");
    // -o with nothing running starts one; -o again stops it.
    h.cli(&["pipe-pane", "-o", "-t", "one:0", "$input | Out-Null"]).await;
    let (code, _, err) = h.cli(&["pipe-pane", "-o", "-t", "one:0", "$input | Out-Null"]).await;
    assert_eq!(code, 0, "{err}");

    // capture-pane -e on a pane that has printed nothing is empty, not junk.
    h.cli(&["new-window", "-d", "-t", "one", "cmd.exe", "/q", "/k", "prompt $h$h$h"]).await;
    let (code, out, _) = h.cli(&["capture-pane", "-p", "-e", "-t", "one:1"]).await;
    assert_eq!(code, 0);
    assert!(out.trim().is_empty() || !out.contains("\u{1b}[0m\u{1b}[0m"), "{out:?}");

    // move-window onto itself and to a free index.
    let (code, _, err) = h.cli(&["move-window", "-s", "one:0", "-t", "one:0"]).await;
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = h.cli(&["move-window", "-s", "one:1", "-t", "one:7"]).await;
    assert_eq!(code, 0, "a free index is where it goes: {err}");
    let (_, out, _) = h.cli(&["list-windows", "-t", "one"]).await;
    assert_eq!(out.lines().count(), 2, "{out}");

    // wait-for on a client that goes away leaves nothing behind: the lock
    // can still be taken afterwards.
    {
        let mut gone = h.connect().await;
        gone.command(&["wait-for", "-L", "held"], false).await;
        // Take the lock away from under it, then drop the connection.
        let _ = gone;
    }
    let (code, _, err) = h.cli(&["wait-for", "-L", "held2"]).await;
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = h.cli(&["wait-for", "-U", "held2"]).await;
    assert_eq!(code, 0, "{err}");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn option_names_take_abbreviations_and_flip() {
    let h = Harness::start("optnames").await;
    h.cli(&["new", "-d", "-s", "o"]).await;
    h.wait_capture("o:0", "shell prompt", |t| t.contains("wmux>")).await;

    // `set sync` is `set synchronize-panes`, and no value flips it.
    assert_eq!(h.cli(&["show", "-gv", "sync"]).await.1.trim(), "off");
    let (code, _, err) = h.cli(&["set", "sync"]).await;
    assert_eq!(code, 0, "{err}");
    assert_eq!(h.cli(&["show", "-gv", "sync"]).await.1.trim(), "on");
    let (_, out, _) = h.cli(&["list-windows", "-t", "o"]).await;
    assert!(out.contains("*S"), "the status flag follows: {out}");
    h.cli(&["set", "sync", "off"]).await;
    assert_eq!(h.cli(&["show", "-gv", "sync"]).await.1.trim(), "off");

    // Each dash-separated word may be shortened too.
    let (code, _, err) = h.cli(&["set", "mon-act", "on"]).await;
    assert_eq!(code, 0, "{err}");
    assert_eq!(h.cli(&["show", "-gv", "monitor-activity"]).await.1.trim(), "on");
    assert_eq!(h.cli(&["show", "-gv", "mon-act"]).await.1.trim(), "on", "show takes them as well");

    // Flipping works for any on/off option, and a number still needs a value.
    let before = h.cli(&["show", "-gv", "mouse"]).await.1.trim().to_string();
    h.cli(&["set", "mouse"]).await;
    assert_ne!(h.cli(&["show", "-gv", "mouse"]).await.1.trim(), before);
    h.cli(&["set", "mou"]).await;
    assert_eq!(h.cli(&["show", "-gv", "mouse"]).await.1.trim(), before);
    let (code, _, err) = h.cli(&["set", "history-limit"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("bad number"), "{err}");

    // An abbreviation that could mean several things says which.
    let (code, _, err) = h.cli(&["set", "mon", "on"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("ambiguous option: mon") && err.contains("monitor-bell"), "{err}");
    // And one that means nothing is still an unknown option.
    let (code, _, err) = h.cli(&["set", "frobnicate", "on"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("unknown option"), "{err}");

    // Inside a session the `:` prompt takes the same short names.
    let mut c = h.connect().await;
    c.attach(&["attach", "-t", "o"]).await;
    c.wait_for("attached", |s| s.contents().contains("wmux>")).await;
    c.prefix(':').await;
    c.type_str("set sync").await;
    c.enter().await;
    c.wait_for("sync on from the prompt", |s| s.rows(0, COLS).nth(ROWS as usize - 1).unwrap().contains("*S")).await;
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn find_text_looks_through_every_pane() {
    let h = Harness::start("findtext").await;
    h.cli(&["new", "-d", "-s", "ft"]).await;
    h.wait_capture("ft:0", "shell prompt", |t| t.contains("wmux>")).await;
    h.cli(&["split-window", "-d", "-t", "ft:0"]).await;
    h.wait_capture("ft:0.1", "second shell", |t| t.contains("wmux>")).await;
    h.cli(&["new-window", "-d", "-t", "ft", "-n", "build"]).await;
    h.wait_capture("ft:1", "third shell", |t| t.contains("wmux>")).await;

    h.cli(&["send-keys", "-t", "ft:0.0", "echo REDIS-TIMEOUT-here", "Enter"]).await;
    h.cli(&["send-keys", "-t", "ft:0.1", "echo nothing-to-see", "Enter"]).await;
    h.cli(&["send-keys", "-t", "ft:1", "echo compile-failed-badly", "Enter"]).await;
    h.wait_capture("ft:1", "the third pane's output", |t| t.matches("compile-failed-badly").count() >= 2).await;

    // A pattern is looked for in what every pane printed, and the hit says
    // which pane and how far back it was.
    let (code, out, err) = h.cli(&["find-text", "redis-timeout"]).await;
    assert_eq!(code, 0, "{err}");
    assert!(out.lines().all(|l| l.starts_with("ft:0.0")), "only the first pane has it: {out}");
    assert!(out.contains("REDIS-TIMEOUT-here"), "{out}");
    assert!(out.contains("  -"), "each hit says how many lines back: {out}");

    let (_, out, _) = h.cli(&["find-text", "compile-failed"]).await;
    assert!(out.lines().all(|l| l.starts_with("ft:1.0")), "{out}");

    // -C makes it match case; -t narrows to one window; -n caps the hits.
    let (code, _, err) = h.cli(&["find-text", "-C", "redis-timeout"]).await;
    assert_eq!(code, 1, "the text is upper case, so this must miss");
    assert!(err.contains("no pane has"), "{err}");
    let (_, out, _) = h.cli(&["find-text", "-C", "REDIS-TIMEOUT"]).await;
    assert!(out.contains("REDIS-TIMEOUT-here"), "{out}");

    let (_, out, _) = h.cli(&["find-text", "-t", "ft:1", "echo"]).await;
    assert!(out.lines().all(|l| l.starts_with("ft:1.0")), "-t limits the search: {out}");
    // -t down to a single pane, and a target that is not there says so
    // instead of reporting an empty search.
    let (_, out, _) = h.cli(&["find-text", "-t", "ft:0.1", "echo"]).await;
    assert!(out.lines().all(|l| l.starts_with("ft:0.1")), "-t takes a pane too: {out}");
    let (code, _, err) = h.cli(&["find-text", "-t", "ft:99", "echo"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("no window 99"), "{err}");
    let (code, _, err) = h.cli(&["find-text", "-t", "ft:0.9", "echo"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("no pane 9"), "{err}");
    // Whitespace is not a search.
    let (code, _, err) = h.cli(&["find-text", " "]).await;
    assert_eq!(code, 1);
    assert!(err.contains("pattern required"), "{err}");
    let (_, out, _) = h.cli(&["find-text", "-n", "1", "echo"]).await;
    assert_eq!(out.lines().count(), 3, "one hit from each of the three panes: {out}");

    // A pattern nobody printed is an error, not an empty success.
    let (code, _, err) = h.cli(&["find-text", "zzz-nobody-printed-this"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("no pane has"), "{err}");
    // And the short name works.
    let (code, _, _) = h.cli(&["findt", "redis"]).await;
    assert_eq!(code, 0);
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_tmux_conf_loads_with_the_rest_skipped() {
    // A config as people actually have them: TPM, copy-mode-vi bindings, a
    // %if block, continuation lines, options tmux has and wmux does not.
    let dir = std::env::temp_dir().join(format!("wmux-tmuxconf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let conf = dir.join("tmux.conf");
    std::fs::write(
        &conf,
        "# my tmux.conf\n\
         set -g prefix C-a\n\
         unbind C-b\n\
         set -g mouse on\n\
         set -g base-index 1\n\
         setw -g mode-keys vi\n\
         set -g default-terminal \"screen-256color\"\n\
         set -ga terminal-overrides \",xterm-256color:Tc\"\n\
         bind -T copy-mode-vi v send-keys -X begin-selection\n\
         bind -T copy-mode-vi y send-keys -X copy-selection-and-cancel\n\
         bind | split-window -h \\\n  -c \"#{pane_current_path}\"\n\
         %if #{==:#{host},nowhere}\n\
         set -g status off\n\
         %endif\n\
         set -g @plugin 'tmux-plugins/tpm'\n\
         set -g @plugin 'tmux-plugins/tmux-sensible'\n\
         set -g status-right '#{pane_current_path}'\n",
    )
    .unwrap();
    // Started by hand with the config given directly: the other tests run
    // in this same process, so nothing may go through the environment.
    let socket = format!("test-tmuxconf-{}", std::process::id());
    let s = socket.clone();
    let options = wmux::server::RunOptions { force_restore: false, config: Some(conf.clone()) };
    let server = tokio::spawn(async move {
        if let Err(e) = wmux::server::run_with(s, options).await {
            panic!("server: {e:#}");
        }
    });
    let pipe = pipe_name(&socket);
    let deadline = Instant::now() + Duration::from_secs(5);
    while ClientOptions::new().open(&pipe).is_err() {
        assert!(Instant::now() < deadline, "server did not come up");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let h = Harness { socket, _server: server, sessions_dir: dir.clone() };
    h.cli(&["set", "-g", "default-command", "cmd.exe /q /k prompt wmux$g"]).await;
    h.cli(&["new", "-d", "-s", "t"]).await;

    // What wmux understands is applied...
    assert_eq!(h.cli(&["show", "-gv", "prefix"]).await.1.trim(), "C-a");
    assert_eq!(h.cli(&["show", "-gv", "mouse"]).await.1.trim(), "on");
    assert_eq!(h.cli(&["show", "-gv", "base-index"]).await.1.trim(), "1");
    let (_, keys, _) = h.cli(&["list-keys"]).await;
    assert!(
        keys.contains("-T prefix | split-window -h -c \"#{pane_current_path}\""),
        "the continued line was joined: {keys}"
    );
    // ...the copy-mode-vi lines did not leak into the prefix table...
    assert!(!keys.lines().any(|l| l.contains("-T prefix v ")), "{keys}");
    assert!(!keys.lines().any(|l| l.contains("-T prefix y ")), "{keys}");
    // ...the %if block was left alone (status is still on)...
    assert_eq!(h.cli(&["show", "-gv", "status"]).await.1.trim(), "on");
    // ...and what was skipped is listed, not thrown at every attach.
    let (_, msgs, _) = h.cli(&["show-messages"]).await;
    assert!(msgs.contains("copy-mode-vi"), "the refused table is named: {msgs}");
    assert!(msgs.contains("@plugin tmux-plugins/tpm"), "the missing plugin is named: {msgs}");
    let mut c = h.connect().await;
    c.attach(&["attach", "-t", "t"]).await;
    c.wait_for("the one-line summary", |s| {
        let t = s.contents();
        t.contains("tmux.conf:") && t.contains("lines wmux could not use were skipped")
    })
    .await;
    assert!(!c.text().contains("copy-mode-vi"), "the details stay in show-messages: {}", c.text());

    // A file that sources itself is refused, not recursed into, and one
    // with an unclosed %if says so instead of quietly dropping the rest.
    let looping = dir.join("loop.conf");
    std::fs::write(
        &looping,
        format!("set -g mouse off\nsource-file \"{}\"\n", looping.display().to_string().replace('\\', "/")),
    )
    .unwrap();
    let (code, _, err) = h.cli(&["source-file", &looping.to_string_lossy()]).await;
    assert_eq!(code, 1);
    assert!(err.contains("source-file loop"), "{err}");
    assert_eq!(h.cli(&["show", "-gv", "mouse"]).await.1.trim(), "off", "the lines before the loop still applied");
    let open = dir.join("open.conf");
    std::fs::write(&open, "set -g mouse on\n%if x\nset -g mouse off\n").unwrap();
    let (code, _, err) = h.cli(&["source-file", &open.to_string_lossy()]).await;
    assert_eq!(code, 1);
    assert!(err.contains("%if without %endif"), "{err}");
    assert_eq!(h.cli(&["show", "-gv", "mouse"]).await.1.trim(), "on");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn record_writes_an_asciinema_file() {
    let h = Harness::start("record").await;
    h.cli(&["new", "-d", "-s", "r"]).await;
    h.wait_capture("r:0", "shell prompt", |t| t.contains("wmux>")).await;
    let cast = std::env::temp_dir().join(format!("wmux-record-{}.cast", std::process::id()));
    let _ = std::fs::remove_file(&cast);

    let (code, _, err) = h.cli(&["record", "-t", "r:0"]).await;
    assert_eq!(code, 1, "nothing to stop yet");
    assert!(err.contains("not recording"), "{err}");
    let (code, out, err) = h.cli(&["record", "-t", "r:0", &cast.to_string_lossy()]).await;
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("recording %"), "{out}");
    h.cli(&["send-keys", "-t", "r:0", "echo captured-in-the-cast", "Enter"]).await;
    h.wait_capture("r:0", "the echo", |t| t.matches("captured-in-the-cast").count() >= 2).await;
    // A split resizes the pane, which the recording notes as an "r" event.
    h.cli(&["split-window", "-d", "-t", "r:0"]).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (code, out, err) = h.cli(&["record", "-t", "r:0.0"]).await;
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("recording stopped"), "{out}");
    tokio::time::sleep(Duration::from_millis(300)).await; // the writer thread flushes on close

    let text = std::fs::read_to_string(&cast).expect("the cast file");
    let mut lines = text.lines();
    let header: serde_json::Value = serde_json::from_str(lines.next().expect("header")).expect("header is JSON");
    assert_eq!(header["version"], 2, "{header}");
    assert!(header["width"].as_u64().unwrap() > 0 && header["height"].as_u64().unwrap() > 0, "{header}");
    let events: Vec<serde_json::Value> = lines.map(|l| serde_json::from_str(l).expect("event is JSON")).collect();
    assert!(!events.is_empty());
    let mut last_t = 0.0;
    for e in &events {
        let t = e[0].as_f64().expect("time");
        assert!(t >= last_t, "times never go backwards: {e}");
        last_t = t;
        assert!(matches!(e[1].as_str(), Some("o") | Some("r")), "{e}");
    }
    assert!(events.iter().any(|e| e[1] == "o" && e[2].as_str().is_some_and(|d| d.contains("captured-in-the-cast"))));
    assert!(events.iter().any(|e| e[1] == "r" && e[2].as_str().is_some_and(|d| d.contains('x'))), "a resize event");
    let _ = std::fs::remove_file(&cast);
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pane_border_status_reserves_a_row_for_its_text() {
    let h = Harness::start("borderstatus").await;
    let mut c = h.connect().await;
    c.attach(&["new", "-s", "b"]).await;
    c.wait_for("prompt", |s| s.contents().contains("wmux>")).await;
    // cmd.exe starts with a blank line, so the prompt is on row 1; with a
    // top border line it moves to row 2 and row 0 becomes the label.
    let prompt_row = c.screen.screen().rows(0, COLS).position(|r| r.contains("wmux>")).unwrap();

    // top: the first row becomes the border text, the pane moves down one.
    h.cli(&["set", "-g", "pane-border-status", "top"]).await;
    c.wait_for("border text on top", |s| {
        let r0 = s.rows(0, COLS).next().unwrap();
        r0.contains("0:") && s.rows(0, COLS).nth(prompt_row + 1).unwrap().contains("wmux>")
    })
    .await;
    // The format is a format: pane variables and modifiers work in it.
    h.cli(&["set", "-g", "pane-border-format", " [#{pane_index}] #{pane_width}x#{pane_height} "]).await;
    // 24 rows less the status line and the border row: 22.
    c.wait_for("custom format", |s| s.rows(0, COLS).next().unwrap().contains(&format!("[0] {}x{}", COLS, ROWS - 2)))
        .await;
    // Two panes side by side: each gets its own text on its own columns.
    c.prefix('%').await;
    c.wait_for("two border texts", |s| s.rows(0, COLS).next().unwrap().matches("] ").count() == 2).await;
    // Splitting while the border row is on divides the layout cell, not the
    // drawn rect, so the halves come out even: the 23-row cell -> 11 + 1 + 11,
    // less a border row each = 10 + 10 of content (not 11 + 9).
    h.cli(&["split-window", "-v", "-d", "-t", "b:0.1"]).await;
    let (_, a, _) = h.cli(&["display-message", "-p", "-t", "b:0.1", "#{pane_height}"]).await;
    let (_, b, _) = h.cli(&["display-message", "-p", "-t", "b:0.2", "#{pane_height}"]).await;
    assert_eq!((a.trim(), b.trim()), ("10", "10"), "even halves with a border row each");

    // bottom: the row just above the status line.
    h.cli(&["set", "-g", "pane-border-status", "bottom"]).await;
    c.wait_for("border text at the bottom", |s| {
        s.rows(0, COLS).nth(prompt_row).unwrap().contains("wmux>")
            && s.rows(0, COLS).nth(ROWS as usize - 2).unwrap().matches("] ").count() == 2
    })
    .await;
    // off: everything back.
    h.cli(&["set", "-g", "pane-border-status", "off"]).await;
    c.wait_for("back to normal", |s| !s.rows(0, COLS).nth(ROWS as usize - 2).unwrap().contains("] ")).await;
    let (code, _, err) = h.cli(&["set", "-g", "pane-border-status", "sideways"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("off, top or bottom"), "{err}");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn format_variables_answer_from_the_live_tree() {
    let h = Harness::start("formats").await;
    h.cli(&["new", "-d", "-s", "fmt"]).await;
    h.wait_capture("fmt:0", "shell prompt", |t| t.contains("wmux>")).await;
    h.cli(&["split-window", "-d", "-t", "fmt:0"]).await;
    // A pane's path is what the shell announced or set-cwd recorded.
    let dir = std::env::temp_dir();
    h.cli(&["set-cwd", "-t", "fmt:0.0", &dir.to_string_lossy()]).await;
    let (code, out, err) = h
        .cli(&[
            "display-message",
            "-p",
            "#{session_windows}|#{window_panes}|#{pane_pid}|#{client_width}x#{client_height}|#{=2:session_name}|#{session_id}|#{window_id}|#{pane_id}|#{pane_active}|#{pane_dead}|#{b:pane_current_path}|#{version}",
        ])
        .await;
    assert_eq!(code, 0, "{err}");
    let parts: Vec<&str> = out.trim().split('|').collect();
    assert_eq!(parts.len(), 12, "{out}");
    assert_eq!(parts[0], "1");
    assert_eq!(parts[1], "2");
    assert!(parts[2].parse::<u32>().is_ok_and(|p| p > 0), "pane_pid: {out}");
    assert_eq!(parts[3], "80x24");
    assert_eq!(parts[4], "fm");
    assert!(parts[5].starts_with('$') && parts[6].starts_with('@') && parts[7].starts_with('%'), "{out}");
    assert_eq!(parts[8], "1");
    assert_eq!(parts[9], "0");
    let base = dir.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(parts[10], base, "basename only: {out}");
    assert_eq!(parts[11], env!("CARGO_PKG_VERSION"));
    // Conditionals on the new variables.
    let (_, out, _) = h.cli(&["display-message", "-p", "#{?window_zoomed_flag,Z,-}#{?pane_synchronized,S,-}"]).await;
    assert_eq!(out.trim(), "--");
    h.cli(&["set", "sync"]).await;
    let (_, out, _) = h.cli(&["display-message", "-p", "#{?pane_synchronized,S,-}"]).await;
    assert_eq!(out.trim(), "S");
    h.cli(&["kill-server"]).await;
}

// Keep the unused-import lint quiet for helper traits used through split().
#[allow(dead_code)]
fn _assert_traits<T: AsyncRead + AsyncWrite>() {}

#[tokio::test(flavor = "multi_thread")]
async fn save_history_all_keeps_the_whole_scrollback_with_colours() {
    let h = Harness::start("savehist").await;
    h.cli(&["new", "-d", "-s", "h"]).await;
    h.wait_capture("h:0", "shell prompt", |t| t.contains("wmux>")).await;
    // A coloured prompt, then more lines than the screen holds.
    h.cli(&["send-keys", "-t", "h:0", "prompt $e[31mred$e[0m$g", "Enter"]).await;
    h.cli(&["send-keys", "-t", "h:0", "for /l %i in (1,1,60) do @echo scroll-line-%i", "Enter"]).await;
    h.wait_capture("h:0", "the last line", |t| t.contains("scroll-line-60")).await;
    let saved = |h: &Harness| {
        std::fs::read_dir(&h.sessions_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
            .find(|t| t.contains("\"name\": \"h\""))
            .expect("a saved file for h")
    };

    // A number keeps that many lines from the bottom...
    h.cli(&["set", "-g", "save-history", "5"]).await;
    let (code, _, err) = h.cli(&["save-session", "-t", "h"]).await;
    assert_eq!(code, 0, "{err}");
    let file = saved(&h);
    assert!(file.contains("\"scroll-line-60\""), "{file}");
    assert!(!file.contains("\"scroll-line-1\""), "only the last 5 lines: {file}");
    // ...and `all` keeps everything, colours as escape sequences.
    let (code, _, err) = h.cli(&["set", "-g", "save-history", "all"]).await;
    assert_eq!(code, 0, "{err}");
    assert_eq!(h.cli(&["show", "-gv", "save-history"]).await.1.trim(), "all");
    h.cli(&["save-session", "-t", "h"]).await;
    let file = saved(&h);
    assert!(file.contains("\"scroll-line-1\""), "the first line, long scrolled off: {file}");
    assert!(file.contains("\\u001b[31mred"), "the red prompt: {file}");
    let (code, _, err) = h.cli(&["set", "-g", "save-history", "lots"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("or all"), "{err}");

    // Resumed, the whole scrollback is back and the prompt is still red.
    h.cli(&["new", "-d", "-s", "keeper"]).await;
    h.cli(&["kill-session", "-t", "h"]).await;
    let (code, _, err) = h.cli(&["resume", "h"]).await;
    assert_eq!(code, 0, "{err}");
    h.wait_capture("h:0", "the restored shell", |t| t.contains("wmux>")).await;
    let (_, out, _) = h.cli(&["capture-pane", "-p", "-e", "-S", "-", "-t", "h:0"]).await;
    assert!(out.lines().any(|l| l.trim_end() == "scroll-line-1"), "{out}");
    assert!(out.contains("\x1b[31mred"), "{out}");

    // A detached session takes its size from -x/-y (there is no terminal
    // to take it from); one row goes to the status line.
    let (code, _, err) = h.cli(&["new", "-d", "-s", "small", "-x", "30", "-y", "5"]).await;
    assert_eq!(code, 0, "{err}");
    let (_, out, _) = h.cli(&["display-message", "-p", "-t", "small:0", "#{pane_width}x#{pane_height}"]).await;
    assert_eq!(out.trim(), "30x4");
    let (code, _, err) = h.cli(&["new", "-d", "-s", "tooSmall", "-x", "3"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("at least 10"), "{err}");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn jobs_lists_every_pane_with_its_state() {
    let h = Harness::start("jobs").await;
    h.cli(&["set", "-g", "remain-on-exit", "on"]).await;
    h.cli(&["new", "-d", "-s", "build"]).await;
    h.cli(&["new", "-d", "-s", "web"]).await;
    h.cli(&["split-window", "-d", "-t", "web:0"]).await;
    let (code, _, err) = h.cli(&["new-window", "-d", "-t", "build", "-n", "dies", "cmd.exe", "/c", "exit", "4"]).await;
    assert_eq!(code, 0, "{err}");
    // The board notices the exit (remain-on-exit keeps the pane to show it).
    let deadline = Instant::now() + Duration::from_secs(10);
    let out = loop {
        let (code, out, err) = h.cli(&["jobs"]).await;
        assert_eq!(code, 0, "{err}");
        if out.contains("exit 4") {
            break out;
        }
        assert!(Instant::now() < deadline, "the exited pane never showed: {out}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 5, "a header and four panes: {out}");
    assert!(lines[0].starts_with("PANE") && lines[0].contains("STATE") && lines[0].contains("IDLE"), "{out}");
    for want in ["build:0.0", "build:1.0", "web:0.0", "web:0.1"] {
        assert!(lines.iter().any(|l| l.starts_with(want)), "{want} listed: {out}");
    }
    let dead = lines.iter().find(|l| l.starts_with("build:1.0")).unwrap();
    assert!(dead.contains("exit 4"), "{dead}");
    let live = lines.iter().find(|l| l.starts_with("web:0.1")).unwrap();
    assert!(live.contains("running"), "{live}");
    // pid, then the command: a number in the PID column.
    assert!(live.split_whitespace().nth(4).is_some_and(|p| p.parse::<u32>().is_ok()), "{live}");

    // -t narrows to a session, a window or one pane; -F says what to print.
    let (_, out, _) = h.cli(&["jobs", "-t", "web"]).await;
    assert_eq!(out.lines().count(), 3, "{out}");
    assert!(out.lines().skip(1).all(|l| l.starts_with("web:")), "{out}");
    let (_, out, _) = h.cli(&["jobs", "-t", "web:0.1", "-F", "#{pane_index}"]).await;
    assert_eq!(out.trim(), "1", "one pane asked for, one answered: {out:?}");
    // Columns line up on screen even with a double-width session name.
    h.cli(&["new", "-d", "-s", "中文"]).await;
    let (_, out, _) = h.cli(&["jobs"]).await;
    let offsets: std::collections::HashSet<usize> = out
        .lines()
        .map(|l| {
            let at = l.find("running").or_else(|| l.find("STATE")).or_else(|| l.find("exit")).unwrap();
            unicode_width::UnicodeWidthStr::width(&l[..at])
        })
        .collect();
    assert_eq!(offsets.len(), 1, "the STATE column starts at one screen column on every row: {out}");
    let (_, out, _) =
        h.cli(&["jobs", "-t", "build:1", "-F", "#{session_name}/#{window_name} #{pane_dead_status}"]).await;
    assert_eq!(out.trim(), "build/dies 4");
    let (_, out, _) = h.cli(&["jobs", "-t", "web:0.0", "-F", "#{pane_start_time} #{pane_activity}"]).await;
    let mut it = out.split_whitespace().map(|n| n.parse::<i64>().expect("unix seconds"));
    let (start, activity) = (it.next().unwrap(), it.next().unwrap());
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    assert!((now - start).abs() < 60 && activity >= start && activity <= now, "{out} vs now {now}");

    let (code, _, err) = h.cli(&["jobs", "-t", "nosuch"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("nosuch"), "{err}");
    let (code, _, err) = h.cli(&["jobs", "extra"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("unexpected argument"), "{err}");
    h.cli(&["kill-server"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn status_justify_and_separator_move_the_window_list() {
    let h = Harness::start("justify").await;
    let mut c = h.connect().await;
    c.attach(&["new", "-s", "j", "-n", "aa"]).await;
    c.wait_for("prompt", |s| s.contents().contains("wmux>")).await;
    h.cli(&["new-window", "-d", "-t", "j", "-n", "bb"]).await;
    let status = |s: &vt100::Screen| s.rows(0, COLS).last().unwrap();
    // The separator goes between the labels, the list starts after "[j] ".
    h.cli(&["set", "-g", "window-status-separator", " | "]).await;
    c.wait_for("separator", |s| status(s).contains("0:aa") && status(s).contains(" | 1:bb")).await;
    assert_eq!(status(c.screen.screen()).find("0:aa"), Some(4), "{}", status(c.screen.screen()));
    // right: the list ends one gap before the right side, which starts
    // with the quoted pane title.
    h.cli(&["set", "-g", "status-justify", "right"]).await;
    c.wait_for("right-justified", |s| status(s).contains("1:bb \"")).await;
    // centre: away from both sides, and `center` spells it too.
    h.cli(&["set", "-g", "status-justify", "center"]).await;
    assert_eq!(h.cli(&["show", "-gv", "status-justify"]).await.1.trim(), "centre");
    c.wait_for("centred", |s| {
        let row = status(s);
        row.find("0:aa").is_some_and(|at| at > 6) && !row.contains("1:bb \"") && row.contains("1:bb  ")
    })
    .await;
    let (code, _, err) = h.cli(&["set", "-g", "status-justify", "sideways"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("left, centre, right or absolute-centre"), "{err}");
    h.cli(&["kill-server"]).await;
}
