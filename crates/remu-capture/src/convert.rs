//! Every pixel format the backends can hand us, funnelled into BGRA8.
//!
//! The rest of Remu — the encoder, the preview, the PNG snapshot — only
//! ever sees BGRA8 with a known stride. Doing the conversion in one pure
//! module is what makes it testable at all: the capture path itself needs a
//! real screen, but a synthetic buffer through these functions proves the
//! arithmetic that would otherwise only show up as a sheared image on a user's
//! monitor.

use crate::{CaptureError, Frame};

/// Byte order of a source buffer, named the way the backends name it.
///
/// The names follow the GStreamer/PipeWire convention: the letters are the
/// bytes in memory order, so `Rgbx` is `R,G,B,pad` at ascending addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PixelLayout {
    /// `B,G,R,A` — already the output layout.
    Bgra,
    /// `B,G,R,pad` — the pad byte is not alpha and must not be forwarded.
    Bgrx,
    /// `R,G,B,pad`.
    Rgbx,
    /// `pad,B,G,R`.
    Xbgr,
    /// `R,G,B`, three bytes per pixel.
    Rgb,
    /// `B,G,R`, three bytes per pixel.
    Bgr,
}

impl PixelLayout {
    pub(crate) const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb | Self::Bgr => 3,
            Self::Bgra | Self::Bgrx | Self::Rgbx | Self::Xbgr => 4,
        }
    }
}

/// Converts one captured buffer into a tightly packed BGRA8 [`Frame`].
///
/// The source stride is *inferred* from the buffer length rather than assumed
/// to be `width * bytes_per_pixel`: PipeWire hands over a buffer sized by the
/// negotiated `maxsize`, which is routinely padded out to a hardware row
/// alignment. Treating that padding as pixels is what shears an image
/// diagonally, so a buffer too small for the claimed geometry is rejected
/// instead of read past its end.
pub(crate) fn to_bgra8(
    data: Vec<u8>,
    width: u32,
    height: u32,
    layout: PixelLayout,
) -> Result<Frame, CaptureError> {
    if width == 0 || height == 0 {
        return Err(CaptureError::Backend(format!(
            "capture backend produced a {width}x{height} frame"
        )));
    }
    let (w, h) = (width as usize, height as usize);
    let bpp = layout.bytes_per_pixel();
    let src_row = w.checked_mul(bpp).ok_or_else(|| too_large(width, height))?;
    let dst_row = w.checked_mul(4).ok_or_else(|| too_large(width, height))?;
    let dst_len = dst_row
        .checked_mul(h)
        .ok_or_else(|| too_large(width, height))?;

    let src_stride = data.len() / h;
    if src_stride < src_row {
        return Err(CaptureError::Backend(format!(
            "capture backend produced {} bytes for a {width}x{height} {layout:?} frame, \
             which needs at least {src_row} bytes per row",
            data.len()
        )));
    }

    // The common case on macOS and Windows: already BGRA8 with no padding, so
    // the buffer is moved rather than copied. At 4K60 this is ~2 GB/s of memcpy
    // that simply does not happen.
    if layout == PixelLayout::Bgra && src_stride == dst_row && data.len() == dst_len {
        return Ok(Frame {
            width,
            height,
            stride: dst_row,
            data,
        });
    }

    let mut out = vec![0u8; dst_len];
    for row in 0..h {
        // Bounded by construction: src_stride is floor(len / h), so the last
        // row ends at or before data.len().
        let src = &data[row * src_stride..row * src_stride + src_row];
        let dst = &mut out[row * dst_row..(row + 1) * dst_row];
        convert_row(src, dst, layout);
    }
    Ok(Frame {
        width,
        height,
        stride: dst_row,
        data: out,
    })
}

fn convert_row(src: &[u8], dst: &mut [u8], layout: PixelLayout) {
    // `as_chunks` rather than `chunks_exact`: the pixel width is known at
    // compile time, so each element is a fixed-size array and the indexing
    // below carries no bounds check. This runs once per row of every captured
    // frame, so it is worth the extra few lines.
    fn map<const N: usize>(src: &[u8], dst: &mut [u8], f: impl Fn(&[u8; N]) -> [u8; 4]) {
        let (src, _) = src.as_chunks::<N>();
        let (dst, _) = dst.as_chunks_mut::<4>();
        for (s, d) in src.iter().zip(dst.iter_mut()) {
            *d = f(s);
        }
    }

    match layout {
        PixelLayout::Bgra => dst.copy_from_slice(src),
        PixelLayout::Bgrx => map::<4>(src, dst, |s| [s[0], s[1], s[2], 0xFF]),
        PixelLayout::Rgbx => map::<4>(src, dst, |s| [s[2], s[1], s[0], 0xFF]),
        PixelLayout::Xbgr => map::<4>(src, dst, |s| [s[1], s[2], s[3], 0xFF]),
        PixelLayout::Rgb => map::<3>(src, dst, |s| [s[2], s[1], s[0], 0xFF]),
        PixelLayout::Bgr => map::<3>(src, dst, |s| [s[0], s[1], s[2], 0xFF]),
    }
}

/// Converts a bi-planar NV12 (`Y` plane + interleaved `Cb,Cr`) frame to BGRA8.
///
/// Only reachable if a backend starts handing us YUV although BGRA was
/// requested, so it is written for correctness rather than speed. The matrix
/// is BT.601 limited range, which is what `420YpCbCr8BiPlanarVideoRange`
/// carries; a BT.709 source converted here comes out slightly desaturated
/// rather than wrong, and the capture path never requests YUV in the first
/// place.
pub(crate) fn nv12_to_bgra8(
    luma: &[u8],
    luma_stride: usize,
    chroma: &[u8],
    chroma_stride: usize,
    width: u32,
    height: u32,
) -> Result<Frame, CaptureError> {
    if width == 0 || height == 0 {
        return Err(CaptureError::Backend(format!(
            "capture backend produced a {width}x{height} NV12 frame"
        )));
    }
    let (w, h) = (width as usize, height as usize);
    let chroma_w = w.div_ceil(2);
    let chroma_h = h.div_ceil(2);

    if luma_stride < w || chroma_stride < chroma_w * 2 {
        return Err(CaptureError::Backend(format!(
            "NV12 strides {luma_stride}/{chroma_stride} are too narrow for a {width}x{height} frame"
        )));
    }
    if luma.len() < (h - 1) * luma_stride + w
        || chroma.len() < (chroma_h - 1) * chroma_stride + chroma_w * 2
    {
        return Err(CaptureError::Backend(format!(
            "NV12 planes are {} and {} bytes, too small for a {width}x{height} frame",
            luma.len(),
            chroma.len()
        )));
    }
    let dst_row = w.checked_mul(4).ok_or_else(|| too_large(width, height))?;
    let dst_len = dst_row
        .checked_mul(h)
        .ok_or_else(|| too_large(width, height))?;

    let mut out = vec![0u8; dst_len];
    for y in 0..h {
        let chroma_row = (y / 2) * chroma_stride;
        for x in 0..w {
            let luma_sample = i32::from(luma[y * luma_stride + x]) - 16;
            let cb = i32::from(chroma[chroma_row + (x / 2) * 2]) - 128;
            let cr = i32::from(chroma[chroma_row + (x / 2) * 2 + 1]) - 128;
            let r = (298 * luma_sample + 409 * cr + 128) >> 8;
            let g = (298 * luma_sample - 100 * cb - 208 * cr + 128) >> 8;
            let b = (298 * luma_sample + 516 * cb + 128) >> 8;
            let px = y * dst_row + x * 4;
            out[px] = clamp_u8(b);
            out[px + 1] = clamp_u8(g);
            out[px + 2] = clamp_u8(r);
            out[px + 3] = 0xFF;
        }
    }
    Ok(Frame {
        width,
        height,
        stride: dst_row,
        data: out,
    })
}

fn clamp_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

fn too_large(width: u32, height: u32) -> CaptureError {
    CaptureError::Backend(format!(
        "a {width}x{height} frame does not fit in this process's address space"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `width * height` BGRA pixels, each a distinct value, tightly packed.
    fn bgra_ramp(width: u32, height: u32) -> Vec<u8> {
        (0..width * height)
            .flat_map(|i| {
                let i = i as u8;
                [i, i.wrapping_add(1), i.wrapping_add(2), 0x80]
            })
            .collect()
    }

    #[test]
    fn passes_tightly_packed_bgra_through_untouched() {
        let src = bgra_ramp(4, 3);
        let frame = to_bgra8(src.clone(), 4, 3, PixelLayout::Bgra).unwrap();
        assert_eq!(frame.stride, 16);
        assert_eq!(frame.data, src);
        // Alpha from the source survives: a BGRA backend already set it.
        assert_eq!(frame.data[3], 0x80);
    }

    #[test]
    fn drops_row_padding_when_the_stride_exceeds_the_width() {
        // 2x2 image in a buffer with 8 bytes of padding per row.
        let stride = 2 * 4 + 8;
        let mut src = vec![0xEE; stride * 2];
        src[0..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        src[stride..stride + 8].copy_from_slice(&[9, 10, 11, 12, 13, 14, 15, 16]);

        let frame = to_bgra8(src, 2, 2, PixelLayout::Bgra).unwrap();

        assert_eq!(frame.stride, 8, "output is tightly packed");
        assert_eq!(
            frame.data,
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
        assert!(
            !frame.data.contains(&0xEE),
            "padding must not reach the wire"
        );
    }

    #[test]
    fn rejects_a_buffer_too_small_for_its_geometry() {
        // One byte short of 2 rows of 2 pixels.
        let err = to_bgra8(vec![0; 15], 2, 2, PixelLayout::Bgra).unwrap_err();
        match err {
            CaptureError::Backend(msg) => {
                assert!(msg.contains("15 bytes"), "{msg}");
                assert!(msg.contains("2x2"), "{msg}");
            }
            other => panic!("expected Backend, got {other:?}"),
        }
    }

    #[test]
    fn rejects_a_zero_sized_frame() {
        assert!(to_bgra8(vec![], 0, 4, PixelLayout::Bgra).is_err());
        assert!(to_bgra8(vec![], 4, 0, PixelLayout::Bgra).is_err());
    }

    #[test]
    fn reorders_every_four_byte_layout_into_bgra() {
        // One pixel: blue=10, green=20, red=30.
        let cases = [
            (PixelLayout::Bgrx, vec![10, 20, 30, 7]),
            (PixelLayout::Rgbx, vec![30, 20, 10, 7]),
            (PixelLayout::Xbgr, vec![7, 10, 20, 30]),
        ];
        for (layout, src) in cases {
            let frame = to_bgra8(src, 1, 1, layout).unwrap();
            assert_eq!(frame.data, vec![10, 20, 30, 0xFF], "{layout:?}");
        }
    }

    #[test]
    fn forces_opaque_alpha_when_the_source_has_none() {
        // The pad byte is 7, not 7-alpha: forwarding it would make the whole
        // screen near-transparent in the preview.
        let frame = to_bgra8(vec![1, 2, 3, 7], 1, 1, PixelLayout::Bgrx).unwrap();
        assert_eq!(frame.data[3], 0xFF);
    }

    #[test]
    fn expands_three_byte_layouts_to_four() {
        let rgb = to_bgra8(vec![30, 20, 10, 31, 21, 11], 2, 1, PixelLayout::Rgb).unwrap();
        assert_eq!(rgb.data, vec![10, 20, 30, 0xFF, 11, 21, 31, 0xFF]);
        assert_eq!(rgb.stride, 8);

        let bgr = to_bgra8(vec![10, 20, 30, 11, 21, 31], 2, 1, PixelLayout::Bgr).unwrap();
        assert_eq!(bgr.data, vec![10, 20, 30, 0xFF, 11, 21, 31, 0xFF]);
    }

    #[test]
    fn handles_a_padded_three_byte_source() {
        // 2x2 RGB rows padded to 8 bytes (needs 6).
        let mut src = vec![0xEE; 16];
        src[0..6].copy_from_slice(&[1, 2, 3, 4, 5, 6]);
        src[8..14].copy_from_slice(&[7, 8, 9, 10, 11, 12]);
        let frame = to_bgra8(src, 2, 2, PixelLayout::Rgb).unwrap();
        assert_eq!(
            frame.data,
            vec![3, 2, 1, 0xFF, 6, 5, 4, 0xFF, 9, 8, 7, 0xFF, 12, 11, 10, 0xFF]
        );
    }

    #[test]
    fn a_bgra_source_with_padding_is_copied_not_moved() {
        // Guards the fast path's condition: it must not fire when the buffer
        // is longer than width*height*4, or the padding would be kept.
        let src = vec![9u8; 4 * 4 + 16];
        let frame = to_bgra8(src, 4, 1, PixelLayout::Bgra).unwrap();
        assert_eq!(frame.data.len(), 16);
    }

    #[test]
    fn nv12_black_and_white_hit_the_exact_endpoints() {
        // Video-range luma: 16 is black, 235 is white, neutral chroma.
        let black = nv12_to_bgra8(&[16], 1, &[128, 128], 2, 1, 1).unwrap();
        assert_eq!(black.data, vec![0, 0, 0, 0xFF]);

        let white = nv12_to_bgra8(&[235], 1, &[128, 128], 2, 1, 1).unwrap();
        assert_eq!(white.data, vec![255, 255, 255, 0xFF]);
    }

    #[test]
    fn nv12_chroma_drives_the_expected_channel() {
        // Cr high, Cb neutral => red dominates; Cb high => blue dominates.
        let red = nv12_to_bgra8(&[81], 1, &[90, 240], 2, 1, 1).unwrap();
        assert!(
            red.data[2] > red.data[0] && red.data[2] > red.data[1],
            "{red:?}"
        );
        assert!(red.data[2] > 200, "{:?}", red.data);

        let blue = nv12_to_bgra8(&[41], 1, &[240, 110], 2, 1, 1).unwrap();
        assert!(blue.data[0] > blue.data[1] && blue.data[0] > blue.data[2]);
    }

    #[test]
    fn nv12_shares_one_chroma_sample_across_a_two_by_two_block() {
        // 2x2 luma ramp, a single chroma pair: all four pixels differ in
        // brightness only, which is exactly 4:2:0 subsampling.
        let frame = nv12_to_bgra8(&[16, 100, 150, 235], 2, &[128, 128], 2, 2, 2).unwrap();
        assert_eq!(frame.stride, 8);
        assert_eq!(&frame.data[0..4], &[0, 0, 0, 0xFF]);
        assert_eq!(&frame.data[12..16], &[255, 255, 255, 0xFF]);
        for px in frame.data.as_chunks::<4>().0 {
            assert_eq!(px[0], px[1], "neutral chroma means grey");
            assert_eq!(px[1], px[2], "neutral chroma means grey");
        }
    }

    #[test]
    fn nv12_honours_plane_padding() {
        // Both planes padded; the pad bytes would produce garbage if read.
        let luma = vec![16, 235, 0xEE, 0xEE, 235, 16, 0xEE, 0xEE];
        let chroma = vec![128, 128, 0xEE, 0xEE];
        let frame = nv12_to_bgra8(&luma, 4, &chroma, 4, 2, 2).unwrap();
        assert_eq!(&frame.data[0..4], &[0, 0, 0, 0xFF]);
        assert_eq!(&frame.data[4..8], &[255, 255, 255, 0xFF]);
        assert_eq!(&frame.data[8..12], &[255, 255, 255, 0xFF]);
        assert_eq!(&frame.data[12..16], &[0, 0, 0, 0xFF]);
    }

    #[test]
    fn nv12_rejects_planes_that_are_too_small() {
        // Claims 4x4 but carries one row of luma.
        let err = nv12_to_bgra8(&[16; 4], 4, &[128; 4], 4, 4, 4).unwrap_err();
        assert!(matches!(err, CaptureError::Backend(_)));
        // Stride narrower than the image.
        assert!(nv12_to_bgra8(&[16; 16], 2, &[128; 8], 4, 4, 4).is_err());
    }

    #[test]
    fn nv12_clamps_out_of_range_samples_instead_of_wrapping() {
        // Full-range 0 and 255 land outside video range; the result must
        // saturate, not wrap around to the opposite colour.
        let dark = nv12_to_bgra8(&[0], 1, &[128, 128], 2, 1, 1).unwrap();
        assert_eq!(dark.data, vec![0, 0, 0, 0xFF]);
        // Y=255, Cb=Cr=255 overshoots blue and red past 255 and leaves green
        // mid-range; saturating is right, wrapping would invert the pixel.
        let bright = nv12_to_bgra8(&[255], 1, &[255, 255], 2, 1, 1).unwrap();
        assert_eq!(bright.data[0], 255, "blue saturates");
        assert_eq!(bright.data[2], 255, "red saturates");
        assert_eq!(bright.data[1], 125, "green stays where the matrix puts it");
    }
}
