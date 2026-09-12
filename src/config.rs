//! Server options and the config file (`%USERPROFILE%\.wmux.conf`), which is
//! a list of wmux commands, one per line, like `.tmux.conf`.

use crate::keys::Key;
use std::path::PathBuf;
use vt100::Color;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    pub prefix: Key,
    /// Program used for new panes when no command is given ("pwsh", "wsl", a path...).
    pub default_shell: String,
    /// Full command line overriding `default_shell` when non-empty.
    pub default_command: Vec<String>,
    pub mouse: bool,
    pub history_limit: usize,
    pub status: bool,
    pub status_top: bool,
    pub status_fg: Color,
    pub status_bg: Color,
    pub base_index: usize,
    pub display_time_ms: u64,
    pub pane_border_active_fg: Color,
    pub pane_border_fg: Color,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            prefix: Key::ctrl('b'),
            default_shell: "pwsh".into(),
            default_command: Vec::new(),
            mouse: true,
            history_limit: 5000,
            status: true,
            status_top: false,
            status_fg: Color::Idx(0),
            status_bg: Color::Idx(2),
            base_index: 0,
            display_time_ms: 1500,
            pane_border_active_fg: Color::Idx(2),
            pane_border_fg: Color::Idx(8),
        }
    }
}

fn parse_bool(v: &str) -> Result<bool, String> {
    match v.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "1" => Ok(true),
        "off" | "false" | "no" | "0" => Ok(false),
        other => Err(format!("bad boolean '{other}'")),
    }
}

pub fn parse_color(v: &str) -> Result<Color, String> {
    let v = v.trim();
    let names = [
        "black",
        "red",
        "green",
        "yellow",
        "blue",
        "magenta",
        "cyan",
        "white",
        "brightblack",
        "brightred",
        "brightgreen",
        "brightyellow",
        "brightblue",
        "brightmagenta",
        "brightcyan",
        "brightwhite",
    ];
    if v.eq_ignore_ascii_case("default") {
        return Ok(Color::Default);
    }
    if let Some(i) = names.iter().position(|n| n.eq_ignore_ascii_case(v)) {
        return Ok(Color::Idx(i as u8));
    }
    if let Some(n) = v.strip_prefix("colour").or_else(|| v.strip_prefix("color")) {
        return n.parse::<u8>().map(Color::Idx).map_err(|_| format!("bad colour '{v}'"));
    }
    if let Some(hex) = v.strip_prefix('#')
        && hex.len() == 6
    {
        let p = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| format!("bad colour '{v}'"));
        return Ok(Color::Rgb(p(0)?, p(2)?, p(4)?));
    }
    Err(format!("bad colour '{v}'"))
}

/// Parse "fg=green,bg=black" style values into (fg, bg).
fn parse_style(v: &str) -> Result<(Option<Color>, Option<Color>), String> {
    let mut fg = None;
    let mut bg = None;
    for part in v.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        if let Some(c) = part.strip_prefix("fg=") {
            fg = Some(parse_color(c)?);
        } else if let Some(c) = part.strip_prefix("bg=") {
            bg = Some(parse_color(c)?);
        } else if part == "default" {
            fg = Some(Color::Default);
            bg = Some(Color::Default);
        } else {
            return Err(format!("bad style '{part}'"));
        }
    }
    Ok((fg, bg))
}

impl Options {
    pub fn set(&mut self, name: &str, value: &str) -> Result<(), String> {
        match name {
            "prefix" => self.prefix = Key::parse(value).ok_or_else(|| format!("bad key '{value}'"))?,
            "default-shell" => self.default_shell = value.to_string(),
            "default-command" => self.default_command = crate::command::tokenize(value)?,
            "mouse" => self.mouse = parse_bool(value)?,
            "history-limit" => self.history_limit = value.parse().map_err(|_| format!("bad number '{value}'"))?,
            "status" => {
                self.status = match value {
                    "top" => {
                        self.status_top = true;
                        true
                    }
                    "bottom" => {
                        self.status_top = false;
                        true
                    }
                    v => parse_bool(v)?,
                }
            }
            "status-position" => {
                self.status_top = match value {
                    "top" => true,
                    "bottom" => false,
                    v => return Err(format!("bad status-position '{v}'")),
                }
            }
            "status-style" => {
                let (fg, bg) = parse_style(value)?;
                if let Some(c) = fg {
                    self.status_fg = c;
                }
                if let Some(c) = bg {
                    self.status_bg = c;
                }
            }
            "status-fg" => self.status_fg = parse_color(value)?,
            "status-bg" => self.status_bg = parse_color(value)?,
            "pane-active-border-style" => {
                if let (Some(c), _) = parse_style(value)? {
                    self.pane_border_active_fg = c;
                }
            }
            "pane-border-style" => {
                if let (Some(c), _) = parse_style(value)? {
                    self.pane_border_fg = c;
                }
            }
            "base-index" => self.base_index = value.parse().map_err(|_| format!("bad number '{value}'"))?,
            "display-time" => self.display_time_ms = value.parse().map_err(|_| format!("bad number '{value}'"))?,
            // Accepted for .tmux.conf compatibility; no effect on Windows.
            "escape-time"
            | "default-terminal"
            | "terminal-overrides"
            | "focus-events"
            | "set-clipboard"
            | "renumber-windows"
            | "allow-rename"
            | "automatic-rename"
            | "status-interval"
            | "status-left"
            | "status-right"
            | "status-left-length"
            | "status-right-length"
            | "window-status-format"
            | "window-status-current-format"
            | "window-status-current-style"
            | "mode-keys"
            | "aggressive-resize"
            | "visual-bell"
            | "bell-action"
            | "monitor-activity"
            | "visual-activity"
            | "repeat-time"
            | "set-titles"
            | "set-titles-string"
            | "pane-base-index"
            | "remain-on-exit"
            | "history-file" => {}
            other => return Err(format!("unknown option '{other}'")),
        }
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<String> {
        Some(match name {
            "prefix" => self.prefix.to_string(),
            "default-shell" => self.default_shell.clone(),
            "default-command" => self.default_command.join(" "),
            "mouse" => if self.mouse { "on" } else { "off" }.into(),
            "history-limit" => self.history_limit.to_string(),
            "status" => if self.status { "on" } else { "off" }.into(),
            "status-position" => if self.status_top { "top" } else { "bottom" }.into(),
            "base-index" => self.base_index.to_string(),
            "display-time" => self.display_time_ms.to_string(),
            _ => return None,
        })
    }
}

/// Candidate config file locations, first existing wins.
pub fn config_paths() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(p) = std::env::var("WMUX_CONFIG") {
        v.push(PathBuf::from(p));
    }
    if let Some(home) = dirs::home_dir() {
        v.push(home.join(".wmux.conf"));
        v.push(home.join(".config").join("wmux").join("wmux.conf"));
    }
    if let Some(cfg) = dirs::config_dir() {
        v.push(cfg.join("wmux").join("wmux.conf"));
    }
    v
}

pub fn find_config() -> Option<PathBuf> {
    config_paths().into_iter().find(|p| p.is_file())
}

/// Resolve the executable to run for a new pane when no command is given.
pub fn resolve_shell(opts: &Options) -> Vec<String> {
    if !opts.default_command.is_empty() {
        return opts.default_command.clone();
    }
    let s = opts.default_shell.as_str();
    match s.to_ascii_lowercase().as_str() {
        "pwsh" | "pwsh.exe" => {
            if which("pwsh.exe").is_some() {
                vec!["pwsh.exe".into(), "-NoLogo".into()]
            } else {
                vec!["powershell.exe".into(), "-NoLogo".into()]
            }
        }
        "powershell" | "powershell.exe" => vec!["powershell.exe".into(), "-NoLogo".into()],
        "wsl" | "wsl.exe" => vec!["wsl.exe".into()],
        "cmd" | "cmd.exe" => vec!["cmd.exe".into()],
        _ => vec![s.to_string()],
    }
}

/// Minimal PATH lookup for an executable name.
pub fn which(name: &str) -> Option<PathBuf> {
    let p = std::path::Path::new(name);
    if p.is_absolute() {
        return p.is_file().then(|| p.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into())
        .split(';')
        .map(|s| s.to_ascii_lowercase())
        .collect();
    let has_ext = exts.iter().any(|e| name.to_ascii_lowercase().ends_with(e));
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if has_ext && cand.is_file() {
            return Some(cand);
        }
        if !has_ext {
            for e in &exts {
                let c = dir.join(format!("{name}{e}"));
                if c.is_file() {
                    return Some(c);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_options() {
        let mut o = Options::default();
        o.set("prefix", "C-a").unwrap();
        assert_eq!(o.prefix, Key::ctrl('a'));
        o.set("mouse", "off").unwrap();
        assert!(!o.mouse);
        o.set("status", "top").unwrap();
        assert!(o.status && o.status_top);
        o.set("status-style", "fg=white,bg=colour234").unwrap();
        assert_eq!(o.status_fg, Color::Idx(7));
        assert_eq!(o.status_bg, Color::Idx(234));
        o.set("status-bg", "#1a2b3c").unwrap();
        assert_eq!(o.status_bg, Color::Rgb(0x1a, 0x2b, 0x3c));
        o.set("default-command", "wsl.exe -d Ubuntu").unwrap();
        assert_eq!(o.default_command, vec!["wsl.exe", "-d", "Ubuntu"]);
        assert!(o.set("nonsense", "1").is_err());
        assert!(o.set("mouse", "maybe").is_err());
        assert!(o.set("history-limit", "x").is_err());
        assert_eq!(o.get("prefix").as_deref(), Some("C-a"));
    }

    #[test]
    fn shell_resolution() {
        let mut o = Options { default_shell: "wsl".into(), ..Default::default() };
        assert_eq!(resolve_shell(&o), vec!["wsl.exe"]);
        o.default_shell = "cmd".into();
        assert_eq!(resolve_shell(&o), vec!["cmd.exe"]);
        o.default_command = vec!["nu.exe".into()];
        assert_eq!(resolve_shell(&o), vec!["nu.exe"]);
        assert!(which("cmd.exe").is_some());
        assert!(which("cmd").is_some());
        assert!(which("definitely-not-a-real-binary-xyz").is_none());
    }
}
