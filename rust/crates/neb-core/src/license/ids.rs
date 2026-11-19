//! Random identifiers for the account flow.
//!
//! Both the OAuth `state` and the device id have to be unpredictable, so they come from the system
//! CSPRNG (`BCryptGenRandom`) rather than from a clock or a counter.

use super::jwt::base64url_encode;

/// Fills `buffer` from the system CSPRNG.
#[cfg(windows)]
pub fn fill_random(buffer: &mut [u8]) -> Result<(), String> {
    use std::ffi::c_void;
    use std::ptr;

    #[link(name = "bcrypt")]
    unsafe extern "system" {
        fn BCryptGenRandom(algorithm: *mut c_void, buffer: *mut u8, length: u32, flags: u32)
        -> i32;
    }

    /// `BCRYPT_USE_SYSTEM_PREFERRED_RNG`.
    const USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;

    if buffer.is_empty() {
        return Ok(());
    }

    // SAFETY: the buffer is live for `buffer.len()` bytes, a null algorithm selects the system RNG,
    // and the returned status is checked before the bytes are used.
    let status = unsafe {
        BCryptGenRandom(
            ptr::null_mut(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            USE_SYSTEM_PREFERRED_RNG,
        )
    };

    if status < 0 {
        return Err(format!("BCryptGenRandom failed (status 0x{status:08X})"));
    }

    Ok(())
}

/// Fills `buffer` from `/dev/urandom`; only used by a non-Windows build.
#[cfg(not(windows))]
pub fn fill_random(buffer: &mut [u8]) -> Result<(), String> {
    use std::io::Read;

    let mut source = std::fs::File::open("/dev/urandom")
        .map_err(|error| format!("no random source: {error}"))?;
    source
        .read_exact(buffer)
        .map_err(|error| format!("cannot read random bytes: {error}"))
}

/// A base64url token with `bytes` bytes of entropy.
pub fn random_token(bytes: usize) -> Result<String, String> {
    let mut buffer = vec![0u8; bytes];
    fill_random(&mut buffer)?;
    Ok(base64url_encode(&buffer))
}

/// A random UUID in the canonical 8-4-4-4-12 spelling.
pub fn random_uuid() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    fill_random(&mut bytes)?;
    // RFC 4122 version 4 and the RFC 4122 variant.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn uuids_are_unique_and_well_formed() {
        let mut seen = HashSet::new();
        for _ in 0..32 {
            let uuid = random_uuid().expect("uuid");
            assert_eq!(uuid.len(), 36);
            assert_eq!(uuid.as_bytes()[14], b'4', "version nibble: {uuid}");
            assert!(
                matches!(uuid.as_bytes()[19], b'8' | b'9' | b'a' | b'b'),
                "variant nibble: {uuid}"
            );
            assert!(seen.insert(uuid), "UUIDs must not repeat");
        }
    }

    #[test]
    fn tokens_differ_and_are_base64url() {
        let first = random_token(24).expect("token");
        let second = random_token(24).expect("token");

        assert_ne!(first, second);
        assert!(
            first
                .chars()
                .all(|character| character.is_ascii_alphanumeric()
                    || character == '-'
                    || character == '_')
        );
    }
}
