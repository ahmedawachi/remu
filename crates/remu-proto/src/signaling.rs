//! Messages exchanged with the relay server.
//!
//! The relay is a dumb forwarder: it allocates desk IDs and copies envelopes
//! between two peers. It never holds a key, never sees decrypted media, and
//! cannot read the unattended password (see [`crate::auth`]).
//!
//! # Connection handshake
//!
//! ```text
//! controller                  relay                    host
//!     |-- connect ------------->|-- connect ------------>|
//!     |<------------ challenge -|<- challenge (nonce) ---|
//!     |-- offer (sdp, auth) --->|-- offer -------------->|   host verifies auth
//!     |<---------- answer (sdp)-|<- answer --------------|
//!     |<==== ice ==============>|<===== ice ============>|
//!     |<~~~~~~~~ encrypted peer-to-peer media ~~~~~~~~~~>|
//! ```
//!
//! The `connect`/`challenge` exchange is the one structural change from the
//! Electron original, which sent `sha256(password)` inside the offer. That hash
//! was a fixed value: anyone who observed one offer could replay it forever.
//! The host now issues a fresh single-use nonce per attempt, so an observed
//! proof authenticates nothing else. It also lets a busy host decline before
//! the controller spends anything on ICE gathering.

use serde::{Deserialize, Serialize};

use crate::peer::{PeerId, SessionId};

/// Relay messages larger than this are dropped and the sender disconnected.
/// SDP for a screen-share offer runs a few kilobytes; 256 KiB is generous
/// enough to never trip on a legitimate message and small enough that a
/// hostile client cannot exhaust the relay's memory.
pub const MAX_SIGNALING_MESSAGE_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SdpKind {
    Offer,
    Answer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDescription {
    #[serde(rename = "type")]
    pub kind: SdpKind,
    pub sdp: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IceCandidate {
    pub candidate: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sdp_mid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sdp_mline_index: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username_fragment: Option<String>,
}

/// Why a session attempt ended before it began.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RejectReason {
    /// A human at the host clicked Decline.
    Declined,
    /// The host is already in a session.
    Busy,
    /// The relay has no such peer connected.
    PeerOffline,
    /// The unattended password proof did not verify.
    BadPassword,
    /// The host could not capture its screen (permission, or no display).
    CaptureFailed,
    /// Nobody answered the prompt in time.
    Timeout,
    /// The controller gave up before the host answered.
    Cancelled,
    /// The two sides do not speak the same protocol version.
    ProtocolMismatch,
    /// Something else; see the accompanying `detail`.
    Other,
}

/// Machine-readable failures reported by the relay itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCode {
    MalformedMessage,
    NotRegistered,
    AlreadyRegistered,
    UnsupportedProtocol,
    Unauthorized,
    RateLimited,
    MessageTooLarge,
    IdUnavailable,
    PeerOffline,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ClientToServer {
    /// First message on every connection. `preferred_id` asks to keep the ID
    /// from a previous connection so saved contacts stay reachable across a
    /// network blip; the relay grants it only if it is currently unused.
    #[serde(rename_all = "camelCase")]
    Register {
        protocol: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preferred_id: Option<PeerId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alias: Option<String>,
        /// Shared secret, when the relay is configured to require one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Lookup {
        peer_id: PeerId,
    },
    #[serde(rename_all = "camelCase")]
    Connect {
        to: PeerId,
        session_id: SessionId,
    },
    #[serde(rename_all = "camelCase")]
    Challenge {
        to: PeerId,
        session_id: SessionId,
        nonce: String,
        /// Whether the host will require a password proof in the offer.
        needs_password: bool,
    },
    #[serde(rename_all = "camelCase")]
    Offer {
        to: PeerId,
        session_id: SessionId,
        sdp: SessionDescription,
        /// Hex HMAC over the host's nonce; see [`crate::auth`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Answer {
        to: PeerId,
        session_id: SessionId,
        sdp: SessionDescription,
    },
    #[serde(rename_all = "camelCase")]
    Ice {
        to: PeerId,
        session_id: SessionId,
        candidate: IceCandidate,
    },
    #[serde(rename_all = "camelCase")]
    Reject {
        to: PeerId,
        session_id: SessionId,
        reason: RejectReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Bye {
        to: PeerId,
        session_id: SessionId,
    },
    Ping,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ServerToClient {
    #[serde(rename_all = "camelCase")]
    Welcome {
        peer_id: PeerId,
        protocol: u16,
        /// Server name and version, for the client's status bar.
        server: String,
    },
    #[serde(rename_all = "camelCase")]
    LookupResult {
        peer_id: PeerId,
        online: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alias: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Connect {
        from: PeerId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_alias: Option<String>,
        session_id: SessionId,
    },
    #[serde(rename_all = "camelCase")]
    Challenge {
        from: PeerId,
        session_id: SessionId,
        nonce: String,
        needs_password: bool,
    },
    #[serde(rename_all = "camelCase")]
    Offer {
        from: PeerId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_alias: Option<String>,
        session_id: SessionId,
        sdp: SessionDescription,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Answer {
        from: PeerId,
        session_id: SessionId,
        sdp: SessionDescription,
    },
    #[serde(rename_all = "camelCase")]
    Ice {
        from: PeerId,
        session_id: SessionId,
        candidate: IceCandidate,
    },
    #[serde(rename_all = "camelCase")]
    Reject {
        from: PeerId,
        session_id: SessionId,
        reason: RejectReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Bye {
        from: PeerId,
        session_id: SessionId,
    },
    Error {
        code: ErrorCode,
        message: String,
    },
    Pong,
}

impl ClientToServer {
    /// The peer this message should be forwarded to, if any.
    ///
    /// The relay routes on this rather than matching every variant at the call
    /// site, so adding a peer-to-peer message type cannot accidentally create
    /// one the relay silently drops.
    pub fn destination(&self) -> Option<PeerId> {
        match self {
            ClientToServer::Connect { to, .. }
            | ClientToServer::Challenge { to, .. }
            | ClientToServer::Offer { to, .. }
            | ClientToServer::Answer { to, .. }
            | ClientToServer::Ice { to, .. }
            | ClientToServer::Reject { to, .. }
            | ClientToServer::Bye { to, .. } => Some(*to),
            ClientToServer::Register { .. }
            | ClientToServer::Lookup { .. }
            | ClientToServer::Ping => None,
        }
    }

    /// The session this message belongs to, if any.
    pub fn session(&self) -> Option<SessionId> {
        match self {
            ClientToServer::Connect { session_id, .. }
            | ClientToServer::Challenge { session_id, .. }
            | ClientToServer::Offer { session_id, .. }
            | ClientToServer::Answer { session_id, .. }
            | ClientToServer::Ice { session_id, .. }
            | ClientToServer::Reject { session_id, .. }
            | ClientToServer::Bye { session_id, .. } => Some(*session_id),
            ClientToServer::Register { .. }
            | ClientToServer::Lookup { .. }
            | ClientToServer::Ping => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(n: u32) -> PeerId {
        PeerId::new(n).unwrap()
    }

    #[test]
    fn tags_messages_by_type_in_kebab_case() {
        let msg = ClientToServer::Register {
            protocol: 1,
            preferred_id: None,
            alias: Some("Reception PC".into()),
            token: None,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "register");
        assert_eq!(json["alias"], "Reception PC");
        // Absent optionals stay out of the wire form entirely.
        assert!(json.get("preferredId").is_none());
        assert!(json.get("token").is_none());
    }

    #[test]
    fn uses_camel_case_field_names_for_cross_language_clients() {
        let msg = ServerToClient::LookupResult {
            peer_id: peer(123_456_789),
            online: true,
            alias: None,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "lookup-result");
        assert_eq!(json["peerId"], "123456789");
    }

    #[test]
    fn routes_every_peer_to_peer_variant_and_no_others() {
        let to = peer(222_222_222);
        let sid = SessionId::random();
        let routed = vec![
            ClientToServer::Connect {
                to,
                session_id: sid,
            },
            ClientToServer::Challenge {
                to,
                session_id: sid,
                nonce: "n".into(),
                needs_password: false,
            },
            ClientToServer::Offer {
                to,
                session_id: sid,
                sdp: SessionDescription {
                    kind: SdpKind::Offer,
                    sdp: "v=0".into(),
                },
                auth: None,
            },
            ClientToServer::Answer {
                to,
                session_id: sid,
                sdp: SessionDescription {
                    kind: SdpKind::Answer,
                    sdp: "v=0".into(),
                },
            },
            ClientToServer::Ice {
                to,
                session_id: sid,
                candidate: IceCandidate {
                    candidate: "candidate:1".into(),
                    sdp_mid: Some("0".into()),
                    sdp_mline_index: Some(0),
                    username_fragment: None,
                },
            },
            ClientToServer::Reject {
                to,
                session_id: sid,
                reason: RejectReason::Busy,
                detail: None,
            },
            ClientToServer::Bye {
                to,
                session_id: sid,
            },
        ];
        for msg in &routed {
            assert_eq!(msg.destination(), Some(to), "{msg:?} should be routed");
            assert_eq!(msg.session(), Some(sid), "{msg:?} should carry a session");
        }

        let local = vec![
            ClientToServer::Register {
                protocol: 1,
                preferred_id: None,
                alias: None,
                token: None,
            },
            ClientToServer::Lookup { peer_id: to },
            ClientToServer::Ping,
        ];
        for msg in &local {
            assert_eq!(msg.destination(), None, "{msg:?} should not be routed");
            assert_eq!(msg.session(), None);
        }
    }

    #[test]
    fn round_trips_a_full_offer_through_json() {
        let msg = ServerToClient::Offer {
            from: peer(111_111_111),
            from_alias: Some("Ahmed".into()),
            session_id: SessionId::random(),
            sdp: SessionDescription {
                kind: SdpKind::Offer,
                sdp: "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\n".into(),
            },
            auth: Some("deadbeef".into()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<ServerToClient>(&json).unwrap(), msg);
    }

    #[test]
    fn round_trips_every_reject_reason() {
        for reason in [
            RejectReason::Declined,
            RejectReason::Busy,
            RejectReason::PeerOffline,
            RejectReason::BadPassword,
            RejectReason::CaptureFailed,
            RejectReason::Timeout,
            RejectReason::Cancelled,
            RejectReason::ProtocolMismatch,
            RejectReason::Other,
        ] {
            let json = serde_json::to_string(&reason).unwrap();
            assert_eq!(serde_json::from_str::<RejectReason>(&json).unwrap(), reason);
        }
    }

    #[test]
    fn rejects_an_unknown_message_type() {
        let err = serde_json::from_str::<ClientToServer>(r#"{"type":"shutdown"}"#);
        assert!(err.is_err());
    }
}
