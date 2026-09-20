//! BGRA <-> I420 colour conversion, BT.601 limited range.
//!
//! Screen capture hands us BGRA and H.264 wants I420, so this conversion sits
//! on the hot path twice per session: once per captured frame on the host and
//! once per decoded frame on the viewer. At 60 Hz on an 8 MP display that is
//! half a billion pixels a second, so nothing here allocates per frame —
//! callers keep an [`I420Buffer`] and a `Vec<u8>` alive across frames and both
//! are resized only when the display resolution actually changes.
//!
//! BT.601 limited range is what an H.264 stream means when it says nothing, and
//! it is what the encoder is configured to signal in its VUI, so host and
//! viewer agree on the matrix even if the viewer is some other client.

use crate::CodecError;

/// Rounding constant for the fixed-point matrices below: adding half of the
/// 1/256 quantum before shifting turns truncation into round-to-nearest.
const HALF: i32 = 128;

/// BT.601 limited-range luma, `Y = 16 + (65.738 R + 129.057 G + 25.064 B)/256`.
///
/// Inputs are `0..=255`, so the result is provably inside the 16..=235 studio
/// range and the cast cannot truncate.
#[inline]
fn rgb_to_y(r: i32, g: i32, b: i32) -> u8 {
    ((((66 * r + 129 * g + 25 * b + HALF) >> 8) + 16) & 0xFF) as u8
}

/// BT.601 limited-range Cb. Result is provably inside 16..=240.
#[inline]
fn rgb_to_u(r: i32, g: i32, b: i32) -> u8 {
    ((((-38 * r - 74 * g + 112 * b + HALF) >> 8) + 128) & 0xFF) as u8
}

/// BT.601 limited-range Cr. Result is provably inside 16..=240.
#[inline]
fn rgb_to_v(r: i32, g: i32, b: i32) -> u8 {
    ((((112 * r - 94 * g - 18 * b + HALF) >> 8) + 128) & 0xFF) as u8
}

/// Reusable I420 (planar 4:2:0) frame.
///
/// The chroma planes are `ceil(w/2) x ceil(h/2)`: rounding *up* means a 1x1 or
/// 3x3 frame still has a chroma sample for its final odd column and row instead
/// of a half-written plane.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct I420Buffer {
    width: u32,
    height: u32,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl I420Buffer {
    /// An empty buffer, to be sized by the first conversion into it.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocates a buffer for a known frame size.
    pub fn with_dimensions(width: u32, height: u32) -> Self {
        let mut buffer = Self::new();
        buffer.resize(width, height);
        buffer
    }

    /// Sizes the planes for `width x height`, keeping the existing allocation
    /// when the dimensions are unchanged — the common case, frame after frame.
    pub fn resize(&mut self, width: u32, height: u32) {
        if self.width == width && self.height == height && self.planes_are_sized() {
            return;
        }
        let (w, h) = (width as usize, height as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        self.width = width;
        self.height = height;
        // `resize` reuses the existing capacity when shrinking, so switching
        // between two monitors back and forth allocates at most twice.
        self.y.resize(w * h, 0);
        self.u.resize(cw * ch, 0);
        self.v.resize(cw * ch, 0);
    }

    fn planes_are_sized(&self) -> bool {
        let (w, h) = (self.width as usize, self.height as usize);
        let chroma = w.div_ceil(2) * h.div_ceil(2);
        self.y.len() == w * h && self.u.len() == chroma && self.v.len() == chroma
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Plane strides as `(y, u, v)`, in bytes. The planes are tightly packed,
    /// so these are the plane widths.
    pub fn strides(&self) -> (usize, usize, usize) {
        let cw = (self.width as usize).div_ceil(2);
        (self.width as usize, cw, cw)
    }

    pub fn y(&self) -> &[u8] {
        &self.y
    }

    pub fn u(&self) -> &[u8] {
        &self.u
    }

    pub fn v(&self) -> &[u8] {
        &self.v
    }
}

/// Converts a BGRA frame (the layout every capture backend gives us) into
/// `out`, resizing `out` only when the frame size changed.
///
/// `stride` is the distance in bytes between the starts of two rows and may
/// exceed `width * 4` — capture backends routinely pad rows to a hardware
/// alignment, and the padding must not leak into the picture.
pub fn bgra_to_i420(
    bgra: &[u8],
    width: u32,
    height: u32,
    stride: usize,
    out: &mut I420Buffer,
) -> Result<(), CodecError> {
    let (w, h) = check_dimensions(width, height)?;

    let min_stride = w * 4;
    if stride < min_stride {
        return Err(CodecError::StrideTooSmall {
            stride,
            minimum: min_stride,
        });
    }
    // Only the used part of the final row has to be present: a capture buffer
    // is allowed to end right after the last pixel rather than after its
    // padding, and rejecting that would fail on perfectly valid frames.
    let needed = (h - 1) * stride + min_stride;
    if bgra.len() < needed {
        return Err(CodecError::BufferTooSmall {
            expected: needed,
            actual: bgra.len(),
        });
    }

    out.resize(width, height);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));

    for row in 0..h {
        let src = &bgra[row * stride..row * stride + min_stride];
        let dst = &mut out.y[row * w..row * w + w];
        for (px, y) in src.as_chunks::<4>().0.iter().zip(dst.iter_mut()) {
            *y = rgb_to_y(i32::from(px[2]), i32::from(px[1]), i32::from(px[0]));
        }
    }

    // Chroma is averaged over each 2x2 block of *linear-ish* BGRA rather than
    // point-sampled: point sampling throws away three quarters of the colour
    // information and shows up as fringing on text, which is most of what a
    // remote desktop transmits. Blocks on an odd edge cover 2 or 1 pixels.
    for cy in 0..ch {
        for cx in 0..cw {
            let mut sum = [0u32; 3];
            let mut count = 0u32;
            for dy in 0..2 {
                let row = cy * 2 + dy;
                if row >= h {
                    break;
                }
                for dx in 0..2 {
                    let col = cx * 2 + dx;
                    if col >= w {
                        break;
                    }
                    let px = row * stride + col * 4;
                    sum[0] += u32::from(bgra[px]);
                    sum[1] += u32::from(bgra[px + 1]);
                    sum[2] += u32::from(bgra[px + 2]);
                    count += 1;
                }
            }
            // `count` is at least one: cy < ch and cx < cw guarantee the
            // top-left pixel of the block is inside the frame.
            let b = (sum[0] / count) as i32;
            let g = (sum[1] / count) as i32;
            let r = (sum[2] / count) as i32;
            out.u[cy * cw + cx] = rgb_to_u(r, g, b);
            out.v[cy * cw + cx] = rgb_to_v(r, g, b);
        }
    }

    Ok(())
}

/// Converts I420 planes back to RGBA, resizing `out` to `width * height * 4`.
///
/// The planes may be strided (the decoder hands back hardware-aligned rows) and
/// the chroma planes are read as `ceil(w/2) x ceil(h/2)`.
pub fn i420_to_rgba(
    y: &[u8],
    u: &[u8],
    v: &[u8],
    strides: (usize, usize, usize),
    width: u32,
    height: u32,
    out: &mut Vec<u8>,
) -> Result<(), CodecError> {
    let (w, h) = check_dimensions(width, height)?;
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let (sy, su, sv) = strides;

    for (stride, minimum) in [(sy, w), (su, cw), (sv, cw)] {
        if stride < minimum {
            return Err(CodecError::StrideTooSmall { stride, minimum });
        }
    }
    for (plane, rows, stride, row_len) in [(y, h, sy, w), (u, ch, su, cw), (v, ch, sv, cw)] {
        let needed = (rows - 1) * stride + row_len;
        if plane.len() < needed {
            return Err(CodecError::BufferTooSmall {
                expected: needed,
                actual: plane.len(),
            });
        }
    }

    // Resize rather than clear+extend: the buffer keeps its capacity and, when
    // the resolution is unchanged, this is a no-op.
    out.resize(w * h * 4, 0);

    for row in 0..h {
        let y_row = &y[row * sy..row * sy + w];
        let c_row = row / 2;
        let u_row = &u[c_row * su..c_row * su + cw];
        let v_row = &v[c_row * sv..c_row * sv + cw];
        let dst = &mut out[row * w * 4..(row + 1) * w * 4];

        for (col, px) in dst.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            // BT.601 limited-range inverse, same fixed-point scale as above.
            let c = i32::from(y_row[col]) - 16;
            let d = i32::from(u_row[col / 2]) - 128;
            let e = i32::from(v_row[col / 2]) - 128;
            let y298 = 298 * c + HALF;
            px[0] = clamp_u8((y298 + 409 * e) >> 8);
            px[1] = clamp_u8((y298 - 100 * d - 208 * e) >> 8);
            px[2] = clamp_u8((y298 + 516 * d) >> 8);
            px[3] = 255;
        }
    }

    Ok(())
}

#[inline]
fn clamp_u8(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

/// Rejects zero-sized frames once, so every loop below can assume `w >= 1` and
/// `h >= 1` and index the first row without a guard.
fn check_dimensions(width: u32, height: u32) -> Result<(usize, usize), CodecError> {
    if width == 0 || height == 0 {
        return Err(CodecError::ZeroDimensions { width, height });
    }
    Ok((width as usize, height as usize))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a tightly packed BGRA frame from a pixel function returning RGB.
    fn bgra_frame(w: u32, h: u32, px: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let [r, g, b] = px(x, y);
                out.extend_from_slice(&[b, g, r, 255]);
            }
        }
        out
    }

    fn solid(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
        bgra_frame(w, h, |_, _| rgb)
    }

    /// Float BT.601 limited-range reference, independent of the fixed-point
    /// implementation, used to prove the integer matrices are not merely
    /// self-consistent.
    fn reference_yuv(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
        (
            16.0 + (65.481 * r + 128.553 * g + 24.966 * b) / 255.0,
            128.0 + (-37.797 * r - 74.203 * g + 112.0 * b) / 255.0,
            128.0 + (112.0 * r - 93.786 * g - 18.214 * b) / 255.0,
        )
    }

    #[test]
    fn converts_the_primary_colours_to_their_bt601_values() {
        // Exact expected output of the documented fixed-point matrices; the
        // next test independently proves these track the BT.601 definition.
        let cases: [([u8; 3], [u8; 3]); 8] = [
            ([0, 0, 0], [16, 128, 128]),
            ([255, 255, 255], [235, 128, 128]),
            ([255, 0, 0], [82, 90, 240]),
            ([0, 255, 0], [144, 54, 34]),
            ([0, 0, 255], [41, 240, 110]),
            ([255, 255, 0], [210, 16, 146]),
            ([0, 255, 255], [169, 166, 16]),
            ([255, 0, 255], [107, 202, 222]),
        ];

        let mut buf = I420Buffer::new();
        for (rgb, [ey, eu, ev]) in cases {
            let frame = solid(2, 2, rgb);
            bgra_to_i420(&frame, 2, 2, 8, &mut buf).unwrap();
            assert_eq!(buf.y(), [ey; 4], "luma for {rgb:?}");
            assert_eq!(buf.u(), [eu], "cb for {rgb:?}");
            assert_eq!(buf.v(), [ev], "cr for {rgb:?}");
        }
    }

    #[test]
    fn stays_within_one_step_of_the_floating_point_bt601_matrix() {
        let mut buf = I420Buffer::new();
        for r in (0..=255).step_by(17) {
            for g in (0..=255).step_by(51) {
                for b in (0..=255).step_by(85) {
                    let frame = solid(2, 2, [r, g, b]);
                    bgra_to_i420(&frame, 2, 2, 8, &mut buf).unwrap();
                    let (ry, ru, rv) = reference_yuv(f64::from(r), f64::from(g), f64::from(b));
                    let diff = |got: u8, want: f64| (f64::from(got) - want).abs();
                    assert!(diff(buf.y()[0], ry) <= 1.0, "Y {r},{g},{b}: {ry}");
                    assert!(diff(buf.u()[0], ru) <= 1.0, "U {r},{g},{b}: {ru}");
                    assert!(diff(buf.v()[0], rv) <= 1.0, "V {r},{g},{b}: {rv}");
                }
            }
        }
    }

    #[test]
    fn luma_and_chroma_never_leave_the_studio_range() {
        let mut buf = I420Buffer::new();
        for r in (0..=255).step_by(15) {
            for g in (0..=255).step_by(15) {
                for b in (0..=255).step_by(15) {
                    let frame = solid(2, 2, [r, g, b]);
                    bgra_to_i420(&frame, 2, 2, 8, &mut buf).unwrap();
                    assert!(
                        (16..=235).contains(&buf.y()[0]),
                        "Y out of range {r},{g},{b}"
                    );
                    assert!(
                        (16..=240).contains(&buf.u()[0]),
                        "U out of range {r},{g},{b}"
                    );
                    assert!(
                        (16..=240).contains(&buf.v()[0]),
                        "V out of range {r},{g},{b}"
                    );
                }
            }
        }
    }

    #[test]
    fn ignores_row_padding_when_the_stride_exceeds_the_width() {
        let (w, h) = (7u32, 5u32);
        let tight = bgra_frame(w, h, |x, y| [(x * 31) as u8, (y * 47) as u8, 0x5A]);

        // Same picture, rows padded to 40 bytes with a colour that must not
        // reach the output.
        let stride = 40usize;
        let mut padded = vec![0xFFu8; stride * h as usize];
        for row in 0..h as usize {
            let row_bytes = (w * 4) as usize;
            padded[row * stride..row * stride + row_bytes]
                .copy_from_slice(&tight[row * row_bytes..(row + 1) * row_bytes]);
        }

        let mut a = I420Buffer::new();
        let mut b = I420Buffer::new();
        bgra_to_i420(&tight, w, h, (w * 4) as usize, &mut a).unwrap();
        bgra_to_i420(&padded, w, h, stride, &mut b).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn sizes_chroma_planes_by_rounding_up_on_odd_dimensions() {
        for (w, h) in [(1u32, 1u32), (1, 2), (2, 1), (3, 3), (5, 7), (7, 5)] {
            let frame = bgra_frame(w, h, |x, y| [(x * 9) as u8, (y * 9) as u8, 0]);
            let mut buf = I420Buffer::new();
            bgra_to_i420(&frame, w, h, (w * 4) as usize, &mut buf).unwrap();

            let (cw, ch) = ((w as usize).div_ceil(2), (h as usize).div_ceil(2));
            assert_eq!(buf.y().len(), (w * h) as usize, "{w}x{h} luma");
            assert_eq!(buf.u().len(), cw * ch, "{w}x{h} cb");
            assert_eq!(buf.v().len(), cw * ch, "{w}x{h} cr");
            assert_eq!(buf.strides(), (w as usize, cw, cw));

            // And back out again, which is where an off-by-one in the chroma
            // edge blocks would index past the end of a plane.
            let mut rgba = Vec::new();
            i420_to_rgba(buf.y(), buf.u(), buf.v(), buf.strides(), w, h, &mut rgba).unwrap();
            assert_eq!(rgba.len(), (w * h * 4) as usize);
        }
    }

    #[test]
    fn averages_chroma_over_partial_blocks_on_an_odd_edge() {
        // A 3x1 frame: the second chroma block covers the single last pixel, so
        // it must carry that pixel's colour, not a half-summed average.
        let frame = bgra_frame(3, 1, |x, _| match x {
            0 | 1 => [255, 0, 0],
            _ => [0, 0, 255],
        });
        let mut buf = I420Buffer::new();
        bgra_to_i420(&frame, 3, 1, 12, &mut buf).unwrap();

        assert_eq!(buf.u(), [90, 240], "left block is red, right block is blue");
        assert_eq!(buf.v(), [240, 110]);
    }

    #[test]
    fn round_trips_a_gradient_within_five_levels_per_channel() {
        let (w, h) = (64u32, 48u32);
        // A smooth gradient: 4:2:0 subsampling is near-lossless on it, so a
        // tight tolerance here would still catch a wrong matrix or a swapped
        // channel, which a flat colour would not.
        let src = bgra_frame(w, h, |x, y| {
            [(x * 4) as u8, (y * 5) as u8, ((x + y) * 2) as u8]
        });

        let mut yuv = I420Buffer::new();
        bgra_to_i420(&src, w, h, (w * 4) as usize, &mut yuv).unwrap();
        let mut rgba = Vec::new();
        i420_to_rgba(yuv.y(), yuv.u(), yuv.v(), yuv.strides(), w, h, &mut rgba).unwrap();

        let mut worst = 0i32;
        let mut total = 0i64;
        let mut count = 0i64;
        for (src_px, out_px) in src.as_chunks::<4>().0.iter().zip(rgba.as_chunks::<4>().0) {
            // BGRA in, RGBA out: the channel order swap is part of what is
            // being asserted here.
            for (s, o) in [
                (src_px[2], out_px[0]),
                (src_px[1], out_px[1]),
                (src_px[0], out_px[2]),
            ] {
                let delta = (i32::from(s) - i32::from(o)).abs();
                worst = worst.max(delta);
                total += i64::from(delta);
                count += 1;
            }
            assert_eq!(out_px[3], 255, "alpha is opaque");
        }
        // The blue ramp changes on every pixel, so 4:2:0 genuinely cannot carry
        // it exactly and a handful of levels of error is the codec working as
        // designed. The mean is the bound that would catch a wrong matrix: a
        // swapped coefficient or a full-range one lands an order of magnitude
        // above it, while honest subsampling loss stays near one level.
        assert!(worst <= 5, "worst per-channel round-trip error was {worst}");
        let mean = total as f64 / count as f64;
        assert!(mean < 1.5, "mean per-channel round-trip error was {mean}");
    }

    #[test]
    fn round_trips_saturated_colours_within_five_levels_per_channel() {
        // Hard-clipped primaries are the worst case for a limited-range matrix:
        // they sit at the edge of the representable gamut, so the tolerance is
        // looser than for the gradient above but still bounded.
        let mut yuv = I420Buffer::new();
        let mut rgba = Vec::new();
        for rgb in [
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [255, 255, 255],
            [0, 0, 0],
        ] {
            let src = solid(4, 4, rgb);
            bgra_to_i420(&src, 4, 4, 16, &mut yuv).unwrap();
            i420_to_rgba(yuv.y(), yuv.u(), yuv.v(), yuv.strides(), 4, 4, &mut rgba).unwrap();
            for px in rgba.as_chunks::<4>().0 {
                for (c, want) in px[..3].iter().zip(rgb.iter()) {
                    let delta = (i32::from(*c) - i32::from(*want)).abs();
                    assert!(delta <= 5, "{rgb:?} came back {px:?} (delta {delta})");
                }
            }
        }
    }

    #[test]
    fn rejects_zero_dimensions_instead_of_panicking() {
        let mut buf = I420Buffer::new();
        for (w, h) in [(0u32, 0u32), (0, 8), (8, 0)] {
            assert_eq!(
                bgra_to_i420(&[0; 4096], w, h, 4096, &mut buf),
                Err(CodecError::ZeroDimensions {
                    width: w,
                    height: h
                })
            );
            let mut out = Vec::new();
            assert_eq!(
                i420_to_rgba(&[0; 64], &[0; 64], &[0; 64], (8, 4, 4), w, h, &mut out),
                Err(CodecError::ZeroDimensions {
                    width: w,
                    height: h
                })
            );
        }
    }

    #[test]
    fn rejects_a_source_buffer_shorter_than_the_frame() {
        let mut buf = I420Buffer::new();
        let frame = solid(8, 8, [1, 2, 3]);
        assert!(bgra_to_i420(&frame, 8, 8, 32, &mut buf).is_ok());
        assert_eq!(
            bgra_to_i420(&frame[..frame.len() - 1], 8, 8, 32, &mut buf),
            Err(CodecError::BufferTooSmall {
                expected: 8 * 32,
                actual: 8 * 32 - 1
            })
        );
    }

    #[test]
    fn rejects_a_stride_narrower_than_the_frame() {
        let mut buf = I420Buffer::new();
        let frame = solid(8, 8, [1, 2, 3]);
        assert_eq!(
            bgra_to_i420(&frame, 8, 8, 31, &mut buf),
            Err(CodecError::StrideTooSmall {
                stride: 31,
                minimum: 32
            })
        );

        let mut out = Vec::new();
        assert_eq!(
            i420_to_rgba(&[0; 64], &[0; 16], &[0; 16], (7, 4, 4), 8, 8, &mut out),
            Err(CodecError::StrideTooSmall {
                stride: 7,
                minimum: 8
            })
        );
    }

    #[test]
    fn rejects_short_chroma_planes_instead_of_reading_out_of_bounds() {
        let mut out = Vec::new();
        // 8x8 needs 4x4 chroma; hand it one row too few.
        let err = i420_to_rgba(&[0; 64], &[0; 12], &[0; 16], (8, 4, 4), 8, 8, &mut out);
        assert_eq!(
            err,
            Err(CodecError::BufferTooSmall {
                expected: 16,
                actual: 12
            })
        );
    }

    #[test]
    fn accepts_planes_that_stop_after_the_last_used_pixel() {
        // A decoder may hand back a final row without its stride padding; that
        // is a complete picture and must not be rejected as too small.
        let (w, h) = (6usize, 4usize);
        let (sy, sc) = (16usize, 9usize);
        let y = vec![128u8; (h - 1) * sy + w];
        let u = vec![128u8; (h / 2 - 1) * sc + w.div_ceil(2)];
        let v = u.clone();
        let mut out = Vec::new();
        i420_to_rgba(&y, &u, &v, (sy, sc, sc), w as u32, h as u32, &mut out).unwrap();
        assert_eq!(out.len(), w * h * 4);
    }

    #[test]
    fn reuses_its_allocation_across_frames_of_the_same_size() {
        let mut buf = I420Buffer::with_dimensions(64, 64);
        let (y_ptr, cap) = (buf.y().as_ptr(), buf.y().len());
        let frame = solid(64, 64, [10, 20, 30]);
        for _ in 0..8 {
            bgra_to_i420(&frame, 64, 64, 256, &mut buf).unwrap();
        }
        assert_eq!(buf.y().as_ptr(), y_ptr, "luma plane was reallocated");
        assert_eq!(buf.y().len(), cap);

        // Switching monitors resizes; switching back must still be correct.
        bgra_to_i420(&solid(32, 16, [10, 20, 30]), 32, 16, 128, &mut buf).unwrap();
        assert_eq!((buf.width(), buf.height()), (32, 16));
        assert_eq!(buf.y().len(), 32 * 16);
        assert_eq!(buf.u().len(), 16 * 8);
        bgra_to_i420(&frame, 64, 64, 256, &mut buf).unwrap();
        assert_eq!(buf.y().len(), 64 * 64);
        assert!(buf.y().iter().all(|&y| y == buf.y()[0]));
    }

    #[test]
    fn writes_every_pixel_of_a_reused_output_vec() {
        // A stale, over-long buffer from a bigger frame must be truncated and
        // fully overwritten, never partially reused.
        let mut out = vec![0x7Fu8; 4096];
        let y = vec![235u8; 16];
        let uv = vec![128u8; 4];
        i420_to_rgba(&y, &uv, &uv, (4, 2, 2), 4, 4, &mut out).unwrap();
        assert_eq!(out.len(), 4 * 4 * 4);
        for px in out.as_chunks::<4>().0 {
            assert!(
                px[0] > 250 && px[1] > 250 && px[2] > 250,
                "white expected, got {px:?}"
            );
        }
    }

    #[test]
    fn clamps_out_of_gamut_chroma_rather_than_wrapping() {
        // Y=235 with extreme chroma overflows every channel; the result must
        // saturate at 255 instead of wrapping to a dark pixel.
        let y = vec![235u8; 4];
        let u = vec![255u8; 1];
        let v = vec![255u8; 1];
        let mut out = Vec::new();
        i420_to_rgba(&y, &u, &v, (2, 1, 1), 2, 2, &mut out).unwrap();
        for px in out.as_chunks::<4>().0 {
            assert_eq!(px[0], 255);
            assert_eq!(px[2], 255);
        }

        // And the opposite end: negative luma with extreme chroma clamps to 0.
        let y = vec![0u8; 4];
        i420_to_rgba(&y, &[0; 1], &[0; 1], (2, 1, 1), 2, 2, &mut out).unwrap();
        for px in out.as_chunks::<4>().0 {
            assert_eq!(px[0], 0);
            assert_eq!(px[2], 0);
        }
    }
}
