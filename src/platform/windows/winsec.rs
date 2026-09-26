//! Small Win32 helpers: the current user's SID, a per-user security
//! descriptor for the server pipe, and a kill-on-close job object that takes a
//! whole process tree down with its pane.

use anyhow::{Result, bail};
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// String SID of the user running this process (e.g. `S-1-5-21-...-1001`).
pub fn current_user_sid() -> Result<String> {
    unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            bail!("OpenProcessToken: {}", std::io::Error::last_os_error());
        }
        let mut len = 0u32;
        GetTokenInformation(token, TokenUser, null_mut(), 0, &mut len);
        let mut buf = vec![0u8; len as usize];
        let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr() as *mut _, len, &mut len);
        CloseHandle(token);
        if ok == 0 {
            bail!("GetTokenInformation: {}", std::io::Error::last_os_error());
        }
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut s: *mut u16 = null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut s) == 0 {
            bail!("ConvertSidToStringSid: {}", std::io::Error::last_os_error());
        }
        let mut n = 0;
        while *s.add(n) != 0 {
            n += 1;
        }
        let out = String::from_utf16_lossy(std::slice::from_raw_parts(s, n));
        LocalFree(s as *mut _);
        Ok(out)
    }
}

/// SDDL granting full access to `sid` and SYSTEM only (protected DACL).
pub fn owner_only_sddl(sid: &str) -> String {
    format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)")
}

/// Security attributes restricting an object to the current user.
pub struct OwnerOnly {
    sd: *mut core::ffi::c_void,
    attrs: SECURITY_ATTRIBUTES,
}

// The descriptor is a self-contained LocalAlloc block; no thread affinity.
unsafe impl Send for OwnerOnly {}

impl OwnerOnly {
    pub fn new() -> Result<OwnerOnly> {
        let sddl = owner_only_sddl(&current_user_sid()?);
        let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut sd = null_mut();
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(wide.as_ptr(), SDDL_REVISION_1, &mut sd, null_mut())
        };
        if ok == 0 {
            bail!("ConvertStringSecurityDescriptorToSecurityDescriptor({sddl}): {}", std::io::Error::last_os_error());
        }
        let attrs = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd,
            bInheritHandle: 0,
        };
        Ok(OwnerOnly { sd, attrs })
    }

    /// Pointer valid for the lifetime of `self`.
    pub fn as_ptr(&mut self) -> *mut SECURITY_ATTRIBUTES {
        &mut self.attrs
    }
}

impl Drop for OwnerOnly {
    fn drop(&mut self) {
        unsafe { LocalFree(self.sd) };
    }
}

/// A job object with KILL_ON_JOB_CLOSE: every process assigned to it (and
/// their descendants) dies when the last handle is closed.
pub struct KillOnCloseJob {
    handle: HANDLE,
}

unsafe impl Send for KillOnCloseJob {}
unsafe impl Sync for KillOnCloseJob {}

impl KillOnCloseJob {
    pub fn new() -> Result<KillOnCloseJob> {
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                bail!("CreateJobObject: {}", std::io::Error::last_os_error());
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            // Breakaway only when a process asks for it by name
            // (CREATE_BREAKAWAY_FROM_JOB): `restart-server` run from inside
            // a pane must outlive the pane. Every other child stays in.
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
            let ok = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                let e = std::io::Error::last_os_error();
                CloseHandle(handle);
                bail!("SetInformationJobObject: {e}");
            }
            Ok(KillOnCloseJob { handle })
        }
    }

    /// # Safety
    /// `process` must be a valid process handle with PROCESS_SET_QUOTA and
    /// PROCESS_TERMINATE access (any handle from CreateProcess qualifies).
    pub unsafe fn assign(&self, process: HANDLE) -> Result<()> {
        if unsafe { AssignProcessToJobObject(self.handle, process) } == 0 {
            bail!("AssignProcessToJobObject: {}", std::io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for KillOnCloseJob {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sid_and_sddl() {
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-5-"), "{sid}");
        let sddl = owner_only_sddl(&sid);
        assert!(sddl.contains(&sid));
        let mut sa = OwnerOnly::new().unwrap();
        assert!(!sa.as_ptr().is_null());
    }

    #[test]
    fn job_kills_tree_on_drop() {
        use std::os::windows::io::AsRawHandle;
        let job = KillOnCloseJob::new().unwrap();
        // cmd starts a grandchild (ping for ~10s) and waits for it.
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/c", "ping -n 11 127.0.0.1 > nul"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        unsafe { job.assign(child.as_raw_handle() as HANDLE) }.unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(child.try_wait().unwrap().is_none(), "child should still be running");
        drop(job);
        // The whole tree is gone well before ping would have finished.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "job close did not kill the child");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}
