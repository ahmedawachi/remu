//! WebSocket client for the Remu relay.
//!
//! Owns one background task that keeps a socket to the relay up: it registers,
//! forwards [`ClientToServer`] messages out, publishes [`RelayEvent`]s in, and
//! reconnects with [`crate::backoff`] pacing until [`RelayClient::shutdown`].
//!
//! # Threading
//!
//! [`RelayClient::connect`] is not `async` but spawns onto the ambient Tokio
//! runtime, so it must be called from inside one (`#[tokio::main]`, a
//! `Runtime::block_on`, or any spawned task). This mirrors the original
//! `SignalingClient`, which was constructed synchronously from UI code.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use remu_proto::{
    ClientToServer, PeerId, ServerToClient, MAX_SIGNALING_MESSAGE_BYTES, PROTOCOL_VERSION,
};
use tokio::sync::{mpsc, Notify};
use tokio_tungstenite::tungstenite::Message;

use crate::backoff::Backoff;

/// How often the client sends an application-level `ping`.
///
/// Distinct from the WebSocket control ping tungstenite answers for us: this
/// one traverses the relay's own message path, so a relay that has stopped
/// processing messages while its socket stays open is still detected.
const HEARTBEAT: Duration = Duration::from_secs(25);

/// What the relay connection reports to the application.
#[derive(Debug, Clone, PartialEq)]
pub enum RelayEvent {
    /// The socket is up. Registration has been sent but not yet acknowledged.
    Connected,
    /// The relay assigned (or reassigned) this desk's ID.
    ///
    /// Emitted in addition to the [`RelayEvent::Message`] carrying the same
    /// `welcome`, because almost every caller wants the ID without matching on
    /// the full message enum.
    Welcome {
        /// The nine-digit ID other desks dial.
        peer_id: PeerId,
    },
    /// A message from the relay, forwarded verbatim.
    Message(ServerToClient),
    /// The socket went down. A reconnect is already scheduled unless
    /// [`RelayClient::shutdown`] was called.
    Disconnected {
        /// Human-readable cause, for the status bar.
        reason: String,
    },
    /// Something went wrong that the user should see: a failed connect attempt,
    /// an unreadable message, or a protocol-version mismatch.
    Error {
        /// Human-readable description.
        message: String,
    },
}

/// A live, self-healing connection to the relay.
#[derive(Debug)]
pub struct RelayClient {
    outbound: mpsc::UnboundedSender<ClientToServer>,
    events: parking_lot::Mutex<Option<mpsc::UnboundedReceiver<RelayEvent>>>,
    /// The last ID the relay assigned, or 0 for "none yet". Stored as an atomic
    /// rather than behind the mutex so `peer_id()` is safe to call from a UI
    /// paint loop.
    peer_id: Arc<AtomicU32>,
    shutdown: Arc<AtomicBool>,
    /// Wakes the driver out of a reconnect sleep so `shutdown()` is immediate
    /// rather than waiting out a 30-second backoff.
    wake: Arc<Notify>,
}

impl RelayClient {
    /// Connects to `url`, registering with `alias` and `token`, and starts the
    /// reconnect loop. Returns immediately; watch [`events`](Self::events).
    ///
    /// `url` is a `ws://` or `wss://` address. TLS support depends on a feature
    /// of `tokio-tungstenite` being enabled in the binary crate — see the
    /// crate-level docs.
    pub fn connect(url: &str, alias: Option<String>, token: Option<String>) -> Self {
        let (outbound_tx, outbound_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let peer_id = Arc::new(AtomicU32::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(Notify::new());

        let driver = Driver {
            url: url.to_string(),
            alias,
            token,
            outbound: outbound_rx,
            events: event_tx,
            peer_id: Arc::clone(&peer_id),
            shutdown: Arc::clone(&shutdown),
            wake: Arc::clone(&wake),
        };
        tokio::spawn(driver.run());

        Self {
            outbound: outbound_tx,
            events: parking_lot::Mutex::new(Some(event_rx)),
            peer_id,
            shutdown,
            wake,
        }
    }

    /// Queues a message for the relay.
    ///
    /// Never blocks and never fails: signalling is best-effort, and a message
    /// queued while the socket is down is discarded rather than delivered late
    /// into a session that has since been abandoned.
    pub fn send(&self, msg: ClientToServer) {
        if self.outbound.send(msg).is_err() {
            tracing::debug!("relay driver has stopped; dropping outbound message");
        }
    }

    /// Takes the event stream.
    ///
    /// Single-consumer: the first call yields the receiver and every later one
    /// yields `None`. Returning an `Option` rather than panicking keeps a
    /// double-take an application bug the caller can handle, not a crash.
    pub fn events(&self) -> Option<mpsc::UnboundedReceiver<RelayEvent>> {
        self.events.lock().take()
    }

    /// The ID the relay last assigned, if registration has completed.
    pub fn peer_id(&self) -> Option<PeerId> {
        PeerId::new(self.peer_id.load(Ordering::Relaxed)).ok()
    }

    /// Stops the driver and prevents any further reconnect. Idempotent.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.wake.notify_waiters();
    }
}

impl Drop for RelayClient {
    fn drop(&mut self) {
        // Without this, dropping the handle would leave the driver task
        // reconnecting to a relay nobody is listening to for the rest of the
        // process's life.
        self.shutdown();
    }
}

struct Driver {
    url: String,
    alias: Option<String>,
    token: Option<String>,
    outbound: mpsc::UnboundedReceiver<ClientToServer>,
    events: mpsc::UnboundedSender<RelayEvent>,
    peer_id: Arc<AtomicU32>,
    shutdown: Arc<AtomicBool>,
    wake: Arc<Notify>,
}

impl Driver {
    async fn run(mut self) {
        let mut backoff = Backoff::default();

        while !self.stopping() {
            match tokio_tungstenite::connect_async(self.url.as_str()).await {
                Ok((socket, _response)) => {
                    backoff.reset();
                    self.emit(RelayEvent::Connected);
                    let reason = self.pump(socket).await;
                    self.emit(RelayEvent::Disconnected { reason });
                }
                Err(err) => {
                    self.emit(RelayEvent::Error {
                        message: format!("could not reach the relay at {}: {err}", self.url),
                    });
                }
            }

            if self.stopping() {
                break;
            }

            let delay = backoff.next_delay();
            tracing::debug!(
                ?delay,
                attempt = backoff.attempt(),
                "relay reconnect pending"
            );
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = self.wake.notified() => {}
                // Messages produced while offline are drained and dropped here
                // rather than piling up to be flushed into a stale session.
                Some(stale) = self.outbound.recv() => {
                    tracing::debug!(?stale, "discarding a message queued while offline");
                }
            }
        }

        tracing::debug!("relay driver stopped");
    }

    /// Runs one connected socket to completion. Returns why it ended.
    async fn pump<S>(&mut self, socket: S) -> String
    where
        S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error>
            + futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
            + Unpin,
    {
        let (mut sink, mut stream) = socket.split();

        // `preferred_id` is what keeps a saved contact reachable across a blip:
        // the relay hands back the same nine digits if nobody else took them.
        let register = ClientToServer::Register {
            protocol: PROTOCOL_VERSION,
            preferred_id: PeerId::new(self.peer_id.load(Ordering::Relaxed)).ok(),
            alias: self.alias.clone(),
            token: self.token.clone(),
        };
        if let Err(reason) = send_json(&mut sink, &register).await {
            return reason;
        }

        let mut heartbeat = tokio::time::interval(HEARTBEAT);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await; // the first tick is immediate; skip it

        loop {
            if self.stopping() {
                let _ = sink.close().await;
                return "closed by the local user".to_string();
            }

            tokio::select! {
                incoming = stream.next() => match incoming {
                    Some(Ok(Message::Text(text))) => self.on_text(text.as_str()),
                    Some(Ok(Message::Binary(_))) => {
                        // Signalling is JSON text only. A binary frame means the
                        // peer is not a Remu relay, so say so instead of
                        // silently ignoring it forever.
                        self.emit(RelayEvent::Error {
                            message: "the relay sent a binary frame; signalling is JSON text".into(),
                        });
                    }
                    Some(Ok(Message::Close(frame))) => {
                        return frame
                            .map(|f| format!("relay closed the connection: {} {}", f.code, f.reason))
                            .unwrap_or_else(|| "relay closed the connection".to_string());
                    }
                    // Ping/Pong/Frame are handled by tungstenite itself.
                    Some(Ok(_)) => {}
                    Some(Err(err)) => return format!("socket error: {err}"),
                    None => return "socket closed".to_string(),
                },
                outgoing = self.outbound.recv() => match outgoing {
                    Some(msg) => {
                        if let Err(reason) = send_json(&mut sink, &msg).await {
                            return reason;
                        }
                    }
                    None => return "the client handle was dropped".to_string(),
                },
                _ = heartbeat.tick() => {
                    if let Err(reason) = send_json(&mut sink, &ClientToServer::Ping).await {
                        return reason;
                    }
                }
                _ = self.wake.notified() => {
                    let _ = sink.close().await;
                    return "closed by the local user".to_string();
                }
            }
        }
    }

    fn on_text(&mut self, text: &str) {
        // Checked before parsing: a hostile relay must not be able to make the
        // client allocate an unbounded serde tree.
        if text.len() > MAX_SIGNALING_MESSAGE_BYTES {
            self.emit(RelayEvent::Error {
                message: format!(
                    "relay message is {} bytes, over the {MAX_SIGNALING_MESSAGE_BYTES}-byte limit",
                    text.len()
                ),
            });
            return;
        }

        let msg: ServerToClient = match serde_json::from_str(text) {
            Ok(msg) => msg,
            Err(err) => {
                self.emit(RelayEvent::Error {
                    message: format!("could not read a relay message: {err}"),
                });
                return;
            }
        };

        if let ServerToClient::Welcome {
            peer_id, protocol, ..
        } = &msg
        {
            self.peer_id.store(peer_id.get(), Ordering::Relaxed);
            if *protocol != PROTOCOL_VERSION {
                self.emit(RelayEvent::Error {
                    message: format!(
                        "protocol mismatch: the relay speaks v{protocol}, this client speaks \
                         v{PROTOCOL_VERSION}. Update whichever is older."
                    ),
                });
            }
            self.emit(RelayEvent::Welcome { peer_id: *peer_id });
        }

        self.emit(RelayEvent::Message(msg));
    }

    fn emit(&self, event: RelayEvent) {
        if self.events.send(event).is_err() {
            // The application dropped its receiver; stop doing work for it.
            self.shutdown.store(true, Ordering::SeqCst);
        }
    }

    fn stopping(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }
}

/// Serializes and writes one message, mapping either failure to a reason string.
async fn send_json<S>(sink: &mut S, msg: &ClientToServer) -> Result<(), String>
where
    S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    let json = match serde_json::to_string(msg) {
        Ok(json) => json,
        // Only reachable if a `ClientToServer` variant gains a non-serializable
        // field, which would be a compile-time-visible protocol change.
        Err(err) => return Err(format!("could not encode {msg:?}: {err}")),
    };
    sink.send(Message::text(json))
        .await
        .map_err(|err| format!("could not write to the relay: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use remu_proto::{ErrorCode, SessionId};

    /// The driver's inbound handling is the part worth testing without a
    /// socket: build one wired to channels and feed it text directly.
    fn driver() -> (Driver, mpsc::UnboundedReceiver<RelayEvent>) {
        let (_outbound_tx, outbound_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let driver = Driver {
            url: "ws://relay.invalid".into(),
            alias: None,
            token: None,
            outbound: outbound_rx,
            events: event_tx,
            peer_id: Arc::new(AtomicU32::new(0)),
            shutdown: Arc::new(AtomicBool::new(false)),
            wake: Arc::new(Notify::new()),
        };
        (driver, event_rx)
    }

    fn drain(rx: &mut mpsc::UnboundedReceiver<RelayEvent>) -> Vec<RelayEvent> {
        let mut out = Vec::new();
        while let Ok(event) = rx.try_recv() {
            out.push(event);
        }
        out
    }

    #[test]
    fn a_welcome_records_the_peer_id_and_is_announced_twice() {
        let (mut driver, mut events) = driver();
        driver.on_text(
            r#"{"type":"welcome","peerId":"123456789","protocol":1,"server":"remu-relay/0.1.0"}"#,
        );

        assert_eq!(driver.peer_id.load(Ordering::Relaxed), 123_456_789);
        let seen = drain(&mut events);
        assert_eq!(
            seen[0],
            RelayEvent::Welcome {
                peer_id: PeerId::new(123_456_789).unwrap()
            }
        );
        assert!(matches!(
            seen[1],
            RelayEvent::Message(ServerToClient::Welcome { .. })
        ));
        assert_eq!(seen.len(), 2);
    }

    #[test]
    fn a_protocol_mismatch_is_surfaced_as_an_error_before_the_welcome() {
        let (mut driver, mut events) = driver();
        driver.on_text(
            r#"{"type":"welcome","peerId":"123456789","protocol":99,"server":"remu-relay/9"}"#,
        );

        let seen = drain(&mut events);
        match &seen[0] {
            RelayEvent::Error { message } => {
                assert!(message.contains("v99"), "unhelpful message: {message}");
                assert!(message.contains("v1"), "unhelpful message: {message}");
            }
            other => panic!("expected an error first, got {other:?}"),
        }
        assert!(matches!(seen[1], RelayEvent::Welcome { .. }));
    }

    #[test]
    fn an_ordinary_message_is_forwarded_without_extra_events() {
        let (mut driver, mut events) = driver();
        let sid = SessionId::random();
        let json = serde_json::to_string(&ServerToClient::Bye {
            from: PeerId::new(222_222_222).unwrap(),
            session_id: sid,
        })
        .unwrap();
        driver.on_text(&json);

        let seen = drain(&mut events);
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0],
            RelayEvent::Message(ServerToClient::Bye {
                from: PeerId::new(222_222_222).unwrap(),
                session_id: sid
            })
        );
    }

    #[test]
    fn unparseable_text_becomes_an_error_and_does_not_kill_the_connection() {
        let (mut driver, mut events) = driver();
        driver.on_text("}{ not json");
        driver.on_text(r#"{"type":"pong"}"#);

        let seen = drain(&mut events);
        assert!(matches!(seen[0], RelayEvent::Error { .. }));
        assert_eq!(seen[1], RelayEvent::Message(ServerToClient::Pong));
    }

    #[test]
    fn an_oversized_message_is_refused_before_it_is_parsed() {
        let (mut driver, mut events) = driver();
        // Valid JSON, but far past the cap: it must be rejected on length alone.
        let huge = format!(
            r#"{{"type":"error","code":"rate-limited","message":"{}"}}"#,
            "x".repeat(MAX_SIGNALING_MESSAGE_BYTES)
        );
        driver.on_text(&huge);

        let seen = drain(&mut events);
        assert_eq!(seen.len(), 1);
        match &seen[0] {
            RelayEvent::Error { message } => assert!(message.contains("over the")),
            other => panic!("expected a size error, got {other:?}"),
        }
    }

    #[test]
    fn a_relay_error_reaches_the_application_verbatim() {
        let (mut driver, mut events) = driver();
        driver.on_text(r#"{"type":"error","code":"id-unavailable","message":"taken"}"#);
        assert_eq!(
            drain(&mut events)[0],
            RelayEvent::Message(ServerToClient::Error {
                code: ErrorCode::IdUnavailable,
                message: "taken".into()
            })
        );
    }

    #[tokio::test]
    async fn the_event_stream_is_handed_out_exactly_once() {
        let client = RelayClient::connect("ws://127.0.0.1:1/never", None, None);
        assert!(client.events().is_some());
        assert!(client.events().is_none());
        client.shutdown();
    }

    #[tokio::test]
    async fn a_failed_connect_is_reported_and_then_retried() {
        // Port 1 is reserved and never listening, so the connect always fails.
        let client = RelayClient::connect("ws://127.0.0.1:1/relay", None, None);
        let mut events = client.events().expect("first take");

        let first = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("an error event within five seconds")
            .expect("the channel stays open");
        assert!(matches!(first, RelayEvent::Error { .. }), "got {first:?}");

        // The driver must still be alive and trying again, not give up silently.
        let second = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("a second attempt within five seconds")
            .expect("the channel stays open");
        assert!(matches!(second, RelayEvent::Error { .. }), "got {second:?}");

        client.shutdown();
    }

    #[tokio::test]
    async fn shutdown_stops_the_driver_and_closes_the_event_stream() {
        let client = RelayClient::connect("ws://127.0.0.1:1/relay", None, None);
        let mut events = client.events().expect("first take");
        client.shutdown();

        // Whatever is already queued drains, then the sender is dropped and the
        // stream ends — it must not stay open forever on a dead driver.
        let closed = tokio::time::timeout(Duration::from_secs(5), async {
            while events.recv().await.is_some() {}
        })
        .await;
        assert!(closed.is_ok(), "the driver kept the event stream alive");
    }

    #[tokio::test]
    async fn peer_id_is_none_until_the_relay_says_otherwise() {
        let client = RelayClient::connect("ws://127.0.0.1:1/relay", None, None);
        assert_eq!(client.peer_id(), None);
        client.shutdown();
    }
}
