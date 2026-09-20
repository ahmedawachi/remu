//! Reassembly of an H.264 Annex-B bitstream from received RTP packets.
//!
//! The controller receives RTP, but the decoder downstream of it wants whole
//! access units with start codes. webrtc-rs stops at the packet: it hands over
//! `rtp::Packet`s and leaves depacketization to the application, so this is
//! where FU-A fragments and STAP-A aggregates are put back together.
//!
//! Kept free of any webrtc-rs connection state so it can be driven straight
//! from synthetic packets in tests.

use bytes::{Bytes, BytesMut};
use rtc::rtp::codec::h264::H264Packet;
use rtc::rtp::packetizer::Depacketizer;
use rtc::rtp::Packet;

/// The H.264 RTP clock, fixed at 90 kHz by RFC 6184.
const CLOCK_HZ: u64 = 90_000;

/// Ceiling on one access unit under construction.
///
/// Nothing in RTP bounds a frame: the buffers here only empty at a marker bit
/// or a timestamp change, so a host that sends fragments forever with neither
/// grows them until the process dies — measured at 120 MB from 100,000
/// 1.2 KB packets, which was then handed to the UI as a "valid" frame. The
/// media path has no relay message cap or rate limiter in front of it, so the
/// bound has to live here.
///
/// 32 MiB cannot reject anything real: a 4K keyframe at a sane quality is a
/// few megabytes, and Remu's own encoder is capped far below that.
pub const MAX_ACCESS_UNIT_BYTES: usize = 32 * 1024 * 1024;

/// One reassembled access unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFrame {
    /// Annex-B bytes: every NAL unit prefixed with `00 00 00 01`.
    pub data: Bytes,
    /// Presentation time in microseconds since the first packet of the stream,
    /// derived from the RTP timestamp with wraparound unwrapped.
    pub timestamp_us: u64,
}

/// Turns a stream of RTP packets back into Annex-B frames.
#[derive(Debug, Default)]
pub struct H264Assembler {
    depacketizer: H264Packet,
    pending: BytesMut,
    /// RTP timestamp of the frame currently being assembled.
    pending_timestamp: Option<u32>,
    /// Timestamp of the first packet ever seen, so output times start near zero
    /// instead of at whatever random offset the sender chose.
    epoch: Option<u32>,
    /// Count of 2³² wraps of the RTP timestamp. A 90 kHz clock wraps every
    /// ~13 hours, well inside a plausible unattended session.
    wraps: u64,
    last_timestamp: u32,
    /// Sequence number of the previous packet, for gap detection.
    last_sequence: Option<u16>,
    /// Payload bytes fed into the frame being assembled. Counted separately
    /// from `pending.len()` because an FU-A run that never ends its NAL sits
    /// inside the depacketizer's own buffer instead, where it is just as
    /// unbounded and not visible from here.
    au_input_bytes: usize,
    /// Set when a gap or a decode failure has ruined the frame being built.
    /// Cleared at every frame boundary.
    frame_damaged: bool,
    /// Sticky: something has been lost since the last [`clear_loss`]. Kept
    /// separate from `frame_damaged` because it answers a different question
    /// -- "should we ask the host for a keyframe?" -- and must survive the
    /// frame boundary to do so. Folding the two together meant the flag was
    /// wiped by the next frame before the session ever read it, so no keyframe
    /// was ever requested and a damaged stream never recovered.
    lost: bool,
}

impl H264Assembler {
    /// Feeds one packet in. Returns a frame once its final packet arrives.
    ///
    /// A frame that lost packets is dropped rather than emitted: a truncated
    /// access unit makes most decoders emit garbage or stall, and the next
    /// keyframe recovers faster than the artefacts would clear.
    pub fn push(&mut self, packet: &Packet) -> Option<VideoFrame> {
        let timestamp = packet.header.timestamp;
        // Observed for every packet, including ones whose frame is discarded:
        // if the wrap were only noticed on emitted frames, a burst of loss
        // spanning the wrap would lose 13 hours of presentation time.
        self.observe_clock(timestamp);

        if self.pending_timestamp.is_some_and(|ts| ts != timestamp) {
            // A new timestamp without the previous frame's marker bit: the
            // marker was lost, so the half-built frame is unusable.
            tracing::debug!("h264: frame boundary missed, discarding a partial access unit");
            self.mark_lost();
            self.reset_frame();
        }

        if let Some(previous) = self.last_sequence {
            if packet.header.sequence_number != previous.wrapping_add(1) {
                self.mark_lost();
            }
        }
        self.last_sequence = Some(packet.header.sequence_number);
        self.pending_timestamp = Some(timestamp);

        self.au_input_bytes = self.au_input_bytes.saturating_add(packet.payload.len());

        match self.depacketizer.depacketize(&packet.payload) {
            // An empty result is the normal case for a non-final FU-A fragment.
            // A damaged frame is dropped at the marker regardless, so its bytes
            // are not worth keeping.
            Ok(part) if !self.frame_damaged => self.pending.extend_from_slice(&part),
            Ok(_) => {}
            Err(err) => {
                tracing::debug!(%err, "h264: undecodable RTP payload");
                self.mark_lost();
            }
        }

        if self.au_input_bytes > MAX_ACCESS_UNIT_BYTES || self.pending.len() > MAX_ACCESS_UNIT_BYTES
        {
            self.discard_oversized_frame();
        }

        if !packet.header.marker {
            return None;
        }

        let complete = !self.frame_damaged && !self.pending.is_empty();
        let data = self.pending.split().freeze();
        self.reset_frame();

        if !complete {
            return None;
        }

        Some(VideoFrame {
            data,
            timestamp_us: self.presentation_us(timestamp),
        })
    }

    /// Whether the assembler has seen loss since the last
    /// [`clear_loss`](Self::clear_loss).
    ///
    /// The session uses this to decide when to ask the host for a keyframe: a
    /// PLI is worth sending only after something was actually dropped.
    pub fn lost_data(&self) -> bool {
        self.lost
    }

    /// Acknowledges the loss flag after a keyframe request has gone out.
    pub fn clear_loss(&mut self) {
        self.lost = false;
    }

    /// Throws away an access unit that grew past [`MAX_ACCESS_UNIT_BYTES`].
    ///
    /// The frame stays marked damaged until its boundary, so nothing partial is
    /// emitted, and the sticky loss flag makes the session ask for a keyframe.
    fn discard_oversized_frame(&mut self) {
        tracing::warn!(
            pending = self.pending.len(),
            input = self.au_input_bytes,
            limit = MAX_ACCESS_UNIT_BYTES,
            "h264: access unit over the size limit, discarding it"
        );
        self.mark_lost();
        // Replaced, not `clear()`ed: clearing keeps the capacity, which would
        // leave the ceiling allocated for the rest of the session.
        self.pending = BytesMut::new();
        self.au_input_bytes = 0;
        // Frees whatever partial FU-A run the depacketizer is holding; it is
        // rebuilt again at the frame boundary, which is harmless.
        self.depacketizer = H264Packet::default();
    }

    fn mark_lost(&mut self) {
        self.frame_damaged = true;
        self.lost = true;
    }

    fn reset_frame(&mut self) {
        self.pending.clear();
        self.pending_timestamp = None;
        self.au_input_bytes = 0;
        if self.frame_damaged {
            // A fragment run cut short leaves the depacketizer holding a
            // partial FU-A buffer, which would otherwise be prepended to the
            // next healthy frame.
            self.depacketizer = H264Packet::default();
        }
        self.frame_damaged = false;
    }

    /// Updates the epoch and wrap count from one packet's timestamp.
    fn observe_clock(&mut self, timestamp: u32) {
        if self.epoch.is_none() {
            self.epoch = Some(timestamp);
            self.last_timestamp = timestamp;
            return;
        }

        if timestamp < self.last_timestamp {
            // Timestamps only ever go forwards within a stream, so a decrease
            // that is not a tiny reordering is the counter wrapping.
            let backwards = self.last_timestamp.wrapping_sub(timestamp);
            if backwards > u32::MAX / 2 {
                self.wraps += 1;
            }
        }
        self.last_timestamp = timestamp;
    }

    /// Converts an RTP timestamp to microseconds since the first packet seen.
    fn presentation_us(&self, timestamp: u32) -> u64 {
        let epoch = self.epoch.unwrap_or(timestamp);
        let extended = self.wraps * (u32::MAX as u64 + 1) + timestamp as u64;
        let from_epoch = extended.saturating_sub(epoch as u64);
        from_epoch.saturating_mul(1_000_000) / CLOCK_HZ
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtc::rtp::header::Header;

    const START_CODE: &[u8] = &[0, 0, 0, 1];

    fn packet(seq: u16, timestamp: u32, marker: bool, payload: &[u8]) -> Packet {
        Packet {
            header: Header {
                version: 2,
                marker,
                payload_type: 102,
                sequence_number: seq,
                timestamp,
                ssrc: 0xDEAD_BEEF,
                ..Default::default()
            },
            payload: Bytes::copy_from_slice(payload),
        }
    }

    /// A single NAL unit small enough to travel in one packet: NALU type 1
    /// (non-IDR slice) with two bytes of payload.
    ///
    /// Two bytes, not one: RFC 6184 allows a 2-byte NAL only for an access
    /// unit delimiter, and the depacketizer rejects anything else that short
    /// as a truncated packet.
    fn single_nal(body: u8) -> Vec<u8> {
        vec![0x41, body, 0x00]
    }

    #[test]
    fn a_single_packet_frame_comes_out_as_annex_b() {
        let mut assembler = H264Assembler::default();
        let frame = assembler
            .push(&packet(1, 9000, true, &single_nal(0xAA)))
            .expect("a marked single-NAL packet is a whole frame");

        let mut want = START_CODE.to_vec();
        want.extend_from_slice(&single_nal(0xAA));
        assert_eq!(frame.data.as_ref(), want.as_slice());
    }

    #[test]
    fn packets_before_the_marker_produce_nothing() {
        let mut assembler = H264Assembler::default();
        assert!(assembler
            .push(&packet(1, 9000, false, &single_nal(0x01)))
            .is_none());
        assert!(assembler
            .push(&packet(2, 9000, true, &single_nal(0x02)))
            .is_some());
    }

    /// FU-A: one NAL split across packets. The reassembled unit must carry a
    /// single start code and one rebuilt NAL header, not one per fragment.
    #[test]
    fn a_fragmented_nal_is_rebuilt_into_one_unit() {
        let mut assembler = H264Assembler::default();
        // FU indicator 0x7C (type 28, nri 3), FU header 0x85 = start + type 5.
        let start = [0x7C, 0x85, 0x11, 0x22];
        // FU header 0x45 = end + type 5.
        let end = [0x7C, 0x45, 0x33, 0x44];

        assert!(assembler.push(&packet(1, 9000, false, &start)).is_none());
        let frame = assembler
            .push(&packet(2, 9000, true, &end))
            .expect("the end fragment completes the unit");

        let mut want = START_CODE.to_vec();
        want.push(0x65); // nri 3 | type 5
        want.extend_from_slice(&[0x11, 0x22, 0x33, 0x44]);
        assert_eq!(frame.data.as_ref(), want.as_slice());
    }

    #[test]
    fn a_frame_with_a_sequence_gap_is_dropped_and_flagged() {
        let mut assembler = H264Assembler::default();
        assembler.push(&packet(1, 9000, false, &single_nal(0x01)));
        // Sequence jumps from 1 to 3: packet 2 never arrived.
        assert!(assembler
            .push(&packet(3, 9000, true, &single_nal(0x02)))
            .is_none());
        assert!(assembler.lost_data());
    }

    #[test]
    fn the_loss_flag_clears_once_a_keyframe_has_been_requested() {
        let mut assembler = H264Assembler::default();
        assembler.push(&packet(1, 9000, false, &single_nal(0x01)));
        assembler.push(&packet(9, 9000, true, &single_nal(0x02)));
        assert!(assembler.lost_data());
        assembler.clear_loss();
        assert!(!assembler.lost_data());
    }

    /// A lost marker bit must not glue two access units together: the stale
    /// bytes are dropped when the timestamp moves on.
    #[test]
    fn a_missed_marker_discards_the_partial_frame_rather_than_merging_it() {
        let mut assembler = H264Assembler::default();
        assembler.push(&packet(1, 9000, false, &single_nal(0x01)));

        let frame = assembler
            .push(&packet(2, 12_000, true, &single_nal(0x02)))
            .expect("the new frame completes on its own marker");
        let mut want = START_CODE.to_vec();
        want.extend_from_slice(&single_nal(0x02));
        assert_eq!(
            frame.data.as_ref(),
            want.as_slice(),
            "bytes from the abandoned frame leaked in"
        );
    }

    #[test]
    fn an_undecodable_payload_poisons_only_its_own_frame() {
        let mut assembler = H264Assembler::default();
        // NALU type 31 has no defined payload format.
        assert!(assembler
            .push(&packet(1, 9000, true, &[0x9F, 0x00]))
            .is_none());

        let frame = assembler
            .push(&packet(2, 12_000, true, &single_nal(0x07)))
            .expect("the next frame decodes normally");
        assert!(!frame.data.is_empty());
    }

    /// Body bytes for a fat packet, 1.2 KB — the size the reviewer's probe
    /// used to grow a 120 MB "frame" out of 100,000 packets.
    fn bulk_body() -> Vec<u8> {
        vec![0xAB; 1_200]
    }

    /// How many packets of `payload_len` bytes it takes to go past the cap,
    /// with a margin so the assembler is pushed well beyond it.
    fn packets_past_the_cap(payload_len: usize) -> usize {
        MAX_ACCESS_UNIT_BYTES / payload_len + 64
    }

    /// A host that never sets the marker bit and never moves the timestamp
    /// used to grow the pending access unit without limit, and then emit it as
    /// a valid frame. Neither the relay's message cap nor its rate limiter
    /// covers the media path, so the ceiling has to hold here.
    #[test]
    fn an_access_unit_that_never_ends_is_capped_instead_of_growing_without_limit() {
        let mut assembler = H264Assembler::default();
        let mut nal = vec![0x41];
        nal.extend_from_slice(&bulk_body());

        let count = packets_past_the_cap(nal.len());
        for i in 0..count {
            assert!(
                assembler
                    .push(&packet(i as u16, 9_000, false, &nal))
                    .is_none(),
                "no marker bit, so nothing may be emitted"
            );
            assert!(
                assembler.pending.len() <= MAX_ACCESS_UNIT_BYTES,
                "the pending access unit grew to {} bytes",
                assembler.pending.len()
            );
            assert!(
                assembler.au_input_bytes <= MAX_ACCESS_UNIT_BYTES,
                "{} bytes were accepted into one access unit",
                assembler.au_input_bytes
            );
        }
        assert!(
            assembler.pending.capacity() <= MAX_ACCESS_UNIT_BYTES,
            "the buffer held on to {} bytes of capacity",
            assembler.pending.capacity()
        );
        assert!(
            assembler.lost_data(),
            "discarding a frame must ask for a keyframe"
        );

        // The marker on the poisoned frame publishes nothing...
        assert!(assembler
            .push(&packet(count as u16, 9_000, true, &nal))
            .is_none());
        // ...and the next frame comes through clean.
        let frame = assembler
            .push(&packet(count as u16 + 1, 12_000, true, &single_nal(0x07)))
            .expect("the assembler recovers at the next frame");
        let mut want = START_CODE.to_vec();
        want.extend_from_slice(&single_nal(0x07));
        assert_eq!(frame.data.as_ref(), want.as_slice());
    }

    /// The same attack one layer down: FU-A fragments with neither the start
    /// nor the end bit pile up inside the depacketizer, where `pending.len()`
    /// cannot see them. Counting input bytes is what bounds this one.
    #[test]
    fn an_endless_fu_a_run_is_capped_although_its_bytes_sit_in_the_depacketizer() {
        let mut assembler = H264Assembler::default();
        // 0x7C: FU-A, nri 3. 0x85 starts NAL type 5; 0x05 continues it.
        let mut start = vec![0x7C, 0x85];
        start.extend_from_slice(&bulk_body());
        let mut middle = vec![0x7C, 0x05];
        middle.extend_from_slice(&bulk_body());

        assert!(assembler.push(&packet(0, 9_000, false, &start)).is_none());
        let count = packets_past_the_cap(middle.len());
        for i in 1..=count {
            assert!(assembler
                .push(&packet(i as u16, 9_000, false, &middle))
                .is_none());
            assert!(
                assembler.au_input_bytes <= MAX_ACCESS_UNIT_BYTES,
                "{} bytes were fed into one access unit",
                assembler.au_input_bytes
            );
            assert!(assembler.pending.len() <= MAX_ACCESS_UNIT_BYTES);
        }
        assert!(assembler.lost_data());

        let frame = assembler
            .push(&packet(count as u16 + 1, 12_000, true, &single_nal(0x07)))
            .expect("the assembler recovers at the next frame");
        let mut want = START_CODE.to_vec();
        want.extend_from_slice(&single_nal(0x07));
        assert_eq!(
            frame.data.as_ref(),
            want.as_slice(),
            "bytes from the abandoned run leaked into the next frame"
        );
    }

    #[test]
    fn timestamps_start_at_zero_and_advance_in_microseconds() {
        let mut assembler = H264Assembler::default();
        let first = assembler
            .push(&packet(1, 1_000_000, true, &single_nal(0x01)))
            .unwrap();
        assert_eq!(first.timestamp_us, 0);

        // 3000 ticks of a 90 kHz clock is one 33.3 ms frame.
        let second = assembler
            .push(&packet(2, 1_003_000, true, &single_nal(0x02)))
            .unwrap();
        assert_eq!(second.timestamp_us, 3_000 * 1_000_000 / 90_000);
    }

    /// A 90 kHz counter wraps roughly every 13 hours, which an unattended
    /// session reaches. Presentation time must keep increasing across it.
    #[test]
    fn presentation_time_keeps_increasing_across_a_timestamp_wrap() {
        let mut assembler = H264Assembler::default();
        let before = assembler
            .push(&packet(1, u32::MAX - 1_000, true, &single_nal(0x01)))
            .unwrap();
        let after = assembler
            .push(&packet(2, 2_000, true, &single_nal(0x02)))
            .unwrap();
        assert!(
            after.timestamp_us > before.timestamp_us,
            "{} went backwards from {}",
            after.timestamp_us,
            before.timestamp_us
        );
        let advance = after.timestamp_us - before.timestamp_us;
        assert_eq!(advance, 3_001 * 1_000_000 / 90_000);
    }
}
