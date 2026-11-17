//! The crash half of taskbar hiding.
//!
//! The shell writes `%LocalAppData%\ProjectNeb\taskbar-state.json` *before* it hides the taskbar,
//! so a shell that dies without running its exit path still leaves enough on disk for another
//! process to put the taskbar back. This module is that other process: if the snapshot says the
//! taskbar was hidden and no shell is answering, it restores the auto-hide bit in
//! `HKCU\...\Explorer\StuckRects3\Settings` and broadcasts `WM_SETTINGCHANGE`.
//!
//! The byte arithmetic is pure and tested; the registry and the broadcast are the thin Win32 edge
//! around it. Both directions are idempotent, so a repair that runs twice changes nothing the
//! second time.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_BINARY, REG_NONE, REG_SAM_FLAGS,
    REG_VALUE_TYPE, RegCloseKey, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
};
use windows::core::PCWSTR;

/// File under the ProjectNeb data directory that records the pre-hide layout.
pub const STATE_FILE: &str = "taskbar-state.json";

/// Registry key the Explorer taskbar layout lives in.
pub const KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StuckRects3";

/// Value inside [`KEY_PATH`] carrying the layout bytes.
pub const VALUE_NAME: &str = "Settings";

/// Index of the flags byte inside the layout.
pub const FLAG_INDEX: usize = 8;

/// The bit inside [`FLAG_INDEX`] that means "hide the taskbar when it is not used".
pub const AUTO_HIDE_BIT: u8 = 0x01;

/// How long the settings broadcast waits for one window before skipping it.
const BROADCAST_TIMEOUT_MS: u32 = 1_000;

/// What the shell recorded before it touched the taskbar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Whether the shell hid the taskbar.
    pub hidden: bool,
    /// The whole `Settings` value as it was before the change.
    pub previous: Vec<u8>,
}

/// The on-disk shape, which is the C# `TaskbarState` record: camelCase, and `byte[]` as base64.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredState {
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    previous: String,
}

/// What a repair attempt did, so the caller logs a decision instead of a guess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing is recorded: the shell never hid the taskbar.
    NoSnapshot,
    /// The snapshot says the taskbar was shown; the file is a leftover and was removed.
    NotHidden,
    /// A shell is alive and owns the taskbar, so the snapshot is its business.
    ShellRunning,
    /// The taskbar was put back and the snapshot removed.
    Restored,
    /// The repair could not be completed; the snapshot stays for the next attempt.
    Failed(String),
}

/// The snapshot file next to the config, or `None` when this machine cannot name a data directory.
pub fn state_path() -> Option<PathBuf> {
    neb_core::paths::try_root()
        .ok()
        .map(|root| root.join(STATE_FILE))
}

/// Reads and validates a snapshot file.
///
/// A file that cannot be read, decoded or that carries no layout is treated as absent: guessing a
/// layout would be worse than leaving the taskbar alone.
pub fn load(path: &Path) -> Option<Snapshot> {
    let bytes = std::fs::read(path).ok()?;
    let stored: StoredState = serde_json::from_slice(&bytes).ok()?;
    let previous = decode_base64(&stored.previous)?;

    (!previous.is_empty()).then_some(Snapshot {
        hidden: stored.hidden,
        previous,
    })
}

/// Removes the snapshot; a missing file is not an error.
pub fn clear(path: &Path) -> bool {
    match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

/// Whether `settings` asks for an auto-hidden taskbar.
pub fn is_hidden(settings: &[u8]) -> bool {
    settings
        .get(FLAG_INDEX)
        .is_some_and(|flags| flags & AUTO_HIDE_BIT != 0)
}

/// A copy of `settings` with only the auto-hide bit written.
///
/// Everything else - the edge, the monitor block, the always-on-top bit - is left exactly as it
/// was, and a value too short to carry the flag is returned unchanged: guessing the rest of the
/// layout would be worse than leaving the taskbar alone. Idempotent by construction.
pub fn with_hidden(settings: &[u8], hidden: bool) -> Vec<u8> {
    let mut copy = settings.to_vec();
    if let Some(flags) = copy.get_mut(FLAG_INDEX) {
        *flags = if hidden {
            *flags | AUTO_HIDE_BIT
        } else {
            *flags & !AUTO_HIDE_BIT
        };
    }
    copy
}

/// The bytes to write so the taskbar is in the recorded auto-hide state.
///
/// The *current* layout is kept and only the bit is moved, because the monitor block in the
/// registry may have changed since the snapshot was taken. When the current value cannot be read
/// the snapshot is written whole, which is what the C# repair does.
pub fn plan_restore(current: Option<&[u8]>, previous: &[u8]) -> Vec<u8> {
    match current {
        Some(current) => with_hidden(current, is_hidden(previous)),
        None => previous.to_vec(),
    }
}

/// Repairs the taskbar when the snapshot is orphaned.
///
/// `shell_running` is the caller's answer to "is a shell alive right now"; while one is, the
/// snapshot belongs to it and must not be touched.
pub fn repair_if_orphaned(path: &Path, shell_running: bool) -> Outcome {
    let Some(snapshot) = load(path) else {
        return Outcome::NoSnapshot;
    };

    if !snapshot.hidden {
        clear(path);
        return Outcome::NotHidden;
    }

    if shell_running {
        return Outcome::ShellRunning;
    }

    let current = match read_settings() {
        Ok(bytes) => Some(bytes),
        Err(reason) => {
            return Outcome::Failed(format!("cannot read '{KEY_PATH}\\{VALUE_NAME}': {reason}"));
        }
    };

    let next = plan_restore(current.as_deref(), &snapshot.previous);
    if let Err(reason) = write_settings(&next) {
        return Outcome::Failed(format!("cannot write '{KEY_PATH}\\{VALUE_NAME}': {reason}"));
    }

    broadcast_change();

    if !clear(path) {
        return Outcome::Failed(format!(
            "the taskbar was restored but '{}' cannot be removed",
            path.display()
        ));
    }

    Outcome::Restored
}

/// The raw `Settings` value, or the reason it cannot be read.
pub fn read_settings() -> Result<Vec<u8>, String> {
    let key = Key::open(KEY_QUERY_VALUE)?;
    let name = wide(VALUE_NAME);

    let mut size = 0u32;
    // SAFETY: `key` is an open key, `name` a NUL-terminated buffer, and a null data pointer asks
    // for the size only, which is what the two-call pattern requires.
    let status = unsafe {
        RegQueryValueExW(
            key.0,
            PCWSTR(name.as_ptr()),
            None,
            None,
            None,
            Some(&mut size),
        )
    };
    if status.0 != 0 {
        return Err(format!("size query failed with {status:?}"));
    }

    if size == 0 {
        return Err("the value is empty".to_owned());
    }

    let mut bytes = vec![0u8; size as usize];
    let mut kind = REG_VALUE_TYPE::default();
    // SAFETY: `bytes` is allocated with exactly `size` bytes and the same size is passed in, so the
    // call cannot write past it; `kind` is a valid out-parameter.
    let status = unsafe {
        RegQueryValueExW(
            key.0,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut kind),
            Some(bytes.as_mut_ptr()),
            Some(&mut size),
        )
    };
    if status.0 != 0 {
        return Err(format!("read failed with {status:?}"));
    }

    if kind != REG_BINARY && kind != REG_NONE {
        return Err(format!("unexpected value type {kind:?}"));
    }

    bytes.truncate(size as usize);

    Ok(bytes)
}

/// Writes the layout bytes back.
pub fn write_settings(bytes: &[u8]) -> Result<(), String> {
    let key = Key::open(KEY_SET_VALUE)?;
    let name = wide(VALUE_NAME);

    // SAFETY: `key` is an open key, `name` a NUL-terminated buffer and `bytes` a live slice whose
    // length is passed alongside it.
    let status =
        unsafe { RegSetValueExW(key.0, PCWSTR(name.as_ptr()), None, REG_BINARY, Some(bytes)) };
    if status.0 != 0 {
        return Err(format!("write failed with {status:?}"));
    }

    Ok(())
}

/// Tells every window that the taskbar settings changed.
///
/// Returns whether at least one window answered; a broadcast nobody hears is normal on a machine
/// without Explorer, so the caller logs it rather than treating it as a failure.
pub fn broadcast_change() -> bool {
    let parameter = wide("TraySettings");

    // SAFETY: the window is `HWND_BROADCAST`, the parameter is a NUL-terminated UTF-16 buffer that
    // outlives the call, and the timeout keeps a hung window from blocking the supervisor.
    let answered = unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(parameter.as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            BROADCAST_TIMEOUT_MS,
            None,
        )
    };

    answered.0 != 0
}

/// An open registry key, closed on drop.
#[derive(Debug)]
struct Key(HKEY);

impl Key {
    fn open(access: REG_SAM_FLAGS) -> Result<Self, String> {
        let path = wide(KEY_PATH);
        let mut key = HKEY(std::ptr::null_mut());

        // SAFETY: `path` is a NUL-terminated buffer and `key` a valid out-parameter.
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(path.as_ptr()),
                None,
                access,
                &mut key,
            )
        };
        if status.0 != 0 {
            return Err(format!("open failed with {status:?}"));
        }

        Ok(Self(key))
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the key was opened by `RegOpenKeyExW` and is closed exactly once, here.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

/// A NUL-terminated UTF-16 copy of `text`.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Decodes standard base64 with padding, which is what `System.Text.Json` writes for a `byte[]`.
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let text = text.trim();
    if text.is_empty() {
        return Some(Vec::new());
    }

    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let mut accumulator: u32 = 0;
    let mut bits = 0u32;
    let mut padding = 0usize;

    for &byte in bytes {
        if byte == b'=' {
            padding += 1;
            continue;
        }

        if padding > 0 {
            return None;
        }

        let value = ALPHABET.iter().position(|&candidate| candidate == byte)? as u32;
        accumulator = (accumulator << 6) | value;
        bits += 6;

        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
            accumulator &= (1 << bits) - 1;
        }
    }

    (!out.is_empty() || text.len() % 4 == 0).then_some(out)
}

/// Encodes bytes the way `System.Text.Json` writes a `byte[]`, for the tests and for fixtures.
#[cfg(test)]
fn encode_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0] as u32;
        let second = chunk.get(1).copied().unwrap_or(0) as u32;
        let third = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (first << 16) | (second << 8) | third;

        text.push(ALPHABET[(triple >> 18) as usize & 0x3F] as char);
        text.push(ALPHABET[(triple >> 12) as usize & 0x3F] as char);
        text.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3F] as char
        } else {
            '='
        });
        text.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3F] as char
        } else {
            '='
        });
    }

    text
}

/// A `Settings` layout with the auto-hide bit set, which is what a shell leaves behind.
#[cfg(test)]
pub fn hidden_layout() -> Vec<u8> {
    let mut layout: Vec<u8> = (0..44).collect();
    layout[FLAG_INDEX] |= AUTO_HIDE_BIT;
    layout
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_auto_hide_bit_is_byte_eight_bit_zero() {
        let mut layout = vec![0u8; 16];
        layout[FLAG_INDEX] = 0b0000_0010;

        assert!(!is_hidden(&layout));

        layout[FLAG_INDEX] = 0b0000_0011;
        assert!(is_hidden(&layout));
    }

    #[test]
    fn hiding_and_showing_touch_nothing_else() {
        let original: Vec<u8> = (0..44).collect();
        let with_flag = with_hidden(&original, true);
        let without_flag = with_hidden(&with_flag, false);

        assert_eq!(with_flag.len(), original.len());
        assert_eq!(with_flag[FLAG_INDEX], original[FLAG_INDEX] | AUTO_HIDE_BIT);
        assert_eq!(
            with_flag[..FLAG_INDEX],
            original[..FLAG_INDEX],
            "the bytes before the flags are untouched"
        );
        assert_eq!(
            with_flag[FLAG_INDEX + 1..],
            original[FLAG_INDEX + 1..],
            "the bytes after the flags are untouched"
        );
        assert_eq!(without_flag, original);
    }

    #[test]
    fn the_transform_is_idempotent() {
        let layout = hidden_layout();

        assert_eq!(with_hidden(&layout, true), layout);
        assert_eq!(
            with_hidden(&with_hidden(&layout, false), false),
            with_hidden(&layout, false)
        );
    }

    #[test]
    fn a_layout_too_short_to_carry_the_flag_is_left_alone() {
        for length in 0..=FLAG_INDEX {
            let short = vec![0xFFu8; length];
            assert_eq!(with_hidden(&short, true), short);
            assert_eq!(with_hidden(&short, false), short);
            assert!(!is_hidden(&short));
        }
    }

    #[test]
    fn a_restore_keeps_the_current_layout_and_moves_only_the_bit() {
        let mut current: Vec<u8> = (0..44).collect();
        current[FLAG_INDEX] = 0b0000_0110;
        let previous = hidden_layout();

        let next = plan_restore(Some(&current), &previous);

        assert_eq!(
            next,
            with_hidden(&current, true),
            "the snapshot asked for hidden"
        );
        assert_eq!(
            next[..FLAG_INDEX],
            current[..FLAG_INDEX],
            "a monitor change since the snapshot survives the repair"
        );
    }

    #[test]
    fn without_a_current_value_the_snapshot_is_written_whole() {
        let previous = hidden_layout();

        assert_eq!(plan_restore(None, &previous), previous);
    }

    #[test]
    fn a_restore_of_a_shown_taskbar_clears_the_bit() {
        let previous = with_hidden(&hidden_layout(), false);
        let current = hidden_layout();

        assert!(!is_hidden(&plan_restore(Some(&current), &previous)));
    }

    #[test]
    fn base64_round_trips_what_the_c_sharp_store_writes() {
        for bytes in [
            vec![],
            vec![0u8],
            vec![1, 2],
            vec![1, 2, 3],
            hidden_layout(),
            (0..=255).collect::<Vec<u8>>(),
        ] {
            let text = encode_base64(&bytes);
            assert_eq!(decode_base64(&text), Some(bytes.clone()), "{text}");
        }

        assert_eq!(encode_base64(b"Man"), "TWFu");
        assert_eq!(encode_base64(b"Ma"), "TWE=");
        assert_eq!(encode_base64(b"M"), "TQ==");
    }

    #[test]
    fn a_snapshot_round_trips_through_the_file_the_shell_writes() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join(STATE_FILE);
        let layout = hidden_layout();
        let json = format!(
            r#"{{"hidden":true,"previous":"{}","savedAt":"2026-11-17T10:00:00.0000000+03:00"}}"#,
            encode_base64(&layout)
        );
        std::fs::write(&path, json).expect("write");

        let snapshot = load(&path).expect("the C# shape must parse");

        assert!(snapshot.hidden);
        assert_eq!(snapshot.previous, layout);
    }

    #[test]
    fn a_snapshot_without_a_layout_is_ignored() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join(STATE_FILE);

        std::fs::write(&path, r#"{"hidden":true,"previous":""}"#).expect("write");
        assert_eq!(load(&path), None);

        std::fs::write(&path, r#"{"hidden":true}"#).expect("write");
        assert_eq!(load(&path), None);

        std::fs::write(&path, "not json").expect("write");
        assert_eq!(load(&path), None);

        assert_eq!(load(&directory.path().join("absent.json")), None);
    }

    #[test]
    fn a_leftover_snapshot_for_a_shown_taskbar_is_dropped_without_a_repair() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join(STATE_FILE);
        std::fs::write(&path, r#"{"hidden":false,"previous":"AQID"}"#).expect("write");

        assert_eq!(repair_if_orphaned(&path, false), Outcome::NotHidden);
        assert!(!path.exists(), "the leftovers are removed");
    }

    #[test]
    fn a_hidden_snapshot_is_left_to_the_shell_that_owns_it() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join(STATE_FILE);
        std::fs::write(
            &path,
            format!(
                r#"{{"hidden":true,"previous":"{}"}}"#,
                encode_base64(&hidden_layout())
            ),
        )
        .expect("write");

        assert_eq!(repair_if_orphaned(&path, true), Outcome::ShellRunning);
        assert!(path.exists(), "the live shell still owns the snapshot");
    }

    #[test]
    fn no_snapshot_means_nothing_to_do() {
        let directory = tempfile::tempdir().expect("temp dir");

        assert_eq!(
            repair_if_orphaned(&directory.path().join(STATE_FILE), false),
            Outcome::NoSnapshot
        );
    }

    #[test]
    fn clearing_a_missing_file_is_not_a_failure() {
        let directory = tempfile::tempdir().expect("temp dir");

        assert!(clear(&directory.path().join("absent.json")));
    }
}
