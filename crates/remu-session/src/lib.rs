//! Relay signalling and the peer-to-peer session transport.

#![forbid(unsafe_code)]

pub mod backoff;
pub mod error;
pub mod peer;
pub mod relay;
pub mod sdp;
pub mod transfer;
pub mod video;

pub use backoff::Backoff;
pub use error::SessionError;
pub use peer::{
    event_channel, ConnState, IceConfig, PeerSession, Role, SessionEvent, EVENT_CHANNEL_CAPACITY,
};
pub use relay::{RelayClient, RelayEvent};
pub use transfer::{
    BulkSink, CancelToken, FileReceiver, FileSender, TransferProgress, TransferState,
};
