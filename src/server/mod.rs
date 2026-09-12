//! The wmux server: owns sessions, windows and panes; talks to clients over a
//! named pipe; renders frames.

pub mod input;
pub mod layout;
pub mod pane;
pub mod render;

use crate::command::{Cmd, Dir, PaneSel, Target};
use crate::config::{Options, resolve_shell};
use crate::ipc::{ClientMsg, KeyRecord, MouseRecord, PROTOCOL_VERSION, ServerMsg, read_frame, write_frame};
use crate::keys::{Key, KeyCode, key_from_record};
use anyhow::{Context, Result};
use input::MouseButton;
use layout::{Node, PaneId, Rect};
use pane::{CopyMode, Pane, PaneEvent};
use render::{CopyView, Frame, Grid, PaneView, StatusLine};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use tokio::net::windows::named_pipe::ServerOptions;
use tokio::sync::mpsc;

pub type ClientId = u32;
pub type SessionId = u32;
pub type WindowId = u32;

const MOUSE_MOVED: u32 = 0x0001;
const MOUSE_WHEELED: u32 = 0x0004;
const MOUSE_HWHEELED: u32 = 0x0008;
const BTN_LEFT: u32 = 0x0001;
const BTN_RIGHT: u32 = 0x0002;
const BTN_MIDDLE: u32 = 0x0004;

/// Frames queued per client before we stop diffing and fall back to a full
/// redraw once the client catches up (a slow console must not make the
/// server's memory grow without bound).
const OUTPUT_QUEUE: usize = 64;

enum Event {
    Pane(PaneEvent),
    Connected(ClientId, mpsc::Sender<ServerMsg>),
    Msg(ClientId, ClientMsg),
    Gone(ClientId),
    Tick,
}

enum PromptKind {
    /// Run the input as a command line, or substitute it into a template.
    Command {
        template: Option<String>,
    },
    Confirm(Cmd),
}

struct Prompt {
    kind: PromptKind,
    label: String,
    input: String,
    cursor: usize,
}

struct Drag {
    pane: PaneId,
    horizontal: bool,
    last: i32,
}

struct Client {
    id: ClientId,
    tx: mpsc::Sender<ServerMsg>,
    cols: u16,
    rows: u16,
    cwd: String,
    interactive: bool,
    pane_env: Option<u32>,
    session: Option<SessionId>,
    last_grid: Option<Grid>,
    last_cursor: Option<(u16, u16)>,
    prefix: bool,
    prompt: Option<Prompt>,
    message: Option<(String, Instant)>,
    /// Multi-line command output shown over the window until a key is pressed
    /// (tmux "view mode"), e.g. `list-keys`.
    overlay: Option<Vec<String>>,
    mouse_buttons: u32,
    drag: Option<Drag>,
    /// Virtual keys whose key-down we consumed; drop the matching key-up.
    swallow_up: HashSet<u16>,
    /// Control messages that did not fit in the output queue yet.
    pending: std::collections::VecDeque<ServerMsg>,
}

impl Client {
    /// Queue a control message. Control messages are never dropped: when the
    /// queue is full (a console frozen by a QuickEdit selection blocks our
    /// writer) they wait in `pending` and go out before any further frame.
    fn send(&mut self, m: ServerMsg) {
        if !self.pending.is_empty() {
            self.pending.push_back(m);
            return;
        }
        if let Err(mpsc::error::TrySendError::Full(m)) = self.tx.try_send(m) {
            self.pending.push_back(m);
        }
    }

    /// Queue a screen update. When the client lags, the frame is dropped and
    /// the next render is forced to be a full redraw, which supersedes it.
    fn send_output(&mut self, bytes: Vec<u8>) {
        if !self.pending.is_empty() || self.tx.try_send(ServerMsg::Output(bytes)).is_err() {
            self.last_grid = None;
            self.last_cursor = None;
        }
    }

    /// Retry queued control messages; returns true when none are left.
    fn flush_pending(&mut self) -> bool {
        while let Some(m) = self.pending.pop_front() {
            if let Err(mpsc::error::TrySendError::Full(m)) = self.tx.try_send(m) {
                self.pending.push_front(m);
                return false;
            }
        }
        true
    }
}

struct Window {
    id: WindowId,
    name: String,
    layout: Node,
    panes: Vec<Pane>,
    active: PaneId,
    last_pane: Option<PaneId>,
    zoomed: bool,
    rects: Vec<(PaneId, Rect)>,
}

impl Window {
    fn pane(&self, id: PaneId) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == id)
    }
    fn pane_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|p| p.id == id)
    }
    fn active_pane(&self) -> Option<&Pane> {
        self.pane(self.active)
    }
    fn active_pane_mut(&mut self) -> Option<&mut Pane> {
        let id = self.active;
        self.pane_mut(id)
    }
    fn rect_of(&self, id: PaneId) -> Option<Rect> {
        self.rects.iter().find(|(i, _)| *i == id).map(|(_, r)| *r)
    }
    fn pane_at(&self, x: u16, y: u16) -> Option<PaneId> {
        self.rects.iter().find(|(_, r)| r.contains(x, y)).map(|(id, _)| *id)
    }

    /// Recompute pane rectangles for a window area and resize the panes.
    fn relayout(&mut self, area: Rect) {
        self.rects.clear();
        if self.zoomed && self.pane(self.active).is_some() {
            self.rects.push((self.active, area));
        } else {
            self.zoomed = false;
            self.layout.layout(area, &mut self.rects);
        }
        for (id, r) in self.rects.clone() {
            if let Some(p) = self.pane_mut(id) {
                p.resize(r.w, r.h);
            }
        }
    }
}

struct Session {
    id: SessionId,
    name: String,
    windows: Vec<Window>,
    cur: usize,
    last: Option<WindowId>,
    cols: u16,
    rows: u16,
    created: Instant,
    last_used: Instant,
}

impl Session {
    fn window(&self) -> Option<&Window> {
        self.windows.get(self.cur)
    }
    fn window_mut(&mut self) -> Option<&mut Window> {
        self.windows.get_mut(self.cur)
    }
    fn select_window(&mut self, idx: usize) {
        if idx < self.windows.len() && idx != self.cur {
            self.last = self.windows.get(self.cur).map(|w| w.id);
            self.cur = idx;
        }
    }
}

enum Outcome {
    Ok,
    Text(String),
    Attach(SessionId),
    Error(String),
}

impl From<Result<(), String>> for Outcome {
    fn from(r: Result<(), String>) -> Outcome {
        match r {
            Ok(()) => Outcome::Ok,
            Err(e) => Outcome::Error(e),
        }
    }
}

pub struct Server {
    opts: Options,
    prefix_binds: HashMap<Key, Cmd>,
    root_binds: HashMap<Key, Cmd>,
    sessions: Vec<Session>,
    clients: HashMap<ClientId, Client>,
    next_id: u32,
    pane_tx: std::sync::mpsc::Sender<PaneEvent>,
    socket: String,
    had_session: bool,
    started: Instant,
    quit: bool,
    /// Errors from the config file, pending display on the first attach.
    config_errors: Option<Vec<String>>,
}

pub async fn run(socket: String) -> Result<()> {
    let pipe = crate::ipc::pipe_name(&socket);
    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    let (pane_tx, pane_rx) = std::sync::mpsc::channel::<PaneEvent>();
    {
        let tx = tx.clone();
        std::thread::Builder::new()
            .name("pane-events".into())
            .spawn(move || {
                while let Ok(ev) = pane_rx.recv() {
                    if tx.send(Event::Pane(ev)).is_err() {
                        break;
                    }
                }
            })
            .context("spawn pane event bridge")?;
    }

    // Only this user (and SYSTEM) may open the pipe: tmux's 0700 socket.
    let mut sec = crate::winsec::OwnerOnly::new().context("pipe security descriptor")?;
    // Listener: the first instance must be created before we are considered up.
    let first = unsafe {
        ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(&pipe, sec.as_ptr() as *mut _)
    }
    .with_context(|| format!("create pipe {pipe} (server already running?)"))?;
    log::info!("listening on {pipe}");
    let listener = {
        let tx = tx.clone();
        let pipe = pipe.clone();
        tokio::spawn(async move {
            let mut server = first;
            let mut next_client: ClientId = 1;
            loop {
                if let Err(e) = server.connect().await {
                    log::error!("pipe connect: {e}");
                    break;
                }
                let conn = server;
                server = match unsafe {
                    ServerOptions::new()
                        .reject_remote_clients(true)
                        .create_with_security_attributes_raw(&pipe, sec.as_ptr() as *mut _)
                } {
                    Ok(s) => s,
                    Err(e) => {
                        log::error!("create next pipe instance: {e}");
                        break;
                    }
                };
                let id = next_client;
                next_client += 1;
                let tx = tx.clone();
                tokio::spawn(async move {
                    let (mut rd, mut wr) = tokio::io::split(conn);
                    let (out_tx, mut out_rx) = mpsc::channel::<ServerMsg>(OUTPUT_QUEUE);
                    let _ = tx.send(Event::Connected(id, out_tx));
                    let writer = tokio::spawn(async move {
                        while let Some(m) = out_rx.recv().await {
                            if write_frame(&mut wr, &m).await.is_err() {
                                break;
                            }
                        }
                    });
                    loop {
                        match read_frame::<_, ClientMsg>(&mut rd).await {
                            Ok(Some(m)) => {
                                if tx.send(Event::Msg(id, m)).is_err() {
                                    break;
                                }
                            }
                            Ok(None) => break,
                            Err(e) => {
                                log::warn!("client {id}: {e}");
                                break;
                            }
                        }
                    }
                    let _ = tx.send(Event::Gone(id));
                    writer.abort();
                });
            }
        })
    };

    let mut srv = Server::new(pane_tx, socket);
    srv.load_config();
    loop {
        let ev = tokio::select! {
            ev = rx.recv() => match ev { Some(ev) => ev, None => break },
            _ = tokio::time::sleep(Duration::from_millis(1000)) => Event::Tick,
        };
        srv.handle(ev);
        while let Ok(ev) = rx.try_recv() {
            srv.handle(ev);
        }
        srv.render_all();
        if srv.quit {
            break;
        }
    }
    // Stop accepting (releases the pipe name), let client writers flush.
    listener.abort();
    drop(srv);
    tokio::time::sleep(Duration::from_millis(150)).await;
    log::info!("server exiting");
    Ok(())
}

fn default_bindings() -> HashMap<Key, Cmd> {
    let mut m = HashMap::new();
    let lines = [
        ("c", "new-window"),
        (",", "command-prompt -p (rename-window) -I \"#W\" \"rename-window -- %%\""),
        ("$", "command-prompt -p (rename-session) -I \"#S\" \"rename-session -- %%\""),
        ("&", "confirm-before -p \"kill-window #W? (y/n)\" kill-window"),
        ("x", "confirm-before -p \"kill-pane #P? (y/n)\" kill-pane"),
        ("d", "detach-client"),
        ("%", "split-window -h"),
        ("\"", "split-window -v"),
        ("o", "select-pane -t next"),
        (";", "select-pane -l"),
        ("Up", "select-pane -U"),
        ("Down", "select-pane -D"),
        ("Left", "select-pane -L"),
        ("Right", "select-pane -R"),
        ("C-Up", "resize-pane -U 1"),
        ("C-Down", "resize-pane -D 1"),
        ("C-Left", "resize-pane -L 1"),
        ("C-Right", "resize-pane -R 1"),
        ("M-Up", "resize-pane -U 5"),
        ("M-Down", "resize-pane -D 5"),
        ("M-Left", "resize-pane -L 5"),
        ("M-Right", "resize-pane -R 5"),
        ("n", "next-window"),
        ("p", "previous-window"),
        ("l", "last-window"),
        ("z", "resize-pane -Z"),
        ("[", "copy-mode"),
        ("PPage", "copy-mode -u"),
        ("]", "paste-buffer"),
        (":", "command-prompt"),
        ("?", "list-keys"),
        ("{", "swap-pane -U"),
        ("}", "swap-pane -D"),
        ("!", "break-pane"),
        ("(", "switch-client -p"),
        (")", "switch-client -n"),
        ("w", "list-windows"),
        ("s", "list-sessions"),
        ("i", "display-message \"#S:#W.#P #T\""),
    ];
    for (k, l) in lines {
        let key = Key::parse(k).expect(k);
        let cmd = crate::command::parse_line(l).expect(l).expect(l);
        m.insert(key, cmd);
    }
    for d in 0..10u8 {
        m.insert(Key::ch((b'0' + d) as char), Cmd::SelectWindow { target: Target::parse(&format!(":{d}")) });
    }
    m
}

impl Server {
    pub fn new(pane_tx: std::sync::mpsc::Sender<PaneEvent>, socket: String) -> Server {
        Server {
            opts: Options::default(),
            prefix_binds: default_bindings(),
            root_binds: HashMap::new(),
            sessions: Vec::new(),
            clients: HashMap::new(),
            next_id: 1,
            pane_tx,
            socket,
            had_session: false,
            started: Instant::now(),
            quit: false,
            config_errors: None,
        }
    }

    fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn load_config(&mut self) {
        if let Some(path) = crate::config::find_config() {
            match self.source_file(&path.to_string_lossy()) {
                Ok(n) => log::info!("loaded {} ({n} commands)", path.display()),
                Err(e) => {
                    log::error!("config {}: {e}", path.display());
                    // Shown to the first client that attaches, like tmux does.
                    self.config_errors = Some(e.lines().map(str::to_string).collect());
                }
            }
        }
    }

    fn source_file(&mut self, path: &str) -> Result<usize, String> {
        let path = &expand_home(path);
        let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        let mut n = 0;
        let mut errors = Vec::new();
        for (i, line) in text.lines().enumerate() {
            match crate::command::parse_line(line) {
                Ok(None) => {}
                Ok(Some(cmd)) => {
                    n += 1;
                    if let Outcome::Error(e) = self.exec(cmd, None) {
                        errors.push(format!("{path}:{}: {e}", i + 1));
                    }
                }
                Err(e) => errors.push(format!("{path}:{}: {e}", i + 1)),
            }
        }
        if errors.is_empty() { Ok(n) } else { Err(errors.join("\n")) }
    }

    // ----------------------------------------------------------------- events

    fn handle(&mut self, ev: Event) {
        match ev {
            Event::Pane(PaneEvent::Output(id, bytes)) => {
                if let Some(p) = self.find_pane_mut(id) {
                    p.process_output(&bytes);
                }
            }
            Event::Pane(PaneEvent::Exit(id, code)) => {
                log::info!("pane %{id} exited with {code}");
                self.remove_pane(id, Some(code));
            }
            Event::Connected(id, tx) => {
                self.clients.insert(
                    id,
                    Client {
                        id,
                        tx,
                        cols: 80,
                        rows: 24,
                        cwd: String::new(),
                        interactive: false,
                        pane_env: None,
                        session: None,
                        last_grid: None,
                        last_cursor: None,
                        prefix: false,
                        prompt: None,
                        message: None,
                        overlay: None,
                        mouse_buttons: 0,
                        drag: None,
                        swallow_up: HashSet::new(),
                        pending: std::collections::VecDeque::new(),
                    },
                );
            }
            Event::Gone(id) => {
                self.clients.remove(&id);
            }
            Event::Msg(id, msg) => self.handle_msg(id, msg),
            Event::Tick => {
                // A server nobody uses has no reason to live (e.g. started by
                // `wmux attach` when there was nothing to attach to).
                if self.sessions.is_empty()
                    && self.clients.is_empty()
                    && self.started.elapsed() > Duration::from_secs(10)
                {
                    log::info!("idle with no sessions; exiting");
                    self.quit = true;
                }
            }
        }
        if self.had_session && self.sessions.is_empty() {
            self.quit = true;
        }
    }

    fn handle_msg(&mut self, cid: ClientId, msg: ClientMsg) {
        match msg {
            ClientMsg::Command { version, argv, cwd, cols, rows, interactive, pane_env } => {
                if version != PROTOCOL_VERSION {
                    if let Some(c) = self.clients.get_mut(&cid) {
                        c.send(ServerMsg::Error(format!(
                            "protocol mismatch: client {version}, server {PROTOCOL_VERSION}"
                        )));
                        c.send(ServerMsg::Done { code: 1 });
                    }
                    return;
                }
                if let Some(c) = self.clients.get_mut(&cid) {
                    c.cols = cols.max(1);
                    c.rows = rows.max(1);
                    c.cwd = cwd;
                    c.interactive = interactive;
                    c.pane_env = pane_env;
                }
                let cmd = match crate::command::parse(&argv) {
                    Ok(c) => c,
                    Err(e) => {
                        self.reply(cid, Outcome::Error(e));
                        return;
                    }
                };
                let out = self.exec(cmd, Some(cid));
                self.reply(cid, out);
            }
            ClientMsg::Key(rec) => self.handle_key(cid, rec),
            ClientMsg::Mouse(rec) => self.handle_mouse(cid, rec),
            ClientMsg::Resize { cols, rows } => {
                let Some(c) = self.clients.get_mut(&cid) else { return };
                c.cols = cols.max(1);
                c.rows = rows.max(1);
                c.last_grid = None;
                if let Some(sid) = c.session {
                    self.resize_session(sid, cols, rows);
                }
            }
            ClientMsg::Detach => self.detach(cid, "detached"),
        }
    }

    /// Deliver a command outcome to a CLI client (or attach it).
    fn reply(&mut self, cid: ClientId, out: Outcome) {
        let Some(c) = self.clients.get_mut(&cid) else { return };
        match out {
            Outcome::Ok => {
                if c.session.is_some() {
                    return; // attached client running a prompt command
                }
                c.send(ServerMsg::Done { code: 0 });
            }
            Outcome::Text(t) => {
                if c.session.is_some() {
                    self.show(cid, &t);
                    return;
                }
                c.send(ServerMsg::Text(t));
                c.send(ServerMsg::Done { code: 0 });
            }
            Outcome::Error(e) => {
                if c.session.is_some() {
                    self.show(cid, &e);
                    return;
                }
                c.send(ServerMsg::Error(e));
                c.send(ServerMsg::Done { code: 1 });
            }
            Outcome::Attach(sid) => {
                let (cols, rows) = (c.cols, c.rows);
                let name = self.sessions.iter().find(|s| s.id == sid).map(|s| s.name.clone()).unwrap_or_default();
                let mouse = self.opts.mouse;
                let config_errors = self.config_errors.take();
                let c = self.clients.get_mut(&cid).unwrap();
                c.session = Some(sid);
                c.last_grid = None;
                c.last_cursor = None;
                c.prefix = false;
                c.prompt = None;
                c.overlay = config_errors;
                c.message = None;
                c.drag = None;
                c.mouse_buttons = 0;
                c.swallow_up.clear();
                c.send(ServerMsg::Attached { session: name });
                c.send(ServerMsg::SetMouse(mouse));
                self.resize_session(sid, cols, rows);
                if let Some(s) = self.session_mut(sid) {
                    s.last_used = Instant::now();
                }
            }
        }
    }

    /// One-line text goes to the status line for `display-time`; multi-line
    /// text becomes an overlay that stays until a key is pressed.
    fn show(&mut self, cid: ClientId, text: &str) {
        if let Some(c) = self.clients.get_mut(&cid) {
            if text.contains('\n') {
                c.overlay = Some(text.lines().map(str::to_string).collect());
            } else {
                c.message = Some((text.replace('\r', " "), Instant::now()));
            }
        }
    }

    fn message(&mut self, cid: ClientId, text: &str) {
        self.show(cid, text);
    }

    fn detach(&mut self, cid: ClientId, reason: &str) {
        if let Some(c) = self.clients.get_mut(&cid)
            && c.session.take().is_some()
        {
            c.last_grid = None;
            c.send(ServerMsg::Detached { reason: reason.to_string() });
        }
    }

    // ---------------------------------------------------------------- lookup

    fn session(&self, id: SessionId) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }
    fn session_mut(&mut self, id: SessionId) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    fn find_pane_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        self.sessions.iter_mut().flat_map(|s| s.windows.iter_mut()).find_map(|w| w.pane_mut(id))
    }

    fn session_of_pane(&self, id: PaneId) -> Option<SessionId> {
        self.sessions.iter().find(|s| s.windows.iter().any(|w| w.pane(id).is_some())).map(|s| s.id)
    }

    /// Session for a command: explicit target, else the client's attached
    /// session, else the session of the pane the client runs in, else the
    /// most recently used.
    fn resolve_session(&self, target: Option<&Target>, cid: Option<ClientId>) -> Result<SessionId, String> {
        if let Some(t) = target
            && let Some(name) = &t.session
        {
            return self
                .sessions
                .iter()
                .find(|s| s.name == *name || name.strip_prefix('$').and_then(|n| n.parse::<u32>().ok()) == Some(s.id))
                .map(|s| s.id)
                .or_else(|| {
                    // Prefix match like tmux.
                    let m: Vec<_> = self.sessions.iter().filter(|s| s.name.starts_with(name.as_str())).collect();
                    if m.len() == 1 { Some(m[0].id) } else { None }
                })
                .ok_or_else(|| format!("can't find session: {name}"));
        }
        if let Some(c) = cid.and_then(|c| self.clients.get(&c)) {
            if let Some(s) = c.session {
                return Ok(s);
            }
            if let Some(p) = c.pane_env
                && let Some(s) = self.session_of_pane(p)
            {
                return Ok(s);
            }
        }
        self.sessions.iter().max_by_key(|s| s.last_used).map(|s| s.id).ok_or_else(|| "no sessions".to_string())
    }

    fn resolve_window(&self, sid: SessionId, target: Option<&Target>) -> Result<usize, String> {
        let s = self.session(sid).ok_or("no such session")?;
        let Some(w) = target.and_then(|t| t.window.as_ref()) else { return Ok(s.cur) };
        if let Ok(n) = w.parse::<usize>() {
            let idx = n.wrapping_sub(self.opts.base_index);
            if idx < s.windows.len() {
                return Ok(idx);
            }
            return Err(format!("no window {n}"));
        }
        match w.as_str() {
            "+" | "next" => return Ok((s.cur + 1) % s.windows.len().max(1)),
            "-" | "prev" => return Ok((s.cur + s.windows.len().max(1) - 1) % s.windows.len().max(1)),
            "!" | "last" => {
                return s
                    .last
                    .and_then(|id| s.windows.iter().position(|x| x.id == id))
                    .ok_or_else(|| "no last window".into());
            }
            _ => {}
        }
        s.windows
            .iter()
            .position(|x| x.name == *w || w.strip_prefix('@').and_then(|n| n.parse::<u32>().ok()) == Some(x.id))
            .ok_or_else(|| format!("can't find window: {w}"))
    }

    fn resolve_pane(&self, sid: SessionId, widx: usize, target: Option<&Target>) -> Result<PaneId, String> {
        let s = self.session(sid).ok_or("no such session")?;
        let w = s.windows.get(widx).ok_or("no such window")?;
        match target.and_then(|t| t.pane) {
            None => Ok(w.active),
            Some(i) => w.layout.panes().get(i).copied().ok_or_else(|| format!("no pane {i}")),
        }
    }

    /// (session id, window index, pane id) for a command context.
    fn resolve(&self, target: Option<&Target>, cid: Option<ClientId>) -> Result<(SessionId, usize, PaneId), String> {
        let sid = self.resolve_session(target, cid)?;
        let widx = self.resolve_window(sid, target)?;
        let pid = self.resolve_pane(sid, widx, target)?;
        Ok((sid, widx, pid))
    }

    fn window_area(&self, cols: u16, rows: u16) -> Rect {
        if self.opts.status && rows > 1 {
            Rect { x: 0, y: if self.opts.status_top { 1 } else { 0 }, w: cols, h: rows - 1 }
        } else {
            Rect { x: 0, y: 0, w: cols, h: rows }
        }
    }

    fn resize_session(&mut self, sid: SessionId, cols: u16, rows: u16) {
        let area = self.window_area(cols.max(1), rows.max(1));
        if let Some(s) = self.session_mut(sid) {
            s.cols = cols.max(1);
            s.rows = rows.max(1);
            for w in &mut s.windows {
                w.relayout(area);
            }
        }
    }

    fn relayout_session(&mut self, sid: SessionId) {
        if let Some(s) = self.session(sid) {
            let (c, r) = (s.cols, s.rows);
            self.resize_session(sid, c, r);
        }
    }

    // ----------------------------------------------------------------- panes

    fn pane_env(&self, pane_id: PaneId) -> Vec<(String, String)> {
        vec![("WMUX".into(), self.socket.clone()), ("WMUX_PANE".into(), pane_id.to_string())]
    }

    fn spawn_pane(&mut self, argv: &[String], cwd: Option<&str>, cols: u16, rows: u16) -> Result<Pane, String> {
        let id = self.alloc_id();
        let argv = if argv.is_empty() { resolve_shell(&self.opts) } else { argv.to_vec() };
        let env = self.pane_env(id);
        Pane::spawn(id, &argv, cwd, cols, rows, self.opts.history_limit, &env, self.pane_tx.clone())
            .map_err(|e| format!("{e:#}"))
    }

    /// Remove a pane (dead or killed) and collapse empty windows/sessions.
    fn remove_pane(&mut self, id: PaneId, exit_code: Option<u32>) {
        let Some(sid) = self.session_of_pane(id) else { return };
        let mut session_dead = false;
        {
            let s = self.session_mut(sid).unwrap();
            let widx = s.windows.iter().position(|w| w.pane(id).is_some()).unwrap();
            let w = &mut s.windows[widx];
            if let Some(p) = w.pane_mut(id) {
                p.exit_code = exit_code.or(Some(0));
            }
            w.layout.remove(id);
            w.panes.retain(|p| p.id != id);
            if w.active == id {
                w.active = w
                    .last_pane
                    .filter(|l| w.pane(*l).is_some())
                    .or_else(|| w.layout.panes().first().copied())
                    .unwrap_or(0);
                w.zoomed = false;
            }
            if w.last_pane == Some(id) {
                w.last_pane = None;
            }
            if w.panes.is_empty() {
                let wid = w.id;
                s.windows.remove(widx);
                if s.last == Some(wid) {
                    s.last = None;
                }
                if s.windows.is_empty() {
                    session_dead = true;
                } else if let Some(last) = s.last.and_then(|l| s.windows.iter().position(|w| w.id == l)) {
                    s.cur = last;
                    s.last = None;
                } else if s.cur >= s.windows.len() || s.cur > widx {
                    s.cur = s.cur.saturating_sub(1).min(s.windows.len() - 1);
                } else {
                    s.cur = s.cur.min(s.windows.len() - 1);
                }
            }
        }
        if session_dead {
            self.kill_session(sid, "exited");
        } else {
            self.relayout_session(sid);
        }
    }

    fn kill_session(&mut self, sid: SessionId, reason: &str) {
        let Some(pos) = self.sessions.iter().position(|s| s.id == sid) else { return };
        let mut s = self.sessions.remove(pos);
        for w in &mut s.windows {
            for p in &mut w.panes {
                p.kill();
            }
        }
        let ids: Vec<ClientId> = self.clients.values().filter(|c| c.session == Some(sid)).map(|c| c.id).collect();
        for cid in ids {
            self.detach(cid, reason);
        }
        log::info!("session {} killed ({reason})", s.name);
    }

    fn new_window(
        &mut self,
        sid: SessionId,
        name: Option<String>,
        cwd: Option<&str>,
        argv: &[String],
    ) -> Result<usize, String> {
        let (cols, rows) = self.session(sid).map(|s| (s.cols, s.rows)).ok_or("no such session")?;
        let area = self.window_area(cols, rows);
        let pane = self.spawn_pane(argv, cwd, area.w, area.h)?;
        let wid = self.alloc_id();
        let name = name.unwrap_or_else(|| pane.command.clone());
        let mut w = Window {
            id: wid,
            name,
            layout: Node::Leaf(pane.id),
            active: pane.id,
            panes: vec![pane],
            last_pane: None,
            zoomed: false,
            rects: Vec::new(),
        };
        w.relayout(area);
        let s = self.session_mut(sid).unwrap();
        s.windows.push(w);
        let idx = s.windows.len() - 1;
        s.select_window(idx);
        Ok(idx)
    }

    fn new_session(
        &mut self,
        name: Option<String>,
        window_name: Option<String>,
        cwd: Option<&str>,
        argv: &[String],
        cols: u16,
        rows: u16,
    ) -> Result<SessionId, String> {
        let name = match name {
            Some(n) => {
                if n.contains(':') || n.contains('.') || n.is_empty() {
                    return Err(format!("bad session name: {n}"));
                }
                if self.sessions.iter().any(|s| s.name == n) {
                    return Err(format!("duplicate session: {n}"));
                }
                n
            }
            None => {
                let mut i = 0;
                loop {
                    let n = i.to_string();
                    if !self.sessions.iter().any(|s| s.name == n) {
                        break n;
                    }
                    i += 1;
                }
            }
        };
        let sid = self.alloc_id();
        self.sessions.push(Session {
            id: sid,
            name,
            windows: Vec::new(),
            cur: 0,
            last: None,
            cols: cols.max(1),
            rows: rows.max(1),
            created: Instant::now(),
            last_used: Instant::now(),
        });
        self.had_session = true;
        if let Err(e) = self.new_window(sid, window_name, cwd, argv) {
            self.sessions.retain(|s| s.id != sid);
            return Err(e);
        }
        Ok(sid)
    }

    fn client_cwd(&self, cid: Option<ClientId>) -> Option<String> {
        cid.and_then(|c| self.clients.get(&c)).map(|c| c.cwd.clone()).filter(|s| !s.is_empty())
    }

    /// Working directory for a new pane: explicit, else the active pane's
    /// starting directory, else the client's.
    fn pane_cwd(&self, explicit: Option<&str>, sid: SessionId, cid: Option<ClientId>) -> Option<String> {
        if let Some(c) = explicit {
            return Some(c.to_string());
        }
        if let Some(c) = self.client_cwd(cid) {
            return Some(c);
        }
        self.session(sid).and_then(|s| s.window()).and_then(|w| w.active_pane()).and_then(|p| p.cwd.clone())
    }

    // -------------------------------------------------------------- commands

    fn exec(&mut self, cmd: Cmd, cid: Option<ClientId>) -> Outcome {
        match cmd {
            Cmd::Version => Outcome::Text(format!("wmux {}", env!("CARGO_PKG_VERSION"))),
            Cmd::NewSession { name, window_name, cwd, detached, argv, attach_existing } => {
                let client = cid.and_then(|c| self.clients.get(&c));
                let interactive = client.is_some_and(|c| c.interactive);
                let (cols, rows) = client.map(|c| (c.cols, c.rows)).unwrap_or((80, 24));
                let inside = client.is_some_and(|c| c.pane_env.is_some() && c.session.is_none());
                if inside && !detached && interactive {
                    return Outcome::Error("sessions should be nested with care, unset WMUX to force".into());
                }
                if attach_existing
                    && let Some(n) = &name
                    && self.sessions.iter().any(|s| s.name == *n)
                {
                    if detached || !interactive {
                        return Outcome::Ok;
                    }
                    return self.exec(Cmd::AttachSession { target: Some(Target::parse(n)), detach_others: false }, cid);
                }
                let cwd = cwd.or_else(|| self.client_cwd(cid));
                match self.new_session(name, window_name, cwd.as_deref(), &argv, cols, rows) {
                    Ok(sid) => {
                        if detached || !interactive {
                            Outcome::Ok
                        } else {
                            Outcome::Attach(sid)
                        }
                    }
                    Err(e) => Outcome::Error(e),
                }
            }
            Cmd::AttachSession { target, detach_others } => {
                let Some(cid) = cid else { return Outcome::Error("attach-session: no client".into()) };
                let sid = match self.resolve_session(target.as_ref(), Some(cid)) {
                    Ok(s) => s,
                    Err(e) => return Outcome::Error(e),
                };
                if detach_others {
                    let others: Vec<ClientId> =
                        self.clients.values().filter(|c| c.session == Some(sid) && c.id != cid).map(|c| c.id).collect();
                    for o in others {
                        self.detach(o, "detached (attach -d)");
                    }
                }
                let c = self.clients.get(&cid).unwrap();
                if !c.interactive {
                    return Outcome::Error("not a terminal".into());
                }
                if c.session.is_some() {
                    // Already attached: switch.
                    return self.exec(
                        Cmd::SwitchClient {
                            next: false,
                            prev: false,
                            target: Some(Target {
                                session: self.session(sid).map(|s| s.name.clone()),
                                ..Default::default()
                            }),
                        },
                        Some(cid),
                    );
                }
                Outcome::Attach(sid)
            }
            Cmd::DetachClient => {
                if let Some(cid) = cid {
                    self.detach(cid, "detached");
                }
                Outcome::Ok
            }
            Cmd::ListSessions => {
                let mut lines = Vec::new();
                for s in &self.sessions {
                    let attached = self.clients.values().filter(|c| c.session == Some(s.id)).count();
                    let ago = s.created.elapsed().as_secs();
                    lines.push(format!(
                        "{}: {} windows (created {}s ago) [{}x{}]{}",
                        s.name,
                        s.windows.len(),
                        ago,
                        s.cols,
                        s.rows,
                        if attached > 0 { " (attached)" } else { "" }
                    ));
                }
                if lines.is_empty() {
                    return Outcome::Error("no sessions".into());
                }
                Outcome::Text(lines.join("\n"))
            }
            Cmd::ListWindows { target } => {
                let sid = match self.resolve_session(target.as_ref(), cid) {
                    Ok(s) => s,
                    Err(e) => return Outcome::Error(e),
                };
                let s = self.session(sid).unwrap();
                let lines: Vec<String> = s
                    .windows
                    .iter()
                    .enumerate()
                    .map(|(i, w)| {
                        format!(
                            "{}: {}{} ({} panes) [{}x{}]",
                            i + self.opts.base_index,
                            w.name,
                            if i == s.cur {
                                "*"
                            } else if Some(w.id) == s.last {
                                "-"
                            } else {
                                ""
                            },
                            w.panes.len(),
                            s.cols,
                            s.rows
                        )
                    })
                    .collect();
                Outcome::Text(lines.join("\n"))
            }
            Cmd::ListPanes { target } => {
                let (sid, widx, _) = match self.resolve(target.as_ref(), cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                let w = &self.session(sid).unwrap().windows[widx];
                let lines: Vec<String> = w
                    .layout
                    .panes()
                    .iter()
                    .enumerate()
                    .map(|(i, id)| {
                        let r = w.rect_of(*id).unwrap_or_default();
                        let p = w.pane(*id);
                        format!(
                            "{i}: [{}x{}] %{id} {}{}",
                            r.w,
                            r.h,
                            p.map(|p| p.display_title()).unwrap_or(""),
                            if *id == w.active { " (active)" } else { "" }
                        )
                    })
                    .collect();
                Outcome::Text(lines.join("\n"))
            }
            Cmd::HasSession { target } => match self.resolve_session(Some(&target), cid) {
                Ok(_) => Outcome::Ok,
                Err(e) => Outcome::Error(e),
            },
            Cmd::KillSession { target, all_but } => match self.resolve_session(target.as_ref(), cid) {
                Ok(sid) => {
                    let victims: Vec<SessionId> =
                        self.sessions.iter().map(|s| s.id).filter(|id| (*id == sid) != all_but).collect();
                    for v in victims {
                        self.kill_session(v, "session killed");
                    }
                    Outcome::Ok
                }
                Err(e) => Outcome::Error(e),
            },
            Cmd::KillServer => {
                let ids: Vec<SessionId> = self.sessions.iter().map(|s| s.id).collect();
                for sid in ids {
                    self.kill_session(sid, "server exited");
                }
                self.quit = true;
                Outcome::Ok
            }
            Cmd::RenameSession { target, name } => {
                if name.is_empty() || name.contains(':') || name.contains('.') {
                    return Outcome::Error(format!("bad session name: {name}"));
                }
                if self.sessions.iter().any(|s| s.name == name) {
                    return Outcome::Error(format!("duplicate session: {name}"));
                }
                match self.resolve_session(target.as_ref(), cid) {
                    Ok(sid) => {
                        self.session_mut(sid).unwrap().name = name;
                        Outcome::Ok
                    }
                    Err(e) => Outcome::Error(e),
                }
            }
            Cmd::NewWindow { name, cwd, target, argv, detached } => {
                let sid = match self.resolve_session(target.as_ref(), cid) {
                    Ok(s) => s,
                    Err(e) => return Outcome::Error(e),
                };
                let cwd = self.pane_cwd(cwd.as_deref(), sid, cid);
                let before = self.session(sid).map(|s| (s.cur, s.last));
                let r = self.new_window(sid, name, cwd.as_deref(), &argv);
                if detached && let (Ok(_), Some((cur, last))) = (&r, before) {
                    let s = self.session_mut(sid).unwrap();
                    s.cur = cur;
                    s.last = last;
                }
                r.map(|_| ()).into()
            }
            Cmd::KillWindow { target, all_but } => {
                let (sid, widx, _) = match self.resolve(target.as_ref(), cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                let s = self.session(sid).unwrap();
                let ids: Vec<PaneId> = s
                    .windows
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| (*i == widx) != all_but)
                    .flat_map(|(_, w)| w.panes.iter().map(|p| p.id))
                    .collect();
                for id in ids {
                    if let Some(p) = self.find_pane_mut(id) {
                        p.kill();
                    }
                    self.remove_pane(id, Some(0));
                }
                Outcome::Ok
            }
            Cmd::RenameWindow { target, name } => {
                let (sid, widx, _) = match self.resolve(target.as_ref(), cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                self.session_mut(sid).unwrap().windows[widx].name = name;
                Outcome::Ok
            }
            Cmd::SelectWindow { target } => {
                let sid = match self.resolve_session(Some(&target), cid) {
                    Ok(s) => s,
                    Err(e) => return Outcome::Error(e),
                };
                match self.resolve_window(sid, Some(&target)) {
                    Ok(idx) => {
                        self.session_mut(sid).unwrap().select_window(idx);
                        Outcome::Ok
                    }
                    Err(e) => Outcome::Error(e),
                }
            }
            Cmd::NextWindow | Cmd::PreviousWindow | Cmd::LastWindow => {
                let w = match cmd {
                    Cmd::NextWindow => "+",
                    Cmd::PreviousWindow => "-",
                    _ => "!",
                };
                self.exec(Cmd::SelectWindow { target: Target::parse(&format!(":{w}")) }, cid)
            }
            Cmd::SplitWindow { horizontal, cwd, target, argv, detached, before, full } => {
                let (sid, widx, pid) = match self.resolve(target.as_ref(), cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                let cwd = self.pane_cwd(cwd.as_deref(), sid, cid);
                let (scols, srows) = self.session(sid).map(|s| (s.cols, s.rows)).unwrap();
                let area = self.window_area(scols, srows);
                // Unzoom first so the layout rectangles are real.
                let w = &mut self.session_mut(sid).unwrap().windows[widx];
                if w.zoomed {
                    w.zoomed = false;
                    w.relayout(area);
                }
                let rect = if full {
                    area
                } else {
                    match w.rect_of(pid) {
                        Some(r) => r,
                        None => return Outcome::Error("pane has no layout".into()),
                    }
                };
                if (horizontal && rect.w < 3) || (!horizontal && rect.h < 3) {
                    return Outcome::Error("pane too small to split".into());
                }
                let (nw, nh) = if horizontal { ((rect.w - 1) / 2, rect.h) } else { (rect.w, (rect.h - 1) / 2) };
                let pane = match self.spawn_pane(&argv, cwd.as_deref(), nw.max(1), nh.max(1)) {
                    Ok(p) => p,
                    Err(e) => return Outcome::Error(e),
                };
                let w = &mut self.session_mut(sid).unwrap().windows[widx];
                let nid = pane.id;
                if full {
                    w.layout.split_root(horizontal, nid, rect, before);
                } else {
                    w.layout.split_at(pid, horizontal, nid, rect, before);
                }
                w.panes.push(pane);
                if !detached {
                    w.last_pane = Some(w.active);
                    w.active = nid;
                }
                self.relayout_session(sid);
                Outcome::Ok
            }
            Cmd::KillPane { target, all_but } => {
                let (sid, widx, pid) = match self.resolve(target.as_ref(), cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                let ids: Vec<PaneId> = self.session(sid).unwrap().windows[widx]
                    .panes
                    .iter()
                    .map(|p| p.id)
                    .filter(|id| (*id == pid) != all_but)
                    .collect();
                for id in ids {
                    if let Some(p) = self.find_pane_mut(id) {
                        p.kill();
                    }
                    self.remove_pane(id, Some(0));
                }
                Outcome::Ok
            }
            Cmd::SelectPane { sel } => {
                let (sid, widx, _) = match self.resolve(None, cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                let w = &mut self.session_mut(sid).unwrap().windows[widx];
                let order = w.layout.panes();
                let cur = order.iter().position(|p| *p == w.active).unwrap_or(0);
                let next = match sel {
                    PaneSel::Dir(d) => layout::neighbour(&w.rects, w.active, d),
                    PaneSel::Next => order.get((cur + 1) % order.len().max(1)).copied(),
                    PaneSel::Prev => order.get((cur + order.len().max(1) - 1) % order.len().max(1)).copied(),
                    PaneSel::Last => w.last_pane.filter(|l| w.pane(*l).is_some()),
                    PaneSel::Index(i) => order.get(i).copied(),
                };
                match next {
                    Some(id) if id != w.active => {
                        w.last_pane = Some(w.active);
                        w.active = id;
                        if w.zoomed {
                            w.zoomed = false;
                            self.relayout_session(sid);
                        }
                        Outcome::Ok
                    }
                    Some(_) => Outcome::Ok,
                    None => Outcome::Error("no such pane".into()),
                }
            }
            Cmd::ResizePane { dir, amount, zoom } => {
                let (sid, widx, pid) = match self.resolve(None, cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                let w = &mut self.session_mut(sid).unwrap().windows[widx];
                if zoom {
                    w.zoomed = !w.zoomed && w.panes.len() > 1;
                } else if let Some(d) = dir {
                    w.zoomed = false;
                    w.layout.resize(pid, d, amount.max(1));
                }
                self.relayout_session(sid);
                Outcome::Ok
            }
            Cmd::SwapPane { up } => {
                let (sid, widx, pid) = match self.resolve(None, cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                let w = &mut self.session_mut(sid).unwrap().windows[widx];
                let order = w.layout.panes();
                let cur = order.iter().position(|p| *p == pid).unwrap_or(0);
                let n = order.len();
                if n < 2 {
                    return Outcome::Ok;
                }
                let other = if up { order[(cur + n - 1) % n] } else { order[(cur + 1) % n] };
                w.layout.swap(pid, other);
                self.relayout_session(sid);
                Outcome::Ok
            }
            Cmd::BreakPane => {
                let (sid, widx, pid) = match self.resolve(None, cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                let s = self.session_mut(sid).unwrap();
                let w = &mut s.windows[widx];
                if w.panes.len() < 2 {
                    return Outcome::Error("can't break with only one pane".into());
                }
                w.layout.remove(pid);
                let pos = w.panes.iter().position(|p| p.id == pid).unwrap();
                let pane = w.panes.remove(pos);
                if w.active == pid {
                    w.active = w.layout.panes()[0];
                }
                w.last_pane = None;
                w.zoomed = false;
                let wid = self.alloc_id();
                let s = self.session_mut(sid).unwrap();
                let name = pane.command.clone();
                s.windows.push(Window {
                    id: wid,
                    name,
                    layout: Node::Leaf(pid),
                    active: pid,
                    panes: vec![pane],
                    last_pane: None,
                    zoomed: false,
                    rects: Vec::new(),
                });
                let idx = s.windows.len() - 1;
                s.select_window(idx);
                self.relayout_session(sid);
                Outcome::Ok
            }
            Cmd::SendKeys { target, keys, literal } => {
                let (_, _, pid) = match self.resolve(target.as_ref(), cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                let Some(p) = self.find_pane_mut(pid) else { return Outcome::Error("no such pane".into()) };
                let app = p.screen().application_cursor();
                let mut bytes = Vec::new();
                for k in &keys {
                    if literal {
                        bytes.extend_from_slice(k.as_bytes());
                    } else {
                        bytes.extend(input::encode_send_key(k, app));
                    }
                }
                p.write_input(&bytes);
                Outcome::Ok
            }
            Cmd::CopyMode { page_up } => {
                let (_, _, pid) = match self.resolve(None, cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                let Some(p) = self.find_pane_mut(pid) else { return Outcome::Error("no such pane".into()) };
                enter_copy_mode(p);
                if page_up {
                    let rows = p.rows as usize;
                    copy_scroll(p, rows as i64);
                }
                Outcome::Ok
            }
            Cmd::PasteBuffer => {
                let (_, _, pid) = match self.resolve(None, cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                let text = match crate::clipboard::get_text() {
                    Ok(t) => t,
                    Err(e) => return Outcome::Error(format!("clipboard: {e}")),
                };
                if text.is_empty() {
                    return Outcome::Error("clipboard is empty".into());
                }
                let Some(p) = self.find_pane_mut(pid) else { return Outcome::Error("no such pane".into()) };
                let b = p.screen().bracketed_paste();
                p.write_input(&input::encode_paste(&text, b));
                Outcome::Ok
            }
            Cmd::CommandPrompt { prompt, initial, template } => {
                let Some(cid) = cid else { return Outcome::Error("command-prompt: no client".into()) };
                let initial = initial.map(|i| self.expand_format(&i, cid)).unwrap_or_default();
                let label = prompt.map(|p| self.expand_format(&p, cid)).unwrap_or_else(|| ":".into());
                let label = if label.ends_with(' ') || label == ":" { label } else { format!("{label} ") };
                if let Some(c) = self.clients.get_mut(&cid) {
                    let cursor = initial.chars().count();
                    c.prompt = Some(Prompt { kind: PromptKind::Command { template }, label, input: initial, cursor });
                }
                Outcome::Ok
            }
            Cmd::ConfirmBefore { prompt, cmd } => {
                let Some(cid) = cid else { return Outcome::Error("confirm-before: no client".into()) };
                let label =
                    prompt.map(|p| self.expand_format(&p, cid)).unwrap_or_else(|| format!("Confirm '{cmd}'? (y/n)"));
                if let Some(c) = self.clients.get_mut(&cid) {
                    c.prompt = Some(Prompt {
                        kind: PromptKind::Confirm(*cmd),
                        label: format!("{label} "),
                        input: String::new(),
                        cursor: 0,
                    });
                }
                Outcome::Ok
            }
            Cmd::DisplayMessage { msg } => {
                let Some(cid) = cid else { return Outcome::Ok };
                let m = self.expand_format(&msg, cid);
                Outcome::Text(m)
            }
            Cmd::BindKey { root, key, cmd } => match Key::parse(&key) {
                Some(k) => {
                    if root { &mut self.root_binds } else { &mut self.prefix_binds }.insert(k, *cmd);
                    Outcome::Ok
                }
                None => Outcome::Error(format!("unknown key: {key}")),
            },
            Cmd::UnbindKey { root, key } => match Key::parse(&key) {
                Some(k) => {
                    if root { &mut self.root_binds } else { &mut self.prefix_binds }.remove(&k);
                    Outcome::Ok
                }
                None => Outcome::Error(format!("unknown key: {key}")),
            },
            Cmd::SetOption { name, value } => {
                let r = self.opts.set(&name, &value);
                if r.is_ok() {
                    // Status line toggles change the window area.
                    let ids: Vec<SessionId> = self.sessions.iter().map(|s| s.id).collect();
                    for sid in ids {
                        self.relayout_session(sid);
                    }
                    let mouse = self.opts.mouse;
                    for c in self.clients.values_mut() {
                        c.last_grid = None;
                        if name == "mouse" && c.session.is_some() {
                            c.send(ServerMsg::SetMouse(mouse));
                        }
                    }
                }
                r.into()
            }
            Cmd::SwitchClient { next, prev, target } => {
                let Some(cid) = cid else { return Outcome::Error("switch-client: no client".into()) };
                let cur = self.clients.get(&cid).and_then(|c| c.session);
                let Some(cur) = cur else { return Outcome::Error("client not attached".into()) };
                let pos = self.sessions.iter().position(|s| s.id == cur).unwrap_or(0);
                let n = self.sessions.len();
                let sid = if next {
                    self.sessions[(pos + 1) % n].id
                } else if prev {
                    self.sessions[(pos + n - 1) % n].id
                } else {
                    match self.resolve_session(target.as_ref(), None) {
                        Ok(s) => s,
                        Err(e) => return Outcome::Error(e),
                    }
                };
                if sid == cur {
                    return Outcome::Ok;
                }
                let c = self.clients.get_mut(&cid).unwrap();
                let (cols, rows) = (c.cols, c.rows);
                c.session = Some(sid);
                c.last_grid = None;
                self.resize_session(sid, cols, rows);
                if let Some(s) = self.session_mut(sid) {
                    s.last_used = Instant::now();
                }
                Outcome::Ok
            }
            Cmd::ListKeys => {
                let mut lines: Vec<String> =
                    self.prefix_binds.iter().map(|(k, c)| format!("bind-key -T prefix {k:<10} {c}")).collect();
                lines.extend(self.root_binds.iter().map(|(k, c)| format!("bind-key -T root   {k:<10} {c}")));
                lines.sort();
                Outcome::Text(lines.join("\n"))
            }
            Cmd::ClearHistory => {
                let (_, _, pid) = match self.resolve(None, cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                let hist = self.opts.history_limit;
                if let Some(p) = self.find_pane_mut(pid) {
                    // The scrollback the copy cursor refers to is about to vanish.
                    exit_copy_mode(p);
                    // vt100 has no explicit clear; re-create the parser at the same size.
                    let (rows, cols) = p.screen().size();
                    let mut fresh = vt100::Parser::new_with_callbacks(rows, cols, hist, pane::Callbacks::default());
                    fresh.process(&p.screen().state_formatted());
                    p.parser = fresh;
                }
                Outcome::Ok
            }
            Cmd::SourceFile { path } => self.source_file(&path).map(|_| ()).into(),
        }
    }

    /// Expand `#S` (session), `#W` (window), `#P` (pane index), `#T` (pane title).
    fn expand_format(&self, s: &str, cid: ClientId) -> String {
        let Ok((sid, widx, pid)) = self.resolve(None, Some(cid)) else { return s.to_string() };
        let sess = self.session(sid).unwrap();
        let w = &sess.windows[widx];
        let pidx = w.layout.panes().iter().position(|p| *p == pid).unwrap_or(0);
        let title = w.pane(pid).map(|p| p.display_title().to_string()).unwrap_or_default();
        s.replace("#S", &sess.name)
            .replace("#W", &w.name)
            .replace("#P", &pidx.to_string())
            .replace("#T", &title)
            .replace("#I", &(widx + self.opts.base_index).to_string())
    }

    // ------------------------------------------------------------------ keys

    fn handle_key(&mut self, cid: ClientId, rec: KeyRecord) {
        let Some(c) = self.clients.get_mut(&cid) else { return };
        let Some(sid) = c.session else { return };
        if !rec.down && c.swallow_up.remove(&rec.vk) {
            return;
        }
        let key = key_from_record(&rec);
        if key.is_some() && self.opts.display_time_ms == 0 {
            c.message = None;
        }

        if c.overlay.is_some() {
            if key.is_some() {
                c.overlay = None;
                c.swallow_up.insert(rec.vk);
            }
            return;
        }
        if c.prompt.is_some() {
            if let Some(k) = key {
                c.swallow_up.insert(rec.vk);
                self.prompt_key(cid, k);
            }
            return;
        }

        let in_copy =
            self.session(sid).and_then(|s| s.window()).and_then(|w| w.active_pane()).is_some_and(|p| p.copy.is_some());

        if let Some(k) = key {
            let c = self.clients.get_mut(&cid).unwrap();
            if c.prefix {
                c.prefix = false;
                c.swallow_up.insert(rec.vk);
                if k == self.opts.prefix {
                    // Send the prefix key itself to the pane.
                    self.write_active(sid, &input::encode_key_record(&rec));
                    return;
                }
                match self.prefix_binds.get(&k).cloned() {
                    Some(cmd) => {
                        let out = self.exec(cmd, Some(cid));
                        self.reply(cid, out);
                    }
                    None => self.message(cid, &format!("unbound key: {k}")),
                }
                return;
            }
            if k == self.opts.prefix {
                c.prefix = true;
                c.swallow_up.insert(rec.vk);
                return;
            }
            if in_copy {
                c.swallow_up.insert(rec.vk);
                self.copy_key(cid, sid, k);
                return;
            }
            if let Some(cmd) = self.root_binds.get(&k).cloned() {
                c.swallow_up.insert(rec.vk);
                let out = self.exec(cmd, Some(cid));
                self.reply(cid, out);
                return;
            }
        } else if in_copy {
            return;
        }
        self.write_active(sid, &input::encode_key_record(&rec));
    }

    fn write_active(&mut self, sid: SessionId, bytes: &[u8]) {
        if let Some(p) = self.session_mut(sid).and_then(|s| s.window_mut()).and_then(|w| w.active_pane_mut()) {
            p.write_input(bytes);
        }
    }

    fn prompt_key(&mut self, cid: ClientId, k: Key) {
        let Some(c) = self.clients.get_mut(&cid) else { return };
        let Some(p) = c.prompt.as_mut() else { return };
        match &p.kind {
            PromptKind::Confirm(_) => {
                let prompt = c.prompt.take().unwrap();
                if matches!(k.code, KeyCode::Char('y') | KeyCode::Char('Y'))
                    && !k.ctrl
                    && !k.alt
                    && let PromptKind::Confirm(cmd) = prompt.kind
                {
                    let out = self.exec(cmd, Some(cid));
                    self.reply(cid, out);
                }
            }
            PromptKind::Command { .. } => match (k.code, k.ctrl, k.alt) {
                (KeyCode::Escape, _, _) | (KeyCode::Char('c'), true, _) | (KeyCode::Char('g'), true, _) => {
                    c.prompt = None;
                }
                (KeyCode::Enter, _, _) => {
                    let prompt = c.prompt.take().unwrap();
                    let PromptKind::Command { template } = prompt.kind else { unreachable!() };
                    let line = match template {
                        Some(t) => t.replace("%%", &crate::command::quote(&prompt.input)),
                        None => prompt.input,
                    };
                    match crate::command::parse_line(&line) {
                        Ok(Some(cmd)) => {
                            let out = self.exec(cmd, Some(cid));
                            self.reply(cid, out);
                        }
                        Ok(None) => {}
                        Err(e) => self.message(cid, &e),
                    }
                }
                (KeyCode::BSpace, false, false) | (KeyCode::Char('h'), true, _) => {
                    if p.cursor > 0 {
                        p.cursor -= 1;
                        let idx = char_index(&p.input, p.cursor);
                        p.input.remove(idx);
                    }
                }
                (KeyCode::DC, _, _) | (KeyCode::Char('d'), true, _) => {
                    if p.cursor < p.input.chars().count() {
                        let idx = char_index(&p.input, p.cursor);
                        p.input.remove(idx);
                    }
                }
                (KeyCode::Left, _, _) | (KeyCode::Char('b'), true, _) => p.cursor = p.cursor.saturating_sub(1),
                (KeyCode::Right, _, _) | (KeyCode::Char('f'), true, _) => {
                    p.cursor = (p.cursor + 1).min(p.input.chars().count())
                }
                (KeyCode::Home, _, _) | (KeyCode::Char('a'), true, _) => p.cursor = 0,
                (KeyCode::End, _, _) | (KeyCode::Char('e'), true, _) => p.cursor = p.input.chars().count(),
                (KeyCode::Char('u'), true, _) => {
                    let idx = char_index(&p.input, p.cursor);
                    p.input = p.input[idx..].to_string();
                    p.cursor = 0;
                }
                (KeyCode::Char('k'), true, _) => {
                    let idx = char_index(&p.input, p.cursor);
                    p.input.truncate(idx);
                }
                (KeyCode::Char('w'), true, _) => {
                    let idx = char_index(&p.input, p.cursor);
                    let head = &p.input[..idx];
                    let trimmed = head.trim_end();
                    let cut = trimmed.rfind(' ').map(|i| i + 1).unwrap_or(0);
                    let removed = head[cut..].chars().count();
                    p.input = format!("{}{}", &head[..cut], &p.input[idx..]);
                    p.cursor -= removed;
                }
                (KeyCode::Char(ch), false, false) => {
                    let idx = char_index(&p.input, p.cursor);
                    p.input.insert(idx, ch);
                    p.cursor += 1;
                }
                _ => {}
            },
        }
    }

    // ------------------------------------------------------------- copy mode

    fn copy_key(&mut self, cid: ClientId, sid: SessionId, k: Key) {
        let Some(p) = self.session_mut(sid).and_then(|s| s.window_mut()).and_then(|w| w.active_pane_mut()) else {
            return;
        };
        let rows = p.rows as i64;
        let cols = p.cols;
        let Some(c) = p.copy.as_mut() else { return };
        match (k.code, k.ctrl, k.alt) {
            (KeyCode::Escape, _, _) => {
                if c.anchor.is_some() {
                    c.anchor = None;
                } else {
                    exit_copy_mode(p);
                }
            }
            (KeyCode::Char('q'), false, false) | (KeyCode::Char('c'), true, _) => exit_copy_mode(p),
            (KeyCode::Up, _, _) | (KeyCode::Char('k'), false, false) => {
                if c.cy > 0 {
                    c.cy -= 1;
                } else {
                    copy_scroll(p, 1);
                }
            }
            (KeyCode::Down, _, _) | (KeyCode::Char('j'), false, false) => {
                if (c.cy as i64) < rows - 1 {
                    c.cy += 1;
                } else {
                    copy_scroll(p, -1);
                }
            }
            (KeyCode::Left, _, _) | (KeyCode::Char('h'), false, false) => c.cx = c.cx.saturating_sub(1),
            (KeyCode::Right, _, _) | (KeyCode::Char('l'), false, false) => {
                c.cx = (c.cx + 1).min(cols.saturating_sub(1))
            }
            (KeyCode::PPage, _, _) | (KeyCode::Char('b'), true, _) => copy_scroll(p, rows),
            (KeyCode::NPage, _, _) | (KeyCode::Char('f'), true, _) | (KeyCode::Char(' '), true, _) => {
                copy_scroll(p, -rows)
            }
            (KeyCode::Char('u'), true, _) => copy_scroll(p, rows / 2),
            (KeyCode::Char('d'), true, _) => copy_scroll(p, -rows / 2),
            (KeyCode::Char('g'), false, false) => {
                let max = p.scrollback_len();
                let c = p.copy.as_mut().unwrap();
                c.offset = max;
                c.cy = 0;
            }
            (KeyCode::Char('G'), false, false) => {
                c.offset = 0;
                c.cy = (rows - 1) as u16;
            }
            (KeyCode::Home, _, _) | (KeyCode::Char('0'), false, false) | (KeyCode::Char('a'), true, _) => c.cx = 0,
            (KeyCode::End, _, _) | (KeyCode::Char('$'), false, false) | (KeyCode::Char('e'), true, _) => {
                c.cx = cols.saturating_sub(1)
            }
            (KeyCode::Char(' '), false, false) | (KeyCode::Char('v'), false, false) => {
                let abs = copy_abs(p);
                let c = p.copy.as_mut().unwrap();
                c.anchor = Some((abs, c.cx));
            }
            (KeyCode::Enter, _, _) | (KeyCode::Char('y'), false, false) | (KeyCode::Char('w'), true, _) => {
                if c.anchor.is_some() {
                    match copy_selection(p) {
                        Ok(n) => self.message(cid, &format!("copied {n} characters")),
                        Err(e) => self.message(cid, &e),
                    }
                }
                if let Some(p) = self.session_mut(sid).and_then(|s| s.window_mut()).and_then(|w| w.active_pane_mut()) {
                    exit_copy_mode(p);
                }
            }
            _ => {}
        }
    }

    // ----------------------------------------------------------------- mouse

    fn handle_mouse(&mut self, cid: ClientId, m: MouseRecord) {
        let Some(c) = self.clients.get_mut(&cid) else { return };
        let Some(sid) = c.session else { return };
        if c.prompt.is_some() || c.overlay.is_some() {
            return;
        }
        let x = m.x.max(0) as u16;
        let y = m.y.max(0) as u16;
        let buttons = m.buttons & 0x7;
        let prev = c.mouse_buttons;
        c.mouse_buttons = buttons;
        let shift = m.ctrl & crate::keys::SHIFT_PRESSED != 0;
        let alt = m.ctrl & (crate::keys::LEFT_ALT_PRESSED | crate::keys::RIGHT_ALT_PRESSED) != 0;
        let ctrl = m.ctrl & (crate::keys::LEFT_CTRL_PRESSED | crate::keys::RIGHT_CTRL_PRESSED) != 0;
        let wheel = if m.flags & MOUSE_WHEELED != 0 {
            let delta = (m.buttons >> 16) as i16;
            Some(delta > 0)
        } else {
            None
        };
        if m.flags & MOUSE_HWHEELED != 0 {
            return;
        }
        let pressed = buttons & !prev;
        let released = prev & !buttons;
        let motion = m.flags & MOUSE_MOVED != 0 && wheel.is_none();

        // Border drag in progress?
        if let Some(d) = c.drag.take()
            && buttons & BTN_LEFT != 0
        {
            let pos = if d.horizontal { x as i32 } else { y as i32 };
            let delta = pos - d.last;
            if delta != 0 {
                let dir = match (d.horizontal, delta > 0) {
                    (true, true) => Dir::Right,
                    (true, false) => Dir::Left,
                    (false, true) => Dir::Down,
                    (false, false) => Dir::Up,
                };
                if let Some(w) = self.session_mut(sid).and_then(|s| s.window_mut()) {
                    w.layout.resize(d.pane, dir, delta.unsigned_abs() as u16);
                }
                self.relayout_session(sid);
            }
            self.clients.get_mut(&cid).unwrap().drag = Some(Drag { pane: d.pane, horizontal: d.horizontal, last: pos });
            return;
        }

        let status_y = if self.opts.status {
            Some(if self.opts.status_top { 0 } else { self.session(sid).map(|s| s.rows - 1).unwrap_or(0) })
        } else {
            None
        };
        if Some(y) == status_y {
            if pressed & BTN_LEFT != 0 {
                self.status_click(cid, sid, x);
            }
            return;
        }

        let mouse_opt = self.opts.mouse;
        let area = match self.session(sid) {
            Some(s) => self.window_area(s.cols, s.rows),
            None => return,
        };
        let Some(s) = self.session_mut(sid) else { return };
        let Some(w) = s.window_mut() else { return };
        let Some(pid) = w.pane_at(x, y) else {
            // On a border: start a drag from the pane whose edge this is.
            // The outer edge of the window is not a border.
            if pressed & BTN_LEFT != 0
                && mouse_opt
                && area.contains(x, y)
                && let Some((pane, horizontal)) = border_owner(&w.rects, x, y)
            {
                self.clients.get_mut(&cid).unwrap().drag =
                    Some(Drag { pane, horizontal, last: if horizontal { x as i32 } else { y as i32 } });
            }
            return;
        };
        let rect = w.rect_of(pid).unwrap();
        let (px, py) = (x - rect.x, y - rect.y);

        // Select the pane on any button press.
        if pressed != 0 && mouse_opt && pid != w.active {
            w.last_pane = Some(w.active);
            w.active = pid;
        }
        let pane = w.pane_mut(pid).unwrap();
        let app_mouse = pane.screen().mouse_protocol_mode();
        let in_copy = pane.copy.is_some();

        if app_mouse != vt100::MouseProtocolMode::None && !in_copy && !shift {
            use vt100::MouseProtocolMode as M;
            let btn_of = |b: u32| {
                if b & BTN_LEFT != 0 {
                    MouseButton::Left
                } else if b & BTN_MIDDLE != 0 {
                    MouseButton::Middle
                } else if b & BTN_RIGHT != 0 {
                    MouseButton::Right
                } else {
                    MouseButton::None
                }
            };
            let mut out = Vec::new();
            if let Some(up) = wheel {
                out.extend(input::encode_mouse_sgr(
                    if up { MouseButton::WheelUp } else { MouseButton::WheelDown },
                    false,
                    false,
                    shift,
                    alt,
                    ctrl,
                    px,
                    py,
                ));
            } else if pressed != 0 {
                out.extend(input::encode_mouse_sgr(btn_of(pressed), false, false, shift, alt, ctrl, px, py));
            } else if released != 0 {
                if app_mouse != M::Press {
                    out.extend(input::encode_mouse_sgr(btn_of(released), false, true, shift, alt, ctrl, px, py));
                }
            } else if motion {
                let report = match app_mouse {
                    M::AnyMotion => true,
                    M::ButtonMotion => buttons != 0,
                    _ => false,
                };
                if report {
                    out.extend(input::encode_mouse_sgr(btn_of(buttons), true, false, shift, alt, ctrl, px, py));
                }
            }
            if !out.is_empty() {
                pane.write_input(&out);
            }
            return;
        }

        if !mouse_opt {
            return;
        }

        if let Some(up) = wheel {
            if pane.screen().alternate_screen() && !in_copy {
                // Full-screen app without mouse support: scroll with arrow keys.
                let k = if up { KeyCode::Up } else { KeyCode::Down };
                let app = pane.screen().application_cursor();
                let mut b = Vec::new();
                for _ in 0..3 {
                    b.extend(input::encode_key(Key::plain(k), app));
                }
                pane.write_input(&b);
            } else {
                if !in_copy {
                    enter_copy_mode(pane);
                }
                copy_scroll(pane, if up { 3 } else { -3 });
                if pane.copy.as_ref().is_some_and(|c| c.offset == 0 && c.anchor.is_none()) && !up {
                    exit_copy_mode(pane);
                }
            }
            return;
        }
        if pressed & BTN_LEFT != 0 {
            if !in_copy {
                enter_copy_mode(pane);
            }
            let c = pane.copy.as_mut().unwrap();
            c.cx = px.min(pane.cols - 1);
            c.cy = py.min(pane.rows - 1);
            let abs = copy_abs(pane);
            let c = pane.copy.as_mut().unwrap();
            c.anchor = Some((abs, c.cx));
            c.dragging = true;
            return;
        }
        if motion && buttons & BTN_LEFT != 0 && in_copy {
            let c = pane.copy.as_mut().unwrap();
            if c.dragging {
                c.cx = px.min(pane.cols - 1);
                c.cy = py.min(pane.rows - 1);
            }
            return;
        }
        if released & BTN_LEFT != 0 && in_copy {
            let (cols, rows) = (pane.cols, pane.rows);
            let c = pane.copy.as_mut().unwrap();
            if c.dragging {
                c.dragging = false;
                c.cx = px.min(cols - 1);
                c.cy = py.min(rows - 1);
                let (anchor, cx) = (c.anchor, c.cx);
                let same = anchor == Some((copy_abs(pane), cx));
                if same {
                    // A plain click: just leave copy mode.
                    exit_copy_mode(pane);
                } else {
                    match copy_selection(pane) {
                        Ok(n) => self.message(cid, &format!("copied {n} characters")),
                        Err(e) => self.message(cid, &e),
                    }
                    if let Some(p) = self.session_mut(sid).and_then(|s| s.window_mut()).and_then(|w| w.pane_mut(pid)) {
                        exit_copy_mode(p);
                    }
                }
            }
        }
    }

    fn status_click(&mut self, _cid: ClientId, sid: SessionId, x: u16) {
        let base = self.opts.base_index;
        let Some(s) = self.session_mut(sid) else { return };
        // Mirror the layout in `status_line`: "[name] " then "idx:name<flag> ".
        let mut pos = s.name.chars().count() as u16 + 3;
        let labels: Vec<(usize, u16)> = s
            .windows
            .iter()
            .enumerate()
            .map(|(i, w)| (i, (window_label(i, w, s, base).chars().count() + 1) as u16))
            .collect();
        for (i, wlen) in labels {
            if x >= pos && x < pos + wlen {
                s.select_window(i);
                return;
            }
            pos += wlen;
        }
    }

    // ---------------------------------------------------------------- render

    fn render_all(&mut self) {
        let now = Instant::now();
        // display-time 0 means "until the next key press" (see handle_key).
        if self.opts.display_time_ms > 0 {
            let ttl = Duration::from_millis(self.opts.display_time_ms);
            for c in self.clients.values_mut() {
                if c.message.as_ref().is_some_and(|(_, t)| now.duration_since(*t) > ttl) {
                    c.message = None;
                }
            }
        }
        for c in self.clients.values_mut() {
            c.flush_pending();
        }
        let ids: Vec<ClientId> = self.clients.values().filter(|c| c.session.is_some()).map(|c| c.id).collect();
        for cid in ids {
            self.render_client(cid);
        }
    }

    fn render_client(&mut self, cid: ClientId) {
        let Some(c) = self.clients.get(&cid) else { return };
        let Some(sid) = c.session else { return };
        let (cols, rows) = (c.cols, c.rows);
        let Some(spos) = self.sessions.iter().position(|s| s.id == sid) else { return };
        let (status, status_top, border_fg, active_fg, base_index, opts_status) = (
            self.opts.status,
            self.opts.status_top,
            self.opts.pane_border_fg,
            self.opts.pane_border_active_fg,
            self.opts.base_index,
            self.opts.status,
        );
        let _ = status;
        let (message, prompt) = {
            let c = self.clients.get(&cid).unwrap();
            (
                c.message.as_ref().map(|(m, _)| m.clone()),
                c.prompt.as_ref().map(|p| (p.label.clone(), p.input.clone(), p.cursor)),
            )
        };
        let mut bell = false;
        let s = &mut self.sessions[spos];
        let status_line = if opts_status {
            let right = {
                let w = s.window();
                let title = w.and_then(|w| w.active_pane()).map(|p| p.display_title().to_string()).unwrap_or_default();
                let now = chrono::Local::now();
                format!("\"{}\" {}", truncate(&title, 30), now.format("%H:%M %d-%b-%y"))
            };
            Some(StatusLine {
                session: s.name.clone(),
                windows: s
                    .windows
                    .iter()
                    .enumerate()
                    .map(|(i, w)| (window_label(i, w, s, base_index), i == s.cur))
                    .collect(),
                right,
                message,
                prompt,
                fg: self.opts.status_fg,
                bg: self.opts.status_bg,
            })
        } else {
            None
        };
        let Some(w) = s.window_mut() else { return };
        let active = w.active;
        // Apply copy-mode scroll offsets for rendering and precompute the
        // copy views (they need mutable access to the scrollback).
        let mut copy_views: HashMap<PaneId, CopyView> = HashMap::new();
        for p in &mut w.panes {
            if p.bell {
                p.bell = false;
                bell = true;
            }
            if p.copy.is_some() {
                let cur_abs = copy_abs(p);
                let cm = p.copy.as_ref().unwrap();
                let sel = copy_sel_view(p.rows, p.cols, cm, cur_abs);
                copy_views.insert(p.id, CopyView { cx: cm.cx, cy: cm.cy, sel, offset: cm.offset });
                let off = cm.offset;
                p.parser.screen_mut().set_scrollback(off);
            }
        }
        let mut views = Vec::new();
        for (id, rect) in &w.rects {
            let Some(p) = w.pane(*id) else { continue };
            views.push(PaneView {
                rect: *rect,
                screen: p.screen(),
                active: *id == active,
                copy: copy_views.get(id).copied(),
            });
        }
        let frame =
            Frame { cols, rows, panes: views, status: status_line, status_top, border_fg, active_border_fg: active_fg };
        let (mut grid, mut cursor) = render::compose(&frame);
        for p in &mut w.panes {
            if p.copy.is_some() {
                p.parser.screen_mut().set_scrollback(0);
            }
        }
        let area = self.window_area(cols, rows);
        let c = self.clients.get_mut(&cid).unwrap();
        if let Some(lines) = &c.overlay {
            render::draw_overlay(&mut grid, area, lines);
            cursor = None;
        }
        let changed = c.last_grid.as_ref() != Some(&grid) || c.last_cursor != cursor;
        if !changed && !bell {
            return;
        }
        let mut bytes = render::diff(c.last_grid.as_ref(), &grid, cursor);
        if bell {
            bytes.insert(0, 0x07);
        }
        c.last_grid = Some(grid);
        c.last_cursor = cursor;
        c.send_output(bytes);
    }
}

/// Expand a leading `~` or `~/` to the user's home directory.
fn expand_home(path: &str) -> String {
    if (path == "~" || path.starts_with("~/") || path.starts_with("~\\"))
        && let Some(home) = dirs::home_dir()
    {
        return format!("{}{}", home.display(), &path[1..]);
    }
    path.to_string()
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n.saturating_sub(1)).chain(std::iter::once('…')).collect()
    }
}

fn window_label(i: usize, w: &Window, s: &Session, base: usize) -> String {
    let flag = if i == s.cur {
        "*"
    } else if Some(w.id) == s.last {
        "-"
    } else {
        ""
    };
    let zoom = if w.zoomed { "Z" } else { "" };
    format!("{}:{}{}{}", i + base, w.name, flag, zoom)
}

fn char_index(s: &str, chars: usize) -> usize {
    s.char_indices().nth(chars).map(|(i, _)| i).unwrap_or(s.len())
}

/// Which pane's trailing edge a border cell belongs to, and whether that
/// edge is vertical (horizontal drag).
fn border_owner(rects: &[(PaneId, Rect)], x: u16, y: u16) -> Option<(PaneId, bool)> {
    for (id, r) in rects {
        if r.x + r.w == x && y >= r.y && y < r.y + r.h {
            return Some((*id, true));
        }
        if r.y + r.h == y && x >= r.x && x < r.x + r.w {
            return Some((*id, false));
        }
    }
    None
}

fn enter_copy_mode(p: &mut Pane) {
    if p.copy.is_none() {
        let (row, col) = p.screen().cursor_position();
        p.copy = Some(CopyMode {
            offset: 0,
            cx: col.min(p.cols.saturating_sub(1)),
            cy: row.min(p.rows.saturating_sub(1)),
            anchor: None,
            dragging: false,
        });
    }
}

fn exit_copy_mode(p: &mut Pane) {
    p.copy = None;
    p.parser.screen_mut().set_scrollback(0);
}

/// Scroll the copy-mode view by `delta` lines (positive = older).
fn copy_scroll(p: &mut Pane, delta: i64) {
    let max = p.scrollback_len() as i64;
    if let Some(c) = p.copy.as_mut() {
        c.offset = (c.offset as i64 + delta).clamp(0, max) as usize;
    }
}

/// Absolute line number under the copy-mode cursor. The scrollback can shrink
/// under us (resize, clear-history), so the offset is re-clamped here.
fn copy_abs(p: &mut Pane) -> usize {
    let max = p.scrollback_len();
    let rows = p.rows;
    let c = p.copy.as_mut().unwrap();
    c.offset = c.offset.min(max);
    c.cy = c.cy.min(rows.saturating_sub(1));
    max - c.offset + c.cy as usize
}

/// Selection as inclusive screen coordinates for rendering. `cursor_abs` is
/// the absolute line under the copy cursor.
fn copy_sel_view(rows: u16, cols: u16, cm: &CopyMode, cursor_abs: usize) -> Option<((u16, u16), (u16, u16))> {
    let (abs_a, col_a) = cm.anchor?;
    let ay = cm.cy as i64 + (abs_a as i64 - cursor_abs as i64);
    let rows = rows as i64;
    let mut a = (ay, col_a as i64);
    let mut b = (cm.cy as i64, cm.cx as i64);
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }
    if b.0 < 0 || a.0 >= rows {
        return None;
    }
    let (sy, sx) = if a.0 < 0 { (0, 0) } else { (a.0 as u16, a.1 as u16) };
    let (ey, ex) = if b.0 >= rows { ((rows - 1) as u16, cols.saturating_sub(1)) } else { (b.0 as u16, b.1 as u16) };
    Some(((sy, sx), (ey, ex)))
}

/// Copy the selected text to the clipboard; returns the character count.
fn copy_selection(p: &mut Pane) -> Result<usize, String> {
    let cur_abs = copy_abs(p);
    let c = p.copy.as_ref().unwrap();
    let Some((abs_a, col_a)) = c.anchor else { return Err("no selection".into()) };
    let mut a = (abs_a, col_a);
    let mut b = (cur_abs, c.cx);
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }
    let mut text = String::new();
    for abs in a.0..=b.0 {
        let (line, wrapped) = p.line_text(abs);
        let chars: Vec<char> = line.chars().collect();
        let start = if abs == a.0 { a.1 as usize } else { 0 };
        let end = if abs == b.0 { (b.1 as usize + 1).min(chars.len()) } else { chars.len() };
        let seg: String = if start < end { chars[start..end].iter().collect() } else { String::new() };
        if abs == b.0 {
            text.push_str(seg.trim_end());
        } else {
            text.push_str(seg.trim_end());
            if !wrapped {
                text.push('\n');
            }
        }
    }
    let n = text.chars().count();
    crate::clipboard::set_text(&text).map_err(|e| format!("clipboard: {e}"))?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(cap: usize) -> (Client, mpsc::Receiver<ServerMsg>) {
        let (tx, rx) = mpsc::channel(cap);
        let c = Client {
            id: 1,
            tx,
            cols: 10,
            rows: 2,
            cwd: String::new(),
            interactive: true,
            pane_env: None,
            session: Some(1),
            last_grid: Some(Grid::new(10, 2)),
            last_cursor: Some((0, 0)),
            prefix: false,
            prompt: None,
            message: None,
            overlay: None,
            mouse_buttons: 0,
            drag: None,
            swallow_up: HashSet::new(),
            pending: std::collections::VecDeque::new(),
        };
        (c, rx)
    }

    #[test]
    fn lagging_client_forces_full_redraw() {
        let (mut c, mut rx) = client(1);
        c.send_output(b"frame1".to_vec());
        assert!(c.last_grid.is_some(), "first frame queued, diff state kept");
        c.send_output(b"frame2".to_vec());
        assert!(c.last_grid.is_none() && c.last_cursor.is_none(), "dropped frame invalidates the diff base");
        // Only the first frame is in the queue; nothing else was allocated.
        assert!(matches!(rx.try_recv(), Ok(ServerMsg::Output(b)) if b == b"frame1"));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn control_messages_survive_a_full_queue_in_order() {
        let (mut c, mut rx) = client(1);
        c.send_output(b"frame1".to_vec());
        c.send(ServerMsg::SetMouse(false));
        c.send(ServerMsg::Detached { reason: "x".into() });
        assert_eq!(c.pending.len(), 2);
        // A frame must not overtake pending control messages.
        c.last_grid = Some(Grid::new(10, 2));
        c.send_output(b"frame2".to_vec());
        assert!(c.last_grid.is_none());
        assert!(!c.flush_pending(), "queue still full");
        // The client drains one slot at a time; each drain lets one message through.
        assert!(matches!(rx.try_recv(), Ok(ServerMsg::Output(_))));
        assert!(!c.flush_pending());
        assert!(matches!(rx.try_recv(), Ok(ServerMsg::SetMouse(false))));
        assert!(c.flush_pending());
        assert!(matches!(rx.try_recv(), Ok(ServerMsg::Detached { .. })));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn expand_home_forms() {
        let home = dirs::home_dir().unwrap().display().to_string();
        assert_eq!(expand_home("~"), home);
        assert_eq!(expand_home("~/.wmux.conf"), format!("{home}/.wmux.conf"));
        assert_eq!(expand_home("~\\x"), format!("{home}\\x"));
        assert_eq!(expand_home("~user/x"), "~user/x");
        assert_eq!(expand_home("C:\\x"), "C:\\x");
    }
}
