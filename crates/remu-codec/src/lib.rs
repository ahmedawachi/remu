//! H.264 encode, decode and colour conversion for Remu.
//!
//! The host captures BGRA frames, this crate turns them into Annex-B H.264 for
//! the wire, and the viewer turns them back into RGBA for the screen. Both
//! directions go through [`color`], which is pure and carries the bulk of the
//! test suite; the codec itself is a thin, allocation-conscious wrapper around
//! OpenH264.
//!
//! The Electron original shipped whatever the browser's WebRTC stack chose and
//! had no way to inspect or steer it. Here the encoder is explicit — screen
//! content tuning, an enforced bitrate target, keyframes on demand — because a
//! remote desktop's feel is decided by exactly those choices.
//!
//! ```no_run
//! # fn main() -> Result<(), remu_codec::CodecError> {
//! let mut encoder = remu_codec::encoder(remu_codec::EncoderConfig {
//!     width: 1920,
//!     height: 1080,
//!     bitrate_kbps: 4_000,
//!     max_fps: 30,
//! })?;
//! let frame = vec![0u8; 1920 * 1080 * 4];
//! if let Some(packet) = encoder.encode(&frame, 1920, 1080, 1920 * 4, 0, false)? {
//!     assert!(packet.keyframe); // the first frame always is
//! }
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod color;
mod h264;

pub use color::{bgra_to_i420, i420_to_rgba, I420Buffer};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
    #[error("frame dimensions must be non-zero (got {width}x{height})")]
    ZeroDimensions { width: u32, height: u32 },
    #[error("stride {stride} is smaller than the minimum {minimum} for this row")]
    StrideTooSmall { stride: usize, minimum: usize },
    #[error("buffer holds {actual} bytes, {expected} needed for this frame")]
    BufferTooSmall { expected: usize, actual: usize },
    #[error("{width}x{height} exceeds the 3840x2160 H.264 level 5.2 limit")]
    DimensionsTooLarge { width: u32, height: u32 },
    #[error("H.264 4:2:0 needs even frame dimensions (got {width}x{height})")]
    OddDimensions { width: u32, height: u32 },
    #[error("bitrate must be greater than zero (got {0} kbps)")]
    InvalidBitrate(u32),
    #[error("could not create the H.264 encoder: {0}")]
    EncoderInit(String),
    #[error("could not create the H.264 decoder: {0}")]
    DecoderInit(String),
    #[error("H.264 encode failed: {0}")]
    Encode(String),
    #[error("H.264 decode failed: {0}")]
    Decode(String),
}

/// One compressed picture in Annex-B form: NAL units with start codes, ready to
/// go straight onto a data channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFrame {
    pub data: Vec<u8>,
    /// True for an IDR or I frame — the only frames a joining or recovering
    /// viewer can start decoding from.
    pub keyframe: bool,
    pub timestamp_us: u64,
}

/// One decoded picture, RGBA8 and tightly packed, which is what both the egui
/// texture upload and any screenshot path want.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub bitrate_kbps: u32,
    pub max_fps: u32,
}

pub trait VideoEncoder: Send {
    /// Encodes one BGRA frame.
    ///
    /// `stride` is the byte distance between row starts and may exceed
    /// `width * 4`. Dimensions that differ from the current [`config`] cause
    /// the encoder to reconfigure itself and emit a keyframe, so a mid-session
    /// monitor switch needs no special handling from the caller.
    ///
    /// `Ok(None)` means the encoder produced no packet for this frame, which is
    /// normal and means the viewer should keep showing the previous picture.
    ///
    /// [`config`]: VideoEncoder::config
    fn encode(
        &mut self,
        bgra: &[u8],
        width: u32,
        height: u32,
        stride: usize,
        timestamp_us: u64,
        force_keyframe: bool,
    ) -> Result<Option<EncodedFrame>, CodecError>;

    /// Retargets the bitrate, e.g. after congestion feedback. The next frame
    /// will be a keyframe.
    fn set_bitrate(&mut self, kbps: u32) -> Result<(), CodecError>;

    fn config(&self) -> &EncoderConfig;
}

pub trait VideoDecoder: Send {
    /// Decodes an Annex-B buffer.
    ///
    /// `Ok(None)` means "need more data" — the usual answer while parameter
    /// sets have arrived but the first keyframe has not.
    fn decode(&mut self, annexb: &[u8]) -> Result<Option<DecodedFrame>, CodecError>;
}

/// Creates the host-side encoder.
pub fn encoder(cfg: EncoderConfig) -> Result<Box<dyn VideoEncoder>, CodecError> {
    Ok(Box::new(h264::OpenH264Encoder::new(cfg)?))
}

/// Creates the viewer-side decoder.
pub fn decoder() -> Result<Box<dyn VideoDecoder>, CodecError> {
    Ok(Box::new(h264::OpenH264Decoder::new()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(width: u32, height: u32) -> EncoderConfig {
        EncoderConfig {
            width,
            height,
            bitrate_kbps: 4_000,
            max_fps: 30,
        }
    }

    /// A gradient with a hard-edged block in it: gradients compress so well
    /// that a broken encoder can still score a good error, while the block
    /// edges expose a shifted or transposed picture.
    fn scene(width: u32, height: u32, phase: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let inside = x > width / 4 && x < width / 2 && y > height / 4 && y < height / 2;
                let (r, g, b) = if inside {
                    (255u32, 32, 16)
                } else {
                    (
                        (x * 255 / width.max(1) + phase) % 256,
                        (y * 255 / height.max(1)) % 256,
                        64,
                    )
                };
                out.extend_from_slice(&[b as u8, g as u8, r as u8, 255]);
            }
        }
        out
    }

    /// Mean absolute error per colour channel between a BGRA source frame and a
    /// decoded RGBA frame.
    fn mean_abs_error(bgra: &[u8], decoded: &DecodedFrame) -> f64 {
        let mut total = 0u64;
        let mut count = 0u64;
        for (src, out) in bgra
            .as_chunks::<4>()
            .0
            .iter()
            .zip(decoded.rgba.as_chunks::<4>().0)
        {
            for (s, o) in [(src[2], out[0]), (src[1], out[1]), (src[0], out[2])] {
                total += u64::from(s.abs_diff(o));
                count += 1;
            }
        }
        assert!(count > 0);
        total as f64 / count as f64
    }

    #[test]
    fn encodes_a_frame_and_decodes_a_recognisable_picture_back() {
        let (w, h) = (256u32, 128u32);
        let mut enc = encoder(config(w, h)).unwrap();
        let mut dec = decoder().unwrap();

        let src = scene(w, h, 0);
        let packet = enc
            .encode(&src, w, h, (w * 4) as usize, 0, false)
            .unwrap()
            .expect("first frame must produce a packet");
        assert!(packet.keyframe, "a stream has to open with a keyframe");
        assert!(packet.data.starts_with(&[0, 0, 0, 1]), "Annex-B start code");
        assert_eq!(packet.timestamp_us, 0);

        let frame = dec
            .decode(&packet.data)
            .unwrap()
            .expect("a keyframe is decodable on its own");
        assert_eq!((frame.width, frame.height), (w, h));
        assert_eq!(frame.rgba.len(), (w * h * 4) as usize);

        let mae = mean_abs_error(&src, &frame);
        assert!(
            mae < 12.0,
            "mean absolute error {mae} — picture is not the input"
        );
        assert!(frame.rgba.as_chunks::<4>().0.iter().all(|px| px[3] == 255));
    }

    #[test]
    fn keeps_tracking_the_source_across_a_sequence_of_frames() {
        let (w, h) = (192u32, 96u32);
        let mut enc = encoder(config(w, h)).unwrap();
        let mut dec = decoder().unwrap();

        let mut decoded_frames = 0;
        for i in 0..10u32 {
            let src = scene(w, h, i * 7);
            let ts = u64::from(i) * 33_333;
            let Some(packet) = enc.encode(&src, w, h, (w * 4) as usize, ts, false).unwrap() else {
                continue;
            };
            assert_eq!(packet.keyframe, i == 0, "only frame 0 should be an IDR");
            if let Some(frame) = dec.decode(&packet.data).unwrap() {
                decoded_frames += 1;
                let mae = mean_abs_error(&src, &frame);
                assert!(mae < 12.0, "frame {i} drifted: mean absolute error {mae}");
            }
        }
        assert!(
            decoded_frames >= 9,
            "only {decoded_frames} frames came back"
        );
    }

    #[test]
    fn produces_a_keyframe_on_demand() {
        let (w, h) = (128u32, 64u32);
        let mut enc = encoder(config(w, h)).unwrap();
        let src = scene(w, h, 0);

        let first = enc.encode(&src, w, h, (w * 4) as usize, 0, false).unwrap();
        assert!(first.unwrap().keyframe);

        // A static scene: without the request the encoder has no reason at all
        // to spend bits on an IDR here.
        let second = enc
            .encode(&src, w, h, (w * 4) as usize, 33_000, false)
            .unwrap();
        assert!(!second.unwrap().keyframe);

        let forced = enc
            .encode(&src, w, h, (w * 4) as usize, 66_000, true)
            .unwrap()
            .expect("a forced keyframe is never skipped");
        assert!(forced.keyframe, "force_keyframe was ignored");
    }

    #[test]
    fn reconfigures_itself_when_the_frame_size_changes_mid_stream() {
        let mut enc = encoder(config(160, 96)).unwrap();
        let mut dec = decoder().unwrap();

        let first = scene(160, 96, 0);
        enc.encode(&first, 160, 96, 160 * 4, 0, false).unwrap();
        assert_eq!((enc.config().width, enc.config().height), (160, 96));

        // The user dragged the session to a second monitor.
        let bigger = scene(320, 192, 3);
        let packet = enc
            .encode(&bigger, 320, 192, 320 * 4, 33_000, false)
            .unwrap()
            .expect("the first frame at a new size must be emitted");
        assert!(packet.keyframe, "a resolution change needs a fresh IDR");
        assert_eq!((enc.config().width, enc.config().height), (320, 192));

        let frame = dec
            .decode(&packet.data)
            .unwrap()
            .expect("the new-size keyframe is self-contained");
        assert_eq!((frame.width, frame.height), (320, 192));
        assert!(mean_abs_error(&bigger, &frame) < 12.0);
    }

    #[test]
    fn honours_the_declared_stride_when_rows_are_padded() {
        let (w, h) = (96u32, 64u32);
        let tight = scene(w, h, 0);
        let stride = (w * 4) as usize + 64;
        let mut padded = vec![0u8; stride * h as usize];
        for row in 0..h as usize {
            let row_bytes = (w * 4) as usize;
            padded[row * stride..row * stride + row_bytes]
                .copy_from_slice(&tight[row * row_bytes..(row + 1) * row_bytes]);
        }

        let mut enc = encoder(config(w, h)).unwrap();
        let mut dec = decoder().unwrap();
        let packet = enc
            .encode(&padded, w, h, stride, 0, false)
            .unwrap()
            .unwrap();
        let frame = dec.decode(&packet.data).unwrap().unwrap();
        assert!(
            mean_abs_error(&tight, &frame) < 12.0,
            "padding bled into the encoded picture"
        );
    }

    #[test]
    fn changing_the_bitrate_keeps_the_stream_decodable() {
        let (w, h) = (128u32, 64u32);
        let mut enc = encoder(config(w, h)).unwrap();
        let mut dec = decoder().unwrap();
        let src = scene(w, h, 0);

        enc.encode(&src, w, h, (w * 4) as usize, 0, false).unwrap();
        enc.set_bitrate(800).unwrap();
        assert_eq!(enc.config().bitrate_kbps, 800);

        let packet = enc
            .encode(&src, w, h, (w * 4) as usize, 33_000, false)
            .unwrap()
            .expect("frame after a bitrate change");
        assert!(
            packet.keyframe,
            "a rebuilt encoder must re-open the stream with an IDR"
        );
        let frame = dec.decode(&packet.data).unwrap().unwrap();
        assert_eq!((frame.width, frame.height), (w, h));
    }

    #[test]
    fn rejects_a_zero_bitrate_at_construction_and_at_runtime() {
        let mut cfg = config(64, 64);
        cfg.bitrate_kbps = 0;
        assert_eq!(
            encoder(cfg).err(),
            Some(CodecError::InvalidBitrate(0)),
            "a zero-bitrate encoder would emit nothing"
        );

        let mut enc = encoder(config(64, 64)).unwrap();
        assert_eq!(enc.set_bitrate(0), Err(CodecError::InvalidBitrate(0)));
        assert_eq!(
            enc.config().bitrate_kbps,
            4_000,
            "config must not be clobbered"
        );
    }

    #[test]
    fn rejects_frames_the_codec_cannot_represent() {
        assert_eq!(
            encoder(config(0, 1080)).err(),
            Some(CodecError::ZeroDimensions {
                width: 0,
                height: 1080
            })
        );
        assert_eq!(
            encoder(config(7680, 2160)).err(),
            Some(CodecError::DimensionsTooLarge {
                width: 7680,
                height: 2160
            })
        );
        assert_eq!(
            encoder(config(1921, 1080)).err(),
            Some(CodecError::OddDimensions {
                width: 1921,
                height: 1080
            })
        );

        // And the same checks apply per frame, not just at construction.
        let mut enc = encoder(config(64, 64)).unwrap();
        let src = scene(64, 64, 0);
        assert_eq!(
            enc.encode(&src, 65, 64, 65 * 4, 0, false),
            Err(CodecError::OddDimensions {
                width: 65,
                height: 64
            })
        );
        assert_eq!(
            (enc.config().width, enc.config().height),
            (64, 64),
            "a rejected frame must not disturb the live configuration"
        );
    }

    #[test]
    fn rejects_a_frame_buffer_shorter_than_its_declared_size() {
        let mut enc = encoder(config(64, 64)).unwrap();
        let src = scene(64, 64, 0);
        assert_eq!(
            enc.encode(&src[..src.len() - 4], 64, 64, 64 * 4, 0, false),
            Err(CodecError::BufferTooSmall {
                expected: 64 * 64 * 4,
                actual: 64 * 64 * 4 - 4
            })
        );
    }

    #[test]
    fn decoder_asks_for_more_data_until_it_has_a_keyframe() {
        let mut dec = decoder().unwrap();
        assert_eq!(
            dec.decode(&[]).unwrap(),
            None,
            "empty buffer is not an error"
        );

        let (w, h) = (96u32, 64u32);
        let mut enc = encoder(config(w, h)).unwrap();
        let packet = enc
            .encode(&scene(w, h, 0), w, h, (w * 4) as usize, 0, false)
            .unwrap()
            .unwrap();

        // Feed the parameter sets alone: everything before the last start code
        // is SPS/PPS/SEI, which cannot yield a picture on its own.
        let last_nal = (4..packet.data.len())
            .rev()
            .find(|&i| packet.data[i..].starts_with(&[0, 0, 0, 1]))
            .expect("a keyframe carries several NAL units");
        assert_eq!(dec.decode(&packet.data[..last_nal]).unwrap(), None);

        // The slice completes the picture.
        let frame = dec.decode(&packet.data[last_nal..]).unwrap();
        assert!(
            frame.is_some(),
            "the keyframe slice should complete a picture"
        );
    }

    #[test]
    fn decoder_reports_garbage_rather_than_inventing_a_picture() {
        let mut dec = decoder().unwrap();
        // A well-formed start code followed by a nonsense NAL: either the
        // decoder errors or it has nothing to show, but it must never hand back
        // a frame it did not decode.
        let junk = [0, 0, 0, 1, 0x65, 0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55];
        match dec.decode(&junk) {
            Ok(None) => {}
            Ok(Some(frame)) => panic!("decoded {}x{} out of junk", frame.width, frame.height),
            Err(CodecError::Decode(_)) => {}
            Err(other) => panic!("unexpected error {other}"),
        }
    }

    #[test]
    fn encoder_and_decoder_are_send_so_they_can_live_on_worker_threads() {
        fn assert_send<T: Send + ?Sized>() {}
        assert_send::<dyn VideoEncoder>();
        assert_send::<dyn VideoDecoder>();

        // The capture thread owns the encoder, so it must actually move.
        let mut enc = encoder(config(64, 64)).unwrap();
        std::thread::spawn(move || {
            let src = scene(64, 64, 0);
            enc.encode(&src, 64, 64, 64 * 4, 0, false).unwrap()
        })
        .join()
        .unwrap()
        .expect("frame encoded on another thread");
    }
}
