//! A pane: a ConPTY-backed process plus an in-memory terminal (vt100).

use super::layout::PaneId;
use anyhow::{Context, Result};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::sync::mpsc::Sender;

/// Events a pane reports back to the server loop. The generation tells a
/// respawned pane's output and exit apart from its predecessor's, whose
/// threads may still be finishing.
pub enum PaneEvent {
    Output(PaneId, u32, Vec<u8>),
    Exit(PaneId, u32, u32),
}

/// vt100 callbacks: collects terminal replies ConPTY expects from a real
/// terminal, the window title and bells.
#[derive(Default)]
pub struct Callbacks {
    pub responses: Vec<u8>,
    pub title: Option<String>,
    pub bell: bool,
    /// Working directory the shell announced (OSC 7 / OSC 9;9), as a Windows path.
    pub cwd: Option<String>,
}

/// Turn a shell-announced directory into a Windows path usable as a process
/// working directory: `file:///C:/x`, `file://host/C:/x`, `C:\x`, `/mnt/c/x`
/// (WSL) all become `C:\x`; other Linux paths are not usable and yield None.
pub fn windows_path_from_announced(raw: &str) -> Option<String> {
    let mut s = raw.trim().to_string();
    if let Some(rest) = s.strip_prefix("file://") {
        // file://host/C:/x or file:///C:/x
        let path = &rest[rest.find('/')?..];
        s = percent_decode(path);
    }
    // /C:/x  or  /c/x (some shells) -> C:/x
    if s.len() >= 3 && s.as_bytes()[0] == b'/' && s.as_bytes()[2] == b':' {
        s = s[1..].to_string();
    }
    if let Some(rest) = s.strip_prefix("/mnt/")
        && !rest.is_empty()
        && rest.as_bytes()[0].is_ascii_alphabetic()
        && (rest.len() == 1 || rest.as_bytes()[1] == b'/')
    {
        let drive = rest.as_bytes()[0].to_ascii_uppercase() as char;
        s = format!("{drive}:{}", &rest[1..]);
    }
    if s.len() >= 2 && s.as_bytes()[1] == b':' && s.as_bytes()[0].is_ascii_alphabetic() {
        let mut p = s.replace('/', "\\");
        if p.len() == 2 {
            p.push('\\');
        }
        return Some(p);
    }
    None
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl vt100::Callbacks for Callbacks {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.bell = true;
    }
    fn visual_bell(&mut self, _: &mut vt100::Screen) {
        self.bell = true;
    }
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.title = Some(String::from_utf8_lossy(title).into_owned());
    }
    fn set_window_icon_name(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        if self.title.is_none() {
            self.title = Some(String::from_utf8_lossy(title).into_owned());
        }
    }
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        _i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        match (i1, c) {
            // DSR: cursor position report. ConPTY (with INHERIT_CURSOR) asks
            // for this at startup and waits for the answer.
            (None, 'n') if params.first().is_some_and(|p| p.first() == Some(&6)) => {
                let (row, col) = screen.cursor_position();
                self.responses.extend_from_slice(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
            }
            (None, 'n') if params.first().is_some_and(|p| p.first() == Some(&5)) => {
                self.responses.extend_from_slice(b"\x1b[0n");
            }
            // Primary device attributes: claim a VT220-class terminal.
            (None, 'c') => self.responses.extend_from_slice(b"\x1b[?62;22c"),
            (Some(b'>'), 'c') => self.responses.extend_from_slice(b"\x1b[>0;10;1c"),
            _ => {}
        }
    }
    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        // OSC 7 ; file://host/path   (bash/zsh/fish shell integration)
        // OSC 9 ; 9 ; path           (ConEmu / Windows Terminal "current directory")
        let raw = match params {
            [b"7", p] => Some(*p),
            [b"9", b"9", p] => Some(*p),
            _ => None,
        };
        if let Some(raw) = raw
            && let Some(p) = windows_path_from_announced(&String::from_utf8_lossy(raw))
        {
            self.cwd = Some(p);
        }
    }
}

pub struct CopyMode {
    /// Lines of scrollback shown above the top of the screen.
    pub offset: usize,
    pub cx: u16,
    pub cy: u16,
    /// Selection anchor as (absolute line, column).
    pub anchor: Option<(usize, u16)>,
    /// A mouse drag selection is in progress.
    pub dragging: bool,
    /// Last `/` or `?` pattern, repeated by `n` and `N`.
    pub search: Option<String>,
    /// Direction of that search: `/` is towards older lines.
    pub search_back: bool,
    /// Digits typed before a motion (`3j`), vi style.
    pub count: Option<usize>,
    /// `C-v`: the selection is a rectangle, not a run of lines.
    pub rect: bool,
}

pub struct Pane {
    pub id: PaneId,
    /// Bumped by `respawn`; events from an older generation are ignored.
    pub generation: u32,
    pub parser: vt100::Parser<Callbacks>,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// Kill-on-close job holding the shell and everything it started, so a
    /// dead pane never leaves orphans (tmux's SIGHUP-the-process-group).
    job: Option<crate::winsec::KillOnCloseJob>,
    pub title: String,
    pub command: String,
    /// The command line this pane was started with (for save/restore).
    pub argv: Vec<String>,
    pub cwd: Option<String>,
    pub exit_code: Option<u32>,
    /// `clock-mode`: a clock is drawn over this pane until a key arrives.
    pub clock: bool,
    pub copy: Option<CopyMode>,
    pub bell: bool,
    pub cols: u16,
    pub rows: u16,
    /// `pipe-pane`: everything this pane writes is copied here as well.
    pub pipe: Option<Pipe>,
    /// `record`: an asciinema file being written from this pane's output.
    pub recorder: Option<Recorder>,
    /// Process id of the pane's program (`#{pane_pid}`).
    pub pid: Option<u32>,
    /// When the current program was started (`jobs`, uptime).
    pub spawned_at: std::time::Instant,
}

/// An asciinema v2 recording in progress: a header line, then one JSON
/// event per output chunk or resize, stamped with seconds since the start.
/// Writing goes through a thread like `Pipe`, so a slow disk never stalls
/// the server; dropping the recorder closes the file.
pub struct Recorder {
    pub path: String,
    started: std::time::Instant,
    tx: std::sync::mpsc::SyncSender<String>,
}

impl Recorder {
    fn event(&mut self, kind: &str, data: &str) {
        let t = self.started.elapsed().as_secs_f64();
        let line =
            format!("[{t:.6}, {}, {}]\n", serde_json::to_string(kind).unwrap(), serde_json::to_string(data).unwrap());
        // A recording that cannot keep up loses events rather than output.
        let _ = self.tx.try_send(line);
    }
}

/// A running `pipe-pane` command. The bytes go through a bounded channel to a
/// writer thread, so a command that stops reading can never wedge the server;
/// dropping the `Pipe` closes the command's standard input.
pub struct Pipe {
    pub command: String,
    tx: std::sync::mpsc::SyncSender<Vec<u8>>,
    /// Output dropped because the command was not keeping up.
    pub dropped: u64,
}

impl Pane {
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        id: PaneId,
        argv: &[String],
        cwd: Option<&str>,
        cols: u16,
        rows: u16,
        history: usize,
        env: &[(String, String)],
        tx: Sender<PaneEvent>,
    ) -> Result<Pane> {
        Pane::spawn_gen(id, 0, argv, cwd, cols, rows, history, env, tx)
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_gen(
        id: PaneId,
        generation: u32,
        argv: &[String],
        cwd: Option<&str>,
        cols: u16,
        rows: u16,
        history: usize,
        env: &[(String, String)],
        tx: Sender<PaneEvent>,
    ) -> Result<Pane> {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let pty = native_pty_system();
        let pair =
            pty.openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }).context("CreatePseudoConsole")?;
        let mut cmd = CommandBuilder::new(&argv[0]);
        cmd.args(&argv[1..]);
        if let Some(d) = cwd
            && std::path::Path::new(d).is_dir()
        {
            cmd.cwd(d);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = pair.slave.spawn_command(cmd).with_context(|| format!("spawn {:?}", argv))?;
        let pid = child.process_id();
        let killer = child.clone_killer();
        drop(pair.slave);
        let job = match crate::winsec::KillOnCloseJob::new() {
            Ok(j) => match child.as_raw_handle() {
                // The handle comes straight from CreateProcessW inside portable-pty.
                Some(h) => match unsafe { j.assign(h as _) } {
                    Ok(()) => Some(j),
                    Err(e) => {
                        log::warn!("pane {id}: job assign failed: {e}");
                        None
                    }
                },
                None => None,
            },
            Err(e) => {
                log::warn!("pane {id}: no job object: {e}");
                None
            }
        };
        let mut reader = pair.master.try_clone_reader().context("pty reader")?;
        let writer = pair.master.take_writer().context("pty writer")?;

        let tx_out = tx.clone();
        std::thread::Builder::new()
            .name(format!("pane-{id}-read"))
            .spawn(move || {
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if tx_out.send(PaneEvent::Output(id, generation, buf[..n].to_vec())).is_err() {
                                break;
                            }
                        }
                    }
                }
            })
            .context("spawn reader thread")?;
        std::thread::Builder::new()
            .name(format!("pane-{id}-wait"))
            .spawn(move || {
                let code = child.wait().map(|s| s.exit_code()).unwrap_or(1);
                let _ = tx.send(PaneEvent::Exit(id, generation, code));
            })
            .context("spawn waiter thread")?;

        Ok(Pane {
            id,
            generation,
            parser: vt100::Parser::new_with_callbacks(rows, cols, history, Callbacks::default()),
            master: pair.master,
            writer,
            killer,
            job,
            title: String::new(),
            command: std::path::Path::new(&argv[0])
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| argv[0].clone()),
            argv: argv.to_vec(),
            cwd: cwd.map(str::to_string),
            exit_code: None,
            clock: false,
            copy: None,
            bell: false,
            cols,
            rows,
            pipe: None,
            recorder: None,
            pid,
            spawned_at: std::time::Instant::now(),
        })
    }

    /// Start recording this pane's output to `path` as asciinema v2,
    /// replacing any recording in progress.
    pub fn record_to(&mut self, path: &str) -> Result<()> {
        use std::io::Write as _;
        self.recorder = None;
        let mut file = std::io::BufWriter::new(
            std::fs::File::create(path).with_context(|| format!("record: cannot create {path}"))?,
        );
        let header = serde_json::json!({
            "version": 2,
            "width": self.cols,
            "height": self.rows,
            "timestamp": chrono::Local::now().timestamp(),
            "env": { "TERM": "xterm-256color", "SHELL": self.command },
            "title": self.display_title(),
        });
        writeln!(file, "{header}").context("record: write header")?;
        let (tx, rx) = std::sync::mpsc::sync_channel::<String>(1024);
        std::thread::Builder::new()
            .name(format!("pane-{}-record", self.id))
            .spawn(move || {
                for line in rx {
                    if file.write_all(line.as_bytes()).is_err() {
                        break;
                    }
                }
                let _ = file.flush();
            })
            .context("record: spawn writer thread")?;
        self.recorder = Some(Recorder { path: path.to_string(), started: std::time::Instant::now(), tx });
        Ok(())
    }

    /// Output for the recording, if one is running.
    pub fn record_write(&mut self, bytes: &[u8]) {
        if let Some(r) = self.recorder.as_mut() {
            r.event("o", &String::from_utf8_lossy(bytes));
        }
    }

    /// Start a `pipe-pane` command for this pane, replacing any running one.
    /// The old pipe stops first, so a failed start never leaves the previous
    /// command quietly running instead of the one that was asked for.
    pub fn pipe_to(&mut self, command: &str, env: &[(String, String)]) -> Result<()> {
        use std::process::{Command, Stdio};
        self.pipe = None;
        let (exe, args): (&str, Vec<String>) = match crate::config::which("pwsh.exe") {
            Some(_) => ("pwsh.exe", vec!["-NoLogo".into(), "-NoProfile".into(), "-Command".into(), command.into()]),
            None => match crate::config::which("powershell.exe") {
                Some(_) => {
                    ("powershell.exe", vec!["-NoLogo".into(), "-NoProfile".into(), "-Command".into(), command.into()])
                }
                None => ("cmd.exe", vec!["/d".into(), "/c".into(), command.into()]),
            },
        };
        let mut c = Command::new(exe);
        c.args(&args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
        for (k, v) in env {
            c.env(k, v);
        }
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: the server has no console
        let mut child = c.spawn().with_context(|| format!("pipe-pane: {exe}"))?;
        let mut stdin = child.stdin.take().context("pipe-pane: no stdin")?;
        // Bounded: a command that stops reading costs at most this much
        // memory (each chunk is one read from the pty, up to 64 KiB) before
        // output starts being dropped, which is reported.
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(64);
        let id = self.id;
        std::thread::Builder::new()
            .name(format!("pane-{id}-pipe"))
            .spawn(move || {
                for b in rx {
                    if stdin.write_all(&b).is_err() {
                        break;
                    }
                }
                drop(stdin); // the command sees end of input and can finish
                let _ = child.wait();
            })
            .context("pipe-pane: spawn writer thread")?;
        self.pipe = Some(Pipe { command: command.to_string(), tx, dropped: 0 });
        Ok(())
    }

    /// Copy pane output into the `pipe-pane` command, if one is running.
    /// A command that has stopped reading loses bytes instead of blocking the
    /// server, and one that has exited ends the pipe. Returns the command the
    /// first time output is lost, so the loss is reported rather than silent.
    pub fn pipe_write(&mut self, bytes: &[u8]) -> Option<String> {
        let p = self.pipe.as_mut()?;
        match p.tx.try_send(bytes.to_vec()) {
            Ok(()) => None,
            Err(std::sync::mpsc::TrySendError::Full(_)) => {
                let first = p.dropped == 0;
                p.dropped += bytes.len() as u64;
                first.then(|| p.command.clone())
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                self.pipe = None;
                None
            }
        }
    }

    /// Start the pane's command again in place, keeping the pane id, its
    /// position in the layout and its size (tmux `respawn-pane`). The old
    /// process and its children are killed first.
    pub fn respawn(
        &mut self,
        argv: Option<&[String]>,
        history: usize,
        env: &[(String, String)],
        tx: Sender<PaneEvent>,
    ) -> Result<()> {
        let argv = argv.map(|a| a.to_vec()).unwrap_or_else(|| self.argv.clone());
        if argv.is_empty() {
            anyhow::bail!("pane has no command to respawn");
        }
        let fresh = Pane::spawn_gen(
            self.id,
            self.generation.wrapping_add(1),
            &argv,
            self.cwd.as_deref(),
            self.cols,
            self.rows,
            history,
            env,
            tx,
        )?;
        // Putting the new pane in place drops the old one, whose Drop closes
        // the job object and takes the old process tree with it.
        let old = std::mem::replace(self, fresh);
        drop(old);
        Ok(())
    }

    /// Feed process output into the terminal model; answer any queries.
    pub fn process_output(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
        let cb = self.parser.callbacks_mut();
        if let Some(t) = cb.title.take() {
            self.title = t;
        }
        if let Some(d) = cb.cwd.take() {
            self.cwd = Some(d);
        }
        if cb.bell {
            cb.bell = false;
            self.bell = true;
        }
        if !cb.responses.is_empty() {
            let resp = std::mem::take(&mut cb.responses);
            let _ = self.writer.write_all(&resp);
        }
    }

    pub fn write_input(&mut self, bytes: &[u8]) {
        if self.exit_code.is_some() {
            return;
        }
        if let Err(e) = self.writer.write_all(bytes) {
            log::warn!("pane {} write failed: {e}", self.id);
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        self.parser.screen_mut().set_size(rows, cols);
        if let Some(r) = self.recorder.as_mut() {
            r.event("r", &format!("{cols}x{rows}"));
        }
        if let Err(e) = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }) {
            log::warn!("pane {} resize failed: {e}", self.id);
        }
        if let Some(c) = &mut self.copy {
            c.cx = c.cx.min(cols - 1);
            c.cy = c.cy.min(rows - 1);
        }
    }

    /// Kill the shell and its whole process tree.
    pub fn kill(&mut self) {
        // Dropping the job terminates every process in it; the killer covers
        // the (unlikely) case where the job could not be created.
        self.job = None;
        let _ = self.killer.kill();
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Total lines of scrollback currently available.
    pub fn scrollback_len(&mut self) -> usize {
        let s = self.parser.screen_mut();
        let cur = s.scrollback();
        s.set_scrollback(usize::MAX);
        let max = s.scrollback();
        s.set_scrollback(cur);
        max
    }

    /// Text of the absolute line `abs` (0 = oldest scrollback line), joined
    /// across soft-wrapped rows is *not* done here; one visual row per call.
    pub fn line_text(&mut self, abs: usize) -> (String, bool) {
        let max = self.scrollback_len();
        let rows = self.rows as usize;
        let cols = self.cols;
        // Choose an offset so that `abs` is visible: offset = max - (abs - row).
        let (offset, row) = if abs < max { (max - abs, 0usize) } else { (0, abs - max) };
        if row >= rows {
            return (String::new(), false);
        }
        let s = self.parser.screen_mut();
        let cur = s.scrollback();
        s.set_scrollback(offset);
        let text = s.rows(0, cols).nth(row).unwrap_or_default();
        let wrapped = s.row_wrapped(row as u16);
        s.set_scrollback(cur);
        (text, wrapped)
    }

    /// Find `needle` in everything this pane has on screen and in its
    /// scrollback, newest line first, at most `max` hits.
    ///
    /// Written as one pass rather than a loop over `line_text` because that
    /// recomputes the scrollback length for every line, which turns a search
    /// over a few thousand lines into a few thousand full scans.
    pub fn search(&mut self, needle: &str, max: usize, case_sensitive: bool) -> Vec<(usize, String)> {
        let mut hits = Vec::new();
        if needle.is_empty() || max == 0 {
            return hits;
        }
        let want = if case_sensitive { needle.to_string() } else { needle.to_lowercase() };
        let total = self.scrollback_len();
        let rows = self.rows as usize;
        let cols = self.cols;
        let s = self.parser.screen_mut();
        let keep = s.scrollback();
        // `abs` counts from the oldest scrolled-off line; walk from the
        // newest so the first hits reported are the ones just printed.
        for abs in (0..total + rows).rev() {
            let (offset, row) = if abs < total { (total - abs, 0usize) } else { (0, abs - total) };
            s.set_scrollback(offset);
            let Some(text) = s.rows(0, cols).nth(row) else { continue };
            let hay = if case_sensitive { text.clone() } else { text.to_lowercase() };
            if hay.contains(&want) {
                hits.push((abs, text.trim_end().to_string()));
                if hits.len() >= max {
                    break;
                }
            }
        }
        s.set_scrollback(keep);
        hits
    }

    /// Like `line_text`, but keeping the colours and attributes as escape
    /// sequences (`capture-pane -e`).
    pub fn line_escapes(&mut self, abs: usize) -> String {
        let max = self.scrollback_len();
        let rows = self.rows as usize;
        let cols = self.cols;
        let (offset, row) = if abs < max { (max - abs, 0usize) } else { (0, abs - max) };
        if row >= rows {
            return String::new();
        }
        let s = self.parser.screen_mut();
        let cur = s.scrollback();
        s.set_scrollback(offset);
        let bytes = s.rows_formatted(0, cols).nth(row).unwrap_or_default();
        s.set_scrollback(cur);
        let mut text = String::from_utf8_lossy(&bytes).into_owned();
        if text.contains('\x1b') {
            text.push_str("\x1b[0m"); // never leak a colour into the next line
        }
        text
    }

    /// Display name for the status line.
    pub fn display_title(&self) -> &str {
        if self.title.is_empty() { &self.command } else { &self.title }
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        // Field drop order would close the ConPTY before the job; kill first so
        // ClosePseudoConsole never waits on a live process tree.
        self.job = None;
        if self.exit_code.is_none() {
            let _ = self.killer.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::time::{Duration, Instant};

    fn pump(
        pane: &mut Pane,
        rx: &std::sync::mpsc::Receiver<PaneEvent>,
        until: impl Fn(&Pane) -> bool,
        timeout: Duration,
    ) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if until(pane) {
                return true;
            }
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(PaneEvent::Output(_, _, b)) => pane.process_output(&b),
                Ok(PaneEvent::Exit(_, _, c)) => pane.exit_code = Some(c),
                Err(_) => {}
            }
        }
        until(pane)
    }

    #[test]
    fn cmd_echo_roundtrip() {
        let (tx, rx) = channel();
        let argv = vec!["cmd.exe".to_string(), "/q".into(), "/k".into(), "prompt $g".into()];
        let mut p = Pane::spawn(1, &argv, None, 80, 24, 100, &[], tx).unwrap();
        assert!(pump(&mut p, &rx, |p| p.screen().contents().contains('>'), Duration::from_secs(10)), "no prompt");
        p.write_input(b"echo hello-wmux\r");
        assert!(
            pump(&mut p, &rx, |p| p.screen().contents().matches("hello-wmux").count() >= 2, Duration::from_secs(10)),
            "no echo: {:?}",
            p.screen().contents()
        );
        p.write_input(b"exit\r");
        assert!(pump(&mut p, &rx, |p| p.exit_code.is_some(), Duration::from_secs(10)), "no exit");
        assert_eq!(p.exit_code, Some(0));
    }

    #[test]
    fn resize_reaches_child() {
        let (tx, rx) = channel();
        let argv = vec!["cmd.exe".to_string(), "/q".into(), "/k".into(), "prompt $g".into()];
        let mut p = Pane::spawn(2, &argv, None, 80, 24, 100, &[], tx).unwrap();
        assert!(pump(&mut p, &rx, |p| p.screen().contents().contains('>'), Duration::from_secs(10)));
        p.resize(100, 30);
        assert_eq!(p.screen().size(), (30, 100));
        p.write_input(b"mode con\r");
        // `mode con` output is localized; check the numbers at line ends.
        let has = |p: &Pane, n: &str| p.screen().contents().lines().any(|l| l.trim_end().ends_with(n));
        assert!(
            pump(&mut p, &rx, |p| has(p, "100") && has(p, "30"), Duration::from_secs(10)),
            "{:?}",
            p.screen().contents()
        );
        p.kill();
        assert!(pump(&mut p, &rx, |p| p.exit_code.is_some(), Duration::from_secs(10)));
    }

    #[test]
    fn kill_takes_grandchildren_down() {
        let (tx, rx) = channel();
        // The shell starts ping (a grandchild of wmux) and waits for it.
        let argv = vec!["cmd.exe".to_string(), "/q".into(), "/k".into(), "prompt $g".into()];
        let mut p = Pane::spawn(9, &argv, None, 80, 24, 100, &[], tx).unwrap();
        assert!(pump(&mut p, &rx, |p| p.screen().contents().contains('>'), Duration::from_secs(10)));
        let marker = std::env::temp_dir().join(format!("wmux-job-marker-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        p.write_input(format!("ping -n 4 127.0.0.1 > nul & echo done > \"{}\"\r", marker.display()).as_bytes());
        std::thread::sleep(Duration::from_millis(500));
        p.kill();
        assert!(pump(&mut p, &rx, |p| p.exit_code.is_some(), Duration::from_secs(10)), "shell did not die");
        // ping would finish after ~3s and write the marker; it must not.
        std::thread::sleep(Duration::from_secs(4));
        assert!(!marker.exists(), "grandchild survived the pane kill");
    }

    /// A wide character straddling the new right edge after a shrink used to
    /// leave a half-wide cell in the last column; the next erase then indexed
    /// past the row (vt100 0.16.2 `Row::clear_wide`). Seen live with a CJK
    /// IME in a 50-column pane.
    #[test]
    fn shrink_through_wide_char_then_erase_does_not_panic() {
        let mut p = vt100::Parser::new(5, 100, 0);
        let line = format!("{}中x", "a".repeat(49)); // 中 occupies cols 49-50
        p.process(line.as_bytes());
        p.screen_mut().set_size(5, 50);
        // Cursor to the last column, erase to end of line, then overwrite.
        p.process(b"\x1b[1;50H\x1b[K\x1b[1;49H\x1b[2X\x1b[1;1H\x1b[P");
        let row: String = p.screen().rows(0, 50).next().unwrap();
        assert!(!row.contains('中'), "{row:?}");
        // The same through a Pane resize (what a split does).
        let (tx, _rx) = channel();
        let argv = vec!["cmd.exe".to_string(), "/c".into(), "exit".into()];
        let mut pane = Pane::spawn(11, &argv, None, 100, 5, 10, &[], tx).unwrap();
        pane.parser.process(line.as_bytes());
        pane.resize(50, 5);
        pane.parser.process(b"\x1b[1;50H\x1b[K");
        assert_eq!(pane.screen().size(), (5, 50));
    }

    #[test]
    fn announced_directories_become_windows_paths() {
        let w = windows_path_from_announced;
        assert_eq!(w("file:///C:/Users/x").as_deref(), Some("C:\\Users\\x"));
        assert_eq!(w("file://BOX/C:/Users/x%20y").as_deref(), Some("C:\\Users\\x y"));
        assert_eq!(w("C:\\src").as_deref(), Some("C:\\src"));
        assert_eq!(w("C:/src/a").as_deref(), Some("C:\\src\\a"));
        assert_eq!(w("D:").as_deref(), Some("D:\\"));
        assert_eq!(w("/mnt/c/Users/x").as_deref(), Some("C:\\Users\\x"));
        assert_eq!(w("/mnt/d").as_deref(), Some("D:\\"));
        assert_eq!(w("file://HOST/mnt/c/x").as_deref(), Some("C:\\x"));
        assert_eq!(w("/home/user"), None);
        assert_eq!(w("/mnt/wsl/x"), None);
        assert_eq!(w(""), None);
        assert_eq!(w("file://"), None);
    }

    #[test]
    fn osc_cwd_updates_the_pane() {
        let (tx, _rx) = channel();
        let argv = vec!["cmd.exe".to_string(), "/c".into(), "exit".into()];
        let mut p = Pane::spawn(12, &argv, Some("C:\\"), 20, 5, 10, &[], tx).unwrap();
        assert_eq!(p.cwd.as_deref(), Some("C:\\"));
        p.process_output(b"\x1b]9;9;C:\\Users\x07");
        assert_eq!(p.cwd.as_deref(), Some("C:\\Users"));
        p.process_output(b"\x1b]7;file://HOST/mnt/c/Windows\x1b\\");
        assert_eq!(p.cwd.as_deref(), Some("C:\\Windows"));
        // A Linux-only path cannot be a Windows working directory: keep the last one.
        p.process_output(b"\x1b]7;file://HOST/home/user\x07");
        assert_eq!(p.cwd.as_deref(), Some("C:\\Windows"));
    }

    #[test]
    fn dsr_is_answered() {
        let (tx, _rx) = channel();
        let argv = vec!["cmd.exe".to_string(), "/c".into(), "exit".into()];
        let mut p = Pane::spawn(3, &argv, None, 20, 5, 10, &[], tx).unwrap();
        p.parser.process(b"abc\x1b[6n");
        assert_eq!(p.parser.callbacks().responses, b"\x1b[1;4R");
        p.parser.process(b"\x1b[c");
        assert!(p.parser.callbacks().responses.ends_with(b"\x1b[?62;22c"));
        p.parser.process(b"\x1b]0;My Title\x07\x07");
        p.process_output(b"");
        assert_eq!(p.title, "My Title");
        assert!(p.bell);
    }

    #[test]
    fn scrollback_lines() {
        let (tx, _rx) = channel();
        let argv = vec!["cmd.exe".to_string(), "/c".into(), "exit".into()];
        let mut p = Pane::spawn(4, &argv, None, 10, 3, 100, &[], tx).unwrap();
        for i in 0..10 {
            p.parser.process(format!("line{i}\r\n").as_bytes());
        }
        // 11 rows written (10 lines + empty), 3 visible -> 8 in scrollback.
        assert_eq!(p.scrollback_len(), 8);
        assert_eq!(p.line_text(0).0.trim_end(), "line0");
        assert_eq!(p.line_text(7).0.trim_end(), "line7");
        assert_eq!(p.line_text(8).0.trim_end(), "line8");
        assert_eq!(p.line_text(10).0.trim_end(), "");
        assert_eq!(p.screen().scrollback(), 0);
    }
}
