//! Conversion between the wire types in `remu-proto` and webrtc-rs types.
//!
//! The protocol crate deliberately knows nothing about webrtc-rs, so the
//! translation lives here — in one place, tested both directions, rather than
//! inlined at each of the five call sites that need it.

use remu_proto::{IceCandidate, SdpKind, SessionDescription};
use rtc::peer_connection::sdp::{RTCSdpType, RTCSessionDescription};
use rtc::peer_connection::transport::RTCIceCandidateInit;

use crate::error::SessionError;

/// Wire description to the form webrtc-rs applies.
///
/// The SDP text is parsed eagerly by webrtc-rs, so a peer that sends garbage is
/// rejected here rather than at `set_remote_description` time.
pub(crate) fn to_webrtc_sdp(
    desc: &SessionDescription,
) -> Result<RTCSessionDescription, SessionError> {
    let sdp = desc.sdp.clone();
    let converted = match desc.kind {
        SdpKind::Offer => RTCSessionDescription::offer(sdp)?,
        SdpKind::Answer => RTCSessionDescription::answer(sdp)?,
    };
    Ok(converted)
}

/// webrtc-rs description to the form that goes on the wire.
///
/// `pranswer` and `rollback` are rejected: Remu's signalling has exactly two
/// SDP kinds, and mapping a third onto one of them would put a description on
/// the wire that the far side would apply in the wrong signalling state.
pub(crate) fn from_webrtc_sdp(
    desc: &RTCSessionDescription,
) -> Result<SessionDescription, SessionError> {
    let kind = match desc.sdp_type {
        RTCSdpType::Offer => SdpKind::Offer,
        RTCSdpType::Answer => SdpKind::Answer,
        other => return Err(SessionError::UnsupportedSdpType(other)),
    };
    Ok(SessionDescription {
        kind,
        sdp: desc.sdp.clone(),
    })
}

/// Wire candidate to the form webrtc-rs applies.
pub(crate) fn to_webrtc_ice(candidate: &IceCandidate) -> RTCIceCandidateInit {
    RTCIceCandidateInit {
        candidate: candidate.candidate.clone(),
        sdp_mid: candidate.sdp_mid.clone(),
        sdp_mline_index: candidate.sdp_mline_index,
        username_fragment: candidate.username_fragment.clone(),
        // Only meaningful for locally gathered srflx/relay candidates, and the
        // far side has no use for which of our STUN servers found it.
        url: None,
    }
}

/// webrtc-rs candidate to the form that goes on the wire.
pub(crate) fn from_webrtc_ice(candidate: &RTCIceCandidateInit) -> IceCandidate {
    IceCandidate {
        candidate: candidate.candidate.clone(),
        sdp_mid: candidate.sdp_mid.clone(),
        sdp_mline_index: candidate.sdp_mline_index,
        username_fragment: candidate.username_fragment.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal but genuinely parseable SDP; webrtc-rs validates eagerly, so a
    /// hand-waved `"v=0"` would not survive the constructor.
    const SDP: &str = "v=0\r\n\
o=- 4611731400430051336 2 IN IP4 127.0.0.1\r\n\
s=-\r\n\
t=0 0\r\n\
a=group:BUNDLE 0\r\n\
m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n\
c=IN IP4 0.0.0.0\r\n\
a=ice-ufrag:abcd\r\n\
a=ice-pwd:efghijklmnopqrstuvwxyz01\r\n\
a=fingerprint:sha-256 \
00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:\
00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF\r\n\
a=setup:actpass\r\n\
a=mid:0\r\n\
a=sctp-port:5000\r\n";

    #[test]
    fn an_offer_survives_the_round_trip_in_both_directions() {
        let wire = SessionDescription {
            kind: SdpKind::Offer,
            sdp: SDP.to_string(),
        };
        let native = to_webrtc_sdp(&wire).unwrap();
        assert_eq!(native.sdp_type, RTCSdpType::Offer);
        assert_eq!(from_webrtc_sdp(&native).unwrap(), wire);
    }

    #[test]
    fn an_answer_keeps_its_kind_rather_than_defaulting_to_offer() {
        let wire = SessionDescription {
            kind: SdpKind::Answer,
            sdp: SDP.to_string(),
        };
        let native = to_webrtc_sdp(&wire).unwrap();
        assert_eq!(native.sdp_type, RTCSdpType::Answer);
        assert_eq!(from_webrtc_sdp(&native).unwrap().kind, SdpKind::Answer);
    }

    #[test]
    fn unparseable_sdp_is_rejected_at_the_boundary() {
        let wire = SessionDescription {
            kind: SdpKind::Offer,
            sdp: "this is not sdp".to_string(),
        };
        assert!(matches!(to_webrtc_sdp(&wire), Err(SessionError::WebRtc(_))));
    }

    #[test]
    fn a_pranswer_has_no_wire_form_and_is_refused() {
        // Built through the constructor: `parsed` is private, so the struct
        // cannot be assembled literally from outside the webrtc crate.
        let pranswer =
            RTCSessionDescription::pranswer(SDP.to_string()).expect("the fixture is valid SDP");
        assert!(matches!(
            from_webrtc_sdp(&pranswer),
            Err(SessionError::UnsupportedSdpType(RTCSdpType::Pranswer))
        ));
    }

    #[test]
    fn a_fully_populated_candidate_round_trips() {
        let wire = IceCandidate {
            candidate: "candidate:1 1 udp 2130706431 192.168.1.10 54321 typ host".into(),
            sdp_mid: Some("0".into()),
            sdp_mline_index: Some(0),
            username_fragment: Some("abcd".into()),
        };
        assert_eq!(from_webrtc_ice(&to_webrtc_ice(&wire)), wire);
    }

    /// Browsers omit `sdpMid`/`sdpMLineIndex` on end-of-candidates and on some
    /// trickled candidates; the absence has to survive, not become `Some("")`.
    #[test]
    fn absent_optional_fields_stay_absent() {
        let wire = IceCandidate {
            candidate: String::new(),
            sdp_mid: None,
            sdp_mline_index: None,
            username_fragment: None,
        };
        let native = to_webrtc_ice(&wire);
        assert_eq!(native.sdp_mid, None);
        assert_eq!(native.sdp_mline_index, None);
        assert_eq!(native.username_fragment, None);
        assert_eq!(from_webrtc_ice(&native), wire);
    }

    /// The `url` field is a webrtc-rs-only annotation naming the STUN server
    /// that produced a local candidate. It must not leak onto the wire, and an
    /// inbound candidate must never claim one.
    #[test]
    fn the_gathering_server_url_is_not_sent_to_the_peer() {
        let wire = IceCandidate {
            candidate: "candidate:2 1 udp 1694498815 203.0.113.7 3478 typ srflx".into(),
            sdp_mid: Some("0".into()),
            sdp_mline_index: Some(0),
            username_fragment: None,
        };
        assert_eq!(to_webrtc_ice(&wire).url, None);
    }
}
