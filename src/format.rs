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

/// Values a format can refer to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Context {
    pub session: String,
    pub window: String,
    pub window_index: usize,
    pub pane_index: usize,
    pub pane_title: String,
    pub pane_command: String,
    /// Last known working directory of the pane (`#{pane_current_path}`).
    pub pane_path: String,
    pub host: String,
    /// Window flags: `*` current, `-` last, `Z` zoomed.
    pub flags: String,
}

impl Context {
    fn var(&self, name: &str) -> Option<String> {
        Some(match name {
            "session_name" | "S" => self.session.clone(),
            "window_name" | "W" => self.window.clone(),
            "window_index" | "I" => self.window_index.to_string(),
            "pane_index" | "P" => self.pane_index.to_string(),
            "pane_title" | "T" => self.pane_title.clone(),
            "pane_current_command" => self.pane_command.clone(),
            "pane_current_path" => self.pane_path.clone(),
            "host" | "host_short" | "H" => self.host.clone(),
            "window_flags" | "F" => self.flags.clone(),
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
        }
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
