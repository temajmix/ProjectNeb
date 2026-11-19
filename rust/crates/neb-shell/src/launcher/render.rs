//! Direct2D drawing for the launcher panel.
//!
//! The panel is a normal top-level window with an `ID2D1HwndRenderTarget`, not a layered one: a
//! layered window cannot show a native `EDIT` control, and the query box has to be a real edit control
//! for IME and Cyrillic input to work. The glass is therefore the window's own constant alpha (96 %,
//! applied by [`window`](super::window)) over an opaque `#0B0A12` surface, and the rounded corners are
//! a window region.
//!
//! Everything a frame draws is cached outside the frame: the brushes, the text formats, the trimming
//! signs, the footer, the scope note, and one [`RowArt`] per *visible* result row. A row's art is
//! rebuilt when the query changes (the caller syncs after every change) and never from `paint`, which
//! keeps a frame allocation-free and a keystroke's work proportional to the panel, not the index.

use neb_core::strings::{Language, Text, get};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES, D2D1_DC_INITIALIZE_MODE_COPY,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE,
    D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
    ID2D1Bitmap, ID2D1GdiInteropRenderTarget, ID2D1HwndRenderTarget, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER, IDWriteFactory, IDWriteInlineObject, IDWriteTextFormat,
    IDWriteTextLayout,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{BitBlt, HDC, SRCCOPY};
use windows::core::Interface;
use windows_numerics::{Matrix3x2, Vector2};

use super::Entry;
use crate::icons::IconBitmap;
use crate::render::{PaintOutcome, RenderError, Resources};
use crate::theme::Theme;
use crate::widgets::WideBuf;

/// The panel's width in device-independent pixels.
pub const PANEL_WIDTH: f32 = 720.0;

/// The panel's height in device-independent pixels.
pub const PANEL_HEIGHT: f32 = 480.0;

/// The corner radius in device-independent pixels.
pub const CORNER_RADIUS: f32 = 18.0;

/// The window's constant alpha, i.e. the glass at 96 %.
pub const GLASS_ALPHA: u8 = 245;

/// Height of the search row above the divider.
pub const HEADER_HEIGHT: f32 = 76.0;

/// Height of the footer with the key hint.
pub const FOOTER_HEIGHT: f32 = 34.0;

/// Height of one result row.
pub const ROW_HEIGHT: f32 = 48.0;

/// Side of a result icon.
pub const ROW_ICON: f32 = 32.0;

/// Inner padding of the panel, in device-independent pixels.
pub const PADDING: f32 = 20.0;

/// Inset of a row from the panel edge.
pub const ROW_INSET: f32 = 8.0;

/// Width of the accent bar on the selected row.
pub const ACCENT_BAR: f32 = 3.0;

/// Height of the native query control.
pub const QUERY_HEIGHT: f32 = 40.0;

/// Font size of a result's name.
const NAME_SIZE: f32 = 14.0;

/// Font size of a result's path.
const PATH_SIZE: f32 = 11.0;

/// Font size of the footer hint, the scope note and the empty-state line.
const NOTE_SIZE: f32 = 11.0;

/// Font size of a placeholder's initial.
const LETTER_SIZE: f32 = 16.0;

/// The families the panel asks for, best first.
const UI_FAMILIES: [&str; 2] = ["Segoe UI Variable Text", "Segoe UI"];

/// The panel's geometry, in device-independent pixels.
///
/// Pure arithmetic, so the row count at a given height is a test rather than a screenshot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LauncherLayout {
    /// Panel size in DIPs.
    pub size: (f32, f32),
    /// The query control's rectangle in DIPs: `left, top, width, height`.
    pub query: (f32, f32, f32, f32),
    /// Where the first row starts.
    pub list_top: f32,
    /// How many rows are on screen at once.
    pub rows: usize,
    /// Where the footer begins.
    pub footer_top: f32,
}

impl LauncherLayout {
    /// The layout of a panel that measures `size` DIPs.
    pub fn new(size: (f32, f32)) -> Self {
        let width = size.0.max(1.0);
        let height = size.1.max(1.0);
        let footer_top = (height - FOOTER_HEIGHT).max(HEADER_HEIGHT + ROW_HEIGHT);
        let list_top = HEADER_HEIGHT;
        let rows = (((footer_top - list_top) / ROW_HEIGHT).floor() as usize).max(1);

        Self {
            size: (width, height),
            query: (
                PADDING,
                (HEADER_HEIGHT - QUERY_HEIGHT) / 2.0,
                (width - PADDING * 2.0).max(1.0),
                QUERY_HEIGHT,
            ),
            list_top,
            rows,
            footer_top,
        }
    }

    /// The rectangle of local row `row`: `left, top, right, bottom` in DIPs.
    pub fn row_rect(&self, row: usize) -> (f32, f32, f32, f32) {
        let top = self.list_top + row as f32 * ROW_HEIGHT;

        (
            ROW_INSET,
            top,
            (self.size.0 - ROW_INSET).max(ROW_INSET + 1.0),
            top + ROW_HEIGHT,
        )
    }

    /// The local row under a client-space `y`, if the point is inside the list.
    pub fn row_at(&self, y: f32) -> Option<usize> {
        if y < self.list_top || y >= self.footer_top {
            return None;
        }

        let row = ((y - self.list_top) / ROW_HEIGHT).floor() as usize;
        (row < self.rows).then_some(row)
    }
}

/// The brushes one panel frame uses, all belonging to one render target.
#[derive(Debug)]
struct Brushes {
    glass: ID2D1SolidColorBrush,
    stroke: ID2D1SolidColorBrush,
    divider: ID2D1SolidColorBrush,
    selection: ID2D1SolidColorBrush,
    accent: ID2D1SolidColorBrush,
    text: ID2D1SolidColorBrush,
    dim: ID2D1SolidColorBrush,
    placeholder: ID2D1SolidColorBrush,
}

/// One visible result row, with every object the frame needs already built.
#[derive(Debug)]
struct RowArt {
    icon: Option<ID2D1Bitmap>,
    letter: Option<IDWriteTextLayout>,
    name: Option<IDWriteTextLayout>,
    path: Option<IDWriteTextLayout>,
}

/// A measured string, so a centred line can be placed without asking DirectWrite in a frame.
#[derive(Debug, Clone, Copy, Default)]
struct Measured {
    width: f32,
    height: f32,
}

/// The launcher panel's drawing state.
#[derive(Debug)]
pub struct LauncherRenderer {
    resources: &'static Resources,
    target: ID2D1HwndRenderTarget,
    brushes: Brushes,
    name_format: IDWriteTextFormat,
    path_format: IDWriteTextFormat,
    note_format: IDWriteTextFormat,
    letter_format: IDWriteTextFormat,
    name_trim: Option<IDWriteInlineObject>,
    path_trim: Option<IDWriteInlineObject>,
    language: Language,
    hint: IDWriteTextLayout,
    hint_size: Measured,
    note: IDWriteTextLayout,
    note_size: Measured,
    empty: Option<(IDWriteTextLayout, Measured)>,
    rows: Vec<RowArt>,
    layout: LauncherLayout,
    pixel: (u32, u32),
    dpi: f32,
    fingerprint: u64,
}

impl LauncherRenderer {
    /// A renderer for `hwnd`, which measures `pixel` physical pixels at `dpi`.
    ///
    /// # Errors
    ///
    /// [`RenderError`] when Direct2D or DirectWrite refuses; the caller then draws nothing rather
    /// than taking the shell down.
    pub fn new(
        hwnd: HWND,
        pixel: (u32, u32),
        dpi: u32,
        theme: &Theme,
        language: Language,
    ) -> Result<Self, RenderError> {
        let resources = Resources::shared()?;
        let dpi = dpi.max(96) as f32;
        let pixel = (pixel.0.max(1), pixel.1.max(1));

        let properties = D2D1_RENDER_TARGET_PROPERTIES {
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
                width: pixel.0,
                height: pixel.1,
            },
            presentOptions: D2D1_PRESENT_OPTIONS_NONE,
        };

        // SAFETY: both property structures are live and fully initialised, and `hwnd` is a window
        // this process created and keeps alive for as long as the target.
        let target = unsafe {
            resources
                .factory()
                .CreateHwndRenderTarget(&properties, &hwnd_properties)
        }
        .map_err(|error| RenderError::call("create the launcher target", error))?;

        // SAFETY: `target` is live; grayscale antialiasing keeps ClearType's colour fringes off a
        // near-black panel.
        unsafe {
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        }

        let brush = |colour: &D2D1_COLOR_F| -> Result<ID2D1SolidColorBrush, RenderError> {
            // SAFETY: the colour outlives the call and `None` is an identity transform at full
            // opacity, which is what every brush here wants.
            unsafe { target.CreateSolidColorBrush(colour, None) }
                .map_err(|error| RenderError::call("create a launcher brush", error))
        };

        let brushes = Brushes {
            glass: brush(&hue(&theme.background, 1.0))?,
            stroke: brush(&hue(&theme.text, 0.18))?,
            divider: brush(&hue(&theme.text, 0.08))?,
            selection: brush(&hue(&theme.accent, 0.16))?,
            accent: brush(&theme.accent)?,
            text: brush(&theme.text)?,
            dim: brush(&theme.text_dim)?,
            placeholder: brush(&hue(&theme.text, 0.10))?,
        };

        let locale = crate::render::user_locale();
        let name_format = crate::render::text_format(
            resources.dwrite(),
            &locale,
            &UI_FAMILIES,
            NAME_SIZE,
            DWRITE_FONT_WEIGHT_SEMI_BOLD,
        )?;
        let path_format = crate::render::text_format(
            resources.dwrite(),
            &locale,
            &UI_FAMILIES,
            PATH_SIZE,
            DWRITE_FONT_WEIGHT_NORMAL,
        )?;
        let note_format = crate::render::text_format(
            resources.dwrite(),
            &locale,
            &UI_FAMILIES,
            NOTE_SIZE,
            DWRITE_FONT_WEIGHT_NORMAL,
        )?;
        let letter_format = crate::render::text_format(
            resources.dwrite(),
            &locale,
            &UI_FAMILIES,
            LETTER_SIZE,
            DWRITE_FONT_WEIGHT_SEMI_BOLD,
        )?;

        // SAFETY: all four formats are live interfaces owned by this value.
        unsafe {
            let _ = letter_format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
            let _ = letter_format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
        }

        let name_trim = unsafe { resources.dwrite().CreateEllipsisTrimmingSign(&name_format) }.ok();
        let path_trim = unsafe { resources.dwrite().CreateEllipsisTrimmingSign(&path_format) }.ok();

        let layout =
            LauncherLayout::new((pixel.0 as f32 * 96.0 / dpi, pixel.1 as f32 * 96.0 / dpi));
        let hint = create_layout(
            resources.dwrite(),
            &note_format,
            &wide(get(language, Text::LauncherHint)),
            (layout.size.0 - PADDING * 2.0, 20.0),
        )?;
        let hint_size = measure(&hint)?;
        let note = create_layout(
            resources.dwrite(),
            &note_format,
            &wide(get(language, Text::LauncherScope)),
            (layout.size.0 - PADDING * 2.0, 20.0),
        )?;
        let note_size = measure(&note)?;

        Ok(Self {
            resources,
            target,
            brushes,
            name_format,
            path_format,
            note_format,
            letter_format,
            name_trim,
            path_trim,
            language,
            hint,
            hint_size,
            note,
            note_size,
            empty: None,
            rows: Vec::new(),
            layout,
            pixel,
            dpi,
            fingerprint: 0,
        })
    }

    /// The DPI the panel is drawn at.
    pub fn dpi(&self) -> u32 {
        self.dpi as u32
    }

    /// The render target's pixel size.
    pub fn pixel_size(&self) -> (u32, u32) {
        self.pixel
    }

    /// The panel size in DIPs, which the window turns into a [`LauncherLayout`].
    pub fn size_dip(&self) -> (f32, f32) {
        let scale = 96.0 / self.dpi;
        (self.pixel.0 as f32 * scale, self.pixel.1 as f32 * scale)
    }

    /// Tells the target it now measures `pixel`; a no-op when it already does.
    pub fn resize(&mut self, pixel: (u32, u32)) -> Result<(), RenderError> {
        let pixel = (pixel.0.max(1), pixel.1.max(1));
        if pixel == self.pixel {
            return Ok(());
        }

        let size = D2D_SIZE_U {
            width: pixel.0,
            height: pixel.1,
        };

        // SAFETY: `self.target` is live and `size` outlives the call.
        unsafe { self.target.Resize(&size) }
            .map_err(|error| RenderError::call("resize the launcher target", error))?;
        self.pixel = pixel;
        Ok(())
    }

    /// Rebuilds the visible rows and the empty-state line, if anything they depend on changed.
    ///
    /// Called after a keystroke, a new index or a resize — never from [`paint`](Self::paint). The
    /// fingerprint makes a call that changes nothing cost a hash and no Direct2D work.
    pub fn sync(
        &mut self,
        layout: LauncherLayout,
        entries: &[Entry],
        results: &[usize],
        top: usize,
        building: bool,
    ) {
        let end = (top + layout.rows).min(results.len());
        let visible = results.get(top.min(results.len())..end).unwrap_or(&[]);

        let fingerprint = fingerprint(layout, top, building, entries, visible, results.len());
        if fingerprint == self.fingerprint {
            return;
        }

        self.fingerprint = fingerprint;
        self.layout = layout;

        let text_width = (layout.size.0 - ROW_INSET * 2.0 - 12.0 - ROW_ICON - 12.0 - 12.0).max(1.0);
        let mut rows = Vec::with_capacity(visible.len());

        for index in visible {
            let Some(entry) = entries.get(*index) else {
                continue;
            };

            rows.push(self.row_art(entry, text_width));
        }

        self.rows = rows;

        self.empty = if results.is_empty() {
            let text = if building {
                get(self.language, Text::LauncherIndexing)
            } else {
                get(self.language, Text::LauncherEmpty)
            };

            match create_layout(
                self.resources.dwrite(),
                &self.note_format,
                &wide(text),
                (layout.size.0 - PADDING * 2.0, 20.0),
            ) {
                Ok(layout) => measure(&layout).ok().map(|size| (layout, size)),
                Err(_) => None,
            }
        } else {
            None
        };
    }

    /// One row's icon (or placeholder) and its two cached text layouts.
    fn row_art(&self, entry: &Entry, text_width: f32) -> RowArt {
        let icon = crate::icons::load(&entry.path).and_then(|icon| self.icon_bitmap(&icon).ok());

        let letter = if icon.is_some() {
            None
        } else {
            let mut text: WideBuf<4> = WideBuf::new();
            text.push(crate::icons::initial(&entry.name));

            create_layout(
                self.resources.dwrite(),
                &self.letter_format,
                text.as_units(),
                (ROW_ICON, ROW_ICON),
            )
            .ok()
        };

        let name = self.text_layout(&entry.name, &self.name_format, &self.name_trim, text_width);
        let path = self.text_layout(
            &entry.path.to_string_lossy(),
            &self.path_format,
            &self.path_trim,
            text_width,
        );

        RowArt {
            icon,
            letter,
            name,
            path,
        }
    }

    /// Uploads one extracted icon as a Direct2D bitmap.
    fn icon_bitmap(&self, icon: &IconBitmap) -> Result<ID2D1Bitmap, RenderError> {
        let properties = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode:
                    windows::Win32::Graphics::Direct2D::Common::D2D1_ALPHA_MODE_PREMULTIPLIED,
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
        .map_err(|error| RenderError::call("create a launcher icon", error))
    }

    /// One one-line layout for `text`, trimmed with an ellipsis where the format has a sign.
    fn text_layout(
        &self,
        text: &str,
        format: &IDWriteTextFormat,
        trim: &Option<IDWriteInlineObject>,
        width: f32,
    ) -> Option<IDWriteTextLayout> {
        let height = format_height(format);
        let layout = create_layout(
            self.resources.dwrite(),
            format,
            &wide(text),
            (width, height),
        )
        .ok()?;

        if let Some(sign) = trim {
            // SAFETY: both interfaces are live and owned by this renderer.
            unsafe {
                let _ = layout.SetTrimming(
                    &DWRITE_TRIMMING {
                        granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                        delimiter: 0,
                        delimiterCount: 0,
                    },
                    sign,
                );
            }
        }

        Some(layout)
    }

    /// Draws one frame.
    ///
    /// `scale` is the reveal animation's scale about the panel's centre; `1.0` is settled.
    ///
    /// This allocates nothing: every layout, bitmap and brush it uses exists before it is called.
    pub fn paint(&mut self, selection: Option<usize>, top: usize, scale: f32) -> PaintOutcome {
        let layout = self.layout;

        // SAFETY: `self.target` is live and every brush and layout belongs to it.
        let drawn = unsafe {
            self.target.BeginDraw();
            self.set_scale(scale, layout);
            self.draw(layout, selection, top);

            self.target.SetTransform(&Matrix3x2::identity());
            self.target
                .EndDraw(None, None)
                .map_err(|error| RenderError::call("end a launcher frame", error))
        };

        match drawn {
            Ok(()) => PaintOutcome::Painted,
            Err(error) if error.is_recreate() => PaintOutcome::Recreate,
            Err(error) => PaintOutcome::Failed(error),
        }
    }

    /// Scales the frame about the panel's centre for the reveal animation.
    ///
    /// # Safety
    ///
    /// Must run inside a `BeginDraw`/`EndDraw` pair on `self.target`.
    unsafe fn set_scale(&self, scale: f32, layout: LauncherLayout) {
        let scale = scale.clamp(0.0, 1.0);
        let (width, height) = layout.size;

        // SAFETY: the caller guarantees an open draw call on this target.
        unsafe {
            self.target.SetTransform(&Matrix3x2 {
                M11: scale,
                M12: 0.0,
                M21: 0.0,
                M22: scale,
                M31: (width / 2.0) * (1.0 - scale),
                M32: (height / 2.0) * (1.0 - scale),
            });
        }
    }

    /// The frame itself.
    ///
    /// # Safety
    ///
    /// Must run inside a `BeginDraw`/`EndDraw` pair on `self.target`.
    unsafe fn draw(&self, layout: LauncherLayout, selection: Option<usize>, top: usize) {
        let width = layout.size.0;
        let height = layout.size.1;

        let panel = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: 0.5,
                top: 0.5,
                right: width - 0.5,
                bottom: height - 0.5,
            },
            radiusX: CORNER_RADIUS,
            radiusY: CORNER_RADIUS,
        };

        // SAFETY: the caller guarantees an open draw call and brushes of this target.
        unsafe {
            self.target.Clear(Some(&opaque(&self.brushes.glass)));
            self.target
                .DrawRoundedRectangle(&panel, &self.brushes.stroke, 1.0, None);

            self.target.FillRectangle(
                &D2D_RECT_F {
                    left: PADDING,
                    top: HEADER_HEIGHT - 0.5,
                    right: width - PADDING,
                    bottom: HEADER_HEIGHT + 0.5,
                },
                &self.brushes.divider,
            );

            for (local, art) in self.rows.iter().enumerate() {
                let slot = top + local;
                let (left, row_top, right, bottom) = layout.row_rect(local);

                if selection == Some(slot) {
                    self.target.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F {
                                left,
                                top: row_top + 2.0,
                                right,
                                bottom: bottom - 2.0,
                            },
                            radiusX: 10.0,
                            radiusY: 10.0,
                        },
                        &self.brushes.selection,
                    );
                    self.target.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F {
                                left,
                                top: row_top + 9.0,
                                right: left + ACCENT_BAR,
                                bottom: bottom - 9.0,
                            },
                            radiusX: ACCENT_BAR / 2.0,
                            radiusY: ACCENT_BAR / 2.0,
                        },
                        &self.brushes.accent,
                    );
                }

                let icon_left = left + 12.0;
                let icon_top = row_top + (ROW_HEIGHT - ROW_ICON) / 2.0;
                let text_left = icon_left + ROW_ICON + 12.0;

                match art.icon.as_ref() {
                    Some(bitmap) => self.target.DrawBitmap(
                        bitmap,
                        Some(&D2D_RECT_F {
                            left: icon_left,
                            top: icon_top,
                            right: icon_left + ROW_ICON,
                            bottom: icon_top + ROW_ICON,
                        }),
                        1.0,
                        D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                        None,
                    ),
                    None => {
                        self.target.FillRoundedRectangle(
                            &D2D1_ROUNDED_RECT {
                                rect: D2D_RECT_F {
                                    left: icon_left,
                                    top: icon_top,
                                    right: icon_left + ROW_ICON,
                                    bottom: icon_top + ROW_ICON,
                                },
                                radiusX: 7.0,
                                radiusY: 7.0,
                            },
                            &self.brushes.placeholder,
                        );

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

                if let Some(name) = art.name.as_ref() {
                    self.target.DrawTextLayout(
                        Vector2 {
                            X: text_left,
                            Y: row_top + 7.0,
                        },
                        name,
                        &self.brushes.text,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                    );
                }

                if let Some(path) = art.path.as_ref() {
                    self.target.DrawTextLayout(
                        Vector2 {
                            X: text_left,
                            Y: row_top + 26.0,
                        },
                        path,
                        &self.brushes.dim,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                    );
                }
            }

            if let Some((empty, size)) = self.empty.as_ref() {
                let top = layout.list_top
                    + ((layout.footer_top - layout.list_top - size.height) / 2.0).max(0.0);

                self.target.DrawTextLayout(
                    Vector2 {
                        X: ((width - size.width) / 2.0).max(PADDING),
                        Y: top,
                    },
                    empty,
                    &self.brushes.dim,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );

                // The empty state says why it is empty: the index is the Start Menu, not every
                // program on the machine, so an application that was never opened from there cannot
                // be a result.
                self.target.DrawTextLayout(
                    Vector2 {
                        X: ((width - self.note_size.width) / 2.0).max(PADDING),
                        Y: top + size.height + 6.0,
                    },
                    &self.note,
                    &self.brushes.dim,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }

            self.target.FillRectangle(
                &D2D_RECT_F {
                    left: PADDING,
                    top: layout.footer_top,
                    right: width - PADDING,
                    bottom: layout.footer_top + 1.0,
                },
                &self.brushes.divider,
            );

            self.target.DrawTextLayout(
                Vector2 {
                    X: PADDING,
                    Y: layout.footer_top + (FOOTER_HEIGHT - self.hint_size.height) / 2.0,
                },
                &self.hint,
                &self.brushes.dim,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
    }

    /// Copies the last frame into `target`, which is how `WM_PRINTCLIENT` answers a screenshot and
    /// how `PrintWindow` sees the panel.
    pub fn blit_to(&self, target: HDC) -> bool {
        // SAFETY: `target` is a live DC from Windows, the DIB-less render target is alive, and the
        // interop interface is released on every path.
        unsafe {
            let Ok(interop) = self.target.cast::<ID2D1GdiInteropRenderTarget>() else {
                return false;
            };
            let Ok(source) = interop.GetDC(D2D1_DC_INITIALIZE_MODE_COPY) else {
                return false;
            };

            let copied = BitBlt(
                target,
                0,
                0,
                i32::try_from(self.pixel.0).unwrap_or(i32::MAX),
                i32::try_from(self.pixel.1).unwrap_or(i32::MAX),
                Some(source),
                0,
                0,
                SRCCOPY,
            )
            .is_ok();

            let _ = interop.ReleaseDC(None);
            copied
        }
    }
}

/// The row geometry's bottom edge, kept in one place so a test can assert the last row fits.
pub fn list_bottom(layout: &LauncherLayout) -> f32 {
    layout.list_top + layout.rows as f32 * ROW_HEIGHT
}

/// The colour of `source` with `alpha`.
fn hue(source: &D2D1_COLOR_F, alpha: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: source.r,
        g: source.g,
        b: source.b,
        a: alpha,
    }
}

/// The same colour, fully opaque: the window's constant alpha is the glass, so the surface itself
/// must not be translucent as well.
fn opaque(source: &ID2D1SolidColorBrush) -> D2D1_COLOR_F {
    let colour = unsafe { source.GetColor() };

    D2D1_COLOR_F {
        r: colour.r,
        g: colour.g,
        b: colour.b,
        a: 1.0,
    }
}

/// The font size a format asks for, as a layout height with room for a descender.
fn format_height(format: &IDWriteTextFormat) -> f32 {
    // SAFETY: `format` is a live interface owned by the caller.
    (unsafe { format.GetFontSize() }) * 1.6
}

/// Creates one text layout.
fn create_layout(
    dwrite: &IDWriteFactory,
    format: &IDWriteTextFormat,
    text: &[u16],
    max: (f32, f32),
) -> Result<IDWriteTextLayout, RenderError> {
    // SAFETY: `text` is live UTF-16, `format` is live, and the two maxima are plain values; the
    // returned layout owns its own copy of the string.
    unsafe { dwrite.CreateTextLayout(text, format, max.0, max.1) }
        .map_err(|error| RenderError::call("create a launcher layout", error))
}

/// Reads a layout's measured size.
fn measure(layout: &IDWriteTextLayout) -> Result<Measured, RenderError> {
    let mut metrics = DWRITE_TEXT_METRICS::default();

    // SAFETY: `metrics` is a live out-parameter of the size the API documents.
    unsafe { layout.GetMetrics(&mut metrics) }
        .map_err(|error| RenderError::call("measure a launcher layout", error))?;

    Ok(Measured {
        width: metrics.widthIncludingTrailingWhitespace,
        height: metrics.height,
    })
}

/// A hash of everything the visible rows and the empty state depend on.
fn fingerprint(
    layout: LauncherLayout,
    top: usize,
    building: bool,
    entries: &[Entry],
    visible: &[usize],
    total: usize,
) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;

    let mix = |hash: &mut u64, byte: u8| {
        *hash ^= u64::from(byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    };

    for number in [
        layout.size.0.to_bits(),
        layout.size.1.to_bits(),
        layout.rows as u32,
        top as u32,
        total as u32,
        u32::from(building),
    ] {
        for byte in number.to_le_bytes() {
            mix(&mut hash, byte);
        }
    }

    for index in visible {
        for byte in (*index as u32).to_le_bytes() {
            mix(&mut hash, byte);
        }

        if let Some(entry) = entries.get(*index) {
            for byte in entry.name.as_bytes() {
                mix(&mut hash, *byte);
            }
            for byte in entry.path.to_string_lossy().as_bytes() {
                mix(&mut hash, byte.to_ascii_lowercase());
            }
        }
    }

    hash
}

/// `text` as a NUL-terminated UTF-16 buffer.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_panel_is_seven_hundred_twenty_by_four_eighty_with_seven_rows() {
        let layout = LauncherLayout::new((PANEL_WIDTH, PANEL_HEIGHT));

        assert_eq!((PANEL_WIDTH, PANEL_HEIGHT), (720.0, 480.0));
        assert_eq!(layout.size, (720.0, 480.0));
        assert_eq!(layout.rows, 7, "the list has seven full rows at 480 DIP");
        assert!(list_bottom(&layout) <= layout.footer_top);
    }

    #[test]
    fn a_row_is_the_row_height_and_the_list_never_overlaps_the_footer() {
        let layout = LauncherLayout::new((PANEL_WIDTH, PANEL_HEIGHT));
        let (left, top, right, bottom) = layout.row_rect(0);
        let (_, last_top, _, last_bottom) = layout.row_rect(layout.rows - 1);

        assert_eq!(bottom - top, ROW_HEIGHT);
        assert_eq!(left, ROW_INSET);
        assert_eq!(right, PANEL_WIDTH - ROW_INSET);
        assert!(last_top >= top);
        assert!(last_bottom <= layout.footer_top);
    }

    #[test]
    fn a_short_panel_still_has_one_row() {
        let layout = LauncherLayout::new((320.0, 120.0));

        assert_eq!(layout.rows, 1);
        assert!(layout.query.2 >= 1.0);
    }

    #[test]
    fn the_query_control_sits_inside_the_header() {
        let layout = LauncherLayout::new((PANEL_WIDTH, PANEL_HEIGHT));
        let (left, top, width, height) = layout.query;

        assert_eq!(left, PADDING);
        assert!(top >= 0.0);
        assert!(top + height <= HEADER_HEIGHT);
        assert_eq!(width, PANEL_WIDTH - PADDING * 2.0);
    }

    #[test]
    fn a_point_maps_to_a_row_only_inside_the_list() {
        let layout = LauncherLayout::new((PANEL_WIDTH, PANEL_HEIGHT));

        assert_eq!(layout.row_at(layout.list_top + 1.0), Some(0));
        assert_eq!(layout.row_at(layout.list_top + ROW_HEIGHT + 1.0), Some(1));
        assert_eq!(layout.row_at(layout.list_top - 1.0), None);
        assert_eq!(layout.row_at(layout.footer_top), None);
    }

    #[test]
    fn the_glass_constants_are_the_ones_the_brief_names() {
        assert_eq!(CORNER_RADIUS, 18.0);
        assert_eq!(GLASS_ALPHA, 245, "245/255 is 96 %");
        assert_eq!(ROW_ICON, 32.0);
    }

    #[test]
    fn the_fingerprint_follows_the_visible_results_and_the_state() {
        let layout = LauncherLayout::new((PANEL_WIDTH, PANEL_HEIGHT));
        let entries = vec![Entry {
            name: "Paint".to_owned(),
            path: "C:/Windows/System32/mspaint.exe".into(),
        }];

        let first = fingerprint(layout, 0, false, &entries, &[0], 1);

        assert_eq!(first, fingerprint(layout, 0, false, &entries, &[0], 1));
        assert_ne!(first, fingerprint(layout, 0, true, &entries, &[0], 1));
        assert_ne!(first, fingerprint(layout, 1, false, &entries, &[0], 1));
        assert_ne!(first, fingerprint(layout, 0, false, &entries, &[], 0));
        assert_ne!(
            first,
            fingerprint(
                layout,
                0,
                false,
                &[Entry {
                    name: "Other".to_owned(),
                    path: "C:/Other.exe".into()
                }],
                &[0],
                1
            )
        );
    }
}
