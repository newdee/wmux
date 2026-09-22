//! Desktop notifications, so a job that finishes in a window nobody is
//! looking at can say so outside the terminal too.
//!
//! This is a tray balloon (`Shell_NotifyIcon` with `NIF_INFO`), not a WinRT
//! toast: a toast needs an AppUserModelID registered by an installed
//! shortcut, which the portable zip has no way to provide, while a balloon
//! works for any process. Windows 10 and 11 turn the balloon into a real
//! notification in the Action Center by themselves.
//!
//! The icon only exists while a notification is on screen: a multiplexer
//! has no business leaving something in the tray for the hours it sits
//! there doing nothing.

use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, IDI_APPLICATION, LoadIconW, MSG, PeekMessageW,
    PostQuitMessage, RegisterClassExW, TranslateMessage, WM_DESTROY, WNDCLASSEXW, WS_OVERLAPPED,
};

/// One notification: a bold first line and the text under it.
pub struct Note {
    pub title: String,
    pub body: String,
}

/// The thread that owns the window and the tray icon. Created on the first
/// notification and kept for the life of the server; it sits in a channel
/// receive, not a spin loop.
static SENDER: OnceLock<Option<Sender<Note>>> = OnceLock::new();

/// Show a desktop notification. Returns false when the desktop cannot be
/// reached at all (a service, session 0, a locked-down environment), which
/// is not an error worth failing a command over.
pub fn notify(title: &str, body: &str) -> bool {
    let tx = SENDER.get_or_init(spawn_thread);
    match tx {
        Some(tx) => tx.send(Note { title: title.to_string(), body: body.to_string() }).is_ok(),
        None => false,
    }
}

fn spawn_thread() -> Option<Sender<Note>> {
    let (tx, rx) = channel::<Note>();
    let (ready_tx, ready_rx) = channel::<bool>();
    let spawned = std::thread::Builder::new().name("notify".into()).spawn(move || {
        let Some(hwnd) = message_window() else {
            let _ = ready_tx.send(false);
            return;
        };
        let _ = ready_tx.send(true);
        // A notification stays up for a few seconds; the icon is added just
        // before it and taken away once Windows has had time to show it.
        while let Ok(note) = rx.recv() {
            if !show(hwnd, &note) {
                log::warn!("notification failed: {}", note.title);
            }
            // Keep draining while more arrive so a burst shares one icon.
            let deadline = std::time::Instant::now() + Duration::from_secs(6);
            loop {
                pump(hwnd);
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(next) => {
                        show(hwnd, &next);
                    }
                    Err(_) if std::time::Instant::now() >= deadline => break,
                    Err(_) => {}
                }
            }
            remove_icon(hwnd);
        }
        unsafe { DestroyWindow(hwnd) };
    });
    if spawned.is_err() {
        return None;
    }
    // If the desktop is unreachable the thread says so instead of leaving
    // every later notification to queue up in a channel nobody reads.
    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(true) => Some(tx),
        _ => None,
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// Copy into one of the fixed-size fields of NOTIFYICONDATAW, truncating on
/// a character boundary and leaving the terminating zero.
fn put(dst: &mut [u16], s: &str) {
    let src = wide(s);
    let n = src.len().min(dst.len().saturating_sub(1));
    dst[..n].copy_from_slice(&src[..n]);
    dst[n] = 0;
}

fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut d: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    d.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    d.hWnd = hwnd;
    d.uID = 1;
    d
}

fn show(hwnd: HWND, note: &Note) -> bool {
    let mut d = icon_data(hwnd);
    d.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_INFO;
    d.uCallbackMessage = 0;
    d.hIcon = unsafe { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) };
    put(&mut d.szTip, "wmux");
    put(&mut d.szInfoTitle, &note.title);
    put(&mut d.szInfo, &note.body);
    d.dwInfoFlags = NIIF_INFO;
    // ADD first: modifying an icon that is not there does nothing.
    let added = unsafe { Shell_NotifyIconW(NIM_ADD, &d) } != 0;
    if added {
        return true;
    }
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &d) != 0 }
}

fn remove_icon(hwnd: HWND) {
    let d = icon_data(hwnd);
    unsafe { Shell_NotifyIconW(NIM_DELETE, &d) };
}

/// Drain the window's queue; the balloon needs a pumping thread to appear.
fn pump(_hwnd: HWND) {
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    while unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, 1) } != 0 {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_DESTROY {
        unsafe { PostQuitMessage(0) };
        return 0;
    }
    unsafe { DefWindowProcW(hwnd, msg, w, l) }
}

/// A window nobody sees, only there because a tray icon needs an owner.
fn message_window() -> Option<HWND> {
    let class = wide("wmux-notify\0");
    let name = wide("wmux\0");
    let mut wc: WNDCLASSEXW = unsafe { std::mem::zeroed() };
    wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
    wc.lpfnWndProc = Some(wnd_proc);
    wc.lpszClassName = class.as_ptr();
    // A second server in the same process would re-register; either the
    // class is ours already or CreateWindowExW will tell us.
    unsafe { RegisterClassExW(&wc) };
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            name.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    if hwnd.is_null() { None } else { Some(hwnd) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text fields are fixed-size; long input must be cut, never
    /// overflow, and always stay zero-terminated.
    #[test]
    fn long_text_is_truncated_safely() {
        let mut buf = [0u16; 8];
        put(&mut buf, "hello world, this is much too long");
        assert_eq!(buf[7], 0, "zero terminated");
        assert_eq!(String::from_utf16_lossy(&buf[..7]), "hello w");

        let mut small = [0u16; 1];
        put(&mut small, "x");
        assert_eq!(small[0], 0, "nothing fits but the terminator");

        let mut exact = [0u16; 4];
        put(&mut exact, "abc");
        assert_eq!(String::from_utf16_lossy(&exact[..3]), "abc");
        assert_eq!(exact[3], 0);
    }

    /// Asking for a notification must never take the server down, whatever
    /// the desktop situation is (this test runs in session 0 on CI).
    #[test]
    fn notify_never_panics() {
        let _ = notify("wmux", "test notification");
        let _ = notify("wmux", "");
        let _ = notify("", "body only");
    }
}
