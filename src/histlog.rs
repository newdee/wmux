//! `log-history`: what panes print, kept on disk after the scrollback has
//! let it go, one plain-text file per pane position per day:
//!
//! ```text
//! <dir>\<session>\<window>.<pane>\2026-09-25.log
//! ```
//!
//! A line is written when it scrolls off the top of its pane's screen, so
//! a program that redraws in place (a progress bar, a prompt being edited)
//! leaves its final text, not every frame; a full-screen program (vim,
//! less) on the alternate screen leaves nothing. What is still on screen
//! when the pane goes is written then. A command the shell reported gets a
//! line of its times before it (`── 14:03:22 · 3.2s · ✓ ──`).
//!
//! Writing is done by one thread, so a slow disk never holds up the
//! server. A file stops growing at `DAY_CAP` bytes (with a note saying
//! so); files older than `log-history-days` are removed.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Most bytes one pane position writes in one day.
pub const DAY_CAP: u64 = 20 * 1024 * 1024;

/// Where the files go when `log-history-dir` is not set: `KEEPANE_HISTORY_DIR`,
/// else beside the saved sessions when `KEEPANE_SESSIONS_DIR` moves those
/// (the tests do, and so stay out of the real directory), else
/// `%LOCALAPPDATA%\keepane\history`.
pub fn default_dir() -> PathBuf {
    if let Some(d) = crate::legacy::var_os("KEEPANE_HISTORY_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    if let Some(d) = crate::legacy::var_os("KEEPANE_SESSIONS_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d).join("history");
    }
    crate::logger::log_dir().join("history")
}

/// A name made safe to be a directory: characters Windows refuses become
/// `_`, as do trailing dots and spaces; nothing at all becomes `_`.
pub fn safe_name(name: &str) -> String {
    let mut s: String =
        name.chars().map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c }).collect();
    while s.ends_with('.') || s.ends_with(' ') {
        s.pop();
    }
    if s.is_empty() { "_".into() } else { s }
}

/// The file a pane position writes on `day`.
pub fn file_for(dir: &Path, session: &str, window: usize, pane: usize, day: chrono::NaiveDate) -> PathBuf {
    dir.join(safe_name(session)).join(format!("{window}.{pane}")).join(format!("{}.log", day.format("%Y-%m-%d")))
}

enum Msg {
    Append(PathBuf, String),
    Prune(PathBuf, u32),
    Flush(Sender<()>),
    /// A writer dying of a bug, for the test of its replacement.
    #[cfg(test)]
    Crash,
}

fn start() -> Sender<Msg> {
    let (tx, rx) = channel();
    std::thread::Builder::new().name("history-log".into()).spawn(move || run(rx)).expect("history-log thread");
    tx
}

fn writer() -> &'static Mutex<Sender<Msg>> {
    static TX: OnceLock<Mutex<Sender<Msg>>> = OnceLock::new();
    TX.get_or_init(|| Mutex::new(start()))
}

fn send(m: Msg) {
    let Ok(mut tx) = writer().lock() else { return };
    // A writer that died of a bug must not take the log with it for the
    // rest of the server's life: another one takes over.
    if let Err(back) = tx.send(m) {
        log::error!("history log writer stopped; starting another");
        *tx = start();
        let _ = tx.send(back.0);
    }
}

/// Add `text` to the end of `path`, from the writer thread.
pub fn append(path: PathBuf, text: String) {
    if !text.is_empty() {
        send(Msg::Append(path, text));
    }
}

/// Remove the files under `dir` older than `days` days (0 keeps them all),
/// and the directories that leaves empty.
pub fn prune(dir: PathBuf, days: u32) {
    if days > 0 {
        send(Msg::Prune(dir, days));
    }
}

/// Wait (at most `wait`) until everything asked for so far is on disk.
pub fn flush(wait: Duration) {
    let (tx, rx) = channel();
    send(Msg::Flush(tx));
    let _ = rx.recv_timeout(wait);
}

struct Open {
    file: std::io::BufWriter<std::fs::File>,
    size: u64,
}

/// How long the writer waits for more before what it holds goes to disk:
/// output comes a few lines at a time, and a write per few lines costs more
/// than everything else the log does.
const SETTLE: Duration = Duration::from_millis(500);

fn flush_all(open: &mut HashMap<PathBuf, Open>) {
    open.retain(|path, o| match o.file.flush() {
        Ok(()) => true,
        Err(e) => {
            log::warn!("history log {}: {e}", path.display());
            false
        }
    });
}

fn run(rx: Receiver<Msg>) {
    let mut open: HashMap<PathBuf, Open> = HashMap::new();
    // A directory that cannot be written is said once, not every second.
    let mut failed: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    loop {
        let m = match rx.recv_timeout(SETTLE) {
            Ok(m) => m,
            // Quiet: to disk, and the files closed, so one deleted or moved
            // meanwhile is made again rather than written into unseen.
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                flush_all(&mut open);
                open.clear();
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        match m {
            Msg::Append(path, text) => {
                // A few dozen panes at most write at once; a day's worth of
                // stale handles is not kept.
                if open.len() > 64 && !open.contains_key(&path) {
                    flush_all(&mut open);
                    open.clear();
                }
                if !open.contains_key(&path) {
                    let opened = path
                        .parent()
                        .map_or(Ok(()), std::fs::create_dir_all)
                        .and_then(|()| std::fs::OpenOptions::new().create(true).append(true).open(&path));
                    match opened {
                        Ok(file) => {
                            let size = file.metadata().map(|m| m.len()).unwrap_or(0);
                            let file = std::io::BufWriter::with_capacity(64 * 1024, file);
                            open.insert(path.clone(), Open { file, size });
                        }
                        Err(e) => {
                            if failed.insert(path.clone()) {
                                log::warn!("history log {}: {e}", path.display());
                            }
                            continue;
                        }
                    }
                }
                let o = open.get_mut(&path).unwrap();
                if o.size >= DAY_CAP {
                    continue;
                }
                // What would pass the cap is replaced by a note saying so,
                // after which the file counts as full.
                let (bytes, size) = if o.size + text.len() as u64 > DAY_CAP {
                    (
                        format!("[keepane: this file reached {} MB; the rest of the day is not kept]\n", DAY_CAP >> 20),
                        DAY_CAP,
                    )
                } else {
                    let n = o.size + text.len() as u64;
                    (text, n)
                };
                match o.file.write_all(bytes.as_bytes()) {
                    Ok(()) => o.size = size,
                    Err(e) => {
                        log::warn!("history log {}: {e}", path.display());
                        open.remove(&path);
                    }
                }
            }
            Msg::Prune(dir, days) => {
                // Handles into files about to go would keep them.
                flush_all(&mut open);
                open.clear();
                // More days than the calendar goes back: nothing is that old.
                if let Some(before) =
                    chrono::Local::now().date_naive().checked_sub_days(chrono::Days::new(u64::from(days)))
                {
                    prune_dir(&dir, before);
                }
            }
            Msg::Flush(done) => {
                flush_all(&mut open);
                let _ = done.send(());
            }
            #[cfg(test)]
            Msg::Crash => panic!("history-log test: told to crash"),
        }
    }
}

/// Remove day files dated before `before` under `dir` (two levels down),
/// then the directories left empty. Nothing that is not a day file is
/// touched.
fn prune_dir(dir: &Path, before: chrono::NaiveDate) {
    for session in read_dirs(dir) {
        for key in read_dirs(&session) {
            for f in std::fs::read_dir(&key).into_iter().flatten().flatten() {
                if day_of(&f.path()).is_some_and(|d| d < before) {
                    let _ = std::fs::remove_file(f.path());
                }
            }
            let _ = std::fs::remove_dir(&key); // only if empty
        }
        let _ = std::fs::remove_dir(&session);
    }
}

fn read_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    v.sort();
    v
}

/// The day a `YYYY-MM-DD.log` file is for.
pub fn day_of(path: &Path) -> Option<chrono::NaiveDate> {
    if path.extension()? != "log" {
        return None;
    }
    chrono::NaiveDate::parse_from_str(path.file_stem()?.to_str()?, "%Y-%m-%d").ok()
}

/// One pane position that has history: its session directory's name, its
/// `window.pane`, and its day files, newest first, with their sizes.
#[derive(Clone, Debug, PartialEq)]
pub struct Kept {
    pub session: String,
    pub key: String,
    pub days: Vec<(chrono::NaiveDate, PathBuf, u64)>,
}

/// Everything under `dir`, sessions in name order, positions in window
/// then pane order.
pub fn scan(dir: &Path) -> Vec<Kept> {
    let mut out = Vec::new();
    for session in read_dirs(dir) {
        let mut keys: Vec<(Vec<u64>, Kept)> = Vec::new();
        for key in read_dirs(&session) {
            let mut days: Vec<(chrono::NaiveDate, PathBuf, u64)> = std::fs::read_dir(&key)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|f| f.file_type().is_ok_and(|t| t.is_file()))
                .filter_map(|f| {
                    let p = f.path();
                    let d = day_of(&p)?;
                    Some((d, p, f.metadata().map(|m| m.len()).unwrap_or(0)))
                })
                .collect();
            if days.is_empty() {
                continue;
            }
            days.sort_by_key(|d| std::cmp::Reverse(d.0));
            let name = key.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let order = name.split('.').map(|n| n.parse().unwrap_or(u64::MAX)).collect();
            keys.push((
                order,
                Kept {
                    session: session.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                    key: name,
                    days,
                },
            ));
        }
        keys.sort_by(|a, b| a.0.cmp(&b.0));
        out.extend(keys.into_iter().map(|(_, k)| k));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The writer is one for the process: these tests take turns with it.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn turn() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A writer that died is replaced: what is sent after it went is
    /// written (what was on its way when it died is lost with it).
    #[test]
    fn a_dead_writer_is_replaced() {
        let _turn = turn();
        let dir = std::env::temp_dir().join(format!("keepane-histlog-crash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        send(Msg::Crash);
        std::thread::sleep(Duration::from_millis(300));
        let f = file_for(&dir, "s", 0, 0, chrono::Local::now().date_naive());
        append(f.clone(), "after the crash\n".into());
        flush(Duration::from_secs(5));
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "after the crash\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_become_paths_and_days_are_read_back() {
        assert_eq!(safe_name("a:b/c\\d?"), "a_b_c_d_");
        assert_eq!(safe_name("dots. "), "dots");
        assert_eq!(safe_name(""), "_");
        let day = chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap();
        let p = file_for(Path::new("C:\\h"), "work", 1, 0, day);
        assert_eq!(p, Path::new("C:\\h\\work\\1.0\\2026-09-25.log"));
        assert_eq!(day_of(&p), Some(day));
        assert_eq!(day_of(Path::new("notes.txt")), None);
        assert_eq!(day_of(Path::new("2026-13-01.log")), None);
    }

    /// Odd things in the directory and odd settings: nothing panics, and
    /// the writer keeps writing.
    #[test]
    fn junk_and_extreme_settings_are_survived() {
        let _turn = turn();
        let dir = std::env::temp_dir().join(format!("keepane-histlog-junk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let today = chrono::Local::now().date_naive();
        // A file where a session directory would be, a directory where a
        // day file would be, a file that is not a day, an empty day.
        std::fs::create_dir_all(dir.join("s").join("0.0").join("2026-01-01.log")).unwrap();
        std::fs::write(dir.join("not-a-session"), "x").unwrap();
        std::fs::write(dir.join("s").join("0.0").join("notes.txt"), "x").unwrap();
        std::fs::create_dir_all(dir.join("s").join("1.0")).unwrap();
        std::fs::write(dir.join("s").join("1.0").join("2026-02-30.log"), "").unwrap();
        std::fs::write(dir.join("s").join("1.0").join(format!("{}.log", today.format("%Y-%m-%d"))), "").unwrap();
        let kept = scan(&dir);
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!((kept[0].key.as_str(), kept[0].days.len(), kept[0].days[0].2), ("1.0", 1, 0));
        // Keep more days than the calendar has: nothing to remove, and the
        // writer is still there afterwards.
        prune(dir.clone(), u32::MAX);
        prune(dir.clone(), 1);
        let f = file_for(&dir, "s", 1, 0, today);
        append(f.clone(), "still writing\n".into());
        flush(Duration::from_secs(5));
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "still writing\n");
        assert!(dir.join("not-a-session").exists() && dir.join("s").join("0.0").join("notes.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn written_scanned_capped_and_pruned() {
        let _turn = turn();
        let dir = std::env::temp_dir().join(format!("keepane-histlog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let today = chrono::Local::now().date_naive();
        let old = today - chrono::Days::new(40);
        append(file_for(&dir, "s", 0, 0, today), "one\n".into());
        append(file_for(&dir, "s", 0, 0, today), "two\n".into());
        append(file_for(&dir, "s", 10, 0, today), "ten\n".into());
        append(file_for(&dir, "s", 2, 1, old), "old\n".into());
        flush(Duration::from_secs(5));
        let kept = scan(&dir);
        let keys: Vec<&str> = kept.iter().map(|k| k.key.as_str()).collect();
        assert_eq!(keys, ["0.0", "2.1", "10.0"], "window order, not text order");
        assert_eq!(std::fs::read_to_string(&kept[0].days[0].1).unwrap(), "one\ntwo\n");
        // Past the cap: one note, then nothing.
        let big = file_for(&dir, "s", 3, 0, today);
        append(big.clone(), "x".repeat(DAY_CAP as usize - 10));
        append(big.clone(), "this line does not fit\n".into());
        append(big.clone(), "nor this one\n".into());
        flush(Duration::from_secs(5));
        let text = std::fs::read_to_string(&big).unwrap();
        assert!(text.ends_with("the rest of the day is not kept]\n"), "{}", &text[text.len() - 80..]);
        assert!(!text.contains("nor this"));
        // Thirty days kept: the forty-day-old file and its directory go.
        prune(dir.clone(), 30);
        flush(Duration::from_secs(5));
        let keys: Vec<String> = scan(&dir).into_iter().map(|k| k.key).collect();
        assert_eq!(keys, ["0.0", "3.0", "10.0"]);
        assert!(!dir.join("s").join("2.1").exists());
        // A file deleted while keepane runs is made again after a quiet
        // moment, not written into unseen.
        let f = file_for(&dir, "s", 0, 0, today);
        std::thread::sleep(SETTLE * 3);
        std::fs::remove_file(&f).unwrap();
        append(f.clone(), "after\n".into());
        flush(Duration::from_secs(5));
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "after\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
