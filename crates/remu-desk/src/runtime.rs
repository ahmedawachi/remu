//! The engine behind the UI: relay, session lifecycle, and the media loops.
//!
//! The views are pure and the shell only mutates state, so everything that
//! blocks, waits or talks to the world lives here. The boundary is two
//! channels: the shell sends [`Command`]s and drains [`Update`]s once a frame,
//! and neither side ever holds a lock the other needs.
//!
//! Three execution contexts, deliberately separated:
//!
//! - a **tokio runtime** owning the WebSocket and all WebRTC I/O;
//! - a **capture thread** per hosted session, because capture and H.264
//!   encoding are blocking and must never stall the reactor;
//! - a **decode thread** per controlled session, for the same reason.
//!
//! Frames move by channel and are *moved*, never shared, so no frame is ever
//! behind a mutex on the hot path.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TrySendError};
use remu_proto::{
    auth, ClientToServer, ControlMessage, DisplayInfo, InputEvent, PeerId, RejectReason,
    ServerToClient, SessionDescription, SessionId, SessionStats, Settings,
};
use remu_session::peer::{ConnState, IceConfig, PeerSession, Role, SessionEvent};
use remu_session::{RelayClient, RelayEvent};
use tokio::sync::mpsc;

use crate::state::LinkStatus;

/// Frames the decoder may fall behind before we start dropping.
///
/// Small on purpose: in a remote session a late frame is worthless, and the
/// only thing a deep queue buys is latency the user feels as lag.
const FRAME_QUEUE: usize = 2;

/// Updates buffered for the UI thread between repaints.
const UPDATE_QUEUE: usize = 256;

/// Something the UI asks the engine to do.
#[derive(Debug)]
pub enum Command {
    ConnectRelay(Box<Settings>),
    /// Control the desk with this ID. The password may be empty.
    Connect {
        peer: PeerId,
        password: String,
    },
    AcceptIncoming,
    DeclineIncoming,
    EndSession,
    SendChat(String),
    SendFiles(Vec<PathBuf>),
    SwitchDisplay(String),
    RemoteInput(InputEvent),
    SettingsChanged(Box<Settings>),
    Shutdown,
}

/// Something that happened, for the UI to reflect.
///
/// `Debug` is hand-written for one reason: a derived one prints every byte of
/// a decoded frame, which turns a single trace line into megabytes and made
/// the end-to-end test emit 64 MB of pixel values.
pub enum Update {
    Link(LinkStatus),
    MyId(PeerId),
    Incoming {
        from: PeerId,
        alias: Option<String>,
        session_id: SessionId,
        authenticated: bool,
    },
    IncomingWithdrawn,
    SessionStarted {
        remote: PeerId,
        alias: Option<String>,
        controller: bool,
    },
    SessionEnded(String),
    /// A decoded frame for the controller to present. RGBA8, tightly packed.
    Frame {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    Displays {
        displays: Vec<DisplayInfo>,
        active: String,
    },
    Chat {
        text: String,
        at: u64,
    },
    Stats(SessionStats),
    Toast {
        error: bool,
        text: String,
    },
}

impl std::fmt::Debug for Update {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Update::Link(link) => write!(f, "Link({link:?})"),
            Update::MyId(id) => write!(f, "MyId({id})"),
            Update::Incoming {
                from,
                authenticated,
                ..
            } => {
                write!(f, "Incoming(from={from}, authenticated={authenticated})")
            }
            Update::IncomingWithdrawn => write!(f, "IncomingWithdrawn"),
            Update::SessionStarted {
                remote, controller, ..
            } => {
                write!(
                    f,
                    "SessionStarted(remote={remote}, controller={controller})"
                )
            }
            Update::SessionEnded(reason) => write!(f, "SessionEnded({reason})"),
            Update::Frame {
                width,
                height,
                rgba,
            } => {
                write!(f, "Frame({width}x{height}, {} bytes)", rgba.len())
            }
            Update::Displays { displays, active } => {
                write!(f, "Displays({} available, active={active})", displays.len())
            }
            Update::Chat { text, .. } => write!(f, "Chat({} chars)", text.chars().count()),
            Update::Stats(stats) => write!(f, "Stats({:.0} fps)", stats.fps),
            Update::Toast { error, text } => write!(f, "Toast(error={error}, {text:?})"),
        }
    }
}

/// Handle to the engine. Dropping it shuts everything down.
pub struct Runtime {
    commands: mpsc::UnboundedSender<Command>,
    updates: Receiver<Update>,
    _tokio: tokio::runtime::Runtime,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime").finish_non_exhaustive()
    }
}

impl Runtime {
    /// Starts the engine and immediately connects to the configured relay.
    pub fn start(settings: Settings, repaint: impl Fn() + Send + Sync + 'static) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (update_tx, update_rx) = crossbeam_channel::bounded(UPDATE_QUEUE);

        let tokio = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("the tokio runtime is built once, at startup");

        let emitter = Emitter {
            tx: update_tx,
            repaint: Arc::new(repaint),
        };
        tokio.spawn(drive(settings, cmd_rx, emitter));

        Self {
            commands: cmd_tx,
            updates: update_rx,
            _tokio: tokio,
        }
    }

    pub fn send(&self, command: Command) {
        if self.commands.send(command).is_err() {
            tracing::warn!("engine has stopped; command dropped");
        }
    }

    /// Everything that has happened since the last call. Never blocks.
    pub fn drain(&self) -> impl Iterator<Item = Update> + '_ {
        self.updates.try_iter()
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
    }
}

/// Sends updates to the UI and wakes it.
#[derive(Clone)]
struct Emitter {
    tx: Sender<Update>,
    repaint: Arc<dyn Fn() + Send + Sync>,
}

impl Emitter {
    /// Drops the update rather than blocking when the UI is behind.
    ///
    /// Blocking here would stall the WebRTC task, which is far worse than a
    /// skipped frame: the transport would stop reading the socket and the peer
    /// would see the link die.
    fn send(&self, update: Update) {
        match self.tx.try_send(update) {
            Ok(()) => (self.repaint)(),
            Err(TrySendError::Full(dropped)) => {
                tracing::debug!(?dropped, "UI is behind; dropping an update")
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    fn toast(&self, error: bool, text: impl Into<String>) {
        self.send(Update::Toast {
            error,
            text: text.into(),
        });
    }
}

/// What the engine is doing right now.
struct Active {
    peer: Arc<PeerSession>,
    remote: PeerId,
    session_id: SessionId,
    controller: bool,
    /// Host side: told to stop when the session ends.
    capture_stop: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Controller side: encoded frames go here to be decoded off-reactor.
    decode_tx: Option<mpsc::Sender<bytes::Bytes>>,
}

/// An offer waiting on a human.
struct Pending {
    from: PeerId,
    alias: Option<String>,
    session_id: SessionId,
    sdp: SessionDescription,
}

async fn drive(
    mut settings: Settings,
    mut commands: mpsc::UnboundedReceiver<Command>,
    ui: Emitter,
) {
    let mut relay = connect_relay(&settings, &ui);
    let mut relay_events = relay.events();

    // Per-session channel, recreated for each session.
    let (mut session_tx, mut session_rx) = mpsc::channel::<SessionEvent>(64);
    let mut active: Option<Active> = None;
    let mut pending: Option<Pending> = None;
    // Nonce this host issued for an attempt in flight, single use.
    let mut issued_nonce: Option<(SessionId, String)> = None;
    // Password the controller supplied, held until the challenge arrives.
    let mut outgoing_password: Option<(SessionId, PeerId, String)> = None;

    loop {
        tokio::select! {
            Some(command) = commands.recv() => {
                match command {
                    Command::Shutdown => {
                        if let Some(a) = active.take() {
                            stop_session(&a).await;
                        }
                        relay.shutdown();
                        return;
                    }
                    Command::ConnectRelay(next) => {
                        settings = *next;
                        relay.shutdown();
                        relay = connect_relay(&settings, &ui);
                        relay_events = relay.events();
                    }
                    Command::SettingsChanged(next) => settings = *next,
                    Command::Connect { peer, password } => {
                        if active.is_some() {
                            ui.toast(true, "Already in a session");
                            continue;
                        }
                        let session_id = SessionId::random();
                        outgoing_password = Some((session_id, peer, password));
                        relay.send(ClientToServer::Connect { to: peer, session_id });
                        ui.send(Update::Link(LinkStatus::Calling));
                    }
                    Command::AcceptIncoming => {
                        if let Some(p) = pending.take() {
                            match begin_host(&p, &settings, &session_tx, &relay, &ui).await {
                                Ok(a) => {
                                    ui.send(Update::SessionStarted {
                                        remote: a.remote,
                                        alias: p.alias.clone(),
                                        controller: false,
                                    });
                                    active = Some(a);
                                }
                                Err(err) => {
                                    relay.send(ClientToServer::Reject {
                                        to: p.from,
                                        session_id: p.session_id,
                                        reason: RejectReason::CaptureFailed,
                                        detail: Some(err.clone()),
                                    });
                                    ui.toast(true, err);
                                    ui.send(Update::Link(LinkStatus::Ready));
                                }
                            }
                        }
                    }
                    Command::DeclineIncoming => {
                        if let Some(p) = pending.take() {
                            relay.send(ClientToServer::Reject {
                                to: p.from,
                                session_id: p.session_id,
                                reason: RejectReason::Declined,
                                detail: None,
                            });
                            ui.send(Update::Link(LinkStatus::Ready));
                        }
                    }
                    Command::EndSession => {
                        if let Some(a) = active.take() {
                            relay.send(ClientToServer::Bye { to: a.remote, session_id: a.session_id });
                            stop_session(&a).await;
                            ui.send(Update::SessionEnded("ended".into()));
                            ui.send(Update::Link(LinkStatus::Ready));
                        }
                    }
                    Command::RemoteInput(event) => {
                        if let Some(a) = active.as_ref().filter(|a| a.controller) {
                            let _ = a.peer.send_control(&ControlMessage::Input { event }).await;
                        }
                    }
                    Command::SendChat(text) => {
                        if let Some(a) = active.as_ref() {
                            let at = now_ms();
                            let _ = a.peer.send_control(&ControlMessage::Chat { text, at }).await;
                        }
                    }
                    Command::SwitchDisplay(source_id) => {
                        if let Some(a) = active.as_ref().filter(|a| a.controller) {
                            let _ = a.peer
                                .send_control(&ControlMessage::SwitchDisplay { source_id })
                                .await;
                        }
                    }
                    Command::SendFiles(paths) => {
                        if active.is_some() {
                            // The transfer engine exists in remu-session; wiring the
                            // picker to it is the next change, and saying so beats a
                            // button that silently does nothing.
                            ui.toast(true, format!("File transfer is not wired yet ({} file(s))", paths.len()));
                        }
                    }
                }
            }

            Some(event) = async { match relay_events.as_mut() { Some(rx) => rx.recv().await, None => None } } => {
                match event {
                    RelayEvent::Connected => ui.send(Update::Link(LinkStatus::Connecting)),
                    RelayEvent::Welcome { peer_id } => {
                        ui.send(Update::MyId(peer_id));
                        ui.send(Update::Link(LinkStatus::Ready));
                    }
                    RelayEvent::Disconnected { reason } => {
                        ui.send(Update::Link(LinkStatus::Failed(reason)));
                    }
                    RelayEvent::Error { message } => ui.toast(true, message),
                    RelayEvent::Message(msg) => {
                        handle_relay_message(
                            msg, &settings, &relay, &ui, &session_tx,
                            &mut active, &mut pending, &mut issued_nonce, &mut outgoing_password,
                        ).await;
                    }
                }
            }

            Some(event) = session_rx.recv() => {
                match event {
                    SessionEvent::LocalIce(candidate) => {
                        if let Some(a) = active.as_ref() {
                            relay.send(ClientToServer::Ice {
                                to: a.remote,
                                session_id: a.session_id,
                                candidate,
                            });
                        }
                    }
                    SessionEvent::ConnectionState(state) => match state {
                        ConnState::Connected => ui.send(Update::Link(LinkStatus::Connected)),
                        ConnState::Failed => {
                            ui.toast(true, "The peer connection failed — a TURN relay may be needed");
                        }
                        // New, Connecting, Disconnected and Closed are all
                        // normal points in the handshake; `Closed` below is the
                        // terminal event the UI actually reacts to.
                        _ => {}
                    },
                    SessionEvent::ChannelsOpen => {
                        if let Some(a) = active.as_ref() {
                            on_channels_open(a, &settings, &ui).await;
                        }
                    }
                    SessionEvent::Control(msg) => {
                        handle_control(msg, &settings, active.as_ref(), &ui).await;
                    }
                    SessionEvent::VideoFrame { data, .. } => {
                        if let Some(tx) = active.as_ref().and_then(|a| a.decode_tx.as_ref()) {
                            // try_send, not send: a decoder that has fallen
                            // behind must drop frames rather than back up into
                            // the transport. A late frame is worthless anyway.
                            if tx.try_send(data).is_err() {
                                tracing::trace!("decoder is behind; dropping a frame");
                            }
                        }
                    }
                    SessionEvent::BulkChunk { .. } => {}
                    SessionEvent::Closed { reason } => {
                        if let Some(a) = active.take() {
                            stop_session(&a).await;
                        }
                        ui.send(Update::SessionEnded(reason));
                        ui.send(Update::Link(LinkStatus::Ready));
                        // A fresh channel, so a late event from the old session
                        // cannot be mistaken for the next one's.
                        (session_tx, session_rx) = mpsc::channel(64);
                    }
                }
            }

            else => return,
        }
    }
}

fn connect_relay(settings: &Settings, ui: &Emitter) -> RelayClient {
    ui.send(Update::Link(LinkStatus::Connecting));
    RelayClient::connect(
        &settings.relay_url,
        Some(settings.alias.clone()).filter(|a| !a.trim().is_empty()),
        Some(settings.relay_token.clone()).filter(|t| !t.is_empty()),
    )
}

#[allow(clippy::too_many_arguments)]
async fn handle_relay_message(
    msg: ServerToClient,
    settings: &Settings,
    relay: &RelayClient,
    ui: &Emitter,
    session_tx: &mpsc::Sender<SessionEvent>,
    active: &mut Option<Active>,
    pending: &mut Option<Pending>,
    issued_nonce: &mut Option<(SessionId, String)>,
    outgoing_password: &mut Option<(SessionId, PeerId, String)>,
) {
    match msg {
        // --- host side ---
        ServerToClient::Connect {
            from, session_id, ..
        } => {
            if active.is_some() || pending.is_some() {
                relay.send(ClientToServer::Reject {
                    to: from,
                    session_id,
                    reason: RejectReason::Busy,
                    detail: None,
                });
                return;
            }
            let nonce = auth::generate_nonce();
            *issued_nonce = Some((session_id, nonce.clone()));
            relay.send(ClientToServer::Challenge {
                to: from,
                session_id,
                nonce,
                needs_password: settings.unattended_enabled(),
            });
        }
        ServerToClient::Offer {
            from,
            from_alias,
            session_id,
            sdp,
            auth: proof,
        } => {
            // The nonce is single use: taking it here means a replayed offer
            // carrying the same proof finds nothing to check against.
            let nonce = match issued_nonce.take() {
                Some((id, nonce)) if id == session_id => nonce,
                _ => {
                    relay.send(ClientToServer::Reject {
                        to: from,
                        session_id,
                        reason: RejectReason::Other,
                        detail: Some("no challenge is outstanding".into()),
                    });
                    return;
                }
            };

            let authenticated = settings.unattended_enabled()
                && proof
                    .as_deref()
                    .is_some_and(|p| auth::verify(&settings.unattended_password, &nonce, p));

            let auto =
                authenticated || matches!(settings.accept_policy, remu_proto::AcceptPolicy::Always);

            let waiting = Pending {
                from,
                alias: from_alias.clone(),
                session_id,
                sdp,
            };
            if auto {
                match begin_host(&waiting, settings, session_tx, relay, ui).await {
                    Ok(a) => {
                        ui.send(Update::SessionStarted {
                            remote: a.remote,
                            alias: from_alias,
                            controller: false,
                        });
                        *active = Some(a);
                    }
                    Err(err) => {
                        relay.send(ClientToServer::Reject {
                            to: from,
                            session_id,
                            reason: RejectReason::CaptureFailed,
                            detail: Some(err.clone()),
                        });
                        ui.toast(true, err);
                    }
                }
            } else {
                ui.send(Update::Incoming {
                    from,
                    alias: from_alias,
                    session_id,
                    authenticated,
                });
                ui.send(Update::Link(LinkStatus::Incoming));
                *pending = Some(waiting);
            }
        }

        // --- controller side ---
        ServerToClient::Challenge {
            from,
            session_id,
            nonce,
            needs_password,
        } => {
            let Some((expected, peer, password)) = outgoing_password.take() else {
                return;
            };
            if expected != session_id || peer != from {
                return;
            }
            if needs_password && password.is_empty() {
                ui.toast(true, "That desk requires an unattended access password");
                ui.send(Update::Link(LinkStatus::Ready));
                return;
            }
            let proof = needs_password.then(|| auth::challenge_response(&password, &nonce));

            match start_controller(session_id, from, settings, session_tx).await {
                Ok((peer_session, offer)) => {
                    relay.send(ClientToServer::Offer {
                        to: from,
                        session_id,
                        sdp: offer,
                        auth: proof,
                    });
                    ui.send(Update::Link(LinkStatus::Negotiating));
                    // The controller needs this too, not just the host: it is
                    // what opens the session screen. Without it frames arrived
                    // with nowhere to be drawn. Sent now rather than on
                    // ChannelsOpen so the user sees the negotiating state
                    // instead of a frozen home screen.
                    ui.send(Update::SessionStarted {
                        remote: from,
                        alias: None,
                        controller: true,
                    });
                    let decode_tx = spawn_decoder(ui.clone());
                    *active = Some(Active {
                        peer: peer_session,
                        remote: from,
                        session_id,
                        controller: true,
                        capture_stop: None,
                        decode_tx: Some(decode_tx),
                    });
                }
                Err(err) => {
                    ui.toast(true, format!("Could not start the session: {err}"));
                    ui.send(Update::Link(LinkStatus::Ready));
                }
            }
        }
        ServerToClient::Answer {
            sdp, session_id, ..
        } => {
            if let Some(a) = active.as_ref().filter(|a| a.session_id == session_id) {
                if let Err(err) = a.peer.accept_answer(sdp).await {
                    ui.toast(true, format!("Bad answer from the peer: {err}"));
                }
            }
        }

        // --- either side ---
        ServerToClient::Ice {
            candidate,
            session_id,
            ..
        } => {
            if let Some(a) = active.as_ref().filter(|a| a.session_id == session_id) {
                let _ = a.peer.add_ice(candidate).await;
            }
        }
        ServerToClient::Reject { reason, detail, .. } => {
            *outgoing_password = None;
            if let Some(a) = active.take() {
                stop_session(&a).await;
            }
            let text = match detail {
                Some(d) => format!("{reason:?}: {d}"),
                None => format!("{reason:?}"),
            };
            ui.toast(true, format!("Connection refused — {text}"));
            ui.send(Update::SessionEnded(text));
            ui.send(Update::Link(LinkStatus::Ready));
        }
        ServerToClient::Bye { session_id, .. } => {
            if pending.as_ref().is_some_and(|p| p.session_id == session_id) {
                *pending = None;
                ui.send(Update::IncomingWithdrawn);
            }
            if let Some(a) = active.take().filter(|a| a.session_id == session_id) {
                stop_session(&a).await;
                ui.send(Update::SessionEnded("the peer hung up".into()));
            }
            ui.send(Update::Link(LinkStatus::Ready));
        }
        ServerToClient::Error { message, .. } => ui.toast(true, message),
        ServerToClient::Welcome { .. }
        | ServerToClient::LookupResult { .. }
        | ServerToClient::Pong => {}
    }
}

/// Builds the controller's peer connection and its offer.
async fn start_controller(
    session_id: SessionId,
    remote: PeerId,
    settings: &Settings,
    events: &mpsc::Sender<SessionEvent>,
) -> Result<(Arc<PeerSession>, SessionDescription), String> {
    let peer = PeerSession::new(
        session_id,
        Role::Controller,
        remote,
        ice_config(settings),
        events.clone(),
    )
    .await
    .map_err(|e| e.to_string())?;
    let peer = Arc::new(peer);
    let offer = peer.create_offer().await.map_err(|e| e.to_string())?;
    Ok((peer, offer))
}

/// Accepts an offer, starts capturing, and answers.
async fn begin_host(
    waiting: &Pending,
    settings: &Settings,
    events: &mpsc::Sender<SessionEvent>,
    relay: &RelayClient,
    ui: &Emitter,
) -> Result<Active, String> {
    let display = remu_capture::list_displays()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|d| d.primary)
        .or_else(|| remu_capture::list_displays().ok()?.into_iter().next())
        .ok_or_else(|| "no capturable display".to_string())?;

    let peer = Arc::new(
        PeerSession::new(
            waiting.session_id,
            Role::Host,
            waiting.from,
            ice_config(settings),
            events.clone(),
        )
        .await
        .map_err(|e| e.to_string())?,
    );

    let answer = peer
        .accept_offer(waiting.sdp.clone())
        .await
        .map_err(|e| e.to_string())?;
    relay.send(ClientToServer::Answer {
        to: waiting.from,
        session_id: waiting.session_id,
        sdp: answer,
    });

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    spawn_capture(
        display.id.clone(),
        settings.quality,
        Arc::clone(&peer),
        Arc::clone(&stop),
        ui.clone(),
    );

    Ok(Active {
        peer,
        remote: waiting.from,
        session_id: waiting.session_id,
        controller: false,
        capture_stop: Some(stop),
        decode_tx: None,
    })
}

fn ice_config(settings: &Settings) -> IceConfig {
    IceConfig {
        turn: settings.turn.is_configured().then(|| settings.turn.clone()),
        ..IceConfig::default()
    }
}

/// Decode → RGBA → UI, on its own thread.
///
/// Decoding a 4K frame costs milliseconds; doing it on the reactor would stall
/// every other socket the runtime owns. Dropping the returned sender ends the
/// thread, which is how a finished session cleans it up.
fn spawn_decoder(ui: Emitter) -> mpsc::Sender<bytes::Bytes> {
    let (tx, mut rx) = mpsc::channel::<bytes::Bytes>(FRAME_QUEUE);
    std::thread::Builder::new()
        .name("remu-decode".into())
        .spawn(move || {
            let mut decoder = match remu_codec::decoder() {
                Ok(d) => d,
                Err(err) => {
                    ui.toast(true, format!("Could not start the decoder: {err}"));
                    return;
                }
            };
            while let Some(sample) = rx.blocking_recv() {
                match decoder.decode(&sample) {
                    // `None` is normal before the first keyframe arrives.
                    Ok(None) => {}
                    Ok(Some(frame)) => ui.send(Update::Frame {
                        width: frame.width,
                        height: frame.height,
                        rgba: frame.rgba,
                    }),
                    Err(err) => tracing::debug!(%err, "decode failed; skipping a frame"),
                }
            }
        })
        .expect("spawning the decode thread");
    tx
}

/// Capture → encode → send, on its own thread.
fn spawn_capture(
    display_id: String,
    quality: remu_proto::Quality,
    peer: Arc<PeerSession>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    ui: Emitter,
) {
    use std::sync::atomic::Ordering;

    let (frames_tx, mut frames_rx) = mpsc::channel::<(Vec<u8>, Duration)>(FRAME_QUEUE);

    // The writer half stays on the reactor: write_video is async.
    tokio::spawn(async move {
        while let Some((sample, duration)) = frames_rx.recv().await {
            if let Err(err) = peer.write_video(&sample, duration).await {
                tracing::debug!(%err, "video write failed; ending the capture loop");
                break;
            }
        }
    });

    std::thread::Builder::new()
        .name("remu-capture".into())
        .spawn(move || {
            let (bitrate, fps) = quality.targets();
            let opts = remu_capture::CaptureOptions {
                max_fps: fps,
                show_cursor: true,
            };
            let mut capturer = match remu_capture::open(&display_id, opts) {
                Ok(c) => c,
                Err(err) => {
                    ui.toast(true, format!("Screen capture failed: {err}"));
                    return;
                }
            };

            let mut encoder: Option<Box<dyn remu_codec::VideoEncoder>> = None;
            let frame_budget = Duration::from_secs_f64(1.0 / f64::from(fps.max(1)));
            let started = Instant::now();

            while !stop.load(Ordering::Relaxed) {
                let frame = match capturer.next_frame(frame_budget) {
                    Ok(Some(frame)) => frame,
                    // An idle screen produces nothing; that is not an error.
                    Ok(None) => continue,
                    Err(err) => {
                        tracing::debug!(%err, "capture ended");
                        break;
                    }
                };

                // Odd dimensions are rejected by the encoder, and a display can
                // report one; crop rather than fail the session.
                let width = frame.width & !1;
                let height = frame.height & !1;
                if width == 0 || height == 0 {
                    continue;
                }

                let needs_new = encoder
                    .as_ref()
                    .is_none_or(|e| e.config().width != width || e.config().height != height);
                if needs_new {
                    match remu_codec::encoder(remu_codec::EncoderConfig {
                        width,
                        height,
                        bitrate_kbps: bitrate,
                        max_fps: fps,
                    }) {
                        Ok(e) => encoder = Some(e),
                        Err(err) => {
                            ui.toast(true, format!("Encoder failed: {err}"));
                            return;
                        }
                    }
                }

                let Some(enc) = encoder.as_mut() else {
                    continue;
                };
                let timestamp_us = started.elapsed().as_micros() as u64;
                match enc.encode(
                    &frame.data,
                    width,
                    height,
                    frame.stride,
                    timestamp_us,
                    false,
                ) {
                    // `None` is the encoder dropping a frame to hold the
                    // bitrate; the previous picture stands.
                    Ok(None) => {}
                    Ok(Some(encoded)) => {
                        if frames_tx
                            .blocking_send((encoded.data, frame_budget))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(err) => tracing::debug!(%err, "encode failed; skipping a frame"),
                }
            }
            capturer.stop();
        })
        .expect("spawning the capture thread");
}

/// Announces what this host allows and which displays it has.
async fn on_channels_open(active: &Active, settings: &Settings, ui: &Emitter) {
    if active.controller {
        return;
    }
    let _ = active
        .peer
        .send_control(&ControlMessage::Permissions(settings.host_permissions()))
        .await;

    if let Ok(displays) = remu_capture::list_displays() {
        let infos: Vec<DisplayInfo> = displays.iter().map(remu_capture::to_display_info).collect();
        let active_id = infos
            .iter()
            .find(|d| d.primary)
            .or_else(|| infos.first())
            .map(|d| d.id.clone())
            .unwrap_or_default();
        let _ = active
            .peer
            .send_control(&ControlMessage::DisplayList {
                displays: infos,
                active: active_id,
            })
            .await;
    }
    ui.toast(false, "Session connected");
}

/// Applies a control message from the peer.
async fn handle_control(
    msg: ControlMessage,
    settings: &Settings,
    active: Option<&Active>,
    ui: &Emitter,
) {
    let Some(active) = active else { return };
    match msg {
        ControlMessage::Input { event } => {
            // Only a host injects, and only when the user allows it. The event
            // is sanitized inside the injector as well: it is peer-supplied.
            if active.controller || !settings.allow_input {
                return;
            }
            inject(event);
        }
        ControlMessage::Chat { text, at } => ui.send(Update::Chat { text, at }),
        ControlMessage::DisplayList { displays, active } => {
            ui.send(Update::Displays { displays, active })
        }
        ControlMessage::Stats(stats) => ui.send(Update::Stats(stats)),
        ControlMessage::Clipboard { .. }
        | ControlMessage::CursorPos { .. }
        | ControlMessage::SwitchDisplay { .. }
        | ControlMessage::Permissions(_)
        | ControlMessage::FileMeta { .. }
        | ControlMessage::FileAccept { .. }
        | ControlMessage::FileRefuse { .. }
        | ControlMessage::FileEnd { .. }
        | ControlMessage::FileCancel { .. } => {}
    }
}

/// Injects one event on the shared injector.
///
/// Held in a thread-local rather than rebuilt per event: constructing an
/// `Enigo` opens a connection to the window server, which at input rates would
/// dominate the cost of the injection itself.
fn inject(event: InputEvent) {
    use std::cell::RefCell;
    thread_local! {
        static INJECTOR: RefCell<Option<remu_input::InputInjector>> = const { RefCell::new(None) };
    }
    INJECTOR.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            match remu_input::InputInjector::new() {
                Ok(i) => *slot = Some(i),
                Err(err) => {
                    tracing::warn!(%err, "cannot inject input");
                    return;
                }
            }
        }
        let Some(injector) = slot.as_mut() else {
            return;
        };
        let geometry = match remu_input::primary_geometry() {
            Ok(g) => g,
            Err(err) => {
                tracing::debug!(%err, "no screen geometry; dropping input");
                return;
            }
        };
        if let Err(err) = injector.inject(&event, geometry) {
            tracing::debug!(%err, "input injection failed");
        }
    });
}

async fn stop_session(active: &Active) {
    if let Some(stop) = active.capture_stop.as_ref() {
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    // A controller that walks away mid-keystroke would otherwise leave the
    // modifier held on the host.
    if active.controller {
        let _ = active
            .peer
            .send_control(&ControlMessage::Input {
                event: InputEvent::ReleaseAll,
            })
            .await;
    }
    active.peer.close().await;
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ice_config_carries_turn_only_when_one_is_configured() {
        let mut settings = Settings::default();
        assert!(ice_config(&settings).turn.is_none());
        assert!(
            !ice_config(&settings).stun.is_empty(),
            "STUN must always be present or nothing traverses NAT"
        );

        settings.turn.url = "turn:turn.example.com:3478".into();
        assert!(ice_config(&settings).turn.is_some());
    }

    #[test]
    fn an_emitter_drops_updates_instead_of_blocking_a_full_queue() {
        let (tx, rx) = crossbeam_channel::bounded(2);
        let emitter = Emitter {
            tx,
            repaint: Arc::new(|| {}),
        };
        for _ in 0..50 {
            emitter.send(Update::Link(LinkStatus::Ready));
        }
        // Whatever fits is kept; the rest is discarded rather than stalling the
        // transport behind a UI that is not repainting.
        assert_eq!(rx.len(), 2);
    }

    #[test]
    fn odd_capture_dimensions_are_cropped_to_even() {
        // OpenH264 refuses odd sizes and a display can report one, so the
        // capture loop rounds down rather than dropping the session.
        for (raw, want) in [(1921u32, 1920u32), (1080, 1080), (1, 0), (0, 0)] {
            assert_eq!(raw & !1, want);
        }
    }
}
