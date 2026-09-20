//! The `scap` backend: one capture thread per open display.
//!
//! `scap`'s own `Capturer::get_next_frame` blocks forever, which a session
//! loop cannot use — it needs a deadline so it can service control messages
//! and shut down promptly. So this module drives `scap`'s `Engine` directly on
//! a worker thread and hands frames over through a single-slot mailbox.
//!
//! Two constraints shaped the design:
//!
//! - On Windows `scap::Target` holds a raw `HMONITOR`, which is not `Send`.
//!   The worker therefore receives the numeric display id and re-resolves the
//!   target on its own thread; nothing platform-specific crosses the boundary.
//! - `scap` 0.0.8 panics rather than returning errors on a surprising number
//!   of paths (a display that disappeared, a stream that refuses to start).
//!   The worker is wrapped in `catch_unwind` so those arrive as a typed
//!   `Backend` error instead of taking the process down.

use std::any::Any;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use scap::capturer::{Options, Resolution};
use scap::frame::{Frame as ScapFrame, FrameType};

use crate::convert::{nv12_to_bgra8, to_bgra8, PixelLayout};
use crate::ids::format_display_id;
#[cfg(not(target_os = "linux"))]
use crate::ids::parse_display_id;
use crate::{CaptureError, CaptureOptions, DisplayTarget, Frame, ScreenCapturer};

/// How long the worker sleeps between checks of the stop flag when no frames
/// are arriving. Bounds how long `stop()` blocks, nothing else — a frame that
/// does arrive wakes the wait immediately.
const POLL: Duration = Duration::from_millis(100);

/// How long `open` waits for the backend to confirm the stream started.
///
/// Generous because on Linux this covers the xdg-desktop-portal dialog, where
/// the user has to pick a screen by hand.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) const MIN_FPS: u32 = 1;
pub(crate) const MAX_FPS: u32 = 240;

/// Id of the synthetic Linux target; see [`resolve_display`].
#[cfg(target_os = "linux")]
pub(crate) const PORTAL_DISPLAY_ID: &str = "portal";

const UNSUPPORTED: &str =
    "this system has no supported screen-capture API (macOS 12.3+, Windows 10 1803+, \
     or a PipeWire desktop portal is required)";

pub(crate) fn list_displays() -> Result<Vec<DisplayTarget>, CaptureError> {
    if !scap::is_supported() {
        return Err(CaptureError::Unsupported(UNSUPPORTED.to_string()));
    }
    if !scap::has_permission() {
        return Err(CaptureError::PermissionDenied);
    }
    platform_displays()
}

#[cfg(target_os = "linux")]
fn platform_displays() -> Result<Vec<DisplayTarget>, CaptureError> {
    // PipeWire has no enumeration API: the user picks the screen in the
    // portal dialog when capture starts, so the only honest answer before
    // then is a single placeholder whose size is filled in from the first
    // frame. See `ScapCapturer::next_frame`.
    Ok(vec![DisplayTarget {
        id: PORTAL_DISPLAY_ID.to_string(),
        name: "Screen (chosen when sharing starts)".to_string(),
        width: 0,
        height: 0,
        primary: true,
    }])
}

#[cfg(not(target_os = "linux"))]
fn platform_displays() -> Result<Vec<DisplayTarget>, CaptureError> {
    let mut displays = Vec::new();
    for target in scap::get_all_targets() {
        let scap::Target::Display(display) = target else {
            continue; // windows are a separate feature, not a display
        };
        // Copy what we need before handing the display to scap: `Target` takes
        // it by value and scap does not expose the `Display` type by name, so
        // it cannot be borrowed back out afterwards.
        let (raw_id, title) = (display.id, display.title.clone());
        let (width, height) = display_size(scap::Target::Display(display), raw_id);
        displays.push(DisplayTarget {
            id: format_display_id(raw_id),
            name: title,
            width,
            height,
            // Neither ScreenCaptureKit nor EnumDisplayMonitors exposes a
            // "this is the main screen" flag through scap, but both list the
            // primary display first. Documented as best-effort on the field.
            primary: displays.is_empty(),
        });
    }
    if displays.is_empty() {
        return Err(CaptureError::NoDisplays);
    }
    Ok(displays)
}

/// Asks scap what a stream on this display would produce.
///
/// This is the captured pixel size, not the logical desktop size — on a
/// Retina or scaled monitor the two differ, and the frames will match this.
#[cfg(not(target_os = "linux"))]
fn display_size(target: scap::Target, raw_id: u32) -> (u32, u32) {
    let options = Options {
        target: Some(target),
        output_type: FrameType::BGRAFrame,
        output_resolution: Resolution::Captured,
        ..Default::default()
    };
    // scap unwraps the display mode here; a monitor unplugged mid-enumeration
    // panics rather than erroring. Unknown size is recoverable (the first
    // frame corrects it), a dead UI thread is not.
    match panic::catch_unwind(AssertUnwindSafe(|| {
        scap::capturer::get_output_frame_size(&options)
    })) {
        Ok([width, height]) => (width, height),
        Err(payload) => {
            tracing::warn!(
                // Not `display`: that field name collides with
                // `tracing::field::display` inside the macro.
                display_id = raw_id,
                reason = panic_text(&payload),
                "could not measure display, reporting unknown size"
            );
            (0, 0)
        }
    }
}

/// Turns a wire id into the target to capture plus the platform number the
/// worker will re-resolve.
///
/// `None` as the second element means "let the backend choose", which is the
/// only mode PipeWire offers.
pub(crate) fn resolve_display(
    display_id: &str,
) -> Result<(DisplayTarget, Option<u32>), CaptureError> {
    #[cfg(target_os = "linux")]
    {
        if display_id != PORTAL_DISPLAY_ID {
            return Err(CaptureError::NoSuchDisplay(display_id.to_string()));
        }
        let target = platform_displays()?
            .into_iter()
            .next()
            .ok_or(CaptureError::NoDisplays)?;
        Ok((target, None))
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Parse before touching the OS: a malformed id is answerable without
        // enumerating anything.
        let raw = parse_display_id(display_id)?;
        let canonical = format_display_id(raw);
        let target = list_displays()?
            .into_iter()
            .find(|d| d.id == canonical)
            .ok_or_else(|| CaptureError::NoSuchDisplay(display_id.to_string()))?;
        Ok((target, Some(raw)))
    }
}

pub(crate) fn open(
    display_id: &str,
    opts: CaptureOptions,
) -> Result<Box<dyn ScreenCapturer>, CaptureError> {
    if !scap::is_supported() {
        return Err(CaptureError::Unsupported(UNSUPPORTED.to_string()));
    }
    if !scap::has_permission() {
        return Err(CaptureError::PermissionDenied);
    }
    let (target, raw_display_id) = resolve_display(display_id)?;

    let config = WorkerConfig {
        raw_display_id,
        // 0 fps would divide by zero in the frame limiter and become a zero
        // timescale in ScreenCaptureKit's CMTime.
        fps: opts.max_fps.clamp(MIN_FPS, MAX_FPS),
        show_cursor: opts.show_cursor,
    };
    let shared = Arc::new(Shared {
        state: Mutex::new(SlotState::default()),
        ready: Condvar::new(),
    });
    let stop = Arc::new(AtomicBool::new(false));
    let (startup_tx, startup_rx) = mpsc::channel();

    let worker = {
        let shared = Arc::clone(&shared);
        let stop = Arc::clone(&stop);
        thread::Builder::new()
            .name("remu-capture".to_string())
            .spawn(move || worker_main(config, &shared, &stop, &startup_tx))
            .map_err(|e| CaptureError::Backend(format!("could not start a capture thread: {e}")))?
    };

    match startup_rx.recv_timeout(STARTUP_TIMEOUT) {
        Ok(Ok(())) => Ok(Box::new(ScapCapturer {
            target,
            shared,
            stop,
            worker: Some(worker),
        })),
        Ok(Err(e)) => {
            stop.store(true, Ordering::Relaxed);
            let _ = worker.join();
            Err(e)
        }
        Err(RecvTimeoutError::Timeout) => {
            // Detached on purpose: the thread is stuck inside the OS (most
            // likely an unanswered portal dialog) and joining would hang the
            // caller too. The flag makes it exit as soon as it returns.
            stop.store(true, Ordering::Relaxed);
            Err(CaptureError::Backend(format!(
                "the screen-capture backend did not start within {}s",
                STARTUP_TIMEOUT.as_secs()
            )))
        }
        Err(RecvTimeoutError::Disconnected) => {
            let _ = worker.join();
            Err(CaptureError::Backend(end_message(&shared).unwrap_or_else(
                || "the capture thread ended without saying why".to_string(),
            )))
        }
    }
}

#[derive(Debug)]
struct ScapCapturer {
    target: DisplayTarget,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    /// `None` once joined, so stopping twice cannot join twice.
    worker: Option<JoinHandle<()>>,
}

impl ScreenCapturer for ScapCapturer {
    fn target(&self) -> &DisplayTarget {
        &self.target
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, CaptureError> {
        let deadline = Instant::now() + timeout;
        // Cloned so the guard borrows the Arc, not `self`, leaving the target
        // free to be corrected below.
        let shared = Arc::clone(&self.shared);
        let mut state = lock(&shared.state);
        let frame = loop {
            if let Some(frame) = state.newest.take() {
                break frame;
            }
            match &state.end {
                Some(EndReason::Stopped) => {
                    return Err(CaptureError::Backend(
                        "the capture stream has been stopped".to_string(),
                    ))
                }
                Some(EndReason::Failed(why)) => return Err(CaptureError::Backend(why.clone())),
                None => {}
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Ok(None); // idle screen, not an error
            };
            let (next, _) = shared
                .ready
                .wait_timeout(state, remaining)
                .unwrap_or_else(|e| e.into_inner());
            state = next;
        };
        drop(state);

        // The Linux portal only reveals the stream geometry once frames flow,
        // so a placeholder target learns its size from the first frame.
        if self.target.width == 0 || self.target.height == 0 {
            self.target.width = frame.width;
            self.target.height = frame.height;
        }
        Ok(Some(frame))
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            // Joining is what guarantees the OS stream is torn down — on
            // macOS that is what turns the recording indicator off — and it
            // costs at most one POLL interval.
            let _ = worker.join();
        }
    }
}

impl Drop for ScapCapturer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Debug)]
struct Shared {
    state: Mutex<SlotState>,
    ready: Condvar,
}

/// A one-frame mailbox rather than a queue.
///
/// A viewer that falls behind wants the newest screen, not a backlog of stale
/// ones: replacing the pending frame bounds both latency and memory at one
/// frame instead of letting a slow encoder grow an unbounded queue of 4K
/// buffers.
#[derive(Debug, Default)]
struct SlotState {
    newest: Option<Frame>,
    end: Option<EndReason>,
}

#[derive(Debug)]
enum EndReason {
    Stopped,
    Failed(String),
}

/// A worker that panicked leaves the mutex poisoned; the state behind it is
/// still exactly what we need to report, so recover it instead of panicking in
/// turn.
fn lock(state: &Mutex<SlotState>) -> MutexGuard<'_, SlotState> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

fn end_message(shared: &Shared) -> Option<String> {
    match lock(&shared.state).end {
        Some(EndReason::Failed(ref why)) => Some(why.clone()),
        Some(EndReason::Stopped) => {
            Some("the capture stream stopped before delivering a frame".to_string())
        }
        None => None,
    }
}

#[derive(Debug, Clone, Copy)]
struct WorkerConfig {
    raw_display_id: Option<u32>,
    fps: u32,
    show_cursor: bool,
}

fn worker_main(
    config: WorkerConfig,
    shared: &Shared,
    stop: &AtomicBool,
    startup: &mpsc::Sender<Result<(), CaptureError>>,
) {
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        worker_body(config, shared, stop, startup)
    }));
    let end = match outcome {
        Ok(Ok(())) => EndReason::Stopped,
        Ok(Err(e)) => {
            // Harmless if `open` already got its Ok: the receiver is gone and
            // the error is reported through the mailbox instead.
            let _ = startup.send(Err(e.clone()));
            EndReason::Failed(e.to_string())
        }
        Err(payload) => {
            let why = format!(
                "the screen-capture backend panicked: {}",
                panic_text(&payload)
            );
            let _ = startup.send(Err(CaptureError::Backend(why.clone())));
            EndReason::Failed(why)
        }
    };
    let mut state = lock(&shared.state);
    state.end.get_or_insert(end);
    drop(state);
    shared.ready.notify_all();
}

fn worker_body(
    config: WorkerConfig,
    shared: &Shared,
    stop: &AtomicBool,
    startup: &mpsc::Sender<Result<(), CaptureError>>,
) -> Result<(), CaptureError> {
    let target = match config.raw_display_id {
        Some(raw) => Some(find_display(raw)?),
        None => None,
    };
    let options = Options {
        fps: config.fps,
        show_cursor: config.show_cursor,
        target,
        // BGRA is the one format every backend can produce and the only one
        // whose macOS path reports an idle screen instead of stalling.
        output_type: FrameType::BGRAFrame,
        output_resolution: Resolution::Captured,
        ..Default::default()
    };

    let (raw_tx, raw_rx) = mpsc::channel();
    let mut engine = scap::capturer::engine::Engine::new(&options, raw_tx);
    engine.start();
    if startup.send(Ok(())).is_err() {
        engine.stop(); // `open` gave up waiting; do not leave the stream live
        return Ok(());
    }

    let min_interval = Duration::from_secs_f64(1.0 / f64::from(config.fps));
    let mut last_frame = Instant::now()
        .checked_sub(min_interval)
        .unwrap_or_else(Instant::now);

    let result = loop {
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }
        match raw_rx.recv_timeout(POLL) {
            Ok(item) => {
                let Some(raw) = engine.process_channel_item(item) else {
                    continue;
                };
                // Rate-limit here as well as in the backend: Windows'
                // Graphics.Capture has no frame-interval setting, so this is
                // the only thing honouring `max_fps` there. Dropping before
                // conversion is what makes it a saving.
                if last_frame.elapsed() < min_interval {
                    continue;
                }
                match to_frame(raw) {
                    Ok(Some(frame)) => {
                        last_frame = Instant::now();
                        let mut state = lock(&shared.state);
                        state.newest = Some(frame);
                        drop(state);
                        shared.ready.notify_all();
                    }
                    Ok(None) => {}
                    Err(e) => break Err(e),
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                break Err(CaptureError::Backend(
                    "the screen-capture backend closed its frame channel".to_string(),
                ))
            }
        }
    };
    engine.stop();
    result
}

fn find_display(raw: u32) -> Result<scap::Target, CaptureError> {
    scap::get_all_targets()
        .into_iter()
        .find(|t| matches!(t, scap::Target::Display(d) if d.id == raw))
        // Not the same as the check in `open`: the display can be unplugged
        // between the two, and scap would panic on the missing id.
        .ok_or_else(|| CaptureError::NoSuchDisplay(format_display_id(raw)))
}

/// Normalises whatever scap produced into BGRA8.
///
/// `Ok(None)` means "this was not a picture": macOS reports an unchanged
/// screen as a 0x0 BGRA frame with an empty buffer, which is a normal idle
/// tick rather than a fault.
fn to_frame(frame: ScapFrame) -> Result<Option<Frame>, CaptureError> {
    match frame {
        ScapFrame::BGRA(f) => match dimensions(f.width, f.height) {
            Some((w, h)) if !f.data.is_empty() => {
                to_bgra8(f.data, w, h, PixelLayout::Bgra).map(Some)
            }
            _ => Ok(None),
        },
        ScapFrame::BGRx(f) => convert(f.data, f.width, f.height, PixelLayout::Bgrx),
        ScapFrame::RGBx(f) => convert(f.data, f.width, f.height, PixelLayout::Rgbx),
        ScapFrame::XBGR(f) => convert(f.data, f.width, f.height, PixelLayout::Xbgr),
        ScapFrame::RGB(f) => convert(f.data, f.width, f.height, PixelLayout::Rgb),
        ScapFrame::BGR0(f) => convert(f.data, f.width, f.height, PixelLayout::Bgr),
        ScapFrame::YUVFrame(f) => match dimensions(f.width, f.height) {
            Some((w, h)) => nv12_to_bgra8(
                &f.luminance_bytes,
                stride(f.luminance_stride),
                &f.chrominance_bytes,
                stride(f.chrominance_stride),
                w,
                h,
            )
            .map(Some),
            None => Ok(None),
        },
    }
}

fn convert(
    data: Vec<u8>,
    width: i32,
    height: i32,
    layout: PixelLayout,
) -> Result<Option<Frame>, CaptureError> {
    match dimensions(width, height) {
        Some((w, h)) if !data.is_empty() => to_bgra8(data, w, h, layout).map(Some),
        _ => Ok(None),
    }
}

/// scap reports geometry as `i32`; a negative or zero value is an idle tick,
/// not a frame to encode.
fn dimensions(width: i32, height: i32) -> Option<(u32, u32)> {
    match (u32::try_from(width), u32::try_from(height)) {
        (Ok(w), Ok(h)) if w > 0 && h > 0 => Some((w, h)),
        _ => None,
    }
}

fn stride(value: i32) -> usize {
    usize::try_from(value).unwrap_or(0) // 0 fails the plane checks with a message
}

fn panic_text(payload: &Box<dyn Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "no message".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scap::frame::{BGRAFrame, BGRxFrame, YUVFrame};

    #[test]
    fn an_idle_macos_tick_is_no_frame_rather_than_an_error() {
        // What ScreenCaptureKit sends for an unchanged screen.
        let idle = ScapFrame::BGRA(BGRAFrame {
            display_time: 12,
            width: 0,
            height: 0,
            data: vec![],
        });
        assert!(matches!(to_frame(idle), Ok(None)));
    }

    #[test]
    fn a_frame_with_a_negative_dimension_is_ignored() {
        let bogus = ScapFrame::BGRx(BGRxFrame {
            display_time: 0,
            width: -4,
            height: 2,
            data: vec![0; 32],
        });
        assert!(matches!(to_frame(bogus), Ok(None)));
    }

    #[test]
    fn a_bgra_frame_keeps_its_pixels_and_gains_a_stride() {
        let frame = to_frame(ScapFrame::BGRA(BGRAFrame {
            display_time: 0,
            width: 2,
            height: 1,
            data: vec![1, 2, 3, 4, 5, 6, 7, 8],
        }))
        .unwrap()
        .expect("a 2x1 frame is a frame");
        assert_eq!((frame.width, frame.height, frame.stride), (2, 1, 8));
        assert_eq!(frame.data, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn a_truncated_buffer_is_reported_not_read_past() {
        // Claims 4x4 but carries one row.
        let short = ScapFrame::BGRx(BGRxFrame {
            display_time: 0,
            width: 4,
            height: 4,
            data: vec![0; 16],
        });
        assert!(matches!(to_frame(short), Err(CaptureError::Backend(_))));
    }

    #[test]
    fn a_yuv_frame_with_impossible_strides_errors_instead_of_panicking() {
        let frame = ScapFrame::YUVFrame(YUVFrame {
            display_time: 0,
            width: 4,
            height: 4,
            luminance_bytes: vec![16; 16],
            luminance_stride: -1,
            chrominance_bytes: vec![128; 8],
            chrominance_stride: -1,
        });
        assert!(matches!(to_frame(frame), Err(CaptureError::Backend(_))));
    }

    #[test]
    fn a_missing_display_is_named_in_the_error() {
        let err = resolve_display("display:4294967295").unwrap_err();
        // A machine with no capture support answers that first; either way the
        // caller learns something true rather than getting a capturer.
        assert!(
            matches!(
                err,
                CaptureError::NoSuchDisplay(_)
                    | CaptureError::Unsupported(_)
                    | CaptureError::PermissionDenied
                    | CaptureError::NoDisplays
            ),
            "{err:?}"
        );
    }

    #[test]
    fn a_malformed_display_id_is_rejected_without_asking_the_os() {
        let err = resolve_display("not-a-display").unwrap_err();
        assert_eq!(
            err,
            CaptureError::NoSuchDisplay("not-a-display".to_string())
        );
    }

    #[test]
    #[ignore = "needs a real display and screen-recording permission; run with --ignored"]
    fn captures_a_frame_from_the_first_display() {
        let displays = list_displays().expect("a desktop session has displays");
        let target = displays.first().expect("at least one display");
        let mut capturer = open(
            &target.id,
            CaptureOptions {
                max_fps: 10,
                show_cursor: false,
            },
        )
        .expect("capture opens");

        // Idle screens legitimately yield None, so poll for a few seconds.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got = None;
        while Instant::now() < deadline && got.is_none() {
            got = capturer
                .next_frame(Duration::from_millis(500))
                .expect("no backend error");
        }
        let frame = got.expect("a frame within 5s");
        assert_eq!(frame.stride, frame.width as usize * 4);
        assert_eq!(frame.data.len(), frame.stride * frame.height as usize);
        capturer.stop();
        assert!(capturer.next_frame(Duration::from_millis(10)).is_err());
    }
}
