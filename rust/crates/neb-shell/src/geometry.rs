//! Rectangles and monitors, in physical pixels, because that is what Win32 and the AppBar API speak.
//!
//! Every other module works in device-independent pixels for drawing; this one is the seam where the
//! bar learns where it is on real hardware and how much of a DIP a physical pixel is worth.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

/// The DPI of a display that is not scaled.
pub const BASE_DPI: u32 = 96;

/// An integer rectangle in physical pixels: `left`/`top` inclusive, `right`/`bottom` exclusive.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rect {
    /// Left edge.
    pub left: i32,
    /// Top edge.
    pub top: i32,
    /// Right edge, exclusive.
    pub right: i32,
    /// Bottom edge, exclusive.
    pub bottom: i32,
}

impl Rect {
    /// A rectangle from its edges.
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// A rectangle from its origin and size, with the size forced positive.
    pub fn from_size(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self::new(x, y, x + width.max(1), y + height.max(1))
    }

    /// Width in physical pixels.
    pub fn width(&self) -> i32 {
        (self.right - self.left).max(1)
    }

    /// Height in physical pixels.
    pub fn height(&self) -> i32 {
        (self.bottom - self.top).max(1)
    }

    /// The Win32 structure the AppBar API and `SetWindowPos` take.
    pub const fn to_win32(self) -> RECT {
        RECT {
            left: self.left,
            top: self.top,
            right: self.right,
            bottom: self.bottom,
        }
    }
}

impl From<RECT> for Rect {
    fn from(rect: RECT) -> Self {
        Self::new(rect.left, rect.top, rect.right, rect.bottom)
    }
}

/// One monitor, as the bar needs to see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monitor {
    /// The `HMONITOR` the display was enumerated from, as a plain integer so it can travel.
    pub handle: isize,
    /// Device name such as `\\.\DISPLAY1`, for log lines.
    pub device: String,
    /// The full monitor rectangle in physical pixels.
    pub rect: Rect,
    /// The work area, i.e. the monitor minus the taskbar and any reserved strip.
    pub work: Rect,
    /// Digits per inch, so a 40 DIP bar can be sized in physical pixels.
    pub dpi: u32,
}

impl Monitor {
    /// The bar's thickness in physical pixels for a request in DIPs.
    pub fn to_pixels(&self, dip: i32) -> i32 {
        let scaled = i64::from(dip.max(1)) * i64::from(self.dpi) / i64::from(BASE_DPI);

        i32::try_from(scaled).unwrap_or(i32::MAX).max(1)
    }

    /// DIPs per physical pixel, for the renderer.
    pub fn scale(&self) -> f32 {
        self.dpi as f32 / BASE_DPI as f32
    }
}

/// Every monitor this process can draw a bar on, in the order Windows enumerates them.
pub fn monitors() -> Vec<Monitor> {
    let mut collected: Vec<Monitor> = Vec::new();
    let pointer = &raw mut collected as isize;

    // SAFETY: `enumerate` is a valid `MONITORENUMPROC` and `pointer` is the address of a `Vec` that
    // outlives the call; `EnumDisplayMonitors` only ever hands it back to that callback.
    let _ = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(enumerate),
            windows::Win32::Foundation::LPARAM(pointer),
        )
    };

    collected
}

/// Collects one monitor into the vector the enumeration was started with.
///
/// # Safety
///
/// `data` must be the address of a live `Vec<Monitor>` for the whole enumeration.
unsafe extern "system" fn enumerate(
    monitor: HMONITOR,
    _device_context: HDC,
    _rect: *mut RECT,
    data: windows::Win32::Foundation::LPARAM,
) -> windows::core::BOOL {
    let collected = data.0 as *mut Vec<Monitor>;

    if collected.is_null() {
        return false.into();
    }

    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;

    // SAFETY: `info` is live and its `cbSize` describes the larger `MONITORINFOEXW` this cast
    // starts from, which is the documented way to ask `GetMonitorInfoW` for the device name.
    let filled = unsafe { GetMonitorInfoW(monitor, &raw mut info.monitorInfo) };

    if !filled.as_bool() {
        return true.into();
    }

    let length = info
        .szDevice
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(info.szDevice.len());
    let device = String::from_utf16_lossy(&info.szDevice[..length]);

    // SAFETY: `collected` was derived from a live `Vec<Monitor>` the caller keeps for the whole call;
    // the callback runs on the calling thread, so no other reference exists at this point.
    unsafe {
        (*collected).push(Monitor {
            handle: monitor.0 as isize,
            device,
            rect: Rect::from(info.monitorInfo.rcMonitor),
            work: Rect::from(info.monitorInfo.rcWork),
            dpi: effective_dpi(monitor),
        });
    }

    true.into()
}

/// The monitor's effective DPI, or [`BASE_DPI`] when the shell refuses to say.
fn effective_dpi(monitor: HMONITOR) -> u32 {
    let mut x = BASE_DPI;
    let mut y = BASE_DPI;

    // SAFETY: both out-parameters are live; the call fails harmlessly on a system without shcore.
    let _ = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) };

    x.max(BASE_DPI)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(dpi: u32) -> Monitor {
        Monitor {
            handle: 1,
            device: r"\\.\DISPLAY1".to_owned(),
            rect: Rect::new(0, 0, 1920, 1080),
            work: Rect::new(0, 0, 1920, 1032),
            dpi,
        }
    }

    #[test]
    fn a_rect_reports_positive_dimensions_even_when_it_is_degenerate() {
        assert_eq!(Rect::new(0, 0, 1920, 40).width(), 1920);
        assert_eq!(Rect::new(0, 0, 1920, 40).height(), 40);
        assert_eq!(Rect::new(5, 5, 5, 5).width(), 1);
        assert_eq!(Rect::from_size(10, 20, 0, 0), Rect::new(10, 20, 11, 21));
    }

    #[test]
    fn a_rect_round_trips_through_the_win32_structure() {
        let rect = Rect::new(-1920, 0, 0, 1080);

        assert_eq!(Rect::from(rect.to_win32()), rect);
    }

    #[test]
    fn a_dip_thickness_scales_with_the_monitor() {
        assert_eq!(monitor(96).to_pixels(40), 40);
        assert_eq!(monitor(120).to_pixels(40), 50);
        assert_eq!(monitor(144).to_pixels(40), 60);
        assert_eq!(monitor(96).to_pixels(0), 1, "a strip is never zero pixels");
    }

    #[test]
    fn the_scale_is_the_dpi_ratio() {
        assert!((monitor(96).scale() - 1.0).abs() < f32::EPSILON);
        assert!((monitor(144).scale() - 1.5).abs() < f32::EPSILON);
    }

    #[test]
    fn this_machine_has_at_least_one_monitor_with_a_sane_geometry() {
        let monitors = monitors();

        assert!(!monitors.is_empty(), "a desktop session has a monitor");
        for monitor in monitors {
            assert!(monitor.rect.width() > 0 && monitor.rect.height() > 0);
            assert!(monitor.dpi >= BASE_DPI, "{monitor:?}");
            assert!(!monitor.device.is_empty(), "{monitor:?}");
            assert!(
                monitor.work.left >= monitor.rect.left && monitor.work.top >= monitor.rect.top,
                "the work area is inside the monitor: {monitor:?}"
            );
        }
    }
}
