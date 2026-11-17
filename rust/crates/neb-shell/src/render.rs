//! Direct2D painting for the bar: one factory, one render target per window, cached everything.
//!
//! The rules the module exists to satisfy:
//!
//! * the Direct2D and DirectWrite factories are created once for the process ([`Resources`]);
//! * the render target, its brushes, the text formats and the text layouts are created outside
//!   [`Renderer::paint`], which therefore draws a frame without allocating;
//! * `WM_PAINT` is the only caller of [`Renderer::paint`], so a frame is drawn when Windows asks for
//!   one and at no other time;
//! * a lost target (`D2DERR_RECREATE_TARGET`, which a driver update or a session lock causes) is
//!   reported as [`PaintOutcome::Recreate`] and rebuilt on the next frame instead of being ignored.
//!
//! Coordinates are device-independent pixels throughout: the render target is created at the
//! monitor's DPI, so a 40 DIP bar is 40 DIP of drawing on a 100 % and on a 150 % display alike.

use std::sync::OnceLock;

use windows::Win32::Foundation::HWND;
use windows::Win32::Globalization::GetUserDefaultLocaleName;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory, ID2D1Factory, ID2D1HwndRenderTarget,
    ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_FEATURE, DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_PARAGRAPH_ALIGNMENT_NEAR,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_METRICS, DWRITE_TEXT_RANGE, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory,
    IDWriteFactory, IDWriteFontCollection, IDWriteTextFormat, IDWriteTextLayout,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::core::PCWSTR;
use windows_numerics::Vector2;

use crate::theme::Theme;
use crate::widgets::BarText;

/// Padding between the bar's edge and its first widget, in DIPs.
pub const PADDING: f32 = 16.0;

/// Side of the square the launcher glyph is drawn in.
pub const GLYPH: f32 = 16.0;

/// Space between the launcher glyph and the profile name, and between the metrics and the gear.
pub const GAP: f32 = 8.0;

/// Side of the gear glyph.
pub const GEAR: f32 = 8.0;

/// Thickness of the hairline along the bar's bottom edge.
pub const BORDER: f32 = 1.0;

/// Bar height below which the two-line clock does not fit and the date is hidden.
pub const TWO_LINE_MIN_HEIGHT: f32 = 34.0;

/// `D2DERR_RECREATE_TARGET`: the device went away and everything drawn with it is worthless.
const D2DERR_RECREATE_TARGET: i32 = 0x8899_000C_u32 as i32;

/// What a paint call did.
#[derive(Debug)]
pub enum PaintOutcome {
    /// The frame is on screen.
    Painted,
    /// The render target must be dropped and rebuilt before the next frame.
    Recreate,
    /// The frame could not be drawn; the target is still usable.
    Failed(RenderError),
}

/// Why rendering is unavailable.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// The factory could not be created, so no window can draw at all.
    #[error("direct2d is unavailable: {0}")]
    Unavailable(String),
    /// A Direct2D or DirectWrite call refused its arguments.
    #[error("{action}: {source}")]
    Call {
        /// What was being attempted.
        action: &'static str,
        /// The refusal.
        #[source]
        source: windows::core::Error,
    },
}

impl RenderError {
    /// Wraps a Win32 failure with the action that produced it.
    pub(crate) fn call(action: &'static str, source: windows::core::Error) -> Self {
        Self::Call { action, source }
    }

    /// Whether Direct2D said the device is gone and everything must be rebuilt.
    pub(crate) fn is_recreate(&self) -> bool {
        matches!(self, Self::Call { source, .. } if source.code().0 == D2DERR_RECREATE_TARGET)
    }
}

/// The process-wide Direct2D and DirectWrite state.
///
/// Both factories are documented as safe to share between threads; this process only ever uses them
/// from the UI thread, which is why the Direct2D factory is created single-threaded.
#[derive(Debug)]
pub struct Resources {
    factory: ID2D1Factory,
    dwrite: IDWriteFactory,
    fonts: Fonts,
}

/// The text formats the bar draws with, created once per process.
#[derive(Debug)]
struct Fonts {
    clock: IDWriteTextFormat,
    date: IDWriteTextFormat,
    metrics: IDWriteTextFormat,
    profile: IDWriteTextFormat,
}

/// The families the bar asks for, best first; each is tried until one exists.
const UI_FAMILIES: [&str; 2] = ["Segoe UI Variable Text", "Segoe UI"];
/// The metrics readout wants fixed-width digits, but not at the price of a missing glyph.
const MONO_FAMILIES: [&str; 3] = ["Cascadia Mono", "Consolas", "Segoe UI"];

/// Font sizes, matching the C# bar.
const CLOCK_SIZE: f32 = 13.0;
const DATE_SIZE: f32 = 10.0;
const METRICS_SIZE: f32 = 11.0;
const PROFILE_SIZE: f32 = 12.0;

static RESOURCES: OnceLock<Result<Resources, String>> = OnceLock::new();

impl Resources {
    /// The shared factories, created on first use.
    ///
    /// A failure is cached too: a machine without Direct2D must report that once, not retry the
    /// factory for every window and every frame.
    pub fn shared() -> Result<&'static Self, RenderError> {
        match RESOURCES.get_or_init(|| Resources::create().map_err(|error| error.to_string())) {
            Ok(resources) => Ok(resources),
            Err(message) => Err(RenderError::Unavailable(message.clone())),
        }
    }

    fn create() -> Result<Self, RenderError> {
        // SAFETY: neither call takes a pointer into this process's memory, and each returns a
        // reference-counted COM object owned by the returned interface.
        let factory: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }
                .map_err(|error| RenderError::call("create the Direct2D factory", error))?;
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }
            .map_err(|error| RenderError::call("create the DirectWrite factory", error))?;

        let locale = user_locale();
        let fonts = Fonts {
            clock: text_format(
                &dwrite,
                &locale,
                &UI_FAMILIES,
                CLOCK_SIZE,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
            )?,
            date: text_format(
                &dwrite,
                &locale,
                &UI_FAMILIES,
                DATE_SIZE,
                DWRITE_FONT_WEIGHT_NORMAL,
            )?,
            metrics: text_format(
                &dwrite,
                &locale,
                &MONO_FAMILIES,
                METRICS_SIZE,
                DWRITE_FONT_WEIGHT_NORMAL,
            )?,
            profile: text_format(
                &dwrite,
                &locale,
                &UI_FAMILIES,
                PROFILE_SIZE,
                DWRITE_FONT_WEIGHT_NORMAL,
            )?,
        };

        Ok(Self {
            factory,
            dwrite,
            fonts,
        })
    }

    /// The DirectWrite factory, for the layout cache.
    pub(crate) fn dwrite(&self) -> &IDWriteFactory {
        &self.dwrite
    }

    /// The Direct2D factory, for a window that draws into its own target — the dock's layered bitmap.
    pub(crate) fn factory(&self) -> &ID2D1Factory {
        &self.factory
    }
}

/// Creates a text format for the first family in `families` this machine has.
pub(crate) fn text_format(
    dwrite: &IDWriteFactory,
    locale: &str,
    families: &[&str],
    size: f32,
    weight: DWRITE_FONT_WEIGHT,
) -> Result<IDWriteTextFormat, RenderError> {
    let mut last = None;
    let locale = wide(locale);

    for family in families {
        let name = wide(family);
        // SAFETY: `name` and `locale` are NUL-terminated UTF-16 buffers that outlive the call, `None`
        // asks for the system collection, and every other argument is a plain value. The locale
        // cannot be null: DirectWrite answers `E_INVALIDARG` for that, which a probe confirmed.
        let created = unsafe {
            dwrite.CreateTextFormat(
                PCWSTR(name.as_ptr()),
                None::<&IDWriteFontCollection>,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                PCWSTR(locale.as_ptr()),
            )
        };

        match created {
            Ok(format) => {
                // SAFETY: `format` is a live interface this process owns. A bar is one line per
                // widget, so wrapping could only ever be a bug.
                unsafe {
                    let _ = format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                    let _ = format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
                    let _ = format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
                }
                return Ok(format);
            }
            Err(error) => last = Some(error),
        }
    }

    Err(RenderError::call(
        "create a text format",
        last.unwrap_or_else(windows::core::Error::from_thread),
    ))
}

/// A rasterised frame: the render target and the brushes that belong to it.
///
/// Only the four colours the bar actually draws get a brush. The theme's `surface`, `accent` and
/// `accent2` tokens are still parsed and validated — a theme file has to stay valid for the C# build —
/// but a brush that no frame ever fills with would be dead weight in a process with a memory budget.
#[derive(Debug)]
struct Surface {
    target: ID2D1HwndRenderTarget,
    background: ID2D1SolidColorBrush,
    text: ID2D1SolidColorBrush,
    dim: ID2D1SolidColorBrush,
    stroke: ID2D1SolidColorBrush,
}

impl Surface {
    fn create(
        resources: &Resources,
        hwnd: HWND,
        pixel: (u32, u32),
        dpi: f32,
        theme: &Theme,
    ) -> Result<Self, RenderError> {
        let properties = D2D1_RENDER_TARGET_PROPERTIES {
            // Software: a 1920x40 strip at one frame per second costs nothing to rasterise, and it
            // keeps a D3D11 device (and its GPU driver) out of a process whose budget is 35 MB.
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_IGNORE,
            },
            dpiX: dpi,
            dpiY: dpi,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let hwnd_properties = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd,
            pixelSize: D2D_SIZE_U {
                width: pixel.0.max(1),
                height: pixel.1.max(1),
            },
            presentOptions: D2D1_PRESENT_OPTIONS_NONE,
        };

        // SAFETY: both property structures are live and fully initialised, and `hwnd` is a window
        // this process created and keeps alive for as long as the target.
        let target = unsafe {
            resources
                .factory
                .CreateHwndRenderTarget(&properties, &hwnd_properties)
        }
        .map_err(|error| RenderError::call("create a render target", error))?;

        // SAFETY: `target` is live; grayscale antialiasing is what a dark, opaque surface wants —
        // ClearType's subpixel fringes are visible as colour on a near-black bar.
        unsafe {
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        }

        let brush = |colour: &D2D1_COLOR_F| -> Result<ID2D1SolidColorBrush, RenderError> {
            // SAFETY: the colour outlives the call; `None` means an identity transform at full
            // opacity, which is what every brush here wants.
            unsafe { target.CreateSolidColorBrush(colour, None) }
                .map_err(|error| RenderError::call("create a brush", error))
        };

        let background = brush(&theme.background)?;
        let text = brush(&theme.text)?;
        let dim = brush(&theme.text_dim)?;
        let stroke = brush(&theme.stroke)?;

        Ok(Self {
            target,
            background,
            text,
            dim,
            stroke,
        })
    }

    /// Tells Direct2D the surface changed size.
    fn resize(&mut self, pixel: (u32, u32)) -> Result<(), RenderError> {
        let size = D2D_SIZE_U {
            width: pixel.0.max(1),
            height: pixel.1.max(1),
        };

        // SAFETY: `self.target` is live and `size` outlives the call.
        unsafe { self.target.Resize(&size) }
            .map_err(|error| RenderError::call("resize a render target", error))
    }
}

/// One bar's drawing state.
#[derive(Debug)]
pub struct Renderer {
    resources: &'static Resources,
    hwnd: HWND,
    surface: Option<Surface>,
    dpi: f32,
    pixel: (u32, u32),
    theme: Theme,
}

impl Renderer {
    /// A renderer for `hwnd`; the target itself is created on the first resize or frame, once the
    /// window has a size.
    pub fn new(hwnd: HWND, dpi: u32, theme: &Theme) -> Result<Self, RenderError> {
        Ok(Self {
            resources: Resources::shared()?,
            hwnd,
            surface: None,
            dpi: dpi.max(96) as f32,
            pixel: (1, 1),
            theme: theme.clone(),
        })
    }

    /// The bar's size in device-independent pixels, as the layout code sees it.
    pub fn size_dip(&self) -> (f32, f32) {
        let scale = 96.0 / self.dpi;
        (self.pixel.0 as f32 * scale, self.pixel.1 as f32 * scale)
    }

    /// The pixel size the target was last told to be, i.e. the client rectangle a frame draws into.
    pub fn pixel_size(&self) -> (u32, u32) {
        self.pixel
    }

    /// Tells the renderer its window moved to another DPI.
    ///
    /// `Resize` cannot do this: the target carries the DPI it was created with, so a 40 DIP bar on a
    /// 150 % monitor needs 60 physical pixels and a new target.
    pub fn set_dpi(&mut self, dpi: u32) {
        let dpi = dpi.max(96) as f32;
        if (dpi - self.dpi).abs() < f32::EPSILON {
            return;
        }

        self.dpi = dpi;
        self.surface = None;
    }

    /// The DPI the target is drawn at.
    pub fn dpi(&self) -> u32 {
        self.dpi as u32
    }

    /// Tells the renderer its window now measures `pixel`.
    pub fn resize(&mut self, pixel: (u32, u32)) -> Result<(), RenderError> {
        let pixel = (pixel.0.max(1), pixel.1.max(1));
        if pixel == self.pixel && self.surface.is_some() {
            return Ok(());
        }

        self.pixel = pixel;

        match self.surface.as_mut() {
            Some(surface) => surface.resize(pixel),
            None => {
                self.surface = Some(Surface::create(
                    self.resources,
                    self.hwnd,
                    pixel,
                    self.dpi,
                    &self.theme,
                )?);
                Ok(())
            }
        }
    }

    /// Draws one frame.
    ///
    /// This is the only place that touches the render target, and it allocates nothing: every brush,
    /// format and layout it uses was created before it was called.
    pub fn paint(&mut self, theme: &Theme, layouts: &Layouts) -> PaintOutcome {
        if self.surface.is_none() {
            match Surface::create(self.resources, self.hwnd, self.pixel, self.dpi, theme) {
                Ok(surface) => self.surface = Some(surface),
                Err(error) => return PaintOutcome::Failed(error),
            }
        }

        // The surface is taken out of `self` for the duration of the frame: painting cannot re-enter
        // anything that would resize or retheme the target, and keeping the borrow in one place is
        // what lets the failure path put it back or drop it.
        let Some(surface) = self.surface.take() else {
            return PaintOutcome::Recreate;
        };

        let result = paint_into(&surface, theme, layouts);

        match result {
            Ok(()) => {
                self.surface = Some(surface);
                PaintOutcome::Painted
            }
            Err(error) if error.is_recreate() => PaintOutcome::Recreate,
            Err(error) => {
                self.surface = Some(surface);
                PaintOutcome::Failed(error)
            }
        }
    }
}

/// Draws one frame into `surface`.
///
/// Direct2D's `BeginDraw`, `Clear`, the `Fill*` calls and `DrawTextLayout` are all `void`; only
/// `EndDraw` reports anything, which is why the frame's verdict is decided there.
fn paint_into(surface: &Surface, theme: &Theme, layouts: &Layouts) -> Result<(), RenderError> {
    // SAFETY: `surface` is this process's render target for the current window and every structure
    // below is a plain value that outlives the call.
    unsafe {
        surface.target.BeginDraw();
        draw(surface, theme, layouts);

        // `EndDraw` runs even when a drawing call was skipped: leaving a frame open would wedge the
        // target for good.
        surface
            .target
            .EndDraw(None, None)
            .map_err(|error| RenderError::call("end a frame", error))
    }
}

/// Draws the whole strip: background, border, and the three groups of widgets.
///
/// # Safety
///
/// `surface.target` must be inside a `BeginDraw`/`EndDraw` pair and every brush must belong to it.
unsafe fn draw(surface: &Surface, theme: &Theme, layouts: &Layouts) {
    let (width, height) = layouts.size;

    // SAFETY: the caller guarantees an open draw call and brushes of this target.
    unsafe {
        surface.target.Clear(Some(&theme.background));

        // The hairline along the bottom edge is what separates the bar from whatever is behind it.
        surface.target.FillRectangle(
            &D2D_RECT_F {
                left: 0.0,
                top: height - BORDER,
                right: width,
                bottom: height,
            },
            &surface.stroke,
        );

        draw_launcher(surface, height, theme.corner_radius);
        draw_profile(surface, layouts);
        draw_clock(surface, layouts);
        draw_metrics(surface, layouts);
    }
}

/// The profile name, right of the launcher glyph, vertically centred.
///
/// # Safety
///
/// Must be called inside an open draw call on `surface.target`.
unsafe fn draw_profile(surface: &Surface, layouts: &Layouts) {
    let (_, height) = layouts.size;

    // SAFETY: the caller guarantees an open draw call and layouts built for this target.
    unsafe {
        surface.target.DrawTextLayout(
            vector(
                PADDING + GLYPH + GAP,
                (height - layouts.profile_size.height) / 2.0,
            ),
            &layouts.profile,
            &surface.dim,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
        );
    }
}

/// The four-square launcher glyph on the left edge.
///
/// The tiles' softness is the theme's corner radius scaled down: a theme with sharper corners gets a
/// sharper glyph, and the shipped `liquid-glass` radius of 14 lands on the 1.6 DIP the design uses.
///
/// # Safety
///
/// Must be called inside an open draw call on `surface.target`.
unsafe fn draw_launcher(surface: &Surface, height: f32, corner_radius: f32) {
    const TILE: f32 = 6.5;
    const STEP: f32 = 9.5;

    let radius = (corner_radius * 0.115).clamp(0.5, TILE / 2.0);
    let left = PADDING;
    let top = (height - GLYPH) / 2.0;

    for (column, row) in [(0.0, 0.0), (STEP, 0.0), (0.0, STEP), (STEP, STEP)] {
        // SAFETY: the caller guarantees an open draw call on this target.
        unsafe {
            surface.target.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: left + column,
                        top: top + row,
                        right: left + column + TILE,
                        bottom: top + row + TILE,
                    },
                    radiusX: radius,
                    radiusY: radius,
                },
                &surface.text,
            );
        }
    }
}

/// The centred two-line clock: `HH:mm`, with the long date underneath when the bar is tall enough.
///
/// # Safety
///
/// Must be called inside an open draw call on `surface.target`.
unsafe fn draw_clock(surface: &Surface, layouts: &Layouts) {
    let (width, height) = layouts.size;
    let date_height = if layouts.date.is_some() {
        layouts.date_size.height
    } else {
        0.0
    };
    let total = layouts.clock_size.height + date_height;
    let top = ((height - total) / 2.0).max(0.0);

    // SAFETY: the caller guarantees an open draw call and layouts built for this target.
    unsafe {
        surface.target.DrawTextLayout(
            vector((width - layouts.clock_size.width) / 2.0, top),
            &layouts.clock,
            &surface.text,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
        );

        if let Some(date) = layouts.date.as_ref() {
            surface.target.DrawTextLayout(
                vector(
                    (width - layouts.date_size.width) / 2.0,
                    top + layouts.clock_size.height,
                ),
                date,
                &surface.dim,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
    }
}

/// The readout and the gear on the right edge.
///
/// # Safety
///
/// Must be called inside an open draw call on `surface.target`.
unsafe fn draw_metrics(surface: &Surface, layouts: &Layouts) {
    let (width, height) = layouts.size;
    let gear_left = width - PADDING - GEAR;
    let gear_top = (height - GEAR) / 2.0;
    let text_right = gear_left - GAP;
    let text_left = text_right - layouts.metrics_size.width;
    let text_top = (height - layouts.metrics_size.height) / 2.0;

    // SAFETY: the caller guarantees an open draw call and layouts built for this target.
    unsafe {
        surface.target.DrawTextLayout(
            vector(text_left, text_top),
            &layouts.metrics,
            &surface.dim,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
        );

        draw_gear(
            &surface.target,
            (gear_left + GEAR / 2.0, gear_top + GEAR / 2.0),
            &surface.dim,
            &surface.background,
        );
    }
}

/// An eight-toothed gear, drawn from circles because a bar this short has no room for anything finer.
///
/// # Safety
///
/// `target` must be inside an open draw call and both brushes must belong to it.
unsafe fn draw_gear(
    target: &ID2D1HwndRenderTarget,
    centre: (f32, f32),
    ink: &ID2D1SolidColorBrush,
    hole: &ID2D1SolidColorBrush,
) {
    let circle = |x: f32, y: f32, diameter: f32| D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: x - diameter / 2.0,
            top: y - diameter / 2.0,
            right: x + diameter / 2.0,
            bottom: y + diameter / 2.0,
        },
        radiusX: diameter / 2.0,
        radiusY: diameter / 2.0,
    };

    // SAFETY: the caller guarantees an open draw call on this target.
    unsafe {
        for step in 0..8 {
            let angle = std::f32::consts::FRAC_PI_4 * step as f32;
            target.FillRoundedRectangle(
                &circle(
                    centre.0 + angle.cos() * 3.1,
                    centre.1 + angle.sin() * 3.1,
                    1.7,
                ),
                ink,
            );
        }

        target.FillRoundedRectangle(&circle(centre.0, centre.1, 6.0), ink);
        target.FillRoundedRectangle(&circle(centre.0, centre.1, 2.6), hole);
    }
}

/// Direct2D wants a `Vector2` origin; a pair of floats keeps the call sites readable.
fn vector(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
}

/// Measured width and height of one layout, in device-independent pixels.
#[derive(Debug, Clone, Copy, Default)]
struct Measured {
    width: f32,
    height: f32,
}

/// One measured block of text plus the layout that draws it.
#[derive(Debug)]
pub struct Layouts {
    clock: IDWriteTextLayout,
    date: Option<IDWriteTextLayout>,
    metrics: IDWriteTextLayout,
    profile: IDWriteTextLayout,
    clock_size: Measured,
    date_size: Measured,
    metrics_size: Measured,
    profile_size: Measured,
    size: (f32, f32),
}

impl Layouts {
    /// Builds every layout the frame needs.
    ///
    /// Called when the text, the size or the DPI changed — never from `paint`.
    ///
    /// # Errors
    ///
    /// [`RenderError`] when DirectWrite refuses a layout; the caller then keeps its previous layouts
    /// and draws a slightly stale frame, which is better than a blank bar.
    pub fn build(text: &BarText, show_date: bool, size: (f32, f32)) -> Result<Self, RenderError> {
        let resources = Resources::shared()?;
        let fonts = &resources.fonts;
        let dwrite = resources.dwrite();

        let clock = layout(dwrite, &fonts.clock, text.time.as_units(), size, Trim::No)?;
        let clock_size = measure(&clock)?;

        let date = if show_date {
            let created = layout(dwrite, &fonts.date, text.date.as_units(), size, Trim::No)?;
            let measured = measure(&created)?;
            Some((created, measured))
        } else {
            None
        };
        let (date, date_size) = match date {
            Some((date, measured)) => (Some(date), measured),
            None => (None, Measured::default()),
        };

        // The clock owns the middle of the bar, so neither side may grow into it.
        let half = (size.0 / 2.0).max(1.0);
        let metrics = layout(
            dwrite,
            &fonts.metrics,
            text.metrics.as_units(),
            ((half - PADDING - GEAR - GAP).max(1.0), size.1),
            Trim::No,
        )?;
        let metrics_size = measure(&metrics)?;

        let profile = layout(
            dwrite,
            &fonts.profile,
            text.profile.as_units(),
            ((half - PADDING - GLYPH - GAP).max(1.0), size.1),
            Trim::Yes,
        )?;
        let profile_size = measure(&profile)?;

        Ok(Self {
            clock,
            date,
            metrics,
            profile,
            clock_size,
            date_size,
            metrics_size,
            profile_size,
            size,
        })
    }

    /// Whether the date line fits in this shape.
    pub fn show_date(&self) -> bool {
        self.date.is_some()
    }

    /// The profile name's measured size, which is where the clock may not start.
    pub fn profile_size(&self) -> (f32, f32) {
        (self.profile_size.width, self.profile_size.height)
    }

    /// Every measured height, for the layout log line.
    pub fn heights(&self) -> (f32, f32) {
        (self.clock_size.height, self.date_size.height)
    }
}

/// Whether a layout ends in an ellipsis when it does not fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trim {
    /// Overflow is a caller error, so nothing is trimmed.
    No,
    /// A long profile name is cut with an ellipsis instead of spilling over the clock.
    Yes,
}

/// Creates one layout, with the tabular figures the clock needs.
fn layout(
    dwrite: &IDWriteFactory,
    format: &IDWriteTextFormat,
    text: &[u16],
    max: (f32, f32),
    trim: Trim,
) -> Result<IDWriteTextLayout, RenderError> {
    // SAFETY: `text` is a live slice of UTF-16, `format` is a live interface, and the two maxima are
    // plain values; the returned layout owns its own copy of the string.
    let created = unsafe { dwrite.CreateTextLayout(text, format, max.0, max.1) }
        .map_err(|error| RenderError::call("create a text layout", error))?;

    match trim {
        Trim::Yes => {
            // SAFETY: `created` and `format` are live interfaces.
            unsafe {
                trim_with_ellipsis(dwrite, &created, format);
            }
        }
        Trim::No => apply_tabular_figures(dwrite, &created),
    }

    Ok(created)
}

/// Asks for tabular figures, which is what keeps `HH:mm` from shifting as the digits change; the C#
/// bar gets the same effect from `Typography.NumeralAlignment`. A font without the feature ignores it.
fn apply_tabular_figures(dwrite: &IDWriteFactory, layout: &IDWriteTextLayout) {
    // SAFETY: `dwrite` is a live factory; a failure is reported by returning early.
    let Ok(typography) = (unsafe { dwrite.CreateTypography() }) else {
        return;
    };

    let feature = DWRITE_FONT_FEATURE {
        nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
        parameter: 1,
    };

    // SAFETY: both interfaces are live and the range covers the whole layout.
    unsafe {
        if typography.AddFontFeature(feature).is_ok() {
            let _ = layout.SetTypography(
                &typography,
                DWRITE_TEXT_RANGE {
                    startPosition: 0,
                    length: u32::MAX,
                },
            );
        }
    }
}

/// Ends an over-long string with an ellipsis instead of letting it spill over its neighbour.
///
/// # Safety
///
/// `layout` and `format` must be live interfaces.
unsafe fn trim_with_ellipsis(
    dwrite: &IDWriteFactory,
    layout: &IDWriteTextLayout,
    format: &IDWriteTextFormat,
) {
    // SAFETY: the caller guarantees both interfaces are live.
    let Ok(sign) = (unsafe { dwrite.CreateEllipsisTrimmingSign(format) }) else {
        return;
    };

    let trimming = DWRITE_TRIMMING {
        granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
        delimiter: 0,
        delimiterCount: 0,
    };

    // SAFETY: as above; `sign` stays alive for the duration of the call.
    unsafe {
        let _ = layout.SetTrimming(&trimming, &sign);
    }
}

/// Reads a layout's measured size.
fn measure(layout: &IDWriteTextLayout) -> Result<Measured, RenderError> {
    let mut metrics = DWRITE_TEXT_METRICS::default();

    // SAFETY: `metrics` is a live out-parameter of the size the API documents.
    unsafe { layout.GetMetrics(&mut metrics) }
        .map_err(|error| RenderError::call("measure a text layout", error))?;

    Ok(Measured {
        width: metrics.widthIncludingTrailingWhitespace,
        height: metrics.height,
    })
}

/// `text` as a NUL-terminated UTF-16 buffer.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The current user's locale name, e.g. `ru-RU`.
///
/// DirectWrite wants a locale for every text format and refuses a null one, so the bar asks the
/// system instead of hard-coding a language: the digits, the date and the shaping all follow the
/// user's own locale.
pub(crate) fn user_locale() -> String {
    let mut buffer = [0u16; 85];

    // SAFETY: `buffer` is a live, correctly sized array and the call writes at most its length.
    let written = unsafe { GetUserDefaultLocaleName(&mut buffer) };

    if written <= 1 {
        return "en-US".to_owned();
    }

    let units = buffer
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(buffer.len());

    match String::from_utf16(&buffer[..units]) {
        Ok(locale) if !locale.is_empty() => locale,
        _ => "en-US".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_constants_match_the_brief() {
        assert_eq!(PADDING, 16.0);
        assert_eq!(GLYPH, 16.0);
        assert_eq!(GEAR, 8.0);
        assert_eq!(BORDER, 1.0);
        assert_eq!(TWO_LINE_MIN_HEIGHT, 34.0);
    }

    #[test]
    fn the_recreate_target_code_is_the_documented_one() {
        assert_eq!(D2DERR_RECREATE_TARGET, 0x8899_000C_u32 as i32);
    }

    #[test]
    fn a_recreate_error_is_recognised_and_another_failure_is_not() {
        let recreate = RenderError::Call {
            action: "test",
            source: windows::core::Error::from_hresult(windows::core::HRESULT(
                D2DERR_RECREATE_TARGET,
            )),
        };
        let other = RenderError::Call {
            action: "test",
            source: windows::core::Error::from_hresult(windows::core::HRESULT(
                0x8000_4005_u32 as i32,
            )),
        };

        assert!(recreate.is_recreate());
        assert!(!other.is_recreate());
    }

    #[test]
    fn shared_factories_are_created_once_and_reused() {
        let first = Resources::shared().expect("this machine must have Direct2D");
        let second = Resources::shared().expect("the second call reuses the first");

        assert!(std::ptr::eq(first, second));
    }

    #[test]
    fn the_fonts_resolve_on_this_machine() {
        // `Resources::create` fails when no family in a list resolves, so a shared instance is proof
        // that all four formats exist.
        let resources = Resources::shared().expect("this machine must have Direct2D and Segoe UI");

        assert!(
            unsafe {
                resources
                    .dwrite()
                    .CreateEllipsisTrimmingSign(&resources.fonts.clock)
            }
            .is_ok()
        );
        assert_eq!(UI_FAMILIES[1], "Segoe UI");
        assert_eq!(MONO_FAMILIES[1], "Consolas");
    }

    #[test]
    fn a_font_name_becomes_a_nul_terminated_utf16_buffer() {
        let buffer = wide("Segoe UI");

        assert_eq!(buffer.len(), 9);
        assert_eq!(buffer.last(), Some(&0));
    }
}
