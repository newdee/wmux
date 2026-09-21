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

/// One line of a `display-menu`: a label, the key that picks it, and what it
/// runs. A label with no command is a separator (tmux writes it as `""`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuItem {
    pub name: String,
    pub key: Option<String>,
    pub cmd: Option<Box<Cmd>>,
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
    DetachClient {
        /// `-a`: every client but this one; with a session, every client of it.
        all: bool,
        target: Option<Target>,
    },
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
    /// `swap-window [-s src] [-t dst]` / `move-window [-s src] [-t dst]`:
    /// reorder the windows of a session.
    SwapWindow {
        src: Option<Target>,
        dst: Option<Target>,
        /// `move-window`: take the source out and put it at the destination,
        /// shifting the rest, instead of exchanging the two.
        move_it: bool,
    },
    NextWindow {
        target: Option<Target>,
        /// `-a`: the next window with an alert (activity, bell or silence).
        alert: bool,
    },
    PreviousWindow {
        target: Option<Target>,
        alert: bool,
    },
    LastWindow {
        target: Option<Target>,
    },
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
    /// `select-pane -T title` / `-m` / `-M`: name a pane, or mark it so
    /// `join-pane` and `swap-pane` know where to take a pane from.
    PaneTitle {
        target: Option<Target>,
        title: Option<String>,
        mark: bool,
        unmark: bool,
    },
    ResizePane {
        dir: Option<Dir>,
        amount: u16,
        zoom: bool,
        target: Option<Target>,
        /// `-x`/`-y`: an exact width/height in cells, or a percentage of the
        /// window ("50%").
        width: Option<String>,
        height: Option<String>,
    },
    SwapPane {
        up: bool,
        target: Option<Target>,
    },
    /// `select-layout [-n|-p] [-t target] [name]`: rearrange a window's panes
    /// into one of the named layouts (tmux `select-layout`).
    SelectLayout {
        /// A layout name, or None with `next`/`prev` to cycle.
        name: Option<String>,
        next: bool,
        prev: bool,
        /// `-E`: even out the panes beside the target, keeping the layout.
        spread: bool,
        target: Option<Target>,
    },
    BreakPane {
        target: Option<Target>,
    },
    /// `join-pane [-h|-v] [-b] [-s src] [-t dst]` (and `move-pane`, the same
    /// command): take a pane out of its window and split another with it.
    JoinPane {
        src: Option<Target>,
        dst: Option<Target>,
        horizontal: bool,
        before: bool,
    },
    /// `find-window [-t target] pattern`: pick from the windows whose name,
    /// title or visible text matches.
    FindWindow {
        pattern: String,
    },
    SendKeys {
        target: Option<Target>,
        keys: Vec<String>,
        /// `-l`: every argument is literal text, never a key name.
        literal: bool,
    },
    /// `send-keys -X <name> [arg]`: run a copy-mode command by name, so a
    /// binding can drive copy mode (tmux's `-X`).
    CopyCommand {
        target: Option<Target>,
        name: String,
        arg: Option<String>,
    },
    CopyMode {
        page_up: bool,
    },
    /// `display-panes`: show each pane's number for `display-time`; a digit
    /// pressed while they are up selects that pane.
    DisplayPanes,
    /// `clock-mode [-t]`: a big clock in the pane until a key is pressed.
    ClockMode {
        target: Option<Target>,
    },
    /// `if-shell [-b] [-F] condition command [command]`
    IfShell {
        /// `-F`: the condition is a format string, not a shell command.
        format: bool,
        condition: String,
        then_cmd: Box<Cmd>,
        else_cmd: Option<Box<Cmd>>,
    },
    /// `rotate-window [-D|-U] [-t]`: move every pane one place around the
    /// layout, keeping the layout itself.
    RotateWindow {
        down: bool,
        target: Option<Target>,
    },
    /// `refresh-client`: redraw everything this client shows.
    RefreshClient,
    /// `send-prefix [-t]`: send the prefix key itself to the pane.
    SendPrefix {
        target: Option<Target>,
    },
    /// `list-commands`: the command names this wmux knows.
    ListCommands,
    /// `list-clients`: who is attached, to what, at what size.
    ListClients,
    /// `paste-buffer [-b name] [-t target] [-p]`: send a buffer (or, with no
    /// buffer named, the Windows clipboard) to a pane.
    PasteBuffer {
        name: Option<String>,
        target: Option<Target>,
        /// `-p`: bracketed paste, if the program asked for it.
        bracketed: bool,
    },
    /// `set-buffer [-b name] [-a] data`
    SetBuffer {
        name: Option<String>,
        data: String,
        append: bool,
    },
    /// `load-buffer [-b name] path` / `save-buffer [-b name] path`
    LoadBuffer {
        name: Option<String>,
        path: String,
    },
    SaveBuffer {
        name: Option<String>,
        path: String,
        append: bool,
    },
    ShowBuffer {
        name: Option<String>,
    },
    DeleteBuffer {
        name: Option<String>,
    },
    ListBuffers,
    /// `choose-buffer`: pick a buffer from a list and paste it.
    ChooseBuffer,
    /// `choose-client`: pick an attached client from a list and detach it
    /// (tmux binds this to prefix `D`).
    ChooseClient,
    /// `display-menu [-T title] name key command ...`: a menu over the window;
    /// an empty name is a separator, `key` selects the entry directly.
    DisplayMenu {
        title: Option<String>,
        items: Vec<MenuItem>,
    },
    /// `display-popup [-C] [-E] [-d dir] [-w width] [-h height] [command]`: a
    /// program in a box over the window, with its own terminal.
    DisplayPopup {
        /// `-C`: close the popup this client has open.
        close: bool,
        /// `-E`: close the popup as soon as the command finishes.
        close_on_exit: bool,
        width: Option<String>,
        height: Option<String>,
        cwd: Option<String>,
        argv: Vec<String>,
    },
    /// `pipe-pane [-o] [-t target] [command]`: copy everything a pane writes
    /// into the standard input of a shell command. No command stops the pipe.
    PipePane {
        target: Option<Target>,
        command: Option<String>,
        /// `-o`: stop instead of starting if this pane is already piped.
        toggle: bool,
    },
    /// `wait-for [-L|-S|-U] channel`: block a client until another one signals
    /// (or unlocks) the channel, so scripts can wait for each other.
    WaitFor {
        channel: String,
        lock: bool,
        unlock: bool,
        signal: bool,
    },
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
        /// `-r`: the key repeats without the prefix for `repeat-time`.
        repeat: bool,
        cmd: Box<Cmd>,
    },
    UnbindKey {
        root: bool,
        key: String,
    },
    SetOption {
        name: String,
        value: String,
        /// `-a`: add to the option's current value instead of replacing it.
        append: bool,
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
        /// `-l`: back to the session this client came from.
        last: bool,
        target: Option<Target>,
    },
    /// `show-messages`: the recent status-line messages, newest last.
    ShowMessages,
    /// `set-environment [-r] name [value]`: the environment new panes get.
    SetEnvironment {
        name: String,
        value: Option<String>,
        remove: bool,
    },
    ShowEnvironment {
        name: Option<String>,
    },
    /// `respawn-pane [-k] [-t] [command]` / `respawn-window`: start the
    /// command again in a pane that has exited (or, with `-k`, in a live one).
    RespawnPane {
        target: Option<Target>,
        kill: bool,
        argv: Vec<String>,
        /// Every pane of the window instead of one.
        window: bool,
    },
    ListKeys,
    /// `choose-tree [-s|-w]`: interactive session/window picker (tmux
    /// `choose-tree`); `choose-session` = `-s`, `choose-window` = `-w`.
    ChooseTree {
        /// `-s`: sessions only (collapsed).
        sessions: bool,
        /// `-w`: every session expanded to its windows.
        windows: bool,
    },
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
        /// `-e`: keep the colours and attributes as escape sequences.
        escapes: bool,
    },
    /// `set-cwd [-t target] [dir]`: record a pane's working directory (used
    /// by save/resume and `#{pane_current_path}`); no `dir` means the
    /// calling client's current directory.
    SetCwd {
        target: Option<Target>,
        dir: Option<String>,
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
            Cmd::DetachClient { all, target } => {
                f.write_str("detach-client")?;
                if *all {
                    f.write_str(" -a")?;
                }
                fmt_target(f, target)
            }
            Cmd::ShowMessages => f.write_str("show-messages"),
            Cmd::SetEnvironment { name, value, remove } => {
                f.write_str("set-environment -g")?;
                if *remove {
                    f.write_str(" -r")?;
                }
                write!(f, " {}", quote(name))?;
                if let Some(v) = value {
                    write!(f, " {}", quote(v))?;
                }
                Ok(())
            }
            Cmd::ShowEnvironment { name } => {
                f.write_str("show-environment -g")?;
                if let Some(n) = name {
                    write!(f, " {}", quote(n))?;
                }
                Ok(())
            }
            Cmd::RespawnPane { target, kill, argv, window } => {
                f.write_str(if *window { "respawn-window" } else { "respawn-pane" })?;
                if *kill {
                    f.write_str(" -k")?;
                }
                fmt_target(f, target)?;
                for a in argv {
                    write!(f, " {}", quote(a))?;
                }
                Ok(())
            }
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
            Cmd::SwapWindow { src, dst, move_it } => {
                f.write_str(if *move_it { "move-window" } else { "swap-window" })?;
                if let Some(s) = src {
                    f.write_str(" -s")?;
                    fmt_target(f, &Some(s.clone()))?;
                }
                if let Some(d) = dst {
                    f.write_str(" -t")?;
                    fmt_target(f, &Some(d.clone()))?;
                }
                Ok(())
            }
            Cmd::NextWindow { target, alert } => {
                f.write_str("next-window")?;
                if *alert {
                    f.write_str(" -a")?;
                }
                fmt_target(f, target)
            }
            Cmd::PreviousWindow { target, alert } => {
                f.write_str("previous-window")?;
                if *alert {
                    f.write_str(" -a")?;
                }
                fmt_target(f, target)
            }
            Cmd::LastWindow { target } => {
                f.write_str("last-window")?;
                fmt_target(f, target)
            }
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
            Cmd::PaneTitle { target, title, mark, unmark } => {
                f.write_str("select-pane")?;
                if *mark {
                    f.write_str(" -m")?;
                }
                if *unmark {
                    f.write_str(" -M")?;
                }
                fmt_target(f, target)?;
                if let Some(t) = title {
                    write!(f, " -T {}", quote(t))?;
                }
                Ok(())
            }
            Cmd::ResizePane { dir, amount, zoom, target, width, height } => {
                f.write_str("resize-pane")?;
                if *zoom {
                    f.write_str(" -Z")?;
                }
                if let Some(x) = width {
                    write!(f, " -x {x}")?;
                }
                if let Some(y) = height {
                    write!(f, " -y {y}")?;
                }
                match dir {
                    Some(Dir::Left) => write!(f, " -L {amount}")?,
                    Some(Dir::Right) => write!(f, " -R {amount}")?,
                    Some(Dir::Up) => write!(f, " -U {amount}")?,
                    Some(Dir::Down) => write!(f, " -D {amount}")?,
                    None => {}
                }
                fmt_target(f, target)
            }
            Cmd::SwapPane { up, target } => {
                write!(f, "swap-pane {}", if *up { "-U" } else { "-D" })?;
                fmt_target(f, target)
            }
            Cmd::SelectLayout { name, next, prev, spread, target } => {
                f.write_str("select-layout")?;
                if *next {
                    f.write_str(" -n")?;
                }
                if *prev {
                    f.write_str(" -p")?;
                }
                if *spread {
                    f.write_str(" -E")?;
                }
                fmt_target(f, target)?;
                if let Some(n) = name {
                    write!(f, " {}", quote(n))?;
                }
                Ok(())
            }
            Cmd::BreakPane { target } => {
                f.write_str("break-pane")?;
                fmt_target(f, target)
            }
            Cmd::JoinPane { src, dst, horizontal, before } => {
                f.write_str("join-pane")?;
                f.write_str(if *horizontal { " -h" } else { " -v" })?;
                if *before {
                    f.write_str(" -b")?;
                }
                if let Some(s) = src {
                    f.write_str(" -s")?;
                    fmt_target(f, &Some(s.clone()))?;
                }
                if let Some(d) = dst {
                    f.write_str(" -t")?;
                    fmt_target(f, &Some(d.clone()))?;
                }
                Ok(())
            }
            Cmd::FindWindow { pattern } => write!(f, "find-window {}", quote(pattern)),
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
            Cmd::CopyCommand { target, name, arg } => {
                f.write_str("send-keys -X")?;
                fmt_target(f, target)?;
                write!(f, " {}", quote(name))?;
                if let Some(a) = arg {
                    write!(f, " {}", quote(a))?;
                }
                Ok(())
            }
            Cmd::CopyMode { page_up } => f.write_str(if *page_up { "copy-mode -u" } else { "copy-mode" }),
            Cmd::DisplayPanes => f.write_str("display-panes"),
            Cmd::ClockMode { target } => {
                f.write_str("clock-mode")?;
                fmt_target(f, target)
            }
            Cmd::IfShell { format, condition, then_cmd, else_cmd } => {
                f.write_str("if-shell")?;
                if *format {
                    f.write_str(" -F")?;
                }
                write!(f, " {} {}", quote(condition), quote(&then_cmd.to_string()))?;
                if let Some(e) = else_cmd {
                    write!(f, " {}", quote(&e.to_string()))?;
                }
                Ok(())
            }
            Cmd::RotateWindow { down, target } => {
                write!(f, "rotate-window {}", if *down { "-D" } else { "-U" })?;
                fmt_target(f, target)
            }
            Cmd::RefreshClient => f.write_str("refresh-client"),
            Cmd::SendPrefix { target } => {
                f.write_str("send-prefix")?;
                fmt_target(f, target)
            }
            Cmd::ListCommands => f.write_str("list-commands"),
            Cmd::ListClients => f.write_str("list-clients"),
            Cmd::PasteBuffer { name, target, bracketed } => {
                f.write_str("paste-buffer")?;
                if *bracketed {
                    f.write_str(" -p")?;
                }
                if let Some(b) = name {
                    write!(f, " -b {}", quote(b))?;
                }
                fmt_target(f, target)
            }
            Cmd::SetBuffer { name, data, append } => {
                f.write_str("set-buffer")?;
                if *append {
                    f.write_str(" -a")?;
                }
                if let Some(b) = name {
                    write!(f, " -b {}", quote(b))?;
                }
                write!(f, " {}", quote(data))
            }
            Cmd::LoadBuffer { name, path } => {
                f.write_str("load-buffer")?;
                if let Some(b) = name {
                    write!(f, " -b {}", quote(b))?;
                }
                write!(f, " {}", quote(path))
            }
            Cmd::SaveBuffer { name, path, append } => {
                f.write_str("save-buffer")?;
                if *append {
                    f.write_str(" -a")?;
                }
                if let Some(b) = name {
                    write!(f, " -b {}", quote(b))?;
                }
                write!(f, " {}", quote(path))
            }
            Cmd::ShowBuffer { name } => {
                f.write_str("show-buffer")?;
                if let Some(b) = name {
                    write!(f, " -b {}", quote(b))?;
                }
                Ok(())
            }
            Cmd::DeleteBuffer { name } => {
                f.write_str("delete-buffer")?;
                if let Some(b) = name {
                    write!(f, " -b {}", quote(b))?;
                }
                Ok(())
            }
            Cmd::ListBuffers => f.write_str("list-buffers"),
            Cmd::ChooseBuffer => f.write_str("choose-buffer"),
            Cmd::ChooseClient => f.write_str("choose-client"),
            Cmd::DisplayMenu { title, items } => {
                f.write_str("display-menu")?;
                if let Some(t) = title {
                    write!(f, " -T {}", quote(t))?;
                }
                for it in items {
                    write!(f, " {}", quote(&it.name))?;
                    match (&it.key, &it.cmd) {
                        (Some(k), Some(c)) => write!(f, " {} {}", quote(k), quote(&c.to_string()))?,
                        (Some(k), None) => write!(f, " {} {}", quote(k), quote(""))?,
                        _ => {}
                    }
                }
                Ok(())
            }
            Cmd::DisplayPopup { close, close_on_exit, width, height, cwd, argv } => {
                f.write_str("display-popup")?;
                if *close {
                    f.write_str(" -C")?;
                }
                if *close_on_exit {
                    f.write_str(" -E")?;
                }
                if let Some(w) = width {
                    write!(f, " -w {}", quote(w))?;
                }
                if let Some(h) = height {
                    write!(f, " -h {}", quote(h))?;
                }
                if let Some(d) = cwd {
                    write!(f, " -d {}", quote(d))?;
                }
                for a in argv {
                    write!(f, " {}", quote(a))?;
                }
                Ok(())
            }
            Cmd::PipePane { target, command, toggle } => {
                f.write_str("pipe-pane")?;
                if *toggle {
                    f.write_str(" -o")?;
                }
                fmt_target(f, target)?;
                if let Some(c) = command {
                    write!(f, " {}", quote(c))?;
                }
                Ok(())
            }
            Cmd::WaitFor { channel, lock, unlock, signal } => {
                f.write_str("wait-for")?;
                if *lock {
                    f.write_str(" -L")?;
                }
                if *unlock {
                    f.write_str(" -U")?;
                }
                if *signal {
                    f.write_str(" -S")?;
                }
                write!(f, " {}", quote(channel))
            }
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
            Cmd::BindKey { root, key, repeat, cmd } => {
                f.write_str("bind-key")?;
                if *root {
                    f.write_str(" -n")?;
                }
                if *repeat {
                    f.write_str(" -r")?;
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
            Cmd::SetOption { name, value, append } => {
                write!(f, "set-option{} {} {}", if *append { " -a" } else { "" }, quote(name), quote(value))
            }
            Cmd::SwitchClient { next, prev, last, target } => {
                f.write_str("switch-client")?;
                if *next {
                    f.write_str(" -n")?;
                }
                if *prev {
                    f.write_str(" -p")?;
                }
                if *last {
                    f.write_str(" -l")?;
                }
                fmt_target(f, target)
            }
            Cmd::ListKeys => f.write_str("list-keys"),
            Cmd::ChooseTree { sessions, windows } => {
                f.write_str("choose-tree")?;
                if *sessions {
                    f.write_str(" -s")?;
                }
                if *windows {
                    f.write_str(" -w")?;
                }
                Ok(())
            }
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
            Cmd::SetCwd { target, dir } => {
                f.write_str("set-cwd")?;
                fmt_target(f, target)?;
                if let Some(d) = dir {
                    write!(f, " {}", quote(d))?;
                }
                Ok(())
            }
            Cmd::CapturePane { target, history, escapes } => {
                f.write_str("capture-pane -p")?;
                if *escapes {
                    f.write_str(" -e")?;
                }
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

/// Every command name, for the unambiguous-prefix lookup below.
pub const COMMANDS: &[&str] = &[
    "attach-session",
    "break-pane",
    "bind-key",
    "capture-pane",
    "choose-buffer",
    "choose-client",
    "choose-session",
    "clock-mode",
    "choose-tree",
    "choose-window",
    "clear-history",
    "command-prompt",
    "confirm-before",
    "delete-buffer",
    "delete-saved",
    "detach-client",
    "display-menu",
    "display-message",
    "display-popup",
    "display-panes",
    "find-window",
    "has-session",
    "if-shell",
    "join-pane",
    "kill-pane",
    "kill-server",
    "kill-session",
    "kill-window",
    "last-pane",
    "last-window",
    "list-buffers",
    "list-clients",
    "list-commands",
    "list-keys",
    "list-panes",
    "list-plugins",
    "list-saved",
    "list-sessions",
    "list-windows",
    "load-buffer",
    "load-plugin",
    "move-pane",
    "move-window",
    "new-session",
    "new-window",
    "next-layout",
    "next-window",
    "paste-buffer",
    "pipe-pane",
    "previous-layout",
    "previous-window",
    "refresh-client",
    "respawn-pane",
    "respawn-window",
    "rotate-window",
    "rename-session",
    "rename-window",
    "resize-pane",
    "restore-session",
    "resume",
    "run-shell",
    "save-buffer",
    "save-session",
    "select-layout",
    "select-pane",
    "select-window",
    "send-keys",
    "send-prefix",
    "set-buffer",
    "set-cwd",
    "set-environment",
    "set-hook",
    "set-option",
    "show-buffer",
    "show-environment",
    "show-hooks",
    "show-messages",
    "show-options",
    "source-file",
    "split-window",
    "swap-pane",
    "swap-window",
    "switch-client",
    "version",
    "wait-for",
];

/// tmux lets any unambiguous prefix stand for a command name, so `att` is
/// `attach-session` and `splitw` is `split-window`. Several matches is an
/// error rather than a guess.
fn resolve_prefix(name: &str) -> Result<&'static str, String> {
    let hits: Vec<&&str> = COMMANDS.iter().filter(|c| c.starts_with(name)).collect();
    match hits.len() {
        1 => Ok(hits[0]),
        0 => Err(format!("unknown command: {name}")),
        _ => {
            let mut names: Vec<&str> = hits.into_iter().copied().collect();
            names.sort_unstable();
            Err(format!("ambiguous command: {name} (could be {})", names.join(", ")))
        }
    }
}

/// Parse an argv (first word is the command name) into a `Cmd`.
pub fn parse(words: &[String]) -> Result<Cmd, String> {
    let name = words.first().ok_or_else(|| "empty command".to_string())?.as_str();
    let mut a = Args { words, pos: 1 };
    let canonical = match name {
        "new-session" | "new" => "new-session",
        "attach-session" | "attach" | "a" | "at" => "attach-session",
        "detach-client" | "detach" => "detach-client",
        "show-messages" | "showmsgs" => "show-messages",
        "set-environment" | "setenv" => "set-environment",
        "show-environment" | "showenv" => "show-environment",
        "respawn-pane" | "respawnp" => "respawn-pane",
        "respawn-window" | "respawnw" => "respawn-window",
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
        "swap-window" | "swapw" => "swap-window",
        "move-window" | "movew" => "move-window",
        "next-window" | "next" => "next-window",
        "previous-window" | "prev" => "previous-window",
        "last-window" | "last" => "last-window",
        "split-window" | "splitw" => "split-window",
        "kill-pane" | "killp" => "kill-pane",
        "select-pane" | "selectp" => "select-pane",
        "resize-pane" | "resizep" => "resize-pane",
        "swap-pane" | "swapp" => "swap-pane",
        "break-pane" | "breakp" => "break-pane",
        "join-pane" | "joinp" => "join-pane",
        "move-pane" | "movep" => "join-pane",
        "find-window" | "findw" => "find-window",
        "select-layout" | "selectl" => "select-layout",
        "next-layout" | "nextl" => "next-layout",
        "previous-layout" | "prevl" => "previous-layout",
        "rotate-window" | "rotatew" => "rotate-window",
        "refresh-client" | "refresh" => "refresh-client",
        "send-prefix" => "send-prefix",
        "list-commands" | "lscm" => "list-commands",
        "list-clients" | "lsc" => "list-clients",
        "last-pane" | "lastp" => "last-pane",
        "send-keys" | "send" => "send-keys",
        "copy-mode" => "copy-mode",
        "paste-buffer" | "pasteb" => "paste-buffer",
        "set-buffer" | "setb" => "set-buffer",
        "load-buffer" | "loadb" => "load-buffer",
        "save-buffer" | "saveb" => "save-buffer",
        "show-buffer" | "showb" => "show-buffer",
        "delete-buffer" | "deleteb" => "delete-buffer",
        "list-buffers" | "lsb" => "list-buffers",
        "choose-buffer" => "choose-buffer",
        "choose-client" => "choose-client",
        "command-prompt" => "command-prompt",
        "pipe-pane" | "pipep" => "pipe-pane",
        "wait-for" | "wait" => "wait-for",
        "display-menu" | "menu" => "display-menu",
        "display-popup" | "popup" => "display-popup",
        "display-message" | "display" => "display-message",
        "display-panes" | "displayp" => "display-panes",
        "clock-mode" => "clock-mode",
        "if-shell" | "if" => "if-shell",
        "confirm-before" | "confirm" => "confirm-before",
        "bind-key" | "bind" => "bind-key",
        "unbind-key" | "unbind" => "unbind-key",
        "set-option" | "set" => "set-option",
        "show-options" | "show-option" | "show" => "show-options",
        "switch-client" | "switchc" => "switch-client",
        "list-keys" | "lsk" => "list-keys",
        "choose-tree" => "choose-tree",
        "choose-window" => "choose-window",
        "choose-session" => "choose-session",
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
        "set-cwd" | "cwd" => "set-cwd",
        "source-file" | "source" => "source-file",
        "version" | "-V" | "--version" => "version",
        other => resolve_prefix(other)?,
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
            let (mut all, mut target) = (false, None);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-a" => all = true,
                    "-s" => {
                        all = true;
                        target = Some(Target::parse(a.value("-s")?));
                    }
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    "-P" | "-E" => {} // tmux: what to do with the pane afterwards
                    f => return Err(bad_flag(n, f)),
                }
            }
            a.none_left(n)?;
            Cmd::DetachClient { all, target }
        }
        "show-messages" | "showmsgs" => {
            while a.is_flag() {
                a.next();
            }
            a.none_left(n)?;
            Cmd::ShowMessages
        }
        "set-environment" => {
            let mut remove = false;
            while a.is_flag() {
                match a.next().unwrap() {
                    "-r" | "-u" => remove = true,
                    "-g" | "-h" => {}
                    f => return Err(bad_flag(n, f)),
                }
            }
            let name = a.next().ok_or("set-environment: name required")?.to_string();
            let rest = a.rest();
            let value = if rest.is_empty() { None } else { Some(rest.join(" ")) };
            Cmd::SetEnvironment { name, value, remove }
        }
        "show-environment" => {
            while a.is_flag() {
                a.next();
            }
            let name = a.next().map(str::to_string);
            a.none_left(n)?;
            Cmd::ShowEnvironment { name }
        }
        "respawn-pane" | "respawn-window" => {
            let (mut kill, mut target) = (false, None);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-k" => kill = true,
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    "-c" | "-e" => {
                        a.value("-c")?;
                    }
                    f => return Err(bad_flag(n, f)),
                }
            }
            Cmd::RespawnPane { target, kill, argv: a.rest(), window: n == "respawn-window" }
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
        "swap-window" | "move-window" => {
            let (mut src, mut dst) = (None, None);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-s" => src = Some(Target::parse(a.value("-s")?)),
                    "-t" => dst = Some(Target::parse(a.value("-t")?)),
                    "-d" | "-k" | "-a" | "-b" | "-r" => {} // tmux flags without a wmux meaning
                    f => return Err(bad_flag(n, f)),
                }
            }
            a.none_left(n)?;
            Cmd::SwapWindow { src, dst, move_it: n == "move-window" }
        }
        "next-window" | "previous-window" | "last-window" => {
            let (mut target, mut alert) = (None, false);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    "-a" if n != "last-window" => alert = true,
                    f => return Err(bad_flag(n, f)),
                }
            }
            a.none_left(n)?;
            match n {
                "next-window" => Cmd::NextWindow { target, alert },
                "previous-window" => Cmd::PreviousWindow { target, alert },
                _ => Cmd::LastWindow { target },
            }
        }
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
            // -T/-m/-M are a different command underneath: they name or mark
            // a pane rather than moving to one.
            let (mut title, mut mark, mut unmark, mut mark_target) = (None, false, false, None);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-T" => title = Some(a.value("-T")?.to_string()),
                    "-m" => mark = true,
                    "-M" => unmark = true,
                    "-L" => sel = Some(PaneSel::Dir(Dir::Left)),
                    "-R" => sel = Some(PaneSel::Dir(Dir::Right)),
                    "-U" => sel = Some(PaneSel::Dir(Dir::Up)),
                    "-D" => sel = Some(PaneSel::Dir(Dir::Down)),
                    "-l" => sel = Some(PaneSel::Last),
                    "-t" => {
                        // With -m/-M/-T the value is a pane target; on its own
                        // it selects a pane (a direction word or an index).
                        let v = a.value("-t")?;
                        mark_target = Some(Target::parse(v));
                        sel = match v {
                            "next" | ":.+" | "+" => Some(PaneSel::Next),
                            "prev" | "previous" | ":.-" | "-" => Some(PaneSel::Prev),
                            "last" | ":.!" | "!" => Some(PaneSel::Last),
                            other => other.trim_start_matches('%').parse().ok().map(PaneSel::Index),
                        };
                    }
                    f => return Err(bad_flag(n, f)),
                }
            }
            a.none_left(n)?;
            if title.is_some() || mark || unmark {
                return Ok(Cmd::PaneTitle { target: mark_target, title, mark, unmark });
            }
            match sel {
                Some(sel) => Cmd::SelectPane { sel },
                None if mark_target.is_some() => {
                    return Err("select-pane: bad target (use a pane index, next, prev or last)".into());
                }
                None => return Err("select-pane: direction or -t required".into()),
            }
        }
        "resize-pane" => {
            let (mut dir, mut amount, mut zoom, mut target) = (None, 1u16, false, None);
            let (mut width, mut height) = (None, None);
            while a.is_flag() {
                let f = a.next().unwrap();
                match f {
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    "-x" => width = Some(a.value("-x")?.to_string()),
                    "-y" => height = Some(a.value("-y")?.to_string()),
                    "-M" => {} // tmux: resize with the mouse
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
            if dir.is_none() && !zoom && width.is_none() && height.is_none() {
                return Err("resize-pane: direction, -x/-y or -Z required".into());
            }
            Cmd::ResizePane { dir, amount, zoom, target, width, height }
        }
        "swap-pane" => {
            let (mut up, mut target) = (false, None);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-U" => up = true,
                    "-D" => up = false,
                    "-t" | "-s" => target = Some(Target::parse(a.value("-t")?)),
                    f => return Err(bad_flag(n, f)),
                }
            }
            a.none_left(n)?;
            Cmd::SwapPane { up, target }
        }
        "next-layout" | "previous-layout" => {
            let mut target = None;
            while a.is_flag() {
                match a.next().unwrap() {
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    f => return Err(bad_flag(n, f)),
                }
            }
            a.none_left(n)?;
            Cmd::SelectLayout {
                name: None,
                next: n == "next-layout",
                prev: n == "previous-layout",
                spread: false,
                target,
            }
        }
        "rotate-window" => {
            let (mut down, mut target) = (false, None);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-D" => down = true,
                    "-U" => down = false,
                    "-Z" => {}
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    f => return Err(bad_flag(n, f)),
                }
            }
            a.none_left(n)?;
            Cmd::RotateWindow { down, target }
        }
        "refresh-client" => {
            while a.is_flag() {
                // tmux has many flags here; none of them mean anything yet.
                let f = a.next().unwrap();
                if matches!(f, "-U" | "-D" | "-L" | "-R" | "-C" | "-t") {
                    a.value(f)?;
                }
            }
            a.none_left(n)?;
            Cmd::RefreshClient
        }
        "send-prefix" => {
            let mut target = None;
            while a.is_flag() {
                match a.next().unwrap() {
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    "-2" => {}
                    f => return Err(bad_flag(n, f)),
                }
            }
            a.none_left(n)?;
            Cmd::SendPrefix { target }
        }
        "list-commands" => {
            while a.is_flag() {
                a.next();
            }
            a.none_left(n)?;
            Cmd::ListCommands
        }
        "list-clients" => {
            while a.is_flag() {
                let f = a.next().unwrap();
                if matches!(f, "-t" | "-F") {
                    a.value(f)?;
                }
            }
            a.none_left(n)?;
            Cmd::ListClients
        }
        "last-pane" => {
            while a.is_flag() {
                let f = a.next().unwrap();
                if f == "-t" {
                    a.value(f)?;
                }
            }
            a.none_left(n)?;
            Cmd::SelectPane { sel: PaneSel::Last }
        }
        "select-layout" => {
            let (mut next, mut prev, mut spread, mut target) = (false, false, false, None);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-n" => next = true,
                    "-p" => prev = true,
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    "-E" => spread = true,
                    "-o" => {} // tmux: back to the previous layout
                    f => return Err(bad_flag(n, f)),
                }
            }
            let name = a.next().map(str::to_string);
            a.none_left(n)?;
            if name.is_none() && !next && !prev && !spread {
                return Err("select-layout: layout name, -n, -p or -E required".into());
            }
            Cmd::SelectLayout { name, next, prev, spread, target }
        }
        "join-pane" => {
            let (mut src, mut dst, mut horizontal, mut before) = (None, None, false, false);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-h" => horizontal = true,
                    "-v" => horizontal = false,
                    "-b" => before = true,
                    "-s" => src = Some(Target::parse(a.value("-s")?)),
                    "-t" => dst = Some(Target::parse(a.value("-t")?)),
                    "-d" | "-f" => {}
                    "-l" | "-p" => {
                        a.value("-l")?;
                    }
                    f => return Err(bad_flag(n, f)),
                }
            }
            a.none_left(n)?;
            Cmd::JoinPane { src, dst, horizontal, before }
        }
        "find-window" => {
            while a.is_flag() {
                let f = a.next().unwrap();
                if f == "-t" {
                    a.value(f)?;
                }
            }
            let pattern = a.rest().join(" ");
            if pattern.is_empty() {
                return Err("find-window: pattern required".into());
            }
            Cmd::FindWindow { pattern }
        }
        "break-pane" => {
            let mut target = None;
            while a.is_flag() {
                match a.next().unwrap() {
                    "-t" | "-s" => target = Some(Target::parse(a.value("-t")?)),
                    "-d" | "-P" => {} // tmux flags without a wmux meaning
                    f => return Err(bad_flag(n, f)),
                }
            }
            a.none_left(n)?;
            Cmd::BreakPane { target }
        }
        "send-keys" => {
            let (mut target, mut literal, mut copy_cmd) = (None, false, false);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    "-l" => literal = true,
                    "-X" => copy_cmd = true,
                    "-R" | "-M" | "-N" => {}
                    f => return Err(bad_flag(n, f)),
                }
            }
            if copy_cmd {
                let mut rest = a.rest().into_iter();
                let name = rest.next().ok_or("send-keys -X: command name required")?;
                return Ok(Cmd::CopyCommand { target, name, arg: rest.next() });
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
        "paste-buffer" => {
            let (mut name, mut target, mut bracketed) = (None, None, false);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-b" => name = Some(a.value("-b")?.to_string()),
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    "-p" => bracketed = true,
                    "-d" | "-r" => {}
                    "-s" => {
                        a.value("-s")?;
                    }
                    f => return Err(bad_flag(n, f)),
                }
            }
            a.none_left(n)?;
            Cmd::PasteBuffer { name, target, bracketed }
        }
        "set-buffer" | "load-buffer" | "save-buffer" | "show-buffer" | "delete-buffer" => {
            let (mut name, mut append) = (None, false);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-b" => name = Some(a.value("-b")?.to_string()),
                    "-a" => append = true,
                    "-w" | "-n" => {} // tmux: also set the clipboard / new name
                    f => return Err(bad_flag(n, f)),
                }
            }
            match n {
                "set-buffer" => {
                    let data = a.rest().join(" ");
                    Cmd::SetBuffer { name, data, append }
                }
                "load-buffer" => {
                    let path = a.next().ok_or("load-buffer: path required")?.to_string();
                    a.none_left(n)?;
                    Cmd::LoadBuffer { name, path }
                }
                "save-buffer" => {
                    let path = a.next().ok_or("save-buffer: path required")?.to_string();
                    a.none_left(n)?;
                    Cmd::SaveBuffer { name, path, append }
                }
                "show-buffer" => {
                    a.none_left(n)?;
                    Cmd::ShowBuffer { name }
                }
                _ => {
                    a.none_left(n)?;
                    Cmd::DeleteBuffer { name }
                }
            }
        }
        "list-buffers" => {
            while a.is_flag() {
                let f = a.next().unwrap();
                if f == "-F" {
                    a.value(f)?;
                }
            }
            a.none_left(n)?;
            Cmd::ListBuffers
        }
        "choose-buffer" => {
            while a.is_flag() {
                a.next();
            }
            a.none_left(n)?;
            Cmd::ChooseBuffer
        }
        "choose-client" => {
            while a.is_flag() {
                let f = a.next().unwrap();
                if matches!(f, "-t" | "-F" | "-f" | "-O") {
                    a.value(f)?;
                }
            }
            a.none_left(n)?;
            Cmd::ChooseClient
        }
        "display-menu" => {
            let mut title = None;
            while a.is_flag() {
                match a.next().unwrap() {
                    "-T" => title = Some(a.value("-T")?.to_string()),
                    // Placement and the client to show it on: wmux draws the
                    // menu over the window of the client that asked for it.
                    "-x" | "-y" | "-c" | "-t" => {
                        a.value("-x")?;
                    }
                    "-O" => {}
                    f => return Err(bad_flag(n, f)),
                }
            }
            let words = a.rest();
            if words.is_empty() {
                return Err("display-menu: at least one item required".into());
            }
            let mut items = Vec::new();
            let mut i = 0;
            while i < words.len() {
                let name = words[i].clone();
                if name.is_empty() {
                    // A separator takes no key and no command.
                    items.push(MenuItem { name, key: None, cmd: None });
                    i += 1;
                    continue;
                }
                let key = words.get(i + 1).ok_or("display-menu: item needs a key and a command")?;
                let raw = words.get(i + 2).ok_or("display-menu: item needs a command")?;
                let key = if key.is_empty() { None } else { Some(key.clone()) };
                let cmd = if raw.is_empty() { None } else { Some(Box::new(parse(&tokenize(raw)?)?)) };
                items.push(MenuItem { name, key, cmd });
                i += 3;
            }
            Cmd::DisplayMenu { title, items }
        }
        "display-popup" => {
            let (mut close, mut close_on_exit) = (false, false);
            let (mut width, mut height, mut cwd) = (None, None, None);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-C" => close = true,
                    "-E" | "-EE" => close_on_exit = true,
                    "-w" => width = Some(a.value("-w")?.to_string()),
                    "-h" => height = Some(a.value("-h")?.to_string()),
                    "-d" => cwd = Some(a.value("-d")?.to_string()),
                    // Where it sits and whose client it is: wmux centres the
                    // popup on the client that asked for it.
                    "-x" | "-y" | "-c" | "-t" | "-T" | "-s" | "-S" | "-b" | "-e" => {
                        a.value("-x")?;
                    }
                    "-B" => {}
                    f => return Err(bad_flag(n, f)),
                }
            }
            let argv = a.rest();
            Cmd::DisplayPopup { close, close_on_exit, width, height, cwd, argv }
        }
        "pipe-pane" => {
            let (mut target, mut toggle) = (None, false);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-o" => toggle = true,
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    "-O" => {} // pane output into the command: the only direction
                    "-I" => return Err("pipe-pane: -I (command output into the pane) is not supported".into()),
                    f => return Err(bad_flag(n, f)),
                }
            }
            let command = a.rest().join(" ");
            Cmd::PipePane { target, command: if command.is_empty() { None } else { Some(command) }, toggle }
        }
        "wait-for" => {
            let (mut lock, mut unlock, mut signal) = (false, false, false);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-L" => lock = true,
                    "-U" => unlock = true,
                    "-S" => signal = true,
                    f => return Err(bad_flag(n, f)),
                }
            }
            let channel = a.next().ok_or("wait-for: channel required")?.to_string();
            a.none_left(n)?;
            if [lock, unlock, signal].iter().filter(|x| **x).count() > 1 {
                return Err("wait-for: -L, -S and -U are exclusive".into());
            }
            Cmd::WaitFor { channel, lock, unlock, signal }
        }
        "clock-mode" => {
            let mut target = None;
            while a.is_flag() {
                match a.next().unwrap() {
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    f => return Err(bad_flag(n, f)),
                }
            }
            a.none_left(n)?;
            Cmd::ClockMode { target }
        }
        "if-shell" => {
            let mut format = false;
            while a.is_flag() {
                match a.next().unwrap() {
                    "-F" => format = true,
                    "-b" => {} // background: wmux runs it inline either way
                    "-t" => {
                        a.value("-t")?;
                    }
                    f => return Err(bad_flag(n, f)),
                }
            }
            let condition = a.next().ok_or("if-shell: condition required")?.to_string();
            let then_raw = a.next().ok_or("if-shell: command required")?.to_string();
            let else_raw = a.next().map(str::to_string);
            a.none_left(n)?;
            let parse_sub = |s: &str| -> Result<Cmd, String> { parse(&tokenize(s)?) };
            Cmd::IfShell {
                format,
                condition,
                then_cmd: Box::new(parse_sub(&then_raw)?),
                else_cmd: match else_raw {
                    Some(e) => Some(Box::new(parse_sub(&e)?)),
                    None => None,
                },
            }
        }
        "display-panes" => {
            while a.is_flag() {
                match a.next().unwrap() {
                    "-b" | "-N" => {} // tmux: background / no key wait
                    "-d" => {
                        a.value("-d")?;
                    }
                    f => return Err(bad_flag(n, f)),
                }
            }
            a.none_left(n)?;
            Cmd::DisplayPanes
        }
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
            let (mut root, mut repeat) = (false, false);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-n" => root = true,
                    "-r" => repeat = true,
                    "-T" => {
                        root = a.value("-T")? == "root";
                    }
                    f => return Err(bad_flag(n, f)),
                }
            }
            let key = a.next().ok_or("bind-key: key required")?.to_string();
            let rest = a.rest();
            let inner = if rest.len() == 1 { tokenize(&rest[0])? } else { rest };
            Cmd::BindKey { root, key, repeat, cmd: Box::new(parse(&inner)?) }
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
            // -g / -s / -w are accepted and ignored; -a appends, and tmux
            // users write it combined ("set -ag status-right ...").
            let mut append = false;
            while a.is_flag() {
                let flag = a.next().unwrap();
                if flag.contains('a') {
                    append = true;
                }
            }
            let name = a.next().ok_or("set-option: option name required")?.to_string();
            let value = a.rest().join(" ");
            Cmd::SetOption { name, value, append }
        }
        "switch-client" => {
            let (mut next, mut prev, mut last, mut target) = (false, false, false, None);
            while a.is_flag() {
                match a.next().unwrap() {
                    "-n" => next = true,
                    "-p" => prev = true,
                    "-l" => last = true,
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    "-Z" | "-E" | "-r" => {}
                    f => return Err(bad_flag(n, f)),
                }
            }
            a.none_left(n)?;
            Cmd::SwitchClient { next, prev, last, target }
        }
        "list-keys" => {
            a.none_left(n)?;
            Cmd::ListKeys
        }
        "choose-tree" | "choose-window" | "choose-session" => {
            let (mut sessions, mut windows) = (n == "choose-session", n == "choose-window");
            while a.is_flag() {
                // Combined flags as tmux configs write them: -Zw, -Zs.
                let flag = a.next().unwrap();
                if flag.len() < 2 || !flag[1..].chars().all(|c| "swZNGr".contains(c)) {
                    return Err(bad_flag(n, flag));
                }
                for c in flag[1..].chars() {
                    match c {
                        's' => sessions = true,
                        'w' => windows = true,
                        _ => {} // -Z -N -G -r: tmux flags without a wmux meaning
                    }
                }
            }
            a.none_left(n)?;
            Cmd::ChooseTree { sessions, windows }
        }
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
        "list-plugins" => {
            a.none_left(n)?;
            Cmd::ListPlugins
        }
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
        "clear-history" => {
            a.none_left(n)?;
            Cmd::ClearHistory
        }
        "set-cwd" => {
            let mut target = None;
            while a.is_flag() {
                match a.next().unwrap() {
                    "-t" => target = Some(Target::parse(a.value("-t")?)),
                    f => return Err(bad_flag(n, f)),
                }
            }
            let dir = a.next().map(str::to_string);
            a.none_left(n)?;
            Cmd::SetCwd { target, dir }
        }
        "capture-pane" => {
            let (mut target, mut history, mut escapes) = (None, 0usize, false);
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
                    "-e" => escapes = true,
                    "-p" | "-J" => {} // always printed; wrapped lines stay split
                    f => return Err(bad_flag(n, f)),
                }
            }
            a.none_left(n)?;
            Cmd::CapturePane { target, history, escapes }
        }
        "source-file" => {
            let path = a.next().ok_or("source-file: path required")?.to_string();
            a.none_left(n)?;
            Cmd::SourceFile { path }
        }
        "version" => {
            a.none_left(n)?;
            Cmd::Version
        }
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
    fn unambiguous_prefixes_are_command_names() {
        // tmux habits: att, splitw, lsp, neww, ...
        assert!(matches!(p("att"), Cmd::AttachSession { .. }));
        assert!(matches!(p("attach-s"), Cmd::AttachSession { .. }));
        assert!(matches!(p("spl -h"), Cmd::SplitWindow { horizontal: true, .. }));
        assert!(matches!(p("resi -Z"), Cmd::ResizePane { zoom: true, .. }));
        assert!(matches!(p("choose-t"), Cmd::ChooseTree { .. }));
        assert!(matches!(p("swap-p -U"), Cmd::SwapPane { up: true, .. }));
        // The short aliases still win over the prefix rule.
        assert!(matches!(p("ls"), Cmd::ListSessions));
        assert!(matches!(p("new -d"), Cmd::NewSession { detached: true, .. }));
        // Ambiguity is an error, not a guess.
        let e = parse_line("kill").unwrap_err();
        assert!(e.starts_with("ambiguous command: kill (could be kill-pane, kill-server"), "{e}");
        assert!(parse_line("list-s").unwrap_err().starts_with("ambiguous command"));
        assert_eq!(parse_line("nosuchthing").unwrap_err(), "unknown command: nosuchthing");
    }

    #[test]
    fn parse_choose_tree() {
        assert_eq!(p("choose-tree"), Cmd::ChooseTree { sessions: false, windows: false });
        assert_eq!(p("choose-tree -Zw"), Cmd::ChooseTree { sessions: false, windows: true });
        assert_eq!(p("choose-window"), Cmd::ChooseTree { sessions: false, windows: true });
        assert_eq!(p("choose-session"), Cmd::ChooseTree { sessions: true, windows: false });
        assert_eq!(p("choose-tree -s -w").to_string(), "choose-tree -s -w");
        assert!(parse_line("choose-tree -x").is_err());
        assert!(parse_line("choose-tree extra").is_err());
    }

    #[test]
    fn parse_pane_commands() {
        assert_eq!(p("select-pane -L"), Cmd::SelectPane { sel: PaneSel::Dir(Dir::Left) });
        assert_eq!(p("selectp -t next"), Cmd::SelectPane { sel: PaneSel::Next });
        assert_eq!(
            p("resize-pane -R 5"),
            Cmd::ResizePane { dir: Some(Dir::Right), amount: 5, zoom: false, target: None, width: None, height: None }
        );
        assert_eq!(
            p("resizep -Z"),
            Cmd::ResizePane { dir: None, amount: 1, zoom: true, target: None, width: None, height: None }
        );
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
    fn pane_commands_take_a_target() {
        // tmux scripts address a pane from outside the session; every pane
        // command must accept -t, and print it back.
        let t = |s: &str| Some(Target::parse(s));
        assert_eq!(
            p("resize-pane -Z -t work:1"),
            Cmd::ResizePane { dir: None, amount: 1, zoom: true, target: t("work:1"), width: None, height: None }
        );
        assert_eq!(
            p("resizep -L 5 -t work:1.0"),
            Cmd::ResizePane {
                dir: Some(Dir::Left),
                amount: 5,
                zoom: false,
                target: t("work:1.0"),
                width: None,
                height: None
            }
        );
        assert_eq!(p("swap-pane -U -t work"), Cmd::SwapPane { up: true, target: t("work") });
        assert_eq!(p("break-pane -t work:2"), Cmd::BreakPane { target: t("work:2") });
        assert_eq!(p("break-pane"), Cmd::BreakPane { target: None });
        assert_eq!(p("resize-pane -Z -t work:1").to_string(), "resize-pane -Z -t work:1");
        assert_eq!(p("swap-pane -D -t w:0.1").to_string(), "swap-pane -D -t w:0.1");
        assert_eq!(p("break-pane -t work:2").to_string(), "break-pane -t work:2");
        // Unknown flags are still rejected rather than silently dropped.
        assert!(parse_line("break-pane -x").is_err());
        assert!(parse_line("break-pane extra").is_err());
        assert!(parse_line("swap-pane -t").is_err());
    }

    #[test]
    fn window_navigation_takes_a_session_and_rejects_junk() {
        assert_eq!(p("next-window"), Cmd::NextWindow { target: None, alert: false });
        assert_eq!(p("next -t work"), Cmd::NextWindow { target: Some(Target::parse("work")), alert: false });
        assert_eq!(p("prev -t work"), Cmd::PreviousWindow { target: Some(Target::parse("work")), alert: false });
        assert_eq!(p("next-window -a"), Cmd::NextWindow { target: None, alert: true });
        assert_eq!(p("previous-window -a").to_string(), "previous-window -a");
        assert!(parse_line("last-window -a").is_err(), "last-window has no alert search");
        assert_eq!(p("last -t work"), Cmd::LastWindow { target: Some(Target::parse("work")) });
        assert_eq!(p("next-window -t work").to_string(), "next-window -t work");
        // Commands that take nothing say so instead of ignoring the argument.
        for junk in ["next-window -x", "paste-buffer -x", "list-keys extra", "version -v", "clear-history now"] {
            assert!(parse_line(junk).is_err(), "{junk} should be rejected");
        }
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
                repeat: false,
                cmd: Box::new(Cmd::SelectPane { sel: PaneSel::Dir(Dir::Left) })
            }
        );
        assert_eq!(
            p("bind h \"select-pane -L\""),
            Cmd::BindKey {
                root: false,
                key: "h".into(),
                repeat: false,
                cmd: Box::new(Cmd::SelectPane { sel: PaneSel::Dir(Dir::Left) })
            }
        );
        assert_eq!(
            p("bind -r C-h resize-pane -L 5"),
            Cmd::BindKey {
                root: false,
                key: "C-h".into(),
                repeat: true,
                cmd: Box::new(Cmd::ResizePane {
                    dir: Some(Dir::Left),
                    amount: 5,
                    zoom: false,
                    target: None,
                    width: None,
                    height: None
                })
            }
        );
        assert_eq!(p("bind -r C-h resize-pane -L 5").to_string(), "bind-key -r C-h resize-pane -L 5");
        assert_eq!(
            p("set -g prefix C-a"),
            Cmd::SetOption { name: "prefix".into(), value: "C-a".into(), append: false }
        );
        assert_eq!(
            p("set -ag status-right \" | x\""),
            Cmd::SetOption { name: "status-right".into(), value: " | x".into(), append: true }
        );
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
            "set-cwd",
            "set-cwd -t w:0.1 \"C:\\my dir\"",
            "confirm-before -p \"kill? (y/n)\" \"kill-window\"",
            "bind-key -n M-h select-pane -L",
            "set-option prefix C-a",
            "display-message \"hello world\"",
            "select-window -t :3",
            "rename-window \"my win\"",
            "copy-mode -u",
            "command-prompt -p (rename-window) -I \"#W\" \"rename-window -- %%\"",
            "capture-pane -p -e -t w:1",
            "select-layout -E -t w:1",
            "choose-client",
            "pipe-pane -o -t w:1 \"cat > log\"",
            "pipe-pane",
            "wait-for -S done",
            "wait-for -L lock",
            "wait-for chan",
            "display-popup -E -w 50% -h 20 -d C:\\src pwsh.exe",
            "display-popup -C",
            "display-menu -T pane Split h \"split-window -h\" \"\" Kill x kill-pane",
            "next-window -a",
            "previous-window -a",
        ];
        for c in cmds {
            let parsed = p(c);
            let shown = parsed.to_string();
            assert_eq!(p(&shown), parsed, "roundtrip of {c:?} via {shown:?}");
        }
    }

    #[test]
    fn parse_menus_pipes_and_waiting() {
        // A menu is triples of name/key/command; an empty name on its own is
        // a separator, as in tmux.
        let Cmd::DisplayMenu { title, items } =
            p("display-menu -T \"pane #P\" Split h \"split-window -h\" \"\" Kill x kill-pane")
        else {
            panic!("not a menu")
        };
        assert_eq!(title.as_deref(), Some("pane #P"));
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].name, "Split");
        assert_eq!(items[0].key.as_deref(), Some("h"));
        assert_eq!(
            items[0].cmd.as_deref(),
            Some(&Cmd::SplitWindow {
                horizontal: true,
                cwd: None,
                target: None,
                argv: Vec::new(),
                detached: false,
                before: false,
                full: false,
            })
        );
        assert!(items[1].cmd.is_none() && items[1].name.is_empty(), "the separator: {:?}", items[1]);
        assert_eq!(items[2].name, "Kill");
        // A half-written item is an error, not a silently dropped entry.
        assert!(parse_line("display-menu Split h").is_err());
        assert!(parse_line("display-menu").is_err());
        // The nested command is parsed now, not when the key is pressed.
        assert!(parse_line("display-menu Bad k frobnicate").is_err());

        assert_eq!(
            p("pipe-pane -o -t x cat"),
            Cmd::PipePane { target: Some(Target::parse("x")), command: Some("cat".into()), toggle: true }
        );
        assert_eq!(p("pipe-pane"), Cmd::PipePane { target: None, command: None, toggle: false });
        // Writing into a pane is not supported and says so.
        assert!(parse_line("pipe-pane -I cat").unwrap_err().contains("not supported"));

        assert_eq!(p("wait-for -S x"), Cmd::WaitFor { channel: "x".into(), lock: false, unlock: false, signal: true });
        assert!(parse_line("wait-for").is_err());
        assert!(parse_line("wait-for -L -U x").unwrap_err().contains("exclusive"));
        assert!(parse_line("wait-for a b").is_err());

        // Prefixes and aliases reach the new commands.
        assert!(matches!(p("menu Kill x kill-pane"), Cmd::DisplayMenu { .. }));
        assert!(matches!(p("popup -E pwsh.exe"), Cmd::DisplayPopup { close_on_exit: true, .. }));
        assert!(matches!(p("pipep cat"), Cmd::PipePane { .. }));
        assert!(matches!(p("wait x"), Cmd::WaitFor { .. }));
        assert!(matches!(p("choose-c"), Cmd::ChooseClient));
        // "display" alone is the message command, as in tmux.
        assert!(matches!(p("display hi"), Cmd::DisplayMessage { .. }));
        assert!(parse_line("displ hi").unwrap_err().contains("ambiguous"));
    }

    #[test]
    fn unknown_command() {
        assert!(parse_line("frobnicate").is_err());
        assert_eq!(parse_line("").unwrap(), None);
    }
}
