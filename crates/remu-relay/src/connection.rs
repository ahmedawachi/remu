//! One connected client: read loop, write loop, and the teardown between them.

use std::net::IpAddr;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::ws::{CloseFrame, Message, WebSocket};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use remu_proto::{
    ClientToServer, ErrorCode, PeerId, ServerToClient, MAX_SIGNALING_MESSAGE_BYTES,
    PROTOCOL_VERSION,
};
use subtle::ConstantTimeEq;
use tokio::sync::mpsc;
use tokio::sync::Notify;
use tokio::time::{interval_at, MissedTickBehavior};
use tracing::{debug, error, info, warn};

use crate::config::sanitize_alias;
use crate::registry::{ConnId, DeliveryError, IdUnavailable, Outbound, PeerHandle};
use crate::server::Shared;

/// How long the writer gets to flush a final error or `bye` after the read
/// side has gone. Short: the socket is already closing, and nothing here is
/// worth holding a task open for.
const FLUSH_GRACE: Duration = Duration::from_secs(2);

/// A heartbeat this short is almost certainly a misconfiguration, and a zero
/// interval would panic in `interval_at`.
const MIN_HEARTBEAT: Duration = Duration::from_millis(10);

/// What the relay knows about the client on this connection once it has
/// registered.
#[derive(Debug, Clone)]
struct Identity {
    id: PeerId,
    alias: Option<String>,
}

/// Everything the read loop needs that outlives a single message.
#[derive(Debug)]
struct Conn {
    state: Shared,
    conn: ConnId,
    ip: IpAddr,
    outbox: mpsc::Sender<Outbound>,
    handle: PeerHandle,
    last_seen: Arc<Mutex<Instant>>,
    /// Shared with [`run`] so teardown can free the ID even when the writer,
    /// not the reader, is what ended the connection.
    identity: Arc<Mutex<Option<PeerId>>>,
}

pub async fn run(socket: WebSocket, ip: IpAddr, state: Shared) {
    let conn = ConnId::next();
    let (outbox, inbox) = mpsc::channel(state.config.send_queue_depth.max(1));
    let evict = Arc::new(Notify::new());
    let last_seen = Arc::new(Mutex::new(Instant::now()));
    let identity = Arc::new(Mutex::new(None));
    let heartbeat = state.config.heartbeat.max(MIN_HEARTBEAT);
    let pong_timeout = state.config.pong_timeout;

    debug!(%ip, "client connected");
    let (sink, stream) = socket.split();
    let mut writer = Box::pin(write_loop(
        sink,
        inbox,
        Arc::clone(&evict),
        Arc::clone(&last_seen),
        heartbeat,
        pong_timeout,
    ));

    let writer_finished;
    {
        let ctx = Conn {
            state: Arc::clone(&state),
            conn,
            ip,
            outbox: outbox.clone(),
            handle: PeerHandle {
                outbox: outbox.clone(),
                evict,
            },
            last_seen,
            identity: Arc::clone(&identity),
        };
        let mut reader = Box::pin(read_loop(stream, ctx));
        tokio::select! {
            _ = &mut writer => writer_finished = true,
            _ = &mut reader => writer_finished = false,
        }
        // Dropping the reader here releases its outbox clone, which is what
        // eventually lets the writer see the channel close.
    }

    if let Some(id) = *identity.lock() {
        let partners = state.registry.disconnect(conn, id);
        info!(
            peer = %id,
            clients = state.registry.client_count(),
            notified = partners.len(),
            "peer disconnected"
        );
    }
    drop(outbox);

    if !writer_finished {
        // Give the queued `bye`s and errors a chance to reach the wire.
        let _ = tokio::time::timeout(FLUSH_GRACE, writer).await;
    }
}

async fn write_loop(
    mut sink: SplitSink<WebSocket, Message>,
    mut inbox: mpsc::Receiver<Outbound>,
    evict: Arc<Notify>,
    last_seen: Arc<Mutex<Instant>>,
    heartbeat: Duration,
    pong_timeout: Duration,
) {
    let mut ticker = interval_at(tokio::time::Instant::now() + heartbeat, heartbeat);
    // A relay that has been descheduled should not then fire a burst of pings.
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = evict.notified() => {
                warn!("evicting a client that is not draining its queue");
                let _ = sink.send(Message::Close(None)).await;
                return;
            }
            item = inbox.recv() => match item {
                None => {
                    let _ = sink.close().await;
                    return;
                }
                Some(Outbound::Close { code, reason }) => {
                    let _ = sink
                        .send(Message::Close(Some(CloseFrame {
                            code,
                            reason: reason.into(),
                        })))
                        .await;
                    return;
                }
                Some(Outbound::Message(msg)) => {
                    let json = match serde_json::to_string(&msg) {
                        Ok(json) => json,
                        // Unreachable for the protocol's own types, but a
                        // serialization bug must not take the relay down.
                        Err(err) => {
                            error!(error = %err, "could not serialize a relay message");
                            continue;
                        }
                    };
                    if let Err(err) = sink.send(Message::text(json)).await {
                        debug!(error = %err, "write failed; closing");
                        return;
                    }
                }
            },
            _ = ticker.tick() => {
                let silent_for = last_seen.lock().elapsed();
                if silent_for > pong_timeout {
                    warn!(silent_ms = silent_for.as_millis(), "peer stopped answering; closing");
                    let _ = sink.send(Message::Close(None)).await;
                    return;
                }
                if sink.send(Message::Ping(Bytes::new())).await.is_err() {
                    return;
                }
            }
        }
    }
}

async fn read_loop(mut stream: SplitStream<WebSocket>, ctx: Conn) {
    let mut me: Option<Identity> = None;

    while let Some(frame) = stream.next().await {
        let frame = match frame {
            Ok(frame) => frame,
            Err(err) => {
                debug!(ip = %ctx.ip, error = %err, "read failed; closing");
                return;
            }
        };
        // Any frame at all proves the peer is alive, pongs included.
        *ctx.last_seen.lock() = Instant::now();

        let payload = match frame {
            Message::Text(text) => text,
            Message::Binary(_) => {
                if push(
                    &ctx,
                    protocol_error(ErrorCode::MalformedMessage, "expected a JSON text frame"),
                )
                .is_break()
                {
                    return;
                }
                continue;
            }
            // axum answers pings for us; a pong needs nothing beyond the
            // liveness stamp taken above.
            Message::Ping(_) | Message::Pong(_) => continue,
            Message::Close(_) => return,
        };

        // Defence in depth: the upgrade already caps the frame size, so this
        // only fires if that cap is ever loosened.
        if payload.as_str().len() > MAX_SIGNALING_MESSAGE_BYTES {
            warn!(
                ip = %ctx.ip,
                bytes = payload.as_str().len(),
                "oversized message; disconnecting"
            );
            // 1009 is the WebSocket code for "message too big", so a client
            // can tell this apart from a network failure and stop retrying.
            let _ = ctx.outbox.try_send(Outbound::Close {
                code: 1009,
                reason: "message exceeds the signalling size limit",
            });
            return;
        }

        if !ctx.state.limiter.allow_message(ctx.ip, Instant::now()) {
            if push(
                &ctx,
                protocol_error(ErrorCode::RateLimited, "slow down: too many messages"),
            )
            .is_break()
            {
                return;
            }
            continue;
        }

        let msg: ClientToServer = match serde_json::from_str(payload.as_str()) {
            Ok(msg) => msg,
            Err(err) => {
                // The error text names the offending field, never its value:
                // a malformed `offer` must not put SDP in the log.
                debug!(ip = %ctx.ip, error = %err, "malformed message");
                if push(
                    &ctx,
                    protocol_error(ErrorCode::MalformedMessage, "could not parse that message"),
                )
                .is_break()
                {
                    return;
                }
                continue;
            }
        };

        if handle(&ctx, &mut me, msg).is_break() {
            return;
        }
    }
}

fn handle(ctx: &Conn, me: &mut Option<Identity>, msg: ClientToServer) -> ControlFlow<()> {
    if let ClientToServer::Register {
        protocol,
        preferred_id,
        alias,
        token,
    } = &msg
    {
        return register(
            ctx,
            me,
            *protocol,
            *preferred_id,
            alias.as_deref(),
            token.as_deref(),
        );
    }

    let Some(identity) = me.as_ref() else {
        return push(
            ctx,
            protocol_error(
                ErrorCode::NotRegistered,
                "register before sending anything else",
            ),
        );
    };

    match &msg {
        ClientToServer::Ping => return push(ctx, ServerToClient::Pong),
        ClientToServer::Lookup { peer_id } => {
            let found = ctx.state.registry.lookup(*peer_id);
            return push(
                ctx,
                ServerToClient::LookupResult {
                    peer_id: *peer_id,
                    online: found.is_some(),
                    alias: found.flatten(),
                },
            );
        }
        _ => {}
    }

    let (Some(to), Some(session)) = (msg.destination(), msg.session()) else {
        // `destination()` covers every routed variant, so this is only
        // reachable if the protocol grows a local message handled above.
        return push(
            ctx,
            protocol_error(ErrorCode::MalformedMessage, "that message cannot be routed"),
        );
    };
    let from = identity.id;
    let ends_the_session = matches!(
        msg,
        ClientToServer::Bye { .. } | ClientToServer::Reject { .. }
    );
    let kind = kind(&msg);
    let Some(forwarded) = for_peer(msg, from, identity.alias.clone()) else {
        return push(
            ctx,
            protocol_error(ErrorCode::MalformedMessage, "that message cannot be routed"),
        );
    };

    match ctx.state.registry.deliver(to, forwarded) {
        Ok(()) => {
            if ends_the_session {
                ctx.state.registry.end_session(from, to, session);
            } else {
                ctx.state.registry.note_session(from, to, session);
            }
            debug!(%from, %to, kind, "forwarded");
            ControlFlow::Continue(())
        }
        Err(DeliveryError::Offline) => {
            debug!(%from, %to, kind, "destination offline");
            push(
                ctx,
                ServerToClient::Reject {
                    // The reject reads as if it came from the peer the sender
                    // asked for, which is what the client's session state
                    // expects to match against.
                    from: to,
                    session_id: session,
                    reason: remu_proto::RejectReason::PeerOffline,
                    detail: None,
                },
            )
        }
        Err(DeliveryError::Congested) => {
            // The destination is being evicted; dropping this one message is
            // the whole point of the bounded queue.
            warn!(%from, %to, kind, "dropped a message for a congested peer");
            ControlFlow::Continue(())
        }
    }
}

fn register(
    ctx: &Conn,
    me: &mut Option<Identity>,
    protocol: u16,
    preferred: Option<PeerId>,
    alias: Option<&str>,
    token: Option<&str>,
) -> ControlFlow<()> {
    if me.is_some() {
        return push(
            ctx,
            protocol_error(
                ErrorCode::AlreadyRegistered,
                "this connection already holds a peer id",
            ),
        );
    }
    if !ctx.state.limiter.allow_registration(ctx.ip, Instant::now()) {
        warn!(ip = %ctx.ip, "registration rate limited");
        return push(
            ctx,
            protocol_error(
                ErrorCode::RateLimited,
                "too many registrations from this address",
            ),
        );
    }
    if protocol != PROTOCOL_VERSION {
        return push(
            ctx,
            protocol_error(
                ErrorCode::UnsupportedProtocol,
                format!("this relay speaks protocol {PROTOCOL_VERSION}"),
            ),
        );
    }
    if !token_matches(ctx.state.config.token.as_deref(), token) {
        // The offered token is never logged, only the fact that it was wrong.
        warn!(ip = %ctx.ip, "registration refused: bad or missing token");
        return push(
            ctx,
            protocol_error(ErrorCode::Unauthorized, "a valid token is required"),
        );
    }

    let alias = alias.and_then(sanitize_alias);
    let id =
        match ctx
            .state
            .registry
            .register(ctx.conn, preferred, alias.clone(), ctx.handle.clone())
        {
            Ok(id) => id,
            Err(IdUnavailable) => {
                error!("peer id space exhausted");
                return push(
                    ctx,
                    protocol_error(ErrorCode::IdUnavailable, "no peer id is free; try again"),
                );
            }
        };

    *ctx.identity.lock() = Some(id);
    *me = Some(Identity { id, alias });
    info!(
        peer = %id,
        ip = %ctx.ip,
        clients = ctx.state.registry.client_count(),
        kept_id = preferred == Some(id),
        "peer registered"
    );
    push(
        ctx,
        ServerToClient::Welcome {
            peer_id: id,
            protocol: PROTOCOL_VERSION,
            server: remu_proto::server_banner(),
        },
    )
}

/// Rewrites a client's routed message into what its destination receives:
/// `to` becomes `from`, and the sender's alias rides along where the variant
/// carries one.
fn for_peer(msg: ClientToServer, from: PeerId, alias: Option<String>) -> Option<ServerToClient> {
    Some(match msg {
        ClientToServer::Connect { session_id, .. } => ServerToClient::Connect {
            from,
            from_alias: alias,
            session_id,
        },
        ClientToServer::Challenge {
            session_id,
            nonce,
            needs_password,
            ..
        } => ServerToClient::Challenge {
            from,
            session_id,
            nonce,
            needs_password,
        },
        ClientToServer::Offer {
            session_id,
            sdp,
            auth,
            ..
        } => ServerToClient::Offer {
            from,
            from_alias: alias,
            session_id,
            sdp,
            auth,
        },
        ClientToServer::Answer {
            session_id, sdp, ..
        } => ServerToClient::Answer {
            from,
            session_id,
            sdp,
        },
        ClientToServer::Ice {
            session_id,
            candidate,
            ..
        } => ServerToClient::Ice {
            from,
            session_id,
            candidate,
        },
        ClientToServer::Reject {
            session_id,
            reason,
            detail,
            ..
        } => ServerToClient::Reject {
            from,
            session_id,
            reason,
            detail,
        },
        ClientToServer::Bye { session_id, .. } => ServerToClient::Bye { from, session_id },
        ClientToServer::Register { .. } | ClientToServer::Lookup { .. } | ClientToServer::Ping => {
            return None
        }
    })
}

fn kind(msg: &ClientToServer) -> &'static str {
    match msg {
        ClientToServer::Register { .. } => "register",
        ClientToServer::Lookup { .. } => "lookup",
        ClientToServer::Connect { .. } => "connect",
        ClientToServer::Challenge { .. } => "challenge",
        ClientToServer::Offer { .. } => "offer",
        ClientToServer::Answer { .. } => "answer",
        ClientToServer::Ice { .. } => "ice",
        ClientToServer::Reject { .. } => "reject",
        ClientToServer::Bye { .. } => "bye",
        ClientToServer::Ping => "ping",
    }
}

fn protocol_error(code: ErrorCode, message: impl Into<String>) -> ServerToClient {
    ServerToClient::Error {
        code,
        message: message.into(),
    }
}

/// Queues a message for this connection's own client.
///
/// A full queue means the client is not reading, so the connection ends rather
/// than the relay growing a buffer for it.
fn push(ctx: &Conn, msg: ServerToClient) -> ControlFlow<()> {
    match ctx.outbox.try_send(Outbound::Message(msg)) {
        Ok(()) => ControlFlow::Continue(()),
        Err(mpsc::error::TrySendError::Full(_)) => {
            warn!(ip = %ctx.ip, "client is not draining its queue; closing");
            ControlFlow::Break(())
        }
        Err(mpsc::error::TrySendError::Closed(_)) => ControlFlow::Break(()),
    }
}

/// Compares the offered token against the configured one without leaking where
/// the first wrong byte is.
///
/// The length comparison is not constant time: how long the operator's secret
/// is, is not the secret. Content is compared with `subtle`.
fn token_matches(expected: Option<&str>, offered: Option<&str>) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    let offered = offered.unwrap_or_default();
    if expected.len() != offered.len() {
        return false;
    }
    expected.as_bytes().ct_eq(offered.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use remu_proto::{SdpKind, SessionDescription, SessionId};

    fn peer(n: u32) -> PeerId {
        PeerId::new(n).unwrap()
    }

    #[test]
    fn accepts_any_token_when_the_relay_is_open() {
        assert!(token_matches(None, None));
        assert!(token_matches(None, Some("whatever")));
    }

    #[test]
    fn refuses_a_wrong_missing_or_truncated_token() {
        let expected = Some("s3cret-token");
        assert!(token_matches(expected, Some("s3cret-token")));
        assert!(!token_matches(expected, None));
        assert!(!token_matches(expected, Some("")));
        assert!(!token_matches(expected, Some("s3cret-toke")));
        assert!(!token_matches(expected, Some("s3cret-token ")));
        assert!(!token_matches(expected, Some("S3cret-token")));
    }

    #[test]
    fn rewrites_the_destination_into_the_senders_identity() {
        let session = SessionId::random();
        let msg = ClientToServer::Offer {
            to: peer(222_222_222),
            session_id: session,
            sdp: SessionDescription {
                kind: SdpKind::Offer,
                sdp: "v=0".into(),
            },
            auth: Some("deadbeef".into()),
        };
        let forwarded = for_peer(msg, peer(111_111_111), Some("Ahmed".into())).unwrap();
        match forwarded {
            ServerToClient::Offer {
                from,
                from_alias,
                session_id,
                sdp,
                auth,
            } => {
                assert_eq!(from, peer(111_111_111));
                assert_eq!(from_alias.as_deref(), Some("Ahmed"));
                assert_eq!(session_id, session);
                assert_eq!(sdp.sdp, "v=0");
                assert_eq!(auth.as_deref(), Some("deadbeef"));
            }
            other => panic!("expected an offer, got {other:?}"),
        }
    }

    #[test]
    fn carries_the_alias_only_on_the_variants_that_have_one() {
        let session = SessionId::random();
        let answer = for_peer(
            ClientToServer::Answer {
                to: peer(222_222_222),
                session_id: session,
                sdp: SessionDescription {
                    kind: SdpKind::Answer,
                    sdp: "v=0".into(),
                },
            },
            peer(111_111_111),
            Some("Ahmed".into()),
        )
        .unwrap();
        assert!(matches!(
            answer,
            ServerToClient::Answer { from, .. } if from == peer(111_111_111)
        ));

        let connect = for_peer(
            ClientToServer::Connect {
                to: peer(222_222_222),
                session_id: session,
            },
            peer(111_111_111),
            Some("Ahmed".into()),
        )
        .unwrap();
        assert!(matches!(
            connect,
            ServerToClient::Connect { from_alias: Some(a), .. } if a == "Ahmed"
        ));
    }

    #[test]
    fn refuses_to_route_the_relay_local_messages() {
        for msg in [
            ClientToServer::Ping,
            ClientToServer::Lookup {
                peer_id: peer(222_222_222),
            },
            ClientToServer::Register {
                protocol: PROTOCOL_VERSION,
                preferred_id: None,
                alias: None,
                token: None,
            },
        ] {
            assert!(for_peer(msg, peer(111_111_111), None).is_none());
        }
    }
}
