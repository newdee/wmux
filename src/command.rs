//! The wmux command language, shared by the `wmux` CLI, the `:` prompt,
//! key bindings and the config file.

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Target {
    pub session: Option<String>,
    /// Window index or name.
    pub window: Option<String>,
    pub pane: Option<usize>,
}

impl Target {
    /// Parse `session`, `session:window`, `:window`, `session:window.pane`.
    pub fn parse(s: &str) -> Target {
        let (sess, rest) = match s.split_once(':') {
            Some((a, b)) => (if a.is_empty() { None } else { Some(a.to_string()) }, Some(b)),
            None => (if s.is_empty() { None } else { Some(s.to_string()) }, None),
        };
        let (win, pane) = match rest {
            Some(r) => match r.split_once('.') {
                Some((w, p)) => (if w.is_empty() { None } else { Some(w.to_string()) }, p.parse().ok()),
                None => (if r.is_empty() { None } else { Some(r.to_string()) }, None),
            },
            None => (None, None),
        };
        Target { session: sess, window: win, pane }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneSel {
    Dir(Dir),
    Next,
    Prev,
    Last,
    Index(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cmd {
    NewSession {
        name: Option<String>,
        window_name: Option<String>,
        cwd: Option<String>,
        detached: bool,
        argv: Vec<String>,
        /// `-A`: attach to the session instead if it already exists.
        attach_existing: bool,
    },
    AttachSession {
        target: Option<Target>,
        detach_others: bool,
    },
    DetachClient,
    ListSessions,
    ListWindows {
        target: Option<Target>,
    },
    ListPanes {
        target: Option<Target>,
    },
    KillSession {
        target: Option<Target>,
        /// `-a`: kill every session except the target.
        all_but: bool,
    },
    KillServer,
    HasSession {
        target: Target,
    },
    RenameSession {
        target: Option<Target>,
        name: String,
    },
    NewWindow {
        name: Option<String>,
        cwd: Option<String>,
        target: Option<Target>,
        argv: Vec<String>,
        /// `-d`: do not make the new window current.
        detached: bool,
    },
    KillWindow {
        target: Option<Target>,
        all_but: bool,
    },
    RenameWindow {
        target: Option<Target>,
        name: String,
    },
    SelectWindow {
        target: Target,
    },
    NextWindow,
    PreviousWindow,
    LastWindow,
    SplitWindow {
        horizontal: bool,
        cwd: Option<String>,
        target: Option<Target>,
        argv: Vec<String>,
        /// `-d`: keep the current pane active.
        detached: bool,
        /// `-b`: put the new pane before (left of / above) the target.
        before: bool,
        /// `-f`: span the full window width/height instead of the target pane.
        full: bool,
    },
    KillPane {
        target: Option<Target>,
        all_but: bool,
    },
    SelectPane {
        sel: PaneSel,
    },
    ResizePane {
        dir: Option<Dir>,
        amount: u16,
        zoom: bool,
    },
    SwapPane {
        up: bool,
    },
    BreakPane,
    SendKeys {
        target: Option<Target>,
        keys: Vec<String>,
        /// `-l`: every argument is literal text, never a key name.
        literal: bool,
    },
    CopyMode {
        page_up: bool,
    },
    PasteBuffer,
    /// `%%` in `template` is replaced by the prompt input; `#S`/`#W` in
    /// `initial` expand to the session/window name.
    CommandPrompt {
        prompt: Option<String>,
        initial: Option<String>,
        template: Option<String>,
    },
    DisplayMessage {
        msg: String,
    },
    ConfirmBefore {
        prompt: Option<String>,
        cmd: Box<Cmd>,
    },
    BindKey {
        root: bool,
        key: String,
        cmd: Box<Cmd>,
    },
    UnbindKey {
        root: bool,
        key: String,
    },
    SetOption {
        name: String,
        value: String,
    },
    /// `show-options [-g] [-v] [-q] [name]`: one option, or all of them.
    ShowOptions {
        name: Option<String>,
        value_only: bool,
        quiet: bool,
    },
    SwitchClient {
        next: bool,
        prev: bool,
        target: Option<Target>,
    },
    ListKeys,
    /// `run-shell [-b] [-t target] command`: run a shell command with
    /// `WMUX`/`WMUX_PANE` set; output is shown (or printed) when it finishes.
    RunShell {
        command: String,
        background: bool,
        target: Option<Target>,
    },
    /// `set-hook [-g] hook command` / `set-hook -u hook`.
    SetHook {
        hook: String,
        cmd: Option<Box<Cmd>>,
    },
    ShowHooks,
    /// `load-plugin name-or-path`: source `<dir>/<name>.wmux` (or `plugin.wmux`).
    LoadPlugin {
        path: String,
    },
    ListPlugins,
    /// `save-session [-t target]`: write one session (or, with `-a`, all).
    SaveSession {
        target: Option<Target>,
        all: bool,
    },
    /// `restore-session [-a] [name]` / `resume [name]`: recreate a saved
    /// session (or every saved one); `-a` attaches to it afterwards.
    RestoreSession {
        name: Option<String>,
        attach: bool,
    },
    /// `list-saved`: saved sessions, newest first.
    ListSaved,
    /// `delete-saved name`: forget a saved session.
    DeleteSaved {
        name: String,
    },
    ClearHistory,
    /// `capture-pane -p`: print the visible text of a pane (`-S -N` adds N
    /// lines of scrollback above it).
    CapturePane {
        target: Option<Target>,
        history: usize,
    },
    SourceFile {
        path: String,
    },
    Version,
}

impl fmt::Display for Cmd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Cmd::NewSession { name, window_name, cwd, detached, argv, attach_existing } => {
                f.write_str("new-session")?;
                if let Some(n) = name {
                    write!(f, " -s {}", quote(n))?;
                }
                if let Some(n) = window_name {
                    write!(f, " -n {}", quote(n))?;
                }
                if let Some(c) = cwd {
                    write!(f, " -c {}", quote(c))?;
                }
                if *detached {
                    f.write_str(" -d")?;
                }
                if *attach_existing {
                    f.write_str(" -A")?;
                }
                for a in argv {
                    write!(f, " {}", quote(a))?;
                }
                Ok(())
            }
            Cmd::AttachSession { target, detach_others } => {
                f.write_str("attach-session")?;
                if *detach_others {
                    f.write_str(" -d")?;
                }
                fmt_target(f, target)
            }
            Cmd::DetachClient => f.write_str("detach-client"),
            Cmd::ListSessions => f.write_str("list-sessions"),
            Cmd::ListWindows { target } => {
                f.write_str("list-windows")?;
                fmt_target(f, target)
            }
            Cmd::ListPanes { target } => {
                f.write_str("list-panes")?;
                fmt_target(f, target)
            }
            Cmd::KillSession { target, all_but } => {
                f.write_str("kill-session")?;
                if *all_but {
                    f.write_str(" -a")?;
                }
                fmt_target(f, target)
            }
            Cmd::KillServer => f.write_str("kill-server"),
            Cmd::HasSession { target } => {
                f.write_str("has-session")?;
                fmt_target(f, &Some(target.clone()))
            }
            Cmd::RenameSession { target, name } => {
                f.write_str("rename-session")?;
                fmt_target(f, target)?;
                write!(f, " {}", quote(name))
            }
            Cmd::NewWindow { name, cwd, target, argv, detached } => {
                f.write_str("new-window")?;
                if *detached {
                    f.write_str(" -d")?;
                }
                if let Some(n) = name {
                    write!(f, " -n {}", quote(n))?;
                }
                if let Some(c) = cwd {
                    write!(f, " -c {}", quote(c))?;
                }
                fmt_target(f, target)?;
                for a in argv {
                    write!(f, " {}", quote(a))?;
                }
                Ok(())
            }
            Cmd::KillWindow { target, all_but } => {
                f.write_str("kill-window")?;
                if *all_but {
                    f.write_str(" -a")?;
                }
                fmt_target(f, target)
            }
            Cmd::RenameWindow { target, name } => {
                f.write_str("rename-window")?;
                fmt_target(f, target)?;
                write!(f, " {}", quote(name))
            }
            Cmd::SelectWindow { target } => {
                f.write_str("select-window")?;
                fmt_target(f, &Some(target.clone()))
            }
            Cmd::NextWindow => f.write_str("next-window"),
            Cmd::PreviousWindow => f.write_str("previous-window"),
            Cmd::LastWindow => f.write_str("last-window"),
            Cmd::SplitWindow { horizontal, cwd, target, argv, detached, before, full } => {
                f.write_str("split-window")?;
                f.write_str(if *horizontal { " -h" } else { " -v" })?;
                if *detached {
                    f.write_str(" -d")?;
                }
                if *before {
                    f.write_str(" -b")?;
                }
                if *full {
                    f.write_str(" -f")?;
                }
                if let Some(c) = cwd {
                    write!(f, " -c {}", quote(c))?;
                }
                fmt_target(f, target)?;
                for a in argv {
                    write!(f, " {}", quote(a))?;
                }
                Ok(())
            }
            Cmd::KillPane { target, all_but } => {
                f.write_str("kill-pane")?;
                if *all_but {
                    f.write_str(" -a")?;
                }
                fmt_target(f, target)
            }
            Cmd::SelectPane { sel } => {
                f.write_str("select-pane")?;
                match sel {
                    PaneSel::Dir(Dir::Left) => f.write_str(" -L"),
                    PaneSel::Dir(Dir::Right) => f.write_str(" -R"),
                    PaneSel::Dir(Dir::Up) => f.write_str(" -U"),
                    PaneSel::Dir(Dir::Down) => f.write_str(" -D"),
                    PaneSel::Next => f.write_str(" -t next"),
                    PaneSel::Prev => f.write_str(" -t prev"),
                    PaneSel::Last => f.write_str(" -l"),
                    PaneSel::Index(i) => write!(f, " -t {i}"),
                }
            }
            Cmd::ResizePane { dir, amount, zoom } => {
                f.write_str("resize-pane")?;
                if *zoom {
                    f.write_str(" -Z")?;
                }
                match dir {
                    Some(Dir::Left) => write!(f, " -L {amount}")?,
                    Some(Dir::Right) => write!(f, " -R {amount}")?,
                    Some(Dir::Up) => write!(f, " -U {amount}")?,
                    Some(Dir::Down) => write!(f, " -D {amount}")?,
                    None => {}
                }
                Ok(())
            }
            Cmd::SwapPane { up } => write!(f, "swap-pane {}", if *up { "-U" } else { "-D" }),
            Cmd::BreakPane => f.write_str("break-pane"),
            Cmd::SendKeys { target, keys, literal } => {
                f.write_str("send-keys")?;
                if *literal {
                    f.write_str(" -l")?;
                }
                fmt_target(f, target)?;
                for k in keys {
                    write!(f, " {}", quote(k))?;
                }
                Ok(())
            }
            Cmd::CopyMode { page_up } => f.write_str(if *page_up { "copy-mode -u" } else { "copy-mode" }),
            Cmd::PasteBuffer => f.write_str("paste-buffer"),
            Cmd::CommandPrompt { prompt, initial, template } => {
                f.write_str("command-prompt")?;
                if let Some(p) = prompt {
                    write!(f, " -p {}", quote(p))?;
                }
                if let Some(i) = initial {
                    write!(f, " -I {}", quote(i))?;
                }
                if let Some(t) = template {
                    write!(f, " {}", quote(t))?;
                }
                Ok(())
            }
            Cmd::DisplayMessage { msg } => write!(f, "display-message {}", quote(msg)),
            Cmd::ConfirmBefore { prompt, cmd } => {
                f.write_str("confirm-before")?;
                if let Some(p) = prompt {
                    write!(f, " -p {}", quote(p))?;
                }
                write!(f, " {}", quote(&cmd.to_string()))
            }
            Cmd::BindKey { root, key, cmd } => {
                f.write_str("bind-key")?;
                if *root {
                    f.write_str(" -n")?;
                }
                write!(f, " {} {}", quote(key), cmd)
            }
            Cmd::UnbindKey { root, key } => {
                f.write_str("unbind-key")?;
                if *root {
                    f.write_str(" -n")?;
                }
                write!(f, " {}", quote(key))
            }
            Cmd::SetOption { name, value } => write!(f, "set-option {} {}", quote(name), quote(value)),
            Cmd::SwitchClient { next, prev, target } => {
                f.write_str("switch-client")?;
                if *next {
                    f.write_str(" -n")?;
                }
                if *prev {
                    f.write_str(" -p")?;
                }
                fmt_target(f, target)
            }
            Cmd::ListKeys => f.write_str("list-keys"),
            Cmd::ShowOptions { name, value_only, quiet } => {
                f.write_str("show-options -g")?;
                if *value_only {
                    f.write_str(" -v")?;
                }
                if *quiet {
                    f.write_str(" -q")?;
                }
                if let Some(n) = name {
                    write!(f, " {}", quote(n))?;
                }
                Ok(())
            }
            Cmd::RunShell { command, background, target } => {
                f.write_str("run-shell")?;
                if *background {
                    f.write_str(" -b")?;
                }
                fmt_target(f, target)?;
                write!(f, " {}", quote(command))
            }
            Cmd::SetHook { hook, cmd } => match cmd {
                Some(c) => write!(f, "set-hook -g {} {}", quote(hook), quote(&c.to_string())),
                None => write!(f, "set-hook -gu {}", quote(hook)),
            },
            Cmd::ShowHooks => f.write_str("show-hooks -g"),
            Cmd::LoadPlugin { path } => write!(f, "load-plugin {}", quote(path)),
            Cmd::ListPlugins => f.write_str("list-plugins"),
            Cmd::SaveSession { target, all } => {
                f.write_str("save-session")?;
                if *all {
                    f.write_str(" -a")?;
                }
                fmt_target(f, target)
            }
            Cmd::RestoreSession { name, attach } => {
                f.write_str("restore-session")?;
                if *attach {
                    f.write_str(" -a")?;
                }
                if let Some(n) = name {
                    write!(f, " {}", quote(n))?;
                }
                Ok(())
            }
            Cmd::ListSaved => f.write_str("list-saved"),
            Cmd::DeleteSaved { name } => write!(f, "delete-saved {}", quote(name)),
            Cmd::ClearHistory => f.write_str("clear-history"),
            Cmd::CapturePane { target, history } => {
                f.write_str("capture-pane -p")?;
                if *history > 0 {
                    write!(f, " -S -{history}")?;
                }
                fmt_target(f, target)
            }
            Cmd::SourceFile { path } => write!(f, "source-file {}", quote(path)),
            Cmd::Version => f.write_str("version"),
        }
    }
}

fn fmt_target(f: &mut fmt::Formatter<'_>, t: &Option<Target>) -> fmt::Result {
    if let Some(t) = t {
        let mut s = t.session.clone().unwrap_or_default();
        if let Some(w) = &t.window {
            s.push(':');
            s.push_str(w);
        } else if t.pane.is_some() {
            s.push(':');
        }
        if let Some(p) = t.pane {
            s.push('.');
            s.push_str(&p.to_string());
        }
        write!(f, " -t {}", quote(&s))?;
    }
    Ok(())
}

/// Quote a word for the command language if it needs it.
pub fn quote(s: &str) -> String {
    if !s.is_empty()
        && !s.starts_with('#')
        && s.chars().all(|c| !c.is_whitespace() && c != '"' && c != '\'' && c != '\\' && c != ';')
    {
        return s.to_string();
    }
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Split a command line into words, honouring double/single quotes and
/// backslash escapes. A `#` at the start of a word begins a comment.
///
/// Backslashes are only escapes before a quote, another backslash,
/// whitespace, `#` or `;`, so Windows paths like `C:\src` survive unquoted.
pub fn tokenize(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '#' if !in_word => break,
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        None => return Err("unterminated double quote".into()),
                        Some('"') => break,
                        Some('\\') => match chars.peek() {
                            Some(&e @ ('"' | '\\')) => {
                                chars.next();
                                cur.push(e);
                            }
                            _ => cur.push('\\'),
                        },
                        Some(o) => cur.push(o),
                    }
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        None => return Err("unterminated single quote".into()),
                        Some('\'') => break,
                        Some(o) => cur.push(o),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.peek() {
                    Some(&e) if e == '"' || e == '\'' || e == '\\' || e == '#' || e == ';' || e.is_whitespace() => {
                        chars.next();
                        cur.push(e);
                    }
                    _ => cur.push('\\'),
                }
            }
            _ => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    Ok(words)
}

struct Args<'a> {
    words: &'a [String],
    pos: usize,
}

impl<'a> Args<'a> {
    fn peek(&self) -> Option<&'a str> {
        self.words.get(self.pos).map(String::as_str)
    }
    fn next(&mut self) -> Option<&'a str> {
        let w = self.peek();
        if w.is_some() {
            self.pos += 1;
        }
        w
    }
    fn rest(&mut self) -> Vec<String> {
        let r = self.words[self.pos..].to_vec();
        self.pos = self.words.len();
        r
    }
    /// True if the next word is a flag. A bare `--` ends flag parsing and is
    /// consumed, so `rename-window -- -x` names the window `-x`.
    fn is_flag(&mut self) -> bool {
        match self.peek() {
            Some("--") => {
                self.pos += 1;
                false
            }
            Some(w) => w.len() > 1 && w.starts_with('-'),
            None => false,
        }
    }
    fn value(&mut self, flag: &str) -> Result<&'a str, String> {
        self.next().ok_or_else(|| format!("{flag}: missing value"))
    }
    fn none_flags(&mut self, name: &str) -> Result<(), String> {
        if self.is_flag() { Err(format!("{name}: unknown flag '{}'", self.words[self.pos])) } else { Ok(()) }
    }
    fn none_left(&self, name: &str) -> Result<(), String> {
        if self.pos < self.words.len() {
            Err(format!("{name}: unexpected argument '{}'", self.words[self.pos]))
        } else {
            Ok(())
        }
    }
}

fn bad_flag(name: &str, flag: &str) -> String {
    format!("{name}: unknown flag '{flag}'")
}

/// Parse an argv (first word is the command name) into a `Cmd`.
pub fn parse(words: &[String]) -> Result<Cmd, String> {
    let name = words.first().ok_or_else(|| "empty command".to_string())?.as_str();
    let mut a = Args { words, pos: 1 };
    let canonical = match name {
        "new-session" | "new" => "new-session",
        "attach-session" | "attach" | "a" | "at" => "attach-session",
        "detach-client" | "detach" => "detach-client",
        "list-sessions" | "ls" => "list-sessions",
        "list-windows" | "lsw" => "list-windows",
        "list-panes" | "lsp" => "list-panes",
        "kill-session" => "kill-session",
        "kill-server" => "kill-server",
        "has-session" | "has" => "has-session",
        "rename-session" | "rename" => "rename-session",
        "new-window" | "neww" => "new-window",
        "kill-window" | "killw" => "kill-window",
        "rename-window" | "renamew" => "rename-window",
        "select-window" | "selectw" => "select-window",
        "next-window" | "next" => "next-window",
        "previous-window" | "prev" => "previous-window",
        "last-window" | "last" => "last-window",
        "split-window" | "splitw" => "split-window",
        "kill-pane" | "killp" => "kill-pane",
        "select-pane" | "selectp" => "select-pane",
        "resize-pane" | "resizep" => "resize-pane",
        "swap-pane" | "swapp" => "swap-pane",
        "break-pane" | "breakp" => "break-pane",
        "send-keys" | "send" => "send-keys",
        "copy-mode" => "copy-mode",
        "paste-buffer" | "pasteb" => "paste-buffer",
        "command-prompt" => "command-prompt",
        "display-message" | "display" => "display-message",
        "confirm-before" | "confirm" => "confirm-before",
        "bind-key" | "bind" => "bind-key",
        "unbind-key" | "unbind" => "unbind-key",
        "set-option" | "set" => "set-option",
        "show-options" | "show-option" | "show" => "show-options",
        "switch-client" | "switchc" => "switch-client",
        "list-keys" | "lsk" => "list-keys",
        "run-shell" | "run" => "run-shell",
        "set-hook" => "set-hook",
        "show-hooks" => "show-hooks",
        "load-plugin" => "load-plugin",
        "list-plugins" => "list-plugins",
        "save-session" | "save" => "save-session",
        "restore-session" | "restore" => "restore-session",
        "resume" => "resume",
        "list-saved" | "saved" => "list-saved",
        "delete-saved" | "forget" => "delete-saved",
        "clear-history" | "clearhist" => "clear-history",
        "capture-pane" | "capturep" => "capture-pane",
        "source-file" | "source" => "source-file",
        "version" | "-V" | "--version" => "version",
        other => return Err(format!("unknown command: {other}")),
    };
    let n = canonical;
    let cmd = match n {
        "new-session" => {
            let (mut name, mut window_name, mut cwd, mut detached, mut attach_existing) =
                (None, None, None, false, false);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-s" => name = Some(a.value("-s")?.to_string()),
                    "-n" => window_name = Some(a.value("-n")?.to_string()),
                    "-c" => cwd = Some(a.value("-c")?.to_string()),
                    "-d" => detached = true,
                    "-A" => attach_existing = true,
                    "-Ad" | "-dA" => {
                        attach_existing = true;
                        detached = true;
                    }
                    f => return Err(bad_flag(n, f)),
                }
            }
            Cmd::NewSession { name, window_name, cwd, detached, argv: a.rest(), attach_existing }
        }
        "attach-session" => {
            let (mut target, mut detach_others) = (None, false);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    "-d" => detach_others = true,
                    f => return Err(bad_flag(n, f)),
                }
            }
            a.none_left(n)?;
            Cmd::AttachSession { target, detach_others }
        }
        "detach-client" => {
            while a.is_flag() {
                a.next();
            }
            Cmd::DetachClient
        }
        "list-sessions" => {
            a.none_left(n)?;
            Cmd::ListSessions
        }
        "list-windows" | "list-panes" | "kill-session" | "kill-window" | "kill-pane" => {
            let (mut target, mut all_but) = (None, false);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    "-a" if n.starts_with("kill-") => all_but = true,
                    "-a" | "-s" => {} // list-panes -a/-s: we always list the target window only
                    f => return Err(bad_flag(n, f)),
                }
            }
            a.none_left(n)?;
            match n {
                "list-windows" => Cmd::ListWindows { target },
                "list-panes" => Cmd::ListPanes { target },
                "kill-session" => Cmd::KillSession { target, all_but },
                "kill-window" => Cmd::KillWindow { target, all_but },
                _ => Cmd::KillPane { target, all_but },
            }
        }
        "kill-server" => {
            a.none_left(n)?;
            Cmd::KillServer
        }
        "has-session" => {
            let mut target = None;
            while a.is_flag() {
                match a.next().unwrap() {
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    f => return Err(bad_flag(n, f)),
                }
            }
            a.none_left(n)?;
            Cmd::HasSession { target: target.ok_or("has-session: -t required")? }
        }
        "rename-session" | "rename-window" => {
            let mut target = None;
            while a.is_flag() {
                match a.next().unwrap() {
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    f => return Err(bad_flag(n, f)),
                }
            }
            let name = a.next().ok_or_else(|| format!("{n}: name required"))?.to_string();
            a.none_left(n)?;
            if n == "rename-session" { Cmd::RenameSession { target, name } } else { Cmd::RenameWindow { target, name } }
        }
        "new-window" => {
            let (mut name, mut cwd, mut target, mut detached) = (None, None, None, false);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-n" => name = Some(a.value("-n")?.to_string()),
                    "-c" => cwd = Some(a.value("-c")?.to_string()),
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    "-d" => detached = true,
                    "-a" => {} // windows are always appended at the end
                    f => return Err(bad_flag(n, f)),
                }
            }
            Cmd::NewWindow { name, cwd, target, argv: a.rest(), detached }
        }
        "select-window" => {
            let mut target = None;
            while a.is_flag() {
                match a.next().unwrap() {
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    f => return Err(bad_flag(n, f)),
                }
            }
            if target.is_none()
                && let Some(w) = a.next()
            {
                target = Some(Target::parse(&format!(":{w}")));
            }
            a.none_left(n)?;
            Cmd::SelectWindow { target: target.ok_or("select-window: -t required")? }
        }
        "next-window" => Cmd::NextWindow,
        "previous-window" => Cmd::PreviousWindow,
        "last-window" => Cmd::LastWindow,
        "split-window" => {
            let (mut horizontal, mut cwd, mut target) = (false, None, None);
            let (mut detached, mut before, mut full) = (false, false, false);
            while a.is_flag() {
                let flag = a.next().unwrap();
                // Allow tmux-style combined single-letter flags: -hd, -bf, ...
                if flag.len() > 2 && flag[1..].chars().all(|c| "hvdbf".contains(c)) {
                    for c in flag[1..].chars() {
                        match c {
                            'h' => horizontal = true,
                            'v' => horizontal = false,
                            'd' => detached = true,
                            'b' => before = true,
                            _ => full = true,
                        }
                    }
                    continue;
                }
                match flag {
                    "-h" => horizontal = true,
                    "-v" => horizontal = false,
                    "-c" => cwd = Some(a.value("-c")?.to_string()),
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    "-d" => detached = true,
                    "-b" => before = true,
                    "-f" => full = true,
                    f => return Err(bad_flag(n, f)),
                }
            }
            Cmd::SplitWindow { horizontal, cwd, target, argv: a.rest(), detached, before, full }
        }
        "select-pane" => {
            let mut sel = None;
            while a.is_flag() {
                match a.next().unwrap() {
                    "-L" => sel = Some(PaneSel::Dir(Dir::Left)),
                    "-R" => sel = Some(PaneSel::Dir(Dir::Right)),
                    "-U" => sel = Some(PaneSel::Dir(Dir::Up)),
                    "-D" => sel = Some(PaneSel::Dir(Dir::Down)),
                    "-l" => sel = Some(PaneSel::Last),
                    "-t" => {
                        sel = Some(match a.value("-t")? {
                            "next" | ":.+" | "+" => PaneSel::Next,
                            "prev" | "previous" | ":.-" | "-" => PaneSel::Prev,
                            "last" | ":.!" | "!" => PaneSel::Last,
                            other => PaneSel::Index(
                                other
                                    .trim_start_matches('%')
                                    .parse()
                                    .map_err(|_| format!("select-pane: bad target '{other}'"))?,
                            ),
                        })
                    }
                    f => return Err(bad_flag(n, f)),
                }
            }
            a.none_left(n)?;
            Cmd::SelectPane { sel: sel.ok_or("select-pane: direction or -t required")? }
        }
        "resize-pane" => {
            let (mut dir, mut amount, mut zoom) = (None, 1u16, false);
            while a.is_flag() {
                let f = a.next().unwrap();
                match f {
                    "-L" | "-R" | "-U" | "-D" => {
                        dir = Some(match f {
                            "-L" => Dir::Left,
                            "-R" => Dir::Right,
                            "-U" => Dir::Up,
                            _ => Dir::Down,
                        });
                        if let Some(v) = a.peek()
                            && let Ok(v) = v.parse::<u16>()
                        {
                            amount = v;
                            a.next();
                        }
                    }
                    "-Z" => zoom = true,
                    f => return Err(bad_flag(n, f)),
                }
            }
            if let Some(v) = a.peek()
                && let Ok(v) = v.parse::<u16>()
            {
                amount = v;
                a.next();
            }
            a.none_left(n)?;
            if dir.is_none() && !zoom {
                return Err("resize-pane: direction or -Z required".into());
            }
            Cmd::ResizePane { dir, amount, zoom }
        }
        "swap-pane" => {
            let mut up = false;
            while a.is_flag() {
                match a.next().unwrap() {
                    "-U" => up = true,
                    "-D" => up = false,
                    f => return Err(bad_flag(n, f)),
                }
            }
            a.none_left(n)?;
            Cmd::SwapPane { up }
        }
        "break-pane" => Cmd::BreakPane,
        "send-keys" => {
            let (mut target, mut literal) = (None, false);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    "-l" => literal = true,
                    f => return Err(bad_flag(n, f)),
                }
            }
            Cmd::SendKeys { target, keys: a.rest(), literal }
        }
        "copy-mode" => {
            let mut page_up = false;
            while a.is_flag() {
                match a.next().unwrap() {
                    "-u" => page_up = true,
                    "-e" | "-M" => {}
                    f => return Err(bad_flag(n, f)),
                }
            }
            a.none_left(n)?;
            Cmd::CopyMode { page_up }
        }
        "paste-buffer" => Cmd::PasteBuffer,
        "command-prompt" => {
            let (mut prompt, mut initial) = (None, None);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-p" => prompt = Some(a.value("-p")?.to_string()),
                    "-I" => initial = Some(a.value("-I")?.to_string()),
                    "-1" => {}
                    f => return Err(bad_flag(n, f)),
                }
            }
            let rest = a.rest();
            let template = if rest.is_empty() { None } else { Some(rest.join(" ")) };
            Cmd::CommandPrompt { prompt, initial, template }
        }
        "display-message" => {
            while a.is_flag() {
                a.next();
            }
            Cmd::DisplayMessage { msg: a.rest().join(" ") }
        }
        "confirm-before" => {
            let mut prompt = None;
            while a.is_flag() {
                match a.next().unwrap() {
                    "-p" => prompt = Some(a.value("-p")?.to_string()),
                    f => return Err(bad_flag(n, f)),
                }
            }
            let rest = a.rest();
            let inner = if rest.len() == 1 { tokenize(&rest[0])? } else { rest };
            Cmd::ConfirmBefore { prompt, cmd: Box::new(parse(&inner)?) }
        }
        "bind-key" => {
            let mut root = false;
            while a.is_flag() {
                match a.next().unwrap() {
                    "-n" => root = true,
                    "-r" => {}
                    "-T" => {
                        root = a.value("-T")? == "root";
                    }
                    f => return Err(bad_flag(n, f)),
                }
            }
            let key = a.next().ok_or("bind-key: key required")?.to_string();
            let rest = a.rest();
            let inner = if rest.len() == 1 { tokenize(&rest[0])? } else { rest };
            Cmd::BindKey { root, key, cmd: Box::new(parse(&inner)?) }
        }
        "unbind-key" => {
            let mut root = false;
            while a.is_flag() {
                match a.next().unwrap() {
                    "-n" => root = true,
                    "-T" => {
                        root = a.value("-T")? == "root";
                    }
                    f => return Err(bad_flag(n, f)),
                }
            }
            let key = a.next().ok_or("unbind-key: key required")?.to_string();
            a.none_left(n)?;
            Cmd::UnbindKey { root, key }
        }
        "set-option" => {
            while a.is_flag() {
                a.next(); // -g / -s / -w are accepted and ignored
            }
            let name = a.next().ok_or("set-option: option name required")?.to_string();
            let value = a.rest().join(" ");
            Cmd::SetOption { name, value }
        }
        "switch-client" => {
            let (mut next, mut prev, mut target) = (false, false, None);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-n" => next = true,
                    "-p" => prev = true,
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    f => return Err(bad_flag(n, f)),
                }
            }
            a.none_left(n)?;
            Cmd::SwitchClient { next, prev, target }
        }
        "list-keys" => Cmd::ListKeys,
        "show-options" => {
            let (mut value_only, mut quiet) = (false, false);
            while a.is_flag() {
                let flag = a.next().unwrap();
                // Combined flags as tmux users write them: -gqv, -gv, ...
                if flag.len() > 2 && flag[1..].chars().all(|c| "gswpqv".contains(c)) {
                    value_only |= flag.contains('v');
                    quiet |= flag.contains('q');
                    continue;
                }
                match flag {
                    "-v" => value_only = true,
                    "-q" => quiet = true,
                    "-g" | "-s" | "-w" | "-p" => {}
                    "-t" => {
                        a.value("-t")?;
                    }
                    f => return Err(bad_flag(n, f)),
                }
            }
            let name = a.next().map(str::to_string);
            a.none_left(n)?;
            Cmd::ShowOptions { name, value_only, quiet }
        }
        "run-shell" => {
            let (mut background, mut target) = (false, None);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-b" => background = true,
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    "-C" => {} // tmux: run as a wmux command; here everything is a shell command
                    f => return Err(bad_flag(n, f)),
                }
            }
            let rest = a.rest();
            if rest.is_empty() {
                return Err("run-shell: command required".into());
            }
            Cmd::RunShell { command: rest.join(" "), background, target }
        }
        "set-hook" => {
            let mut unset = false;
            while a.is_flag() {
                match a.next().unwrap() {
                    "-u" | "-gu" | "-ug" => unset = true,
                    "-g" | "-a" | "-R" => {}
                    "-t" => {
                        a.value("-t")?;
                    }
                    f => return Err(bad_flag(n, f)),
                }
            }
            let hook = a.next().ok_or("set-hook: hook name required")?.to_string();
            if unset {
                a.none_left(n)?;
                Cmd::SetHook { hook, cmd: None }
            } else {
                let rest = a.rest();
                if rest.is_empty() {
                    return Err("set-hook: command required".into());
                }
                let inner = if rest.len() == 1 { tokenize(&rest[0])? } else { rest };
                Cmd::SetHook { hook, cmd: Some(Box::new(parse(&inner)?)) }
            }
        }
        "show-hooks" => {
            while a.is_flag() {
                a.next();
            }
            Cmd::ShowHooks
        }
        "load-plugin" => {
            let path = a.next().ok_or("load-plugin: name or path required")?.to_string();
            a.none_left(n)?;
            Cmd::LoadPlugin { path }
        }
        "list-plugins" => Cmd::ListPlugins,
        "save-session" => {
            let (mut target, mut all) = (None, false);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    "-a" => all = true,
                    f => return Err(bad_flag(n, f)),
                }
            }
            a.none_left(n)?;
            Cmd::SaveSession { target, all }
        }
        "restore-session" | "resume" => {
            let mut attach = n == "resume";
            while a.is_flag() {
                match a.next().unwrap() {
                    "-a" => attach = true,
                    "-t" => {
                        // `resume -t name` reads naturally next to attach -t.
                        let v = a.value("-t")?.to_string();
                        a.none_left(n)?;
                        return Ok(Cmd::RestoreSession { name: Some(v), attach });
                    }
                    f => return Err(bad_flag(n, f)),
                }
            }
            let name = a.next().map(str::to_string);
            a.none_left(n)?;
            Cmd::RestoreSession { name, attach }
        }
        "list-saved" => {
            a.none_flags(n)?;
            a.none_left(n)?;
            Cmd::ListSaved
        }
        "delete-saved" => {
            a.none_flags(n)?;
            let name = a.next().ok_or("delete-saved: name required")?.to_string();
            a.none_left(n)?;
            Cmd::DeleteSaved { name }
        }
        "clear-history" => Cmd::ClearHistory,
        "capture-pane" => {
            let (mut target, mut history) = (None, 0usize);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    "-S" => {
                        let v = a.value("-S")?;
                        history = match v {
                            "-" => usize::MAX,
                            v => {
                                v.trim_start_matches('-').parse().map_err(|_| format!("capture-pane: bad -S '{v}'"))?
                            }
                        };
                    }
                    "-p" | "-e" | "-J" => {} // always printed, plain text
                    f => return Err(bad_flag(n, f)),
                }
            }
            a.none_left(n)?;
            Cmd::CapturePane { target, history }
        }
        "source-file" => {
            let path = a.next().ok_or("source-file: path required")?.to_string();
            a.none_left(n)?;
            Cmd::SourceFile { path }
        }
        "version" => Cmd::Version,
        _ => unreachable!(),
    };
    Ok(cmd)
}

/// Parse a full command line (as typed at the `:` prompt or in a config file).
pub fn parse_line(line: &str) -> Result<Option<Cmd>, String> {
    let words = tokenize(line)?;
    if words.is_empty() {
        return Ok(None);
    }
    parse(&words).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Cmd {
        parse_line(s).unwrap().unwrap()
    }

    #[test]
    fn tokenizer() {
        assert_eq!(tokenize(r#"a "b c" 'd e' f\ g # comment"#).unwrap(), vec!["a", "b c", "d e", "f g"]);
        assert_eq!(tokenize(r#""x\"y""#).unwrap(), vec!["x\"y"]);
        assert_eq!(tokenize(r"C:\src\new C:\ok").unwrap(), vec![r"C:\src\new", r"C:\ok"]);
        assert_eq!(tokenize(r#""C:\src\new" "a\\b""#).unwrap(), vec![r"C:\src\new", r"a\b"]);
        assert_eq!(tokenize(r"trailing\").unwrap(), vec![r"trailing\"]);
        assert_eq!(tokenize("").unwrap(), Vec::<String>::new());
        assert_eq!(tokenize("   # only comment").unwrap(), Vec::<String>::new());
        assert!(tokenize("\"open").is_err());
    }

    #[test]
    fn quote_roundtrip() {
        for s in ["plain", "with space", "q\"uote", "", "semi;colon", "back\\slash", "#W", "C:\\src\\new", "a\\\\b"] {
            let q = quote(s);
            assert_eq!(tokenize(&q).unwrap(), vec![s], "quote({s:?}) = {q}");
        }
    }

    #[test]
    fn targets() {
        assert_eq!(Target::parse("main"), Target { session: Some("main".into()), window: None, pane: None });
        assert_eq!(
            Target::parse("main:2"),
            Target { session: Some("main".into()), window: Some("2".into()), pane: None }
        );
        assert_eq!(Target::parse(":2"), Target { session: None, window: Some("2".into()), pane: None });
        assert_eq!(
            Target::parse("s:w.3"),
            Target { session: Some("s".into()), window: Some("w".into()), pane: Some(3) }
        );
    }

    #[test]
    fn parse_new_session() {
        assert_eq!(
            p("new -s main -n shell -d -- wsl.exe -d Ubuntu"),
            Cmd::NewSession {
                name: Some("main".into()),
                window_name: Some("shell".into()),
                cwd: None,
                detached: true,
                argv: vec!["wsl.exe".into(), "-d".into(), "Ubuntu".into()],
                attach_existing: false,
            }
        );
        assert_eq!(
            p("new-session"),
            Cmd::NewSession {
                name: None,
                window_name: None,
                cwd: None,
                detached: false,
                argv: vec![],
                attach_existing: false
            }
        );
        assert!(matches!(p("new -A -s x"), Cmd::NewSession { attach_existing: true, detached: false, .. }));
        assert!(matches!(p("new -Ad -s x"), Cmd::NewSession { attach_existing: true, detached: true, .. }));
    }

    #[test]
    fn double_dash_ends_flags() {
        // The default `,` binding expands to exactly this.
        assert_eq!(p("rename-window -- shell"), Cmd::RenameWindow { target: None, name: "shell".into() });
        assert_eq!(p("rename-session -- -weird"), Cmd::RenameSession { target: None, name: "-weird".into() });
        assert!(matches!(p("new-window -- -x"), Cmd::NewWindow { argv, .. } if argv == vec!["-x".to_string()]));
        assert!(
            matches!(p("send-keys -- -l"), Cmd::SendKeys { keys, literal: false, .. } if keys == vec!["-l".to_string()])
        );
        assert!(
            matches!(p("split-window -h -- pwsh -NoLogo"), Cmd::SplitWindow { horizontal: true, argv, .. } if argv.len() == 2)
        );
    }

    #[test]
    fn parse_plugin_commands() {
        assert_eq!(
            p("run-shell -b pwsh -c \"Get-Date\""),
            Cmd::RunShell { command: "pwsh -c Get-Date".into(), background: true, target: None }
        );
        assert_eq!(p("run \"echo hi\""), Cmd::RunShell { command: "echo hi".into(), background: false, target: None });
        assert!(parse_line("run-shell").is_err());
        assert_eq!(
            p("set-hook -g after-new-window rename-window hooked"),
            Cmd::SetHook {
                hook: "after-new-window".into(),
                cmd: Some(Box::new(Cmd::RenameWindow { target: None, name: "hooked".into() }))
            }
        );
        assert_eq!(
            p("set-hook -g after-new-window \"rename-window hooked\""),
            Cmd::SetHook {
                hook: "after-new-window".into(),
                cmd: Some(Box::new(Cmd::RenameWindow { target: None, name: "hooked".into() }))
            }
        );
        assert_eq!(p("set-hook -gu after-new-window"), Cmd::SetHook { hook: "after-new-window".into(), cmd: None });
        assert_eq!(p("load-plugin demo"), Cmd::LoadPlugin { path: "demo".into() });
        assert_eq!(
            p("show-options -gqv @theme"),
            Cmd::ShowOptions { name: Some("@theme".into()), value_only: true, quiet: true }
        );
        assert_eq!(p("show -g"), Cmd::ShowOptions { name: None, value_only: false, quiet: false });
        assert_eq!(p("resume"), Cmd::RestoreSession { name: None, attach: true });
        assert_eq!(p("resume work"), Cmd::RestoreSession { name: Some("work".into()), attach: true });
        assert_eq!(p("resume -t work"), Cmd::RestoreSession { name: Some("work".into()), attach: true });
        assert_eq!(p("restore-session"), Cmd::RestoreSession { name: None, attach: false });
        assert_eq!(p("save -t work"), Cmd::SaveSession { target: Some(Target::parse("work")), all: false });
        assert_eq!(p("save -a"), Cmd::SaveSession { target: None, all: true });
        assert_eq!(p("saved"), Cmd::ListSaved);
        assert_eq!(p("forget old"), Cmd::DeleteSaved { name: "old".into() });
        assert!(parse_line("save-session -x").is_err());
        assert!(parse_line("delete-saved").is_err());
        assert_eq!(
            p("show-option -v mouse"),
            Cmd::ShowOptions { name: Some("mouse".into()), value_only: true, quiet: false }
        );
    }

    #[test]
    fn parse_flags_that_change_behaviour() {
        assert!(matches!(p("neww -d"), Cmd::NewWindow { detached: true, .. }));
        assert!(matches!(
            p("splitw -hdb"),
            Cmd::SplitWindow { horizontal: true, detached: true, before: true, full: false, .. }
        ));
        assert!(matches!(p("splitw -f -v"), Cmd::SplitWindow { horizontal: false, full: true, .. }));
        assert!(matches!(p("killw -a"), Cmd::KillWindow { all_but: true, .. }));
        assert!(matches!(p("killp -a -t :.1"), Cmd::KillPane { all_but: true, .. }));
        assert!(matches!(p("kill-session -a"), Cmd::KillSession { all_but: true, .. }));
        assert!(matches!(p("send -l Enter"), Cmd::SendKeys { literal: true, .. }));
    }

    #[test]
    fn parse_pane_commands() {
        assert_eq!(p("select-pane -L"), Cmd::SelectPane { sel: PaneSel::Dir(Dir::Left) });
        assert_eq!(p("selectp -t next"), Cmd::SelectPane { sel: PaneSel::Next });
        assert_eq!(p("resize-pane -R 5"), Cmd::ResizePane { dir: Some(Dir::Right), amount: 5, zoom: false });
        assert_eq!(p("resizep -Z"), Cmd::ResizePane { dir: None, amount: 1, zoom: true });
        assert_eq!(
            p("splitw -h -c C:\\src"),
            Cmd::SplitWindow {
                horizontal: true,
                cwd: Some("C:\\src".into()),
                target: None,
                argv: vec![],
                detached: false,
                before: false,
                full: false
            }
        );
        assert!(parse_line("resize-pane").is_err());
        assert!(parse_line("select-pane -X").is_err());
    }

    #[test]
    fn parse_nested_commands() {
        assert_eq!(
            p("confirm-before -p \"kill-window? (y/n)\" kill-window"),
            Cmd::ConfirmBefore {
                prompt: Some("kill-window? (y/n)".into()),
                cmd: Box::new(Cmd::KillWindow { target: None, all_but: false })
            }
        );
        assert_eq!(
            p("bind-key -n M-h select-pane -L"),
            Cmd::BindKey {
                root: true,
                key: "M-h".into(),
                cmd: Box::new(Cmd::SelectPane { sel: PaneSel::Dir(Dir::Left) })
            }
        );
        assert_eq!(
            p("bind h \"select-pane -L\""),
            Cmd::BindKey {
                root: false,
                key: "h".into(),
                cmd: Box::new(Cmd::SelectPane { sel: PaneSel::Dir(Dir::Left) })
            }
        );
        assert_eq!(p("set -g prefix C-a"), Cmd::SetOption { name: "prefix".into(), value: "C-a".into() });
    }

    #[test]
    fn display_roundtrip() {
        let cmds = [
            "new-session -s main -n shell -d wsl.exe -d Ubuntu",
            "attach-session -d -t main",
            "split-window -h -c C:\\src",
            "select-pane -L",
            "resize-pane -Z",
            "resize-pane -U 3",
            "send-keys -t main:1 ls Enter",
            "send-keys -l Enter",
            "new-session -s x -d -A",
            "new-window -d -n w",
            "split-window -v -d -b -f",
            "kill-window -a -t :2",
            "kill-pane -a",
            "kill-session -a -t x",
            "capture-pane -p -S -100 -t w:1",
            "run-shell -b -t w:1 \"pwsh -c Get-Date\"",
            "set-hook -g after-new-window \"rename-window hooked\"",
            "set-hook -gu after-new-window",
            "load-plugin \"C:\\plugins\\demo\"",
            "list-plugins",
            "show-hooks -g",
            "show-options -g -v -q @x",
            "show-options -g",
            "save-session -a",
            "save-session -t work",
            "restore-session -a",
            "restore-session \"my session\"",
            "list-saved",
            "delete-saved old",
            "confirm-before -p \"kill? (y/n)\" \"kill-window\"",
            "bind-key -n M-h select-pane -L",
            "set-option prefix C-a",
            "display-message \"hello world\"",
            "select-window -t :3",
            "rename-window \"my win\"",
            "copy-mode -u",
            "command-prompt -p (rename-window) -I \"#W\" \"rename-window -- %%\"",
        ];
        for c in cmds {
            let parsed = p(c);
            let shown = parsed.to_string();
            assert_eq!(p(&shown), parsed, "roundtrip of {c:?} via {shown:?}");
        }
    }

    #[test]
    fn unknown_command() {
        assert!(parse_line("frobnicate").is_err());
        assert_eq!(parse_line("").unwrap(), None);
    }
}
