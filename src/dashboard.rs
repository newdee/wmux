//! `keepane dashboard`: every pane at a glance — its work mode, whether it
//! is free, its inbox, what it says it is doing — and, below, the chosen
//! pane's events, messages, the tasks, or its screen live
//! (docs/design/mailbox.md §10.3). Opened by prefix v in a popup, or run
//! in any terminal.
//!
//! It watches and does not act, with one exception kept behind a mode of
//! its own: in manage mode (E) queued messages can be deleted and moved.
//! Everything it asks the server is in `QUERIES`; everything it may change
//! is in `MANAGES`. The board is kept apart from the console, so it can be
//! tested without one.

use crate::keys::{Key, KeyCode};
use std::time::{Duration, Instant};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// The only server commands the dashboard sends to look.
pub const QUERIES: &[&str] =
    &["list-panes", "list-events", "list-messages", "list-tasks", "capture-pane", "trace-message"];
/// The only ones it sends to change anything, and only in manage mode.
pub const MANAGES: &[&str] = &["drop-message", "move-message"];

/// Manage mode ends by itself after this long without a key.
const MANAGE_IDLE: Duration = Duration::from_secs(30);

/// `list-panes -F` for the board: one pane a line, tab-separated.
pub const PANE_FORMAT: &str = "#{pane_address}\t#{session_name}\t#{window_index}.#{pane_index}\t#{pane_id}\t#{pane_name}\t#{pane_work_mode}\t#{pane_idle}\t#{pane_inbox}\t#{pane_status}\t#{pane_current_command}\t#{pane_message}\t#{pane_dead}";

#[derive(Clone, Debug, PartialEq, Default)]
pub struct PaneRow {
    pub address: String,
    pub session: String,
    pub place: String,
    pub id: String,
    pub name: String,
    pub mode: String,
    pub idle: bool,
    pub inbox: usize,
    pub status: String,
    pub command: String,
    pub message: String,
    pub dead: bool,
}

impl PaneRow {
    pub fn parse(line: &str) -> Option<PaneRow> {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 12 {
            return None;
        }
        Some(PaneRow {
            address: f[0].into(),
            session: f[1].into(),
            place: f[2].into(),
            id: f[3].into(),
            name: f[4].into(),
            mode: f[5].into(),
            idle: f[6] == "1",
            inbox: f[7].parse().unwrap_or(0),
            status: f[8].into(),
            command: f[9].into(),
            message: f[10].into(),
            dead: f[11] == "1",
        })
    }

    fn state(&self) -> &'static str {
        if self.dead {
            "exited"
        } else if self.mode == "normal" {
            "-"
        } else if self.idle {
            "idle"
        } else {
            "busy"
        }
    }

    /// What it is doing: what it said, else the message it works on, else
    /// its program.
    fn doing(&self) -> String {
        if !self.status.is_empty() {
            format!("\"{}\"", self.status)
        } else if !self.message.is_empty() {
            format!("working on #{}", self.message)
        } else {
            self.command.clone()
        }
    }

    fn matches(&self, filter: &str) -> bool {
        let f = filter.to_lowercase();
        [&self.address, &self.session, &self.name, &self.mode, &self.status, &self.command]
            .iter()
            .any(|x| x.to_lowercase().contains(&f))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// The chosen pane's events.
    Events,
    /// Its inbox, what it works on, what it sent and got lately.
    Messages,
    /// Every task.
    Tasks,
    /// Its screen, live.
    Live,
    /// Its screen and scrollback.
    History,
    /// One message in full: envelope, text, what became of it.
    Trace,
}

impl View {
    fn word(self) -> &'static str {
        match self {
            View::Events => "events",
            View::Messages => "messages",
            View::Tasks => "tasks",
            View::Live => "live",
            View::History => "history",
            View::Trace => "message",
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Action {
    Redraw,
    Quit,
    /// A server command to run (a query after a key, or a change in manage mode).
    Run(Vec<String>),
}

pub struct Board {
    pub rows: Vec<PaneRow>,
    /// The chosen pane, by id, so it stays chosen when others come and go.
    pub chosen: Option<String>,
    pub view: View,
    /// Manage mode and the time of its last key.
    pub manage: Option<Instant>,
    /// The lines below the panes, as the last query for the view gave them.
    pub below: Vec<String>,
    /// The queued messages of the chosen pane, in order, and the one picked.
    pub queued: Vec<String>,
    pub pick: usize,
    /// Messages of every pane's inbox, not just the chosen one's (`a`).
    pub all_inboxes: bool,
    /// The message shown in full (`View::Trace`).
    pub traced: Option<String>,
    /// Lines the view below is scrolled up by.
    pub scroll: usize,
    pub filter: String,
    typing: Option<String>,
    pub note: Option<String>,
    pub cols: u16,
    pub height: u16,
}

fn clip(s: &str, width: usize) -> String {
    let mut used = 0;
    let mut out = String::new();
    for c in s.chars() {
        let w = if c.is_control() { 1 } else { c.width().unwrap_or(0) };
        if used + w > width {
            break;
        }
        out.push(if c.is_control() { ' ' } else { c });
        used += w;
    }
    out
}

fn pad(s: &str, width: usize) -> String {
    let s = clip(s, width);
    let n = width.saturating_sub(s.width());
    format!("{s}{}", " ".repeat(n))
}

impl Board {
    pub fn new(cols: u16, height: u16) -> Board {
        Board {
            rows: Vec::new(),
            chosen: None,
            view: View::Events,
            manage: None,
            below: Vec::new(),
            queued: Vec::new(),
            pick: 0,
            all_inboxes: false,
            traced: None,
            scroll: 0,
            filter: String::new(),
            typing: None,
            note: None,
            cols,
            height,
        }
    }

    pub fn resize(&mut self, cols: u16, height: u16) {
        self.cols = cols;
        self.height = height;
    }

    /// The panes shown, after the filter.
    fn shown(&self) -> Vec<&PaneRow> {
        self.rows.iter().filter(|r| self.filter.is_empty() || r.matches(&self.filter)).collect()
    }

    pub fn chosen_row(&self) -> Option<&PaneRow> {
        let shown = self.shown();
        self.chosen.as_ref().and_then(|id| shown.iter().find(|r| &r.id == id).copied()).or(shown.first().copied())
    }

    /// New pane rows from the server; the chosen pane stays chosen.
    pub fn set_rows(&mut self, rows: Vec<PaneRow>) {
        self.rows = rows;
        if self.chosen_row().map(|r| r.id.clone()) != self.chosen {
            self.chosen = self.chosen_row().map(|r| r.id.clone());
        }
    }

    /// What the view below shows, and for messages, which are queued.
    pub fn set_below(&mut self, lines: Vec<String>) {
        if self.view == View::Messages {
            self.queued = lines
                .iter()
                .filter_map(|l| l.strip_prefix("  #"))
                .filter_map(|l| l.split_whitespace().next())
                .map(String::from)
                .collect();
            self.pick = self.pick.min(self.queued.len().saturating_sub(1));
        }
        self.below = lines;
    }

    /// The query that fills the view below, for the chosen pane.
    pub fn query(&self) -> Vec<String> {
        let id = self.chosen_row().map(|r| r.id.clone()).unwrap_or_default();
        let v = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        match self.view {
            View::Tasks => v(&["list-tasks"]),
            View::Messages if self.all_inboxes => v(&["list-messages", "-a"]),
            View::Trace => match &self.traced {
                Some(m) => v(&["trace-message", m]),
                None => Vec::new(),
            },
            _ if id.is_empty() => Vec::new(),
            View::Events => v(&["list-events", "-t", &id, "-n", "200"]),
            View::Messages => v(&["list-messages", "-t", &id]),
            View::Live => v(&["capture-pane", "-p", "-t", &id]),
            View::History => v(&["capture-pane", "-p", "-S", "-2000", "-t", &id]),
        }
    }

    pub fn managing(&self) -> bool {
        self.manage.is_some_and(|t| t.elapsed() < MANAGE_IDLE)
    }

    /// From the refresh: manage mode left alone ends.
    pub fn tick(&mut self) {
        if self.manage.is_some() && !self.managing() {
            self.manage = None;
            self.note = Some("manage mode ended (30s without a key)".into());
        }
    }

    fn step(&mut self, by: isize) {
        let shown = self.shown();
        if shown.is_empty() {
            return;
        }
        let at = self.chosen_row().and_then(|c| shown.iter().position(|r| r.id == c.id)).unwrap_or(0);
        let to = (at as isize + by).clamp(0, shown.len() as isize - 1) as usize;
        self.chosen = Some(shown[to].id.clone());
        self.scroll = 0;
        self.pick = 0;
    }

    fn show(&mut self, v: View) {
        self.view = if self.view == v && v != View::Events { View::Events } else { v };
        self.scroll = 0;
        self.pick = 0;
    }

    pub fn key(&mut self, k: Key) -> Action {
        self.note = None;
        if let Some(text) = self.typing.as_mut() {
            match k.code {
                KeyCode::Escape => {
                    self.typing = None;
                    self.filter.clear();
                }
                KeyCode::Enter => self.typing = None,
                KeyCode::BSpace => {
                    text.pop();
                    self.filter = text.clone();
                }
                KeyCode::Char(c) if !k.ctrl && !k.alt => {
                    text.push(c);
                    self.filter = text.clone();
                }
                _ => {}
            }
            return Action::Redraw;
        }
        let managing = self.managing();
        if managing {
            self.manage = Some(Instant::now());
        }
        let picked = self.queued.get(self.pick).cloned();
        let manage_key = matches!(k.code, KeyCode::Char('d' | 'K' | 'J' | 'g' | 'u')) && !k.ctrl && !k.alt;
        if manage_key {
            if !managing {
                self.note = Some("press E for manage mode to change queued messages".into());
                return Action::Redraw;
            }
            if self.view != View::Messages {
                self.note = Some("m shows the messages to manage".into());
                return Action::Redraw;
            }
            let run = |xs: &[&str]| Action::Run(xs.iter().map(|x| x.to_string()).collect());
            return match (k.code, picked) {
                (KeyCode::Char('u'), _) => run(&["drop-message", "-u"]),
                (KeyCode::Char('d'), Some(id)) => run(&["drop-message", &id]),
                (KeyCode::Char('K'), Some(id)) => run(&["move-message", &id, "up"]),
                (KeyCode::Char('J'), Some(id)) => run(&["move-message", &id, "down"]),
                (KeyCode::Char('g'), Some(id)) => run(&["move-message", &id, "top"]),
                _ => {
                    self.note = Some("no queued message".into());
                    Action::Redraw
                }
            };
        }
        match (k.code, k.ctrl) {
            (KeyCode::Char('q'), false) | (KeyCode::Char('c'), true) => return Action::Quit,
            (KeyCode::Escape, _) if managing => self.manage = None,
            (KeyCode::Escape, _) => return Action::Quit,
            (KeyCode::Char('E'), false) => {
                self.manage = if managing { None } else { Some(Instant::now()) };
            }
            (KeyCode::Down | KeyCode::Char('j'), false) => self.step(1),
            (KeyCode::Up | KeyCode::Char('k'), false) => self.step(-1),
            (KeyCode::Char('n'), false) => self.pick = (self.pick + 1).min(self.queued.len().saturating_sub(1)),
            (KeyCode::Char('p'), false) => self.pick = self.pick.saturating_sub(1),
            (KeyCode::PPage, _) => self.scroll += 10,
            (KeyCode::NPage, _) => self.scroll = self.scroll.saturating_sub(10),
            // In the messages, Enter opens the one picked; from it, back.
            (KeyCode::Enter, _) if self.view == View::Messages && picked.is_some() => {
                self.traced = picked;
                self.view = View::Trace;
                self.scroll = 0;
            }
            (KeyCode::Enter, _) if self.view == View::Trace => self.view = View::Messages,
            (KeyCode::Enter, _) => self.show(View::Events),
            (KeyCode::Char('a'), false) if self.view == View::Messages => {
                self.all_inboxes = !self.all_inboxes;
                self.pick = 0;
            }
            (KeyCode::Char('m'), false) => self.show(View::Messages),
            (KeyCode::Char('t'), false) => self.show(View::Tasks),
            (KeyCode::Char('v'), false) => self.show(View::Live),
            (KeyCode::Char('h'), false) => self.show(View::History),
            (KeyCode::Char('/'), false) => self.typing = Some(self.filter.clone()),
            _ => {}
        }
        Action::Redraw
    }

    /// Everything on screen, as the bytes that draw it.
    pub fn frame(&self, now: &str) -> String {
        let (w, h) = (usize::from(self.cols), usize::from(self.height).max(6));
        let mut lines: Vec<String> = Vec::new();
        let shown = self.shown();
        let busy = shown.iter().filter(|r| r.mode != "normal" && !r.idle && !r.dead).count();
        let queued: usize = shown.iter().map(|r| r.inbox).sum();
        let managing = self.managing();
        let head = format!(
            " keepane dashboard{}   {} panes · {busy} busy · {queued} queued   {now}",
            if managing { "   MANAGE" } else { "" },
            shown.len()
        );
        let colour = if managing { "\x1b[1;37;41m" } else { "\x1b[7m" };
        lines.push(format!("{colour}{}\x1b[0m", pad(&head, w)));
        lines.push(format!("\x1b[2m{}\x1b[0m", pad("   PANE      NAME          MODE    STATE   INBOX  DOING", w)));
        // The panes, grouped by session; the chosen one kept in view.
        let top_rows = ((h - 4) / 2).max(3);
        let mut table: Vec<(bool, String)> = Vec::new();
        let chosen = self.chosen_row().map(|r| r.id.clone());
        let mut session = None;
        for r in &shown {
            if session != Some(&r.session) {
                session = Some(&r.session);
                table.push((false, format!("\x1b[1m {}\x1b[0m", r.session)));
            }
            let me = chosen.as_ref() == Some(&r.id);
            let cells = format!(
                "{} {:<9} {:<13} {:<7} {:<7} {:>5}  {}",
                if me { ">" } else { " " },
                clip(&format!("{} {}", r.place, r.id), 9),
                clip(&r.name, 13),
                clip(&r.mode, 7),
                r.state(),
                r.inbox,
                r.doing()
            );
            let line = pad(&format!(" {cells}"), w);
            table.push((me, if me { format!("\x1b[7m{line}\x1b[0m") } else { line }));
        }
        let at = table.iter().position(|(me, _)| *me).unwrap_or(0);
        let start = at.saturating_sub(top_rows.saturating_sub(1));
        for i in 0..top_rows {
            lines.push(table.get(start + i).map(|(_, l)| l.clone()).unwrap_or_default());
        }
        // Below: the chosen pane and the view.
        let about = match self.chosen_row() {
            Some(r) => format!(
                "── {} {}{} · {} · {} · {} ",
                r.id,
                r.name,
                if r.name.is_empty() { "" } else { " " },
                r.mode,
                r.command,
                r.address
            ),
            None => "── no panes ".into(),
        };
        let about = format!("{about}── {} ", self.view.word());
        let fill = w.saturating_sub(about.width());
        lines.push(format!("\x1b[2m{}{}\x1b[0m", clip(&about, w), "─".repeat(fill)));
        let room = h.saturating_sub(lines.len() + 1);
        let body: Vec<String> = match self.view {
            View::Events => self.below.iter().map(|l| event_line(l)).collect(),
            _ => self.below.clone(),
        };
        let end = body.len().saturating_sub(self.scroll);
        let from = end.saturating_sub(room);
        let picked = self.queued.get(self.pick);
        for l in &body[from..end] {
            let mark = self.view == View::Messages
                && picked.is_some_and(|id| l.strip_prefix("  #").is_some_and(|x| x.starts_with(&format!("{id} "))));
            let text = pad(l, w);
            lines.push(if mark { format!("\x1b[1;33m{text}\x1b[0m") } else { text });
        }
        while lines.len() < h - 1 {
            lines.push(String::new());
        }
        let foot = match (&self.typing, &self.note) {
            (Some(t), _) => format!("/{t}"),
            (None, Some(n)) => format!(" {n}"),
            (None, None) if managing => {
                " MANAGE  n/p pick  d delete  K/J move  g to top  u undo delete  E/Esc done".to_string()
            }
            (None, None) if self.view == View::Messages => {
                " j/k pane  n/p pick  Enter open it  a every inbox  E manage  t tasks  v live  q quit".to_string()
            }
            (None, None) => {
                " j/k pane  Enter events  m messages  t tasks  v live  h history  / filter  E manage  q quit"
                    .to_string()
            }
        };
        lines.push(format!("\x1b[7m{}\x1b[0m", pad(&foot, w)));
        let mut s = String::from("\x1b[?25l\x1b[H");
        for (i, l) in lines.iter().take(h).enumerate() {
            s.push_str("\x1b[2K");
            s.push_str(l);
            if i + 1 < h {
                s.push_str("\r\n");
            }
        }
        s
    }
}

/// An event log line, short: time, what, the gist.
pub fn event_line(json: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return json.to_string() };
    let at = v["at"]
        .as_str()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
        .unwrap_or_default();
    let ev = v["ev"].as_str().unwrap_or_default();
    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    let gist = match ev {
        "sent" | "rejected" => {
            let m = &v["msg"];
            let from = m["name"]
                .as_str()
                .map(|n| format!("%{n}"))
                .unwrap_or_else(|| m["from"].as_str().unwrap_or("").to_string());
            let text = s("text");
            let first = text.lines().next().unwrap_or_default();
            let why = if ev == "rejected" { format!("  ({})", s("why")) } else { String::new() };
            format!(
                "#{} {from} → {} ({}): {first}{why}",
                m["id"],
                m["to"].as_str().unwrap_or(""),
                m["via"].as_str().unwrap_or("")
            )
        }
        "pane" => {
            let what = s("what");
            let value = [s("value"), s("text")].into_iter().find(|x| !x.is_empty()).unwrap_or_default();
            let by = s("by");
            format!("{what} {value}{}", if by.is_empty() { String::new() } else { format!("  by {by}") })
        }
        _ => {
            let mut g = format!("#{}", v["id"]);
            if let Some(ok) = v["ok"].as_bool() {
                g.push_str(if ok { " ok" } else { " failed" });
            }
            for k in ["why", "by"] {
                if !s(k).is_empty() {
                    g.push_str(&format!("  {}", s(k)));
                }
            }
            g
        }
    };
    format!("{at}  {ev:<9} {gist}")
}

/// `keepane dashboard`.
pub fn run(socket: &str, rt: &tokio::runtime::Runtime) -> anyhow::Result<i32> {
    use crate::console::{Console, InputEvent};
    let mut console = Console::open()?;
    console.enter_raw()?;
    let console = std::sync::Arc::new(console);
    let (tx, rx) = std::sync::mpsc::channel::<InputEvent>();
    {
        let c = console.clone();
        std::thread::spawn(move || {
            while let Ok(events) = c.read_events() {
                for e in events {
                    if tx.send(e).is_err() {
                        return;
                    }
                }
            }
        });
    }
    let ask = |argv: &[String]| -> (i32, String, String) {
        debug_assert!(argv.first().is_some_and(|c| QUERIES.contains(&c.as_str()) || MANAGES.contains(&c.as_str())));
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        rt.block_on(crate::client::query(socket, &argv)).unwrap_or_else(|e| (1, String::new(), format!("{e:#}")))
    };
    let (cols, rows) = console.size();
    let mut board = Board::new(cols, rows);
    let result = (|| -> anyhow::Result<()> {
        loop {
            let (code, out, err) = ask(&["list-panes".into(), "-a".into(), "-F".into(), PANE_FORMAT.into()]);
            if code != 0 {
                anyhow::bail!("{}", err.trim());
            }
            board.set_rows(out.lines().filter_map(PaneRow::parse).collect());
            let q = board.query();
            if !q.is_empty() {
                let (_, out, err) = ask(&q);
                let text = if out.is_empty() { err } else { out };
                board.set_below(text.lines().map(String::from).collect());
            } else {
                board.set_below(Vec::new());
            }
            board.tick();
            console.write_str(&board.frame(&chrono::Local::now().format("%H:%M:%S").to_string()));
            // A key, or a second without one: then the board is read again.
            let first = match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(e) => Some(e),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                Err(_) => return Ok(()),
            };
            for ev in first.into_iter().chain(rx.try_iter()) {
                match ev {
                    InputEvent::Key(k) => {
                        let Some(key) = crate::keys::key_from_record(&k) else { continue };
                        match board.key(key) {
                            Action::Quit => return Ok(()),
                            Action::Run(argv) => {
                                let (code, out, err) = ask(&argv);
                                board.note = Some(if code == 0 {
                                    let out = out.trim();
                                    if out.is_empty() { format!("done: {}", argv.join(" ")) } else { out.to_string() }
                                } else {
                                    err.trim().to_string()
                                });
                            }
                            Action::Redraw => {}
                        }
                    }
                    InputEvent::Resize => {
                        let (cols, rows) = console.size();
                        board.resize(cols, rows);
                        console.write_str("\x1b[2J");
                    }
                    InputEvent::Mouse(_) => {}
                }
            }
        }
    })();
    console.restore();
    result.map(|()| 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, session: &str, name: &str, mode: &str, idle: bool, inbox: usize) -> PaneRow {
        PaneRow {
            address: format!("$1:@1.{id}"),
            session: session.into(),
            place: "0.0".into(),
            id: id.into(),
            name: name.into(),
            mode: mode.into(),
            idle,
            inbox,
            command: "pwsh".into(),
            ..Default::default()
        }
    }

    fn board() -> Board {
        let mut b = Board::new(100, 30);
        b.set_rows(vec![
            row("%1", "work", "lead", "ai", false, 0),
            row("%2", "work", "tester", "ai", true, 2),
            row("%3", "ops", "", "normal", false, 0),
        ]);
        b
    }

    fn every_key() -> Vec<Key> {
        let mut keys: Vec<Key> = (' '..='~').map(Key::ch).collect();
        for code in [
            KeyCode::Enter,
            KeyCode::Escape,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::PPage,
            KeyCode::NPage,
            KeyCode::BSpace,
        ] {
            keys.push(Key::plain(code));
        }
        keys
    }

    #[test]
    fn a_pane_line_reads_back() {
        let line = "$1:@2.%7\twork\t1.0\t%7\ttester\tai\t1\t2\trunning 3/10\tclaude\t12\t0";
        let r = PaneRow::parse(line).unwrap();
        assert_eq!((r.id.as_str(), r.idle, r.inbox, r.doing().as_str()), ("%7", true, 2, "\"running 3/10\""));
        assert_eq!(PaneRow::parse("too\tshort"), None);
    }

    #[test]
    fn watching_changes_nothing_whatever_is_pressed() {
        // Every key, in every view, outside manage mode: nothing but queries.
        for view in [View::Events, View::Messages, View::Tasks, View::Live, View::History] {
            let mut b = board();
            b.view = view;
            b.set_below(vec!["  #5  from user  waiting 1s  hi".into()]);
            for k in every_key() {
                if k.code == KeyCode::Char('E') {
                    continue;
                }
                let a = b.key(k);
                assert!(!matches!(a, Action::Run(_)), "{view:?} {k:?} -> {a:?}");
                b.view = view;
                b.typing = None;
                b.filter.clear();
            }
        }
    }

    #[test]
    fn manage_mode_changes_only_queued_messages_and_ends_by_itself() {
        let mut b = board();
        b.key(Key::ch('j'));
        b.key(Key::ch('m'));
        assert_eq!(b.query(), ["list-messages", "-t", "%2"]);
        b.set_below(vec![
            "$1:@1.%2 tester (ai, idle) · 2 queued".into(),
            "  #5  from user  waiting 1s  first".into(),
            "  #6  from user  waiting 1s  second".into(),
        ]);
        assert!(matches!(b.key(Key::ch('d')), Action::Redraw), "not before E");
        assert!(b.note.as_deref().unwrap().contains("press E"));
        b.key(Key::ch('E'));
        assert!(b.managing() && b.frame("12:00").contains("MANAGE"));
        b.key(Key::ch('n'));
        assert_eq!(b.key(Key::ch('g')), Action::Run(vec!["move-message".into(), "6".into(), "top".into()]));
        assert_eq!(b.key(Key::ch('d')), Action::Run(vec!["drop-message".into(), "6".into()]));
        assert_eq!(b.key(Key::ch('u')), Action::Run(vec!["drop-message".into(), "-u".into()]));
        for k in every_key() {
            if let Action::Run(argv) = b.key(k) {
                assert!(MANAGES.contains(&argv[0].as_str()), "{argv:?}");
            }
            b.manage = Some(Instant::now());
            b.view = View::Messages;
            b.typing = None;
        }
        // Left alone, it ends.
        b.manage = Some(Instant::now() - MANAGE_IDLE - Duration::from_secs(1));
        b.tick();
        assert!(!b.managing() && b.note.as_deref().unwrap().contains("ended"));
        assert!(matches!(b.key(Key::ch('d')), Action::Redraw));
    }

    #[test]
    fn the_board_asks_only_queries_and_shows_the_panes() {
        let mut b = board();
        for (k, want) in [('m', "list-messages"), ('t', "list-tasks"), ('v', "capture-pane"), ('h', "capture-pane")] {
            b.key(Key::ch(k));
            assert_eq!(b.query()[0], want);
            assert!(QUERIES.contains(&b.query()[0].as_str()));
        }
        b.key(Key::plain(KeyCode::Enter));
        assert_eq!(b.query()[0], "list-events");
        let f = b.frame("12:00:00");
        assert!(f.contains("3 panes · 1 busy · 2 queued"), "{f}");
        assert!(f.contains("tester") && f.contains(" idle ") && f.contains("ops"), "{f}");
        // The filter narrows the panes; the chosen one follows.
        b.key(Key::ch('/'));
        for c in "ops".chars() {
            b.key(Key::ch(c));
        }
        b.key(Key::plain(KeyCode::Enter));
        assert_eq!(b.chosen_row().unwrap().id, "%3");
        assert!(b.frame("x").contains("1 panes"));
    }

    #[test]
    fn a_message_opens_in_full_and_every_inbox_shows_at_once() {
        let mut b = board();
        b.key(Key::ch('j'));
        b.key(Key::ch('m'));
        b.set_below(vec![
            "hdr".into(),
            "  #5  from user  waiting 1s  a".into(),
            "  #6  from user  waiting 1s  b".into(),
        ]);
        b.key(Key::ch('n'));
        b.key(Key::plain(KeyCode::Enter));
        assert_eq!((b.view, b.query()), (View::Trace, vec!["trace-message".to_string(), "6".into()]));
        b.key(Key::plain(KeyCode::Enter));
        assert_eq!(b.view, View::Messages, "Enter again goes back");
        b.key(Key::ch('a'));
        assert_eq!(b.query(), ["list-messages", "-a"]);
        assert!(b.frame("x").contains("a every inbox"));
    }

    #[test]
    fn events_read_short() {
        let sent = r#"{"at":"2026-09-26T14:28:02.000+08:00","ev":"sent","msg":{"keepane":1,"id":12,"task":12,"from":"$1:@1.%3","name":"lead","mode":"ai","to":"$1:@2.%7","via":"ai","hop":0},"text":"run the tests\nand report"}"#;
        let line = event_line(sent);
        assert!(line.ends_with("sent      #12 %lead → $1:@2.%7 (ai): run the tests"), "{line}");
        let status = r#"{"at":"2026-09-26T14:31:12.000+08:00","ev":"pane","pane":"$1:@2.%7","what":"status","name":"tester","mode":"ai","program":"claude","text":"tests 3/10"}"#;
        assert!(event_line(status).ends_with("pane      status tests 3/10"), "{}", event_line(status));
        let done =
            r#"{"at":"2026-09-26T14:31:12.000+08:00","ev":"failed","id":14,"ok":false,"output":"x","cut":false}"#;
        assert!(event_line(done).ends_with("failed    #14 failed"));
        assert_eq!(event_line("not json"), "not json");
    }
}
