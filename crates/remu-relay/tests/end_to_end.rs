//! End-to-end tests: a real relay on an ephemeral port, driven by real
//! WebSocket clients.
//!
//! The unit tests in `src/` prove the registry and the parsers in isolation.
//! These prove the thing an actual desk app talks to: the HTTP upgrade, the
//! JSON on the wire, the routing between two sockets, and what happens to one
//! peer when the other's TCP connection dies.
//!
//! Every wait here is a wait for a specific event with a deadline, never a
//! fixed sleep: a test that sleeps is a test that is slow when it passes and
//! flaky when it fails.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use remu_proto::{
    ClientToServer, ErrorCode, IceCandidate, PeerId, RejectReason, SdpKind, ServerToClient,
    SessionDescription, SessionId, MAX_SIGNALING_MESSAGE_BYTES, PROTOCOL_VERSION,
};
use remu_relay::{Relay, RelayConfig, RelayHandle};

/// Long enough that a loaded CI box does not trip it, short enough that a
/// genuinely missing message fails the run rather than hanging it.
const DEADLINE: Duration = Duration::from_secs(5);

/// How long "nothing else arrives" is observed for. Only used in the negative
/// assertions, where there is no event to wait for by definition.
const QUIET: Duration = Duration::from_millis(300);

fn peer(raw: u32) -> PeerId {
    PeerId::new(raw).expect("a nine-digit test id")
}

/// A relay bound to port 0 and serving, plus everything needed to stop it.
struct TestRelay {
    addr: SocketAddr,
    handle: RelayHandle,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl TestRelay {
    async fn start() -> Self {
        Self::start_with(RelayConfig::default()).await
    }

    async fn start_with(config: RelayConfig) -> Self {
        // Loopback rather than the default unspecified address: the tests must
        // not open a listener to the network, and Windows will not connect to
        // 0.0.0.0.
        let config = RelayConfig {
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 0,
            ..config
        };
        let relay = Relay::bind(config)
            .await
            .expect("bind on an ephemeral port");
        let addr = relay.local_addr();
        let handle = relay.handle();
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let shutdown = async {
                let _ = stopped.await;
            };
            relay.serve_with_shutdown(shutdown).await.expect("serve");
        });
        Self {
            addr,
            handle,
            stop: Some(stop),
            task,
        }
    }

    fn url(&self) -> String {
        format!("ws://{}/", self.addr)
    }

    async fn client(&self) -> Client {
        Client::connect(&self.url()).await
    }

    /// Connects a client and completes registration, so a test that is not
    /// about registration reads as the scenario it is testing.
    async fn registered(&self, preferred: Option<PeerId>, alias: Option<&str>) -> (Client, PeerId) {
        let mut client = self.client().await;
        let id = client
            .register(ClientToServer::Register {
                protocol: PROTOCOL_VERSION,
                preferred_id: preferred,
                alias: alias.map(str::to_owned),
                token: None,
            })
            .await;
        (client, id)
    }

    /// Plain HTTP/1.1 over the same port, for the endpoints that are not
    /// WebSocket. Returns the status line and the body.
    async fn http_get(&self, path: &str) -> (String, String) {
        let mut socket = TcpStream::connect(self.addr).await.expect("connect");
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            self.addr
        );
        socket
            .write_all(request.as_bytes())
            .await
            .expect("write the request");
        let mut raw = String::new();
        timeout(DEADLINE, socket.read_to_string(&mut raw))
            .await
            .expect("the relay answered within the deadline")
            .expect("read the response");
        let (head, body) = raw.split_once("\r\n\r\n").expect("a complete response");
        let status = head.lines().next().unwrap_or_default().to_owned();
        (status, body.to_owned())
    }
}

impl Drop for TestRelay {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        // Graceful shutdown waits for live connections, and a test that is
        // asserting on a half-dead socket may well still hold one.
        self.task.abort();
    }
}

/// What a client saw when the relay hung up on it.
#[derive(Debug)]
struct Closed {
    /// Signalling messages delivered before the close, if any. A relay that
    /// is supposed to disconnect without answering must leave this empty.
    before: Vec<ServerToClient>,
}

struct Client {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl Client {
    async fn connect(url: &str) -> Self {
        let (ws, response) = timeout(DEADLINE, connect_async(url))
            .await
            .expect("the upgrade completed within the deadline")
            .expect("the relay accepted the WebSocket upgrade");
        assert_eq!(response.status().as_u16(), 101, "expected a 101 upgrade");
        Self { ws }
    }

    async fn send(&mut self, msg: &ClientToServer) {
        let json = serde_json::to_string(msg).expect("the protocol serializes");
        self.send_raw(json).await;
    }

    async fn send_raw(&mut self, text: String) {
        timeout(DEADLINE, self.ws.send(Message::text(text)))
            .await
            .expect("the send completed within the deadline")
            .expect("the socket accepted the frame");
    }

    /// The next signalling message, as JSON, so a test can assert on the wire
    /// form and not only on what serde was willing to parse.
    async fn recv_json(&mut self) -> serde_json::Value {
        let text = timeout(DEADLINE, self.next_text())
            .await
            .expect("the relay answered within the deadline");
        serde_json::from_str(&text).expect("the relay sent JSON")
    }

    async fn recv(&mut self) -> ServerToClient {
        let value = self.recv_json().await;
        serde_json::from_value(value.clone())
            .unwrap_or_else(|err| panic!("{value} is not a protocol message: {err}"))
    }

    async fn next_text(&mut self) -> String {
        loop {
            let frame = self
                .ws
                .next()
                .await
                .expect("the relay closed the connection instead of answering")
                .expect("reading from the relay failed");
            match frame {
                Message::Text(text) => return text.as_str().to_owned(),
                // The relay heartbeats idle connections; tungstenite answers
                // the ping for us, so it is only noise here.
                Message::Ping(_) | Message::Pong(_) => continue,
                other => panic!("expected a JSON text frame, got {other:?}"),
            }
        }
    }

    /// Sends `register` and asserts the relay welcomed it, returning the
    /// allocated ID.
    async fn register(&mut self, msg: ClientToServer) -> PeerId {
        self.send(&msg).await;
        match self.recv().await {
            ServerToClient::Welcome {
                peer_id, protocol, ..
            } => {
                assert_eq!(protocol, PROTOCOL_VERSION);
                peer_id
            }
            other => panic!("expected a welcome, got {other:?}"),
        }
    }

    /// Reads until the relay hangs up, collecting anything it said first.
    async fn wait_until_closed(&mut self) -> Closed {
        let collect = async {
            let mut before = Vec::new();
            while let Some(frame) = self.ws.next().await {
                match frame {
                    Ok(Message::Text(text)) => match serde_json::from_str(text.as_str()) {
                        Ok(msg) => before.push(msg),
                        Err(err) => panic!("the relay sent JSON it cannot parse back: {err}"),
                    },
                    // A close frame, or a transport error from a reset: both
                    // mean the relay is done with this connection.
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => continue,
                }
            }
            Closed { before }
        };
        timeout(DEADLINE, collect)
            .await
            .expect("the relay hung up within the deadline")
    }

    /// Asserts the relay stays silent. Used only where the correct behaviour
    /// is the absence of a message, which nothing can be awaited for.
    async fn expect_silence(&mut self) {
        if let Ok(frame) = timeout(QUIET, self.next_text()).await {
            panic!("expected no further message, got {frame}");
        }
    }
}

fn sdp(kind: SdpKind) -> SessionDescription {
    SessionDescription {
        kind,
        sdp: "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\n".into(),
    }
}

fn candidate(foundation: &str) -> IceCandidate {
    IceCandidate {
        candidate: format!("candidate:{foundation} 1 udp 2130706431 192.0.2.1 5000 typ host"),
        sdp_mid: Some("0".into()),
        sdp_mline_index: Some(0),
        username_fragment: None,
    }
}

#[tokio::test]
async fn two_clients_are_welcomed_with_distinct_nine_digit_ids() {
    let relay = TestRelay::start().await;

    let mut first = relay.client().await;
    let mut second = relay.client().await;
    let open = ClientToServer::Register {
        protocol: PROTOCOL_VERSION,
        preferred_id: None,
        alias: None,
        token: None,
    };
    first.send(&open).await;
    second.send(&open).await;

    let mut ids = Vec::new();
    for client in [&mut first, &mut second] {
        let welcome = client.recv_json().await;
        assert_eq!(welcome["type"], "welcome");
        assert_eq!(welcome["protocol"], PROTOCOL_VERSION);
        assert!(
            welcome["server"]
                .as_str()
                .unwrap_or_default()
                .starts_with("remu-relay/"),
            "the welcome must name the server: {welcome}"
        );
        let raw = welcome["peerId"].as_str().expect("peerId is a JSON string");
        assert_eq!(raw.len(), 9, "peer ids are nine digits: {raw:?}");
        assert!(raw.bytes().all(|b| b.is_ascii_digit()), "{raw:?}");
        ids.push(raw.parse::<PeerId>().expect("a well-formed peer id"));
    }

    assert_ne!(ids[0], ids[1], "two clients must not share an id");
    assert_eq!(relay.handle.connected_clients(), 2);
    assert!(relay.handle.is_online(ids[0]) && relay.handle.is_online(ids[1]));
}

#[tokio::test]
async fn carries_a_whole_connect_to_bye_exchange_between_two_peers() {
    let relay = TestRelay::start().await;
    let (mut controller, controller_id) = relay
        .registered(Some(peer(111_111_111)), Some("Ahmed's Mac"))
        .await;
    let (mut host, host_id) = relay
        .registered(Some(peer(222_222_222)), Some("Reception PC"))
        .await;
    let session = SessionId::random();

    controller
        .send(&ClientToServer::Connect {
            to: host_id,
            session_id: session,
        })
        .await;
    match host.recv().await {
        ServerToClient::Connect {
            from,
            from_alias,
            session_id,
        } => {
            assert_eq!(from, controller_id, "`to` must be rewritten to the sender");
            assert_eq!(from_alias.as_deref(), Some("Ahmed's Mac"));
            assert_eq!(session_id, session);
        }
        other => panic!("expected a connect, got {other:?}"),
    }

    host.send(&ClientToServer::Challenge {
        to: controller_id,
        session_id: session,
        nonce: "a1b2c3".into(),
        needs_password: true,
    })
    .await;
    match controller.recv().await {
        ServerToClient::Challenge {
            from,
            session_id,
            nonce,
            needs_password,
        } => {
            assert_eq!(from, host_id);
            assert_eq!(session_id, session);
            assert_eq!(nonce, "a1b2c3");
            assert!(needs_password);
        }
        other => panic!("expected a challenge, got {other:?}"),
    }

    controller
        .send(&ClientToServer::Offer {
            to: host_id,
            session_id: session,
            sdp: sdp(SdpKind::Offer),
            auth: Some("deadbeef".into()),
        })
        .await;
    match host.recv().await {
        ServerToClient::Offer {
            from,
            from_alias,
            session_id,
            sdp,
            auth,
        } => {
            assert_eq!(from, controller_id);
            assert_eq!(from_alias.as_deref(), Some("Ahmed's Mac"));
            assert_eq!(session_id, session);
            assert_eq!(sdp.kind, SdpKind::Offer);
            assert!(sdp.sdp.starts_with("v=0"), "the SDP must arrive verbatim");
            assert_eq!(
                auth.as_deref(),
                Some("deadbeef"),
                "the relay must forward the auth proof untouched"
            );
        }
        other => panic!("expected an offer, got {other:?}"),
    }

    host.send(&ClientToServer::Answer {
        to: controller_id,
        session_id: session,
        sdp: sdp(SdpKind::Answer),
    })
    .await;
    match controller.recv().await {
        ServerToClient::Answer {
            from,
            session_id,
            sdp,
        } => {
            assert_eq!(from, host_id);
            assert_eq!(session_id, session);
            assert_eq!(sdp.kind, SdpKind::Answer);
        }
        other => panic!("expected an answer, got {other:?}"),
    }

    controller
        .send(&ClientToServer::Ice {
            to: host_id,
            session_id: session,
            candidate: candidate("1"),
        })
        .await;
    host.send(&ClientToServer::Ice {
        to: controller_id,
        session_id: session,
        candidate: candidate("2"),
    })
    .await;
    match host.recv().await {
        ServerToClient::Ice {
            from, candidate, ..
        } => {
            assert_eq!(from, controller_id);
            assert!(candidate.candidate.contains("candidate:1"));
            assert_eq!(candidate.sdp_mid.as_deref(), Some("0"));
            assert_eq!(candidate.sdp_mline_index, Some(0));
        }
        other => panic!("expected ice, got {other:?}"),
    }
    match controller.recv().await {
        ServerToClient::Ice {
            from, candidate, ..
        } => {
            assert_eq!(from, host_id);
            assert!(candidate.candidate.contains("candidate:2"));
        }
        other => panic!("expected ice, got {other:?}"),
    }

    controller
        .send(&ClientToServer::Bye {
            to: host_id,
            session_id: session,
        })
        .await;
    match host.recv().await {
        ServerToClient::Bye { from, session_id } => {
            assert_eq!(from, controller_id);
            assert_eq!(session_id, session);
        }
        other => panic!("expected a bye, got {other:?}"),
    }
}

#[tokio::test]
async fn honours_a_preferred_id_that_is_free_and_refuses_one_that_is_taken() {
    let relay = TestRelay::start().await;
    let wanted = peer(123_456_789);

    let (_holder, granted) = relay.registered(Some(wanted), None).await;
    assert_eq!(granted, wanted, "a free preferred id must be honoured");

    let (_latecomer, other) = relay.registered(Some(wanted), None).await;
    assert_ne!(
        other, wanted,
        "an id another client holds must not be handed out twice"
    );
    assert_eq!(relay.handle.connected_clients(), 2);
}

#[tokio::test]
async fn answers_the_sender_with_peer_offline_when_the_destination_is_not_connected() {
    let relay = TestRelay::start().await;
    let (mut controller, _) = relay.registered(Some(peer(111_111_111)), None).await;
    let absent = peer(999_999_999);
    assert!(!relay.handle.is_online(absent));
    let session = SessionId::random();

    controller
        .send(&ClientToServer::Offer {
            to: absent,
            session_id: session,
            sdp: sdp(SdpKind::Offer),
            auth: None,
        })
        .await;

    match controller.recv().await {
        ServerToClient::Reject {
            from,
            session_id,
            reason,
            ..
        } => {
            assert_eq!(
                from, absent,
                "the reject must read as if it came from the peer that was asked for"
            );
            assert_eq!(session_id, session, "the session must match the attempt");
            assert_eq!(reason, RejectReason::PeerOffline);
        }
        other => panic!("expected a peer-offline reject, got {other:?}"),
    }
}

#[tokio::test]
async fn refuses_every_message_sent_before_register() {
    let relay = TestRelay::start().await;
    let mut client = relay.client().await;

    for msg in [
        ClientToServer::Ping,
        ClientToServer::Lookup {
            peer_id: peer(222_222_222),
        },
        ClientToServer::Connect {
            to: peer(222_222_222),
            session_id: SessionId::random(),
        },
    ] {
        client.send(&msg).await;
        match client.recv().await {
            ServerToClient::Error { code, .. } => {
                assert_eq!(code, ErrorCode::NotRegistered, "for {msg:?}");
            }
            other => panic!("expected not-registered for {msg:?}, got {other:?}"),
        }
    }

    // Refusal, not disconnection: the client may still register afterwards.
    let id = client
        .register(ClientToServer::Register {
            protocol: PROTOCOL_VERSION,
            preferred_id: None,
            alias: None,
            token: None,
        })
        .await;
    assert!(relay.handle.is_online(id));
}

#[tokio::test]
async fn refuses_a_register_that_speaks_another_protocol_version() {
    let relay = TestRelay::start().await;
    let mut client = relay.client().await;

    client
        .send(&ClientToServer::Register {
            protocol: PROTOCOL_VERSION.wrapping_add(1),
            preferred_id: None,
            alias: None,
            token: None,
        })
        .await;

    match client.recv().await {
        ServerToClient::Error { code, message } => {
            assert_eq!(code, ErrorCode::UnsupportedProtocol);
            assert!(
                message.contains(&PROTOCOL_VERSION.to_string()),
                "the refusal must say which version the relay speaks: {message:?}"
            );
        }
        other => panic!("expected unsupported-protocol, got {other:?}"),
    }
    assert_eq!(
        relay.handle.connected_clients(),
        0,
        "a refused register must not allocate an id"
    );
}

#[tokio::test]
async fn refuses_a_second_register_on_the_same_connection() {
    let relay = TestRelay::start().await;
    let (mut client, id) = relay.registered(Some(peer(111_111_111)), None).await;

    client
        .send(&ClientToServer::Register {
            protocol: PROTOCOL_VERSION,
            preferred_id: Some(peer(222_222_222)),
            alias: None,
            token: None,
        })
        .await;

    match client.recv().await {
        ServerToClient::Error { code, .. } => assert_eq!(code, ErrorCode::AlreadyRegistered),
        other => panic!("expected already-registered, got {other:?}"),
    }
    assert_eq!(
        relay.handle.connected_clients(),
        1,
        "the refused second register must not have taken a second id"
    );
    assert!(relay.handle.is_online(id) && !relay.handle.is_online(peer(222_222_222)));

    // The original identity still works.
    client.send(&ClientToServer::Lookup { peer_id: id }).await;
    match client.recv().await {
        ServerToClient::LookupResult { online, .. } => assert!(online),
        other => panic!("expected a lookup result, got {other:?}"),
    }
}

#[tokio::test]
async fn accepts_the_configured_token_and_refuses_a_wrong_or_missing_one() {
    let relay = TestRelay::start_with(RelayConfig {
        token: Some("s3cret-token".into()),
        ..Default::default()
    })
    .await;

    let mut good = relay.client().await;
    let id = good
        .register(ClientToServer::Register {
            protocol: PROTOCOL_VERSION,
            preferred_id: None,
            alias: None,
            token: Some("s3cret-token".into()),
        })
        .await;
    assert!(relay.handle.is_online(id));

    for offered in [None, Some("wrong-token".to_owned()), Some(String::new())] {
        let mut client = relay.client().await;
        client
            .send(&ClientToServer::Register {
                protocol: PROTOCOL_VERSION,
                preferred_id: None,
                alias: None,
                token: offered.clone(),
            })
            .await;
        match client.recv().await {
            ServerToClient::Error { code, message } => {
                assert_eq!(code, ErrorCode::Unauthorized, "for token {offered:?}");
                assert!(
                    !message.contains("s3cret"),
                    "the refusal must not echo the secret: {message:?}"
                );
            }
            other => panic!("expected unauthorized for {offered:?}, got {other:?}"),
        }
    }

    assert_eq!(
        relay.handle.connected_clients(),
        1,
        "only the authorized client holds an id"
    );
}

#[tokio::test]
async fn disconnects_a_client_that_sends_more_than_the_signalling_size_limit() {
    let relay = TestRelay::start().await;
    let noisy_id = peer(111_111_111);
    let (mut noisy, _) = relay.registered(Some(noisy_id), None).await;
    let (mut witness, witness_id) = relay.registered(Some(peer(222_222_222)), None).await;
    // A live session with a witness turns "the relay dropped it" into an event
    // this test can await, rather than a socket state it has to guess at.
    noisy
        .send(&ClientToServer::Connect {
            to: witness_id,
            session_id: SessionId::random(),
        })
        .await;
    witness.recv().await;

    noisy
        .send_raw("x".repeat(MAX_SIGNALING_MESSAGE_BYTES + 1))
        .await;

    let closed = noisy.wait_until_closed().await;
    assert!(
        closed.before.is_empty(),
        "the relay must hang up rather than answer an oversized message, got {:?}",
        closed.before
    );
    assert!(
        matches!(witness.recv().await, ServerToClient::Bye { from, .. } if from == noisy_id),
        "the oversized sender must be torn down, not merely ignored"
    );
    assert!(!relay.handle.is_online(noisy_id));

    // The relay itself survives: another client can still register.
    let (_survivor, id) = relay.registered(Some(peer(333_333_333)), None).await;
    assert!(relay.handle.is_online(id));
}

#[tokio::test]
async fn tells_the_surviving_peer_when_its_partner_drops_the_socket() {
    let relay = TestRelay::start().await;
    let (mut controller, controller_id) = relay.registered(Some(peer(111_111_111)), None).await;
    let (mut host, host_id) = relay.registered(Some(peer(222_222_222)), None).await;
    let session = SessionId::random();

    controller
        .send(&ClientToServer::Connect {
            to: host_id,
            session_id: session,
        })
        .await;
    host.recv().await;

    drop(controller);

    match host.recv().await {
        ServerToClient::Bye { from, session_id } => {
            assert_eq!(from, controller_id);
            assert_eq!(session_id, session);
        }
        other => panic!("expected a bye for the dead peer, got {other:?}"),
    }
    assert!(!relay.handle.is_online(controller_id));
    assert_eq!(relay.handle.connected_clients(), 1);
}

#[tokio::test]
async fn does_not_repeat_a_bye_for_a_session_the_peers_already_ended() {
    let relay = TestRelay::start().await;
    let (mut controller, _) = relay.registered(Some(peer(111_111_111)), None).await;
    let (mut host, host_id) = relay.registered(Some(peer(222_222_222)), None).await;
    let session = SessionId::random();

    controller
        .send(&ClientToServer::Connect {
            to: host_id,
            session_id: session,
        })
        .await;
    host.recv().await;
    controller
        .send(&ClientToServer::Bye {
            to: host_id,
            session_id: session,
        })
        .await;
    host.recv().await;

    drop(controller);

    host.expect_silence().await;
}

#[tokio::test]
async fn hands_a_disconnected_clients_id_to_the_next_client_that_asks_for_it() {
    let relay = TestRelay::start().await;
    let reused = peer(111_111_111);
    let (mut leaver, _) = relay.registered(Some(reused), None).await;
    let (mut witness, witness_id) = relay.registered(Some(peer(222_222_222)), None).await;

    // Putting the two in a session gives the test an event to wait on: the
    // witness's `bye` is the relay saying it has finished the teardown.
    leaver
        .send(&ClientToServer::Connect {
            to: witness_id,
            session_id: SessionId::random(),
        })
        .await;
    witness.recv().await;
    drop(leaver);
    assert!(matches!(
        witness.recv().await,
        ServerToClient::Bye { from, .. } if from == reused
    ));

    let (_newcomer, granted) = relay.registered(Some(reused), None).await;
    assert_eq!(granted, reused, "a freed id must be allocatable again");
}

#[tokio::test]
async fn healthz_reports_ok_with_uptime_and_the_live_client_count() {
    let relay = TestRelay::start().await;
    let (_client, _) = relay.registered(None, None).await;

    let (status, body) = relay.http_get("/healthz").await;

    assert!(
        status.starts_with("HTTP/1.1 200"),
        "status line: {status:?}"
    );
    let json: serde_json::Value = serde_json::from_str(&body).expect("a JSON body");
    assert_eq!(json["status"], "ok");
    assert_eq!(json["clients"], 1);
    assert!(
        json["uptimeSeconds"].is_u64(),
        "uptimeSeconds must be a number: {json}"
    );
}

#[tokio::test]
async fn a_lookup_reports_an_online_peer_with_its_alias_and_an_offline_one_without() {
    let relay = TestRelay::start().await;
    let (_reception, reception_id) = relay
        .registered(Some(peer(222_222_222)), Some("  Reception PC  "))
        .await;
    let (mut asker, _) = relay.registered(Some(peer(111_111_111)), None).await;

    asker
        .send(&ClientToServer::Lookup {
            peer_id: reception_id,
        })
        .await;
    match asker.recv().await {
        ServerToClient::LookupResult {
            peer_id,
            online,
            alias,
        } => {
            assert_eq!(peer_id, reception_id);
            assert!(online);
            assert_eq!(
                alias.as_deref(),
                Some("Reception PC"),
                "the alias must come back trimmed"
            );
        }
        other => panic!("expected a lookup result, got {other:?}"),
    }

    let absent = peer(999_999_999);
    asker
        .send(&ClientToServer::Lookup { peer_id: absent })
        .await;
    match asker.recv().await {
        ServerToClient::LookupResult {
            peer_id,
            online,
            alias,
        } => {
            assert_eq!(peer_id, absent);
            assert!(!online);
            assert_eq!(alias, None, "an offline peer has no alias to report");
        }
        other => panic!("expected a lookup result, got {other:?}"),
    }
}

#[tokio::test]
async fn answers_a_malformed_message_with_an_error_and_keeps_the_connection() {
    let relay = TestRelay::start().await;
    let (mut client, id) = relay.registered(Some(peer(111_111_111)), None).await;

    for bad in [
        "{".to_owned(),
        r#"{"type":"shutdown"}"#.to_owned(),
        r#"{"type":"lookup","peerId":"42"}"#.to_owned(),
    ] {
        client.send_raw(bad.clone()).await;
        match client.recv().await {
            ServerToClient::Error { code, message } => {
                assert_eq!(code, ErrorCode::MalformedMessage, "for {bad}");
                assert!(
                    !message.contains("shutdown") && !message.contains("42"),
                    "the error must not echo what the client sent: {message:?}"
                );
            }
            other => panic!("expected malformed-message for {bad}, got {other:?}"),
        }
    }

    client.send(&ClientToServer::Ping).await;
    assert_eq!(client.recv().await, ServerToClient::Pong);
    assert!(
        relay.handle.is_online(id),
        "the client kept its id throughout"
    );
}

#[tokio::test]
async fn keeps_two_concurrent_sessions_apart() {
    let relay = TestRelay::start().await;
    let (mut alice, alice_id) = relay.registered(Some(peer(111_111_111)), None).await;
    let (mut bob, bob_id) = relay.registered(Some(peer(222_222_222)), None).await;
    let (mut carol, carol_id) = relay.registered(Some(peer(333_333_333)), None).await;
    let with_bob = SessionId::random();
    let with_carol = SessionId::random();

    alice
        .send(&ClientToServer::Connect {
            to: bob_id,
            session_id: with_bob,
        })
        .await;
    alice
        .send(&ClientToServer::Connect {
            to: carol_id,
            session_id: with_carol,
        })
        .await;

    match bob.recv().await {
        ServerToClient::Connect {
            from, session_id, ..
        } => {
            assert_eq!(from, alice_id);
            assert_eq!(session_id, with_bob);
        }
        other => panic!("expected a connect, got {other:?}"),
    }
    match carol.recv().await {
        ServerToClient::Connect {
            from, session_id, ..
        } => {
            assert_eq!(from, alice_id);
            assert_eq!(session_id, with_carol);
        }
        other => panic!("expected a connect, got {other:?}"),
    }

    // Ending one session must not disturb the other.
    alice
        .send(&ClientToServer::Bye {
            to: bob_id,
            session_id: with_bob,
        })
        .await;
    assert!(matches!(
        bob.recv().await,
        ServerToClient::Bye { session_id, .. } if session_id == with_bob
    ));
    drop(alice);
    assert!(matches!(
        carol.recv().await,
        ServerToClient::Bye { from, session_id } if from == alice_id && session_id == with_carol
    ));
    bob.expect_silence().await;
}

#[tokio::test]
async fn refuses_a_binary_frame_without_dropping_the_client() {
    let relay = TestRelay::start().await;
    let (mut client, _) = relay.registered(Some(peer(111_111_111)), None).await;

    timeout(DEADLINE, client.ws.send(Message::binary(vec![0u8, 1, 2])))
        .await
        .expect("the send completed within the deadline")
        .expect("the socket accepted the frame");

    match client.recv().await {
        ServerToClient::Error { code, .. } => assert_eq!(code, ErrorCode::MalformedMessage),
        other => panic!("expected malformed-message, got {other:?}"),
    }
    client.send(&ClientToServer::Ping).await;
    assert_eq!(client.recv().await, ServerToClient::Pong);
}

#[tokio::test]
async fn rate_limits_a_flood_of_messages_from_one_client() {
    let relay = TestRelay::start_with(RelayConfig {
        messages_per_second: 1.0,
        message_burst: 3,
        ..Default::default()
    })
    .await;
    let mut client = relay.client().await;
    // The burst is spent by `register` plus the pings below, so the limit is
    // reached inside one test without waiting for a refill.
    client
        .register(ClientToServer::Register {
            protocol: PROTOCOL_VERSION,
            preferred_id: Some(peer(111_111_111)),
            alias: None,
            token: None,
        })
        .await;

    let mut limited = None;
    for _ in 0..6 {
        client.send(&ClientToServer::Ping).await;
        match client.recv().await {
            ServerToClient::Pong => continue,
            ServerToClient::Error { code, .. } => {
                limited = Some(code);
                break;
            }
            other => panic!("expected a pong or an error, got {other:?}"),
        }
    }
    assert_eq!(
        limited,
        Some(ErrorCode::RateLimited),
        "a client past its quota must be told, not silently served"
    );
    assert!(
        relay.handle.is_online(peer(111_111_111)),
        "rate limiting throttles, it does not disconnect"
    );
}

#[tokio::test]
async fn rate_limits_registrations_from_one_address() {
    let relay = TestRelay::start_with(RelayConfig {
        registrations_per_minute: 1.0,
        registration_burst: 2,
        ..Default::default()
    })
    .await;

    let mut outcomes = Vec::new();
    let mut clients = Vec::new();
    for _ in 0..4 {
        let mut client = relay.client().await;
        client
            .send(&ClientToServer::Register {
                protocol: PROTOCOL_VERSION,
                preferred_id: None,
                alias: None,
                token: None,
            })
            .await;
        outcomes.push(client.recv().await);
        clients.push(client);
    }

    let welcomed = outcomes
        .iter()
        .filter(|msg| matches!(msg, ServerToClient::Welcome { .. }))
        .count();
    assert_eq!(welcomed, 2, "only the burst may register: {outcomes:?}");
    for refused in &outcomes[2..] {
        match refused {
            ServerToClient::Error { code, .. } => assert_eq!(*code, ErrorCode::RateLimited),
            other => panic!("expected rate-limited, got {other:?}"),
        }
    }
    assert_eq!(
        relay.handle.connected_clients(),
        2,
        "a rate-limited register must not have taken an id"
    );
}
