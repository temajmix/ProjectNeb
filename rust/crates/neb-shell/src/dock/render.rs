//! Direct2D drawing for the dock, into a layered window's DIB.
//!
//! The dock is translucent and has rounded corners, which an `ID2D1HwndRenderTarget` cannot do: it is
//! opaque by construction. The strip is therefore painted into a 32-bit top-down DIB section with an
//! `ID2D1DCRenderTarget` and handed to `UpdateLayeredWindow`, which is the only way to get real
//! per-pixel alpha out of a Win32 window without a compositor of our own.
//!
//! Everything a frame draws is created outside the frame: the brushes, the text formats, the icon
//! bitmaps, the placeholder letters and the `Ещё N` label. [`DockRenderer::paint`] therefore allocates
//! nothing once the content has settled, and a test in `tests/paint_no_alloc.rs` counts allocations
//! around a hundred dock frames to keep it that way.
//!
//! Coordinates are device-independent pixels: the target carries the monitor's DPI and the DIB is the
//! window's pixel size, so a 56 DIP tile is 56 DIP of drawing at 100 % and at 150 %.

use std::ffi::c_void;
use std::ptr;

use neb_core::strings::{Language, Text, get};
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_HOLLOW,
    D2D1_FIGURE_END_OPEN, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE,
    D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
    ID2D1Bitmap, ID2D1DCRenderTarget, ID2D1PathGeometry, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_WORD_WRAPPING_NO_WRAP, IDWriteTextFormat, IDWriteTextLayout,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, BitBlt,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HDC, HGDIOBJ,
    SRCCOPY, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, ULW_ALPHA, UpdateLayeredWindow};
use windows_numerics::{Matrix3x2, Vector2};

use super::DockItem;
use super::layout::{DockPlan, Metrics};
use crate::icons::IconBitmap;
use crate::render::{PaintOutcome, RenderError, Resources};
use crate::theme::Theme;
use crate::widgets::WideBuf;

/// Opacity of the strip's background. The brief asks for the glass colour at 88 %.
pub const GLASS_ALPHA: f32 = 0.88;

/// Opacity of the one-pixel outline. It is drawn with the theme's *text* hue rather than its stroke
/// hue: `stroke` is tuned for the bar's bottom hairline against an opaque surface, and at any alpha a
/// strip of glass over a dark wallpaper hides it completely.
pub const STROKE_ALPHA: f32 = 0.18;

/// Opacity of the highlight behind the tile under the pointer.
pub const HOVER_ALPHA: f32 = 0.10;

/// Side of an icon in device-independent pixels.
const ICON_DIP: f32 = 32.0;

/// Size of the placeholder letter, in device-independent pixels.
const LETTER_SIZE: f32 = 18.0;

/// Size of the `Ещё N` label, in device-independent pixels.
const LABEL_SIZE: f32 = 11.0;

/// The families the dock asks for, best first.
const UI_FAMILIES: [&str; 2] = ["Segoe UI Variable Text", "Segoe UI"];

/// The corner radius is clamped to what a one-row strip can carry without becoming a pill.
const MIN_RADIUS: f32 = 6.0;
const MAX_RADIUS: f32 = 26.0;

/// A 32-bit top-down DIB section and the memory DC that draws into it.
#[derive(Debug)]
struct Dib {
    hdc: HDC,
    bitmap: windows::Win32::Graphics::Gdi::HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u8,
    width: u32,
    height: u32,
}

impl Dib {
    /// Creates a `width` × `height` DIB, all zeroes, i.e. fully transparent.
    fn new(width: u32, height: u32) -> Result<Self, RenderError> {
        let width = width.max(1);
        let height = height.max(1);

        let header = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: i32::try_from(width).unwrap_or(i32::MAX),
                // Negative height: a top-down DIB, which is the row order Direct2D wants.
                biHeight: -i32::try_from(height).unwrap_or(i32::MAX),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..BITMAPINFOHEADER::default()
            },
            ..BITMAPINFO::default()
        };

        let mut bits: *mut c_void = ptr::null_mut();
        // SAFETY: `header` is a live, fully initialised description and `bits` is a live
        // out-parameter for the section's pixels, which this value owns until it is dropped.
        let bitmap = unsafe { CreateDIBSection(None, &header, DIB_RGB_COLORS, &mut bits, None, 0) }
            .map_err(|error| RenderError::call("create the dock bitmap", error))?;

        // SAFETY: a memory DC with no device is the documented way to draw into a DIB.
        let hdc = unsafe { CreateCompatibleDC(None) };
        // SAFETY: `hdc` is live and `bitmap` is ours; the previous object is put back on drop.
        let previous = unsafe { SelectObject(hdc, HGDIOBJ(bitmap.0)) };

        Ok(Self {
            hdc,
            bitmap,
            previous,
            bits: bits.cast(),
            width,
            height,
        })
    }

    /// The pixel bytes of the DIB.
    fn pixels(&self) -> &[u8] {
        let length = (self.width * self.height) as usize * 4;

        // SAFETY: the section owns `width * height * 4` bytes and the pointer is valid for as long as
        // this value exists.
        unsafe { std::slice::from_raw_parts(self.bits, length) }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        // SAFETY: the previous object belongs to this DC, and the bitmap and the DC are both owned by
        // this value and are released exactly once, here.
        unsafe {
            let _ = SelectObject(self.hdc, self.previous);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.hdc);
        }
    }
}

/// The brushes a dock frame uses, all belonging to one render target.
#[derive(Debug)]
struct Brushes {
    glass: ID2D1SolidColorBrush,
    stroke: ID2D1SolidColorBrush,
    hover: ID2D1SolidColorBrush,
    text: ID2D1SolidColorBrush,
    dim: ID2D1SolidColorBrush,
    accent: ID2D1SolidColorBrush,
}

/// What one tile needs besides its position: its icon or placeholder letter, and whether it runs.
#[derive(Debug, Default)]
struct TileArt {
    icon: Option<ID2D1Bitmap>,
    letter: Option<IDWriteTextLayout>,
    running: bool,
}

/// One dock window's drawing state.
#[derive(Debug)]
pub struct DockRenderer {
    target: ID2D1DCRenderTarget,
    dib: Dib,
    brushes: Brushes,
    letter_format: IDWriteTextFormat,
    label_format: IDWriteTextFormat,
    language: Language,
    dpi: f32,
    tiles: Vec<TileArt>,
    /// The chevron of the overflow tile, in DIPs around its own origin, so it survives a resize.
    chevron: Option<ID2D1PathGeometry>,
    label: Option<(usize, IDWriteTextLayout)>,
    /// The metrics the chevron and the label were built for; a DPI change rebuilds both.
    metrics: Option<Metrics>,
    fingerprint: u64,
    thumbnails: Vec<u8>,
}

impl DockRenderer {
    /// A renderer for a `width` × `height` strip on a monitor of `dpi`.
    ///
    /// # Errors
    ///
    /// [`RenderError`] when Direct2D, DirectWrite or GDI refuses.
    pub fn new(
        width: u32,
        height: u32,
        dpi: u32,
        theme: &Theme,
        language: Language,
    ) -> Result<Self, RenderError> {
        let resources = Resources::shared()?;
        let dpi = dpi.max(96) as f32;

        let properties = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                // Premultiplied: it is what Direct2D writes for a layered window and what
                // `UpdateLayeredWindow` is given.
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: dpi,
            dpiY: dpi,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };

        // SAFETY: the property structure is live and fully initialised; the factory is shared and
        // single-threaded, and this renderer never leaves the UI thread.
        let target = unsafe { resources.factory().CreateDCRenderTarget(&properties) }
            .map_err(|error| RenderError::call("create a dock render target", error))?;

        // SAFETY: `target` is live; grayscale antialiasing is what a near-black translucent surface
        // wants, because ClearType's sub-pixel fringes read as colour on it.
        unsafe {
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        }

        let brush = |colour: &D2D1_COLOR_F| -> Result<ID2D1SolidColorBrush, RenderError> {
            // SAFETY: the colour outlives the call and `None` means an identity transform at full
            // opacity, which is what every brush here wants.
            unsafe { target.CreateSolidColorBrush(colour, None) }
                .map_err(|error| RenderError::call("create a dock brush", error))
        };

        let brushes = Brushes {
            glass: brush(&hue(&theme.background, GLASS_ALPHA))?,
            stroke: brush(&hue(&theme.text, STROKE_ALPHA))?,
            hover: brush(&hue(&theme.text, HOVER_ALPHA))?,
            text: brush(&theme.text)?,
            dim: brush(&theme.text_dim)?,
            accent: brush(&theme.accent)?,
        };

        let locale = crate::render::user_locale();
        let letter_format = crate::render::text_format(
            resources.dwrite(),
            &locale,
            &UI_FAMILIES,
            LETTER_SIZE,
            DWRITE_FONT_WEIGHT_SEMI_BOLD,
        )?;
        let label_format = crate::render::text_format(
            resources.dwrite(),
            &locale,
            &UI_FAMILIES,
            LABEL_SIZE,
            DWRITE_FONT_WEIGHT_SEMI_BOLD,
        )?;

        // SAFETY: both formats are live interfaces owned by this value.
        unsafe {
            let _ = label_format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            let _ = label_format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
            let _ = label_format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            let _ = letter_format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            let _ = letter_format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
            let _ = letter_format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
        }

        Ok(Self {
            target,
            dib: Dib::new(width, height)?,
            brushes,
            letter_format,
            label_format,
            language,
            dpi,
            tiles: Vec::new(),
            chevron: None,
            label: None,
            metrics: None,
            fingerprint: 0,
            thumbnails: Vec::new(),
        })
    }

    /// The DPI the strip is drawn at.
    pub fn dpi(&self) -> u32 {
        self.dpi as u32
    }

    /// The pixel size of the bitmap behind the strip.
    pub fn pixel_size(&self) -> (u32, u32) {
        (self.dib.width, self.dib.height)
    }

    /// Replaces the item list and the overflow furniture, rebuilding only what actually changed.
    ///
    /// The icons, the placeholder letters, the `Ещё N` label and the chevron are created here — never
    /// in a frame — and a refresh that produces the same items costs one hash and no Direct2D work.
    fn sync(&mut self, items: &[DockItem], plan: &DockPlan) {
        let fingerprint = fingerprint(items);

        if fingerprint != self.fingerprint {
            let tiles: Vec<TileArt> = items.iter().map(|item| self.tile_art(item)).collect();
            self.tiles = tiles;
            self.fingerprint = fingerprint;
        }

        if self.metrics != Some(plan.metrics) {
            self.metrics = Some(plan.metrics);
            self.chevron = self.build_chevron();
        }

        if self
            .label
            .as_ref()
            .is_none_or(|(count, _)| *count != plan.overflow)
        {
            self.label = self.build_label(plan);
        }
    }

    /// The Direct2D bitmap and placeholder letter for one item.
    fn tile_art(&self, item: &DockItem) -> TileArt {
        let icon = item
            .path
            .as_deref()
            .and_then(crate::icons::load)
            .and_then(|icon| self.icon_bitmap(&icon).ok());

        let letter = if icon.is_some() {
            None
        } else {
            let mut text: WideBuf<4> = WideBuf::new();
            text.push(crate::icons::initial(&item.name));
            self.letter_layout(text.as_units()).ok()
        };

        TileArt {
            icon,
            letter,
            running: item.running,
        }
    }

    /// Uploads one extracted icon as a Direct2D bitmap.
    fn icon_bitmap(&self, icon: &IconBitmap) -> Result<ID2D1Bitmap, RenderError> {
        let properties = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: self.dpi,
            dpiY: self.dpi,
        };

        // SAFETY: the pixel buffer outlives the call and is exactly `pitch * height` bytes, which is
        // what Direct2D copies out of it.
        unsafe {
            self.target.CreateBitmap(
                D2D_SIZE_U {
                    width: icon.width,
                    height: icon.height,
                },
                Some(icon.pixels.as_ptr().cast()),
                icon.width * 4,
                &properties,
            )
        }
        .map_err(|error| RenderError::call("create an icon bitmap", error))
    }

    /// One placeholder letter as a layout.
    fn letter_layout(&self, text: &[u16]) -> Result<IDWriteTextLayout, RenderError> {
        let side = ICON_DIP * self.scale();

        // SAFETY: `text` is live UTF-16, the format is live, and the two maxima are plain values.
        unsafe {
            Resources::shared()?
                .dwrite()
                .CreateTextLayout(text, &self.letter_format, side, side)
        }
        .map_err(|error| RenderError::call("create a placeholder layout", error))
    }

    /// The `Ещё N` label, or `None` when nothing overflows.
    fn build_label(&self, plan: &DockPlan) -> Option<(usize, IDWriteTextLayout)> {
        if plan.overflow == 0 {
            return None;
        }

        let mut text: WideBuf<32> = WideBuf::new();
        text.push_str(get(self.language, Text::DockMore));
        text.push(' ');
        text.push_number(u32::try_from(plan.overflow).unwrap_or(u32::MAX));

        // Wide enough for a three-digit count in either language, tall enough for the descender of
        // `Ещё`, both in the strip's own scale.
        let width = plan.metrics.tile as f32 * self.scale();
        let height = 24.0 * self.scale();

        // SAFETY: `text` lives for the call, the format is live, and the maxima are plain values.
        let layout = unsafe {
            Resources::shared().ok()?.dwrite().CreateTextLayout(
                text.as_units(),
                &self.label_format,
                width,
                height,
            )
        }
        .ok()?;

        Some((plan.overflow, layout))
    }

    /// The up-chevron of the overflow tile, built once around its own origin in DIPs.
    ///
    /// A path geometry rather than two rectangles: a chevron is two slanted strokes, and drawing it
    /// from axis-aligned rectangles leaves two bars that read as an exclamation mark.
    fn build_chevron(&self) -> Option<ID2D1PathGeometry> {
        let resources = Resources::shared().ok()?;

        // SAFETY: the factory is live, and the sink is closed before the geometry is used.
        unsafe {
            let geometry = resources.factory().CreatePathGeometry().ok()?;
            let sink = geometry.Open().ok()?;
            sink.BeginFigure(Vector2 { X: -5.0, Y: 2.0 }, D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddLines(&[Vector2 { X: 0.0, Y: -2.5 }, Vector2 { X: 5.0, Y: 2.0 }]);
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            sink.Close().ok()?;

            Some(geometry)
        }
    }

    /// DIPs per pixel, so the corner radius and the outline keep their physical weight.
    fn scale(&self) -> f32 {
        96.0 / self.dpi
    }

    /// Draws one frame and hands it to the window.
    ///
    /// The item list is synced first, which is cheap when nothing changed, so a caller cannot ask for
    /// a frame whose icons do not match its items.
    pub fn paint(
        &mut self,
        hwnd: HWND,
        plan: &DockPlan,
        items: &[DockItem],
        hover: Option<usize>,
    ) -> PaintOutcome {
        if let Err(error) = self.resize(
            u32::try_from(plan.width).unwrap_or(1),
            u32::try_from(plan.height).unwrap_or(1),
        ) {
            return PaintOutcome::Failed(error);
        }

        self.sync(items, plan);

        match self.draw(plan, hover) {
            Ok(()) => {}
            Err(error) if error.is_recreate() => return PaintOutcome::Recreate,
            Err(error) => return PaintOutcome::Failed(error),
        }

        match self.present(hwnd) {
            Ok(()) => PaintOutcome::Painted,
            Err(error) => PaintOutcome::Failed(error),
        }
    }

    /// Re-makes the bitmap when the strip changed size.
    fn resize(&mut self, width: u32, height: u32) -> Result<(), RenderError> {
        if (width.max(1), height.max(1)) == (self.dib.width, self.dib.height) {
            return Ok(());
        }

        self.dib = Dib::new(width, height)?;
        Ok(())
    }

    /// Draws the whole strip: glass, outline and one tile per slot.
    fn draw(&self, plan: &DockPlan, hover: Option<usize>) -> Result<(), RenderError> {
        let metrics = plan.metrics;
        let scale = self.scale();
        let width = plan.width as f32 * scale;
        let height = plan.height as f32 * scale;
        let radius = (metrics.tile as f32 * 0.32).clamp(MIN_RADIUS, MAX_RADIUS) * scale;

        let bounds = D2D_RECT_F {
            left: 0.0,
            top: 0.0,
            right: width,
            bottom: height,
        };
        let strip = D2D1_ROUNDED_RECT {
            rect: bounds,
            radiusX: radius,
            radiusY: radius,
        };

        let frame = RECT {
            left: 0,
            top: 0,
            right: plan.width,
            bottom: plan.height,
        };

        // SAFETY: the DIB is live and at least `plan.width × plan.height` pixels, every brush belongs
        // to this target, and the whole frame is inside one `BeginDraw`/`EndDraw` pair.
        unsafe {
            self.target
                .BindDC(self.dib.hdc, &frame)
                .map_err(|error| RenderError::call("bind the dock bitmap", error))?;
            self.target.BeginDraw();
            self.target.Clear(Some(&D2D1_COLOR_F::default()));

            self.target
                .FillRoundedRectangle(&strip, &self.brushes.glass);
            self.target
                .DrawRoundedRectangle(&strip, &self.brushes.stroke, scale, None);

            for slot in 0..plan.slots() {
                let left = metrics.slot_left(slot) as f32 * scale;
                let top = (metrics.border + metrics.padding) as f32 * scale;
                let side = metrics.tile as f32 * scale;

                if hover == Some(slot) {
                    let tile = D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left,
                            top,
                            right: left + side,
                            bottom: top + side,
                        },
                        radiusX: side * 0.22,
                        radiusY: side * 0.22,
                    };
                    self.target.FillRoundedRectangle(&tile, &self.brushes.hover);
                }

                if slot >= plan.visible {
                    self.draw_chevron(left, top, side);
                    continue;
                }

                self.draw_tile(slot, left, top, side, metrics);
            }

            self.target.EndDraw(None, None)
        }
        .map_err(|error| RenderError::call("end a dock frame", error))
    }

    /// One tile: its icon or placeholder, and the running dot underneath.
    ///
    /// # Safety
    ///
    /// Must be called inside an open draw call on this renderer's target.
    unsafe fn draw_tile(&self, slot: usize, left: f32, top: f32, side: f32, metrics: Metrics) {
        let scale = self.scale();
        let Some(art) = self.tiles.get(slot) else {
            return;
        };

        let icon_left = left + (side - ICON_DIP) / 2.0;
        let icon_top = top + (side - ICON_DIP) / 2.0;

        // SAFETY: the caller guarantees an open draw call on this target.
        unsafe {
            match &art.icon {
                Some(bitmap) => {
                    let destination = D2D_RECT_F {
                        left: icon_left,
                        top: icon_top,
                        right: icon_left + ICON_DIP,
                        bottom: icon_top + ICON_DIP,
                    };
                    self.target.DrawBitmap(
                        bitmap,
                        Some(&destination),
                        1.0,
                        D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                        None,
                    );
                }
                None => {
                    // A placeholder square with the app's initial, so a missing icon still reads as
                    // an application rather than as a hole in the strip.
                    let square = D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: icon_left,
                            top: icon_top,
                            right: icon_left + ICON_DIP,
                            bottom: icon_top + ICON_DIP,
                        },
                        radiusX: ICON_DIP * 0.22,
                        radiusY: ICON_DIP * 0.22,
                    };
                    self.target
                        .FillRoundedRectangle(&square, &self.brushes.hover);

                    if let Some(letter) = art.letter.as_ref() {
                        self.target.DrawTextLayout(
                            Vector2 {
                                X: icon_left,
                                Y: icon_top,
                            },
                            letter,
                            &self.brushes.text,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
                }
            }

            if art.running {
                let dot = metrics.dot as f32 * scale;
                let centre = left + side / 2.0;
                let bottom = top + side + dot;
                let circle = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: centre - dot / 2.0,
                        top: bottom - dot,
                        right: centre + dot / 2.0,
                        bottom,
                    },
                    radiusX: dot / 2.0,
                    radiusY: dot / 2.0,
                };
                self.target
                    .FillRoundedRectangle(&circle, &self.brushes.accent);
            }
        }
    }

    /// The `Ещё N` overflow tile: an up-chevron over the count.
    ///
    /// # Safety
    ///
    /// Must be called inside an open draw call on this renderer's target.
    unsafe fn draw_chevron(&self, left: f32, top: f32, side: f32) {
        let scale = self.scale();
        let centre = left + side / 2.0;

        // SAFETY: the caller guarantees an open draw call on this target, and the geometry is live
        // for as long as this renderer is.
        unsafe {
            if let Some(chevron) = self.chevron.as_ref() {
                self.target.SetTransform(&Matrix3x2 {
                    M11: 1.0,
                    M12: 0.0,
                    M21: 0.0,
                    M22: 1.0,
                    M31: centre,
                    M32: top + 16.0 * scale,
                });
                self.target
                    .DrawGeometry(chevron, &self.brushes.dim, 1.6 * scale, None);
                self.target.SetTransform(&Matrix3x2 {
                    M11: 1.0,
                    M12: 0.0,
                    M21: 0.0,
                    M22: 1.0,
                    M31: 0.0,
                    M32: 0.0,
                });
            }

            if let Some((_, label)) = self.label.as_ref() {
                self.target.DrawTextLayout(
                    Vector2 {
                        X: left,
                        Y: top + 30.0 * scale,
                    },
                    label,
                    &self.brushes.dim,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
        }
    }

    /// Hands the DIB to the window, which is what actually puts the strip on screen.
    fn present(&self, hwnd: HWND) -> Result<(), RenderError> {
        let mut window = RECT::default();
        // SAFETY: `hwnd` is a live window of this process and `window` is a live out-parameter.
        unsafe { GetWindowRect(hwnd, &mut window) }
            .map_err(|error| RenderError::call("read the dock rectangle", error))?;

        let destination = POINT {
            x: window.left,
            y: window.top,
        };
        let size = SIZE {
            cx: i32::try_from(self.dib.width).unwrap_or(i32::MAX),
            cy: i32::try_from(self.dib.height).unwrap_or(i32::MAX),
        };
        let source = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };

        // SAFETY: `hwnd` is layered, the DIB is `size` pixels and outlives the call, and the blend
        // function asks for the per-pixel alpha the DIB carries.
        unsafe {
            UpdateLayeredWindow(
                hwnd,
                None,
                Some(&destination),
                Some(&size),
                Some(self.dib.hdc),
                Some(&source),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        }
        .map_err(|error| RenderError::call("present the dock", error))
    }

    /// Copies the last frame into `target`, which is how `WM_PRINTCLIENT` answers a screenshot.
    pub fn blit_to(&self, target: HDC) -> bool {
        // SAFETY: `target` is the device context Windows handed to the window and the DIB is live.
        unsafe {
            BitBlt(
                target,
                0,
                0,
                i32::try_from(self.dib.width).unwrap_or(i32::MAX),
                i32::try_from(self.dib.height).unwrap_or(i32::MAX),
                Some(self.dib.hdc),
                0,
                0,
                SRCCOPY,
            )
        }
        .is_ok()
    }

    /// The last frame's pixels, BGRA premultiplied, top-down.
    pub fn pixels(&self) -> &[u8] {
        &self.thumbnails
    }

    /// Copies the frame out for a caller that needs the pixels after the frame is gone.
    ///
    /// This allocates, so it is a verification hook — `--probe-dock` and the tests use it — and never
    /// part of a paint.
    pub fn capture(&mut self) {
        self.thumbnails = self.dib.pixels().to_vec();
    }
}

/// A hash of everything a tile's art depends on, so an unchanged refresh rebuilds nothing.
fn fingerprint(items: &[DockItem]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;

    let mix = |hash: &mut u64, byte: u8| {
        *hash ^= u64::from(byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    };

    for item in items {
        for byte in item.name.as_bytes() {
            mix(&mut hash, *byte);
        }
        match &item.path {
            Some(path) => {
                for byte in path.to_string_lossy().as_bytes() {
                    mix(&mut hash, byte.to_ascii_lowercase());
                }
            }
            None => mix(&mut hash, 0xFF),
        }
        for byte in item.target.unwrap_or(0).to_le_bytes() {
            mix(&mut hash, byte);
        }
        mix(&mut hash, u8::from(item.running));
    }

    hash
}

/// The colour of `source` with `alpha`, which is how a token's hue survives a fixed opacity.
fn hue(source: &D2D1_COLOR_F, alpha: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: source.r,
        g: source.g,
        b: source.b,
        a: alpha,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn item(name: &str, target: Option<isize>, running: bool) -> DockItem {
        DockItem {
            name: name.to_owned(),
            path: Some(PathBuf::from(r"C:\Windows\System32\notepad.exe")),
            target,
            pinned: true,
            running,
        }
    }

    #[test]
    fn a_hue_keeps_its_colour_and_takes_the_new_alpha() {
        let source = D2D1_COLOR_F {
            r: 0.1,
            g: 0.2,
            b: 0.3,
            a: 0.92,
        };

        let glass = hue(&source, GLASS_ALPHA);

        assert_eq!((glass.r, glass.g, glass.b), (0.1, 0.2, 0.3));
        assert!((glass.a - GLASS_ALPHA).abs() < f32::EPSILON);
    }

    #[test]
    fn the_fingerprint_follows_the_items_and_nothing_else() {
        let items = vec![item("notepad", Some(0x10), true)];
        let same = items.clone();
        let mut other = items.clone();
        other[0].running = false;

        assert_eq!(fingerprint(&items), fingerprint(&same));
        assert_ne!(fingerprint(&items), fingerprint(&other));
        assert_ne!(fingerprint(&items), fingerprint(&[]));
    }
}
