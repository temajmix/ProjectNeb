//! Application icons, extracted once and cached as raw BGRA pixels.
//!
//! `SHGetFileInfoW` is the only reasonable way to ask the shell for "the icon of this file", and it
//! hands back an `HICON` that has to be converted and destroyed:
//!
//! 1. `SHGetFileInfoW(SHGFI_ICON | SHGFI_LARGEICON)` → a 32×32 `HICON` owned by the caller;
//! 2. `GetIconInfo` → the colour bitmap, which is what says whether there is anything to draw;
//! 3. `DrawIconEx` into a 32×32 top-down DIB section, which is the conversion GDI does correctly and
//!    an `ID2D1Bitmap` can be made from without a WIC decoder;
//! 4. `DestroyIcon` and `DeleteObject` on everything that was created.
//!
//! The cache is keyed by path and remembers a failure as well as a success: a broken or protected
//! executable must not be retried on every refresh. The dock draws a placeholder square for `None`,
//! so a missing icon is a slower-looking tile, never a panic.

use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::{Arc, Mutex, OnceLock};

use neb_core::logging::LogHandle;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, HGDIOBJ, SelectObject,
};
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
use windows::Win32::UI::Shell::{SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGetFileInfoW};
use windows::Win32::UI::WindowsAndMessaging::{
    DI_NORMAL, DestroyIcon, DrawIconEx, GetIconInfo, HICON, ICONINFO,
};
use windows::core::PCWSTR;

/// Side of every extracted icon, in pixels.
pub const ICON_SIDE: u32 = 32;

/// How many icons the process keeps. The launcher and the dock draw the same applications, and an
/// index of several hundred shortcuts would otherwise cache a bitmap for every one of them; 128
/// covers a full dock and a screenful of results, and costs at most half a megabyte of pixels.
pub const CACHE_CAP: usize = 128;

/// Bytes in one BGRA pixel.
const BPP: usize = 4;

/// One icon as raw pixels: 32-bit BGRA, premultiplied, top-down, `ICON_SIDE` square.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconBitmap {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes of BGRA.
    pub pixels: Vec<u8>,
}

/// The icon of `path`, cached; `None` when the shell has nothing to give.
pub fn load(path: &Path) -> Option<Arc<IconBitmap>> {
    let mut cache = cache().lock().unwrap_or_else(|lock| lock.into_inner());

    if let Some(cached) = cache.get(path) {
        return cached;
    }

    let loaded = match extract(path) {
        Some(bitmap) => Some(Arc::new(bitmap)),
        None => {
            if let Some(log) = crate::log() {
                warn(
                    log,
                    &format!("no icon for '{}', drawing a placeholder", path.display()),
                );
            }
            None
        }
    };

    cache.insert(path.to_path_buf(), loaded.clone());
    loaded
}

/// Forgets every cached icon, which the DPI-change path uses when it rebuilds its bitmaps.
pub fn clear_cache() {
    cache()
        .lock()
        .unwrap_or_else(|lock| lock.into_inner())
        .clear();
}

/// How many icons are cached right now; a verification hook for the memory budget.
pub fn cached_icons() -> usize {
    cache()
        .lock()
        .unwrap_or_else(|lock| lock.into_inner())
        .len()
}

/// The letter a placeholder tile shows: the first character of `name`, upper-cased.
pub fn initial(name: &str) -> char {
    name.trim()
        .chars()
        .next()
        .map(|character| character.to_uppercase().next().unwrap_or(character))
        .unwrap_or('?')
}

/// The process-wide icon cache.
fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

    CACHE.get_or_init(|| Mutex::new(Cache::default()))
}

/// A least-recently-used map: a hit moves the path to the back, and an insert drops from the front
/// once [`CACHE_CAP`] is reached.
#[derive(Debug, Default)]
struct Cache {
    map: HashMap<PathBuf, Option<Arc<IconBitmap>>>,
    order: VecDeque<PathBuf>,
}

impl Cache {
    /// The cached answer for `path`, if there is one, marking it as recently used.
    fn get(&mut self, path: &Path) -> Option<Option<Arc<IconBitmap>>> {
        let value = self.map.get(path)?.clone();
        self.touch(path);

        Some(value)
    }

    /// Remembers `value` for `path` and evicts the oldest entry when the cap is crossed.
    fn insert(&mut self, path: PathBuf, value: Option<Arc<IconBitmap>>) {
        if self.map.insert(path.clone(), value).is_some() {
            self.touch(&path);
        } else {
            self.order.push_back(path);
        }

        while self.order.len() > CACHE_CAP {
            if let Some(oldest) = self.order.pop_front() {
                self.map.remove(&oldest);
            }
        }
    }

    /// Moves `path` to the back of the queue.
    fn touch(&mut self, path: &Path) {
        if let Some(position) = self.order.iter().position(|known| known == path)
            && let Some(known) = self.order.remove(position)
        {
            self.order.push_back(known);
        }
    }

    /// How many icons are remembered.
    fn len(&self) -> usize {
        self.map.len()
    }

    /// Forgets everything.
    fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }
}

/// Reports a skipped icon when there is a log to report it to.
fn warn(log: &LogHandle, message: &str) {
    log.warn("icons", message);
}

/// Asks the shell for the large icon of `path` and converts it.
fn extract(path: &Path) -> Option<IconBitmap> {
    let wide = wide(&path.to_string_lossy());
    let mut info = SHFILEINFOW::default();

    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the call, `info` is a live
    // out-parameter of the size passed, and the flags ask for an icon this process must destroy.
    let answered = unsafe {
        SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            FILE_ATTRIBUTE_NORMAL,
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        )
    };

    if answered == 0 || info.hIcon.0.is_null() {
        return None;
    }

    let converted = convert(info.hIcon);

    // SAFETY: the icon came from `SHGetFileInfoW` above, which transfers it to the caller, and it is
    // destroyed exactly once, here.
    unsafe {
        let _ = DestroyIcon(info.hIcon);
    }

    converted
}

/// Draws `icon` into a fresh 32×32 DIB and copies the pixels out.
fn convert(icon: HICON) -> Option<IconBitmap> {
    let mut icon_info = ICONINFO::default();

    // SAFETY: `icon` is a live icon handle and `icon_info` is a live out-parameter.
    unsafe { GetIconInfo(icon, &mut icon_info) }.ok()?;

    let converted = draw_into_dib(icon);

    // SAFETY: the two bitmaps belong to `icon_info`, which the caller of `GetIconInfo` owns; each is
    // deleted exactly once, here.
    unsafe {
        if !icon_info.hbmColor.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(icon_info.hbmColor.0));
        }
        if !icon_info.hbmMask.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(icon_info.hbmMask.0));
        }
    }

    converted
}

/// The conversion itself: a top-down 32-bit DIB, `DrawIconEx` into it, then a copy of its bits.
fn draw_into_dib(icon: HICON) -> Option<IconBitmap> {
    let side = i32::try_from(ICON_SIDE).ok()?;
    let header = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: side,
            // Negative height asks for a top-down bitmap, which is the order Direct2D expects.
            biHeight: -side,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..BITMAPINFOHEADER::default()
        },
        ..BITMAPINFO::default()
    };

    let mut bits: *mut c_void = ptr::null_mut();
    // SAFETY: `header` is a live, fully initialised bitmap description and `bits` is a live
    // out-parameter for the pixel pointer the section owns.
    let bitmap =
        unsafe { CreateDIBSection(None, &header, DIB_RGB_COLORS, &mut bits, None, 0) }.ok()?;

    if bits.is_null() {
        // SAFETY: `bitmap` was created above and is owned here.
        unsafe {
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
        }
        return None;
    }

    // SAFETY: a memory DC with no device is the documented way to draw into a DIB.
    let dc = unsafe { CreateCompatibleDC(None) };
    // SAFETY: `dc` is live and `bitmap` is a bitmap this process owns; the previous object is put
    // back before the bitmap is deleted.
    let previous = unsafe { SelectObject(dc, HGDIOBJ(bitmap.0)) };

    // SAFETY: `dc` has a 32×32 DIB selected into it, `icon` is live, and the result is the icon
    // drawn at its own size with its mask applied.
    let drawn = unsafe { DrawIconEx(dc, 0, 0, icon, side, side, 0, None, DI_NORMAL) }.is_ok();

    let pixels = if drawn {
        // SAFETY: the DIB is `ICON_SIDE * ICON_SIDE * 4` bytes of BGRA and `bits` points at it.
        let slice = unsafe {
            std::slice::from_raw_parts(
                bits.cast::<u8>(),
                (ICON_SIDE * ICON_SIDE * BPP as u32) as usize,
            )
        };
        Some(normalise(slice))
    } else {
        None
    };

    // SAFETY: the object selection is undone before the bitmap is deleted, and both are owned here.
    unsafe {
        let _ = SelectObject(dc, previous);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(dc);
    }

    let pixels = pixels?;

    Some(IconBitmap {
        width: ICON_SIDE,
        height: ICON_SIDE,
        pixels,
    })
}

/// Makes the pixels usable as a premultiplied Direct2D bitmap.
///
/// Two things can come back from `DrawIconEx`: an icon with real per-pixel alpha, which is already
/// premultiplied, and an old-style icon whose pixels are opaque but whose alpha channel is zero. The
/// second is turned into "fully opaque" rather than drawn as an invisible tile.
fn normalise(pixels: &[u8]) -> Vec<u8> {
    let mut out = pixels.to_vec();
    let opaque = out.chunks_exact(BPP).all(|pixel| pixel[3] == 0);

    if opaque {
        for pixel in out.chunks_exact_mut(BPP) {
            pixel[3] = 0xFF;
        }
    }

    out
}

/// `text` as a NUL-terminated UTF-16 buffer.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A 1×1 bitmap's worth of nothing, for a caller that needs a source and has no icon.
///
/// Not used by the dock — a placeholder is drawn from the theme instead — but a test or a future
/// caller that wants "an icon that is not there" should not have to build one by hand.
pub fn transparent_pixel() -> IconBitmap {
    IconBitmap {
        width: 1,
        height: 1,
        pixels: vec![0, 0, 0, 0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_icon_that_exists_is_extracted_as_premultiplied_bgra() {
        let Some(icon) = load(Path::new(r"C:\Windows\System32\notepad.exe")) else {
            // A machine without Notepad is not a machine this test has an opinion about.
            return;
        };

        assert_eq!(icon.width, ICON_SIDE);
        assert_eq!(icon.height, ICON_SIDE);
        assert_eq!(
            icon.pixels.len(),
            (ICON_SIDE * ICON_SIDE) as usize * BPP,
            "one 32-bit pixel per pixel"
        );
        assert!(
            icon.pixels.chunks_exact(BPP).any(|pixel| pixel[3] > 0),
            "a real icon has something visible in it"
        );
        assert!(
            icon.pixels
                .chunks_exact(BPP)
                .all(|pixel| u32::from(pixel[0]) <= u32::from(pixel[3])
                    && u32::from(pixel[1]) <= u32::from(pixel[3])
                    && u32::from(pixel[2]) <= u32::from(pixel[3])),
            "a premultiplied pixel cannot be brighter than its alpha"
        );
    }

    #[test]
    fn an_icon_is_read_from_the_cache_the_second_time() {
        let path = Path::new(r"C:\Windows\System32\notepad.exe");

        let first = load(path);
        let second = load(path);

        assert_eq!(
            first.as_ref().map(Arc::as_ptr),
            second.as_ref().map(Arc::as_ptr),
            "the same bitmap is handed out again"
        );
    }

    #[test]
    fn a_missing_file_has_no_icon() {
        assert!(load(Path::new(r"C:\Neb\There\Is\No\Such\App.exe")).is_none());
    }

    #[test]
    fn a_placeholder_shows_the_first_letter() {
        assert_eq!(initial("notepad"), 'N');
        assert_eq!(initial("  paint"), 'P');
        assert_eq!(initial("ёлка"), 'Ё');
        assert_eq!(initial(""), '?');
    }

    #[test]
    fn an_all_zero_alpha_icon_is_made_opaque_instead_of_invisible() {
        let pixels = vec![
            1, 2, 3, 0, // a pixel with colour but no alpha
            0, 0, 0, 0,
        ];

        let normalised = normalise(&pixels);

        assert_eq!(&normalised[..4], &[1, 2, 3, 255]);
        assert_eq!(&normalised[4..], &[0, 0, 0, 255]);
    }

    #[test]
    fn an_icon_that_already_has_alpha_is_left_alone() {
        let pixels = vec![1, 2, 3, 7];

        assert_eq!(normalise(&pixels), pixels);
    }

    #[test]
    fn the_transparent_pixel_is_transparent() {
        let pixel = transparent_pixel();

        assert_eq!((pixel.width, pixel.height), (1, 1));
        assert_eq!(pixel.pixels, vec![0, 0, 0, 0]);
    }

    #[test]
    fn the_icon_cache_drops_the_least_recently_used_entry() {
        let mut cache = Cache::default();
        let first = PathBuf::from(r"C:\Neb\first.exe");

        cache.insert(first.clone(), None);
        for index in 0..CACHE_CAP {
            cache.insert(PathBuf::from(format!(r"C:\Neb\{index}.exe")), None);
        }

        assert_eq!(cache.len(), CACHE_CAP, "the cache is bounded");
        assert!(
            cache.get(&first).is_none(),
            "the oldest entry was evicted by the flood"
        );
        assert_eq!(cache.len(), CACHE_CAP);
    }

    #[test]
    fn a_hit_makes_an_icon_recent_again() {
        let mut cache = Cache::default();
        let kept = PathBuf::from(r"C:\Neb\kept.exe");

        cache.insert(kept.clone(), None);
        for index in 0..CACHE_CAP - 1 {
            cache.insert(PathBuf::from(format!(r"C:\Neb\filler-{index}.exe")), None);
        }

        assert!(cache.get(&kept).is_some(), "a hit before the flood");
        cache.insert(PathBuf::from(r"C:\Neb\new.exe"), None);

        assert_eq!(cache.len(), CACHE_CAP);
        assert!(
            cache.get(&kept).is_some(),
            "being used moved it ahead of the first filler"
        );
    }

    #[test]
    fn the_capacity_is_the_documented_one_hundred_and_twenty_eight() {
        assert_eq!(CACHE_CAP, 128);
    }
}
