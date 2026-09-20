//! The one error type this crate returns.

use uuid::Uuid;

/// Everything that can go wrong in the signalling client, the peer session and
/// the file-transfer helpers.
///
/// One enum rather than three: an integrator driving a session is already
/// handling SDP, data-channel and transfer failures in the same match arm, and
/// splitting them would only move the `From` conversions into the caller.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// A data channel was asked to send while not open — the peer closed it, or
    /// the SCTP handshake has not finished yet.
    #[error("the {label} data channel is not open (state: {state})")]
    ChannelNotOpen {
        /// The channel label, e.g. `remu.ctrl`.
        label: &'static str,
        /// The channel's ready state at the time of the attempt.
        state: String,
    },

    /// The peer connection has been closed, by us or by the remote side.
    #[error("the peer session is closed")]
    Closed,

    /// The underlying webrtc-rs stack refused an operation.
    #[error("webrtc: {0}")]
    WebRtc(#[from] webrtc::error::Error),

    /// An SDP kind arrived that this protocol does not use. Remu only ever
    /// exchanges offers and answers; `pranswer` and `rollback` are rejected
    /// rather than silently coerced.
    #[error("unsupported SDP type {0:?}; remu exchanges only offers and answers")]
    UnsupportedSdpType(rtc::peer_connection::sdp::RTCSdpType),

    /// A control message could not be encoded to, or decoded from, JSON.
    #[error("control message JSON: {0}")]
    ControlJson(#[from] serde_json::Error),

    /// A bulk payload was larger than one frame may carry.
    #[error("bulk payload is {len} bytes, over the {max}-byte chunk limit")]
    BulkPayloadTooLarge {
        /// Size the caller offered.
        len: usize,
        /// The limit, [`remu_proto::BULK_CHUNK_BYTES`].
        max: usize,
    },

    /// [`crate::PeerSession::write_video`] was called on a controller. Only the
    /// host owns a video track; the controller's transceiver is receive-only.
    #[error("only the host sends video; this session's role is controller")]
    NotSendingVideo,

    /// The host's video track exists but negotiation has not yet produced an
    /// SSRC and payload type for it, so a sample cannot be packetized.
    #[error("the video track has no negotiated codec yet; write after the answer is applied")]
    VideoNotNegotiated,

    /// Reading a file to send, or writing a received one, failed.
    #[error("file {path}: {source}")]
    Io {
        /// The path being read or written.
        path: String,
        /// The underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// A received chunk did not continue the sequence. The bulk channel is
    /// ordered and reliable, so a gap means a bug or a hostile peer, and the
    /// chunk is refused rather than written at the wrong offset.
    #[error("transfer {transfer_id}: expected chunk {expected}, got {got}")]
    ChunkOutOfOrder {
        /// The transfer the chunk claimed to belong to.
        transfer_id: Uuid,
        /// The sequence number the receiver was waiting for.
        expected: u64,
        /// The sequence number that arrived.
        got: u64,
    },

    /// The sender pushed more bytes than the `FileMeta` it announced.
    #[error("transfer {transfer_id}: {received} bytes received, {declared} announced")]
    TransferOversized {
        /// The transfer that overran.
        transfer_id: Uuid,
        /// Bytes received so far, including the offending chunk.
        received: u64,
        /// The size the sender declared up front.
        declared: u64,
    },

    /// The sender declared one size in `FileMeta` and then ended the transfer
    /// short of it. Refused rather than published: the UI showed the declared
    /// size all along, and a silently truncated file looks complete.
    #[error("transfer {transfer_id}: {received} bytes received, {declared} announced")]
    TransferTruncated {
        /// The transfer that ended early.
        transfer_id: Uuid,
        /// Bytes actually received.
        received: u64,
        /// The size the sender declared up front.
        declared: u64,
    },

    /// The received bytes did not hash to the digest the sender promised.
    #[error("transfer {transfer_id}: sha-256 mismatch (expected {expected}, computed {computed})")]
    ChecksumMismatch {
        /// The transfer that failed verification.
        transfer_id: Uuid,
        /// The digest from the sender's `FileEnd`.
        expected: String,
        /// What the received bytes actually hash to.
        computed: String,
    },

    /// The local user or the peer aborted the transfer.
    #[error("transfer {transfer_id} was cancelled")]
    TransferCancelled {
        /// The transfer that was abandoned.
        transfer_id: Uuid,
    },

    /// No free filename could be found in the download directory.
    #[error("could not find an unused name for {name:?} in {dir}")]
    NoFreeFilename {
        /// The sanitized name that collided.
        name: String,
        /// The download directory that was searched.
        dir: String,
    },
}
