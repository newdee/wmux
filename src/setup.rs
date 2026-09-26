//! `keepane setup claude [--install]`: what Claude Code needs to work in a
//! pane that takes messages (docs/design/mailbox.md §9): two hooks that
//! say when the agent is free (`SessionStart`, `Stop` run `keepane
//! pane-ready -q`) and keepane's MCP server. By default it only prints
//! them; `--install` writes the hooks into `~/.claude/settings.json`
//! (backed up first) and registers the MCP server with Claude Code's own
//! `claude mcp add`. A user's global settings are theirs: nothing is
//! written unless asked.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// What the hooks run: quiet outside a pane, ignored in a pane that is not
/// in `ai` work mode, so it is safe in every Claude Code session.
pub const HOOK_COMMAND: &str = "keepane pane-ready -q";

const EVENTS: &[&str] = &["SessionStart", "Stop"];

fn claude_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("KEEPANE_CLAUDE_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(|h| PathBuf::from(h).join(".claude"))
}

/// The settings with keepane's hooks added where they are missing; and
/// whether anything was added. Everything else is left as it was, in its
/// order.
pub fn with_hooks(mut settings: Value) -> Result<(Value, bool)> {
    let Some(root) = settings.as_object_mut() else { bail!("settings.json is not a JSON object") };
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let Some(hooks) = hooks.as_object_mut() else { bail!("\"hooks\" in settings.json is not an object") };
    let mut changed = false;
    for event in EVENTS {
        let list = hooks.entry(*event).or_insert_with(|| json!([]));
        let Some(list) = list.as_array_mut() else { bail!("\"hooks\".\"{event}\" in settings.json is not a list") };
        let there = list.iter().any(|group| {
            group["hooks"].as_array().is_some_and(|hs| {
                hs.iter().any(|h| h["command"].as_str().is_some_and(|c| c.contains("keepane pane-ready")))
            })
        });
        if !there {
            list.push(json!({ "hooks": [{ "type": "command", "command": HOOK_COMMAND }] }));
            changed = true;
        }
    }
    Ok((settings, changed))
}

fn print_plan(settings: &Path) {
    let snippet = with_hooks(json!({})).map(|(v, _)| serde_json::to_string_pretty(&v).unwrap()).unwrap_or_default();
    println!("Claude Code in a keepane pane (work mode ai) needs:");
    println!();
    println!("1. Hooks that say when it is free, in {}:", settings.display());
    println!("{snippet}");
    println!();
    println!("2. keepane's MCP server:");
    println!("   claude mcp add --scope user keepane -- keepane mcp");
    println!();
    println!("`keepane setup claude --install` does both (the settings file is backed up first).");
}

/// Claude Code's own CLI, run the way Windows runs a `.cmd` shim.
fn claude(args: &[&str]) -> Result<std::process::ExitStatus> {
    let exe = crate::config::which("claude").context("`claude` is not on PATH")?;
    let is_script = exe.extension().is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    let mut c = if is_script {
        let mut c = std::process::Command::new("cmd.exe");
        c.arg("/c").arg(&exe);
        c
    } else {
        std::process::Command::new(&exe)
    };
    Ok(c.args(args).status()?)
}

/// The settings-file half of `--install`: the hooks added, the old file
/// backed up first. Broken JSON is left alone.
fn install_hooks(settings: &Path) -> Result<()> {
    let old = match std::fs::read_to_string(settings) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("read {}", settings.display())),
    };
    let current: Value = match &old {
        Some(t) if !t.trim().is_empty() => {
            serde_json::from_str(t).with_context(|| format!("{} is not valid JSON; left alone", settings.display()))?
        }
        _ => json!({}),
    };
    let (new, changed) = with_hooks(current)?;
    if changed {
        if let Some(t) = &old {
            let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
            let backup = settings.with_file_name(format!("settings.json.bak-keepane-{stamp}"));
            std::fs::write(&backup, t).with_context(|| format!("back up to {}", backup.display()))?;
            println!("backed up {} to {}", settings.display(), backup.display());
        }
        if let Some(dir) = settings.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(settings, serde_json::to_string_pretty(&new)? + "\n")?;
        println!("added the SessionStart and Stop hooks ({HOOK_COMMAND}) to {}", settings.display());
    } else {
        println!("the hooks are already in {}", settings.display());
    }
    Ok(())
}

fn install(settings: &Path) -> Result<()> {
    install_hooks(settings)?;
    match claude(&["mcp", "get", "keepane"]) {
        Ok(s) if s.success() => println!("the keepane MCP server is already registered"),
        Ok(_) => {
            let s = claude(&["mcp", "add", "--scope", "user", "keepane", "--", "keepane", "mcp"])?;
            if !s.success() {
                bail!("`claude mcp add` failed; run it yourself: claude mcp add --scope user keepane -- keepane mcp");
            }
        }
        Err(e) => {
            println!("{e:#}; register the MCP server yourself: claude mcp add --scope user keepane -- keepane mcp")
        }
    }
    Ok(())
}

pub fn run(args: &[String]) -> Result<i32> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let Some(dir) = claude_dir() else { bail!("no home directory") };
    let settings = dir.join("settings.json");
    match words.as_slice() {
        ["claude"] => {
            print_plan(&settings);
            Ok(0)
        }
        ["claude", "--install"] => install(&settings).map(|()| 0),
        _ => bail!("usage: keepane setup claude [--install]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_are_added_once_and_nothing_else_moves() {
        let before = json!({
            "model": "opus",
            "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": "notify-me" }] }] },
            "env": { "A": "1" },
        });
        let (after, changed) = with_hooks(before.clone()).unwrap();
        assert!(changed);
        let keys: Vec<&String> = after.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["model", "hooks", "env"], "the user's order stays");
        assert_eq!(after["hooks"]["Stop"][0], before["hooks"]["Stop"][0], "their hook stays first");
        assert_eq!(after["hooks"]["Stop"][1]["hooks"][0]["command"], HOOK_COMMAND);
        assert_eq!(after["hooks"]["SessionStart"][0]["hooks"][0]["command"], HOOK_COMMAND);
        let (again, changed) = with_hooks(after.clone()).unwrap();
        assert!(!changed, "a second run adds nothing");
        assert_eq!(again, after);
        assert!(with_hooks(json!([])).is_err());
        assert!(with_hooks(json!({"hooks": {"Stop": {}}})).is_err(), "a shape it does not know is left alone");
    }

    #[test]
    fn install_backs_up_and_writes_only_what_changes() {
        let dir = std::env::temp_dir().join(format!("keepane-setup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let settings = dir.join("settings.json");
        std::fs::write(&settings, "{\"model\": \"opus\"}").unwrap();
        // Only the file half: the other would register with the real Claude Code.
        install_hooks(&settings).unwrap();
        let written: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(written["model"], "opus");
        assert_eq!(written["hooks"]["Stop"][0]["hooks"][0]["command"], HOOK_COMMAND);
        let backups: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("settings.json.bak-keepane-"))
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read_to_string(backups[0].path()).unwrap(), "{\"model\": \"opus\"}");
        // Broken JSON is never overwritten.
        std::fs::write(&settings, "{ not json").unwrap();
        assert!(install_hooks(&settings).is_err());
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), "{ not json");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
