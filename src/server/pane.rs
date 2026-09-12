//! A pane: a ConPTY-backed process plus an in-memory terminal (vt100).

use super::layout::PaneId;
use anyhow::{Context, Result};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::sync::mpsc::Sender;

/// Events a pane reports back to the server loop.
pub enum PaneEvent {
    Output(PaneId, Vec<u8>),
    Exit(PaneId, u32),
}

/// vt100 callbacks: collects terminal replies ConPTY expects from a real
/// terminal, the window title and bells.
#[derive(Default)]
pub struct Callbacks {
    pub responses: Vec<u8>,
    pub title: Option<String>,
    pub bell: bool,
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
}

pub struct Pane {
    pub id: PaneId,
    pub parser: vt100::Parser<Callbacks>,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// Kill-on-close job holding the shell and everything it started, so a
    /// dead pane never leaves orphans (tmux's SIGHUP-the-process-group).
    job: Option<crate::winsec::KillOnCloseJob>,
    pub title: String,
    pub command: String,
    pub cwd: Option<String>,
    pub exit_code: Option<u32>,
    pub copy: Option<CopyMode>,
    pub bell: bool,
    pub cols: u16,
    pub rows: u16,
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
                            if tx_out.send(PaneEvent::Output(id, buf[..n].to_vec())).is_err() {
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
                let _ = tx.send(PaneEvent::Exit(id, code));
            })
            .context("spawn waiter thread")?;

        Ok(Pane {
            id,
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
            cwd: cwd.map(str::to_string),
            exit_code: None,
            copy: None,
            bell: false,
            cols,
            rows,
        })
    }

    /// Feed process output into the terminal model; answer any queries.
    pub fn process_output(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
        let cb = self.parser.callbacks_mut();
        if let Some(t) = cb.title.take() {
            self.title = t;
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
                Ok(PaneEvent::Output(_, b)) => pane.process_output(&b),
                Ok(PaneEvent::Exit(_, c)) => pane.exit_code = Some(c),
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
