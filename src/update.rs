//! `wmux update [--check]`: is there a newer wmux, and installing it the
//! way this one was installed. Nothing runs in the background and nothing
//! is installed unasked: the command is typed, it says what it does.
//!
//! The release is asked of GitHub with `curl.exe` (part of Windows 10 and
//! later); an MSI is checked against the SHA-256 published beside it before
//! Windows Installer sees it. Afterwards the running server is still the
//! old program: `wmux restart-server` moves the sessions to the new one.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

const REPO: &str = "newdee/wmux";

/// A release: its version ("0.10.0"), its page, and its files by name.
#[derive(Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub page: String,
    pub assets: Vec<(String, String)>,
}

/// How this wmux was installed, which says how to update it.
#[derive(Debug, PartialEq)]
pub enum Kind {
    /// `scoop install wmux`: scoop updates it.
    Scoop,
    /// The MSI, under Program Files.
    Msi,
    /// Anything else (a zip, `cargo install`, a build): by hand.
    Other(PathBuf),
}

/// Version numbers compared as numbers: 0.10.0 is newer than 0.9.0.
pub fn newer(candidate: &str, current: &str) -> bool {
    let parts =
        |v: &str| -> Vec<u64> { v.trim().trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    parts(candidate) > parts(current)
}

/// Where this program lives says how it got there.
pub fn kind_of(exe: &Path) -> Kind {
    let s = exe.to_string_lossy().to_lowercase();
    if s.contains(r"\scoop\apps\") || s.contains(r"\scoop\shims\") {
        return Kind::Scoop;
    }
    let under =
        |var: &str| std::env::var(var).ok().filter(|p| !p.is_empty()).is_some_and(|p| s.starts_with(&p.to_lowercase()));
    if under("ProgramFiles") || under("ProgramW6432") {
        return Kind::Msi;
    }
    Kind::Other(exe.to_path_buf())
}

/// The hash in a `.sha256` file: `<hex>  <file name>`.
pub fn parse_sha256_file(text: &str) -> Option<String> {
    let h = text.split_whitespace().next()?.to_lowercase();
    (h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit())).then_some(h)
}

/// The latest release's JSON (GitHub's REST API), read into a `Release`.
pub fn parse_release(json: &str) -> Result<Release> {
    let v: serde_json::Value = serde_json::from_str(json).context("release: not JSON")?;
    let tag = v["tag_name"].as_str().context("release: no tag_name")?;
    let page = v["html_url"].as_str().unwrap_or_default().to_string();
    let assets = v["assets"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    Some((x["name"].as_str()?.to_string(), x["browser_download_url"].as_str()?.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Release { version: tag.trim_start_matches('v').to_string(), page, assets })
}

fn curl(args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("curl.exe")
        .args(["-fsSL", "--retry", "2", "-H", "User-Agent: wmux-update"])
        .args(args)
        .output()
        .context("curl.exe (part of Windows 10 and later) could not be run")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}

fn latest() -> Result<Release> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body =
        curl(&["-H", "Accept: application/vnd.github+json", &url]).context("asking GitHub for the latest release")?;
    parse_release(&String::from_utf8_lossy(&body))
}

/// SHA-256 of a file, with the `certutil` every Windows has.
fn sha256_of(path: &Path) -> Result<String> {
    let out = Command::new("certutil").args(["-hashfile"]).arg(path).arg("SHA256").output().context("certutil")?;
    let text = String::from_utf8_lossy(&out.stdout);
    // The second line is the hash, bytes separated by spaces on old Windows.
    text.lines()
        .nth(1)
        .map(|l| l.replace(' ', "").to_lowercase())
        .filter(|h| h.len() == 64)
        .context("certutil gave no hash")
}

fn install_msi(r: &Release) -> Result<()> {
    let name = format!("wmux-{}-windows-x86_64.msi", r.version);
    let find = |n: &str| r.assets.iter().find(|(a, _)| a == n).map(|(_, u)| u.clone());
    let msi_url = find(&name).with_context(|| format!("the release has no {name}"))?;
    let sha_url = find(&format!("{name}.sha256")).with_context(|| format!("the release has no {name}.sha256"))?;
    let dir = std::env::temp_dir().join(format!("wmux-update-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let msi = dir.join(&name);
    println!("downloading {name}");
    curl(&["-o", &msi.to_string_lossy(), &msi_url])?;
    let want =
        parse_sha256_file(&String::from_utf8_lossy(&curl(&[&sha_url])?)).context("the .sha256 file has no hash")?;
    let got = sha256_of(&msi)?;
    if got != want {
        let _ = std::fs::remove_dir_all(&dir);
        bail!("{name} does not match its published SHA-256 ({got} != {want}); not installed");
    }
    println!("SHA-256 matches; installing (Windows asks for permission)");
    let status = Command::new("msiexec.exe").arg("/i").arg(&msi).args(["/passive", "/norestart"]).status()?;
    let _ = std::fs::remove_dir_all(&dir);
    // 3010: installed, a reboot finishes it (a file was in use: the old
    // wmux.exe, which the running server still is).
    match status.code() {
        Some(0) | Some(3010) => Ok(()),
        Some(1602) => bail!("the install was cancelled"),
        c => bail!("msiexec failed ({c:?})"),
    }
}

pub async fn run(socket: &str, args: &[String]) -> Result<i32> {
    let check = match args {
        [] => false,
        [a] if a == "--check" || a == "-n" => true,
        _ => bail!("update: takes --check (or nothing)"),
    };
    let current = env!("CARGO_PKG_VERSION");
    let r = latest()?;
    if !newer(&r.version, current) {
        println!("wmux {current} is the latest");
        return Ok(0);
    }
    println!("wmux {} is out (this is {current}): {}", r.version, r.page);
    if check {
        return Ok(0);
    }
    let exe = std::env::current_exe()?;
    match kind_of(&exe) {
        Kind::Scoop => {
            println!("updating with scoop");
            // scoop is a .cmd/.ps1 shim, which Command::new would not find
            // (it looks for scoop.exe): let cmd resolve it.
            let ok = Command::new("cmd.exe")
                .args(["/d", "/c", "scoop", "update", "wmux"])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                bail!("`scoop update wmux` failed");
            }
        }
        Kind::Msi => install_msi(&r)?,
        Kind::Other(path) => {
            println!(
                "this wmux ({}) was not installed by the MSI or scoop: download it from the page above",
                path.display()
            );
            return Ok(1);
        }
    }
    println!("installed wmux {}", r.version);
    if crate::client::server_version(socket).await.is_some() {
        println!("the running server is still the old one: `wmux restart-server` moves your sessions to the new one");
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_numbers() {
        assert!(newer("0.10.0", "0.9.0"));
        assert!(newer("v1.0.0", "0.99.9"));
        assert!(newer("0.9.1", "0.9.0"));
        assert!(!newer("0.9.0", "0.9.0"));
        assert!(!newer("0.8.9", "0.9.0"));
        assert!(!newer("garbage", "0.9.0"));
    }

    #[test]
    fn the_install_is_told_by_the_path() {
        assert_eq!(kind_of(Path::new(r"C:\Users\x\scoop\apps\wmux\current\wmux.exe")), Kind::Scoop);
        if let Ok(pf) = std::env::var("ProgramFiles") {
            assert_eq!(kind_of(&Path::new(&pf).join("wmux").join("wmux.exe")), Kind::Msi);
        }
        let dev = Path::new(r"D:\src\wmux\target\release\wmux.exe");
        assert_eq!(kind_of(dev), Kind::Other(dev.to_path_buf()));
    }

    #[test]
    fn a_release_and_its_hashes_are_read() {
        let json = r#"{"tag_name":"v0.10.0","html_url":"https://github.com/newdee/wmux/releases/tag/v0.10.0",
            "assets":[{"name":"wmux-0.10.0-windows-x86_64.msi","browser_download_url":"https://x/a.msi"},
                      {"name":"broken"}]}"#;
        let r = parse_release(json).unwrap();
        assert_eq!(r.version, "0.10.0");
        assert_eq!(r.assets, vec![("wmux-0.10.0-windows-x86_64.msi".to_string(), "https://x/a.msi".to_string())]);
        assert!(parse_release("{}").is_err());
        assert!(parse_release("not json").is_err());
        let h = "a".repeat(64);
        assert_eq!(parse_sha256_file(&format!("{h}  wmux.msi")), Some(h.clone()));
        assert_eq!(parse_sha256_file(&h.to_uppercase()), Some(h));
        assert_eq!(parse_sha256_file("abc  file"), None);
        assert_eq!(parse_sha256_file(""), None);
    }

    #[test]
    fn certutil_hashes_a_file() {
        let p = std::env::temp_dir().join(format!("wmux-sha-{}", std::process::id()));
        std::fs::write(&p, b"abc").unwrap();
        let h = sha256_of(&p).unwrap();
        std::fs::remove_file(&p).unwrap();
        assert_eq!(h, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
