//! DPAPI (current-user) protection for the licence file.
//!
//! The entitlement and session tokens are bearer credentials. Writing them into `license.json` as
//! plain text would let any process running as the user read them, and a backup would carry them
//! off the machine. Both are handed to `CryptProtectData` instead; only the same Windows user on the
//! same machine can ask `CryptUnprotectData` for them back.

use std::ptr;

/// What went wrong while protecting or unprotecting a blob.
#[derive(Debug, thiserror::Error)]
pub enum DpapiError {
    /// The Windows call refused the request.
    #[error("DPAPI refused the request (status 0x{0:08X})")]
    Status(u32),
    /// DPAPI does not exist on this platform; tests on other hosts must skip.
    #[error("DPAPI is only available on Windows")]
    Unsupported,
    /// The caller handed DPAPI an empty buffer.
    #[error("cannot protect an empty buffer")]
    Empty,
}

/// `DATA_BLOB` from `dpapi.h`.
#[cfg(windows)]
#[repr(C)]
struct DataBlob {
    size: u32,
    data: *mut u8,
}

#[cfg(windows)]
#[link(name = "crypt32")]
unsafe extern "system" {
    fn CryptProtectData(
        data_in: *const DataBlob,
        description: *const u16,
        entropy: *const DataBlob,
        reserved: *mut std::ffi::c_void,
        prompt: *mut std::ffi::c_void,
        flags: u32,
        data_out: *mut DataBlob,
    ) -> i32;

    fn CryptUnprotectData(
        data_in: *const DataBlob,
        description: *mut *mut u16,
        entropy: *const DataBlob,
        reserved: *mut std::ffi::c_void,
        prompt: *mut std::ffi::c_void,
        flags: u32,
        data_out: *mut DataBlob,
    ) -> i32;
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
}

/// `CRYPTPROTECT_UI_FORBIDDEN`: never show a prompt from a background process.
#[cfg(windows)]
const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;

/// Encrypts `plaintext` for the current Windows user.
pub fn protect(plaintext: &[u8]) -> Result<Vec<u8>, DpapiError> {
    if plaintext.is_empty() {
        return Err(DpapiError::Empty);
    }
    protect_impl(plaintext)
}

/// Decrypts a blob produced by [`protect`] for the current Windows user.
pub fn unprotect(ciphertext: &[u8]) -> Result<Vec<u8>, DpapiError> {
    if ciphertext.is_empty() {
        return Err(DpapiError::Empty);
    }
    unprotect_impl(ciphertext)
}

#[cfg(windows)]
fn protect_impl(plaintext: &[u8]) -> Result<Vec<u8>, DpapiError> {
    let input = DataBlob {
        size: plaintext.len() as u32,
        data: plaintext.as_ptr().cast_mut(),
    };
    let mut output = DataBlob {
        size: 0,
        data: ptr::null_mut(),
    };

    // SAFETY: `input` points at `plaintext` for the duration of the call, the description is
    // optional and null, and `output` is a valid out-parameter released below.
    let ok = unsafe {
        CryptProtectData(
            &input,
            ptr::null(),
            ptr::null(),
            ptr::null_mut(),
            ptr::null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };

    if ok == 0 {
        return Err(DpapiError::Status(last_error()));
    }

    // SAFETY: on success `output.data` points at a DPAPI allocation of `output.size` bytes.
    let bytes = unsafe { std::slice::from_raw_parts(output.data, output.size as usize).to_vec() };
    // SAFETY: the allocation came from DPAPI and is freed exactly once.
    unsafe { LocalFree(output.data.cast()) };

    Ok(bytes)
}

#[cfg(windows)]
fn unprotect_impl(ciphertext: &[u8]) -> Result<Vec<u8>, DpapiError> {
    let input = DataBlob {
        size: ciphertext.len() as u32,
        data: ciphertext.as_ptr().cast_mut(),
    };
    let mut output = DataBlob {
        size: 0,
        data: ptr::null_mut(),
    };
    let mut description: *mut u16 = ptr::null_mut();

    // SAFETY: like `protect_impl`; `description` is an optional out-pointer that DPAPI allocates.
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            &mut description,
            ptr::null(),
            ptr::null_mut(),
            ptr::null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };

    if !description.is_null() {
        // SAFETY: DPAPI allocated the description with `LocalAlloc`; it is not used here.
        unsafe { LocalFree(description.cast()) };
    }

    if ok == 0 {
        return Err(DpapiError::Status(last_error()));
    }

    // SAFETY: on success `output.data` is a DPAPI allocation of `output.size` bytes.
    let bytes = unsafe { std::slice::from_raw_parts(output.data, output.size as usize).to_vec() };
    // SAFETY: freed exactly once.
    unsafe { LocalFree(output.data.cast()) };

    Ok(bytes)
}

#[cfg(windows)]
fn last_error() -> u32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32
}

#[cfg(not(windows))]
fn protect_impl(_plaintext: &[u8]) -> Result<Vec<u8>, DpapiError> {
    Err(DpapiError::Unsupported)
}

#[cfg(not(windows))]
fn unprotect_impl(_ciphertext: &[u8]) -> Result<Vec<u8>, DpapiError> {
    Err(DpapiError::Unsupported)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn a_blob_round_trips_for_the_current_user() {
        let secret = b"entitlement.token.session.token";

        let protected = protect(secret).expect("protect");
        assert_ne!(
            protected, secret,
            "the ciphertext must not be the plaintext"
        );
        assert!(
            !protected
                .windows(secret.len())
                .any(|window| window == secret)
        );

        let restored = unprotect(&protected).expect("unprotect");
        assert_eq!(restored, secret);
    }

    #[test]
    fn a_tampered_blob_is_refused() {
        let mut protected = protect(b"secret").expect("protect");
        let last = protected.len() - 1;
        protected[last] ^= 0xff;

        assert!(unprotect(&protected).is_err());
    }

    #[test]
    fn empty_input_is_refused_before_the_system_call() {
        assert!(matches!(protect(b""), Err(DpapiError::Empty)));
        assert!(matches!(unprotect(b""), Err(DpapiError::Empty)));
    }
}
