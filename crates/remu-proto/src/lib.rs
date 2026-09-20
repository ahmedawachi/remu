//! The Remu wire protocol.
//!
//! Everything two Remu peers say to each other, and everything they say to
//! the relay, is defined here. The crate is deliberately free of platform,
//! async and media dependencies so that the relay server, the desk app and any
//! future third-party client all agree on one definition rather than three
//! drifting copies.
//!
//! - [`signaling`] — relay messages: ID allocation, SDP and ICE forwarding.
//! - [`control`] — the peer-to-peer control channel: input, chat, clipboard, files.
//! - [`bulk`] — binary framing for file chunks on the second data channel.
//! - [`input`] — platform-neutral keyboard and pointer events.
//! - [`auth`] — replay-resistant unattended-access proofs.
//! - [`settings`] — persisted settings and the address book.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod auth;
pub mod bulk;
pub mod control;
pub mod input;
pub mod peer;
pub mod settings;
pub mod signaling;

pub use bulk::{decode_chunk, encode_chunk, BulkChunk, BULK_CHUNK_BYTES};
pub use control::{
    sanitize_filename, ControlMessage, DisplayInfo, HostPermissions, SessionStats, TransferError,
    BULK_CHANNEL, CONTROL_CHANNEL,
};
pub use input::{InputEvent, KeyCode, KeyModifiers, MouseButton};
pub use peer::{IdError, PeerId, SessionId, PEER_ID_MAX, PEER_ID_MIN};
pub use settings::{AcceptPolicy, ConnectionRecord, Quality, Settings, Theme, TurnConfig};
pub use signaling::{
    ClientToServer, ErrorCode, IceCandidate, RejectReason, SdpKind, ServerToClient,
    SessionDescription, MAX_SIGNALING_MESSAGE_BYTES,
};

/// Version of the protocol in this crate.
///
/// Sent in `register` and echoed in `welcome`. A mismatch is reported rather
/// than tolerated: two peers that disagree about framing fail at the handshake
/// with a clear message instead of halfway through a session.
pub const PROTOCOL_VERSION: u16 = 1;

/// Name and version the relay reports in `welcome`, for the client status bar.
pub fn server_banner() -> String {
    format!("remu-relay/{}", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_names_the_server_and_its_version() {
        let banner = server_banner();
        assert!(banner.starts_with("remu-relay/"));
        assert!(banner.len() > "remu-relay/".len());
    }

    #[test]
    fn protocol_version_is_pinned() {
        // A deliberate tripwire: changing the wire format means changing this
        // number, and changing this number means updating the relay's
        // compatibility check and this test together.
        assert_eq!(PROTOCOL_VERSION, 1);
    }
}
