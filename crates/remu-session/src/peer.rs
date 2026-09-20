//! The peer-to-peer half of a session: data channels, video, ICE.
//!
//! [`PeerSession`] owns one webrtc-rs connection and the two data channels the
//! protocol defines. It does **not** talk to the relay: signalling messages
//! arrive as arguments ([`accept_offer`](PeerSession::accept_offer),
//! [`add_ice`](PeerSession::add_ice)) and leave as
//! [`SessionEvent`]s ([`SessionEvent::LocalIce`]), so the application decides
//! how an offer reaches the far side and the session stays testable without a
//! network.
//!
//! # Roles
//!
//! The controller is always the offerer: it creates both data channels and a
//! receive-only video transceiver. The host answers, receives the channels
//! through `on_data_channel`, and owns the single H.264 track. Mirrors the
//! predecessor's `startAsController` / `acceptAsHost` split, which the relay
//! handshake in `remu_proto::signaling` already assumes.
//!
//! # Threading
//!
//! Every inbound path — data-channel events, RTP, connection state — is pumped
//! by a background Tokio task that publishes into the caller's event channel.
//! [`PeerSession::new`] must therefore be called on a Tokio runtime.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use parking_lot::Mutex;
use tokio::sync::mpsc::{self, error::TrySendError, Sender};
use tokio::task::JoinHandle;
use uuid::Uuid;

use remu_proto::{
    bulk, ControlMessage, IceCandidate, PeerId, SessionDescription, SessionId, SessionStats,
    TurnConfig, BULK_CHANNEL, BULK_CHUNK_BYTES, CONTROL_CHANNEL,
};

use rtc::data_channel::{RTCDataChannelInit, RTCDataChannelMessage, RTCDataChannelState};
use rtc::media::Sample;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::interceptor_registry::register_default_interceptors;
use rtc::peer_connection::configuration::media_engine::{MediaEngine, MIME_TYPE_H264};
use rtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
use rtc::rtp_transceiver::rtp_sender::{
    RTCPFeedback, RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters,
    RTCRtpEncodingParameters, RtpCodecKind,
};
use rtc::rtp_transceiver::{PayloadType, RTCRtpTransceiverDirection, RTCRtpTransceiverInit, SSRC};
use rtc::statistics::StatsSelector;
use webrtc::data_channel::{DataChannel, DataChannelEvent};
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
    RTCIceServer, RTCPeerConnectionIceEvent, RTCPeerConnectionState, Registry,
};
use webrtc::rtp_transceiver::RtpSender;

use crate::error::SessionError;
use crate::sdp::{from_webrtc_ice, from_webrtc_sdp, to_webrtc_ice, to_webrtc_sdp};
use crate::video::H264Assembler;

/// Payload type offered for H.264.
///
/// Only one codec is registered, so negotiation cannot pick anything else and
/// the controller's depacketizer never has to ask what it is looking at. The
/// number is still read back from the answer before sending
/// (see [`PeerSession::write_video`]) rather than assumed.
const H264_PAYLOAD_TYPE: PayloadType = 102;

/// Sockets to bind for ICE. The wildcard is expanded by webrtc-rs into one
/// socket per real interface, which is what produces dialable host candidates.
const BIND_ADDR: &str = "0.0.0.0:0";

/// Per-channel cap on bytes handed to SCTP but not yet released.
///
/// Turns [`DataChannel::send`] from fire-and-forget into a blocking send, so a
/// file transfer that ignores [`PeerSession::buffered_bulk_bytes`] still cannot
/// grow the send queue without bound. Chromium uses 16 MiB; half that is well
/// clear of the ~1 MiB SCTP window while keeping the worst-case memory of two
/// channels modest.
const SEND_BUFFER_LIMIT: usize = 8 * 1024 * 1024;

/// How often the outstanding bulk-send bytes are resampled.
///
/// [`DataChannel::outstanding_bytes`] is async because it round-trips to the
/// connection driver, but the transfer loop and the UI need a number they can
/// read without awaiting, so one task caches it. At 16 KiB per chunk this is
/// fine-grained enough that a sender pacing on a 1 MiB high-water mark cannot
/// overshoot by more than a few chunks.
const BUFFER_SAMPLE_INTERVAL: Duration = Duration::from_millis(20);

/// Minimum gap between keyframe requests, so a burst of loss produces one PLI
/// rather than one per damaged frame.
const PLI_INTERVAL: Duration = Duration::from_secs(1);

/// Events the session will hold for an application that is not draining
/// them.
///
/// The queue used to be unbounded, which made a stalled UI a memory leak: at
/// 60 fps of 1080p plus a file transfer it grew by tens of megabytes a second,
/// and nothing upstream limits it — the relay's message cap and rate limiter
/// do not sit on this path. 256 is several seconds of control traffic and a
/// few frames of video, which is as much as a UI that has stopped drawing can
/// usefully catch up on.
pub const EVENT_CHANNEL_CAPACITY: usize = 256;

/// The event channel [`PeerSession::new`] expects.
///
/// A helper rather than a `Default`, so the capacity — which is part of the
/// back-pressure policy documented on [`Shared::emit`] — is chosen in one
/// place.
pub fn event_channel() -> (Sender<SessionEvent>, mpsc::Receiver<SessionEvent>) {
    mpsc::channel(EVENT_CHANNEL_CAPACITY)
}

const CONTROL_OPEN: u8 = 0b01;
const BULK_OPEN: u8 = 0b10;
const BOTH_OPEN: u8 = CONTROL_OPEN | BULK_OPEN;

/// Which end of the session this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Offers, drives input, receives the screen.
    Controller,
    /// Answers, shares the screen, applies input.
    Host,
}

/// ICE servers to gather candidates from.
#[derive(Debug, Clone)]
pub struct IceConfig {
    /// STUN URLs, tried in order.
    pub stun: Vec<String>,
    /// A relay of last resort, for peers behind symmetric NAT.
    pub turn: Option<TurnConfig>,
}

impl Default for IceConfig {
    /// Google's public STUN servers and no TURN.
    ///
    /// Two of them, not one: a single unreachable STUN server means no
    /// server-reflexive candidate at all, and the predecessor shipped the same
    /// pair for exactly that reason.
    fn default() -> Self {
        Self {
            stun: vec![
                "stun:stun.l.google.com:19302".to_string(),
                "stun:stun1.l.google.com:19302".to_string(),
            ],
            turn: None,
        }
    }
}

impl IceConfig {
    /// ICE servers in the form webrtc-rs configures.
    ///
    /// A TURN entry whose URL is blank is dropped rather than passed on: an
    /// empty URL fails configuration validation and would take the working STUN
    /// servers down with it.
    fn to_servers(&self) -> Vec<RTCIceServer> {
        let mut servers = Vec::with_capacity(self.stun.len() + 1);
        if !self.stun.is_empty() {
            servers.push(RTCIceServer {
                urls: self.stun.clone(),
                ..Default::default()
            });
        }
        if let Some(turn) = self.turn.as_ref().filter(|t| t.is_configured()) {
            servers.push(RTCIceServer {
                urls: vec![turn.url.clone()],
                username: turn.username.clone(),
                credential: turn.credential.clone(),
            });
        }
        servers
    }
}

/// Where the connection is, in Remu's own terms.
///
/// Not webrtc-rs's enum: the application should not have to depend on the
/// media stack to draw a status line, and webrtc-rs has an `Unspecified`
/// variant that means nothing to a user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    New,
    Connecting,
    Connected,
    Disconnected,
    Failed,
    Closed,
}

impl ConnState {
    fn describe(self) -> &'static str {
        match self {
            ConnState::New => "new",
            ConnState::Connecting => "connecting",
            ConnState::Connected => "connected",
            ConnState::Disconnected => "disconnected",
            ConnState::Failed => "failed",
            ConnState::Closed => "closed",
        }
    }
}

impl From<RTCPeerConnectionState> for ConnState {
    fn from(state: RTCPeerConnectionState) -> Self {
        match state {
            // `Unspecified` is webrtc-rs's default-constructed placeholder; no
            // transport has reported anything yet, which is exactly `New`.
            RTCPeerConnectionState::Unspecified | RTCPeerConnectionState::New => ConnState::New,
            RTCPeerConnectionState::Connecting => ConnState::Connecting,
            RTCPeerConnectionState::Connected => ConnState::Connected,
            RTCPeerConnectionState::Disconnected => ConnState::Disconnected,
            RTCPeerConnectionState::Failed => ConnState::Failed,
            RTCPeerConnectionState::Closed => ConnState::Closed,
            // The enum is `#[non_exhaustive]`: a state added in a future
            // webrtc-rs is reported as the pre-connection state rather than
            // guessed at, so the UI never claims a link that is not there.
            other => {
                tracing::debug!(?other, "unrecognized webrtc connection state");
                ConnState::New
            }
        }
    }
}

/// Everything the session reports to the application.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    ConnectionState(ConnState),
    /// Both data channels are open; the session is usable.
    ChannelsOpen,
    /// A locally gathered candidate. The caller relays it to the peer.
    LocalIce(IceCandidate),
    Control(ControlMessage),
    /// One file chunk off the bulk channel, already unframed.
    BulkChunk {
        transfer_id: Uuid,
        seq: u64,
        payload: Bytes,
    },
    /// A reassembled Annex-B access unit. Controller side only.
    VideoFrame {
        data: Bytes,
        timestamp_us: u64,
    },
    /// Terminal. Emitted exactly once.
    Closed {
        reason: String,
    },
}

/// One peer-to-peer session.
pub struct PeerSession {
    session_id: SessionId,
    role: Role,
    remote: PeerId,
    pc: Arc<dyn PeerConnection>,
    shared: Arc<Shared>,
    /// `Some` on the host only.
    video: Option<VideoTrack>,
    /// Last stats sample, for turning a byte counter into a rate.
    last_sample: Mutex<Option<(Instant, u64)>>,
    /// Guards the teardown in [`PeerSession::close`], separately from
    /// `shared.closed`: the connection may already have failed on its own (which
    /// sets `closed` from the event handler) and still need tearing down here.
    torn_down: AtomicBool,
}

impl std::fmt::Debug for PeerSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // webrtc-rs's handles are trait objects with no `Debug`, so the useful
        // fields are listed by hand rather than derived.
        f.debug_struct("PeerSession")
            .field("session_id", &self.session_id)
            .field("role", &self.role)
            .field("remote", &self.remote)
            .field("video", &self.video)
            .field("closed", &self.shared.closed.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

/// The host's outgoing screen track.
struct VideoTrack {
    track: Arc<TrackLocalStaticSample>,
    sender: Arc<dyn RtpSender>,
    ssrc: SSRC,
    /// Resolved from the answer on first write, then cached.
    payload_type: Mutex<Option<PayloadType>>,
}

impl std::fmt::Debug for VideoTrack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoTrack")
            .field("ssrc", &self.ssrc)
            .field("payload_type", &*self.payload_type.lock())
            .finish_non_exhaustive()
    }
}

/// State the event handler and the session both reach.
///
/// Separate from [`PeerSession`] because webrtc-rs wants the handler at build
/// time, before the connection — and therefore the session — exists.
struct Shared {
    events: Sender<SessionEvent>,
    control: Mutex<Option<Arc<dyn DataChannel>>>,
    bulk: Mutex<Option<Arc<dyn DataChannel>>>,
    channels_open: AtomicU8,
    /// Candidates that arrived before the remote description; see
    /// [`PeerSession::add_ice`].
    pending_ice: Mutex<Vec<IceCandidate>>,
    remote_described: AtomicBool,
    closed: AtomicBool,
    bulk_buffered: AtomicUsize,
    /// Video frames shed because the application was not draining its events;
    /// surfaced through [`PeerSession::stats`] so a stalled UI is visible
    /// rather than silent.
    dropped_frames: AtomicU64,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Shared {
    fn new(events: Sender<SessionEvent>) -> Self {
        Self {
            events,
            control: Mutex::new(None),
            bulk: Mutex::new(None),
            channels_open: AtomicU8::new(0),
            pending_ice: Mutex::new(Vec::new()),
            remote_described: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            bulk_buffered: AtomicUsize::new(0),
            dropped_frames: AtomicU64::new(0),
            tasks: Mutex::new(Vec::new()),
        }
    }

    /// Publishes an event under the session's back-pressure policy.
    ///
    /// The queue is bounded ([`EVENT_CHANNEL_CAPACITY`]), so something has to
    /// give when the application stops draining it. Video is the lossy class:
    /// a frame that does not fit is dropped and counted into
    /// [`SessionStats::dropped_frames`], because a decoder recovers from a
    /// missing frame and the alternative is unbounded memory. Everything else —
    /// control messages, bulk chunks, ICE, state changes — is carried: this
    /// awaits room, which back-pressures the one pump task that produced the
    /// event rather than the transport as a whole, and never the video path.
    ///
    /// [`SessionEvent::Closed`] is the exception to the exception: it is
    /// terminal and must arrive, but [`PeerSession::close`] may be called from
    /// the very task that drains the channel, so waiting here could deadlock a
    /// caller against itself. It is handed to a detached task instead — which
    /// on a full queue is the one case where an event can reach the
    /// application behind it.
    async fn emit(&self, event: SessionEvent) {
        match event {
            SessionEvent::VideoFrame { .. } => match self.events.try_send(event) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    let dropped = self.dropped_frames.fetch_add(1, Ordering::Relaxed) + 1;
                    tracing::debug!(dropped, "event queue full; shedding a video frame");
                }
                Err(TrySendError::Closed(_)) => {
                    tracing::debug!("session event receiver is gone; dropping event");
                }
            },
            SessionEvent::Closed { .. } => match self.events.try_send(event) {
                Ok(()) | Err(TrySendError::Closed(_)) => {}
                Err(TrySendError::Full(event)) => {
                    let events = self.events.clone();
                    tokio::spawn(async move {
                        let _ = events.send(event).await;
                    });
                }
            },
            // Control and lifecycle events must not be dropped, but they must
            // not block either: `emit` is reached from `on_ice_candidate` and
            // `on_connection_state_change`, which webrtc awaits *inline* in the
            // same select! loop that services sockets and the ICE/DTLS/SCTP
            // timers. Awaiting a full queue there froze the entire transport
            // whenever the application stopped draining. Overflow is handed to
            // a task instead, so the handler always returns promptly.
            //
            // The cost is ordering: a deferred event can arrive after one sent
            // later. That only happens once the queue is full, which means
            // video has already been shed and the consumer is badly stuck — a
            // state where liveness matters far more than order.
            other => match self.events.try_send(other) {
                Ok(()) | Err(TrySendError::Closed(_)) => {}
                Err(TrySendError::Full(event)) => {
                    let events = self.events.clone();
                    tokio::spawn(async move {
                        let _ = events.send(event).await;
                    });
                }
            },
        }
    }

    fn spawn(self: &Arc<Self>, task: JoinHandle<()>) {
        self.tasks.lock().push(task);
    }

    /// Records a channel and starts pumping its events.
    fn attach_channel(self: &Arc<Self>, label: &str, channel: Arc<dyn DataChannel>) {
        let mask = match label {
            CONTROL_CHANNEL => CONTROL_OPEN,
            BULK_CHANNEL => BULK_OPEN,
            // A peer speaking our protocol version opens exactly two channels.
            // Anything else is ignored rather than guessed at.
            other => {
                tracing::warn!(label = other, "ignoring an unexpected data channel");
                return;
            }
        };

        let slot = if mask == CONTROL_OPEN {
            &self.control
        } else {
            &self.bulk
        };
        *slot.lock() = Some(Arc::clone(&channel));

        let shared = Arc::clone(self);
        self.spawn(tokio::spawn(async move {
            shared.pump_channel(mask, channel).await;
        }));
    }

    async fn pump_channel(self: Arc<Self>, mask: u8, channel: Arc<dyn DataChannel>) {
        while let Some(event) = channel.poll().await {
            match event {
                DataChannelEvent::OnOpen => self.note_open(mask).await,
                DataChannelEvent::OnMessage(message) => self.on_message(mask, message).await,
                DataChannelEvent::OnClose => break,
                _ => {}
            }
        }
    }

    async fn note_open(&self, mask: u8) {
        let before = self.channels_open.fetch_or(mask, Ordering::AcqRel);
        if before != BOTH_OPEN && before | mask == BOTH_OPEN {
            self.emit(SessionEvent::ChannelsOpen).await;
        }
    }

    /// Decodes one channel message. A message that will not decode is dropped
    /// with a log line: the peer is not trusted, and one bad frame must not end
    /// a session that is otherwise healthy.
    async fn on_message(&self, mask: u8, message: RTCDataChannelMessage) {
        if mask == CONTROL_OPEN {
            match serde_json::from_slice::<ControlMessage>(&message.data) {
                Ok(control) => self.emit(SessionEvent::Control(control)).await,
                Err(err) => tracing::warn!(%err, "dropping an undecodable control message"),
            }
            return;
        }

        match bulk::decode_chunk(&message.data) {
            Ok(chunk) => {
                self.emit(SessionEvent::BulkChunk {
                    transfer_id: chunk.transfer_id,
                    seq: chunk.seq,
                    payload: Bytes::copy_from_slice(chunk.payload),
                })
                .await
            }
            Err(err) => tracing::warn!(%err, "dropping an undecodable bulk frame"),
        }
    }

    /// Reassembles the host's screen and asks for a keyframe after loss.
    async fn pump_video(self: Arc<Self>, track: Arc<dyn TrackRemote>) {
        let mut assembler = H264Assembler::default();
        let mut last_pli: Option<Instant> = None;

        while let Some(event) = track.poll().await {
            let TrackRemoteEvent::OnRtpPacket(packet) = event else {
                continue;
            };

            if let Some(frame) = assembler.push(&packet) {
                self.emit(SessionEvent::VideoFrame {
                    data: frame.data,
                    timestamp_us: frame.timestamp_us,
                })
                .await;
            }

            if !assembler.lost_data() {
                continue;
            }
            let now = Instant::now();
            if last_pli.is_some_and(|at| now.duration_since(at) < PLI_INTERVAL) {
                continue;
            }
            last_pli = Some(now);
            assembler.clear_loss();
            // The packet's own SSRC, not the track's: it is the stream that
            // actually lost data, and it is correct even mid-SSRC-change.
            let pli = PictureLossIndication {
                sender_ssrc: 0,
                media_ssrc: packet.header.ssrc,
            };
            if let Err(err) = track.write_rtcp(vec![Box::new(pli)]).await {
                tracing::debug!(%err, "could not send a keyframe request");
            }
        }
    }

    /// Marks the session finished and emits [`SessionEvent::Closed`] — once,
    /// whichever of the local close and the remote failure gets here first.
    async fn finish(&self, reason: String) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.emit(SessionEvent::Closed { reason }).await;
    }
}

#[derive(Clone)]
struct Handler {
    shared: Arc<Shared>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        match event.candidate.to_json() {
            Ok(init) => {
                self.shared
                    .emit(SessionEvent::LocalIce(from_webrtc_ice(&init)))
                    .await
            }
            Err(err) => tracing::warn!(%err, "could not serialize a local ICE candidate"),
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        // `Closed` is documented as the last event of a session, and tearing
        // the connection down locally makes the stack report `Closed` here
        // afterwards — which used to be published behind it.
        if self.shared.closed.load(Ordering::Acquire) {
            return;
        }
        let state = ConnState::from(state);
        self.shared.emit(SessionEvent::ConnectionState(state)).await;
        if matches!(state, ConnState::Failed | ConnState::Closed) {
            self.shared
                .finish(format!("connection {}", state.describe()))
                .await;
        }
    }

    async fn on_data_channel(&self, channel: Arc<dyn DataChannel>) {
        match channel.label().await {
            Ok(label) => self.shared.attach_channel(&label, channel),
            Err(err) => tracing::warn!(%err, "a remote data channel vanished before it was named"),
        }
    }

    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        if track.kind().await != RtpCodecKind::Video {
            tracing::warn!("ignoring a non-video remote track");
            return;
        }
        let shared = Arc::clone(&self.shared);
        self.shared.spawn(tokio::spawn(async move {
            shared.pump_video(track).await;
        }));
    }
}

impl PeerSession {
    /// Builds the connection, its channels and — on the host — its video track.
    ///
    /// Returns as soon as the local sockets are bound; nothing has been
    /// negotiated yet. Drive the handshake with
    /// [`create_offer`](Self::create_offer) or
    /// [`accept_offer`](Self::accept_offer).
    pub async fn new(
        session_id: SessionId,
        role: Role,
        remote: PeerId,
        ice: IceConfig,
        events: Sender<SessionEvent>,
    ) -> Result<Self, SessionError> {
        let shared = Arc::new(Shared::new(events));

        let mut media_engine = MediaEngine::default();
        media_engine.register_codec(h264_codec(), RtpCodecKind::Video)?;
        // Brings in the NACK responder, the receiver/sender reports the stats
        // report is built from, and TWCC. Without it `stats()` has nothing to
        // read and lost packets are never retransmitted.
        let registry = register_default_interceptors(Registry::new(), &mut media_engine)?;

        let configuration = RTCConfigurationBuilder::new()
            .with_ice_servers(ice.to_servers())
            .build();

        let pc = PeerConnectionBuilder::new()
            .with_configuration(configuration)
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .with_handler(Arc::new(Handler {
                shared: Arc::clone(&shared),
            }))
            .with_udp_addrs(vec![BIND_ADDR.to_string()])
            .with_data_channel_send_buffer_limit(SEND_BUFFER_LIMIT)
            .build()
            .await?;
        let pc: Arc<dyn PeerConnection> = Arc::new(pc);

        let video = match role {
            Role::Controller => {
                // Created here, before the offer, so both channels appear in it
                // and the host learns about them from `on_data_channel`.
                let control = pc
                    .create_data_channel(CONTROL_CHANNEL, Some(reliable_channel()))
                    .await?;
                shared.attach_channel(CONTROL_CHANNEL, control);
                let bulk = pc
                    .create_data_channel(BULK_CHANNEL, Some(reliable_channel()))
                    .await?;
                shared.attach_channel(BULK_CHANNEL, bulk);

                pc.add_transceiver_from_kind(
                    RtpCodecKind::Video,
                    Some(RTCRtpTransceiverInit {
                        direction: RTCRtpTransceiverDirection::Recvonly,
                        ..Default::default()
                    }),
                )
                .await?;
                None
            }
            Role::Host => {
                let ssrc: SSRC = rand::random();
                let track = Arc::new(TrackLocalStaticSample::new(
                    Instant::now(),
                    screen_track(ssrc),
                )?);
                let sender = pc
                    .add_track(Arc::clone(&track) as Arc<dyn TrackLocal>)
                    .await?;
                Some(VideoTrack {
                    track,
                    sender,
                    ssrc,
                    payload_type: Mutex::new(None),
                })
            }
        };

        let sampler = Arc::clone(&shared);
        shared.spawn(tokio::spawn(
            async move { sample_bulk_buffer(sampler).await },
        ));

        Ok(Self {
            session_id,
            role,
            remote,
            pc,
            shared,
            video,
            last_sample: Mutex::new(None),
            torn_down: AtomicBool::new(false),
        })
    }

    /// Creates the offer and applies it locally. Controller side.
    pub async fn create_offer(&self) -> Result<SessionDescription, SessionError> {
        let offer = self.pc.create_offer(None).await?;
        self.pc.set_local_description(offer.clone()).await?;
        from_webrtc_sdp(&offer)
    }

    /// Applies the controller's offer and answers it. Host side.
    pub async fn accept_offer(
        &self,
        sdp: SessionDescription,
    ) -> Result<SessionDescription, SessionError> {
        self.pc.set_remote_description(to_webrtc_sdp(&sdp)?).await?;
        self.forget_negotiated_payload_type();
        self.drain_pending_ice().await;

        let answer = self.pc.create_answer(None).await?;
        self.pc.set_local_description(answer.clone()).await?;
        from_webrtc_sdp(&answer)
    }

    /// Applies the host's answer. Controller side.
    pub async fn accept_answer(&self, sdp: SessionDescription) -> Result<(), SessionError> {
        self.pc.set_remote_description(to_webrtc_sdp(&sdp)?).await?;
        self.forget_negotiated_payload_type();
        self.drain_pending_ice().await;
        Ok(())
    }

    /// Adds a candidate the peer trickled over the relay.
    ///
    /// Candidates routinely arrive before the SDP that gives them a media
    /// section to attach to — the relay forwards whatever the peer sends, in
    /// order, and the peer starts gathering the moment it sets its local
    /// description. webrtc-rs refuses a candidate with no remote description,
    /// so early ones are held and replayed by
    /// [`accept_offer`](Self::accept_offer) or
    /// [`accept_answer`](Self::accept_answer). The predecessor learned this the
    /// same way.
    pub async fn add_ice(&self, c: IceCandidate) -> Result<(), SessionError> {
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(SessionError::Closed);
        }
        if !self.shared.remote_described.load(Ordering::Acquire) {
            self.shared.pending_ice.lock().push(c);
            return Ok(());
        }
        self.pc.add_ice_candidate(to_webrtc_ice(&c)).await?;
        Ok(())
    }

    /// Drops the payload type cached from the last answer.
    ///
    /// The number is read out of the negotiated parameters once and then
    /// reused for every sample. A renegotiation may hand the same codec a
    /// different payload type, and packets stamped with the old one are
    /// discarded by the far side as an unknown format, so the cache is
    /// invalidated whenever a new remote description is applied.
    fn forget_negotiated_payload_type(&self) {
        if let Some(video) = self.video.as_ref() {
            *video.payload_type.lock() = None;
        }
    }

    async fn drain_pending_ice(&self) {
        self.shared.remote_described.store(true, Ordering::Release);
        let pending = std::mem::take(&mut *self.shared.pending_ice.lock());
        for candidate in pending {
            // A buffered candidate can be stale by the time it is applied —
            // the peer may have restarted ICE. Logged, not fatal.
            if let Err(err) = self.pc.add_ice_candidate(to_webrtc_ice(&candidate)).await {
                tracing::debug!(%err, "a buffered ICE candidate was refused");
            }
        }
    }

    /// Sends one control message as JSON text.
    pub async fn send_control(&self, msg: &ControlMessage) -> Result<(), SessionError> {
        let json = serde_json::to_string(msg)?;
        let channel = self.open_channel(CONTROL_CHANNEL).await?;
        channel
            .send_text(&json)
            .await
            .map_err(|err| channel_error(CONTROL_CHANNEL, err))
    }

    /// Sends one framed file chunk on the bulk channel.
    ///
    /// Blocks while the channel's send buffer is over
    /// [`SEND_BUFFER_LIMIT`]; see [`buffered_bulk_bytes`](Self::buffered_bulk_bytes)
    /// for pacing that does not block.
    pub async fn send_bulk(
        &self,
        transfer_id: Uuid,
        seq: u64,
        payload: &[u8],
    ) -> Result<(), SessionError> {
        if payload.len() > BULK_CHUNK_BYTES {
            return Err(SessionError::BulkPayloadTooLarge {
                len: payload.len(),
                max: BULK_CHUNK_BYTES,
            });
        }
        let channel = self.open_channel(BULK_CHANNEL).await?;
        let frame = bulk::encode_chunk(transfer_id, seq, payload);
        channel
            .send(BytesMut::from(&frame[..]))
            .await
            .map_err(|err| channel_error(BULK_CHANNEL, err))?;

        // Refreshed here as well as by the sampler task so that a sender
        // checking the mark immediately after a send sees this send.
        self.shared.bulk_buffered.store(
            channel.outstanding_bytes().await.unwrap_or(0),
            Ordering::Relaxed,
        );
        Ok(())
    }

    /// Packetizes one Annex-B access unit onto the screen track. Host side.
    ///
    /// `duration` is the frame's own presentation duration; it becomes the RTP
    /// timestamp step, so passing a constant on a variable-rate capture makes
    /// the remote playback drift.
    pub async fn write_video(&self, annexb: &[u8], duration: Duration) -> Result<(), SessionError> {
        let video = self.video.as_ref().ok_or(SessionError::NotSendingVideo)?;
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(SessionError::Closed);
        }

        // Copied out rather than matched in place: the guard must not be held
        // across the negotiation round trip below.
        let cached = *video.payload_type.lock();
        let payload_type = match cached {
            Some(payload_type) => payload_type,
            None => {
                // A sender reports the codec it was configured with even
                // before an answer exists, so the parameters alone cannot say
                // whether anything was negotiated. The remote description is
                // what settles it — and without one the packets would be
                // packetized and then dropped by an unbound track.
                if self.pc.current_remote_description().await.is_none() {
                    return Err(SessionError::VideoNotNegotiated);
                }
                let negotiated = video
                    .sender
                    .get_parameters()
                    .await?
                    .rtp_parameters
                    .codecs
                    .first()
                    .map(|codec| codec.payload_type)
                    .ok_or(SessionError::VideoNotNegotiated)?;
                *video.payload_type.lock() = Some(negotiated);
                negotiated
            }
        };

        video
            .track
            .sample_writer(video.ssrc, payload_type)
            .write_sample(&Sample {
                data: Bytes::copy_from_slice(annexb),
                duration,
                ..Sample::new(Instant::now())
            })
            .await?;
        Ok(())
    }

    /// Bytes handed to the bulk channel that SCTP has not released yet.
    ///
    /// Sampled every [`BUFFER_SAMPLE_INTERVAL`], so it lags reality slightly —
    /// it is a pacing signal, not an accounting figure.
    pub fn buffered_bulk_bytes(&self) -> usize {
        self.shared.bulk_buffered.load(Ordering::Relaxed)
    }

    /// Link telemetry for the status line, or `None` before anything has been
    /// measured.
    ///
    /// `bitrate_kbps` is a rate, so it is derived from the byte counter's
    /// movement since the previous call: the first call after connecting
    /// reports zero because there is no interval to divide by yet.
    pub async fn stats(&self) -> Option<SessionStats> {
        let report = self.pc.get_stats(Instant::now(), StatsSelector::None).await;
        if report.is_empty() {
            return None;
        }

        let pair = report
            .candidate_pairs()
            .find(|pair| pair.nominated)
            .or_else(|| report.candidate_pairs().next());
        let rtt_ms = pair
            .map(|pair| (pair.current_round_trip_time * 1000.0).max(0.0) as u32)
            .unwrap_or(0);

        let (fps, bytes, dropped_frames) = match self.role {
            Role::Host => match report.outbound_rtp_streams().next() {
                Some(out) => (
                    out.frames_per_second as f32,
                    out.sent_rtp_stream_stats.bytes_sent,
                    // Frames the encoder produced that never reached the wire.
                    out.frames_encoded.saturating_sub(out.frames_sent),
                ),
                None => (0.0, 0, 0),
            },
            Role::Controller => match report.inbound_rtp_streams().next() {
                Some(inbound) => (
                    inbound.frames_per_second as f32,
                    inbound.bytes_received,
                    inbound.frames_dropped,
                ),
                None => (0.0, 0, 0),
            },
        };

        let now = Instant::now();
        let bitrate_kbps = {
            let mut last = self.last_sample.lock();
            let rate = match *last {
                Some((then, before)) if bytes >= before => {
                    let seconds = now.duration_since(then).as_secs_f64();
                    if seconds > 0.0 {
                        ((bytes - before) as f64 * 8.0 / seconds / 1000.0) as u32
                    } else {
                        0
                    }
                }
                // A counter that went backwards means the stream was replaced;
                // restart the measurement rather than report a negative rate.
                _ => 0,
            };
            *last = Some((now, bytes));
            rate
        };

        // Frames the transport lost and frames the application was too slow
        // to accept are the same thing to a user watching a jerky screen.
        let shed = self.shared.dropped_frames.load(Ordering::Relaxed);
        let dropped_frames = dropped_frames.saturating_add(shed.min(u32::MAX as u64) as u32);

        Some(SessionStats {
            fps,
            bitrate_kbps,
            rtt_ms,
            dropped_frames,
        })
    }

    /// Tears the session down. Idempotent, and safe to call after the
    /// connection has already failed on its own.
    pub async fn close(&self) {
        self.shared.finish("closed locally".to_string()).await;
        if self.torn_down.swap(true, Ordering::AcqRel) {
            return;
        }

        for task in self.shared.tasks.lock().drain(..) {
            task.abort();
        }
        let channels = [
            self.shared.control.lock().clone(),
            self.shared.bulk.lock().clone(),
        ];
        for channel in channels.into_iter().flatten() {
            if let Err(err) = channel.close().await {
                tracing::debug!(%err, "a data channel refused to close");
            }
        }
        if let Err(err) = self.pc.close().await {
            tracing::debug!(%err, "the peer connection refused to close");
        }
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// The peer this session is with, for addressing relay messages.
    pub fn remote(&self) -> PeerId {
        self.remote
    }

    /// The named channel, if it exists and is open.
    async fn open_channel(
        &self,
        label: &'static str,
    ) -> Result<Arc<dyn DataChannel>, SessionError> {
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(SessionError::Closed);
        }
        let slot = if label == CONTROL_CHANNEL {
            self.shared.control.lock().clone()
        } else {
            self.shared.bulk.lock().clone()
        };
        let channel = slot.ok_or_else(|| SessionError::ChannelNotOpen {
            label,
            state: "not negotiated".to_string(),
        })?;

        match channel.ready_state().await {
            Ok(RTCDataChannelState::Open) => Ok(channel),
            Ok(other) => Err(SessionError::ChannelNotOpen {
                label,
                state: format!("{other:?}").to_lowercase(),
            }),
            // The only way the accessor fails is the channel being gone.
            Err(_) => Err(SessionError::ChannelNotOpen {
                label,
                state: "closed".to_string(),
            }),
        }
    }
}

impl Drop for PeerSession {
    fn drop(&mut self) {
        // A dropped session must not leave its pumps running: they hold the
        // channels alive and would keep publishing into an event receiver the
        // application has already stopped reading.
        for task in self.shared.tasks.lock().drain(..) {
            task.abort();
        }
    }
}

/// Maps a send failure onto the typed "channel is not usable" error, so a
/// caller racing a close gets the same error it would get by checking first.
fn channel_error(label: &'static str, err: webrtc::error::Error) -> SessionError {
    match err {
        webrtc::error::Error::ErrDataChannelClosed => SessionError::ChannelNotOpen {
            label,
            state: "closed".to_string(),
        },
        other => SessionError::WebRtc(other),
    }
}

/// Ordered and reliable, for both channels: input events applied out of order
/// would move the remote cursor backwards, and a missing file chunk would
/// corrupt the file.
fn reliable_channel() -> RTCDataChannelInit {
    RTCDataChannelInit {
        ordered: true,
        max_packet_life_time: None,
        max_retransmits: None,
        ..Default::default()
    }
}

/// The single codec this build offers.
fn h264_codec() -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: h264_rtp_codec(),
        payload_type: H264_PAYLOAD_TYPE,
    }
}

fn h264_rtp_codec() -> RTCRtpCodec {
    RTCRtpCodec {
        mime_type: MIME_TYPE_H264.to_owned(),
        clock_rate: 90_000,
        channels: 0,
        // packetization-mode=1 is what makes FU-A fragmentation legal, which a
        // full-screen keyframe needs; the profile is constrained baseline,
        // which is what the openh264 encoder in `remu-codec` produces.
        sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"
            .to_owned(),
        rtcp_feedback: vec![
            RTCPFeedback {
                typ: "nack".to_owned(),
                parameter: String::new(),
            },
            // Lets the controller ask for a keyframe after loss instead of
            // waiting out the encoder's own keyframe interval.
            RTCPFeedback {
                typ: "nack".to_owned(),
                parameter: "pli".to_owned(),
            },
        ],
    }
}

fn screen_track(ssrc: SSRC) -> MediaStreamTrack {
    MediaStreamTrack::new(
        "remu-screen".to_owned(),
        "remu-screen-video".to_owned(),
        "Remote screen".to_owned(),
        RtpCodecKind::Video,
        vec![RTCRtpEncodingParameters {
            rtp_coding_parameters: RTCRtpCodingParameters {
                ssrc: Some(ssrc),
                ..Default::default()
            },
            active: true,
            codec: h264_rtp_codec(),
            ..Default::default()
        }],
    )
}

/// Keeps [`PeerSession::buffered_bulk_bytes`] fresh while the session lives.
async fn sample_bulk_buffer(shared: Arc<Shared>) {
    loop {
        tokio::time::sleep(BUFFER_SAMPLE_INTERVAL).await;
        if shared.closed.load(Ordering::Acquire) {
            return;
        }
        let channel = shared.bulk.lock().clone();
        let outstanding = match channel {
            Some(channel) => channel.outstanding_bytes().await.unwrap_or(0),
            None => 0,
        };
        shared.bulk_buffered.store(outstanding, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use remu_proto::{HostPermissions, SdpKind};
    use tokio::sync::mpsc::{self, Receiver};

    fn peer(raw: u32) -> PeerId {
        PeerId::new(raw).expect("nine-digit test id")
    }

    /// No STUN: every test here is either offline or on loopback, and reaching
    /// out to Google would make them slow and network-dependent.
    fn local_ice() -> IceConfig {
        IceConfig {
            stun: vec![],
            turn: None,
        }
    }

    async fn session(role: Role) -> (PeerSession, Receiver<SessionEvent>) {
        let (tx, rx) = event_channel();
        let session = PeerSession::new(
            SessionId::random(),
            role,
            peer(123_456_789),
            local_ice(),
            tx,
        )
        .await
        .expect("a peer connection binds on an ephemeral local port");
        (session, rx)
    }

    #[test]
    fn the_default_ice_config_offers_two_public_stun_servers() {
        let servers = IceConfig::default().to_servers();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].urls.len(), 2);
        assert!(servers[0].urls.iter().all(|url| url.starts_with("stun:")));
        assert!(servers[0].username.is_empty());
    }

    #[test]
    fn a_configured_turn_server_carries_its_credentials() {
        let config = IceConfig {
            stun: vec!["stun:example.test:3478".into()],
            turn: Some(TurnConfig {
                url: "turn:relay.example.test:3478".into(),
                username: "desk".into(),
                credential: "secret".into(),
            }),
        };
        let servers = config.to_servers();
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[1].urls, vec!["turn:relay.example.test:3478"]);
        assert_eq!(servers[1].username, "desk");
        assert_eq!(servers[1].credential, "secret");
    }

    /// An unset TURN entry is all-empty, and an empty URL fails webrtc-rs's
    /// configuration validation — it must not reach the builder.
    #[test]
    fn an_unconfigured_turn_server_is_left_out_entirely() {
        let config = IceConfig {
            stun: vec!["stun:example.test:3478".into()],
            turn: Some(TurnConfig::default()),
        };
        assert_eq!(config.to_servers().len(), 1);
    }

    #[test]
    fn webrtc_connection_states_map_onto_remu_states() {
        assert_eq!(
            ConnState::from(RTCPeerConnectionState::Unspecified),
            ConnState::New
        );
        assert_eq!(
            ConnState::from(RTCPeerConnectionState::Connected),
            ConnState::Connected
        );
        assert_eq!(
            ConnState::from(RTCPeerConnectionState::Failed),
            ConnState::Failed
        );
    }

    #[tokio::test]
    async fn a_controller_offers_both_data_channels_and_a_video_section() {
        let (controller, _events) = session(Role::Controller).await;
        let offer = controller.create_offer().await.expect("offer");
        assert_eq!(offer.kind, SdpKind::Offer);
        assert!(offer.sdp.contains("m=application"), "{}", offer.sdp);
        assert!(offer.sdp.contains("m=video"), "{}", offer.sdp);
        assert!(offer.sdp.contains("a=recvonly"), "{}", offer.sdp);
        controller.close().await;
    }

    #[tokio::test]
    async fn the_host_answers_the_offer_with_a_sending_h264_track() {
        let (controller, _c_events) = session(Role::Controller).await;
        let (host, _h_events) = session(Role::Host).await;

        let offer = controller.create_offer().await.expect("offer");
        let answer = host.accept_offer(offer).await.expect("answer");

        assert_eq!(answer.kind, SdpKind::Answer);
        assert!(answer.sdp.contains("H264"), "{}", answer.sdp);
        assert!(answer.sdp.contains("a=sendonly"), "{}", answer.sdp);
        controller
            .accept_answer(answer)
            .await
            .expect("apply answer");

        controller.close().await;
        host.close().await;
    }

    /// The bug the predecessor had to fix: candidates outrun the SDP, and
    /// webrtc-rs refuses a candidate before the remote description exists.
    #[tokio::test]
    async fn ice_that_arrives_before_the_remote_description_is_replayed_after_it() {
        let (controller, _c_events) = session(Role::Controller).await;
        let (host, _h_events) = session(Role::Host).await;

        let early = IceCandidate {
            candidate: "candidate:1 1 udp 2130706431 192.0.2.10 54321 typ host".into(),
            sdp_mid: Some("0".into()),
            sdp_mline_index: Some(0),
            username_fragment: None,
        };
        controller
            .add_ice(early)
            .await
            .expect("buffered, not applied");
        assert_eq!(
            controller.shared.pending_ice.lock().len(),
            1,
            "the candidate should be held, not handed to webrtc-rs"
        );

        let offer = controller.create_offer().await.expect("offer");
        let answer = host.accept_offer(offer).await.expect("answer");
        controller
            .accept_answer(answer)
            .await
            .expect("apply answer");

        assert!(
            controller.shared.pending_ice.lock().is_empty(),
            "the buffer should have been drained by the answer"
        );

        // And from now on candidates go straight through.
        let late = IceCandidate {
            candidate: "candidate:2 1 udp 2130706431 192.0.2.11 54322 typ host".into(),
            sdp_mid: Some("0".into()),
            sdp_mline_index: Some(0),
            username_fragment: None,
        };
        controller.add_ice(late).await.expect("applied directly");
        assert!(controller.shared.pending_ice.lock().is_empty());

        controller.close().await;
        host.close().await;
    }

    #[tokio::test]
    async fn sending_before_the_channels_open_is_a_typed_error_not_a_panic() {
        let (controller, _events) = session(Role::Controller).await;
        let err = controller
            .send_control(&ControlMessage::Permissions(HostPermissions::default()))
            .await
            .expect_err("nothing is negotiated yet");
        assert!(
            matches!(err, SessionError::ChannelNotOpen { label, .. } if label == CONTROL_CHANNEL),
            "unexpected error: {err}"
        );
        controller.close().await;
    }

    #[tokio::test]
    async fn an_oversized_bulk_payload_is_refused_before_it_reaches_the_channel() {
        let (controller, _events) = session(Role::Controller).await;
        let payload = vec![0u8; BULK_CHUNK_BYTES + 1];
        let err = controller
            .send_bulk(Uuid::new_v4(), 0, &payload)
            .await
            .expect_err("over one chunk");
        assert!(
            matches!(err, SessionError::BulkPayloadTooLarge { len, max }
                if len == BULK_CHUNK_BYTES + 1 && max == BULK_CHUNK_BYTES),
            "unexpected error: {err}"
        );
        controller.close().await;
    }

    #[tokio::test]
    async fn only_the_host_can_write_video() {
        let (controller, _events) = session(Role::Controller).await;
        let err = controller
            .write_video(&[0, 0, 0, 1, 0x65], Duration::from_millis(33))
            .await
            .expect_err("a controller has no track");
        assert!(matches!(err, SessionError::NotSendingVideo));
        controller.close().await;
    }

    /// Before anything is negotiated the track has no transport, so a written
    /// sample would be packetized and silently dropped. Refusing it is what
    /// turns "the remote screen is black" into an error the host can report.
    #[tokio::test]
    async fn the_host_refuses_to_write_video_before_negotiation() {
        let (host, _events) = session(Role::Host).await;
        let err = host
            .write_video(&[0, 0, 0, 1, 0x65], Duration::from_millis(33))
            .await
            .expect_err("nothing negotiated");
        assert!(
            matches!(err, SessionError::VideoNotNegotiated),
            "unexpected error: {err}"
        );
        host.close().await;
    }

    #[tokio::test]
    async fn close_is_idempotent_and_announces_the_session_once() {
        let (controller, mut events) = session(Role::Controller).await;
        controller.close().await;
        controller.close().await;

        let mut closures = 0;
        while let Ok(event) = events.try_recv() {
            if matches!(event, SessionEvent::Closed { .. }) {
                closures += 1;
            }
        }
        assert_eq!(closures, 1);

        let err = controller
            .send_control(&ControlMessage::Clipboard { text: "x".into() })
            .await
            .expect_err("the session is gone");
        assert!(matches!(err, SessionError::Closed), "unexpected: {err}");
    }

    /// A UI that stalls for a second while the host streams 1080p60 used to
    /// grow the event queue by tens of megabytes: the channel was unbounded
    /// and `emit` only noticed a receiver that had gone away entirely.
    #[tokio::test]
    async fn a_stalled_application_sheds_video_frames_instead_of_queueing_them() {
        let (tx, rx) = mpsc::channel(2);
        let shared = Arc::new(Shared::new(tx));

        let frame = |n: u64| SessionEvent::VideoFrame {
            data: Bytes::from_static(&[0, 0, 0, 1, 0x41, 0xAA, 0x00]),
            timestamp_us: n,
        };
        // Ten frames into a queue that holds two, with nothing draining it.
        // Each of these must return rather than wait for room.
        for n in 0..10 {
            tokio::time::timeout(Duration::from_secs(1), shared.emit(frame(n)))
                .await
                .expect("emitting a video frame must never block the media task");
        }

        assert_eq!(
            shared.dropped_frames.load(Ordering::Relaxed),
            8,
            "the surplus frames should have been counted as dropped"
        );
        assert_eq!(rx.len(), 2, "the queue grew past its capacity");

        // And the drops reach the status line.
        let stats = SessionStats {
            fps: 30.0,
            bitrate_kbps: 0,
            rtt_ms: 0,
            dropped_frames: 1,
        };
        let shed = shared.dropped_frames.load(Ordering::Relaxed) as u32;
        assert_eq!(stats.dropped_frames.saturating_add(shed), 9);
    }

    /// Video is the only class allowed to vanish. A control message waits for
    /// room instead: losing one silently desynchronizes the two ends.
    #[tokio::test]
    async fn a_control_message_waits_for_room_rather_than_being_dropped() {
        let (tx, mut rx) = mpsc::channel(1);
        let shared = Arc::new(Shared::new(tx));
        shared
            .emit(SessionEvent::VideoFrame {
                data: Bytes::from_static(&[0]),
                timestamp_us: 0,
            })
            .await;

        let sender = Arc::clone(&shared);
        let queued = tokio::spawn(async move {
            sender
                .emit(SessionEvent::Control(ControlMessage::Clipboard {
                    text: "keep me".into(),
                }))
                .await;
        });

        // The application catches up, which is what makes room.
        let first = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("an event was queued")
            .expect("the channel is open");
        assert!(matches!(first, SessionEvent::VideoFrame { .. }));

        let second = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("the control message was held, not dropped")
            .expect("the channel is open");
        assert!(
            matches!(
                second,
                SessionEvent::Control(ControlMessage::Clipboard { ref text }) if text == "keep me"
            ),
            "unexpected: {second:?}"
        );
        queued.await.expect("the emitting task finished");
        assert_eq!(shared.dropped_frames.load(Ordering::Relaxed), 0);
    }

    /// `SessionEvent::Closed` is documented as terminal. Closing locally made
    /// the stack report its own `Closed` state afterwards, which landed behind
    /// it in the queue.
    #[tokio::test]
    async fn no_event_follows_the_terminal_closed_event() {
        let (tx, mut rx) = event_channel();
        let shared = Arc::new(Shared::new(tx));
        let handler = Handler {
            shared: Arc::clone(&shared),
        };

        shared.finish("closed locally".to_string()).await;
        handler
            .on_connection_state_change(RTCPeerConnectionState::Closed)
            .await;

        let first = rx.recv().await.expect("the terminal event");
        assert!(
            matches!(first, SessionEvent::Closed { .. }),
            "unexpected: {first:?}"
        );
        assert!(
            rx.try_recv().is_err(),
            "an event was published after the terminal Closed"
        );
    }

    /// The payload type is read out of the negotiated parameters once and
    /// cached. A second negotiation may hand out a different one, and samples
    /// stamped with the stale number are dropped by the far side.
    #[tokio::test]
    async fn applying_a_new_remote_description_drops_the_cached_payload_type() {
        let (controller, _c_events) = session(Role::Controller).await;
        let (host, _h_events) = session(Role::Host).await;

        let offer = controller.create_offer().await.expect("offer");
        host.accept_offer(offer.clone()).await.expect("answer");

        // Stand in for a first negotiation having resolved the payload type.
        *host
            .video
            .as_ref()
            .expect("the host owns the track")
            .payload_type
            .lock() = Some(123);

        host.accept_offer(offer).await.expect("renegotiated");
        assert_eq!(
            *host
                .video
                .as_ref()
                .expect("the host owns the track")
                .payload_type
                .lock(),
            None,
            "the payload type from the previous negotiation survived"
        );

        controller.close().await;
        host.close().await;
    }

    /// End to end between two real peers in one process: real ICE, real DTLS,
    /// real SCTP, a control message in one direction and a packetized,
    /// reassembled video frame in the other.
    ///
    /// Binds real sockets and needs a usable local interface, so it is off by
    /// default:
    ///   cargo test -p remu-session --lib -- --ignored two_in_process_peers
    #[tokio::test]
    #[ignore = "binds real sockets and runs ICE; needs a usable network interface"]
    async fn two_in_process_peers_connect_and_exchange_control_and_video() {
        let (controller, mut controller_events) = session(Role::Controller).await;
        let (host, mut host_events) = session(Role::Host).await;
        let controller = Arc::new(controller);
        let host = Arc::new(host);

        let offer = controller.create_offer().await.expect("offer");
        let answer = host.accept_offer(offer).await.expect("answer");
        controller
            .accept_answer(answer)
            .await
            .expect("answer applied");

        // Trickle both directions until both ends report their channels open.
        // Both, not either: the answerer's channels open on the DCEP OPEN it
        // receives, the offerer's only on the acknowledgement that follows.
        let opened = tokio::time::timeout(Duration::from_secs(20), async {
            let (mut controller_open, mut host_open) = (false, false);
            while !(controller_open && host_open) {
                tokio::select! {
                    Some(event) = controller_events.recv() => {
                        match event {
                            SessionEvent::LocalIce(candidate) => {
                                let _ = host.add_ice(candidate).await;
                            }
                            SessionEvent::ChannelsOpen => controller_open = true,
                            _ => {}
                        }
                    }
                    Some(event) = host_events.recv() => {
                        match event {
                            SessionEvent::LocalIce(candidate) => {
                                let _ = controller.add_ice(candidate).await;
                            }
                            SessionEvent::ChannelsOpen => host_open = true,
                            _ => {}
                        }
                    }
                    else => return,
                }
            }
        })
        .await;
        assert!(opened.is_ok(), "the peers never opened their channels");

        controller
            .send_control(&ControlMessage::Chat {
                text: "ping".into(),
                at: 1,
            })
            .await
            .expect("control send");

        let received = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(event) = host_events.recv().await {
                if let SessionEvent::Control(ControlMessage::Chat { text, .. }) = event {
                    return text;
                }
            }
            String::new()
        })
        .await
        .expect("the host received the chat message");
        assert_eq!(received, "ping");

        // One access unit, packetized by the host and reassembled by the
        // controller: NAL type 1 with two bytes of payload, which is the
        // smallest thing RFC 6184 lets travel in a single RTP packet.
        let annexb: &[u8] = &[0, 0, 0, 1, 0x41, 0xAA, 0x00];
        host.write_video(annexb, Duration::from_millis(33))
            .await
            .expect("the host writes a sample");

        let frame = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(event) = controller_events.recv().await {
                if let SessionEvent::VideoFrame { data, .. } = event {
                    return Some(data);
                }
            }
            None
        })
        .await
        .expect("the controller received a frame in time")
        .expect("the event stream ended before a frame arrived");
        assert_eq!(frame.as_ref(), annexb);

        controller.close().await;
        host.close().await;
    }
}
