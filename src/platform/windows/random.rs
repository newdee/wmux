//! Random bytes from the system's generator.

use anyhow::{Result, bail};

/// Fill `bytes` from the system's cryptographic generator.
pub fn fill(bytes: &mut [u8]) -> Result<()> {
    use windows_sys::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
    let status = unsafe {
        BCryptGenRandom(std::ptr::null_mut(), bytes.as_mut_ptr(), bytes.len() as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG)
    };
    if status != 0 {
        bail!("BCryptGenRandom failed: {status:#x}");
    }
    Ok(())
}
