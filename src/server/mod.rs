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
use std::path::PathBuf;
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
    /// A `run-shell` finished: deliver its output to the client that asked.
    ShellDone {
        cid: Option<ClientId>,
        output: String,
        code: i32,
    },
    /// A status-line `#(command)` finished.
    StatusShell {
        command: String,
        output: String,
    },
}

/// Hooks a plugin can attach commands to (`set-hook -g <name> <command>`).
pub const HOOKS: &[&str] = &[
    "after-new-session",
    "after-new-window",
    "after-split-window",
    "after-select-window",
    "after-select-pane",
    "after-kill-pane",
    "client-attached",
    "client-detached",
    "pane-exited",
];

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

/// The `choose-tree` picker (prefix `w` / `s`): a list of sessions, each
/// followed by its windows when `expand` is set. `items` and `lines` are
/// rebuilt from the live sessions at every render; the selection follows the
/// item's identity, so windows appearing or vanishing meanwhile do not move it.
struct Chooser {
    expand: bool,
    /// (session, window) behind every line; `None` is the session line itself.
    items: Vec<(SessionId, Option<WindowId>)>,
    lines: Vec<String>,
    sel: usize,
    top: usize,
}

impl Chooser {
    fn step(&mut self, delta: i64) {
        let last = self.items.len().saturating_sub(1) as i64;
        self.sel = (self.sel as i64 + delta).clamp(0, last) as usize;
    }
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
    /// A `bind -r` key ran; until this instant its table answers bare keys.
    repeat_until: Option<Instant>,
    prompt: Option<Prompt>,
    message: Option<(String, Instant)>,
    /// Multi-line command output shown over the window until a key is pressed
    /// (tmux "view mode"), e.g. `list-keys`.
    overlay: Option<Vec<String>>,
    chooser: Option<Chooser>,
    mouse_buttons: u32,
    drag: Option<Drag>,
    /// Virtual keys whose key-down we consumed; drop the matching key-up.
    swallow_up: HashSet<u16>,
    /// Control messages that did not fit in the output queue yet.
    pending: std::collections::VecDeque<ServerMsg>,
    /// Status-line column ranges of the window labels last drawn (clicks).
    window_hits: Vec<(u16, u16)>,
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
    /// `synchronize-panes`: input goes to every pane of the window.
    synchronized: bool,
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

/// What a key runs, and whether it keeps running without the prefix for
/// `repeat-time` afterwards (tmux `bind -r`).
#[derive(Clone)]
struct Binding {
    cmd: Cmd,
    repeat: bool,
}

enum Outcome {
    Ok,
    Text(String),
    Attach(SessionId),
    Error(String),
    /// The result arrives later as an event (`run-shell`); nothing to send yet.
    Pending,
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
    prefix_binds: HashMap<Key, Binding>,
    root_binds: HashMap<Key, Binding>,
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
    hooks: HashMap<String, Cmd>,
    /// Re-entrancy guard: a hook must not fire hooks.
    in_hook: bool,
    /// Output of status-line `#(command)` pieces and what is being run.
    shell_cache: crate::format::ShellCache,
    shell_running: HashSet<String>,
    shell_last_refresh: Option<Instant>,
    /// Plugin directories loaded so far.
    plugins: Vec<String>,
    events: mpsc::UnboundedSender<Event>,
    /// JSON of every session as last autosaved, to detect structural change.
    last_saved: HashMap<String, String>,
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

    let mut srv = Server::new(pane_tx, socket, tx.clone());
    srv.load_config();
    // A persistent interval: a fresh `sleep` per iteration would never fire
    // while events keep arriving, and autosave / idle-exit hang off the tick.
    let mut tick = tokio::time::interval(Duration::from_millis(1000));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let ev = tokio::select! {
            ev = rx.recv() => match ev { Some(ev) => ev, None => break },
            _ = tick.tick() => Event::Tick,
        };
        // A bug in one command must not take every session down with it:
        // log the panic and keep serving (the panic hook writes the details).
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            srv.handle(ev);
            while let Ok(ev) = rx.try_recv() {
                srv.handle(ev);
            }
            srv.render_all();
        }));
        if r.is_err() {
            log::error!("recovered from a panic; state may be inconsistent");
            for c in srv.clients.values_mut() {
                c.last_grid = None;
            }
            // A panic inside a hook would otherwise leave the guard set forever.
            srv.in_hook = false;
        }
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

/// Keys that keep working without the prefix for `repeat-time`: moving
/// between panes, resizing them and walking the window list, the things one
/// presses several times in a row.
const REPEATABLE: &[&str] = &[
    "h", "j", "k", "l", "H", "J", "K", "L", "Up", "Down", "Left", "Right", "C-Up", "C-Down", "C-Left", "C-Right",
    "M-Up", "M-Down", "M-Left", "M-Right", "n", "p", "o", "{", "}",
];

fn default_bindings() -> HashMap<Key, Binding> {
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
        // vim keys: h/j/k/l move, H/J/K/L resize (tmux-pain-control style).
        ("h", "select-pane -L"),
        ("j", "select-pane -D"),
        ("k", "select-pane -U"),
        ("l", "select-pane -R"),
        ("H", "resize-pane -L 5"),
        ("J", "resize-pane -D 5"),
        ("K", "resize-pane -U 5"),
        ("L", "resize-pane -R 5"),
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
        ("Tab", "last-window"),
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
        ("w", "choose-tree -Zw"),
        ("s", "choose-tree -Zs"),
        ("i", "display-message \"#S:#W.#P #T\""),
        ("C-s", "save-session"),
        ("C-r", "restore-session"),
        ("S", "set-option -w synchronize-panes"),
    ];
    for (k, l) in lines {
        let key = Key::parse(k).expect(k);
        let cmd = crate::command::parse_line(l).expect(l).expect(l);
        m.insert(key, Binding { cmd, repeat: REPEATABLE.contains(&k) });
    }
    for d in 0..10u8 {
        let cmd = Cmd::SelectWindow { target: Target::parse(&format!(":{d}")) };
        m.insert(Key::ch((b'0' + d) as char), Binding { cmd, repeat: false });
    }
    m
}

impl Server {
    fn new(
        pane_tx: std::sync::mpsc::Sender<PaneEvent>,
        socket: String,
        events: mpsc::UnboundedSender<Event>,
    ) -> Server {
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
            hooks: HashMap::new(),
            in_hook: false,
            shell_cache: crate::format::ShellCache::default(),
            shell_running: HashSet::new(),
            shell_last_refresh: None,
            plugins: Vec::new(),
            events,
            last_saved: HashMap::new(),
        }
    }

    // ------------------------------------------------------------- resurrect

    fn sessions_dir(&self) -> PathBuf {
        if self.opts.sessions_dir.is_empty() {
            crate::resurrect::default_dir()
        } else {
            PathBuf::from(expand_home(&self.opts.sessions_dir))
        }
    }

    /// Describe one live session.
    fn snapshot(&self, s: &Session) -> crate::resurrect::SavedSession {
        use crate::resurrect::*;
        SavedSession {
            name: s.name.clone(),
            current: s.cur,
            windows: s
                .windows
                .iter()
                .map(|w| {
                    let lookup =
                        |id: PaneId| w.pane(id).map(|p| SavedPane { argv: p.argv.clone(), cwd: p.cwd.clone() });
                    let order = w.layout.panes();
                    SavedWindow {
                        name: w.name.clone(),
                        layout: SavedNode::from_layout(&w.layout, &lookup)
                            .unwrap_or(SavedNode::Pane { pane: SavedPane { argv: Vec::new(), cwd: None } }),
                        active: order.iter().position(|p| *p == w.active).unwrap_or(0),
                        zoomed: w.zoomed,
                    }
                })
                .collect(),
        }
    }

    /// Save one session to its file; returns the path.
    fn save_session_file(&mut self, sid: SessionId) -> Result<PathBuf, String> {
        let s = self.session(sid).ok_or("no such session")?;
        let saved = self.snapshot(s);
        let key = serde_json::to_string(&saved).unwrap_or_default();
        let path = crate::resurrect::file_for(&self.sessions_dir(), &saved.name);
        crate::resurrect::SavedFile::new(saved).save(&path)?;
        self.last_saved.insert(path.to_string_lossy().into_owned(), key);
        Ok(path)
    }

    /// Autosave every session whose structure changed since its last save
    /// (called from the tick; cheap: the tree is tiny).
    fn autosave_changed(&mut self) {
        if !self.opts.autosave {
            return;
        }
        let dir = self.sessions_dir();
        let ids: Vec<SessionId> = self.sessions.iter().map(|s| s.id).collect();
        for sid in ids {
            let Some(s) = self.session(sid) else { continue };
            let saved = self.snapshot(s);
            let key = serde_json::to_string(&saved).unwrap_or_default();
            let path = crate::resurrect::file_for(&dir, &saved.name).to_string_lossy().into_owned();
            if self.last_saved.get(&path) == Some(&key) {
                continue;
            }
            match self.save_session_file(sid) {
                Ok(_) => {}
                Err(e) => {
                    log::warn!("autosave {}: {e}", saved.name);
                    // Do not retry every second.
                    self.last_saved.insert(path, key);
                }
            }
        }
    }

    /// Recreate one saved session (its name must be free). Returns its id.
    fn restore_session_file(
        &mut self,
        path: &std::path::Path,
        cols: u16,
        rows: u16,
    ) -> Result<(SessionId, Vec<String>), String> {
        use crate::resurrect::*;
        let ss = SavedFile::load(path)?.session;
        if self.sessions.iter().any(|s| s.name == ss.name) {
            return Err(format!("session {} already exists", ss.name));
        }
        let first = ss.windows.first().ok_or_else(|| format!("session {} has no windows", ss.name))?;
        let mut problems = Vec::new();
        // The session comes with its first window; the placeholder pane of
        // every window is replaced by the saved layout afterwards.
        let sid = self.new_session(Some(ss.name.clone()), Some(first.name.clone()), None, &[], cols, rows)?;
        for (i, sw) in ss.windows.iter().enumerate() {
            if i > 0
                && let Err(e) = self.new_window(sid, Some(sw.name.clone()), None, &[])
            {
                problems.push(format!("{}:{}: {e}", ss.name, sw.name));
                continue;
            }
            if let Err(e) = self.rebuild_window(sid, i, sw) {
                problems.push(format!("{}:{}: {e}", ss.name, sw.name));
            }
        }
        if let Some(s) = self.session_mut(sid) {
            s.cur = ss.current.min(s.windows.len().saturating_sub(1));
            s.last = None;
        }
        self.relayout_session(sid);
        Ok((sid, problems))
    }

    /// Restore every saved session whose name is free.
    fn restore_all(&mut self, cols: u16, rows: u16) -> (Vec<SessionId>, Vec<String>) {
        let mut created = Vec::new();
        let mut problems = Vec::new();
        for (name, _, path) in crate::resurrect::list(&self.sessions_dir()) {
            if self.sessions.iter().any(|s| s.name == name) {
                continue;
            }
            match self.restore_session_file(&path, cols, rows) {
                Ok((sid, p)) => {
                    created.push(sid);
                    problems.extend(p);
                }
                Err(e) => problems.push(e),
            }
        }
        (created, problems)
    }

    /// Replace window `widx`'s panes by the saved layout, spawning one pane
    /// per saved leaf (the placeholder pane created with the window is killed).
    fn rebuild_window(
        &mut self,
        sid: SessionId,
        widx: usize,
        sw: &crate::resurrect::SavedWindow,
    ) -> Result<(), String> {
        let area = {
            let s = self.session(sid).ok_or("no such session")?;
            self.window_area(s.cols, s.rows)
        };
        let mut panes = Vec::new();
        for saved in sw.layout.panes() {
            let argv: Vec<String> = saved.argv.clone();
            let cwd = saved.cwd.as_deref().filter(|d| std::path::Path::new(d).is_dir());
            // Sizes are refitted by relayout; spawn at the window size.
            panes.push(self.spawn_pane(&argv, cwd, area.w, area.h)?);
        }
        let mut ids = panes.iter().map(|p| p.id);
        let layout = sw.layout.to_layout(&mut ids).ok_or("layout/pane count mismatch")?;
        let order = layout.panes();
        let active = order.get(sw.active).or(order.first()).copied().ok_or("empty layout")?;
        let s = self.session_mut(sid).ok_or("no such session")?;
        let w = s.windows.get_mut(widx).ok_or("no such window")?;
        for p in &mut w.panes {
            p.kill();
        }
        w.panes = panes;
        w.layout = layout;
        w.active = active;
        w.last_pane = None;
        w.zoomed = sw.zoomed && w.panes.len() > 1;
        w.relayout(area);
        Ok(())
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
        if self.opts.restore_on_start && self.sessions.is_empty() {
            let (created, problems) = self.restore_all(80, 24);
            log::info!("restore-on-start: {} session(s)", created.len());
            for p in problems {
                log::warn!("restore-on-start: {p}");
            }
        }
    }

    /// Source a file of wmux commands, then load any `@plugin` it declared.
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
        let pending = std::mem::take(&mut self.opts.pending_plugins);
        for p in pending {
            if let Err(e) = self.load_plugin(&p) {
                errors.push(format!("{path}: @plugin {p}: {e}"));
            }
        }
        if errors.is_empty() { Ok(n) } else { Err(errors.join("\n")) }
    }

    /// A plugin is a directory holding `<name>.wmux` or `plugin.wmux`; `name`
    /// is a path or a directory name under `plugin-path`. Sourcing it once is
    /// all "loading" means: it binds keys, sets options, hooks and status
    /// pieces, and its scripts talk back through the `wmux` CLI.
    fn load_plugin(&mut self, name: &str) -> Result<(), String> {
        let name = name.trim_matches(['"', '\'']);
        let direct = std::path::PathBuf::from(expand_home(name));
        let dir = if direct.is_dir() {
            direct
        } else if direct.is_file() {
            let f = direct.to_string_lossy().into_owned();
            if self.plugins.contains(&f) {
                return Ok(());
            }
            self.plugins.push(f.clone());
            return self.source_file(&f).map(|_| ());
        } else {
            let base = std::path::PathBuf::from(expand_home(&self.opts.plugin_path));
            // `user/repo` (TPM style) -> last component.
            let short = name.rsplit(['/', '\\']).next().unwrap_or(name);
            let cand = base.join(short);
            if cand.is_dir() {
                cand
            } else {
                return Err(format!("plugin not found: {name} (looked in {})", base.display()));
            }
        };
        let short = dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let entry = [format!("{short}.wmux"), "plugin.wmux".into(), "plugin.conf".into()]
            .iter()
            .map(|f| dir.join(f))
            .find(|p| p.is_file())
            .ok_or_else(|| format!("{}: no {short}.wmux or plugin.wmux", dir.display()))?;
        let dir_s = dir.to_string_lossy().into_owned();
        if self.plugins.contains(&dir_s) {
            return Ok(());
        }
        self.plugins.push(dir_s);
        self.source_file(&entry.to_string_lossy()).map(|_| ())
    }

    /// Fire a hook's command, if one is set. Hooks never fire from inside a hook.
    fn fire_hook(&mut self, name: &str, cid: Option<ClientId>) {
        if self.in_hook {
            return;
        }
        let Some(cmd) = self.hooks.get(name).cloned() else { return };
        self.in_hook = true;
        let out = self.exec(cmd, cid);
        self.in_hook = false;
        if let Outcome::Error(e) = out {
            log::warn!("hook {name}: {e}");
        }
    }

    /// Run `command` through the shell on a worker thread; the result comes
    /// back as `Event::ShellDone` (or `StatusShell` when `status` is set).
    fn spawn_shell(&self, command: String, pane: Option<PaneId>, cid: Option<ClientId>, status: bool) {
        let events = self.events.clone();
        let mut env = self.pane_env(pane.unwrap_or(0));
        if pane.is_none() {
            env.retain(|(k, _)| k != "WMUX_PANE");
        }
        let spawned = std::thread::Builder::new().name("run-shell".into()).spawn({
            let events = events.clone();
            let command = command.clone();
            move || {
                // A status piece that hangs must not wedge its slot forever;
                // a foreground run-shell waits like tmux does.
                let timeout = if status { Some(STATUS_SHELL_TIMEOUT) } else { None };
                let (output, code) = run_shell_blocking(&command, &env, timeout);
                let ev = if status {
                    Event::StatusShell { command, output }
                } else {
                    Event::ShellDone { cid, output, code }
                };
                let _ = events.send(ev);
            }
        });
        if let Err(e) = spawned {
            // Never leave a client waiting on a Pending outcome.
            let ev = if status {
                Event::StatusShell { command, output: String::new() }
            } else {
                Event::ShellDone { cid, output: format!("cannot start a thread: {e}"), code: 127 }
            };
            let _ = events.send(ev);
        }
    }

    /// Kick off every `#(command)` the status line wants, at most once per
    /// `status-interval`, and never two copies of the same command at once.
    fn refresh_status_shells(&mut self) {
        let wanted = std::mem::take(&mut self.shell_cache.wanted);
        if wanted.is_empty() {
            return;
        }
        let due = self
            .shell_last_refresh
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(self.opts.status_interval.max(1)));
        for cmd in wanted {
            let fresh = self.shell_cache.results.contains_key(&cmd);
            if self.shell_running.contains(&cmd) || (fresh && !due) {
                continue;
            }
            self.shell_running.insert(cmd.clone());
            self.spawn_shell(cmd, None, None, true);
        }
        if due {
            self.shell_last_refresh = Some(Instant::now());
        }
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
                        repeat_until: None,
                        prompt: None,
                        message: None,
                        overlay: None,
                        chooser: None,
                        mouse_buttons: 0,
                        drag: None,
                        swallow_up: HashSet::new(),
                        pending: std::collections::VecDeque::new(),
                        window_hits: Vec::new(),
                    },
                );
            }
            Event::Gone(id) => {
                self.clients.remove(&id);
            }
            Event::Msg(id, msg) => self.handle_msg(id, msg),
            Event::ShellDone { cid, output, code } => {
                let text = output.trim_end().to_string();
                match cid {
                    Some(cid) => {
                        let out = if code == 0 {
                            if text.is_empty() { Outcome::Ok } else { Outcome::Text(text) }
                        } else {
                            Outcome::Error(if text.is_empty() { format!("shell exited with {code}") } else { text })
                        };
                        self.reply(cid, out);
                    }
                    None => {
                        if code != 0 {
                            log::warn!("run-shell exited with {code}: {text}");
                        }
                    }
                }
            }
            Event::StatusShell { command, output } => {
                self.shell_running.remove(&command);
                // First line only, like tmux.
                let line = output.lines().next().unwrap_or("").trim_end().to_string();
                self.shell_cache.results.insert(command, line);
            }
            Event::Tick => {
                self.autosave_changed();
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
            Outcome::Pending => {}
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
                c.repeat_until = None;
                c.prompt = None;
                c.overlay = config_errors;
                c.chooser = None;
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
                self.fire_hook("client-attached", Some(cid));
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
            self.fire_hook("client-detached", None);
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
            Some(i) => w
                .layout
                .panes()
                .get(i.wrapping_sub(self.opts.pane_base_index))
                .copied()
                .ok_or_else(|| format!("no pane {i}")),
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

    /// Environment for panes and `run-shell`: the socket, the pane, and PATH
    /// with this executable's directory first so `wmux` is callable from
    /// scripts and panes even when it was never installed.
    fn pane_env(&self, pane_id: PaneId) -> Vec<(String, String)> {
        vec![
            ("WMUX".into(), self.socket.clone()),
            ("WMUX_PANE".into(), pane_id.to_string()),
            ("PATH".into(), path_with_self()),
        ]
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
        self.fire_hook("pane-exited", None);
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
            synchronized: false,
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
        self.fire_hook("after-new-session", None);
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
                        let p = w.pane(*id);
                        // The pane's own size, not its rectangle: a zoomed
                        // window has no rectangle for the panes it hides, and
                        // those panes keep running at their previous size.
                        let (cols, rows) = p.map(|p| (p.cols, p.rows)).unwrap_or_default();
                        format!(
                            "{}: [{}x{}] %{id} {}{}{}",
                            i + self.opts.pane_base_index,
                            cols,
                            rows,
                            p.map(|p| p.display_title()).unwrap_or(""),
                            p.and_then(|p| p.cwd.as_deref()).map(|d| format!(" [{d}]")).unwrap_or_default(),
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
                // Last chance to save before the tree is gone.
                self.autosave_changed();
                let ids: Vec<SessionId> = self.sessions.iter().map(|s| s.id).collect();
                for sid in ids {
                    self.kill_session(sid, "server exited");
                }
                self.quit = true;
                Outcome::Ok
            }
            Cmd::SaveSession { target, all } => {
                let ids: Vec<SessionId> = if all {
                    self.sessions.iter().map(|s| s.id).collect()
                } else {
                    match self.resolve_session(target.as_ref(), cid) {
                        Ok(s) => vec![s],
                        Err(e) => return Outcome::Error(e),
                    }
                };
                let mut lines = Vec::new();
                for sid in ids {
                    match self.save_session_file(sid) {
                        Ok(p) => lines.push(format!("saved {}", p.display())),
                        Err(e) => return Outcome::Error(e),
                    }
                }
                Outcome::Text(lines.join("\n"))
            }
            Cmd::RestoreSession { name, attach } => {
                let client = cid.and_then(|c| self.clients.get(&c));
                let interactive = client.is_some_and(|c| c.interactive && c.session.is_none());
                let (cols, rows) = client.map(|c| (c.cols, c.rows)).unwrap_or((80, 24));
                let dir = self.sessions_dir();
                let (created, problems, target_sid) = match &name {
                    Some(n) => {
                        // Already running: resuming just means attaching.
                        if let Some(s) = self.sessions.iter().find(|s| s.name == *n) {
                            (Vec::new(), Vec::new(), Some(s.id))
                        } else {
                            let Some(path) = crate::resurrect::find(&dir, n) else {
                                return Outcome::Error(format!("no saved session named {n} (see list-saved)"));
                            };
                            match self.restore_session_file(&path, cols, rows) {
                                Ok((sid, p)) => (vec![sid], p, Some(sid)),
                                Err(e) => return Outcome::Error(e),
                            }
                        }
                    }
                    None => {
                        let (c, p) = self.restore_all(cols, rows);
                        let first = c.first().copied().or_else(|| self.resolve_session(None, cid).ok());
                        (c, p, first)
                    }
                };
                if attach
                    && interactive
                    && let Some(sid) = target_sid
                {
                    for p in &problems {
                        log::warn!("resume: {p}");
                    }
                    return Outcome::Attach(sid);
                }
                let mut lines = vec![format!("restored {} session(s)", created.len())];
                lines.extend(problems);
                if created.is_empty() && lines.len() == 1 && name.is_none() {
                    lines[0] = "nothing to restore (see list-saved)".into();
                }
                Outcome::Text(lines.join("\n"))
            }
            Cmd::ListSaved => {
                let dir = self.sessions_dir();
                let running: Vec<String> = self.sessions.iter().map(|s| s.name.clone()).collect();
                let lines: Vec<String> = crate::resurrect::list(&dir)
                    .into_iter()
                    .map(|(n, at, _)| {
                        let at = at.get(..19).unwrap_or(&at).replace('T', " ");
                        format!("{n}: saved {at}{}", if running.contains(&n) { " (running)" } else { "" })
                    })
                    .collect();
                if lines.is_empty() {
                    Outcome::Text(format!("no saved sessions in {}", dir.display()))
                } else {
                    Outcome::Text(lines.join("\n"))
                }
            }
            Cmd::DeleteSaved { name } => {
                let dir = self.sessions_dir();
                if self.opts.autosave && self.sessions.iter().any(|s| s.name == name) {
                    // Autosave would write it right back on the next tick.
                    return Outcome::Error(format!("session {name} is running; kill it first (or set autosave off)"));
                }
                match crate::resurrect::find(&dir, &name) {
                    Some(p) => match std::fs::remove_file(&p) {
                        Ok(()) => {
                            self.last_saved.remove(&p.to_string_lossy().into_owned());
                            Outcome::Ok
                        }
                        Err(e) => Outcome::Error(format!("{}: {e}", p.display())),
                    },
                    None => Outcome::Error(format!("no saved session named {name}")),
                }
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
                        let old = std::mem::replace(&mut self.session_mut(sid).unwrap().name, name.clone());
                        // The saved file follows the name; no ghost entry under the old one.
                        let dir = self.sessions_dir();
                        if let Some(p) = crate::resurrect::find(&dir, &old) {
                            let to = crate::resurrect::file_for(&dir, &name);
                            if let Err(e) = std::fs::rename(&p, &to) {
                                log::warn!("rename saved session {old} -> {name}: {e}");
                            }
                            self.last_saved.remove(&p.to_string_lossy().into_owned());
                        }
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
                if r.is_ok() {
                    self.fire_hook("after-new-window", cid);
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
                        self.fire_hook("after-select-window", cid);
                        Outcome::Ok
                    }
                    Err(e) => Outcome::Error(e),
                }
            }
            Cmd::NextWindow { ref target } | Cmd::PreviousWindow { ref target } | Cmd::LastWindow { ref target } => {
                let w = match cmd {
                    Cmd::NextWindow { .. } => "+",
                    Cmd::PreviousWindow { .. } => "-",
                    _ => "!",
                };
                let target = target.clone();
                // `-t` names the session to move around in; the window part is
                // ours ("the next one", "the last one").
                let session = target.and_then(|t| t.session);
                self.exec(Cmd::SelectWindow { target: Target { session, window: Some(w.into()), pane: None } }, cid)
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
                self.fire_hook("after-split-window", cid);
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
                self.fire_hook("after-kill-pane", cid);
                Outcome::Ok
            }
            Cmd::SelectPane { sel } => {
                let (sid, widx, _) = match self.resolve(None, cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                let (scols, srows) = self.session(sid).map(|s| (s.cols, s.rows)).unwrap();
                let area = self.window_area(scols, srows);
                let pane_base = self.opts.pane_base_index;
                let w = &mut self.session_mut(sid).unwrap().windows[widx];
                let order = w.layout.panes();
                let cur = order.iter().position(|p| *p == w.active).unwrap_or(0);
                let next = match sel {
                    PaneSel::Dir(d) => {
                        // Zoomed, the only rectangle is the zoomed pane itself,
                        // so directions are answered from the real layout (and
                        // moving unzooms, as it does for any other selection).
                        if w.zoomed {
                            let mut rects = Vec::new();
                            w.layout.layout(area, &mut rects);
                            layout::neighbour(&rects, w.active, d)
                        } else {
                            layout::neighbour(&w.rects, w.active, d)
                        }
                    }
                    PaneSel::Next => order.get((cur + 1) % order.len().max(1)).copied(),
                    PaneSel::Prev => order.get((cur + order.len().max(1) - 1) % order.len().max(1)).copied(),
                    PaneSel::Last => w.last_pane.filter(|l| w.pane(*l).is_some()),
                    PaneSel::Index(i) => order.get(i.wrapping_sub(pane_base)).copied(),
                };
                match next {
                    Some(id) if id != w.active => {
                        w.last_pane = Some(w.active);
                        w.active = id;
                        if w.zoomed {
                            w.zoomed = false;
                            self.relayout_session(sid);
                        }
                        self.fire_hook("after-select-pane", cid);
                        Outcome::Ok
                    }
                    Some(_) => Outcome::Ok,
                    None => Outcome::Error("no such pane".into()),
                }
            }
            Cmd::ResizePane { dir, amount, zoom, target } => {
                let (sid, widx, pid) = match self.resolve(target.as_ref(), cid) {
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
            Cmd::SwapPane { up, target } => {
                let (sid, widx, pid) = match self.resolve(target.as_ref(), cid) {
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
            Cmd::BreakPane { target } => {
                let (sid, widx, pid) = match self.resolve(target.as_ref(), cid) {
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
                    synchronized: false,
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
                // With synchronize-panes on, send-keys reaches the whole window too.
                let synced = self
                    .sessions
                    .iter()
                    .flat_map(|s| s.windows.iter())
                    .find(|w| w.pane(pid).is_some())
                    .is_some_and(|w| w.synchronized);
                if synced {
                    let w = self
                        .sessions
                        .iter_mut()
                        .flat_map(|s| s.windows.iter_mut())
                        .find(|w| w.pane(pid).is_some())
                        .unwrap();
                    for p in &mut w.panes {
                        p.write_input(&bytes);
                    }
                } else if let Some(p) = self.find_pane_mut(pid) {
                    p.write_input(&bytes);
                }
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
            Cmd::BindKey { root, key, repeat, cmd } => match Key::parse(&key) {
                Some(k) => {
                    let b = Binding { cmd: *cmd, repeat };
                    if root { &mut self.root_binds } else { &mut self.prefix_binds }.insert(k, b);
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
            Cmd::SetOption { name, value } if name == "synchronize-panes" => {
                // Window-scoped in tmux; here it applies to the current window.
                let on = match value.trim().to_ascii_lowercase().as_str() {
                    "on" | "true" | "yes" | "1" => Some(true),
                    "off" | "false" | "no" | "0" => Some(false),
                    "" => None, // toggle
                    v => return Outcome::Error(format!("bad boolean '{v}'")),
                };
                let (sid, widx, _) = match self.resolve(None, cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                let w = &mut self.session_mut(sid).unwrap().windows[widx];
                w.synchronized = on.unwrap_or(!w.synchronized);
                Outcome::Ok
            }
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
            Cmd::ChooseTree { sessions, windows } => {
                let Some(cid) = cid else { return Outcome::Error("choose-tree: no client".into()) };
                let Some(sid) = self.clients.get(&cid).and_then(|c| c.session) else {
                    return Outcome::Error("choose-tree: client not attached".into());
                };
                let expand = windows || !sessions;
                let (items, lines) = self.chooser_lines(expand);
                // Start on the current window (or session).
                let cur = self.session(sid).and_then(|s| s.window()).map(|w| w.id).filter(|_| expand);
                let sel = items.iter().position(|i| *i == (sid, cur)).unwrap_or(0);
                let c = self.clients.get_mut(&cid).unwrap();
                // One modal at a time: a picker opened from the `:` prompt
                // replaces it, or its keys would go to an invisible prompt.
                c.prompt = None;
                c.overlay = None;
                c.chooser = Some(Chooser { expand, items, lines, sel, top: 0 });
                Outcome::Ok
            }
            Cmd::ListKeys => {
                let mut lines: Vec<String> = self
                    .prefix_binds
                    .iter()
                    .map(|(k, b)| format!("bind-key {}-T prefix {k:<10} {}", if b.repeat { "-r " } else { "" }, b.cmd))
                    .collect();
                lines.extend(self.root_binds.iter().map(|(k, b)| {
                    format!("bind-key {}-T root   {k:<10} {}", if b.repeat { "-r " } else { "" }, b.cmd)
                }));
                lines.sort();
                Outcome::Text(lines.join("\n"))
            }
            Cmd::SetCwd { target, dir } => {
                // No target: the pane the client runs in (WMUX_PANE), else the active one.
                let pid = match (&target, cid.and_then(|c| self.clients.get(&c)).and_then(|c| c.pane_env)) {
                    (None, Some(p)) if self.find_pane_mut(p).is_some() => p,
                    _ => match self.resolve(target.as_ref(), cid) {
                        Ok((_, _, p)) => p,
                        Err(e) => return Outcome::Error(e),
                    },
                };
                let client_cwd = self.client_cwd(cid);
                let dir = match dir.clone().or_else(|| client_cwd.clone()) {
                    Some(d) => expand_home(&d),
                    None => return Outcome::Error("set-cwd: no directory given and the client has none".into()),
                };
                let dir = pane::windows_path_from_announced(&dir).unwrap_or(dir);
                // A relative directory is relative to where the client runs, not the server.
                let dir = match (std::path::Path::new(&dir).is_relative(), &client_cwd) {
                    (true, Some(base)) => std::path::Path::new(base).join(&dir).to_string_lossy().into_owned(),
                    _ => dir,
                };
                if !std::path::Path::new(&dir).is_dir() {
                    return Outcome::Error(format!("set-cwd: not a directory: {dir}"));
                }
                match self.find_pane_mut(pid) {
                    Some(p) => {
                        p.cwd = Some(dir);
                        Outcome::Ok
                    }
                    None => Outcome::Error("no such pane".into()),
                }
            }
            Cmd::CapturePane { target, history } => {
                let (_, _, pid) = match self.resolve(target.as_ref(), cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                let Some(p) = self.find_pane_mut(pid) else { return Outcome::Error("no such pane".into()) };
                let total = p.scrollback_len();
                let rows = p.rows as usize;
                let from = total.saturating_sub(history);
                let mut lines: Vec<String> = Vec::with_capacity(total - from + rows);
                for abs in from..total + rows {
                    lines.push(p.line_text(abs).0.trim_end().to_string());
                }
                while lines.last().is_some_and(|l| l.is_empty()) {
                    lines.pop();
                }
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
            Cmd::ShowOptions { name, value_only, quiet } => match name {
                Some(n) => match self.opts.get(&n) {
                    Some(v) => Outcome::Text(if value_only { v } else { format!("{n} {}", crate::command::quote(&v)) }),
                    None if quiet => Outcome::Text(String::new()),
                    None => Outcome::Error(format!("unknown option: {n}")),
                },
                None => {
                    let mut lines: Vec<String> = crate::config::SHOWABLE
                        .iter()
                        .filter_map(|n| self.opts.get(n).map(|v| format!("{n} {}", crate::command::quote(&v))))
                        .collect();
                    for (k, v) in &self.opts.user {
                        lines.push(format!("{k} {}", crate::command::quote(v)));
                    }
                    Outcome::Text(lines.join("\n"))
                }
            },
            Cmd::RunShell { command, background, target } => {
                let pane = self.resolve(target.as_ref(), cid).ok().map(|(_, _, p)| p);
                // From a config file (no client) or with -b the output goes to the log.
                let reply_to = if background { None } else { cid };
                self.spawn_shell(command, pane, reply_to, false);
                if reply_to.is_some() { Outcome::Pending } else { Outcome::Ok }
            }
            Cmd::SetHook { hook, cmd } => {
                if !HOOKS.contains(&hook.as_str()) {
                    return Outcome::Error(format!("unknown hook: {hook} (known: {})", HOOKS.join(", ")));
                }
                match cmd {
                    Some(c) => {
                        self.hooks.insert(hook, *c);
                    }
                    None => {
                        self.hooks.remove(&hook);
                    }
                }
                Outcome::Ok
            }
            Cmd::ShowHooks => {
                let mut lines: Vec<String> =
                    self.hooks.iter().map(|(h, c)| format!("{h} {}", crate::command::quote(&c.to_string()))).collect();
                lines.sort();
                Outcome::Text(lines.join("\n"))
            }
            Cmd::LoadPlugin { path } => self.load_plugin(&path).into(),
            Cmd::ListPlugins => {
                if self.plugins.is_empty() {
                    Outcome::Text("no plugins loaded".into())
                } else {
                    Outcome::Text(self.plugins.join("\n"))
                }
            }
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
            .replace("#P", &(pidx + self.opts.pane_base_index).to_string())
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
        // The picker is a mode, like copy mode: the prefix still works (so
        // `prefix d` detaches out of it), everything else belongs to the mode.
        let in_chooser = self.clients.get(&cid).is_some_and(|c| c.chooser.is_some());

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
                    Some(b) => {
                        // `bind -r`: the same table answers bare keys for a
                        // while, so `prefix h h h` walks three panes left.
                        let until = (b.repeat && self.opts.repeat_time_ms > 0)
                            .then(|| Instant::now() + Duration::from_millis(self.opts.repeat_time_ms));
                        let out = self.exec(b.cmd, Some(cid));
                        self.reply(cid, out);
                        if let Some(c) = self.clients.get_mut(&cid) {
                            c.repeat_until = until;
                        }
                    }
                    None => self.message(cid, &format!("unbound key: {k}")),
                }
                return;
            }
            // Still inside the repeat window: a repeatable key runs again.
            if c.repeat_until.is_some_and(|t| Instant::now() < t) {
                match self.prefix_binds.get(&k).cloned() {
                    Some(b) if b.repeat => {
                        let c = self.clients.get_mut(&cid).unwrap();
                        c.swallow_up.insert(rec.vk);
                        c.repeat_until = Some(Instant::now() + Duration::from_millis(self.opts.repeat_time_ms));
                        let out = self.exec(b.cmd, Some(cid));
                        self.reply(cid, out);
                        return;
                    }
                    // Anything else ends the repeat and is handled normally.
                    _ => self.clients.get_mut(&cid).unwrap().repeat_until = None,
                }
            }
            let c = self.clients.get_mut(&cid).unwrap();
            if k == self.opts.prefix {
                c.prefix = true;
                c.swallow_up.insert(rec.vk);
                return;
            }
            if in_chooser {
                c.swallow_up.insert(rec.vk);
                self.chooser_key(cid, k);
                return;
            }
            if in_copy {
                c.swallow_up.insert(rec.vk);
                self.copy_key(cid, sid, k);
                return;
            }
            if let Some(b) = self.root_binds.get(&k).cloned() {
                c.swallow_up.insert(rec.vk);
                let until = (b.repeat && self.opts.repeat_time_ms > 0)
                    .then(|| Instant::now() + Duration::from_millis(self.opts.repeat_time_ms));
                let out = self.exec(b.cmd, Some(cid));
                self.reply(cid, out);
                if let Some(c) = self.clients.get_mut(&cid) {
                    c.repeat_until = until;
                }
                return;
            }
        } else if in_copy || in_chooser {
            return;
        }
        self.write_active(sid, &input::encode_key_record(&rec));
    }

    /// Input for the current window: the active pane, or every pane when
    /// `synchronize-panes` is on.
    fn write_active(&mut self, sid: SessionId, bytes: &[u8]) {
        let Some(w) = self.session_mut(sid).and_then(|s| s.window_mut()) else { return };
        if w.synchronized {
            for p in &mut w.panes {
                p.write_input(bytes);
            }
        } else if let Some(p) = w.active_pane_mut() {
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

    // ----------------------------------------------------------- choose-tree

    /// The picker's lines from the live sessions, tmux `choose-tree` style:
    /// `(n) - name: 2 windows (attached)` and, when expanded,
    /// `(n)   - 0: cmd* (2 panes) "title"` under each session.
    fn chooser_lines(&self, expand: bool) -> (Vec<(SessionId, Option<WindowId>)>, Vec<String>) {
        let mut items = Vec::new();
        let mut lines = Vec::new();
        let tag = |n: usize| if n < 10 { format!("({n}) ") } else { "    ".to_string() };
        for s in &self.sessions {
            let attached = self.clients.values().any(|c| c.session == Some(s.id));
            items.push((s.id, None));
            lines.push(format!(
                "{}{} {}: {} windows{}",
                tag(lines.len()),
                if expand { "-" } else { "+" },
                s.name,
                s.windows.len(),
                if attached { " (attached)" } else { "" }
            ));
            if !expand {
                continue;
            }
            for (i, w) in s.windows.iter().enumerate() {
                let flag = if i == s.cur {
                    "*"
                } else if Some(w.id) == s.last {
                    "-"
                } else {
                    ""
                };
                let title = w.active_pane().map(|p| p.title.as_str()).unwrap_or("");
                items.push((s.id, Some(w.id)));
                lines.push(format!(
                    "{}  - {}: {}{} ({} panes) \"{}\"",
                    tag(lines.len()),
                    i + self.opts.base_index,
                    w.name,
                    flag,
                    w.panes.len(),
                    title
                ));
            }
        }
        (items, lines)
    }

    fn chooser_key(&mut self, cid: ClientId, k: Key) {
        let Some((cols, rows)) = self.clients.get(&cid).map(|c| (c.cols, c.rows)) else { return };
        let page = self.window_area(cols, rows).h.saturating_sub(1).max(1) as i64; // body rows of the picker
        let Some(c) = self.clients.get_mut(&cid) else { return };
        let Some(ch) = c.chooser.as_mut() else { return };
        match (k.code, k.ctrl, k.alt) {
            (KeyCode::Escape, _, _) | (KeyCode::Char('q'), false, false) | (KeyCode::Char('c'), true, _) => {
                c.chooser = None;
            }
            (KeyCode::Down, _, _) | (KeyCode::Char('j'), false, false) | (KeyCode::Char('n'), true, _) => ch.step(1),
            (KeyCode::Up, _, _) | (KeyCode::Char('k'), false, false) | (KeyCode::Char('p'), true, _) => ch.step(-1),
            (KeyCode::Home, _, _) | (KeyCode::Char('g'), false, false) => ch.sel = 0,
            (KeyCode::End, _, _) | (KeyCode::Char('G'), false, false) => ch.sel = ch.items.len().saturating_sub(1),
            (KeyCode::NPage, _, _) | (KeyCode::Char('f'), true, _) => ch.step(page),
            (KeyCode::PPage, _, _) | (KeyCode::Char('b'), true, _) => ch.step(-page),
            (KeyCode::Char('d'), true, _) => ch.step(page / 2),
            (KeyCode::Char('u'), true, _) => ch.step(-(page / 2)),
            (KeyCode::Char(d @ '0'..='9'), false, false) => {
                let i = d as usize - '0' as usize;
                if i < ch.items.len() {
                    ch.sel = i;
                }
            }
            (KeyCode::Enter, _, _) => {
                let target = ch.items.get(ch.sel).copied();
                c.chooser = None;
                if let Some((sid, wid)) = target {
                    self.chooser_go(cid, sid, wid);
                }
            }
            _ => {}
        }
    }

    /// Switch the client to `sid` and, when given, make `wid` its window.
    fn chooser_go(&mut self, cid: ClientId, sid: SessionId, wid: Option<WindowId>) {
        let Some(s) = self.session(sid) else {
            self.message(cid, "session is gone");
            return;
        };
        let widx = match wid {
            Some(w) => match s.windows.iter().position(|x| x.id == w) {
                Some(i) => Some(i),
                None => {
                    self.message(cid, "window is gone");
                    return;
                }
            },
            None => None,
        };
        let name = s.name.clone();
        let out = self.exec(
            Cmd::SwitchClient {
                next: false,
                prev: false,
                target: Some(Target { session: Some(name), ..Default::default() }),
            },
            Some(cid),
        );
        if let Outcome::Error(e) = out {
            self.message(cid, &e);
            return;
        }
        // Same effect as `select-window -t`, hook included.
        if let Some(i) = widx
            && let Some(s) = self.session_mut(sid)
        {
            s.select_window(i);
            self.fire_hook("after-select-window", Some(cid));
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
        if c.chooser.is_some() {
            // Wheel moves the selection; a click puts it on that line.
            let (cols, rows) = (c.cols, c.rows);
            let area = self.window_area(cols, rows);
            let ch = self.clients.get_mut(&cid).unwrap().chooser.as_mut().unwrap();
            if m.flags & MOUSE_WHEELED != 0 {
                ch.step(if (m.buttons >> 16) as i16 > 0 { -1 } else { 1 });
            } else if buttons & 1 != 0 && prev & 1 == 0 && y >= area.y && y + 1 < area.y + area.h {
                let i = ch.top + (y - area.y) as usize;
                if i < ch.items.len() {
                    ch.sel = i;
                }
            }
            return;
        }
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

    fn status_click(&mut self, cid: ClientId, sid: SessionId, x: u16) {
        // Hit ranges recorded by the last render of this client.
        let hit = self.clients.get(&cid).and_then(|c| c.window_hits.iter().position(|(a, b)| x >= *a && x < *b));
        if let Some(i) = hit
            && let Some(s) = self.session_mut(sid)
        {
            s.select_window(i);
            self.fire_hook("after-select-window", Some(cid));
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
        // The picker shows the live tree; keep the selection on the same item.
        if let Some((expand, want, sel)) =
            c.chooser.as_ref().map(|ch| (ch.expand, ch.items.get(ch.sel).copied(), ch.sel))
        {
            let (items, lines) = self.chooser_lines(expand);
            let ch = self.clients.get_mut(&cid).unwrap().chooser.as_mut().unwrap();
            ch.sel =
                want.and_then(|w| items.iter().position(|i| *i == w)).unwrap_or(sel.min(items.len().saturating_sub(1)));
            ch.items = items;
            ch.lines = lines;
        }
        let Some(spos) = self.sessions.iter().position(|s| s.id == sid) else { return };
        let pane_base_index = self.opts.pane_base_index;
        let (status_top, border_fg, active_fg, base_index, opts_status) = (
            self.opts.status_top,
            self.opts.pane_border_fg,
            self.opts.pane_border_active_fg,
            self.opts.base_index,
            self.opts.status,
        );
        let (message, prompt) = {
            let c = self.clients.get(&cid).unwrap();
            (
                c.message.as_ref().map(|(m, _)| m.clone()),
                c.prompt.as_ref().map(|p| (p.label.clone(), p.input.clone(), p.cursor)),
            )
        };
        let mut bell = false;
        let status_line = if opts_status {
            let base = render::Style::colors(self.opts.status_fg, self.opts.status_bg);
            let now = chrono::Local::now();
            let s = &self.sessions[spos];
            let host = std::env::var("COMPUTERNAME").unwrap_or_default();
            let ctx_for = |s: &Session, widx: usize| -> crate::format::Context {
                let w = &s.windows[widx];
                let pane = w.active_pane();
                let pidx = w.layout.panes().iter().position(|p| *p == w.active).unwrap_or(0);
                crate::format::Context {
                    session: s.name.clone(),
                    window: w.name.clone(),
                    window_index: widx + base_index,
                    pane_index: pidx + pane_base_index,
                    pane_title: pane.map(|p| truncate(p.display_title(), 30)).unwrap_or_default(),
                    pane_command: pane.map(|p| p.command.clone()).unwrap_or_default(),
                    pane_path: pane.and_then(|p| p.cwd.clone()).unwrap_or_default(),
                    host: host.clone(),
                    flags: format!(
                        "{}{}",
                        if widx == s.cur {
                            "*"
                        } else if Some(w.id) == s.last {
                            "-"
                        } else {
                            ""
                        },
                        match (w.zoomed, w.synchronized) {
                            (true, true) => "ZS",
                            (true, false) => "Z",
                            (false, true) => "S",
                            (false, false) => "",
                        }
                    ),
                }
            };
            let cache = &mut self.shell_cache;
            let cur_ctx = ctx_for(s, s.cur);
            let mut left = crate::format::expand(&self.opts.status_left, &cur_ctx, cache, base, now);
            let mut right = crate::format::expand(&self.opts.status_right, &cur_ctx, cache, base, now);
            clip_segments(&mut left, self.opts.status_left_length);
            clip_segments(&mut right, self.opts.status_right_length);
            let windows = (0..s.windows.len())
                .map(|i| {
                    let fmt = if i == s.cur {
                        &self.opts.window_status_current_format
                    } else {
                        &self.opts.window_status_format
                    };
                    (crate::format::expand(fmt, &ctx_for(s, i), cache, base, now), i == s.cur)
                })
                .collect();
            Some(StatusLine { left, windows, right, message, prompt, fg: self.opts.status_fg, bg: self.opts.status_bg })
        } else {
            None
        };
        self.refresh_status_shells();
        let s = &mut self.sessions[spos];
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
        let (mut grid, mut cursor, window_hits) = render::compose(&frame);
        for p in &mut w.panes {
            if p.copy.is_some() {
                p.parser.screen_mut().set_scrollback(0);
            }
        }
        let area = self.window_area(cols, rows);
        let c = self.clients.get_mut(&cid).unwrap();
        c.window_hits = window_hits;
        if let Some(ch) = c.chooser.as_mut() {
            let body_h = area.h.saturating_sub(1) as usize;
            if ch.sel < ch.top {
                ch.top = ch.sel;
            } else if body_h > 0 && ch.sel >= ch.top + body_h {
                ch.top = ch.sel + 1 - body_h;
            }
            render::draw_chooser(&mut grid, area, &ch.lines, ch.sel, ch.top);
            cursor = None;
        }
        // An overlay (a hook's message, a `run-shell` result) draws over the
        // picker, because the next key goes to the overlay, not the picker.
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

/// Truncate a segment list to `max` display cells (tmux `status-*-length`).
fn clip_segments(segs: &mut Vec<crate::format::Segment>, max: usize) {
    use unicode_width::UnicodeWidthChar;
    let mut left = max;
    for s in segs.iter_mut() {
        let mut keep = String::new();
        for ch in s.text.chars() {
            let w = ch.width().unwrap_or(0);
            if w > left {
                break;
            }
            left -= w;
            keep.push(ch);
        }
        s.text = keep;
    }
    segs.retain(|s| !s.text.is_empty());
}

/// PATH with the directory of the running executable prepended (once).
fn path_with_self() -> String {
    let path = std::env::var("PATH").unwrap_or_default();
    let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) else {
        return path;
    };
    let dir_s = dir.to_string_lossy().into_owned();
    let already = std::env::split_paths(&path).any(|p| p == dir);
    if already { path } else { format!("{dir_s};{path}") }
}

/// Longest a status-line `#(command)` may run before it is killed.
const STATUS_SHELL_TIMEOUT: Duration = Duration::from_secs(30);

/// Run a shell command to completion, returning (stdout+stderr, exit code).
/// Uses pwsh when available, else Windows PowerShell, else cmd. With a
/// `timeout` the process tree is killed when it expires (exit code 124).
fn run_shell_blocking(command: &str, env: &[(String, String)], timeout: Option<Duration>) -> (String, i32) {
    // `-Command` alone reports 0/1; make a native command's exit code
    // propagate like `sh -c` does for tmux.
    // Without a console PowerShell writes the ANSI code page; ask for UTF-8 so
    // non-ASCII output survives the trip into the status line / overlay.
    let ps_script = format!(
        "[Console]::OutputEncoding = [Text.Encoding]::UTF8; $OutputEncoding = [Text.Encoding]::UTF8\n\
         {command}\nif ($LASTEXITCODE) {{ exit $LASTEXITCODE }}"
    );
    let (exe, args): (&str, Vec<&str>) = if crate::config::which("pwsh.exe").is_some() {
        ("pwsh.exe", vec!["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &ps_script])
    } else if crate::config::which("powershell.exe").is_some() {
        ("powershell.exe", vec!["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &ps_script])
    } else {
        ("cmd.exe", vec!["/d", "/c", command])
    };
    let mut c = std::process::Command::new(exe);
    c.args(&args).stdin(std::process::Stdio::null());
    for (k, v) in env {
        c.env(k, v);
    }
    #[allow(unused_imports)]
    use std::os::windows::process::CommandExt;
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: the server has no console
    c.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let mut job = crate::winsec::KillOnCloseJob::new().ok();
    let child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return (format!("{exe}: {e}"), 127),
    };
    if let Some(j) = &job {
        use std::os::windows::io::AsRawHandle;
        // Straight from CreateProcess; killing the job takes grandchildren too.
        let _ = unsafe { j.assign(child.as_raw_handle() as _) };
    }
    // Watchdog: close the job (killing the tree) when the timeout expires.
    // Without a timeout the job simply lives until the command is done.
    let timed_out = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    if let Some(t) = timeout
        && let Some(j) = job.take()
    {
        let (timed_out, done) = (timed_out.clone(), done.clone());
        std::thread::spawn(move || {
            let deadline = Instant::now() + t;
            while Instant::now() < deadline {
                if done.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            timed_out.store(true, std::sync::atomic::Ordering::SeqCst);
            drop(j);
        });
    }
    let out = child.wait_with_output();
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    drop(job);
    match out {
        Ok(o) => {
            let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
            let err = String::from_utf8_lossy(&o.stderr);
            if !err.trim().is_empty() {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&err);
            }
            if timed_out.load(std::sync::atomic::Ordering::SeqCst) {
                return (format!("{text}\n[timed out after {}s]", timeout.map_or(0, |t| t.as_secs())), 124);
            }
            (text, o.status.code().unwrap_or(1))
        }
        Err(e) => (format!("{exe}: {e}"), 127),
    }
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
            repeat_until: None,
            prompt: None,
            message: None,
            overlay: None,
            chooser: None,
            mouse_buttons: 0,
            drag: None,
            swallow_up: HashSet::new(),
            pending: std::collections::VecDeque::new(),
            window_hits: Vec::new(),
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
    fn path_with_self_contains_our_directory_exactly_once() {
        // (cargo already puts target/debug/deps on PATH for tests, so the
        // interesting property is "present, and not duplicated".)
        let dir = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
        let p = path_with_self();
        let count = std::env::split_paths(&p).filter(|x| *x == dir).count();
        let before = std::env::split_paths(&std::env::var("PATH").unwrap()).filter(|x| *x == dir).count();
        assert_eq!(count, before.max(1));
        if before == 0 {
            assert_eq!(std::env::split_paths(&p).next().unwrap(), dir);
        }
    }

    #[test]
    fn run_shell_reports_exit_code_and_utf8() {
        let (out, code) = run_shell_blocking("Write-Output 中文-ok; exit 3", &[], None);
        assert_eq!(code, 3);
        assert!(out.contains("中文-ok"), "{out:?}");
        let (out, code) =
            run_shell_blocking("Write-Output $env:WMUX_TEST_VAR", &[("WMUX_TEST_VAR".into(), "v1".into())], None);
        assert_eq!(code, 0);
        assert_eq!(out.trim(), "v1");
    }

    #[test]
    fn run_shell_timeout_kills_the_tree() {
        let start = Instant::now();
        let (out, code) = run_shell_blocking(
            "Write-Output first-line; ping -n 30 127.0.0.1 > $null; Write-Output never-reached",
            &[],
            Some(Duration::from_secs(2)),
        );
        assert_eq!(code, 124, "{out:?}");
        assert!(out.contains("first-line") && !out.contains("never-reached"), "{out:?}");
        assert!(start.elapsed() < Duration::from_secs(10), "took {:?}", start.elapsed());
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
