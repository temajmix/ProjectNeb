//! The one registry value that is bytes rather than a number: the taskbar layout.
//!
//! Explorer keeps it in `HKCU\...\Explorer\StuckRects3` value `Settings`. Byte 8 is a flags field
//! and its lowest bit means "hide the taskbar when it is not used". Everything else - the edge, the
//! monitor block, the always-on-top bit - belongs to the user and is never touched here.
//!
//! The transform is shared by two callers: the crash repair in `neb-supervisor` (a shell died with
//! the taskbar hidden) and the Windows replacement mode (the user asked for a taskbar without
//! chrome). Both write the same bit, so keeping one implementation is what makes the two agree.

/// Registry key the Explorer taskbar layout lives in.
pub const KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StuckRects3";

/// Value inside [`KEY_PATH`] carrying the layout bytes.
pub const VALUE_NAME: &str = "Settings";

/// Index of the flags byte inside the layout.
pub const FLAG_INDEX: usize = 8;

/// The bit inside [`FLAG_INDEX`] that means "hide the taskbar when it is not used".
pub const AUTO_HIDE_BIT: u8 = 0x01;

/// Whether `settings` asks for an auto-hidden taskbar.
pub fn is_hidden(settings: &[u8]) -> bool {
    settings
        .get(FLAG_INDEX)
        .is_some_and(|flags| flags & AUTO_HIDE_BIT != 0)
}

/// A copy of `settings` with only the auto-hide bit written.
///
/// Everything else is left exactly as it was, and a value too short to carry the flag is returned
/// unchanged: guessing the rest of the layout would be worse than leaving the taskbar alone.
/// Idempotent by construction.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn hidden_layout() -> Vec<u8> {
        let mut layout: Vec<u8> = (0..44).collect();
        layout[FLAG_INDEX] |= AUTO_HIDE_BIT;
        layout
    }

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
    fn the_flag_is_clamped_to_its_own_bit() {
        let mut all_bits = vec![0u8; FLAG_INDEX + 1];
        all_bits[FLAG_INDEX] = 0xFF;

        let hidden = with_hidden(&all_bits, true);
        let shown = with_hidden(&all_bits, false);

        assert_eq!(
            hidden[FLAG_INDEX], 0xFF,
            "setting a bit that is already set changes nothing"
        );
        assert_eq!(
            shown[FLAG_INDEX], 0xFE,
            "clearing the bit must keep every other flag"
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
}
