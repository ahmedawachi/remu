//! Binary framing for file chunks on the bulk data channel.
//!
//! The Electron original base64-encoded every chunk into a JSON string, paying
//! a 33% bandwidth tax plus an encode and a parse on each one. The channel is
//! already binary-capable, so chunks go out as bytes behind a fixed 25-byte
//! header:
//!
//! ```text
//! 0        1                          17                   25
//! +--------+--------------------------+--------------------+----------- ...
//! | version|   transfer id (uuid)     |  sequence (u64 BE) |  payload
//! +--------+--------------------------+--------------------+----------- ...
//! ```
//!
//! The sequence number is explicit even though the channel is ordered: it makes
//! a truncated or duplicated write detectable rather than silently corrupting
//! the received file.

use uuid::Uuid;

/// Framing version, bumped if the header layout ever changes.
pub const BULK_VERSION: u8 = 1;

/// Size of the fixed header preceding every chunk payload.
pub const BULK_HEADER_BYTES: usize = 1 + 16 + 8;

/// Payload bytes per chunk.
///
/// 16 KiB is the largest message every SCTP implementation accepts without
/// negotiating fragmentation, including browsers — worth staying under so a
/// future web client can join without a second framing path. Throughput is
/// governed by the send window in the transfer loop, not by chunk size.
pub const BULK_CHUNK_BYTES: usize = 16 * 1024;

/// Largest frame that can legitimately arrive.
pub const MAX_BULK_FRAME_BYTES: usize = BULK_HEADER_BYTES + BULK_CHUNK_BYTES;

// Raising the chunk size past 64 KiB would break interoperability with browsers
// and some SCTP stacks. Enforced at compile time so it can never become a field
// report: raising BULK_CHUNK_BYTES too far simply fails the build.
const _: () = assert!(MAX_BULK_FRAME_BYTES <= 64 * 1024);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BulkError {
    #[error("bulk frame is {0} bytes, shorter than the {BULK_HEADER_BYTES}-byte header")]
    TooShort(usize),
    #[error("bulk frame is {0} bytes, over the {MAX_BULK_FRAME_BYTES}-byte limit")]
    TooLong(usize),
    #[error("unsupported bulk framing version {0}")]
    UnsupportedVersion(u8),
}

/// One decoded chunk, borrowing the payload from the received buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkChunk<'a> {
    pub transfer_id: Uuid,
    pub seq: u64,
    pub payload: &'a [u8],
}

/// Frames one chunk for the wire.
pub fn encode_chunk(transfer_id: Uuid, seq: u64, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(BULK_HEADER_BYTES + payload.len());
    out.push(BULK_VERSION);
    out.extend_from_slice(transfer_id.as_bytes());
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Parses a frame received from a peer.
///
/// Every field is checked before use: the peer is not trusted to have produced
/// this, and an oversized frame is rejected outright rather than allocated for.
pub fn decode_chunk(frame: &[u8]) -> Result<BulkChunk<'_>, BulkError> {
    if frame.len() < BULK_HEADER_BYTES {
        return Err(BulkError::TooShort(frame.len()));
    }
    if frame.len() > MAX_BULK_FRAME_BYTES {
        return Err(BulkError::TooLong(frame.len()));
    }
    let version = frame[0];
    if version != BULK_VERSION {
        return Err(BulkError::UnsupportedVersion(version));
    }
    let mut id = [0u8; 16];
    id.copy_from_slice(&frame[1..17]);
    let mut seq = [0u8; 8];
    seq.copy_from_slice(&frame[17..25]);
    Ok(BulkChunk {
        transfer_id: Uuid::from_bytes(id),
        seq: u64::from_be_bytes(seq),
        payload: &frame[BULK_HEADER_BYTES..],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_chunk() {
        let id = Uuid::new_v4();
        let payload = b"the quick brown fox";
        let frame = encode_chunk(id, 42, payload);
        let chunk = decode_chunk(&frame).unwrap();
        assert_eq!(chunk.transfer_id, id);
        assert_eq!(chunk.seq, 42);
        assert_eq!(chunk.payload, payload);
    }

    #[test]
    fn round_trips_a_full_size_and_an_empty_payload() {
        let id = Uuid::new_v4();

        let full = vec![0xABu8; BULK_CHUNK_BYTES];
        let frame = encode_chunk(id, 0, &full);
        assert_eq!(frame.len(), MAX_BULK_FRAME_BYTES);
        assert_eq!(decode_chunk(&frame).unwrap().payload, &full[..]);

        // A zero-length final chunk is legal and must not be mistaken for a
        // truncated frame.
        let empty = encode_chunk(id, 1, &[]);
        assert_eq!(empty.len(), BULK_HEADER_BYTES);
        assert!(decode_chunk(&empty).unwrap().payload.is_empty());
    }

    #[test]
    fn preserves_sequence_numbers_at_the_extremes() {
        let id = Uuid::new_v4();
        for seq in [0, 1, u64::MAX / 2, u64::MAX] {
            let frame = encode_chunk(id, seq, b"x");
            assert_eq!(decode_chunk(&frame).unwrap().seq, seq);
        }
    }

    #[test]
    fn rejects_a_frame_shorter_than_the_header() {
        for len in 0..BULK_HEADER_BYTES {
            let frame = vec![BULK_VERSION; len];
            assert_eq!(decode_chunk(&frame), Err(BulkError::TooShort(len)));
        }
    }

    #[test]
    fn rejects_a_frame_over_the_size_limit() {
        let frame = vec![BULK_VERSION; MAX_BULK_FRAME_BYTES + 1];
        assert_eq!(
            decode_chunk(&frame),
            Err(BulkError::TooLong(MAX_BULK_FRAME_BYTES + 1))
        );
    }

    #[test]
    fn rejects_an_unknown_framing_version() {
        let mut frame = encode_chunk(Uuid::new_v4(), 0, b"x");
        frame[0] = 99;
        assert_eq!(decode_chunk(&frame), Err(BulkError::UnsupportedVersion(99)));
    }

    #[test]
    fn header_layout_is_exactly_as_documented() {
        let id = Uuid::from_u128(0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10);
        let frame = encode_chunk(id, 0x1122_3344_5566_7788, b"!");
        assert_eq!(frame[0], BULK_VERSION);
        assert_eq!(&frame[1..17], id.as_bytes());
        assert_eq!(
            &frame[17..25],
            &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]
        );
        assert_eq!(&frame[25..], b"!");
    }
}
