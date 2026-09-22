//! The wmux server: owns sessions, windows and panes; talks to clients over a
//! named pipe; renders frames.

pub mod input;
pub mod layout;
pub mod pane;
pub mod render;

use crate::command::{Cmd, Dir, MenuItem, PaneSel, Target};
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
    /// `/` or `?` in copy mode: the input is a pattern, not a command.
    Search {
        back: bool,
    },
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
/// What one line of the picker stands for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ChooserItem {
    /// A session line, or one of its windows.
    Tree(SessionId, Option<WindowId>),
    /// A paste buffer, by position in the buffer list.
    Buffer(usize),
    /// An attached client.
    Client(ClientId),
    /// An entry of a `display-menu`, by position.
    Menu(usize),
    /// A pane on the task board (`choose-jobs`).
    Job(SessionId, WindowId, PaneId),
    /// A title or separator line: shown, never selected.
    Separator,
}

/// Where a picker's lines come from.
enum ChooserKind {
    /// The live session tree; `expand` shows each session's windows.
    Tree { expand: bool },
    /// The paste buffers.
    Buffers,
    /// The attached clients.
    Clients,
    /// Every pane on the server with its state (`jobs`), to jump to, kill
    /// or restart.
    Jobs,
    /// A fixed list of windows (the `find-window` hits).
    Found,
    /// A `display-menu` and the commands its entries run.
    Menu(Vec<MenuItem>),
}

impl ChooserKind {
    /// Whether the list is rebuilt from the server state at every render.
    fn live(&self) -> bool {
        matches!(self, ChooserKind::Tree { .. } | ChooserKind::Buffers | ChooserKind::Clients | ChooserKind::Jobs)
    }
}

struct Chooser {
    kind: ChooserKind,
    items: Vec<ChooserItem>,
    lines: Vec<String>,
    sel: usize,
    top: usize,
}

impl Chooser {
    fn step(&mut self, delta: i64) {
        let last = self.items.len().saturating_sub(1) as i64;
        let from = self.sel;
        self.sel = (self.sel as i64 + delta).clamp(0, last) as usize;
        if !self.settle(if delta < 0 { -1 } else { 1 }) {
            self.sel = from; // nothing selectable anywhere: do not move at all
        }
    }

    /// Move off a title or separator line, preferring direction `dir` and
    /// falling back to the other one. False when the list has nothing that
    /// can be selected.
    fn settle(&mut self, dir: i64) -> bool {
        let last = self.items.len().saturating_sub(1) as i64;
        for d in [dir, -dir] {
            let mut i = self.sel as i64;
            while (0..=last).contains(&i) {
                if !matches!(self.items.get(i as usize), Some(ChooserItem::Separator)) {
                    self.sel = i as usize;
                    return true;
                }
                i += d;
            }
        }
        false
    }
}

/// A `display-popup`: its own pane, drawn in a box over the client's window
/// and fed every key until it closes. The size is kept as it was asked for so
/// the box follows the terminal when it is resized.
struct Popup {
    pane: Pane,
    rect: Rect,
    width: Option<String>,
    height: Option<String>,
    /// `-E`: go away as soon as the command finishes.
    close_on_exit: bool,
    /// The command has finished and the next key closes the box.
    finished: bool,
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
    /// Session this client was in before the current one (switch-client -l).
    last_session: Option<SessionId>,
    last_grid: Option<Grid>,
    last_cursor: Option<(u16, u16)>,
    prefix: bool,
    /// A `bind -r` key ran; until this instant its table answers bare keys.
    repeat_until: Option<Instant>,
    /// `display-panes` is showing the pane numbers until this instant.
    panes_until: Option<Instant>,
    prompt: Option<Prompt>,
    message: Option<(String, Instant)>,
    /// Multi-line command output shown over the window until a key is pressed
    /// (tmux "view mode"), e.g. `list-keys`.
    overlay: Option<Vec<String>>,
    chooser: Option<Chooser>,
    /// `display-popup`: a program in a box over the window, taking the keys.
    popup: Option<Popup>,
    mouse_buttons: u32,
    drag: Option<Drag>,
    /// Virtual keys whose key-down we consumed; drop the matching key-up.
    swallow_up: HashSet<u16>,
    /// A bell from a background window, to ring at the next render.
    pending_bell: bool,
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
    /// Last layout applied by `select-layout`, so `-n` knows where to go next.
    layout_preset: Option<layout::Preset>,
    /// `synchronize-panes`: input goes to every pane of the window.
    synchronized: bool,
    /// Where each pane's content is drawn: its layout cell less the row a
    /// `pane-border-status` line takes.
    rects: Vec<(PaneId, Rect)>,
    /// The layout cells themselves, which is what splitting divides.
    layout_rects: Vec<(PaneId, Rect)>,
    /// Alerts waiting to be seen (tmux `#F`): a background window printed
    /// something, rang the bell, or went quiet. Cleared when the window is
    /// drawn, which is when the alert has done its job.
    alert_activity: bool,
    alert_bell: bool,
    alert_silence: bool,
    /// When this window last printed anything (`monitor-silence`).
    last_output: Instant,
}

impl Window {
    /// The `#F` flags: where the window sits, what it is waiting to tell you,
    /// and how it is laid out — in tmux's order.
    fn flags(&self, current: bool, last: bool) -> String {
        let mut s = String::new();
        if current {
            s.push('*');
        } else if last {
            s.push('-');
        }
        if self.alert_activity {
            s.push('#');
        }
        if self.alert_bell {
            s.push('!');
        }
        if self.alert_silence {
            s.push('~');
        }
        if self.zoomed {
            s.push('Z');
        }
        if self.synchronized {
            s.push('S');
        }
        s
    }

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
    /// The pane's layout cell, border-status row included: what a split
    /// divides. Dividing the drawn rect instead leaves the tree one row
    /// short, and the layout hands that row to the first pane (11/9, not
    /// 10/10).
    fn layout_rect(&self, id: PaneId) -> Option<Rect> {
        self.layout_rects.iter().find(|(i, _)| *i == id).map(|(_, r)| *r)
    }
    fn pane_at(&self, x: u16, y: u16) -> Option<PaneId> {
        self.rects.iter().find(|(_, r)| r.contains(x, y)).map(|(id, _)| *id)
    }

    /// Recompute pane rectangles for a window area and resize the panes.
    /// `border`: `pane-border-status` wants a row above (`Some(true)`) or
    /// below (`Some(false)`) every pane, which is taken off the pane itself
    /// and left for the border text.
    fn relayout(&mut self, area: Rect, border: Option<bool>) {
        self.rects.clear();
        if self.zoomed && self.pane(self.active).is_some() {
            self.rects.push((self.active, area));
        } else {
            self.zoomed = false;
            self.layout.layout(area, &mut self.rects);
        }
        self.layout_rects = self.rects.clone();
        if let Some(top) = border {
            for (_, r) in &mut self.rects {
                if r.h >= 2 {
                    if top {
                        r.y += 1;
                    }
                    r.h -= 1;
                }
            }
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
    /// Unix time of creation, for `#{session_created}`.
    created_at: i64,
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

/// One `wait-for` channel: a lock with its queue, and the wait/signal pair.
/// A client that is waiting has had no reply sent yet, so its `wmux wait-for`
/// is still sitting there; waking it means answering that command.
#[derive(Default)]
struct WaitChannel {
    locked: bool,
    /// Clients blocked in `wait-for -L`, in arrival order.
    lockers: std::collections::VecDeque<ClientId>,
    /// Clients blocked in a plain `wait-for`.
    waiters: Vec<ClientId>,
    /// A signal arrived with nobody waiting: the next wait returns at once
    /// (tmux keeps the same one-shot flag).
    woken: bool,
}

impl WaitChannel {
    fn idle(&self) -> bool {
        !self.locked && !self.woken && self.lockers.is_empty() && self.waiters.is_empty()
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
    /// Restore saved sessions at start even with `restore-on-start off`.
    force_restore: bool,
    /// A config file given at start, in place of the usual search.
    config_override: Option<PathBuf>,
    /// Files being sourced right now, outermost first (loop detection).
    sourcing: Vec<String>,
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
    /// Recent status-line messages, newest last (`show-messages`).
    messages: std::collections::VecDeque<String>,
    /// Paste buffers, newest first; `buffer0` is the most recent copy.
    buffers: Vec<(String, String)>,
    next_buffer: usize,
    /// Pane marked with `select-pane -m`, the default source for `join-pane`.
    marked: Option<PaneId>,
    /// When the saved files last had their pane output refreshed.
    last_history_save: Option<Instant>,
    /// Extra environment for new panes (`set-environment`).
    env: Vec<(String, String)>,
    /// `wait-for` channels, by name.
    waits: HashMap<String, WaitChannel>,
    events: mpsc::UnboundedSender<Event>,
    /// JSON of every session as last autosaved, to detect structural change.
    last_saved: HashMap<String, String>,
}

pub async fn run(socket: String) -> Result<()> {
    run_with(socket, RunOptions::default()).await
}

/// How a server is started, beyond its socket name.
#[derive(Default, Clone, Debug)]
pub struct RunOptions {
    /// Bring every saved session back at start whatever the config says
    /// (`wmux __server --restore`, which is what the logon entry runs).
    pub force_restore: bool,
    /// Read this config file instead of looking for one. Tests use it so
    /// that servers sharing a process never share a config through the
    /// environment.
    pub config: Option<PathBuf>,
}

pub async fn run_with(socket: String, options: RunOptions) -> Result<()> {
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
    srv.force_restore = options.force_restore;
    srv.config_override = options.config;
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
        ("M-n", "next-window -a"),
        ("M-p", "previous-window -a"),
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
        ("Space", "select-layout -n"),
        ("E", "select-layout -E"),
        ("q", "display-panes"),
        ("r", "refresh-client"),
        ("~", "show-messages"),
        ("#", "list-buffers"),
        ("f", "command-prompt -p (find-window) \"find-window -- %%\""),
        ("m", "select-pane -m"),
        ("M", "select-pane -M"),
        ("T", "command-prompt -p (pane-title) -I \"#T\" \"select-pane -T -- %%\""),
        ("'", "command-prompt -p (index) \"select-window -t :%%\""),
        (".", "command-prompt -p (move-window-to) \"move-window -t %%\""),
        ("-", "delete-buffer"),
        ("=", "choose-buffer"),
        ("t", "clock-mode"),
        ("C-o", "rotate-window"),
        ("M-o", "rotate-window -D"),
        ("M-1", "select-layout even-horizontal"),
        ("M-2", "select-layout even-vertical"),
        ("M-3", "select-layout main-horizontal"),
        ("M-4", "select-layout main-vertical"),
        ("M-5", "select-layout tiled"),
        ("(", "switch-client -p"),
        (")", "switch-client -n"),
        ("w", "choose-tree -Zw"),
        ("s", "choose-tree -Zs"),
        ("D", "choose-client"),
        ("B", "choose-jobs"),
        (
            ">",
            "display-menu -T \"pane #P\" \"Split horizontally\" h \"split-window -h\" \
             \"Split vertically\" v \"split-window -v\" \"\" \
             \"Swap up\" u \"swap-pane -U\" \"Swap down\" d \"swap-pane -D\" \
             \"Break out\" b break-pane \"\" \
             Zoom z \"resize-pane -Z\" Respawn r \"respawn-pane -k\" \
             Kill x kill-pane",
        ),
        (
            "<",
            "display-menu -T \"window #W\" \"New window\" c new-window Rename r \
             \"command-prompt -p (rename-window) -I \\\"#W\\\" \\\"rename-window -- %%\\\"\" \
             \"\" Previous p previous-window Next n next-window \"\" \
             Kill x kill-window",
        ),
        ("i", "display-message \"#S:#W.#P #T\""),
        ("C-s", "save-session"),
        ("C-r", "restore-session"),
        ("S", "set-option -w synchronize-panes"),
    ];
    for (k, l) in lines {
        let key = Key::parse(k).expect(k);
        let cmd = crate::command::parse_line(l).expect(l).expect(l);
        // Two entries for one key would mean the later one silently won.
        assert!(
            m.insert(key, Binding { cmd, repeat: REPEATABLE.contains(&k) }).is_none(),
            "default key {k} bound twice"
        );
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
            force_restore: false,
            config_override: None,
            sourcing: Vec::new(),
            config_errors: None,
            hooks: HashMap::new(),
            in_hook: false,
            shell_cache: crate::format::ShellCache::default(),
            shell_running: HashSet::new(),
            shell_last_refresh: None,
            plugins: Vec::new(),
            messages: std::collections::VecDeque::new(),
            buffers: Vec::new(),
            next_buffer: 0,
            marked: None,
            last_history_save: None,
            env: Vec::new(),
            waits: HashMap::new(),
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

    /// Describe one live session. `history` holds what each pane had on
    /// screen, collected beforehand because reading it needs `&mut`.
    fn snapshot(&self, s: &Session, history: &HashMap<PaneId, Vec<String>>) -> crate::resurrect::SavedSession {
        use crate::resurrect::*;
        SavedSession {
            name: s.name.clone(),
            current: s.cur,
            size: Some((s.cols, s.rows)),
            windows: s
                .windows
                .iter()
                .map(|w| {
                    let lookup = |id: PaneId| {
                        w.pane(id).map(|p| SavedPane {
                            argv: p.argv.clone(),
                            cwd: p.cwd.clone(),
                            history: history.get(&id).cloned().unwrap_or_default(),
                        })
                    };
                    let order = w.layout.panes();
                    SavedWindow {
                        name: w.name.clone(),
                        layout: SavedNode::from_layout(&w.layout, &lookup).unwrap_or(SavedNode::Pane {
                            pane: SavedPane { argv: Vec::new(), cwd: None, history: Vec::new() },
                        }),
                        active: order.iter().position(|p| *p == w.active).unwrap_or(0),
                        zoomed: w.zoomed,
                    }
                })
                .collect(),
        }
    }

    /// The last `save-history` lines of every pane in a session (all of them
    /// for `all`), colours included.
    fn pane_histories(&mut self, sid: SessionId) -> HashMap<PaneId, Vec<String>> {
        let want = self.opts.save_history;
        let mut out = HashMap::new();
        if want == 0 {
            return out;
        }
        let ids: Vec<PaneId> = match self.session(sid) {
            Some(s) => s.windows.iter().flat_map(|w| w.panes.iter().map(|p| p.id)).collect(),
            None => return out,
        };
        for id in ids {
            let Some(p) = self.find_pane_mut(id) else { continue };
            let total = p.scrollback_len() + p.rows as usize;
            let lines = p.lines_from(total.saturating_sub(want), true);
            if !lines.is_empty() {
                out.insert(id, lines);
            }
        }
        out
    }

    /// Save one session to its file; returns the path.
    fn save_session_file(&mut self, sid: SessionId) -> Result<PathBuf, String> {
        let history = self.pane_histories(sid);
        let s = self.session(sid).ok_or("no such session")?;
        let saved = self.snapshot(s, &history);
        // The change key covers the structure only: pane output changes every
        // second and must not make the file be rewritten every second.
        let key = serde_json::to_string(&self.snapshot(s, &HashMap::new())).unwrap_or_default();
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
        // Pane output is saved too, but refreshing it means writing files, so
        // that happens on a slow beat rather than whenever something printed.
        let refresh_history =
            self.opts.save_history > 0 && self.last_history_save.is_none_or(|t| t.elapsed() > Duration::from_secs(30));
        if refresh_history {
            self.last_history_save = Some(Instant::now());
        }
        for sid in ids {
            let Some(s) = self.session(sid) else { continue };
            let saved = self.snapshot(s, &HashMap::new());
            let key = serde_json::to_string(&saved).unwrap_or_default();
            let path = crate::resurrect::file_for(&dir, &saved.name).to_string_lossy().into_owned();
            if !refresh_history && self.last_saved.get(&path) == Some(&key) {
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
    /// `size` is the terminal about to attach; without one the session
    /// comes back at the size it was saved at (80x24 for older files).
    fn restore_session_file(
        &mut self,
        path: &std::path::Path,
        size: Option<(u16, u16)>,
    ) -> Result<(SessionId, Vec<String>), String> {
        use crate::resurrect::*;
        let ss = SavedFile::load(path)?.session;
        if self.sessions.iter().any(|s| s.name == ss.name) {
            return Err(format!("session {} already exists", ss.name));
        }
        // A file is data: a hand-edited or damaged size must not make a
        // session too small to lay out (`new -x/-y` has the same floor).
        let (cols, rows) = size.or(ss.size).map(|(c, r)| (c.max(10), r.max(3))).unwrap_or((80, 24));
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
    fn restore_all(&mut self, size: Option<(u16, u16)>) -> (Vec<SessionId>, Vec<String>) {
        let mut created = Vec::new();
        let mut problems = Vec::new();
        for (name, _, path) in crate::resurrect::list(&self.sessions_dir()) {
            if self.sessions.iter().any(|s| s.name == name) {
                continue;
            }
            match self.restore_session_file(&path, size) {
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
        let border = self.border_rows();
        let mut panes = Vec::new();
        for saved in sw.layout.panes() {
            let argv: Vec<String> = saved.argv.clone();
            let cwd = saved.cwd.as_deref().filter(|d| std::path::Path::new(d).is_dir());
            // Sizes are refitted by relayout; spawn at the window size.
            let mut pane = self.spawn_pane(&argv, cwd, area.w, area.h)?;
            // Put the saved output back on the screen before the new shell
            // starts printing, so a resumed pane looks like it did.
            if !saved.history.is_empty() {
                let mut text = saved.history.join("\r\n");
                text.push_str("\r\n");
                pane.process_output(text.as_bytes());
            }
            panes.push(pane);
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
        w.relayout(area, border);
        Ok(())
    }

    fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn load_config(&mut self) {
        if let Some(path) = self.config_override.clone().or_else(crate::config::find_config) {
            match self.source_file(&path.to_string_lossy()) {
                Ok(n) => log::info!("loaded {} ({n} commands)", path.display()),
                Err(e) => {
                    log::error!("config {}: {e}", path.display());
                    // Shown to the first client that attaches, like tmux does.
                    self.config_errors = Some(e.lines().map(str::to_string).collect());
                }
            }
        }
        if (self.opts.restore_on_start || self.force_restore) && self.sessions.is_empty() {
            let (created, problems) = self.restore_all(None);
            log::info!("restore-on-start: {} session(s)", created.len());
            for p in problems {
                log::warn!("restore-on-start: {p}");
            }
        }
    }

    /// Source a file of wmux commands, then load any `@plugin` it declared.
    ///
    /// A file named `*tmux.conf` is one written for tmux: whatever wmux
    /// cannot use in it (other key tables, TPM, `%if` blocks) is skipped
    /// with a note in `show-messages`, and the result is a one-line summary
    /// rather than a wall of errors on every attach.
    fn source_file(&mut self, path: &str) -> Result<usize, String> {
        let path = &expand_home(path);
        let lenient = crate::config::is_tmux_conf(std::path::Path::new(path));
        // A file that sources itself (directly or through another) would
        // recurse until the stack ran out and take the server with it.
        let canon = std::fs::canonicalize(path).map(|p| p.to_string_lossy().into_owned()).unwrap_or(path.clone());
        if self.sourcing.contains(&canon) {
            return Err(format!("{path}: is already being sourced (a source-file loop)"));
        }
        self.sourcing.push(canon);
        let r = self.source_file_inner(path, lenient);
        self.sourcing.pop();
        r
    }

    fn source_file_inner(&mut self, path: &String, lenient: bool) -> Result<usize, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        let mut n = 0;
        let mut errors = Vec::new();
        let (lines, unterminated) = logical_lines(&text);
        if let Some(line_no) = unterminated {
            errors.push(format!("{path}:{line_no}: %if without %endif; the rest of the file was skipped"));
        }
        for (line_no, line) in lines {
            match crate::command::parse_line(&line) {
                Ok(None) => {}
                Ok(Some(cmd)) => {
                    n += 1;
                    if let Outcome::Error(e) = self.exec(cmd, None) {
                        errors.push(format!("{path}:{line_no}: {e}"));
                    }
                }
                Err(e) => errors.push(format!("{path}:{line_no}: {e}")),
            }
        }
        let pending = std::mem::take(&mut self.opts.pending_plugins);
        for p in pending {
            if let Err(e) = self.load_plugin(&p) {
                errors.push(format!("{path}: @plugin {p}: {e}"));
            }
        }
        if errors.is_empty() {
            return Ok(n);
        }
        if lenient {
            for e in &errors {
                log::warn!("{e}");
                self.note_message(&format!("skipped: {e}"));
            }
            let short = std::path::Path::new(path).file_name().map(|f| f.to_string_lossy().into_owned());
            return Err(format!(
                "{}: {} lines wmux could not use were skipped (prefix ~ or show-messages lists them)",
                short.unwrap_or_else(|| path.clone()),
                errors.len()
            ));
        }
        Err(errors.join("\n"))
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
            // A respawned pane keeps its id, so the generation says whether
            // this came from the process that is running now.
            Event::Pane(PaneEvent::Output(id, generation, bytes)) => {
                if let Some(p) = self.find_pane_mut(id)
                    && p.generation == generation
                {
                    // pipe-pane and a recording see what the program wrote,
                    // not the screen.
                    let slow = p.pipe_write(&bytes);
                    p.record_write(&bytes);
                    p.process_output(&bytes);
                    if let Some(command) = slow {
                        // Say it once per pipe: a command that cannot keep up
                        // loses output, and silence about that is worse.
                        log::warn!("pipe-pane %{id}: {command} is not keeping up; output is being dropped");
                        self.note_message(&format!("pipe-pane %{id}: {command} is not keeping up; output dropped"));
                    }
                    self.note_output(id);
                }
            }
            Event::Pane(PaneEvent::Exit(id, generation, code)) => {
                if self.find_pane_mut(id).is_some_and(|p| p.generation != generation) {
                    return; // the previous process of a respawned pane
                }
                let command = self.find_pane_mut(id).map(|p| p.command.clone()).unwrap_or_default();
                log::info!("pane %{id} ({command}) exited with {code}");
                // A popup is not part of any window: it either goes away now
                // (`-E`) or waits for a key, showing what the command left.
                if let Some(cid) = self.popup_of_pane(id) {
                    let c = self.clients.get_mut(&cid).unwrap();
                    let p = c.popup.as_mut().unwrap();
                    if p.close_on_exit {
                        c.popup = None;
                    } else {
                        p.finished = true;
                    }
                    c.last_grid = None;
                    return;
                }
                if self.opts.remain_on_exit {
                    // Keep the pane and what it printed; say why it stopped.
                    // `respawn-pane` starts it again, `kill-pane` closes it.
                    if let Some(p) = self.find_pane_mut(id) {
                        p.exit_code = Some(code);
                        p.died_at = Some(Instant::now());
                        let note =
                            format!("\r\n\x1b[7m[{command} exited with {code}; respawn-pane or kill-pane]\x1b[0m\r\n");
                        p.process_output(note.as_bytes());
                    }
                    self.note_message(&format!("pane %{id} ({command}) exited with {code}"));
                    return;
                }
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
                        last_session: None,
                        last_grid: None,
                        last_cursor: None,
                        prefix: false,
                        repeat_until: None,
                        panes_until: None,
                        prompt: None,
                        message: None,
                        overlay: None,
                        chooser: None,
                        popup: None,
                        mouse_buttons: 0,
                        drag: None,
                        swallow_up: HashSet::new(),
                        pending_bell: false,
                        pending: std::collections::VecDeque::new(),
                        window_hits: Vec::new(),
                    },
                );
            }
            Event::Gone(id) => {
                self.clients.remove(&id);
                // A client that went away while waiting must not hold a
                // `wait-for` lock, or nothing could ever take it again.
                let mut freed: Vec<String> = Vec::new();
                for (name, w) in self.waits.iter_mut() {
                    w.waiters.retain(|c| *c != id);
                    w.lockers.retain(|c| *c != id);
                    if w.idle() {
                        freed.push(name.clone());
                    }
                }
                for name in freed {
                    self.waits.remove(&name);
                }
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
                c.popup = None;
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
        self.note_message(&text.replace('\n', " "));
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

    /// Add a line to the `show-messages` log, keeping the last 100 as tmux
    /// does. Every place that has something to report goes through here, so
    /// the log can never grow without bound.
    fn note_message(&mut self, text: &str) {
        let stamp = chrono::Local::now().format("%H:%M:%S");
        self.messages.push_back(format!("{stamp} {text}"));
        while self.messages.len() > 100 {
            self.messages.pop_front();
        }
    }

    /// A pane printed something: keep the window's silence timer fresh and,
    /// when it is not the window being looked at, raise the alerts the
    /// `monitor-*` options ask for.
    fn note_output(&mut self, pid: PaneId) {
        let Some((sid, widx)) = self.window_of_pane(pid) else { return };
        let (activity_on, bell_on) = (self.opts.monitor_activity, self.opts.monitor_bell);
        let Some(s) = self.sessions.iter_mut().find(|s| s.id == sid) else { return };
        let current = s.cur == widx;
        let Some(w) = s.windows.get_mut(widx) else { return };
        w.last_output = Instant::now();
        if let Some(p) = w.panes.iter_mut().find(|p| p.id == pid) {
            p.last_output = w.last_output;
        }
        w.alert_silence = false;
        if current {
            return; // the bell of the window in view is rung by the renderer
        }
        // Nothing consumes a background pane's bell otherwise, so take it here.
        let rang = w.panes.iter().any(|p| p.bell);
        if rang {
            for p in &mut w.panes {
                p.bell = false;
            }
        }
        let (name, index) = (w.name.clone(), widx + self.opts.base_index);
        let mut fresh = Vec::new();
        if activity_on && !w.alert_activity {
            w.alert_activity = true;
            fresh.push(("Activity", false));
        }
        if bell_on && rang && !w.alert_bell {
            w.alert_bell = true;
            fresh.push(("Bell", true));
        }
        for (what, is_bell) in fresh {
            self.alert(sid, &format!("{what} in window {index} ({name})"), is_bell);
        }
    }

    /// Tell the clients of a session about an alert: a status-line message
    /// when `visual-bell`/`visual-activity` is on, the terminal bell otherwise.
    fn alert(&mut self, sid: SessionId, text: &str, is_bell: bool) {
        let visual = if is_bell { self.opts.visual_bell } else { self.opts.visual_activity };
        // A window flag only reaches someone who is looking at the terminal;
        // a notification reaches them when it is behind other windows.
        if self.opts.notify {
            let name = self.session(sid).map(|s| s.name.clone()).unwrap_or_else(|| "wmux".into());
            crate::notify::notify(&format!("wmux: {name}"), text);
        }
        let ids: Vec<ClientId> = self.clients.values().filter(|c| c.session == Some(sid)).map(|c| c.id).collect();
        for cid in ids {
            if visual {
                self.show(cid, text);
            } else if is_bell && let Some(c) = self.clients.get_mut(&cid) {
                c.pending_bell = true;
            }
        }
    }

    /// Run from the render tick: the current window of a session never holds
    /// an alert (that is the one invariant of the flags, enforced here so no
    /// path that changes the current window has to remember it), and
    /// `monitor-silence` flags a background window that has gone quiet.
    fn sweep_alerts(&mut self) {
        let quiet = Duration::from_secs(self.opts.monitor_silence.max(1));
        let watch_silence = self.opts.monitor_silence > 0;
        let base = self.opts.base_index;
        let mut alerts: Vec<(SessionId, String)> = Vec::new();
        for s in &mut self.sessions {
            let (cur, sid) = (s.cur, s.id);
            for (i, w) in s.windows.iter_mut().enumerate() {
                if i == cur {
                    w.alert_activity = false;
                    w.alert_bell = false;
                    w.alert_silence = false;
                    continue;
                }
                if watch_silence && !w.alert_silence && w.last_output.elapsed() >= quiet {
                    w.alert_silence = true;
                    alerts.push((sid, format!("Silence in window {} ({})", i + base, w.name)));
                }
            }
        }
        for (sid, text) in alerts {
            self.alert(sid, &text, false);
        }
    }

    fn detach(&mut self, cid: ClientId, reason: &str) {
        if let Some(c) = self.clients.get_mut(&cid)
            && c.session.take().is_some()
        {
            c.last_grid = None;
            c.popup = None; // its program goes with the client that opened it
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
        if self.sessions.iter().flat_map(|s| s.windows.iter()).any(|w| w.pane(id).is_some()) {
            return self.sessions.iter_mut().flat_map(|s| s.windows.iter_mut()).find_map(|w| w.pane_mut(id));
        }
        // A popup's pane belongs to no window, but its output still arrives.
        self.clients.values_mut().find_map(|c| c.popup.as_mut().filter(|p| p.pane.id == id).map(|p| &mut p.pane))
    }

    /// The client whose popup owns this pane, if any.
    fn popup_of_pane(&self, id: PaneId) -> Option<ClientId> {
        self.clients.values().find(|c| c.popup.as_ref().is_some_and(|p| p.pane.id == id)).map(|c| c.id)
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

    /// Whether `pane-border-status` reserves a row above (`Some(true)`) or
    /// below (`Some(false)`) each pane.
    fn border_rows(&self) -> Option<bool> {
        match self.opts.pane_border_status.as_str() {
            "top" => Some(true),
            "bottom" => Some(false),
            _ => None,
        }
    }

    fn resize_session(&mut self, sid: SessionId, cols: u16, rows: u16) {
        let area = self.window_area(cols.max(1), rows.max(1));
        let border = self.border_rows();
        if let Some(s) = self.session_mut(sid) {
            s.cols = cols.max(1);
            s.rows = rows.max(1);
            for w in &mut s.windows {
                w.relayout(area, border);
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
    /// A buffer by name, or the newest one.
    fn buffer(&self, name: Option<&str>) -> Option<&(String, String)> {
        match name {
            Some(n) => self.buffers.iter().find(|(b, _)| b == n),
            None => self.buffers.first(),
        }
    }

    /// Store text as a buffer: named (replacing or appending to it) or a new
    /// automatic `bufferN` at the front, the way tmux stacks them.
    fn set_buffer(&mut self, name: Option<&str>, data: &str, append: bool) {
        if let Some(n) = name {
            if let Some(slot) = self.buffers.iter_mut().find(|(b, _)| b == n) {
                if append {
                    slot.1.push_str(data);
                } else {
                    slot.1 = data.to_string();
                }
                return;
            }
            self.buffers.insert(0, (n.to_string(), data.to_string()));
        } else {
            if append && let Some(top) = self.buffers.first_mut() {
                top.1.push_str(data);
                return;
            }
            let name = format!("buffer{}", self.next_buffer);
            self.next_buffer += 1;
            self.buffers.insert(0, (name, data.to_string()));
        }
        // 50 is what tmux keeps by default (buffer-limit).
        self.buffers.truncate(50);
    }

    fn pane_env(&self, pane_id: PaneId) -> Vec<(String, String)> {
        let mut env = vec![
            ("WMUX".into(), self.socket.clone()),
            ("WMUX_PANE".into(), pane_id.to_string()),
            ("PATH".into(), path_with_self()),
        ];
        // `set-environment` entries last, so they can override even PATH.
        env.extend(self.env.iter().cloned());
        env
    }

    fn spawn_pane(&mut self, argv: &[String], cwd: Option<&str>, cols: u16, rows: u16) -> Result<Pane, String> {
        let id = self.alloc_id();
        let argv = if argv.is_empty() { resolve_shell(&self.opts) } else { argv.to_vec() };
        let env = self.pane_env(id);
        Pane::spawn(id, &argv, cwd, cols, rows, self.opts.history_limit, &env, self.pane_tx.clone())
            .map_err(|e| format!("{e:#}"))
    }

    /// (session, window index) holding a pane.
    fn window_of_pane(&self, pid: PaneId) -> Option<(SessionId, usize)> {
        self.sessions.iter().find_map(|s| s.windows.iter().position(|w| w.pane(pid).is_some()).map(|widx| (s.id, widx)))
    }

    /// Take a live pane out of its window without killing it, collapsing the
    /// window if it was the last one (`join-pane`).
    fn take_pane(&mut self, sid: SessionId, widx: usize, pid: PaneId) -> Option<Pane> {
        let s = self.session_mut(sid)?;
        let w = s.windows.get_mut(widx)?;
        let pos = w.panes.iter().position(|p| p.id == pid)?;
        let pane = w.panes.remove(pos);
        w.layout.remove(pid);
        w.layout_preset = None;
        w.zoomed = false;
        if w.active == pid {
            w.active = w.layout.panes().first().copied().unwrap_or(0);
        }
        if w.last_pane == Some(pid) {
            w.last_pane = None;
        }
        if w.panes.is_empty() {
            let wid = w.id;
            s.windows.remove(widx);
            if s.last == Some(wid) {
                s.last = None;
            }
            if s.windows.is_empty() {
                // The session has nothing left; it goes the way it does when
                // its last pane exits.
                self.kill_session(sid, "exited");
            } else if let Some(s) = self.session_mut(sid) {
                s.cur = s.cur.min(s.windows.len() - 1);
            }
        }
        Some(pane)
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
            layout_preset: None,
            synchronized: false,
            rects: Vec::new(),
            layout_rects: Vec::new(),
            alert_activity: false,
            alert_bell: false,
            alert_silence: false,
            last_output: Instant::now(),
        };
        w.relayout(area, self.border_rows());
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
            created_at: chrono::Local::now().timestamp(),
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
            Cmd::NewSession { name, window_name, cwd, detached, argv, attach_existing, size } => {
                let client = cid.and_then(|c| self.clients.get(&c));
                let interactive = client.is_some_and(|c| c.interactive);
                let (mut cols, mut rows) = client.map(|c| (c.cols, c.rows)).unwrap_or((80, 24));
                // A session nobody is attaching to has no terminal to size
                // it; -x/-y say instead (tmux ignores them otherwise too).
                if detached || !interactive {
                    cols = size.0.unwrap_or(cols);
                    rows = size.1.unwrap_or(rows);
                }
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
                            last: false,
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
            Cmd::DetachClient { all, target } => {
                // No -a: just this client. With -a (or -s session): every
                // client, or every client of that session.
                let only: Option<SessionId> = match &target {
                    Some(t) if t.session.is_some() => match self.resolve_session(Some(t), cid) {
                        Ok(s) => Some(s),
                        Err(e) => return Outcome::Error(e),
                    },
                    _ => None,
                };
                let victims: Vec<ClientId> = if all || only.is_some() {
                    self.clients
                        .values()
                        .filter(|c| c.session.is_some() && only.is_none_or(|s| c.session == Some(s)))
                        .map(|c| c.id)
                        .collect()
                } else {
                    cid.into_iter().collect()
                };
                for v in victims {
                    self.detach(v, "detached");
                }
                Outcome::Ok
            }
            Cmd::ShowMessages => {
                if self.messages.is_empty() {
                    return Outcome::Text("no messages".into());
                }
                Outcome::Text(self.messages.iter().cloned().collect::<Vec<String>>().join("\n"))
            }
            Cmd::SetEnvironment { name, value, remove } => {
                self.env.retain(|(k, _)| *k != name);
                match (remove, value) {
                    (true, _) | (_, None) => {}
                    (false, Some(v)) => self.env.push((name, v)),
                }
                Outcome::Ok
            }
            Cmd::ShowEnvironment { name } => {
                let lines: Vec<String> = match &name {
                    Some(n) => match self.env.iter().find(|(k, _)| k == n) {
                        Some((k, v)) => vec![format!("{k}={v}")],
                        None => return Outcome::Error(format!("unknown variable: {n}")),
                    },
                    None => self.env.iter().map(|(k, v)| format!("{k}={v}")).collect(),
                };
                if lines.is_empty() {
                    return Outcome::Text("no variables set".into());
                }
                Outcome::Text(lines.join("\n"))
            }
            Cmd::RespawnPane { target, kill, argv, window } => {
                let (sid, widx, pid) = match self.resolve(target.as_ref(), cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                let ids: Vec<PaneId> =
                    if window { self.session(sid).unwrap().windows[widx].layout.panes() } else { vec![pid] };
                let (history, tx) = (self.opts.history_limit, self.pane_tx.clone());
                let mut done = 0;
                for id in ids {
                    // The same environment a new pane gets: WMUX, WMUX_PANE
                    // and PATH, not just the set-environment entries.
                    let env = self.pane_env(id);
                    let Some(p) = self.find_pane_mut(id) else { continue };
                    if p.exit_code.is_none() && !kill {
                        continue; // tmux refuses a live pane without -k
                    }
                    let cmd = if argv.is_empty() { None } else { Some(argv.as_slice()) };
                    match p.respawn(cmd, history, &env, tx.clone()) {
                        Ok(()) => done += 1,
                        Err(e) => return Outcome::Error(format!("respawn: {e:#}")),
                    }
                }
                if done == 0 {
                    return Outcome::Error("pane is still running (use -k)".into());
                }
                self.relayout_session(sid);
                self.autosave_changed();
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
                            w.flags(i == s.cur, Some(w.id) == s.last),
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
                // Only a terminal about to attach has a size worth using;
                // a script resuming from the side leaves the saved size.
                let size = if interactive && attach { client.map(|c| (c.cols, c.rows)) } else { None };
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
                            match self.restore_session_file(&path, size) {
                                Ok((sid, p)) => (vec![sid], p, Some(sid)),
                                Err(e) => return Outcome::Error(e),
                            }
                        }
                    }
                    None => {
                        let (c, p) = self.restore_all(size);
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
            // Whoever asked has already started it.
            Cmd::StartServer => Outcome::Ok,
            Cmd::Record { target, path } => {
                let pid = match self.resolve(target.as_ref(), cid) {
                    Ok((_, _, p)) => p,
                    Err(e) => return Outcome::Error(e),
                };
                let Some(path) = path else {
                    // No path: stop, and say which file was closed.
                    let Some(p) = self.find_pane_mut(pid) else { return Outcome::Error("no such pane".into()) };
                    return match p.recorder.take() {
                        Some(r) => Outcome::Text(format!("recording stopped: {}", r.path)),
                        None => Outcome::Error("not recording".into()),
                    };
                };
                // A relative path is relative to where the client was run,
                // which is where the file will be looked for.
                let mut full = expand_home(&path);
                if std::path::Path::new(&full).is_relative()
                    && let Some(cwd) = cid.and_then(|c| self.clients.get(&c)).map(|c| c.cwd.clone())
                    && !cwd.is_empty()
                {
                    full = std::path::Path::new(&cwd).join(&full).to_string_lossy().into_owned();
                }
                let Some(p) = self.find_pane_mut(pid) else { return Outcome::Error("no such pane".into()) };
                match p.record_to(&full) {
                    Ok(()) => Outcome::Text(format!("recording %{pid} to {full}")),
                    Err(e) => Outcome::Error(format!("{e:#}")),
                }
            }
            Cmd::FindText { pattern, target, case_sensitive, per_pane } => {
                // Which panes to look in: everything, or one session (or one
                // window of it) when -t says so.
                // No -t at all searches every session. A -t names one
                // session (`:1` meaning this client's, as everywhere else),
                // optionally one window of it, optionally one pane of that,
                // and says so when the target does not exist rather than
                // reporting an empty search.
                let (want_session, want_window, want_pane) = match &target {
                    Some(t) => {
                        let sid = match self.resolve_session(Some(t), cid) {
                            Ok(s) => s,
                            Err(e) => return Outcome::Error(e),
                        };
                        let widx = match t.window {
                            Some(_) => match self.resolve_window(sid, Some(t)) {
                                Ok(w) => Some(w),
                                Err(e) => return Outcome::Error(e),
                            },
                            None => None,
                        };
                        let pid = match t.pane {
                            Some(_) => {
                                let w = widx.or_else(|| self.session(sid).map(|s| s.cur)).unwrap_or(0);
                                match self.resolve_pane(sid, w, Some(t)) {
                                    Ok(p) => Some(p),
                                    Err(e) => return Outcome::Error(e),
                                }
                            }
                            None => None,
                        };
                        (Some(sid), widx, pid)
                    }
                    None => (None, None, None),
                };
                let base = self.opts.base_index;
                let pane_base = self.opts.pane_base_index;
                // (label, pane id, rows) first, so the panes can be searched
                // one at a time without holding a borrow of the tree.
                let mut targets: Vec<(String, PaneId, usize)> = Vec::new();
                for s in &self.sessions {
                    if want_session.is_some_and(|w| w != s.id) {
                        continue;
                    }
                    for (widx, w) in s.windows.iter().enumerate() {
                        if want_window.is_some_and(|i| i != widx) {
                            continue;
                        }
                        for (pidx, pid) in w.layout.panes().iter().enumerate() {
                            if want_pane.is_some_and(|p| p != *pid) {
                                continue;
                            }
                            let rows = w.pane(*pid).map(|p| p.rows as usize).unwrap_or(0);
                            targets.push((format!("{}:{}.{}", s.name, widx + base, pidx + pane_base), *pid, rows));
                        }
                    }
                }
                let mut hits: Vec<(String, usize, String)> = Vec::new();
                for (label, pid, rows) in targets {
                    let Some(p) = self.find_pane_mut(pid) else { continue };
                    let total = p.scrollback_len() + rows;
                    for (abs, text) in p.search(&pattern, per_pane, case_sensitive) {
                        // How far back from the newest line, which is what
                        // says whether this is fresh or ancient.
                        hits.push((label.clone(), total.saturating_sub(abs + 1), one_line(&text, 120)));
                    }
                }
                if hits.is_empty() {
                    return Outcome::Error(format!("no pane has: {}", one_line(&pattern, 60)));
                }
                // One column each for the pane and how far back it was, so a
                // list of hits reads down the page.
                let wide = hits.iter().map(|(l, _, _)| l.chars().count()).max().unwrap_or(0);
                let back_w = hits.iter().map(|(_, b, _)| b.to_string().len()).max().unwrap_or(1);
                let lines: Vec<String> = hits
                    .iter()
                    .map(|(label, back, text)| format!("{label:<wide$}  -{back:>back_w$}  {text}"))
                    .collect();
                Outcome::Text(lines.join("\n"))
            }
            Cmd::Notify { title, message } => {
                let title = match (title, cid) {
                    (Some(t), Some(cid)) => self.expand_format(&t, cid),
                    (Some(t), None) => t,
                    (None, _) => "wmux".to_string(),
                };
                let message = match cid {
                    Some(cid) => self.expand_format(&message, cid),
                    None => message,
                };
                if crate::notify::notify(&title, &message) {
                    Outcome::Ok
                } else {
                    Outcome::Error("no desktop to notify (a service, or session 0)".into())
                }
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
            Cmd::SwapWindow { src, dst, move_it } => {
                let (ssid, si) = match self.resolve(src.as_ref(), cid) {
                    Ok((s, w, _)) => (s, w),
                    Err(e) => return Outcome::Error(e),
                };
                let (dsid, di) = match self.resolve(dst.as_ref(), cid) {
                    Ok((s, w, _)) => (s, w),
                    // `move-window -t :5` may name an index that is still
                    // free, which tmux takes as "put it there".
                    Err(e) => {
                        let free = dst
                            .as_ref()
                            .filter(|_| move_it)
                            .and_then(|t| t.window.as_deref())
                            .and_then(|w| w.parse::<usize>().ok());
                        match (free, self.resolve_session(dst.as_ref(), cid)) {
                            (Some(i), Ok(sid)) => (sid, i.saturating_sub(self.opts.base_index)),
                            _ => return Outcome::Error(e),
                        }
                    }
                };
                if ssid != dsid {
                    if !move_it {
                        return Outcome::Error("swap-window works inside one session".into());
                    }
                    // Move a window to another session: take it out, put it in
                    // at the destination index, and leave no empty session.
                    let Some(spos) = self.sessions.iter().position(|s| s.id == ssid) else {
                        return Outcome::Error("no such session".into());
                    };
                    let Some(dpos) = self.sessions.iter().position(|s| s.id == dsid) else {
                        return Outcome::Error("no such session".into());
                    };
                    // Both sessions keep looking at the window they were
                    // looking at, whatever the move does to the indexes.
                    let src_cur = self.sessions[spos].windows.get(self.sessions[spos].cur).map(|w| w.id);
                    let dst_cur = self.sessions[dpos].windows.get(self.sessions[dpos].cur).map(|w| w.id);
                    let w = self.sessions[spos].windows.remove(si);
                    let wid = w.id;
                    {
                        let s = &mut self.sessions[spos];
                        if s.last == Some(wid) {
                            s.last = None;
                        }
                        s.cur = src_cur
                            .filter(|id| *id != wid)
                            .and_then(|id| s.windows.iter().position(|w| w.id == id))
                            .unwrap_or_else(|| s.cur.min(s.windows.len().saturating_sub(1)));
                    }
                    let at = di.min(self.sessions[dpos].windows.len());
                    self.sessions[dpos].windows.insert(at, w);
                    {
                        let s = &mut self.sessions[dpos];
                        if let Some(i) = dst_cur.and_then(|id| s.windows.iter().position(|w| w.id == id)) {
                            s.cur = i;
                        }
                    }
                    if self.sessions[spos].windows.is_empty() {
                        self.kill_session(ssid, "moved away");
                    } else {
                        self.relayout_session(ssid);
                    }
                    self.relayout_session(dsid);
                    self.autosave_changed();
                    return Outcome::Ok;
                }
                let cur_id = self.session(ssid).and_then(|s| s.windows.get(s.cur)).map(|w| w.id);
                let s = self.session_mut(ssid).unwrap();
                if si == di {
                    return Outcome::Ok;
                }
                if move_it {
                    let w = s.windows.remove(si);
                    s.windows.insert(di.min(s.windows.len()), w);
                } else {
                    s.windows.swap(si, di);
                }
                // Follow the window that was current, wherever it landed.
                if let Some(id) = cur_id
                    && let Some(i) = s.windows.iter().position(|w| w.id == id)
                {
                    s.cur = i;
                }
                self.autosave_changed();
                Outcome::Ok
            }
            Cmd::NextWindow { ref target, alert: true } | Cmd::PreviousWindow { ref target, alert: true } => {
                let forward = matches!(cmd, Cmd::NextWindow { .. });
                let sid = match self.resolve_session(target.as_ref(), cid) {
                    Ok(s) => s,
                    Err(e) => return Outcome::Error(e),
                };
                let Some(s) = self.session(sid) else { return Outcome::Error("no such session".into()) };
                let n = s.windows.len();
                // Walk the ring from where we are and stop at the first window
                // waiting to say something.
                let hit = (1..=n)
                    .map(|d| if forward { (s.cur + d) % n } else { (s.cur + n - d % n) % n })
                    .find(|i| s.windows[*i].alert_activity || s.windows[*i].alert_bell || s.windows[*i].alert_silence);
                match hit {
                    Some(i) => {
                        self.sessions.iter_mut().find(|s| s.id == sid).unwrap().select_window(i);
                        self.autosave_changed();
                        self.fire_hook("after-select-window", cid);
                        Outcome::Ok
                    }
                    None => Outcome::Error("no window with an alert".into()),
                }
            }
            Cmd::NextWindow { ref target, .. }
            | Cmd::PreviousWindow { ref target, .. }
            | Cmd::LastWindow { ref target } => {
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
                let border = self.border_rows();
                // Unzoom first so the layout rectangles are real.
                let w = &mut self.session_mut(sid).unwrap().windows[widx];
                if w.zoomed {
                    w.zoomed = false;
                    w.relayout(area, border);
                }
                let rect = if full {
                    area
                } else {
                    match w.layout_rect(pid) {
                        Some(r) => r,
                        None => return Outcome::Error("pane has no layout".into()),
                    }
                };
                // Each half needs a row of content, plus its border-status row.
                let min_rows = if border.is_some() { 5 } else { 3 };
                if (horizontal && rect.w < 3) || (!horizontal && rect.h < min_rows) {
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
            Cmd::ResizePane { dir, amount, zoom, target, width, height } => {
                let (sid, widx, pid) = match self.resolve(target.as_ref(), cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                let (scols, srows) = self.session(sid).map(|s| (s.cols, s.rows)).unwrap();
                let area = self.window_area(scols, srows);
                let w = &mut self.session_mut(sid).unwrap().windows[widx];
                if zoom {
                    w.zoomed = !w.zoomed && w.panes.len() > 1;
                } else if let Some(d) = dir {
                    w.zoomed = false;
                    w.layout.resize(pid, d, amount.max(1));
                }
                // -x / -y: grow or shrink until the pane has that size.
                let border = self.border_rows();
                for (spec, horizontal) in [(width, true), (height, false)] {
                    let Some(spec) = spec else { continue };
                    let full = if horizontal { area.w } else { area.h };
                    let want = match parse_size(&spec, full) {
                        Some(v) => v,
                        None => return Outcome::Error(format!("resize-pane: bad size '{spec}'")),
                    };
                    let w = &mut self.session_mut(sid).unwrap().windows[widx];
                    w.zoomed = false;
                    for _ in 0..full {
                        w.relayout(area, border);
                        let Some(rect) = w.rect_of(pid) else { break };
                        let now = if horizontal { rect.w } else { rect.h };
                        if now == want {
                            break;
                        }
                        let dir = match (horizontal, now < want) {
                            (true, true) => Dir::Right,
                            (true, false) => Dir::Left,
                            (false, true) => Dir::Down,
                            (false, false) => Dir::Up,
                        };
                        let before = now;
                        w.layout.resize(pid, dir, 1);
                        w.relayout(area, border);
                        // Wedged against a minimum: stop rather than spin.
                        let after = w.rect_of(pid).map(|r| if horizontal { r.w } else { r.h }).unwrap_or(before);
                        if after == before {
                            break;
                        }
                    }
                }
                self.relayout_session(sid);
                Outcome::Ok
            }
            Cmd::PaneTitle { target, title, mark, unmark } => {
                let (_, _, pid) = match self.resolve(target.as_ref(), cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                if unmark {
                    self.marked = None;
                }
                if mark {
                    self.marked = if self.marked == Some(pid) { None } else { Some(pid) };
                }
                if let Some(t) = title
                    && let Some(p) = self.find_pane_mut(pid)
                {
                    p.title = t;
                }
                Outcome::Ok
            }
            Cmd::SelectLayout { name, next, prev: _, spread, target } => {
                let (sid, widx, pid) = match self.resolve(target.as_ref(), cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                // `-E` evens out the panes next to this one and leaves the
                // rest of the layout as it is.
                if spread && name.is_none() {
                    let w = &mut self.session_mut(sid).unwrap().windows[widx];
                    if !w.layout.spread(pid) {
                        return Outcome::Error("pane has no panes beside it".into());
                    }
                    w.zoomed = false;
                    self.relayout_session(sid);
                    self.autosave_changed();
                    return Outcome::Ok;
                }
                let w = &self.session(sid).unwrap().windows[widx];
                let current = w.layout_preset;
                let preset = match name {
                    Some(n) => match layout::Preset::parse(&n) {
                        Some(p) => p,
                        None => {
                            let all: Vec<&str> = layout::PRESETS.iter().map(|(n, _)| *n).collect();
                            return Outcome::Error(format!("unknown layout: {n} (one of {})", all.join(", ")));
                        }
                    },
                    // Cycling from nothing starts at the first layout.
                    None if next => current.map(|p| p.next()).unwrap_or(layout::PRESETS[0].1),
                    None => current.map(|p| p.prev()).unwrap_or(layout::PRESETS[0].1),
                };
                let panes = w.layout.panes();
                let Some(tree) = preset.build(&panes) else { return Outcome::Error("window has no panes".into()) };
                let w = &mut self.session_mut(sid).unwrap().windows[widx];
                w.layout = tree;
                w.layout_preset = Some(preset);
                w.zoomed = false;
                self.relayout_session(sid);
                self.message(cid.unwrap_or(0), preset.name());
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
                    layout_preset: None,
                    synchronized: false,
                    rects: Vec::new(),
                    layout_rects: Vec::new(),
                    alert_activity: false,
                    alert_bell: false,
                    alert_silence: false,
                    last_output: Instant::now(),
                });
                let idx = s.windows.len() - 1;
                s.select_window(idx);
                self.relayout_session(sid);
                Outcome::Ok
            }
            Cmd::JoinPane { src, dst, horizontal, before } => {
                // No -s: the marked pane, as in tmux.
                let marked = self.marked.filter(|p| self.session_of_pane(*p).is_some());
                let (ssid, swidx, spid) = match (&src, marked) {
                    (None, Some(m)) => match self.window_of_pane(m) {
                        Some((s, w)) => (s, w, m),
                        None => return Outcome::Error("join-pane: the marked pane is gone".into()),
                    },
                    _ => match self.resolve(src.as_ref(), cid) {
                        Ok(r) => r,
                        Err(e) => return Outcome::Error(e),
                    },
                };
                let (dsid, dwidx, dpid) = match self.resolve(dst.as_ref(), cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                if spid == dpid {
                    return Outcome::Error("join-pane: source and target are the same pane".into());
                }
                if ssid == dsid && swidx == dwidx && self.session(ssid).unwrap().windows[swidx].panes.len() < 2 {
                    return Outcome::Error("join-pane: the pane is already there".into());
                }
                // Make room in the destination before taking the pane out, so
                // a refused split leaves everything where it was.
                let (dcols, drows) = self.session(dsid).map(|s| (s.cols, s.rows)).unwrap();
                let area = self.window_area(dcols, drows);
                let border = self.border_rows();
                let dw = &mut self.session_mut(dsid).unwrap().windows[dwidx];
                if dw.zoomed {
                    dw.zoomed = false;
                    dw.relayout(area, border);
                }
                let Some(rect) = dw.layout_rect(dpid) else { return Outcome::Error("pane has no layout".into()) };
                let min_rows = if border.is_some() { 5 } else { 3 };
                if (horizontal && rect.w < 3) || (!horizontal && rect.h < min_rows) {
                    return Outcome::Error("pane too small to split".into());
                }
                let Some(pane) = self.take_pane(ssid, swidx, spid) else {
                    return Outcome::Error("join-pane: no such pane".into());
                };
                let dw = &mut self.session_mut(dsid).unwrap().windows[dwidx];
                dw.layout.split_at(dpid, horizontal, spid, rect, before);
                dw.panes.push(pane);
                dw.last_pane = Some(dw.active);
                dw.active = spid;
                dw.layout_preset = None;
                self.relayout_session(dsid);
                if ssid != dsid {
                    self.relayout_session(ssid);
                }
                self.fire_hook("after-split-window", cid);
                self.marked = None;
                self.autosave_changed();
                Outcome::Ok
            }
            Cmd::FindWindow { pattern } => {
                let Some(cid) = cid else { return Outcome::Error("find-window: no client".into()) };
                let needle = pattern.to_lowercase();
                let mut hits: Vec<(SessionId, WindowId, String)> = Vec::new();
                for s in &self.sessions {
                    for (i, w) in s.windows.iter().enumerate() {
                        let title = w.active_pane().map(|p| p.display_title().to_string()).unwrap_or_default();
                        let hay = format!("{} {} {}", w.name, title, i + self.opts.base_index).to_lowercase();
                        if hay.contains(&needle) {
                            hits.push((s.id, w.id, format!("{}:{}", s.name, w.name)));
                        }
                    }
                }
                match hits.len() {
                    0 => Outcome::Error(format!("no window matching: {pattern}")),
                    1 => {
                        let (sid, wid, _) = hits[0].clone();
                        self.chooser_go(cid, sid, Some(wid));
                        Outcome::Ok
                    }
                    _ => {
                        // Several: show them in the picker, filtered.
                        let items: Vec<ChooserItem> =
                            hits.iter().map(|(s, w, _)| ChooserItem::Tree(*s, Some(*w))).collect();
                        let lines: Vec<String> = hits
                            .iter()
                            .enumerate()
                            .map(|(i, (_, _, label))| {
                                format!("{}{label}", if i < 10 { format!("({i}) ") } else { "    ".into() })
                            })
                            .collect();
                        if let Some(c) = self.clients.get_mut(&cid) {
                            c.prompt = None;
                            c.overlay = None;
                            c.chooser = Some(Chooser { kind: ChooserKind::Found, items, lines, sel: 0, top: 0 });
                        }
                        Outcome::Ok
                    }
                }
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
            Cmd::RotateWindow { down, target } => {
                let (sid, widx, _) = match self.resolve(target.as_ref(), cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                let w = &mut self.session_mut(sid).unwrap().windows[widx];
                let mut order = w.layout.panes();
                if order.len() < 2 {
                    return Outcome::Ok;
                }
                // -U moves every pane one place towards the front.
                if down {
                    order.rotate_right(1);
                } else {
                    order.rotate_left(1);
                }
                w.layout.set_panes(&order);
                w.zoomed = false;
                self.relayout_session(sid);
                self.autosave_changed();
                Outcome::Ok
            }
            Cmd::RefreshClient => {
                if let Some(cid) = cid
                    && let Some(c) = self.clients.get_mut(&cid)
                {
                    c.last_grid = None;
                    c.last_cursor = None;
                }
                Outcome::Ok
            }
            Cmd::SendPrefix { target } => {
                let (sid, _, pid) = match self.resolve(target.as_ref(), cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                let key = self.opts.prefix;
                let bytes = input::encode_key(key, false);
                let _ = sid;
                if let Some(p) = self.find_pane_mut(pid) {
                    p.write_input(&bytes);
                }
                Outcome::Ok
            }
            Cmd::ListCommands => {
                let mut names: Vec<&str> = crate::command::COMMANDS.to_vec();
                names.sort_unstable();
                Outcome::Text(names.join("\n"))
            }
            Cmd::ListClients => {
                let mut lines: Vec<String> = self
                    .clients
                    .values()
                    .filter(|c| c.session.is_some())
                    .map(|c| {
                        let name = c.session.and_then(|s| self.session(s)).map(|s| s.name.clone()).unwrap_or_default();
                        format!("client-{}: {} [{}x{}]", c.id, name, c.cols, c.rows)
                    })
                    .collect();
                lines.sort();
                if lines.is_empty() {
                    return Outcome::Text("no clients attached".into());
                }
                Outcome::Text(lines.join("\n"))
            }
            Cmd::CopyCommand { target, name, arg } => {
                let Some(cid) = cid else { return Outcome::Error("send-keys -X: no client".into()) };
                let (_, _, pid) = match self.resolve(target.as_ref(), cid.into()) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                // tmux's copy-mode commands, mapped onto the keys that already
                // do the work, so there is one implementation of each motion.
                let key = match name.as_str() {
                    "cursor-up" => Key::ch('k'),
                    "cursor-down" => Key::ch('j'),
                    "cursor-left" => Key::ch('h'),
                    "cursor-right" => Key::ch('l'),
                    "next-word" | "next-space" => Key::ch('w'),
                    "next-word-end" | "next-space-end" => Key::ch('e'),
                    "previous-word" | "previous-space" => Key::ch('b'),
                    "start-of-line" => Key::ch('0'),
                    "back-to-indentation" => Key::ch('^'),
                    "end-of-line" => Key::ch('$'),
                    "top-line" => Key::ch('H'),
                    "middle-line" => Key::ch('M'),
                    "bottom-line" => Key::ch('L'),
                    "previous-paragraph" => Key::ch('{'),
                    "next-paragraph" => Key::ch('}'),
                    "history-top" => Key::ch('g'),
                    "history-bottom" => Key::ch('G'),
                    "page-up" => Key::ctrl('b'),
                    "page-down" => Key::ctrl('f'),
                    "halfpage-up" => Key::ctrl('u'),
                    "halfpage-down" => Key::ctrl('d'),
                    "begin-selection" => Key::ch(' '),
                    "rectangle-toggle" | "rectangle-on" => Key::ctrl('v'),
                    "copy-selection" | "copy-selection-and-cancel" | "copy-pipe-and-cancel" => Key::ch('y'),
                    "cancel" | "stop-selection" => Key::ch('q'),
                    "search-forward" | "search-backward" => {
                        // With a pattern: search straight away. Without one:
                        // open the prompt, as the `/` and `?` keys do.
                        let back = name == "search-backward";
                        match arg {
                            Some(p) => {
                                if let Some(pane) = self.find_pane_mut(pid)
                                    && pane.copy.is_none()
                                {
                                    enter_copy_mode(pane);
                                }
                                self.search_in_copy_mode(cid, pid, &p, back, true);
                                return Outcome::Ok;
                            }
                            None => Key::ch(if back { '?' } else { '/' }),
                        }
                    }
                    "search-again" => Key::ch('n'),
                    "search-reverse" => Key::ch('N'),
                    other => return Outcome::Error(format!("send-keys -X: unknown command '{other}'")),
                };
                // The pane must be in copy mode for any of this to mean
                // something, which is also what tmux requires.
                if let Some(p) = self.find_pane_mut(pid)
                    && p.copy.is_none()
                {
                    enter_copy_mode(p);
                }
                self.copy_key(cid, pid, key);
                Outcome::Ok
            }
            Cmd::ClockMode { target } => {
                let (_, _, pid) = match self.resolve(target.as_ref(), cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                if let Some(p) = self.find_pane_mut(pid) {
                    p.clock = true;
                }
                Outcome::Ok
            }
            Cmd::IfShell { format, condition, then_cmd, else_cmd } => {
                let truth = if format {
                    let v = match cid {
                        Some(c) => self.expand_format(&condition, c),
                        None => condition.clone(),
                    };
                    let v = v.trim();
                    !v.is_empty() && v != "0"
                } else {
                    // A shell command: success means the "then" branch, as in
                    // tmux. Same environment panes get, same 30s watchdog.
                    let env = self.pane_env(0);
                    let (_, code) = run_shell_blocking(&condition, &env, Some(Duration::from_secs(30)));
                    code == 0
                };
                match (truth, else_cmd) {
                    (true, _) => self.exec(*then_cmd, cid),
                    (false, Some(e)) => self.exec(*e, cid),
                    (false, None) => Outcome::Ok,
                }
            }
            Cmd::DisplayPanes => {
                let Some(cid) = cid else { return Outcome::Error("display-panes: no client".into()) };
                // tmux waits a second by default; `display-time` is ours.
                let ms = self.opts.display_time_ms.max(1000);
                if let Some(c) = self.clients.get_mut(&cid) {
                    c.panes_until = Some(Instant::now() + Duration::from_millis(ms));
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
            Cmd::PasteBuffer { name, target, bracketed } => {
                let (_, _, pid) = match self.resolve(target.as_ref(), cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                // No -b: the Windows clipboard, which is also where copy mode
                // puts what it copies. With -b: that named buffer.
                let text = match &name {
                    Some(n) => match self.buffer(Some(n)) {
                        Some((_, data)) => data.clone(),
                        None => return Outcome::Error(format!("no buffer {n}")),
                    },
                    None => match crate::clipboard::get_text() {
                        Ok(t) => t,
                        Err(e) => return Outcome::Error(format!("clipboard: {e}")),
                    },
                };
                if text.is_empty() {
                    return Outcome::Error(
                        if name.is_some() { "buffer is empty" } else { "clipboard is empty" }.into(),
                    );
                }
                let Some(p) = self.find_pane_mut(pid) else { return Outcome::Error("no such pane".into()) };
                let b = bracketed && p.screen().bracketed_paste();
                p.write_input(&input::encode_paste(&text, b));
                Outcome::Ok
            }
            Cmd::SetBuffer { name, data, append } => {
                self.set_buffer(name.as_deref(), &data, append);
                Outcome::Ok
            }
            Cmd::LoadBuffer { name, path } => match std::fs::read_to_string(expand_home(&path)) {
                Ok(data) => {
                    self.set_buffer(name.as_deref(), &data, false);
                    Outcome::Ok
                }
                Err(e) => Outcome::Error(format!("{path}: {e}")),
            },
            Cmd::SaveBuffer { name, path, append } => {
                let Some((_, data)) = self.buffer(name.as_deref()) else {
                    return Outcome::Error("no buffer".into());
                };
                let data = data.clone();
                let path = expand_home(&path);
                let r = if append {
                    use std::io::Write;
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path)
                        .and_then(|mut f| f.write_all(data.as_bytes()))
                } else {
                    std::fs::write(&path, data.as_bytes())
                };
                match r {
                    Ok(()) => Outcome::Ok,
                    Err(e) => Outcome::Error(format!("{path}: {e}")),
                }
            }
            Cmd::ShowBuffer { name } => match self.buffer(name.as_deref()) {
                Some((_, data)) => Outcome::Text(data.clone()),
                None => Outcome::Error("no buffer".into()),
            },
            Cmd::DeleteBuffer { name } => {
                let idx = match &name {
                    Some(n) => self.buffers.iter().position(|(b, _)| b == n),
                    None => (!self.buffers.is_empty()).then_some(0),
                };
                match idx {
                    Some(i) => {
                        self.buffers.remove(i);
                        Outcome::Ok
                    }
                    None => Outcome::Error("no buffer".into()),
                }
            }
            Cmd::ListBuffers => {
                if self.buffers.is_empty() {
                    return Outcome::Text("no buffers".into());
                }
                let lines: Vec<String> =
                    self.buffers.iter().map(|(n, d)| format!("{n}: {} bytes: {}", d.len(), one_line(d, 60))).collect();
                Outcome::Text(lines.join("\n"))
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
            Cmd::Jobs { target, format } => {
                // A session narrows it to that session, a window to that
                // window, a pane to that one pane (a script asking after one).
                let (only_sid, only_widx, only_pid) = match target.as_ref() {
                    Some(t) => match self.resolve(Some(t), cid) {
                        Ok((s, w, p)) => (Some(s), t.window.as_ref().map(|_| w), t.pane.as_ref().map(|_| p)),
                        Err(e) => return Outcome::Error(e),
                    },
                    None => (None, None, None),
                };
                let panes = self.jobs_panes(only_sid, only_widx, only_pid);
                match &format {
                    Some(f) => {
                        let mut lines = Vec::new();
                        for (sid, widx, pid) in panes {
                            let ctx = self.context(sid, widx, Some(pid), cid);
                            lines.push(self.expand_with_shells(f, &ctx, pid));
                        }
                        Outcome::Text(lines.join("\n"))
                    }
                    None if panes.is_empty() => Outcome::Text(String::new()),
                    None => {
                        // Standard view: a header and aligned columns.
                        let mut rows = vec![jobs_header()];
                        rows.extend(panes.into_iter().map(|(sid, widx, pid)| self.jobs_row(sid, widx, pid, cid)));
                        Outcome::Text(align_columns(&rows).join("\n"))
                    }
                }
            }
            Cmd::DisplayMessage { msg, target } => {
                if cid.is_none() && target.is_none() {
                    return Outcome::Ok; // from the config: nobody to show it to
                }
                if let Err(e) = self.resolve(target.as_ref(), cid) {
                    return Outcome::Error(e);
                }
                Outcome::Text(self.expand_format_at(&msg, target.as_ref(), cid))
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
            Cmd::SetOption { name, value, append } if append && name != "synchronize-panes" => {
                // `set -a`: add to what is there (tmux appends the text).
                let current = self.opts.get(&name).unwrap_or_default();
                self.exec(Cmd::SetOption { name, value: format!("{current}{value}"), append: false }, cid)
            }
            Cmd::SetOption { name, value, .. } if name == "synchronize-panes" => {
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
            Cmd::SetOption { name, value, .. } => {
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
            Cmd::SwitchClient { next, prev, last, target } => {
                let Some(cid) = cid else { return Outcome::Error("switch-client: no client".into()) };
                let cur = self.clients.get(&cid).and_then(|c| c.session);
                let Some(cur) = cur else { return Outcome::Error("client not attached".into()) };
                let pos = self.sessions.iter().position(|s| s.id == cur).unwrap_or(0);
                let n = self.sessions.len();
                let sid = if next {
                    self.sessions[(pos + 1) % n].id
                } else if prev {
                    self.sessions[(pos + n - 1) % n].id
                } else if last {
                    let prev_session =
                        self.clients.get(&cid).and_then(|c| c.last_session).filter(|s| self.session(*s).is_some());
                    match prev_session {
                        Some(s) => s,
                        None => return Outcome::Error("no last session".into()),
                    }
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
                c.last_session = Some(cur);
                c.session = Some(sid);
                c.last_grid = None;
                self.resize_session(sid, cols, rows);
                if let Some(s) = self.session_mut(sid) {
                    s.last_used = Instant::now();
                }
                Outcome::Ok
            }
            Cmd::ChooseBuffer => {
                let Some(cid) = cid else { return Outcome::Error("choose-buffer: no client".into()) };
                if self.clients.get(&cid).and_then(|c| c.session).is_none() {
                    return Outcome::Error("choose-buffer: client not attached".into());
                }
                if self.buffers.is_empty() {
                    return Outcome::Error("no buffers".into());
                }
                let (items, lines) = self.chooser_lines(&ChooserKind::Buffers).unwrap_or_default();
                let c = self.clients.get_mut(&cid).unwrap();
                c.prompt = None;
                c.overlay = None;
                c.chooser = Some(Chooser { kind: ChooserKind::Buffers, items, lines, sel: 0, top: 0 });
                Outcome::Ok
            }
            Cmd::ChooseJobs => {
                let Some(cid) = cid else { return Outcome::Error("choose-jobs: no client".into()) };
                if self.clients.get(&cid).and_then(|c| c.session).is_none() {
                    return Outcome::Error("choose-jobs: client not attached".into());
                }
                let (items, lines) = self.chooser_lines(&ChooserKind::Jobs).unwrap_or_default();
                // Start on the client's own pane.
                let sel = self
                    .resolve(None, Some(cid))
                    .ok()
                    .and_then(|(_, _, pid)| {
                        items.iter().position(|i| matches!(i, ChooserItem::Job(_, _, p) if *p == pid))
                    })
                    .unwrap_or(1);
                let c = self.clients.get_mut(&cid).unwrap();
                c.prompt = None;
                c.overlay = None;
                c.chooser = Some(Chooser { kind: ChooserKind::Jobs, items, lines, sel, top: 0 });
                Outcome::Ok
            }
            Cmd::ChooseClient => {
                let Some(cid) = cid else { return Outcome::Error("choose-client: no client".into()) };
                if self.clients.get(&cid).and_then(|c| c.session).is_none() {
                    return Outcome::Error("choose-client: client not attached".into());
                }
                let (items, lines) = self.chooser_lines(&ChooserKind::Clients).unwrap_or_default();
                let c = self.clients.get_mut(&cid).unwrap();
                c.prompt = None;
                c.overlay = None;
                c.chooser = Some(Chooser { kind: ChooserKind::Clients, items, lines, sel: 0, top: 0 });
                Outcome::Ok
            }
            Cmd::DisplayMenu { title, items: entries } => {
                let Some(cid) = cid else { return Outcome::Error("display-menu: no client".into()) };
                if self.clients.get(&cid).and_then(|c| c.session).is_none() {
                    return Outcome::Error("display-menu: client not attached".into());
                }
                let (mut items, mut lines) = (Vec::new(), Vec::new());
                if let Some(t) = &title {
                    items.push(ChooserItem::Separator);
                    lines.push(self.expand_format(t, cid));
                }
                for (i, e) in entries.iter().enumerate() {
                    match (&e.key, &e.cmd) {
                        (Some(k), Some(_)) => {
                            items.push(ChooserItem::Menu(i));
                            lines.push(format!("({k}) {}", self.expand_format(&e.name, cid)));
                        }
                        // No key or no command: a label nobody can pick.
                        _ => {
                            items.push(ChooserItem::Separator);
                            lines.push(if e.name.is_empty() { "-".repeat(20) } else { e.name.clone() });
                        }
                    }
                }
                let c = self.clients.get_mut(&cid).unwrap();
                c.prompt = None;
                c.overlay = None;
                let mut ch = Chooser { kind: ChooserKind::Menu(entries), items, lines, sel: 0, top: 0 };
                ch.settle(1);
                c.chooser = Some(ch);
                Outcome::Ok
            }
            Cmd::DisplayPopup { close, close_on_exit, width, height, cwd, argv } => {
                let Some(cid) = cid else { return Outcome::Error("display-popup: no client".into()) };
                let Some((cols, rows)) = self.clients.get(&cid).map(|c| (c.cols, c.rows)) else {
                    return Outcome::Error("display-popup: no client".into());
                };
                if close {
                    // From an attached client, close its own popup; from a
                    // script or a pane, close the popup of whoever is looking
                    // at that session, which is the one the user can see.
                    let mut targets = vec![cid];
                    if self.clients.get(&cid).is_none_or(|c| c.popup.is_none())
                        && let Ok(sid) = self.resolve_session(None, Some(cid))
                    {
                        targets = self.clients.values().filter(|c| c.session == Some(sid)).map(|c| c.id).collect();
                    }
                    for id in targets {
                        if let Some(c) = self.clients.get_mut(&id) {
                            c.popup = None;
                            c.last_grid = None;
                        }
                    }
                    return Outcome::Ok;
                }
                let Some(sid) = self.clients.get(&cid).and_then(|c| c.session) else {
                    return Outcome::Error("display-popup: client not attached".into());
                };
                let area = self.window_area(cols, rows);
                let rect = popup_rect(area, width.as_deref(), height.as_deref());
                if rect.w < 3 || rect.h < 3 {
                    return Outcome::Error("display-popup: no room for a popup".into());
                }
                let cwd = self.pane_cwd(cwd.as_deref(), sid, Some(cid));
                let pane = match self.spawn_pane(&argv, cwd.as_deref(), rect.w - 2, rect.h - 2) {
                    Ok(p) => p,
                    Err(e) => return Outcome::Error(e),
                };
                let Some(c) = self.clients.get_mut(&cid) else { return Outcome::Error("no client".into()) };
                // One thing at a time: a popup replaces the picker or an
                // overlay, whose keys it would otherwise steal.
                c.prompt = None;
                c.overlay = None;
                c.chooser = None;
                c.last_grid = None;
                c.popup = Some(Popup { pane, rect, width, height, close_on_exit, finished: false });
                Outcome::Ok
            }
            Cmd::PipePane { target, command, toggle } => {
                let pid = match self.resolve(target.as_ref(), cid) {
                    Ok((_, _, p)) => p,
                    Err(e) => return Outcome::Error(e),
                };
                let running = self.find_pane_mut(pid).is_some_and(|p| p.pipe.is_some());
                let command = match command {
                    // No command at all, or `-o` while one is running: stop.
                    None => None,
                    Some(_) if running && toggle => None,
                    Some(c) => Some(match cid {
                        Some(cid) => self.expand_format(&c, cid),
                        None => c,
                    }),
                };
                let env = self.pane_env(pid);
                let Some(p) = self.find_pane_mut(pid) else { return Outcome::Error("no such pane".into()) };
                match command {
                    None => {
                        p.pipe = None;
                        Outcome::Ok
                    }
                    Some(c) => match p.pipe_to(&c, &env) {
                        Ok(()) => Outcome::Ok,
                        Err(e) => Outcome::Error(format!("{e:#}")),
                    },
                }
            }
            Cmd::WaitFor { channel, lock, unlock, signal } => {
                let Some(cid) = cid else { return Outcome::Error("wait-for: no client".into()) };
                let mut wake: Vec<ClientId> = Vec::new();
                let w = self.waits.entry(channel.clone()).or_default();
                let out = if signal {
                    if w.waiters.is_empty() {
                        w.woken = true;
                    } else {
                        wake.append(&mut w.waiters);
                    }
                    Outcome::Ok
                } else if lock {
                    if w.locked {
                        w.lockers.push_back(cid);
                        Outcome::Pending
                    } else {
                        w.locked = true;
                        Outcome::Ok
                    }
                } else if unlock {
                    if !w.locked {
                        Outcome::Error(format!("wait-for: channel {channel} is not locked"))
                    } else {
                        // The next in line takes the lock straight over.
                        match w.lockers.pop_front() {
                            Some(next) => wake.push(next),
                            None => w.locked = false,
                        }
                        Outcome::Ok
                    }
                } else if w.woken {
                    w.woken = false;
                    Outcome::Ok
                } else {
                    w.waiters.push(cid);
                    Outcome::Pending
                };
                if self.waits.get(&channel).is_some_and(WaitChannel::idle) {
                    self.waits.remove(&channel);
                }
                for id in wake {
                    self.reply(id, Outcome::Ok);
                }
                out
            }
            Cmd::ChooseTree { sessions, windows } => {
                let Some(cid) = cid else { return Outcome::Error("choose-tree: no client".into()) };
                let Some(sid) = self.clients.get(&cid).and_then(|c| c.session) else {
                    return Outcome::Error("choose-tree: client not attached".into());
                };
                let expand = windows || !sessions;
                let kind = ChooserKind::Tree { expand };
                let (items, lines) = self.chooser_lines(&kind).unwrap_or_default();
                // Start on the current window (or session).
                let cur = self.session(sid).and_then(|s| s.window()).map(|w| w.id).filter(|_| expand);
                let sel = items.iter().position(|i| *i == ChooserItem::Tree(sid, cur)).unwrap_or(0);
                let c = self.clients.get_mut(&cid).unwrap();
                // One modal at a time: a picker opened from the `:` prompt
                // replaces it, or its keys would go to an invisible prompt.
                c.prompt = None;
                c.overlay = None;
                c.chooser = Some(Chooser { kind, items, lines, sel, top: 0 });
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
            Cmd::CapturePane { target, history, escapes } => {
                let (_, _, pid) = match self.resolve(target.as_ref(), cid) {
                    Ok(r) => r,
                    Err(e) => return Outcome::Error(e),
                };
                let Some(p) = self.find_pane_mut(pid) else { return Outcome::Error("no such pane".into()) };
                let from = p.scrollback_len().saturating_sub(history);
                Outcome::Text(p.lines_from(from, escapes).join("\n"))
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
            // Window-scoped, so it is not in the global table: answer it from
            // the window the client is looking at.
            Cmd::ShowOptions { ref name, value_only, quiet: _ } if name.as_deref() == Some("synchronize-panes") => {
                let on = match self.resolve(None, cid) {
                    Ok((sid, widx, _)) => {
                        self.session(sid).and_then(|s| s.windows.get(widx)).is_some_and(|w| w.synchronized)
                    }
                    Err(e) => return Outcome::Error(e),
                };
                let v = if on { "on" } else { "off" };
                Outcome::Text(if value_only { v.to_string() } else { format!("synchronize-panes {v}") })
            }
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
                    // Window-scoped, so it is not in the table, but leaving it
                    // out of the listing hides an option that can be set.
                    if let Ok((sid, widx, _)) = self.resolve(None, cid)
                        && let Some(w) = self.session(sid).and_then(|s| s.windows.get(widx))
                    {
                        lines.push(format!("synchronize-panes {}", if w.synchronized { "on" } else { "off" }));
                    }
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

    /// Expand a format string for `display-message` and the prompts, with the
    /// same engine the status line uses, so `#{...}`, `#{?...}` and `#(...)`
    /// mean the same thing everywhere.
    fn expand_format(&mut self, s: &str, cid: ClientId) -> String {
        self.expand_format_at(s, None, Some(cid))
    }

    /// `s` expanded for `target` (the client's current pane when None).
    fn expand_format_at(&mut self, s: &str, target: Option<&Target>, cid: Option<ClientId>) -> String {
        let Ok((sid, widx, pid)) = self.resolve(target, cid) else { return s.to_string() };
        let ctx = self.context(sid, widx, Some(pid), cid);
        self.expand_with_shells(s, &ctx, pid)
    }

    /// `s` expanded for `ctx` as text, for a one-shot answer
    /// (`display-message -p`, `jobs -F`): a `#(command)` the status line has
    /// not run yet is run here, with a short leash, since tmux waits for it
    /// too and "empty until the next status-interval" is no answer.
    fn expand_with_shells(&mut self, s: &str, ctx: &crate::format::Context, pid: PaneId) -> String {
        let now = chrono::Local::now();
        let base = render::Style::default();
        let text = |segs: Vec<crate::format::Segment>| segs.into_iter().map(|seg| seg.text).collect::<String>();
        let first = crate::format::expand(s, ctx, &mut self.shell_cache, base, now);
        let missing: Vec<String> = std::mem::take(&mut self.shell_cache.wanted)
            .into_iter()
            .filter(|c| !self.shell_cache.results.contains_key(c))
            .collect();
        if missing.is_empty() {
            return text(first);
        }
        let mut env = self.pane_env(pid);
        env.retain(|(k, _)| k != "WMUX_PANE");
        for cmd in missing {
            let (output, _) = run_shell_blocking(&cmd, &env, Some(ONE_SHOT_SHELL_TIMEOUT));
            let line = output.lines().next().unwrap_or("").trim_end().to_string();
            self.shell_cache.results.insert(cmd, line);
        }
        text(crate::format::expand(s, ctx, &mut self.shell_cache, base, now))
    }

    /// Everything a format can ask about a session, one of its windows, a
    /// pane of that window (the active one when `pid` is None) and the
    /// client it is drawn for. The one place every `#{...}` is answered from.
    fn context(
        &self,
        sid: SessionId,
        widx: usize,
        pid: Option<PaneId>,
        cid: Option<ClientId>,
    ) -> crate::format::Context {
        let mut ctx = crate::format::Context {
            host: std::env::var("COMPUTERNAME").unwrap_or_default(),
            socket: self.socket.clone(),
            ..Default::default()
        };
        let Some(sess) = self.session(sid) else { return ctx };
        ctx.session = sess.name.clone();
        ctx.session_id = sess.id;
        ctx.session_windows = sess.windows.len();
        ctx.session_attached = self.clients.values().filter(|c| c.session == Some(sid)).count();
        ctx.session_created = sess.created_at;
        if let Some(c) = cid.and_then(|c| self.clients.get(&c)) {
            ctx.client_width = c.cols;
            ctx.client_height = c.rows;
        }
        let Some(w) = sess.windows.get(widx) else { return ctx };
        ctx.window = w.name.clone();
        ctx.window_id = w.id;
        ctx.window_index = widx + self.opts.base_index;
        ctx.window_panes = w.panes.len();
        ctx.window_active = widx == sess.cur;
        ctx.window_last = Some(w.id) == sess.last;
        ctx.window_zoomed = w.zoomed;
        ctx.window_width = sess.cols;
        ctx.window_height = sess.rows;
        ctx.window_bell = w.alert_bell;
        ctx.window_activity = w.alert_activity;
        ctx.window_silence = w.alert_silence;
        ctx.flags = w.flags(widx == sess.cur, Some(w.id) == sess.last);
        let pid = pid.unwrap_or(w.active);
        let order = w.layout.panes();
        ctx.pane_index = order.iter().position(|p| *p == pid).unwrap_or(0) + self.opts.pane_base_index;
        ctx.pane_id = pid;
        ctx.pane_active = pid == w.active;
        ctx.pane_synchronized = w.synchronized;
        if let Some(p) = w.pane(pid) {
            ctx.pane_title = p.display_title().to_string();
            ctx.pane_command = p.command.clone();
            ctx.pane_start_command = p.argv.join(" ");
            ctx.pane_path = p.cwd.clone().unwrap_or_default();
            ctx.pane_width = p.cols;
            ctx.pane_height = p.rows;
            ctx.pane_dead = p.exit_code.is_some();
            ctx.pane_dead_status = p.exit_code;
            ctx.pane_in_mode = p.copy.is_some();
            ctx.pane_pid = p.pid;
            let now = chrono::Local::now().timestamp();
            ctx.pane_start_time = now - p.spawned_at.elapsed().as_secs() as i64;
            ctx.pane_activity = now - p.last_output.elapsed().as_secs() as i64;
            ctx.pane_dead_time = p.died_at.map(|d| now - d.elapsed().as_secs() as i64).unwrap_or(0);
        }
        ctx
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
        // While `display-panes` numbers are up, a digit picks that pane and
        // anything else just puts them away (tmux does the same).
        if c.panes_until.is_some_and(|t| Instant::now() < t)
            && let Some(k) = key
        {
            c.panes_until = None;
            c.swallow_up.insert(rec.vk);
            if let KeyCode::Char(d @ '0'..='9') = k.code {
                let idx = d as usize - '0' as usize;
                let out = self.exec(Cmd::SelectPane { sel: PaneSel::Index(idx) }, Some(cid));
                self.reply(cid, out);
            }
            return;
        }

        // A clock swallows the next key and goes away, as tmux does.
        if key.is_some()
            && let Some(p) = self.session_mut(sid).and_then(|s| s.window_mut()).and_then(|w| w.active_pane_mut())
            && p.clock
        {
            p.clock = false;
            if let Some(c) = self.clients.get_mut(&cid) {
                c.swallow_up.insert(rec.vk);
                c.last_grid = None;
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
            // A popup takes every key but the prefix, so `prefix d` still
            // detaches and `prefix :` still reaches the command prompt. The
            // key-up goes through too (as it does for a pane), except for the
            // key that closes a finished popup: that one is not for the pane
            // behind it either.
            if let Some(p) = c.popup.as_ref() {
                if p.finished {
                    c.swallow_up.insert(rec.vk);
                }
                self.popup_key(cid, &rec);
                return;
            }
            if in_chooser {
                c.swallow_up.insert(rec.vk);
                self.chooser_key(cid, k);
                return;
            }
            if in_copy {
                c.swallow_up.insert(rec.vk);
                if let Some(pid) = self.session(sid).and_then(|s| s.window()).map(|w| w.active) {
                    self.copy_key(cid, pid, k);
                }
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
        } else if self.clients.get(&cid).is_some_and(|c| c.popup.is_some()) {
            self.popup_key(cid, &rec);
            return;
        }
        self.write_active(sid, &input::encode_key_record(&rec));
    }

    /// A key while a popup is open: it goes to the popup's program, or closes
    /// the box when the program has already finished.
    fn popup_key(&mut self, cid: ClientId, rec: &KeyRecord) {
        let Some(c) = self.clients.get_mut(&cid) else { return };
        let Some(p) = c.popup.as_mut() else { return };
        if p.finished {
            if rec.down {
                c.popup = None;
                c.last_grid = None;
            }
            return;
        }
        p.pane.write_input(&input::encode_key_record(rec));
    }

    /// Keep a popup centred and sized as it was asked for. Called before
    /// every frame, so the box follows the window area whatever changed it:
    /// a resized terminal, or `set -g status off` giving a row back.
    fn refit_popup(&mut self, cid: ClientId) {
        let Some(c) = self.clients.get(&cid) else { return };
        if c.popup.is_none() {
            return;
        }
        let area = self.window_area(c.cols, c.rows);
        let c = self.clients.get_mut(&cid).unwrap();
        let p = c.popup.as_mut().unwrap();
        let rect = popup_rect(area, p.width.as_deref(), p.height.as_deref());
        if rect.w < 3 || rect.h < 3 {
            c.popup = None; // nowhere left to draw it
            return;
        }
        p.rect = rect;
        p.pane.resize(rect.w - 2, rect.h - 2);
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
            PromptKind::Command { .. } | PromptKind::Search { .. } => match (k.code, k.ctrl, k.alt) {
                (KeyCode::Escape, _, _) | (KeyCode::Char('c'), true, _) | (KeyCode::Char('g'), true, _) => {
                    c.prompt = None;
                }
                (KeyCode::Enter, _, _) if matches!(p.kind, PromptKind::Search { .. }) => {
                    let prompt = c.prompt.take().unwrap();
                    let PromptKind::Search { back } = prompt.kind else { unreachable!() };
                    let pid = self
                        .clients
                        .get(&cid)
                        .and_then(|c| c.session)
                        .and_then(|sid| self.session(sid))
                        .and_then(|s| s.window())
                        .map(|w| w.active);
                    if let Some(pid) = pid {
                        self.search_in_copy_mode(cid, pid, &prompt.input, back, true);
                    }
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

    /// `/` `?` `n` `N` in copy mode: move the cursor to the next line holding
    /// `pattern`, case-insensitively, and remember it for `n`.
    ///
    /// `back` searches towards older lines, which is what `?` does (tmux's
    /// `search-backward`). `fresh` starts from the cursor line itself so that
    /// a new search can match what is already on screen.
    fn search_in_copy_mode(&mut self, cid: ClientId, pid: PaneId, pattern: &str, back: bool, fresh: bool) {
        let pattern = pattern.to_string();
        if pattern.is_empty() {
            return;
        }
        let Some(p) = self.find_pane_mut(pid) else {
            return;
        };
        if p.copy.is_none() {
            enter_copy_mode(p);
        }
        let needle = pattern.to_lowercase();
        let from = copy_abs(p);
        let last = p.scrollback_len() + p.rows as usize - 1;
        // Where to look, nearest first.
        let candidates: Vec<usize> = if back {
            let start = if fresh { from } else { from.saturating_sub(1) };
            (0..=start).rev().collect()
        } else {
            let start = if fresh { from } else { from + 1 };
            (start..=last).collect()
        };
        let hit = candidates.into_iter().find_map(|abs| {
            let (text, _) = p.line_text(abs);
            text.to_lowercase().find(&needle).map(|byte| {
                // Column in characters, which is what the copy cursor counts.
                let col = text[..byte].chars().count() as u16;
                (abs, col)
            })
        });
        let Some((abs, col)) = hit else {
            if let Some(c) = p.copy.as_mut() {
                c.search = Some(pattern.clone());
                c.search_back = back;
            }
            self.message(cid, &format!("no match: {pattern}"));
            return;
        };
        // Put the hit in the middle of the view when there is room above it.
        let max = p.scrollback_len();
        let rows = p.rows as usize;
        let want_row = (rows / 2).min(abs);
        let offset = (max + want_row).saturating_sub(abs).min(max);
        let cy = (abs + offset).saturating_sub(max).min(rows.saturating_sub(1));
        let cols = p.cols;
        if let Some(c) = p.copy.as_mut() {
            c.offset = offset;
            c.cy = cy as u16;
            c.cx = col.min(cols.saturating_sub(1));
            c.search = Some(pattern);
            c.search_back = back;
        }
    }

    // ----------------------------------------------------------- choose-tree

    /// The picker's lines from the live sessions, tmux `choose-tree` style:
    /// `(n) - name: 2 windows (attached)` and, when expanded,
    /// `(n)   - 0: cmd* (2 panes) "title"` under each session.
    /// The panes `jobs` lists, in session / window / layout order, narrowed
    /// to a session, a window of it, or one pane.
    fn jobs_panes(
        &self,
        only_sid: Option<SessionId>,
        only_widx: Option<usize>,
        only_pid: Option<PaneId>,
    ) -> Vec<(SessionId, usize, PaneId)> {
        let mut panes = Vec::new();
        for s in self.sessions.iter().filter(|s| only_sid.is_none_or(|id| id == s.id)) {
            for (wi, w) in s.windows.iter().enumerate().filter(|(wi, _)| only_widx.is_none_or(|i| i == *wi)) {
                panes.extend(
                    w.layout.panes().into_iter().filter(|p| only_pid.is_none_or(|id| id == *p)).map(|p| (s.id, wi, p)),
                );
            }
        }
        panes
    }

    /// One pane's row of the task board, the columns of `jobs_header`.
    fn jobs_row(&self, sid: SessionId, widx: usize, pid: PaneId, cid: Option<ClientId>) -> Vec<String> {
        let ctx = self.context(sid, widx, Some(pid), cid);
        let t = chrono::Local::now().timestamp();
        vec![
            format!("{}:{}.{}", ctx.session, ctx.window_index, ctx.pane_index),
            match ctx.pane_dead_status {
                Some(code) => format!("exit {code}"),
                None => "running".to_string(),
            },
            // A dead pane's clock stopped when it died: how long it ran.
            crate::format::human_duration(
                (if ctx.pane_dead_time > 0 { ctx.pane_dead_time } else { t }) - ctx.pane_start_time,
            ),
            crate::format::human_duration(t - ctx.pane_activity),
            ctx.pane_pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            ctx.pane_title,
            ctx.pane_path,
        ]
    }

    fn chooser_lines(&self, kind: &ChooserKind) -> Option<(Vec<ChooserItem>, Vec<String>)> {
        let mut items = Vec::new();
        let mut lines = Vec::new();
        let tag = |n: usize| if n < 10 { format!("({n}) ") } else { "    ".to_string() };
        let expand = match kind {
            ChooserKind::Tree { expand } => *expand,
            ChooserKind::Buffers => {
                for (i, (name, data)) in self.buffers.iter().enumerate() {
                    items.push(ChooserItem::Buffer(i));
                    lines.push(format!("{}{name}: {} bytes: {}", tag(i), data.len(), one_line(data, 60)));
                }
                return Some((items, lines));
            }
            ChooserKind::Clients => {
                let mut ids: Vec<ClientId> =
                    self.clients.values().filter(|c| c.session.is_some()).map(|c| c.id).collect();
                ids.sort_unstable();
                for id in ids {
                    let Some(c) = self.clients.get(&id) else { continue };
                    let session = c.session.and_then(|s| self.session(s)).map(|s| s.name.as_str()).unwrap_or("-");
                    items.push(ChooserItem::Client(id));
                    lines.push(format!("{}client-{id}: {session} [{}x{}]", tag(lines.len()), c.cols, c.rows));
                }
                return Some((items, lines));
            }
            ChooserKind::Jobs => {
                // The task board: the header is a title line, every other
                // line is a pane to jump to, kill or restart.
                let panes = self.jobs_panes(None, None, None);
                let mut rows = vec![jobs_header()];
                rows.extend(panes.iter().map(|(sid, widx, pid)| self.jobs_row(*sid, *widx, *pid, None)));
                items.push(ChooserItem::Separator);
                for (sid, widx, pid) in &panes {
                    let wid = self.session(*sid).and_then(|s| s.windows.get(*widx)).map(|w| w.id).unwrap_or(0);
                    items.push(ChooserItem::Job(*sid, wid, *pid));
                }
                for (i, line) in align_columns(&rows).into_iter().enumerate() {
                    // Jump tags count items, and the header is item 0.
                    lines.push(format!("{}{line}", if i == 0 { "    ".to_string() } else { tag(i) }));
                }
                return Some((items, lines));
            }
            ChooserKind::Found | ChooserKind::Menu(_) => return None,
        };
        for s in &self.sessions {
            let attached = self.clients.values().any(|c| c.session == Some(s.id));
            items.push(ChooserItem::Tree(s.id, None));
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
                items.push(ChooserItem::Tree(s.id, Some(w.id)));
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
        Some((items, lines))
    }

    fn chooser_key(&mut self, cid: ClientId, k: Key) {
        let Some((cols, rows)) = self.clients.get(&cid).map(|c| (c.cols, c.rows)) else { return };
        let page = self.window_area(cols, rows).h.saturating_sub(1).max(1) as i64; // body rows of the picker
        let Some(c) = self.clients.get_mut(&cid) else { return };
        // A menu entry's own key picks it, ahead of the movement keys, so a
        // menu is free to use `j` or `q` for something.
        let menu_hit = match c.chooser.as_ref().map(|ch| &ch.kind) {
            Some(ChooserKind::Menu(entries)) if k.code != KeyCode::Escape => entries
                .iter()
                .find(|e| e.cmd.is_some() && e.key.as_deref().and_then(Key::parse) == Some(k))
                .and_then(|e| e.cmd.clone()),
            _ => None,
        };
        if let Some(cmd) = menu_hit {
            c.chooser = None;
            let out = self.exec(*cmd, Some(cid));
            self.reply(cid, out);
            return;
        }
        let Some(ch) = c.chooser.as_mut() else { return };
        match (k.code, k.ctrl, k.alt) {
            (KeyCode::Escape, _, _) | (KeyCode::Char('q'), false, false) | (KeyCode::Char('c'), true, _) => {
                c.chooser = None;
            }
            (KeyCode::Down, _, _) | (KeyCode::Char('j'), false, false) | (KeyCode::Char('n'), true, _) => ch.step(1),
            (KeyCode::Up, _, _) | (KeyCode::Char('k'), false, false) | (KeyCode::Char('p'), true, _) => ch.step(-1),
            (KeyCode::Home, _, _) | (KeyCode::Char('g'), false, false) => {
                ch.sel = 0;
                ch.settle(1);
            }
            (KeyCode::End, _, _) | (KeyCode::Char('G'), false, false) => {
                ch.sel = ch.items.len().saturating_sub(1);
                ch.settle(-1);
            }
            (KeyCode::NPage, _, _) | (KeyCode::Char('f'), true, _) => ch.step(page),
            (KeyCode::PPage, _, _) | (KeyCode::Char('b'), true, _) => ch.step(-page),
            (KeyCode::Char('d'), true, _) => ch.step(page / 2),
            (KeyCode::Char('u'), true, _) => ch.step(-(page / 2)),
            // The task board acts on the pane under the cursor and stays
            // open (the list is live, so the row changes in place).
            (KeyCode::Char(key @ ('x' | 'r')), false, false) if matches!(ch.kind, ChooserKind::Jobs) => {
                if let Some(ChooserItem::Job(sid, wid, pid)) = ch.items.get(ch.sel).copied() {
                    let Some(target) = self.pane_target(sid, wid, pid) else {
                        self.message(cid, "pane is gone");
                        return;
                    };
                    let cmd = if key == 'x' {
                        Cmd::KillPane { target: Some(target), all_but: false }
                    } else {
                        Cmd::RespawnPane { target: Some(target), kill: true, argv: Vec::new(), window: false }
                    };
                    if let Outcome::Error(e) = self.exec(cmd, Some(cid)) {
                        self.message(cid, &e);
                    }
                }
            }
            (KeyCode::Char(d @ '0'..='9'), false, false) => {
                let i = d as usize - '0' as usize;
                if i < ch.items.len() && !matches!(ch.items[i], ChooserItem::Separator) {
                    ch.sel = i;
                }
            }
            (KeyCode::Enter, _, _) => {
                let target = ch.items.get(ch.sel).copied();
                let menu = match (&ch.kind, target) {
                    (ChooserKind::Menu(entries), Some(ChooserItem::Menu(i))) => {
                        entries.get(i).and_then(|e| e.cmd.clone())
                    }
                    _ => None,
                };
                c.chooser = None;
                if let Some(cmd) = menu {
                    let out = self.exec(*cmd, Some(cid));
                    self.reply(cid, out);
                    return;
                }
                match target {
                    Some(ChooserItem::Tree(sid, wid)) => self.chooser_go(cid, sid, wid),
                    Some(ChooserItem::Client(other)) => {
                        if self.clients.contains_key(&other) {
                            self.detach(other, "detached");
                        } else {
                            self.message(cid, "client is gone");
                        }
                    }
                    Some(ChooserItem::Job(sid, wid, pid)) => self.chooser_go_pane(cid, sid, wid, pid),
                    Some(ChooserItem::Menu(_)) | Some(ChooserItem::Separator) => {}
                    Some(ChooserItem::Buffer(i)) => {
                        let name = self.buffers.get(i).map(|(n, _)| n.clone());
                        match name {
                            Some(n) => {
                                let out = self
                                    .exec(Cmd::PasteBuffer { name: Some(n), target: None, bracketed: true }, Some(cid));
                                self.reply(cid, out);
                            }
                            None => self.message(cid, "buffer is gone"),
                        }
                    }
                    None => {}
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
                last: false,
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

    /// `session:window.pane` for a pane, as the commands want it; None
    /// when it is gone.
    fn pane_target(&self, sid: SessionId, wid: WindowId, pid: PaneId) -> Option<Target> {
        let s = self.session(sid)?;
        let widx = s.windows.iter().position(|w| w.id == wid)?;
        let w = &s.windows[widx];
        let pidx = w.layout.panes().iter().position(|p| *p == pid)?;
        Some(Target {
            session: Some(s.name.clone()),
            window: Some((widx + self.opts.base_index).to_string()),
            pane: Some(pidx + self.opts.pane_base_index),
        })
    }

    /// Switch the client to a pane: its session, its window, then the pane.
    fn chooser_go_pane(&mut self, cid: ClientId, sid: SessionId, wid: WindowId, pid: PaneId) {
        if self.pane_target(sid, wid, pid).is_none() {
            self.message(cid, "pane is gone");
            return;
        }
        self.chooser_go(cid, sid, Some(wid));
        if let Some(s) = self.session_mut(sid)
            && let Some(w) = s.windows.iter_mut().find(|w| w.id == wid)
            && w.active != pid
            && w.pane(pid).is_some()
        {
            w.last_pane = Some(w.active);
            w.active = pid;
            self.fire_hook("after-select-pane", Some(cid));
        }
    }

    // ------------------------------------------------------------- copy mode

    fn copy_key(&mut self, cid: ClientId, pid: PaneId, k: Key) {
        let Some(p) = self.find_pane_mut(pid) else {
            return;
        };
        let rows = p.rows as i64;
        let cols = p.cols;
        // vi counts: digits before a motion repeat it ("3j"), except a 0 with
        // no count yet, which is "start of line".
        let has_count = p.copy.as_ref().and_then(|c| c.count).is_some();
        if let KeyCode::Char(d @ '0'..='9') = k.code
            && !k.ctrl
            && !k.alt
            && (d != '0' || has_count)
            && let Some(c) = p.copy.as_mut()
        {
            let n = c.count.unwrap_or(0).saturating_mul(10) + (d as usize - '0' as usize);
            c.count = Some(n.min(10_000));
            return;
        }
        let count = p.copy.as_mut().and_then(|c| c.count.take()).unwrap_or(1).max(1);
        // Motions that repeat with a count do so here, once each.
        if count > 1
            && matches!(
                (k.code, k.ctrl),
                (KeyCode::Char('j' | 'k' | 'h' | 'l' | 'w' | 'b' | 'e' | 'W' | 'B' | 'E'), false)
                    | (KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right, _)
            )
        {
            for _ in 0..count {
                self.copy_key(cid, pid, k);
            }
            return;
        }
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
            // A rectangle instead of whole lines, as vi's C-v does.
            (KeyCode::Char('v'), true, _) => {
                let abs = copy_abs(p);
                let c = p.copy.as_mut().unwrap();
                c.rect = !c.rect;
                if c.anchor.is_none() {
                    c.anchor = Some((abs, c.cx));
                }
            }
            // Word motions.
            (KeyCode::Char(w @ ('w' | 'W' | 'b' | 'B' | 'e' | 'E')), false, false) => {
                copy_word_motion(p, w);
            }
            // First non-blank of the line.
            (KeyCode::Char('^'), false, false) => {
                let abs = copy_abs(p);
                let (line, _) = p.line_text(abs);
                let col = line.chars().take_while(|ch| ch.is_whitespace()).count() as u16;
                let c = p.copy.as_mut().unwrap();
                c.cx = col.min(cols.saturating_sub(1));
            }
            // Top, middle and bottom of what is on screen.
            (KeyCode::Char('H'), false, false) => c.cy = 0,
            (KeyCode::Char('M'), false, false) => c.cy = (rows / 2) as u16,
            (KeyCode::Char('L'), false, false) => c.cy = (rows - 1).max(0) as u16,
            // Paragraphs: the next or previous blank line.
            (KeyCode::Char(dir @ ('{' | '}')), false, false) => {
                let back = dir == '{';
                let from = copy_abs(p);
                let last = p.scrollback_len() + p.rows as usize - 1;
                let mut abs = from;
                loop {
                    if back {
                        if abs == 0 {
                            break;
                        }
                        abs -= 1;
                    } else {
                        if abs >= last {
                            break;
                        }
                        abs += 1;
                    }
                    if p.line_text(abs).0.trim().is_empty() && abs != from {
                        break;
                    }
                }
                copy_goto(p, abs);
            }
            // Search as tmux does: / looks forward (towards the newest line),
            // ? looks back through the scrollback, n and N repeat.
            (KeyCode::Char('/'), false, false) | (KeyCode::Char('?'), false, false) => {
                let back = k.code == KeyCode::Char('?');
                if let Some(cl) = self.clients.get_mut(&cid) {
                    cl.prompt = Some(Prompt {
                        kind: PromptKind::Search { back },
                        label: if back { "?".into() } else { "/".into() },
                        input: String::new(),
                        cursor: 0,
                    });
                }
            }
            (KeyCode::Char('n'), false, false) | (KeyCode::Char('N'), false, false) => {
                let Some((pattern, back)) = c.search.clone().map(|s| (s, c.search_back)) else {
                    self.message(cid, "no previous search");
                    return;
                };
                let back = if k.code == KeyCode::Char('N') { !back } else { back };
                self.search_in_copy_mode(cid, pid, &pattern, back, false);
            }
            (KeyCode::Enter, _, _) | (KeyCode::Char('y'), false, false) | (KeyCode::Char('w'), true, _) => {
                if c.anchor.is_some() {
                    match copy_selection(p) {
                        Ok((n, text)) => {
                            // Both places, as tmux with set-clipboard does:
                            // a new paste buffer and the Windows clipboard.
                            self.set_buffer(None, &text, false);
                            self.message(cid, &format!("copied {n} characters"));
                        }
                        Err(e) => self.message(cid, &e),
                    }
                }
                if let Some(p) = self.find_pane_mut(pid) {
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
        // A popup owns the keyboard but not the mouse: the click would land
        // on a pane it covers, so drop it instead.
        if c.prompt.is_some() || c.overlay.is_some() || c.popup.is_some() {
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
                        Ok((n, text)) => {
                            self.set_buffer(None, &text, false);
                            self.message(cid, &format!("copied {n} characters"));
                        }
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
        self.sweep_alerts();
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
        self.refit_popup(cid);
        let Some(c) = self.clients.get(&cid) else { return };
        let Some(sid) = c.session else { return };
        let (cols, rows) = (c.cols, c.rows);
        // A picker of live things (the tree, buffers, clients) is rebuilt every
        // render; the selection follows the item's identity, so things
        // appearing or vanishing meanwhile do not move it. A fixed list (the
        // find-window hits, a menu) is left alone.
        let fresh = match c.chooser.as_ref() {
            Some(ch) if ch.kind.live() => {
                self.chooser_lines(&ch.kind).map(|(i, l)| (i, l, ch.items.get(ch.sel).copied(), ch.sel))
            }
            _ => None,
        };
        if let Some((items, lines, want, sel)) = fresh {
            let ch = self.clients.get_mut(&cid).unwrap().chooser.as_mut().unwrap();
            ch.sel =
                want.and_then(|w| items.iter().position(|i| *i == w)).unwrap_or(sel.min(items.len().saturating_sub(1)));
            ch.items = items;
            ch.lines = lines;
        }
        let Some(spos) = self.sessions.iter().position(|s| s.id == sid) else { return };
        let pane_base_index = self.opts.pane_base_index;
        let (status_top, border_fg, active_fg, opts_status) =
            (self.opts.status_top, self.opts.pane_border_fg, self.opts.pane_border_active_fg, self.opts.status);
        let (message, prompt) = {
            let c = self.clients.get(&cid).unwrap();
            (
                c.message.as_ref().map(|(m, _)| m.clone()),
                c.prompt.as_ref().map(|p| (p.label.clone(), p.input.clone(), p.cursor)),
            )
        };
        // (The alerts of the window being drawn were cleared by the sweep at
        // the top of render_all.)
        let mut bell = self.clients.get_mut(&cid).is_some_and(|c| std::mem::take(&mut c.pending_bell));
        let status_line = if opts_status {
            let base = render::Style::colors(self.opts.status_fg, self.opts.status_bg);
            let now = chrono::Local::now();
            // One context per window, built while nothing is borrowed
            // mutably; the status line's title is clipped so a long path
            // in it cannot push the window list off the line.
            let (cur, n) = (self.sessions[spos].cur, self.sessions[spos].windows.len());
            let ctxs: Vec<crate::format::Context> = (0..n)
                .map(|i| {
                    let mut c = self.context(sid, i, None, Some(cid));
                    c.pane_title = truncate(&c.pane_title, 30);
                    c
                })
                .collect();
            let cache = &mut self.shell_cache;
            let cur_ctx = ctxs.get(cur).cloned().unwrap_or_default();
            let mut left = crate::format::expand(&self.opts.status_left, &cur_ctx, cache, base, now);
            let mut right = crate::format::expand(&self.opts.status_right, &cur_ctx, cache, base, now);
            clip_segments(&mut left, self.opts.status_left_length);
            clip_segments(&mut right, self.opts.status_right_length);
            let windows = ctxs
                .iter()
                .enumerate()
                .map(|(i, ctx)| {
                    let fmt = if i == cur {
                        &self.opts.window_status_current_format
                    } else {
                        &self.opts.window_status_format
                    };
                    (crate::format::expand(fmt, ctx, cache, base, now), i == cur)
                })
                .collect();
            Some(StatusLine {
                left,
                windows,
                separator: self.opts.window_status_separator.clone(),
                justify: render::Justify::parse(&self.opts.status_justify),
                right,
                message,
                prompt,
                fg: self.opts.status_fg,
                bg: self.opts.status_bg,
            })
        } else {
            None
        };
        self.refresh_status_shells();
        // `pane-border-status`: the text for each pane's border row, expanded
        // before the window is borrowed for drawing.
        let border_top = self.border_rows();
        let mut border_texts: HashMap<PaneId, Vec<crate::format::Segment>> = HashMap::new();
        if border_top.is_some() {
            let widx = self.sessions[spos].cur;
            let ids: Vec<PaneId> = self.sessions[spos]
                .windows
                .get(widx)
                .map(|w| w.rects.iter().map(|(id, _)| *id).collect())
                .unwrap_or_default();
            let now = chrono::Local::now();
            let active_id = self.sessions[spos].windows.get(widx).map(|w| w.active);
            for id in ids {
                let ctx = self.context(sid, widx, Some(id), Some(cid));
                let fg = if Some(id) == active_id { active_fg } else { border_fg };
                let base = render::Style::colors(fg, vt100::Color::Default);
                let segs = crate::format::expand(&self.opts.pane_border_format, &ctx, &mut self.shell_cache, base, now);
                border_texts.insert(id, segs);
            }
        }
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
                copy_views.insert(p.id, CopyView { cx: cm.cx, cy: cm.cy, sel, rect: cm.rect, offset: cm.offset });
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
                border_text: border_top.and_then(|top| border_texts.remove(id).map(|segs| (segs, top))),
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
        // `clock-mode`: a big clock over the panes that asked for one.
        {
            let now = chrono::Local::now().format("%H:%M").to_string();
            let w = &self.sessions[spos].windows[self.sessions[spos].cur];
            let clocks: Vec<Rect> =
                w.rects.iter().filter(|(id, _)| w.pane(*id).is_some_and(|p| p.clock)).map(|(_, r)| *r).collect();
            for rect in clocks {
                let style = render::Style::colors(self.opts.pane_border_active_fg, vt100::Color::Default);
                grid.fill(rect, render::Style::default());
                if !render::draw_big_text(&mut grid, rect, &now, style) && rect.w > 0 && rect.h > 0 {
                    grid.put_str(rect.x, rect.y, &now, style, rect.w);
                }
                cursor = None;
            }
        }
        // `display-panes`: each pane wears its number until the timer runs out.
        if self.clients.get(&cid).is_some_and(|c| c.panes_until.is_some_and(|t| Instant::now() < t)) {
            let w = &self.sessions[spos].windows[self.sessions[spos].cur];
            let order = w.layout.panes();
            for (id, rect) in &w.rects {
                let n = order.iter().position(|p| p == id).unwrap_or(0) + pane_base_index;
                render::draw_pane_number(&mut grid, *rect, n, *id == w.active);
            }
            cursor = None;
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
            let actions = match ch.kind {
                ChooserKind::Jobs => "Enter go  x kill  r restart",
                ChooserKind::Clients => "Enter detach",
                ChooserKind::Buffers => "Enter paste",
                _ => "Enter select",
            };
            render::draw_chooser(&mut grid, area, &ch.lines, ch.sel, ch.top, actions);
            cursor = None;
        }
        // An overlay (a hook's message, a `run-shell` result) draws over the
        // picker, because the next key goes to the overlay, not the picker.
        if let Some(lines) = &c.overlay {
            render::draw_overlay(&mut grid, area, lines);
            cursor = None;
        }
        // The popup is the thing with the keyboard, so it draws last.
        if let Some(p) = c.popup.as_ref() {
            let hint = p.finished.then_some("press any key");
            cursor = render::draw_popup(&mut grid, p.rect, p.pane.screen(), active_fg, hint);
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

/// The lines of a config file the way tmux reads them: a `\` at the end of
/// a line continues it on the next, and a `%if` ... `%endif` block (tmux's
/// conditionals, which wmux does not evaluate) is left out whole rather
/// than having both of its branches applied. Each logical line carries the
/// number of its first physical line, for error messages. The second value
/// is the line of a `%if` that was never closed, which swallows everything
/// after it and must be said rather than silently dropped.
fn logical_lines(text: &str) -> (Vec<(usize, String)>, Option<usize>) {
    let mut out = Vec::new();
    let mut joined = String::new();
    let mut start = 0;
    let mut depth = 0usize;
    let mut opened_at = 0usize;
    // Notepad writes a byte-order mark; without this the first command
    // would be "\u{feff}set", which is nothing.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        let trimmed = raw.trim_start();
        if trimmed.starts_with('%') {
            // %if / %elif / %else / %endif / %hidden: all skipped, and a
            // block's body with it.
            let word = trimmed.split_whitespace().next().unwrap_or("");
            match word {
                "%if" => {
                    if depth == 0 {
                        opened_at = n;
                    }
                    depth += 1;
                }
                "%endif" => depth = depth.saturating_sub(1),
                _ => {}
            }
            joined.clear();
            continue;
        }
        if depth > 0 {
            continue;
        }
        if joined.is_empty() {
            start = n;
        }
        if let Some(head) = raw.strip_suffix('\\') {
            joined.push_str(head);
            continue;
        }
        joined.push_str(raw);
        out.push((start, std::mem::take(&mut joined)));
    }
    if !joined.is_empty() {
        out.push((start, joined));
    }
    (out, (depth > 0).then_some(opened_at))
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
/// The columns of the task board (`jobs`, `choose-jobs`).
fn jobs_header() -> Vec<String> {
    ["PANE", "STATE", "UP", "IDLE", "PID", "COMMAND", "DIR"].iter().map(|s| s.to_string()).collect()
}

/// Rows as lines with the columns lined up on screen: padded by display
/// width, since a CJK session name is two cells a character.
fn align_columns(rows: &[Vec<String>]) -> Vec<String> {
    use unicode_width::UnicodeWidthStr;
    let ncol = rows.first().map(|r| r.len()).unwrap_or(0);
    let widths: Vec<usize> =
        (0..ncol).map(|c| rows.iter().map(|r| r.get(c).map(|s| s.width()).unwrap_or(0)).max().unwrap_or(0)).collect();
    rows.iter()
        .map(|r| {
            let mut line = String::new();
            for (c, cell) in r.iter().enumerate() {
                line.push_str(cell);
                if c + 1 < ncol {
                    line.extend(std::iter::repeat_n(' ', widths[c].saturating_sub(cell.width()) + 2));
                }
            }
            line.trim_end().to_string()
        })
        .collect()
}

const STATUS_SHELL_TIMEOUT: Duration = Duration::from_secs(30);
/// A `#(command)` run for a one-shot `display-message -p` or `jobs -F`
/// blocks the server while it runs, so it gets less rope than the status
/// line's background ones.
const ONE_SHOT_SHELL_TIMEOUT: Duration = Duration::from_secs(3);

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
            search: None,
            search_back: true,
            count: None,
            rect: false,
        });
    }
}

/// Move the copy cursor to an absolute line, scrolling if it is off screen.
fn copy_goto(p: &mut Pane, abs: usize) {
    let max = p.scrollback_len();
    let rows = p.rows as usize;
    let Some(c) = p.copy.as_ref() else { return };
    let top = max - c.offset; // absolute line at the top of the view
    let (offset, cy) = if abs < top {
        (max - abs, 0)
    } else if abs >= top + rows {
        ((max + rows - 1).saturating_sub(abs), rows - 1)
    } else {
        (c.offset, abs - top)
    };
    if let Some(c) = p.copy.as_mut() {
        c.offset = offset.min(max);
        c.cy = cy.min(rows.saturating_sub(1)) as u16;
    }
}

/// vi's `w`, `b` and `e` (and their WORD variants) on the copy cursor.
fn copy_word_motion(p: &mut Pane, key: char) {
    let cols = p.cols as usize;
    let big = key.is_uppercase();
    let class = |ch: char| -> u8 {
        if ch.is_whitespace() {
            0
        } else if big || ch.is_alphanumeric() || ch == '_' {
            1
        } else {
            2
        }
    };
    let abs = copy_abs(p);
    let last = p.scrollback_len() + p.rows as usize - 1;
    let mut line: Vec<char> = p.line_text(abs).0.chars().collect();
    line.resize(cols.max(1), ' ');
    let mut x = p.copy.as_ref().map(|c| c.cx as usize).unwrap_or(0).min(cols.saturating_sub(1));
    let mut cur = abs;
    let forward = key != 'b' && key != 'B';
    let mut steps = 0;
    loop {
        steps += 1;
        if steps > 4096 {
            break;
        }
        let moved = if forward {
            if x + 1 < line.len() {
                x += 1;
                true
            } else if cur < last {
                cur += 1;
                line = p.line_text(cur).0.chars().collect();
                line.resize(cols.max(1), ' ');
                x = 0;
                true
            } else {
                false
            }
        } else if x > 0 {
            x -= 1;
            true
        } else if cur > 0 {
            cur -= 1;
            line = p.line_text(cur).0.chars().collect();
            line.resize(cols.max(1), ' ');
            x = line.len().saturating_sub(1);
            true
        } else {
            false
        };
        if !moved {
            break;
        }
        let here = class(line[x]);
        if here == 0 {
            continue;
        }
        // `w`/`b` stop at the start of a word, `e` at its end.
        let neighbour = match key {
            'e' | 'E' => line.get(x + 1).copied().map(class).unwrap_or(0),
            _ if x == 0 => 0,
            _ => class(line[x - 1]),
        };
        if neighbour != here {
            break;
        }
    }
    if cur != abs {
        copy_goto(p, cur);
    }
    if let Some(c) = p.copy.as_mut() {
        c.cx = x.min(cols.saturating_sub(1)) as u16;
    }
}

/// A `-x`/`-y` size: cells, or a percentage of the window.
/// Where a `display-popup` box goes: the asked-for size (`80`, `50%`),
/// defaulting to four fifths of the window, centred in it.
fn popup_rect(area: Rect, width: Option<&str>, height: Option<&str>) -> Rect {
    let size = |spec: Option<&str>, full: u16| -> u16 {
        spec.and_then(|s| parse_size(s, full)).unwrap_or((full as u32 * 4 / 5) as u16).clamp(1, full)
    };
    let w = size(width, area.w);
    let h = size(height, area.h);
    Rect { x: area.x + (area.w - w) / 2, y: area.y + (area.h - h) / 2, w, h }
}

fn parse_size(spec: &str, full: u16) -> Option<u16> {
    let spec = spec.trim();
    if let Some(p) = spec.strip_suffix('%') {
        let pct: u32 = p.trim().parse().ok()?;
        return Some(((full as u32 * pct.min(100)) / 100).max(1) as u16);
    }
    spec.parse::<u16>().ok().map(|v| v.max(1))
}

/// One line of a buffer for a listing, clipped to `max` characters.
fn one_line(s: &str, max: usize) -> String {
    let flat: String = s.chars().map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c }).collect();
    let flat = flat.trim().to_string();
    if flat.chars().count() <= max { flat } else { format!("{}…", flat.chars().take(max).collect::<String>()) }
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
/// Returns the number of characters copied and the text, which the caller
/// also stores as a paste buffer.
fn copy_selection(p: &mut Pane) -> Result<(usize, String), String> {
    let cur_abs = copy_abs(p);
    let c = p.copy.as_ref().unwrap();
    let Some((abs_a, col_a)) = c.anchor else { return Err("no selection".into()) };
    let mut a = (abs_a, col_a);
    let mut b = (cur_abs, c.cx);
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }
    let rect = p.copy.as_ref().is_some_and(|c| c.rect);
    let (rx0, rx1) = (a.1.min(b.1) as usize, a.1.max(b.1) as usize);
    let mut text = String::new();
    for abs in a.0..=b.0 {
        let (line, wrapped) = p.line_text(abs);
        let chars: Vec<char> = line.chars().collect();
        // A rectangle takes the same columns from every line.
        let start = if rect {
            rx0
        } else if abs == a.0 {
            a.1 as usize
        } else {
            0
        };
        let end = if rect {
            (rx1 + 1).min(chars.len())
        } else if abs == b.0 {
            (b.1 as usize + 1).min(chars.len())
        } else {
            chars.len()
        };
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
    Ok((n, text))
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
            last_session: None,
            last_grid: Some(Grid::new(10, 2)),
            last_cursor: Some((0, 0)),
            prefix: false,
            repeat_until: None,
            panes_until: None,
            prompt: None,
            message: None,
            overlay: None,
            chooser: None,
            popup: None,
            mouse_buttons: 0,
            drag: None,
            swallow_up: HashSet::new(),
            pending_bell: false,
            pending: std::collections::VecDeque::new(),
            window_hits: Vec::new(),
        };
        (c, rx)
    }

    /// Every default binding must survive the trip through `list-keys`:
    /// printed as a command line and parsed back into the same command.
    #[test]
    fn default_bindings_round_trip() {
        for (key, b) in default_bindings() {
            let printed = b.cmd.to_string();
            let words = crate::command::tokenize(&printed).unwrap_or_else(|e| panic!("{key}: {printed}: {e}"));
            let back = crate::command::parse(&words).unwrap_or_else(|e| panic!("{key}: {printed}: {e}"));
            assert_eq!(back.to_string(), printed, "{key} does not round-trip");
        }
    }

    /// Every menu the default bindings open must be usable: unique keys, a
    /// command behind each entry, and separators that cannot be selected.
    #[test]
    fn default_menus_have_unique_keys() {
        let mut menus = 0;
        for (key, b) in default_bindings() {
            let Cmd::DisplayMenu { items, .. } = &b.cmd else { continue };
            menus += 1;
            let keys: Vec<&str> = items.iter().filter_map(|i| i.cmd.as_ref().and(i.key.as_deref())).collect();
            let mut seen = std::collections::HashSet::new();
            for k in &keys {
                assert!(crate::keys::Key::parse(k).is_some(), "{key}: menu key {k} is not a key");
                assert!(seen.insert(*k), "{key}: menu key {k} is used twice");
            }
            assert!(keys.len() >= 3, "{key}: a menu worth opening");
            assert!(items.iter().any(|i| i.cmd.is_none()), "{key}: menus are grouped by separators");
        }
        assert_eq!(menus, 2, "the pane menu and the window menu");
    }

    #[test]
    fn menu_selection_skips_the_separators() {
        let items = vec![
            ChooserItem::Separator, // title
            ChooserItem::Menu(0),
            ChooserItem::Separator,
            ChooserItem::Menu(1),
        ];
        let mut ch = Chooser {
            kind: ChooserKind::Menu(Vec::new()),
            lines: vec!["t".into(), "a".into(), "-".into(), "b".into()],
            items,
            sel: 0,
            top: 0,
        };
        ch.settle(1);
        assert_eq!(ch.sel, 1, "opening a menu lands on the first real entry");
        ch.step(1);
        assert_eq!(ch.sel, 3, "j jumps over the separator");
        ch.step(1);
        assert_eq!(ch.sel, 3, "and stops at the end");
        ch.sel = ch.items.len() - 1;
        ch.settle(-1);
        assert_eq!(ch.sel, 3, "G lands on a real entry");
        ch.step(-1);
        assert_eq!(ch.sel, 1, "k jumps back over it");
        ch.step(-1);
        assert_eq!(ch.sel, 1, "and never onto the title");

        // A list of nothing but separators leaves the selection alone.
        let mut empty = Chooser {
            kind: ChooserKind::Menu(Vec::new()),
            items: vec![ChooserItem::Separator; 3],
            lines: vec!["-".into(); 3],
            sel: 1,
            top: 0,
        };
        empty.step(1);
        assert_eq!(empty.sel, 1);
    }

    #[test]
    fn popups_are_centred_and_sized_as_asked() {
        let area = Rect { x: 0, y: 0, w: 80, h: 23 };
        // Default: four fifths of the window, centred.
        let r = popup_rect(area, None, None);
        assert_eq!((r.w, r.h), (64, 18));
        assert_eq!((r.x, r.y), ((80 - 64) / 2, (23 - 18) / 2));
        // Cells and percentages, and never bigger than the window.
        assert_eq!(popup_rect(area, Some("40"), Some("10")).w, 40);
        assert_eq!(popup_rect(area, Some("50%"), Some("50%")).h, 11);
        let big = popup_rect(area, Some("500"), Some("500"));
        assert_eq!((big.w, big.h, big.x, big.y), (80, 23, 0, 0));
        // Nonsense falls back to the default instead of vanishing.
        assert_eq!(popup_rect(area, Some("wide"), None).w, 64);
        // A window with no room gives a box too small to draw, which the
        // caller refuses rather than drawing a broken border.
        let tiny = popup_rect(Rect { x: 0, y: 0, w: 2, h: 2 }, None, None);
        assert!(tiny.w < 3 || tiny.h < 3, "{tiny:?}");
    }

    /// A config file the way tmux reads it: continuations joined, `%if`
    /// blocks left out whole, line numbers pointing at where a line began.
    #[test]
    fn logical_lines_follow_tmux_rules() {
        let text = "set -g a 1\n\
                    bind x \\\n  send-keys \\\n  hi\n\
                    %if #{==:#{host},box}\n\
                    set -g never 1\n\
                    %else\n\
                    set -g also-never 1\n\
                    %endif\n\
                    %hidden foo=1\n\
                    set -g b 2\n\
                    last \\";
        let (got, open) = logical_lines(text);
        assert_eq!(
            got,
            vec![
                (1, "set -g a 1".to_string()),
                (2, "bind x   send-keys   hi".to_string()),
                (11, "set -g b 2".to_string()),
                (12, "last ".to_string()),
            ]
        );
        assert_eq!(open, None);
        // Nested blocks come out whole too.
        let nested = "%if a\n%if b\nx\n%endif\ny\n%endif\nz";
        assert_eq!(logical_lines(nested).0, vec![(7, "z".to_string())]);
        assert!(logical_lines("").0.is_empty());
        // Notepad's BOM and CRLF endings, as a file written on Windows has.
        let notepad = "\u{feff}set -g a 1\r\nset -g b \\\r\n 2\r\n";
        assert_eq!(logical_lines(notepad).0, vec![(1, "set -g a 1".to_string()), (2, "set -g b  2".to_string())]);
        // A %if that never closes swallows the rest, and says where it began.
        let (lines, open) = logical_lines("set -g a 1\n%if x\nset -g b 2\nset -g c 3\n");
        assert_eq!(lines, vec![(1, "set -g a 1".to_string())]);
        assert_eq!(open, Some(2));
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
