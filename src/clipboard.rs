//! Windows clipboard access (CF_UNICODETEXT).

use anyhow::{Result, bail};
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};

const CF_UNICODETEXT: u32 = 13;

struct Open;

impl Open {
    fn new() -> Result<Open> {
        for _ in 0..10 {
            if unsafe { OpenClipboard(null_mut()) } != 0 {
                return Ok(Open);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        bail!("OpenClipboard failed: {}", std::io::Error::last_os_error());
    }
}

impl Drop for Open {
    fn drop(&mut self) {
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
            bail!("GlobalLock failed");
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
        GlobalUnlock(h);
        if SetClipboardData(CF_UNICODETEXT, h as HANDLE).is_null() {
            bail!("SetClipboardData failed: {}", std::io::Error::last_os_error());
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
