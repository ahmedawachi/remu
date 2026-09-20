//! Display identifiers as they travel over the wire.
//!
//! The controller never sees a platform handle: it sees the string in
//! [`DisplayTarget::id`](crate::DisplayTarget::id) and echoes it back in
//! `ControlMessage::SwitchDisplay`. Prefixing the platform's numeric id keeps
//! the string self-describing in logs and leaves room for other kinds of
//! source (a window, a region) without the two ever colliding.

// Only `parse_display_id` needs it, and that is absent on Linux.
#[cfg(not(target_os = "linux"))]
use crate::CaptureError;

/// Prefix of every display id this crate hands out.
pub(crate) const DISPLAY_ID_PREFIX: &str = "display:";

pub(crate) fn format_display_id(raw: u32) -> String {
    format!("{DISPLAY_ID_PREFIX}{raw}")
}

/// Recovers the platform display number from an id that came off the wire.
///
/// The bare number is accepted alongside the canonical form so that a
/// hand-typed id or one persisted by an older build still resolves. Anything
/// else is a display that does not exist here, which is exactly what
/// [`CaptureError::NoSuchDisplay`] means — the peer is not trusted to have
/// sent a well-formed id at all.
// Linux never calls this: the desktop portal chooses the screen in its own
// dialog, so that backend has a single placeholder id and nothing to parse.
#[cfg(not(target_os = "linux"))]
pub(crate) fn parse_display_id(id: &str) -> Result<u32, CaptureError> {
    let digits = id.strip_prefix(DISPLAY_ID_PREFIX).unwrap_or(id);
    let well_formed = !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit());
    if !well_formed {
        return Err(CaptureError::NoSuchDisplay(id.to_string()));
    }
    digits
        .parse::<u32>()
        .map_err(|_| CaptureError::NoSuchDisplay(id.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_a_platform_display_number() {
        assert_eq!(format_display_id(69_732_928), "display:69732928");
    }

    /// Parsing exists only off Linux, where the portal picks the screen in its
    /// own dialog and there is no number to parse. The tests are gated with the
    /// function so the Linux build has neither.
    #[cfg(not(target_os = "linux"))]
    mod parsing {
        use super::*;

        #[test]
        fn round_trips_a_platform_display_number() {
            let id = format_display_id(69_732_928);
            assert_eq!(parse_display_id(&id).unwrap(), 69_732_928);
        }

        #[test]
        fn accepts_a_bare_number_for_compatibility() {
            assert_eq!(parse_display_id("7").unwrap(), 7);
            assert_eq!(parse_display_id("0").unwrap(), 0);
        }

        #[test]
        fn rejects_ids_that_are_not_displays() {
            for junk in [
                "",
                "display:",
                "display:abc",
                "display:-1",
                "display: 7",
                "display:7 ",
                "display:0x7",
                "window:7",
                "display:display:7",
                "+7",
            ] {
                let err = parse_display_id(junk).unwrap_err();
                assert_eq!(err, CaptureError::NoSuchDisplay(junk.to_string()), "{junk}");
            }
        }

        #[test]
        fn rejects_a_number_too_large_for_a_display_handle() {
            // u32::MAX + 1: all digits, so only the parse catches it.
            let err = parse_display_id("display:4294967296").unwrap_err();
            assert_eq!(
                err,
                CaptureError::NoSuchDisplay("display:4294967296".to_string())
            );
            assert_eq!(parse_display_id("display:4294967295").unwrap(), u32::MAX);
        }
    }
}
