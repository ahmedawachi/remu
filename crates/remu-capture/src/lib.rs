//! Screen capture for the Remu host.
//!
//! One display in, a stream of BGRA8 frames out. Everything platform-specific
//! — ScreenCaptureKit on macOS, Windows.Graphics.Capture on Windows, the
//! PipeWire desktop portal on Linux — is hidden behind [`open`], which hands
//! back a [`ScreenCapturer`] the session loop can poll with a deadline.
//!
//! ```no_run
//! use std::time::Duration;
//! use remu_capture::{open, list_displays, CaptureOptions};
//!
//! let displays = list_displays()?;
//! let primary = displays.iter().find(|d| d.primary).unwrap_or(&displays[0]);
//! let mut capturer = open(&primary.id, CaptureOptions::default())?;
//! // `None` simply means the screen did not change within the deadline.
//! if let Some(frame) = capturer.next_frame(Duration::from_millis(100))? {
//!     assert_eq!(frame.stride, frame.width as usize * 4);
//! }
//! capturer.stop();
//! # Ok::<(), remu_capture::CaptureError>(())
//! ```
//!
//! The permission model is the reason [`CaptureError::PermissionDenied`] is
//! its own variant: on macOS the user has to grant Screen Recording in System
//! Settings and then restart the app, and the UI needs to say so precisely
//! rather than showing a generic failure.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

use std::fmt;
use std::time::Duration;

mod backend;
mod convert;
mod ids;

/// A display that can be captured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayTarget {
    /// Stable, human-readable, and safe to send to the peer: `display:<n>`,
    /// where `n` is the platform's display number. Travels as `source_id` in
    /// [`remu_proto::ControlMessage::SwitchDisplay`].
    ///
    /// Stable across a session on every platform, and across reboots on macOS
    /// (`CGDirectDisplayID`). On Windows it wraps an `HMONITOR`, which the OS
    /// may reassign when monitors are plugged or unplugged — treat a stored id
    /// as a hint and re-list if it no longer resolves.
    pub id: String,
    /// What the OS calls this monitor, for the display picker.
    pub name: String,
    /// Captured width in pixels, which on a scaled display is larger than the
    /// logical desktop width. `0` when the size is not knowable until capture
    /// starts (the Linux portal); it is corrected after the first frame.
    pub width: u32,
    pub height: u32,
    /// Best effort: both macOS and Windows list the main screen first, but
    /// neither exposes an explicit flag through this backend, and Linux has no
    /// enumeration at all. Use it to pick a default, not to make decisions.
    pub primary: bool,
}

/// One captured screen image, BGRA8, top row first.
///
/// `stride` is the distance in bytes between the starts of two rows. This
/// crate always produces tightly packed frames, so it equals `width * 4` —
/// but it is part of the type because the sources are not packed, and code
/// that indexes with `width * 4` instead of `stride` is the classic way to
/// shear a captured image diagonally.
#[derive(Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub data: Vec<u8>,
}

impl fmt::Debug for Frame {
    // A derived Debug would dump several megabytes of pixels into a log line.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Frame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("stride", &self.stride)
            .field("bytes", &self.data.len())
            .finish()
    }
}

/// How to capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureOptions {
    /// Upper bound on delivered frames per second, clamped to 1..=240. It is
    /// enforced by this crate as well as by the OS backend, because
    /// Windows.Graphics.Capture has no frame-rate setting of its own.
    pub max_fps: u32,
    /// Whether the pointer is drawn into the frames. The controller draws its
    /// own cursor, so the host usually wants this off for a remote session and
    /// on for a recording.
    pub show_cursor: bool,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            max_fps: 30,
            show_cursor: true,
        }
    }
}

/// A live capture of one display.
///
/// Dropping it stops the capture; [`stop`](ScreenCapturer::stop) does the same
/// thing explicitly and waits for the OS stream to be torn down.
pub trait ScreenCapturer: Send + fmt::Debug {
    /// The display being captured. Its size may be filled in after the first
    /// frame on backends that cannot report it up front.
    fn target(&self) -> &DisplayTarget;

    /// Waits up to `timeout` for the next frame.
    ///
    /// `Ok(None)` means nothing new arrived in time — an idle screen produces
    /// no frames, and that is not an error. Only the most recent frame is
    /// kept, so a slow caller skips stale screens instead of falling behind.
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, CaptureError>;

    /// Stops the capture and releases the OS stream. Idempotent; after it,
    /// `next_frame` returns an error rather than blocking forever.
    fn stop(&mut self);
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    /// The OS refuses screen access. On macOS this is the Screen Recording
    /// permission; the UI should send the user to System Settings and nowhere
    /// else, which is why this is not folded into `Backend`.
    #[error("screen recording permission has not been granted")]
    PermissionDenied,
    #[error("no capturable display was found")]
    NoDisplays,
    #[error("no display with id {0}")]
    NoSuchDisplay(String),
    #[error("screen capture is unsupported here: {0}")]
    Unsupported(String),
    #[error("screen capture backend error: {0}")]
    Backend(String),
}

/// Lists the displays that can be captured, primary first where the platform
/// says so.
///
/// On Linux this returns a single placeholder: the desktop portal picks the
/// screen in its own dialog when capture starts, so there is nothing to
/// enumerate beforehand.
pub fn list_displays() -> Result<Vec<DisplayTarget>, CaptureError> {
    backend::list_displays()
}

/// Starts capturing `display_id`, as taken from [`list_displays`] or from a
/// peer's `SwitchDisplay`.
///
/// Returns only once the OS stream is actually running, so a permission or
/// setup failure surfaces here rather than as an endless absence of frames.
pub fn open(
    display_id: &str,
    opts: CaptureOptions,
) -> Result<Box<dyn ScreenCapturer>, CaptureError> {
    backend::open(display_id, opts)
}

/// Whether this OS build has a usable capture API at all (macOS 13.1+,
/// Windows 10 2004+, a PipeWire portal on Linux).
pub fn is_supported() -> bool {
    backend::supported()
}

/// Whether screen capture is permitted right now. Always true off macOS,
/// where no such permission exists.
pub fn has_permission() -> bool {
    scap::has_permission()
}

/// Asks the OS to prompt for screen-capture permission.
///
/// Returns whether permission is granted *now*. macOS only hands the
/// permission to a process on its next launch, so a `false` here means "ask
/// the user to restart Remu", not "the user refused".
pub fn request_permission() -> bool {
    scap::request_permission()
}

/// Projects a capture target into the protocol type sent to the controller.
pub fn to_display_info(target: &DisplayTarget) -> remu_proto::DisplayInfo {
    remu_proto::DisplayInfo {
        id: target.id.clone(),
        name: target.name.clone(),
        width: target.width,
        height: target.height,
        primary: target.primary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> DisplayTarget {
        DisplayTarget {
            id: "display:3".to_string(),
            name: "Studio Display".to_string(),
            width: 5120,
            height: 2880,
            primary: true,
        }
    }

    #[test]
    fn a_display_target_crosses_into_the_protocol_unchanged() {
        let info = to_display_info(&target());
        assert_eq!(info.id, "display:3");
        assert_eq!(info.name, "Studio Display");
        assert_eq!((info.width, info.height), (5120, 2880));
        assert!(info.primary);
    }

    #[test]
    fn the_id_that_reaches_the_peer_round_trips_back_to_the_same_display() {
        // The controller echoes DisplayInfo.id in SwitchDisplay, so the two
        // sides must agree byte for byte.
        let target = target();
        let info = to_display_info(&target);
        let echoed = remu_proto::ControlMessage::SwitchDisplay {
            source_id: info.id.clone(),
        };
        let json = serde_json::to_string(&echoed).unwrap();
        let back: remu_proto::ControlMessage = serde_json::from_str(&json).unwrap();
        match back {
            remu_proto::ControlMessage::SwitchDisplay { source_id } => {
                assert_eq!(source_id, target.id);
            }
            other => panic!("expected SwitchDisplay, got {other:?}"),
        }
    }

    #[test]
    fn a_frame_debug_line_reports_its_size_not_its_pixels() {
        let frame = Frame {
            width: 2,
            height: 2,
            stride: 8,
            data: vec![0xAB; 32],
        };
        let rendered = format!("{frame:?}");
        assert!(rendered.contains("bytes: 32"), "{rendered}");
        assert!(!rendered.contains("171"), "pixels must not be printed");
    }

    #[test]
    fn the_default_options_are_a_sane_remote_session() {
        let opts = CaptureOptions::default();
        assert_eq!(opts.max_fps, 30);
        assert!(opts.show_cursor);
    }

    #[test]
    fn permission_denied_is_distinguishable_from_every_other_failure() {
        // The UI routes to System Settings on exactly this variant.
        assert_ne!(
            CaptureError::PermissionDenied,
            CaptureError::Backend("permission".to_string())
        );
        assert_eq!(
            CaptureError::PermissionDenied.to_string(),
            "screen recording permission has not been granted"
        );
        assert!(CaptureError::NoSuchDisplay("display:9".to_string())
            .to_string()
            .contains("display:9"));
    }

    #[test]
    fn opening_an_unknown_display_never_returns_a_capturer() {
        let err = open("display:4294967295", CaptureOptions::default())
            .expect_err("id 4294967295 is not a real display");
        assert!(
            !matches!(err, CaptureError::Backend(_)) || !is_supported(),
            "expected a typed failure, got {err:?}"
        );
    }
}
