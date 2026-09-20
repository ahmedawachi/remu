//! OpenH264-backed implementations of [`VideoEncoder`] and [`VideoDecoder`].

use openh264::decoder::Decoder;
use openh264::encoder::{
    BitRate, Encoder, EncoderConfig as OpenH264Config, FrameRate, FrameType, RateControlMode,
    UsageType, VuiConfig,
};
use openh264::formats::YUVSource;
use openh264::{OpenH264API, Timestamp};

use crate::color::{self, I420Buffer};
use crate::{CodecError, DecodedFrame, EncodedFrame, EncoderConfig, VideoDecoder, VideoEncoder};

/// OpenH264 refuses to initialise past level 5.2, and the failure surfaces as
/// an opaque native error code deep inside `encode`. Checking here turns it
/// into a message the session layer can actually show a user.
const MAX_LONG_EDGE: u32 = 3840;
const MAX_SHORT_EDGE: u32 = 2160;

pub(crate) struct OpenH264Encoder {
    cfg: EncoderConfig,
    inner: Encoder,
    /// Kept across frames so the BGRA -> I420 conversion never allocates on the
    /// capture path.
    yuv: I420Buffer,
}

impl OpenH264Encoder {
    pub(crate) fn new(cfg: EncoderConfig) -> Result<Self, CodecError> {
        check_dimensions(cfg.width, cfg.height)?;
        if cfg.bitrate_kbps == 0 {
            return Err(CodecError::InvalidBitrate(0));
        }
        Ok(Self {
            inner: build(&cfg)?,
            yuv: I420Buffer::with_dimensions(cfg.width, cfg.height),
            cfg,
        })
    }
}

fn build(cfg: &EncoderConfig) -> Result<Encoder, CodecError> {
    // Screen content: OpenH264 then favours sharp edges and static regions over
    // the temporal smoothing it would apply to camera video, which is the
    // difference between readable and mushy text on a remote desktop.
    //
    // The VUI says BT.601 limited range because that is what `color` produces;
    // without it a strict decoder is entitled to assume BT.709 and shift every
    // colour on screen.
    let native = OpenH264Config::new()
        .usage_type(UsageType::ScreenContentRealTime)
        .bitrate(BitRate::from_bps(cfg.bitrate_kbps.saturating_mul(1000)))
        .max_frame_rate(FrameRate::from_hz(cfg.max_fps.max(1) as f32))
        .vui(VuiConfig::bt601())
        .rate_control_mode(RateControlMode::Bitrate)
        // OpenH264 warns, and then ignores the bitrate target, unless frame
        // skipping is allowed. Overshooting the link's budget on a remote
        // desktop queues frames and turns into growing input lag, which is
        // worse to work through than an occasional dropped picture, so the
        // target is kept real. `encode` reports a skip as `Ok(None)`.
        .skip_frames(true)
        // Both are auto-disabled for screen content anyway; asking for them
        // only produces a warning on every encoder we build.
        .adaptive_quantization(false)
        .background_detection(false);

    Encoder::with_api_config(OpenH264API::from_source(), native)
        .map_err(|e| CodecError::EncoderInit(e.to_string()))
}

fn check_dimensions(width: u32, height: u32) -> Result<(), CodecError> {
    if width == 0 || height == 0 {
        return Err(CodecError::ZeroDimensions { width, height });
    }
    let long_edge = width.max(height);
    let short_edge = width.min(height);
    if long_edge > MAX_LONG_EDGE || short_edge > MAX_SHORT_EDGE {
        return Err(CodecError::DimensionsTooLarge { width, height });
    }
    if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        // Measured against OpenH264 0.9.8: 101x63 comes back out of the
        // decoder as 100x62 and 5x7 fails with a bare native error code. It
        // either drops the odd edge without telling anyone or dies, so the odd
        // size is refused here where the caller can still do something about
        // it. The capture layer crops to even before handing frames over.
        return Err(CodecError::OddDimensions { width, height });
    }
    Ok(())
}

/// Borrowed view of our own [`I420Buffer`] in the shape OpenH264 wants.
///
/// `openh264::formats::YUVSlices` would do the same job but asserts
/// `u.len() == (height / 2) * stride`, which rounds *down* and therefore panics
/// on the odd-sized buffers `color` deliberately rounds up.
struct I420View<'a>(&'a I420Buffer);

impl YUVSource for I420View<'_> {
    fn dimensions(&self) -> (usize, usize) {
        (self.0.width() as usize, self.0.height() as usize)
    }

    fn strides(&self) -> (usize, usize, usize) {
        self.0.strides()
    }

    fn y(&self) -> &[u8] {
        self.0.y()
    }

    fn u(&self) -> &[u8] {
        self.0.u()
    }

    fn v(&self) -> &[u8] {
        self.0.v()
    }
}

impl VideoEncoder for OpenH264Encoder {
    fn encode(
        &mut self,
        bgra: &[u8],
        width: u32,
        height: u32,
        stride: usize,
        timestamp_us: u64,
        force_keyframe: bool,
    ) -> Result<Option<EncodedFrame>, CodecError> {
        check_dimensions(width, height)?;

        // The user dragged the window to another monitor, or the host changed
        // resolution. OpenH264 re-initialises itself when the source dimensions
        // change and emits an IDR for the new size, so all we owe it is an
        // updated config record; `bgra_to_i420` resizes the plane buffer to
        // match, and both are no-ops when the size is unchanged.
        self.cfg.width = width;
        self.cfg.height = height;

        color::bgra_to_i420(bgra, width, height, stride, &mut self.yuv)?;

        if force_keyframe {
            self.inner.force_intra_frame();
        }

        // OpenH264 timestamps are milliseconds; the protocol carries
        // microseconds because input events need that resolution.
        let timestamp = Timestamp::from_millis(timestamp_us / 1000);
        let bitstream = self
            .inner
            .encode_at(&I420View(&self.yuv), timestamp)
            .map_err(|e| CodecError::Encode(e.to_string()))?;

        let frame_type = bitstream.frame_type();
        let data = bitstream.to_vec();

        match frame_type {
            // Rate control dropped this picture; the viewer keeps the previous
            // one, which is exactly what "no packet" means downstream.
            FrameType::Skip => return Ok(None),
            FrameType::Invalid if data.is_empty() => return Ok(None),
            FrameType::Invalid => {
                return Err(CodecError::Encode(
                    "encoder reported an invalid frame type".into(),
                ))
            }
            _ => {}
        }
        if data.is_empty() {
            return Ok(None);
        }

        Ok(Some(EncodedFrame {
            data,
            keyframe: matches!(frame_type, FrameType::IDR | FrameType::I),
            timestamp_us,
        }))
    }

    fn set_bitrate(&mut self, kbps: u32) -> Result<(), CodecError> {
        if kbps == 0 {
            return Err(CodecError::InvalidBitrate(0));
        }
        if kbps == self.cfg.bitrate_kbps {
            return Ok(());
        }
        self.cfg.bitrate_kbps = kbps;
        // OpenH264 can retune a live encoder only through its raw C API, which
        // is `unsafe` and not exposed safely by the wrapper. Rebuilding is the
        // safe route; the cost is that the next frame is an IDR, which a
        // bitrate change usually wants anyway (the old reference frames were
        // encoded for a different budget).
        self.inner = build(&self.cfg)?;
        Ok(())
    }

    fn config(&self) -> &EncoderConfig {
        &self.cfg
    }
}

pub(crate) struct OpenH264Decoder {
    inner: Decoder,
}

impl OpenH264Decoder {
    pub(crate) fn new() -> Result<Self, CodecError> {
        Decoder::new()
            .map(|inner| Self { inner })
            .map_err(|e| CodecError::DecoderInit(e.to_string()))
    }
}

impl VideoDecoder for OpenH264Decoder {
    fn decode(&mut self, annexb: &[u8]) -> Result<Option<DecodedFrame>, CodecError> {
        if annexb.is_empty() {
            return Ok(None);
        }

        let decoded = self
            .inner
            .decode(annexb)
            .map_err(|e| CodecError::Decode(e.to_string()))?;
        let Some(yuv) = decoded else {
            return Ok(None);
        };

        let (width, height) = yuv.dimensions();
        // A zero-sized picture would mean OpenH264 handed back a buffer it also
        // described as empty; treat it as "need more data" rather than
        // constructing a frame nothing can draw.
        if width == 0 || height == 0 {
            return Ok(None);
        }

        let mut rgba = Vec::new();
        color::i420_to_rgba(
            yuv.y(),
            yuv.u(),
            yuv.v(),
            yuv.strides(),
            width as u32,
            height as u32,
            &mut rgba,
        )?;

        Ok(Some(DecodedFrame {
            width: width as u32,
            height: height as u32,
            rgba,
        }))
    }
}
