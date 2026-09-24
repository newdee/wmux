//! Windows clipboard access (CF_UNICODETEXT).

use anyhow::{Result, bail};
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::GlobalFree;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};

const CF_UNICODETEXT: u32 = 13;

/// One user of the clipboard at a time in this process. `OpenClipboard`
/// with no window keeps other programs out but not other threads of this
/// one: without this, one thread's `EmptyClipboard` frees the memory
/// another is reading or has just handed over, and the heap is corrupted
/// (eight threads copying and pasting did it within a second).
static IN_USE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The clipboard, open, and this process's turn at it; closed (then the
/// turn given up) when dropped.
struct Open(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

impl Open {
    fn new() -> Result<Open> {
        let turn = IN_USE.lock().unwrap_or_else(|e| e.into_inner());
        // Another program may hold the clipboard for a moment (a clipboard
        // manager reacting to the last change takes tens of milliseconds):
        // half a second of trying before giving up.
        for _ in 0..50 {
            if unsafe { OpenClipboard(null_mut()) } != 0 {
                return Ok(Open(turn));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        bail!("OpenClipboard failed: {}", std::io::Error::last_os_error());
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        // Runs before the guard field is dropped: closed, then released.
        unsafe { CloseClipboard() };
    }
}

pub fn set_text(text: &str) -> Result<()> {
    let _open = Open::new()?;
    unsafe {
        if EmptyClipboard() == 0 {
            bail!("EmptyClipboard failed");
        }
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = wide.len() * 2;
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if h.is_null() {
            bail!("GlobalAlloc failed");
        }
        let p = GlobalLock(h) as *mut u16;
        if p.is_null() {
            GlobalFree(h);
            bail!("GlobalLock failed");
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
        GlobalUnlock(h);
        // On success the clipboard owns the memory; on failure it is ours.
        if SetClipboardData(CF_UNICODETEXT, h as HANDLE).is_null() {
            let e = std::io::Error::last_os_error();
            GlobalFree(h);
            bail!("SetClipboardData failed: {e}");
        }
    }
    Ok(())
}

pub fn get_text() -> Result<String> {
    let _open = Open::new()?;
    unsafe {
        let h = GetClipboardData(CF_UNICODETEXT);
        if h.is_null() {
            return Ok(String::new());
        }
        let p = GlobalLock(h) as *const u16;
        if p.is_null() {
            bail!("GlobalLock failed");
        }
        // Never read past the allocation, even if the data is not NUL-terminated.
        let max = GlobalSize(h) / 2;
        let mut len = 0usize;
        while len < max && *p.add(len) != 0 {
            len += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
        GlobalUnlock(h);
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// Threads of one process taking turns: eight copying and pasting at
    /// once used to corrupt the heap within a second (0xc0000374), which is
    /// what the e2e suite's servers, sharing one process, ran into now and
    /// then. What was on the clipboard is put back afterwards.
    #[test]
    fn eight_threads_share_the_clipboard() {
        let saved = super::get_text().unwrap_or_default();
        let stop = Instant::now() + Duration::from_secs(2);
        let ops = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..8u32)
            .map(|t| {
                let ops = ops.clone();
                std::thread::spawn(move || {
                    let mut i = 0u32;
                    while Instant::now() < stop {
                        i += 1;
                        if (i + t).is_multiple_of(2) {
                            let _ = super::set_text(&format!("wmux test {t} {i} {}", "x".repeat((i % 300) as usize)));
                        } else {
                            let _ = super::get_text();
                        }
                        ops.fetch_add(1, Ordering::Relaxed);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let _ = super::set_text(&saved);
        assert!(ops.load(Ordering::Relaxed) > 100, "the threads got their turns");
    }
}
