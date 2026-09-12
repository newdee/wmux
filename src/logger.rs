//! Minimal file logger for the (console-less) server process.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::Mutex;

struct FileLogger {
    file: Mutex<File>,
    level: log::LevelFilter,
}

impl log::Log for FileLogger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= self.level
    }
    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(
                f,
                "{} {:<5} [{}] {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
                r.level(),
                r.target(),
                r.args()
            );
        }
    }
    fn flush(&self) {}
}

pub fn log_dir() -> std::path::PathBuf {
    dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("wmux")
}

pub fn init(name: &str) {
    let dir = log_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{name}.log"));
    let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) else { return };
    let level = match std::env::var("WMUX_LOG").as_deref() {
        Ok("trace") => log::LevelFilter::Trace,
        Ok("debug") => log::LevelFilter::Debug,
        Ok("warn") => log::LevelFilter::Warn,
        Ok("error") => log::LevelFilter::Error,
        _ => log::LevelFilter::Info,
    };
    let logger = Box::leak(Box::new(FileLogger { file: Mutex::new(file), level }));
    if log::set_logger(logger).is_ok() {
        log::set_max_level(level);
    }
}
