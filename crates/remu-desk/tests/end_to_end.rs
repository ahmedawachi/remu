//! A real session, all the way through.
//!
//! Two engines, a real relay on an ephemeral port, real signalling, real
//! WebRTC, real screen capture, real H.264. One machine hosts, the other
//! controls, and the test passes only when a decoded frame comes out the far
//! end.
//!
//! Every other test in this workspace checks one layer. This is the only one
//! that can catch the layers disagreeing — an SDP that negotiates but carries
//! no video, a capture loop feeding an encoder the wrong stride, a decoder
//! handed Annex-B it cannot parse.
//!
//! It needs a real display and screen-recording permission, so it is ignored by
//! default:
//!
//! ```text
//! cargo test -p remu-desk --test end_to_end -- --ignored --nocapture
//! ```

use std::time::{Duration, Instant};

use remu_desk::runtime::{Command, Runtime, Update};
use remu_desk::state::LinkStatus;
use remu_proto::{PeerId, Settings};
use remu_relay::{Relay, RelayConfig};

/// Generous: ICE gathering, DTLS and the first keyframe all have to happen.
const SESSION_TIMEOUT: Duration = Duration::from_secs(45);

/// Polls `drain` until `want` matches an update or the deadline passes.
///
/// Returns every update seen, so a failure can say what *did* arrive rather
/// than only that the thing wanted did not.
fn wait_for<T>(
    engine: &Runtime,
    label: &str,
    timeout: Duration,
    mut want: impl FnMut(&Update) -> Option<T>,
    seen: &mut Vec<String>,
) -> T {
    let deadline = Instant::now() + timeout;
    loop {
        for update in engine.drain() {
            seen.push(format!("{update:?}"));
            if let Some(value) = want(&update) {
                return value;
            }
        }
        if Instant::now() > deadline {
            panic!(
                "timed out waiting for {label}.\nupdates seen:\n  {}",
                seen.join("\n  ")
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn settings_for(url: &str, alias: &str) -> Settings {
    Settings {
        relay_url: url.to_owned(),
        alias: alias.to_owned(),
        // Accept without a human: there is nobody to click the prompt.
        accept_policy: remu_proto::AcceptPolicy::Always,
        ..Settings::default()
    }
}

#[test]
#[ignore = "needs a real display and screen-recording permission; run with --ignored"]
fn two_engines_complete_a_session_and_a_frame_arrives() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("remu_session=debug,remu_desk=debug,warn")
        .try_init();

    // --- a relay on a port the OS picks ---
    let relay_rt = tokio::runtime::Runtime::new().expect("relay runtime");
    let relay = relay_rt.block_on(async {
        Relay::bind(RelayConfig {
            port: 0,
            ..RelayConfig::default()
        })
        .await
        .expect("bind the relay")
    });
    let url = format!("ws://127.0.0.1:{}", relay.local_addr().port());
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    relay_rt.spawn(async move {
        let _ = relay
            .serve_with_shutdown(async {
                let _ = stop_rx.await;
            })
            .await;
    });

    // --- two engines, as two people on two machines ---
    let host = Runtime::start(settings_for(&url, "host"), || {});
    let controller = Runtime::start(settings_for(&url, "controller"), || {});

    let mut host_seen = Vec::new();
    let mut ctrl_seen = Vec::new();

    let host_id: PeerId = wait_for(
        &host,
        "the host's desk ID",
        Duration::from_secs(15),
        |u| match u {
            Update::MyId(id) => Some(*id),
            _ => None,
        },
        &mut host_seen,
    );
    let controller_id: PeerId = wait_for(
        &controller,
        "the controller's desk ID",
        Duration::from_secs(15),
        |u| match u {
            Update::MyId(id) => Some(*id),
            _ => None,
        },
        &mut ctrl_seen,
    );
    assert_ne!(host_id, controller_id, "the relay handed out one ID twice");
    println!("host={host_id} controller={controller_id}");

    // --- connect ---
    controller.send(Command::Connect {
        peer: host_id,
        password: String::new(),
    });

    // The host accepts on its own (AcceptPolicy::Always), so the controller
    // should see the session come up without anyone clicking anything.
    let remote = wait_for(
        &controller,
        "the session to start on the controller",
        SESSION_TIMEOUT,
        |u| match u {
            Update::SessionStarted { remote, .. } => Some(*remote),
            _ => None,
        },
        &mut ctrl_seen,
    );
    assert_eq!(remote, host_id);

    wait_for(
        &host,
        "the session to start on the host",
        SESSION_TIMEOUT,
        |u| matches!(u, Update::SessionStarted { .. }).then_some(()),
        &mut host_seen,
    );

    wait_for(
        &controller,
        "the link to report connected",
        SESSION_TIMEOUT,
        |u| matches!(u, Update::Link(LinkStatus::Connected)).then_some(()),
        &mut ctrl_seen,
    );

    // --- the whole point: a real picture of the host's screen ---
    let (width, height, bytes) = wait_for(
        &controller,
        "a decoded video frame",
        SESSION_TIMEOUT,
        |u| match u {
            Update::Frame {
                width,
                height,
                rgba,
            } => Some((*width, *height, rgba.len())),
            _ => None,
        },
        &mut ctrl_seen,
    );
    println!("decoded a {width}x{height} frame ({bytes} bytes)");
    assert!(width >= 320 && height >= 240, "implausible frame size");
    assert_eq!(
        bytes,
        width as usize * height as usize * 4,
        "frame is not tightly packed RGBA"
    );

    // --- and it keeps going, rather than delivering one keyframe and dying ---
    let mut frames = 1;
    let deadline = Instant::now() + Duration::from_secs(10);
    while frames < 5 && Instant::now() < deadline {
        for update in controller.drain() {
            if matches!(update, Update::Frame { .. }) {
                frames += 1;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        frames >= 5,
        "only {frames} frame(s) arrived; the stream stalled"
    );

    // --- teardown ---
    controller.send(Command::EndSession);
    wait_for(
        &host,
        "the host to notice the session ended",
        Duration::from_secs(20),
        |u| matches!(u, Update::SessionEnded(_)).then_some(()),
        &mut host_seen,
    );

    drop(controller);
    drop(host);
    let _ = stop_tx.send(());
}
