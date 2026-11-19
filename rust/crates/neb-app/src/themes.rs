//! Where the shell's themes live, and the colour spelling the settings window shares.
//!
//! The theme documents themselves belong to `neb_core::theming`: the six presets, the package
//! format, `ThemeStore` and `ProfileStore` are the single source of truth for the bridge and the
//! shell. This module keeps only the folder rule (the shell and the bridge must agree on it) and
//! the `#RRGGBB` check the wallpaper colour control reuses.

use std::path::{Path, PathBuf};

/// The themes folder under a ProjectNeb data root, the same one `neb-shell` reads.
pub fn dir(root: &Path) -> PathBuf {
    root.join("themes")
}

/// Whether `value` is a `#RRGGBB` colour, the only spelling the theme and wallpaper files accept.
pub fn is_hex_color(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 7 && bytes[0] == b'#' && bytes[1..].iter().all(u8::is_ascii_hexdigit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_themes_folder_sits_under_the_data_root() {
        assert_eq!(dir(Path::new(r"C:\Neb")), PathBuf::from(r"C:\Neb\themes"));
    }

    #[test]
    fn only_a_six_digit_hex_colour_passes() {
        assert!(is_hex_color("#8A2BE2"));
        assert!(!is_hex_color("8A2BE2"));
        assert!(!is_hex_color("#8A2BE"));
        assert!(!is_hex_color("#8A2BE20"));
        assert!(!is_hex_color("#GGGGGG"));
    }
}
