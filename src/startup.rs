//! `wmux startup on|off|status`: start the server at logon and bring every
//! saved session back, so after a reboot there is nothing to do but attach.
//!
//! The per-user `Run` registry key rather than a scheduled task: a task with
//! a logon trigger cannot be created without administrator rights, while the
//! `Run` key is the user's own. Windows runs the value through CreateProcess
//! at logon; `conhost --headless` in front of it is what keeps a console
//! window from flashing up for a server that has no console anyway.

use anyhow::{Context, Result, bail};
use windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW, RegOpenKeyExW,
    RegQueryValueExW, RegSetValueExW,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// The value's name under the Run key, one per socket so two servers can
/// both come up.
fn value_name(socket: &str) -> String {
    if socket == "default" { "wmux".to_string() } else { format!("wmux-{socket}") }
}

/// What runs at logon: the server directly (not a client that would start
/// it and exit), told to restore every saved session.
fn command_line(exe: &str, socket: &str) -> String {
    format!("conhost.exe --headless \"{exe}\" -L {socket} __server --restore")
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

struct Key(HKEY);

impl Key {
    fn open(access: u32) -> Result<Key> {
        let mut h: HKEY = std::ptr::null_mut();
        let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, wide(RUN_KEY).as_ptr(), 0, access, &mut h) };
        if rc != 0 {
            bail!("cannot open HKCU\\{RUN_KEY} (error {rc})");
        }
        Ok(Key(h))
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}

/// Register the logon command. Idempotent: setting it again replaces it.
pub fn install(socket: &str) -> Result<String> {
    let exe = std::env::current_exe().context("current_exe")?;
    let exe = exe.to_string_lossy().into_owned();
    let name = value_name(socket);
    let cmd = command_line(&exe, socket);
    let key = Key::open(KEY_SET_VALUE)?;
    let data = wide(&cmd);
    let rc = unsafe {
        RegSetValueExW(key.0, wide(&name).as_ptr(), 0, REG_SZ, data.as_ptr() as *const u8, (data.len() * 2) as u32)
    };
    if rc != 0 {
        bail!("cannot write the Run value (error {rc})");
    }
    Ok(format!(
        "wmux will start at logon and restore every saved session.\n\
         where: HKCU\\{RUN_KEY}\\{name}\n\
         runs:  {cmd}\n\
         `wmux startup off` removes it; `wmux startup status` shows it."
    ))
}

pub fn remove(socket: &str) -> Result<String> {
    let name = value_name(socket);
    let key = Key::open(KEY_SET_VALUE)?;
    let rc = unsafe { RegDeleteValueW(key.0, wide(&name).as_ptr()) };
    match rc {
        0 => Ok(format!("{name}: removed; wmux will not start at logon")),
        rc if rc == ERROR_FILE_NOT_FOUND => Ok(format!("{name}: not installed")),
        rc => bail!("cannot delete the Run value (error {rc})"),
    }
}

/// The command that runs at logon, or None when there is none.
pub fn status(socket: &str) -> Result<Option<String>> {
    let name = value_name(socket);
    let key = Key::open(KEY_QUERY_VALUE)?;
    let mut kind = 0u32;
    let mut size = 0u32;
    let name_w = wide(&name);
    let rc = unsafe {
        RegQueryValueExW(key.0, name_w.as_ptr(), std::ptr::null_mut(), &mut kind, std::ptr::null_mut(), &mut size)
    };
    if rc == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if rc != 0 {
        bail!("cannot read the Run value (error {rc})");
    }
    let mut buf = vec![0u16; (size as usize).div_ceil(2) + 1];
    let rc = unsafe {
        RegQueryValueExW(
            key.0,
            name_w.as_ptr(),
            std::ptr::null_mut(),
            &mut kind,
            buf.as_mut_ptr() as *mut u8,
            &mut size,
        )
    };
    if rc != 0 {
        bail!("cannot read the Run value (error {rc})");
    }
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    Ok(Some(String::from_utf16_lossy(&buf[..end])))
}

/// `wmux startup [on|off|status]`, run on the client side: nothing here
/// needs a server, and the server is exactly what this is about starting.
pub fn run(socket: &str, args: &[String]) -> Result<i32> {
    // One verb and nothing after it: a typo must not look like it worked.
    if args.len() > 1 {
        bail!("startup: unexpected argument '{}'", args[1]);
    }
    match args.first().map(String::as_str) {
        Some("on") | Some("install") | Some("enable") => {
            println!("{}", install(socket)?);
            Ok(0)
        }
        Some("off") | Some("uninstall") | Some("disable") => {
            println!("{}", remove(socket)?);
            Ok(0)
        }
        None | Some("status") => match status(socket)? {
            Some(cmd) => {
                println!("{}: installed, runs at logon:\n  {cmd}", value_name(socket));
                Ok(0)
            }
            None => {
                println!("{}: not installed (`wmux startup on` to start at logon)", value_name(socket));
                Ok(1)
            }
        },
        Some(other) => bail!("startup: expected on, off or status, got '{other}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_command_lines() {
        assert_eq!(value_name("default"), "wmux");
        assert_eq!(value_name("work"), "wmux-work");
        let c = command_line(r"C:\Program Files\wmux\wmux.exe", "default");
        // Headless conhost, the exe quoted (Program Files has a space), the
        // server entry point and the restore flag: all four or the result is
        // visible, broken, a client, or empty after a reboot.
        assert!(c.starts_with("conhost.exe --headless "), "{c}");
        assert!(c.contains(r#""C:\Program Files\wmux\wmux.exe""#), "{c}");
        assert!(c.ends_with("-L default __server --restore"), "{c}");
    }

    #[test]
    fn bad_verb_is_refused_without_touching_the_registry() {
        let e = run("default", &["sideways".to_string()]).unwrap_err().to_string();
        assert!(e.contains("expected on, off or status"), "{e}");
        let e = run("default", &["on".to_string(), "extra".to_string()]).unwrap_err().to_string();
        assert!(e.contains("unexpected argument 'extra'"), "{e}");
    }

    /// The real key, with a throwaway socket name so nothing of the user's
    /// is touched: on, status, off, status, off again.
    #[test]
    fn install_status_remove_round_trip() {
        let sock = format!("unit-test-{}", std::process::id());
        let _ = remove(&sock);
        assert!(status(&sock).unwrap().is_none());
        let msg = install(&sock).unwrap();
        assert!(msg.contains("--restore"), "{msg}");
        let shown = status(&sock).unwrap().expect("installed");
        assert!(shown.starts_with("conhost.exe --headless "), "{shown}");
        assert!(shown.ends_with(&format!("-L {sock} __server --restore")), "{shown}");
        assert!(remove(&sock).unwrap().contains("removed"));
        assert!(status(&sock).unwrap().is_none());
        assert!(remove(&sock).unwrap().contains("not installed"), "removing twice is not an error");
    }
}
