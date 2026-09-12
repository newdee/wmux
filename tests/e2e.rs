//! End-to-end tests: run the server in-process, talk to it over the named
//! pipe exactly like the real client, and check the rendered frames.

use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite, ReadHalf, WriteHalf};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
use wmux::ipc::{ClientMsg, KeyRecord, MouseRecord, PROTOCOL_VERSION, ServerMsg, pipe_name, read_frame, write_frame};
use wmux::keys::{LEFT_CTRL_PRESSED, VK_RETURN};

const COLS: u16 = 80;
const ROWS: u16 = 24;

struct Harness {
    socket: String,
    _server: tokio::task::JoinHandle<()>,
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
        let h = Harness { socket, _server: server };
        // Make every implicitly spawned pane a predictable cmd.exe prompt.
        let (code, _, err) = h.cli(&["set", "-g", "default-command", "cmd.exe /q /k prompt wmux$g"]).await;
        assert_eq!(code, 0, "{err}");
        h
    }

    async fn connect(&self) -> Conn {
        let pipe = pipe_name(&self.socket);
        let c = loop {
            match ClientOptions::new().open(&pipe) {
                Ok(c) => break c,
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        };
        let (rd, wr) = tokio::io::split(c);
        Conn { rd, wr, screen: vt100::Parser::new(ROWS, COLS, 0) }
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
        let vk = if ch.is_ascii_alphabetic() { ch.to_ascii_uppercase() as u16 } else { 0 };
        self.key(vk, ch, 0).await;
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

// Keep the unused-import lint quiet for helper traits used through split().
#[allow(dead_code)]
fn _assert_traits<T: AsyncRead + AsyncWrite>() {}
