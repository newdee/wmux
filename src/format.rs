//! tmux-style format strings for the status line.
//!
//! Supported: `#S #W #I #P #T #H #F`, `#{session_name}`-style variables,
//! `#[fg=red,bg=default,bold]` style changes, `#(command)` whose output is
//! taken from a cache filled by the server (see `ShellCache`), `%H:%M` and
//! other strftime fields, and `##` for a literal `#`.

use crate::config::parse_color;
use crate::server::render::Style;
use std::collections::HashMap;
use vt100::Color;

/// Values a format can refer to: the session, window and pane a format is
/// being expanded for, and the client it is drawn on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Context {
    pub session: String,
    pub session_id: u32,
    pub session_windows: usize,
    /// Clients attached to the session.
    pub session_attached: usize,
    /// Unix time the session was created.
    pub session_created: i64,
    pub window: String,
    pub window_id: u32,
    pub window_index: usize,
    pub window_panes: usize,
    pub window_active: bool,
    pub window_last: bool,
    pub window_zoomed: bool,
    pub window_width: u16,
    pub window_height: u16,
    pub window_bell: bool,
    pub window_activity: bool,
    pub window_silence: bool,
    /// Window flags: `*` current, `-` last, `#` `!` `~` alerts, `Z` zoomed.
    pub flags: String,
    pub pane_index: usize,
    pub pane_id: u32,
    pub pane_title: String,
    pub pane_command: String,
    /// The command line the pane was started with.
    pub pane_start_command: String,
    /// Last known working directory of the pane (`#{pane_current_path}`).
    pub pane_path: String,
    pub pane_width: u16,
    pub pane_height: u16,
    pub pane_active: bool,
    /// The pane's program has exited (`remain-on-exit`), with this code.
    pub pane_dead: bool,
    pub pane_dead_status: Option<u32>,
    pub pane_synchronized: bool,
    /// In copy mode.
    pub pane_in_mode: bool,
    pub pane_pid: Option<u32>,
    /// Unix time the pane's program was started, and of its last output.
    pub pane_start_time: i64,
    pub pane_activity: i64,
    pub client_width: u16,
    pub client_height: u16,
    pub host: String,
    pub socket: String,
}

/// A number of seconds as people say it: `42s`, `5m`, `2h13m`, `3d2h`.
pub fn human_duration(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86399 => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
        _ => format!("{}d{}h", s / 86400, (s % 86400) / 3600),
    }
}

/// `#{=10:var}` (first 10), `#{=-10:var}` (last 10), `#{b:var}` (basename),
/// `#{d:var}` (dirname), `#{t:var}` (a time as a clock), `#{s/a/b/:var}`
/// (substitution). Anything else before a colon is not a modifier.
fn is_modifier(m: &str) -> bool {
    matches!(m, "b" | "d" | "t")
        || m.strip_prefix('=').is_some_and(|n| n.strip_prefix('-').unwrap_or(n).parse::<usize>().is_ok())
        || (m.starts_with("s/") && m.ends_with('/') && m.matches('/').count() >= 3)
}

fn apply_modifier(m: &str, v: String) -> String {
    if let Some(n) = m.strip_prefix('=') {
        return match n.strip_prefix('-') {
            Some(k) => {
                let k: usize = k.parse().unwrap_or(0);
                let total = v.chars().count();
                v.chars().skip(total.saturating_sub(k)).collect()
            }
            None => v.chars().take(n.parse().unwrap_or(0)).collect(),
        };
    }
    // A path may end in a separator (`std::env::temp_dir` does); the last
    // component is still the last component.
    let trimmed = v.trim_end_matches(['/', '\\']);
    match m {
        "b" => trimmed.rsplit(['/', '\\']).next().unwrap_or("").to_string(),
        "d" => match trimmed.rfind(['/', '\\']) {
            Some(i) => trimmed[..i].to_string(),
            None => v,
        },
        "t" => match v.parse::<i64>() {
            Ok(secs) => chrono::DateTime::from_timestamp(secs, 0)
                .map(|t| t.with_timezone(&chrono::Local).format("%a %b %e %H:%M:%S %Y").to_string())
                .unwrap_or(v),
            Err(_) => v,
        },
        _ => {
            // s/from/to/
            let body = &m[2..m.len() - 1];
            match body.split_once('/') {
                Some((from, to)) if !from.is_empty() => v.replace(from, to),
                _ => v,
            }
        }
    }
}

impl Context {
    pub fn var(&self, name: &str) -> Option<String> {
        if let Some((m, rest)) = name.split_once(':')
            && is_modifier(m)
        {
            return self.var(rest).map(|v| apply_modifier(m, v));
        }
        let flag = |b: bool| if b { "1" } else { "0" }.to_string();
        Some(match name {
            "session_name" | "S" => self.session.clone(),
            "session_id" => format!("${}", self.session_id),
            "session_windows" => self.session_windows.to_string(),
            "session_attached" => self.session_attached.to_string(),
            "session_created" => self.session_created.to_string(),
            "window_name" | "W" => self.window.clone(),
            "window_id" => format!("@{}", self.window_id),
            "window_index" | "I" => self.window_index.to_string(),
            "window_panes" => self.window_panes.to_string(),
            "window_active" => flag(self.window_active),
            "window_last_flag" => flag(self.window_last),
            "window_zoomed_flag" => flag(self.window_zoomed),
            "window_width" => self.window_width.to_string(),
            "window_height" => self.window_height.to_string(),
            "window_bell_flag" => flag(self.window_bell),
            "window_activity_flag" => flag(self.window_activity),
            "window_silence_flag" => flag(self.window_silence),
            "window_flags" | "F" => self.flags.clone(),
            "pane_index" | "P" => self.pane_index.to_string(),
            "pane_id" | "D" => format!("%{}", self.pane_id),
            "pane_title" | "T" => self.pane_title.clone(),
            "pane_current_command" => self.pane_command.clone(),
            "pane_start_command" => self.pane_start_command.clone(),
            "pane_current_path" => self.pane_path.clone(),
            "pane_width" => self.pane_width.to_string(),
            "pane_height" => self.pane_height.to_string(),
            "pane_active" => flag(self.pane_active),
            "pane_dead" => flag(self.pane_dead),
            "pane_dead_status" => self.pane_dead_status.map(|c| c.to_string()).unwrap_or_default(),
            "pane_synchronized" => flag(self.pane_synchronized),
            "pane_in_mode" => flag(self.pane_in_mode),
            "pane_pid" => self.pane_pid.map(|p| p.to_string()).unwrap_or_default(),
            "pane_start_time" => self.pane_start_time.to_string(),
            "pane_activity" => self.pane_activity.to_string(),
            "client_width" => self.client_width.to_string(),
            "client_height" => self.client_height.to_string(),
            "host" | "H" => self.host.clone(),
            "host_short" | "h" => self.host.split('.').next().unwrap_or("").to_string(),
            "socket_path" => self.socket.clone(),
            "version" => env!("CARGO_PKG_VERSION").to_string(),
            "pid" => std::process::id().to_string(),
            _ => return None,
        })
    }
}

/// Output of `#(command)` pieces, keyed by the command text. The server
/// refreshes entries every `status-interval` seconds; a missing entry means
/// the command has not finished yet and expands to "".
#[derive(Default, Debug)]
pub struct ShellCache {
    pub results: HashMap<String, String>,
    /// Commands seen during the last expansion, so the server knows what to run.
    pub wanted: Vec<String>,
}

/// A run of text with one style.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    pub text: String,
    pub style: Style,
}

/// Expand `fmt` into styled segments. `base` is the style in effect at the
/// start and after `#[default]`.
pub fn expand(
    fmt: &str,
    ctx: &Context,
    cache: &mut ShellCache,
    base: Style,
    now: chrono::DateTime<chrono::Local>,
) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    let mut style = base;
    let mut text = String::new();
    let mut chars = fmt.chars().peekable();
    let flush = |out: &mut Vec<Segment>, text: &mut String, style: Style| {
        if !text.is_empty() {
            out.push(Segment { text: std::mem::take(text), style });
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '#' => match chars.next() {
                Some('#') => text.push('#'),
                Some('[') => {
                    let spec: String = chars.by_ref().take_while(|&c| c != ']').collect();
                    flush(&mut out, &mut text, style);
                    style = apply_style(style, base, &spec);
                }
                Some('(') => {
                    let cmd = take_balanced(&mut chars, '(', ')');
                    if !cache.wanted.contains(&cmd) {
                        cache.wanted.push(cmd.clone());
                    }
                    if let Some(r) = cache.results.get(&cmd) {
                        text.push_str(r);
                    }
                }
                Some('{') => {
                    let name = take_balanced(&mut chars, '{', '}');
                    let name = name.trim();
                    if let Some(rest) = name.strip_prefix('?') {
                        // The chosen branch is a format in its own right.
                        let branch = conditional(rest, ctx);
                        flush(&mut out, &mut text, style);
                        out.append(&mut expand(&branch, ctx, cache, style, now));
                    } else if let Some(v) = ctx.var(name) {
                        text.push_str(&v);
                    }
                }
                Some(v) => match ctx.var(&v.to_string()) {
                    Some(val) => text.push_str(&val),
                    None => {
                        text.push('#');
                        text.push(v);
                    }
                },
                None => text.push('#'),
            },
            '%' => match chars.next() {
                Some('%') => text.push('%'),
                Some(f) => {
                    // chrono's Display fails (and `to_string` would panic) on
                    // an unknown specifier; keep such text verbatim instead.
                    use std::fmt::Write;
                    let mut s = String::new();
                    if write!(s, "{}", now.format(&format!("%{f}"))).is_ok() {
                        text.push_str(&s);
                    } else {
                        text.push('%');
                        text.push(f);
                    }
                }
                None => text.push('%'),
            },
            c => text.push(c),
        }
    }
    flush(&mut out, &mut text, style);
    out
}

/// `#{?cond,yes,no}`: `cond` is a variable name, or `var==value` /
/// `var!=value`. A bare name is true when it is neither empty nor "0".
/// Either branch may itself contain `#{...}` and `#[...]` pieces, which the
/// caller expands, so only the text is chosen here.
fn conditional(spec: &str, ctx: &Context) -> String {
    let parts = split_top_level(spec);
    let cond = parts.first().map(String::as_str).unwrap_or("");
    let yes = parts.get(1).cloned().unwrap_or_default();
    let no = parts.get(2).cloned().unwrap_or_default();
    let truth = if let Some((name, want)) = cond.split_once("==") {
        ctx.var(name.trim()).unwrap_or_default() == want.trim()
    } else if let Some((name, want)) = cond.split_once("!=") {
        ctx.var(name.trim()).unwrap_or_default() != want.trim()
    } else {
        let v = ctx.var(cond.trim()).unwrap_or_default();
        !v.is_empty() && v != "0"
    };
    if truth { yes } else { no }
}

/// Split on commas that are not inside `#{...}`, `#(...)` or `#[...]`.
fn split_top_level(s: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut depth = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '#' if matches!(chars.peek(), Some('{') | Some('(') | Some('[')) => {
                depth += 1;
                out.last_mut().unwrap().push(c);
                out.last_mut().unwrap().push(chars.next().unwrap());
            }
            '}' | ')' | ']' if depth > 0 => {
                depth -= 1;
                out.last_mut().unwrap().push(c);
            }
            ',' if depth == 0 && out.len() < 3 => out.push(String::new()),
            c => out.last_mut().unwrap().push(c),
        }
    }
    out
}

fn take_balanced(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, open: char, close: char) -> String {
    let mut depth = 1;
    let mut s = String::new();
    for c in chars.by_ref() {
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                break;
            }
        }
        s.push(c);
    }
    s
}

/// Apply a `#[...]` style spec: `fg=`, `bg=`, `bold`, `nobold`, `dim`,
/// `italics`, `underscore`, `reverse`, `default`, `none`.
fn apply_style(mut cur: Style, base: Style, spec: &str) -> Style {
    for part in spec.split([',', ' ']).map(str::trim).filter(|p| !p.is_empty()) {
        if let Some(c) = part.strip_prefix("fg=") {
            if let Ok(c) = parse_color(c) {
                cur.fg = c;
            }
        } else if let Some(c) = part.strip_prefix("bg=") {
            if let Ok(c) = parse_color(c) {
                cur.bg = c;
            }
        } else {
            match part {
                "default" | "none" => cur = base,
                "bold" | "bright" => cur.bold = true,
                "nobold" | "nobright" => cur.bold = false,
                "dim" => cur.dim = true,
                "nodim" => cur.dim = false,
                "italics" => cur.italic = true,
                "noitalics" => cur.italic = false,
                "underscore" => cur.underline = true,
                "nounderscore" => cur.underline = false,
                "reverse" => cur.inverse = true,
                "noreverse" => cur.inverse = false,
                _ => {}
            }
        }
    }
    cur
}

/// Plain text of a segment list (for width computations and tests).
pub fn plain(segs: &[Segment]) -> String {
    segs.iter().map(|s| s.text.as_str()).collect()
}

/// The plain-text colours a style spec would produce; used by tests and by
/// `set -g status-style`.
pub fn style_from_spec(spec: &str, base: Style) -> Style {
    apply_style(base, base, spec)
}

#[allow(dead_code)]
fn _color_in_scope(_: Color) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_like_speech() {
        assert_eq!(human_duration(-5), "0s");
        assert_eq!(human_duration(0), "0s");
        assert_eq!(human_duration(59), "59s");
        assert_eq!(human_duration(60), "1m");
        assert_eq!(human_duration(3599), "59m");
        assert_eq!(human_duration(3600), "1h00m");
        assert_eq!(human_duration(8013), "2h13m");
        assert_eq!(human_duration(86400), "1d0h");
        assert_eq!(human_duration(266400), "3d2h");
        let c = Context { pane_start_time: 1_700_000_000, pane_activity: 1_700_000_042, ..Default::default() };
        assert_eq!(c.var("pane_start_time").as_deref(), Some("1700000000"));
        assert_eq!(c.var("pane_activity").as_deref(), Some("1700000042"));
    }

    fn ctx() -> Context {
        Context {
            session: "main".into(),
            window: "shell".into(),
            window_index: 2,
            pane_index: 1,
            pane_title: "pwsh".into(),
            pane_command: "pwsh".into(),
            pane_path: "C:\\src".into(),
            host: "BOX".into(),
            flags: "*".into(),
            ..Default::default()
        }
    }

    #[test]
    fn more_variables_and_modifiers() {
        let mut cache = ShellCache::default();
        let mut t = |f: &str, c: &Context| plain(&expand(f, c, &mut cache, Style::default(), now()));
        let mut c = ctx();
        c.session_id = 3;
        c.window_id = 7;
        c.pane_id = 12;
        c.window_panes = 2;
        c.pane_active = true;
        c.pane_dead_status = Some(3);
        c.pane_pid = Some(4242);
        c.client_width = 120;
        c.pane_path = r"C:\Users\me\src\wmux".into();
        c.host = "box.example.com".into();
        c.session_created = 0;
        assert_eq!(t("#{session_id} #{window_id} #{pane_id} #D", &c), "$3 @7 %12 %12");
        assert_eq!(t("#{window_panes}/#{pane_active}/#{pane_dead}", &c), "2/1/0");
        assert_eq!(t("#{pane_dead_status} #{pane_pid} #{client_width}", &c), "3 4242 120");
        assert_eq!(t("#{host_short} #h", &c), "box box");
        assert_eq!(t("#{version}", &c), env!("CARGO_PKG_VERSION"));
        // Modifiers, nested and in conditionals.
        assert_eq!(t("#{b:pane_current_path}", &c), "wmux");
        assert_eq!(t("#{d:pane_current_path}", &c), r"C:\Users\me\src");
        c.pane_path = r"C:\Users\me\src\wmux\".into();
        assert_eq!(t("#{b:pane_current_path}", &c), "wmux", "a trailing separator is not a component");
        assert_eq!(t("#{d:pane_current_path}", &c), r"C:\Users\me\src");
        c.pane_path = r"C:\Users\me\src\wmux".into();
        assert_eq!(t("#{=4:pane_current_path}", &c), "C:\\U");
        assert_eq!(t("#{=-4:pane_current_path}", &c), "wmux");
        assert_eq!(t("#{=2:b:pane_current_path}", &c), "wm");
        assert_eq!(t("#{s/src/SRC/:pane_current_path}", &c), r"C:\Users\me\SRC\wmux");
        assert_eq!(t("#{?pane_active,#{b:pane_current_path},-}", &c), "wmux");
        assert!(t("#{t:session_created}", &c).contains("1970") || t("#{t:session_created}", &c).contains("1969"));
        // Things that look like modifiers but are not stay unknown, and an
        // unknown variable is empty as before.
        assert_eq!(t("[#{x:pane_title}]", &c), "[]");
        assert_eq!(t("[#{nosuch}]", &c), "[]");
        c.pane_pid = None;
        assert_eq!(t("[#{pane_pid}]", &c), "[]");
    }

    fn now() -> chrono::DateTime<chrono::Local> {
        chrono::Local.with_ymd_and_hms(2026, 9, 12, 18, 30, 0).unwrap()
    }
    use chrono::TimeZone;

    #[test]
    fn conditionals_pick_a_branch() {
        fn text(f: &str, c: &Context) -> String {
            let mut cache = ShellCache::default();
            expand(f, c, &mut cache, Style::default(), now()).into_iter().map(|s| s.text).collect()
        }
        let mut cache = ShellCache::default();
        let c = ctx();
        // A bare name is true when it is neither empty nor "0".
        assert_eq!(text("#{?window_flags,busy,idle}", &c), "busy");
        assert_eq!(text("#{?pane_current_path,has cwd,no cwd}", &c), "has cwd");
        // Comparisons.
        assert_eq!(text("#{?session_name==main,yes,no}", &c), "yes");
        assert_eq!(text("#{?session_name==other,yes,no}", &c), "no");
        assert_eq!(text("#{?session_name!=other,yes,no}", &c), "yes");
        // The chosen branch is expanded too, styles included.
        assert_eq!(text("#{?window_flags,#{session_name}:#{window_index},-}", &c), "main:2");
        let styled = expand("#{?window_flags,#[bold]on,off}", &c, &mut cache, Style::default(), now());
        assert!(styled.iter().any(|s| s.text == "on" && s.style.bold), "{styled:?}");

        let mut empty = ctx();
        empty.flags = String::new();
        assert_eq!(text("#{?window_flags,busy,idle}", &empty), "idle");
        // A missing branch is empty, an unknown variable is false.
        assert_eq!(text("#{?window_flags,only}", &empty), "");
        assert_eq!(text("#{?nosuchvar,yes,no}", &c), "no");
        // Commas inside a nested piece do not split the branches.
        assert_eq!(text("#{?window_flags,#[fg=red,bold]hot,cold}", &c), "hot");
    }

    #[test]
    fn variables_and_time() {
        let mut cache = ShellCache::default();
        let s = expand("[#S] #I:#W#F #{pane_title}/#P @#H %H:%M %%", &ctx(), &mut cache, Style::default(), now());
        assert_eq!(plain(&s), "[main] 2:shell* pwsh/1 @BOX 18:30 %");
        let s = expand("#{pane_current_path}", &ctx(), &mut cache, Style::default(), now());
        assert_eq!(plain(&s), "C:\\src");
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn styles_split_segments() {
        let mut cache = ShellCache::default();
        let s = expand("a#[fg=red,bold]b#[default]c#[bg=colour17]d", &ctx(), &mut cache, Style::default(), now());
        assert_eq!(plain(&s), "abcd");
        assert_eq!(s.len(), 4);
        assert_eq!(s[1].style.fg, Color::Idx(1));
        assert!(s[1].style.bold);
        assert_eq!(s[2].style, Style::default());
        assert_eq!(s[3].style.bg, Color::Idx(17));
    }

    #[test]
    fn malformed_formats_do_not_panic() {
        let mut cache = ShellCache::default();
        for f in ["%Q", "%中", "100%", "#[fg=red", "#(unclosed", "#{unclosed", "#", "%", "#[fg=notacolour]x", "#[]y"] {
            let _ = expand(f, &ctx(), &mut cache, Style::default(), now());
        }
        // Unknown strftime fields come out verbatim rather than blowing up.
        let s = expand("a%Qb", &ctx(), &mut cache, Style::default(), now());
        assert_eq!(plain(&s), "a%Qb");
    }

    #[test]
    fn shell_pieces_use_the_cache() {
        let mut cache = ShellCache::default();
        let s = expand("cpu #(pwsh -c (Get-Date)) end", &ctx(), &mut cache, Style::default(), now());
        assert_eq!(plain(&s), "cpu  end");
        assert_eq!(cache.wanted, vec!["pwsh -c (Get-Date)"]);
        cache.results.insert("pwsh -c (Get-Date)".into(), "42%".into());
        let s = expand("cpu #(pwsh -c (Get-Date)) end", &ctx(), &mut cache, Style::default(), now());
        assert_eq!(plain(&s), "cpu 42% end");
        // Unknown variables and a trailing '#' are left alone; ## is a literal.
        let s = expand("x#Qy ## z#", &ctx(), &mut cache, Style::default(), now());
        assert_eq!(plain(&s), "x#Qy # z#");
    }
}
