//! The scene: what the aurora looks like at a given moment, as numbers.
//!
//! Everything in this module is pure arithmetic on `f32`s, so the animation is testable without a
//! GPU: the shader only evaluates the shapes this module places. The one hard rule is that a frame
//! costs no allocation — [`Frame::fill`] writes into a caller-owned constant buffer.

use crate::config;

/// Seconds after which the whole animation repeats exactly.
///
/// Every motion below is a sine of `2π · time / LOOP`, so the uniform block at `t` and at
/// `t + LOOP` is bit-identical; a long-running wallpaper cannot drift into a different scene.
pub const LOOP_SECONDS: f32 = 64.0;

/// How many colour blobs the shader mixes.
pub const BLOB_COUNT: usize = 3;

/// Base colours of the aurora, sRGB. They are blended toward the configured colour so a user's
/// palette is visible without losing the shape that makes the scene readable.
pub const ACCENTS: [[u8; 3]; BLOB_COUNT] =
    [[0x76, 0x5b, 0xff], [0x24, 0xd7, 0xc4], [0xff, 0x74, 0xc4]];

/// How much of each accent is replaced by the configured colour, in `0..=1`.
pub const ACCENT_BLEND: f32 = 0.25;

/// The colours one frame mixes, in linear RGB.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// The flat colour the whole scene falls back to at its edges.
    pub base: [f32; 3],
    /// The three blob colours.
    pub blobs: [[f32; 3]; BLOB_COUNT],
}

impl Palette {
    /// Builds the palette for a configured `#rrggbb` colour.
    pub fn from_colour(text: &str) -> Self {
        let base = linear_from_hex(&config::normalise_colour(text));
        let blobs = ACCENTS.map(|accent| {
            let accent = linear_from_srgb(accent);
            mix(accent, base, ACCENT_BLEND)
        });

        Self { base, blobs }
    }

    /// The palette in the flat sRGB form a log line or the IPC answer shows.
    pub fn describe(&self) -> String {
        let to_hex = |colour: [f32; 3]| {
            let rgb = colour.map(srgb_from_linear);
            format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
        };

        format!(
            "{} + {} {} {}",
            to_hex(self.base),
            to_hex(self.blobs[0]),
            to_hex(self.blobs[1]),
            to_hex(self.blobs[2])
        )
    }
}

/// One frame of uniforms, laid out exactly as `shaders/aurora.hlsl` declares its `b0` block.
///
/// Four `float4`s: 64 bytes, the smallest constant buffer D3D11 allows, and no padding the shader
/// would read past.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Frame {
    /// `x` = seconds, `y` = width in pixels, `z` = height in pixels, `w` = intensity.
    pub params: [f32; 4],
    /// First blob colour, linear RGB.
    pub colour_a: [f32; 4],
    /// Second blob colour, linear RGB.
    pub colour_b: [f32; 4],
    /// Third blob colour, linear RGB.
    pub colour_c: [f32; 4],
}

impl Frame {
    /// Writes the uniforms for `elapsed` seconds at `width` × `height` into `out`.
    ///
    /// Takes `out` by `&mut` rather than returning a value so the steady state reuses the buffer
    /// the renderer already mapped; nothing here allocates.
    pub fn fill(out: &mut Self, elapsed: f32, width: u32, height: u32, palette: &Palette) {
        let time = wrap(elapsed);

        out.params = [
            time,
            width.max(1) as f32,
            height.max(1) as f32,
            intensity(time),
        ];
        out.colour_a = with_alpha(palette.blobs[0]);
        out.colour_b = with_alpha(palette.blobs[1]);
        out.colour_c = with_alpha(palette.blobs[2]);
    }
}

/// `elapsed` folded into `0..LOOP_SECONDS`, so the shader's time never grows without bound and the
/// sine arguments stay accurate after hours.
pub fn wrap(elapsed: f32) -> f32 {
    if !elapsed.is_finite() {
        return 0.0;
    }

    elapsed.rem_euclid(LOOP_SECONDS)
}

/// The `2π · time / LOOP` phase every motion is built from.
pub fn phase(time: f32) -> f32 {
    std::f32::consts::TAU * wrap(time) / LOOP_SECONDS
}

/// The centre of blob `index`, in the shader's `0..1` UV space.
///
/// Each blob uses its own integer harmonics, so the pattern does not collapse into a single
/// oscillating line, and all of them share the loop period.
pub fn blob_centre(index: usize, time: f32) -> [f32; 2] {
    let angle = phase(time) + std::f32::consts::TAU * (index as f32) / (BLOB_COUNT as f32);
    let (sin, cos) = angle.sin_cos();

    match index % BLOB_COUNT {
        0 => [0.5 + 0.30 * cos, 0.34 + 0.16 * (2.0 * angle).sin()],
        1 => [
            0.5 + 0.34 * (angle + 2.0).sin(),
            0.62 + 0.14 * (3.0 * angle).cos(),
        ],
        _ => [0.5 + 0.26 * (2.0 * angle + 1.0).cos(), 0.5 + 0.22 * sin],
    }
}

/// The brightness the scene pulses between, in `0.75..=1.0`.
///
/// A slow breath keeps the wallpaper alive when the shapes themselves are at a still point.
pub fn intensity(time: f32) -> f32 {
    0.875 + 0.125 * phase(time).sin()
}

/// sRGB byte to linear `f32` in `0..=1`.
pub fn linear_from_byte(value: u8) -> f32 {
    let channel = f32::from(value) / 255.0;
    if channel <= 0.040_45 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear `f32` back to an sRGB byte, for logging and IPC.
pub fn srgb_from_linear(value: f32) -> u8 {
    let channel = value.clamp(0.0, 1.0);
    let srgb = if channel <= 0.003_130_8 {
        channel * 12.92
    } else {
        1.055 * channel.powf(1.0 / 2.4) - 0.055
    };

    (srgb * 255.0).round().clamp(0.0, 255.0) as u8
}

/// A whole `#rrggbb` colour in linear RGB; unusable text is the shipped colour.
pub fn linear_from_hex(text: &str) -> [f32; 3] {
    let rgb = config::parse_colour(text)
        .or_else(|| config::parse_colour(neb_core::config::DEFAULT_WALLPAPER_COLOR))
        .unwrap_or([0x14, 0x10, 0x21]);

    linear_from_srgb(rgb)
}

/// sRGB bytes to linear `f32`.
pub fn linear_from_srgb(rgb: [u8; 3]) -> [f32; 3] {
    rgb.map(linear_from_byte)
}

/// Component-wise linear interpolation; `amount` is clamped.
pub fn mix(from: [f32; 3], to: [f32; 3], amount: f32) -> [f32; 3] {
    let amount = amount.clamp(0.0, 1.0);
    [
        from[0] + (to[0] - from[0]) * amount,
        from[1] + (to[1] - from[1]) * amount,
        from[2] + (to[2] - from[2]) * amount,
    ]
}

fn with_alpha(colour: [f32; 3]) -> [f32; 4] {
    [colour[0], colour[1], colour[2], 1.0]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette() -> Palette {
        Palette::from_colour("#141021")
    }

    #[test]
    fn the_time_wraps_at_the_loop_and_never_goes_negative() {
        assert_eq!(wrap(0.0), 0.0);
        assert_eq!(wrap(LOOP_SECONDS), 0.0);
        assert!((wrap(LOOP_SECONDS + 3.0) - 3.0).abs() < 1e-4);
        assert!((wrap(-1.0) - (LOOP_SECONDS - 1.0)).abs() < 1e-3);
        assert_eq!(wrap(f32::NAN), 0.0);
        assert_eq!(wrap(f32::INFINITY), 0.0);
    }

    #[test]
    fn the_frame_repeats_exactly_after_one_loop() {
        let palette = palette();
        let mut first = Frame::default();
        let mut later = Frame::default();

        Frame::fill(&mut first, 7.25, 1920, 1080, &palette);
        Frame::fill(&mut later, 7.25 + LOOP_SECONDS * 4.0, 1920, 1080, &palette);

        // The sine of a wrapped phase is periodic, not merely close: the same arguments produce the
        // same bits, which is what lets the loop run for days without drifting.
        assert_eq!(first, later);
    }

    #[test]
    fn the_uniforms_carry_the_size_and_never_a_zero() {
        let palette = palette();
        let mut frame = Frame::default();

        Frame::fill(&mut frame, 0.0, 0, 0, &palette);

        assert_eq!(frame.params[1], 1.0);
        assert_eq!(frame.params[2], 1.0);
        assert!(frame.params[3] >= 0.75 && frame.params[3] <= 1.0);

        Frame::fill(&mut frame, 0.0, 3840, 2160, &palette);
        assert_eq!(frame.params[1], 3840.0);
        assert_eq!(frame.params[2], 2160.0);
        assert_eq!(std::mem::size_of::<Frame>(), 64);
    }

    #[test]
    fn every_blob_stays_on_screen_for_the_whole_loop() {
        for step in 0..=512 {
            let time = LOOP_SECONDS * step as f32 / 512.0;

            for index in 0..BLOB_COUNT {
                let [x, y] = blob_centre(index, time);
                assert!(
                    (0.05..=0.95).contains(&x) && (0.05..=0.95).contains(&y),
                    "blob {index} left the frame at {time}s: {x},{y}"
                );
            }
        }
    }

    #[test]
    fn the_blobs_stay_distinct_and_actually_move() {
        let mut leftmost = [f32::MAX; BLOB_COUNT];
        let mut rightmost = [f32::MIN; BLOB_COUNT];

        for step in 0..=512 {
            let time = LOOP_SECONDS * step as f32 / 512.0;

            for index in 0..BLOB_COUNT {
                let centre = blob_centre(index, time);
                leftmost[index] = leftmost[index].min(centre[0]);
                rightmost[index] = rightmost[index].max(centre[0]);

                for other in (index + 1)..BLOB_COUNT {
                    let there = blob_centre(other, time);
                    let distance =
                        ((centre[0] - there[0]).powi(2) + (centre[1] - there[1]).powi(2)).sqrt();
                    assert!(
                        distance > 1e-3,
                        "blobs {index} and {other} collapse into one at {time}s"
                    );
                }
            }
        }

        for index in 0..BLOB_COUNT {
            assert!(
                rightmost[index] - leftmost[index] > 0.2,
                "blob {index} never travels across the frame"
            );
        }
    }

    #[test]
    fn the_intensity_breathes_inside_its_band_and_returns_each_loop() {
        for step in 0..=256 {
            let time = LOOP_SECONDS * step as f32 / 256.0;
            let value = intensity(time);

            assert!((0.7..=1.0).contains(&value), "{value} at {time}s");
            assert!((value - intensity(time + LOOP_SECONDS)).abs() < 1e-5);
        }

        assert!(intensity(0.0) < intensity(LOOP_SECONDS / 4.0));
    }

    #[test]
    fn srgb_conversion_round_trips_and_clamps() {
        for value in [0u8, 1, 17, 128, 200, 255] {
            assert_eq!(srgb_from_linear(linear_from_byte(value)), value);
        }

        assert_eq!(srgb_from_linear(-1.0), 0);
        assert_eq!(srgb_from_linear(9.0), 255);
        assert!((linear_from_byte(255) - 1.0).abs() < 1e-6);
        assert_eq!(linear_from_byte(0), 0.0);
    }

    #[test]
    fn a_palette_keeps_the_configured_colour_as_its_base() {
        let dark = Palette::from_colour("#141021");
        let light = Palette::from_colour("#ffffff");

        assert!(light.base.iter().all(|channel| *channel > 0.9));
        assert!(dark.base.iter().all(|channel| *channel < 0.05));

        // The accents are pulled a quarter of the way toward the base, so they differ from one
        // palette to the next but never become the base.
        for index in 0..BLOB_COUNT {
            assert!(
                dark.blobs[index] != light.blobs[index],
                "blob {index} ignores the configured colour"
            );
        }

        assert_eq!(Palette::from_colour("not a colour"), dark);
        assert!(dark.describe().starts_with('#'));
    }

    #[test]
    fn mix_clamps_and_hits_both_ends() {
        let black = [0.0, 0.0, 0.0];
        let white = [1.0, 1.0, 1.0];

        assert_eq!(mix(black, white, 0.0), black);
        assert_eq!(mix(black, white, 1.0), white);
        assert_eq!(mix(black, white, -3.0), black);
        assert_eq!(mix(black, white, 3.0), white);
        assert_eq!(mix(black, white, 0.5), [0.5, 0.5, 0.5]);
    }
}
