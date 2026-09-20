//! Messages carried on the peer-to-peer control channel.
//!
//! Remu opens two data channels rather than one:
//!
//! - [`CONTROL_CHANNEL`] — small, latency-critical JSON: input, chat, clipboard.
//! - [`BULK_CHANNEL`] — file bytes, framed binary (see [`crate::bulk`]).
//!
//! Both are reliable and ordered, but they are *separate SCTP streams*, and
//! that separation is the point. The Electron original multiplexed files and
//! input onto one channel, so sending a large file head-of-line blocked every
//! mouse move behind it and the remote cursor visibly froze. Splitting them
//! means a file transfer can saturate the link without the session feeling dead.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::input::InputEvent;

/// Label of the ordered, reliable channel carrying [`ControlMessage`] as JSON.
pub const CONTROL_CHANNEL: &str = "remu.ctrl";
/// Label of the ordered, reliable channel carrying framed file chunks.
pub const BULK_CHANNEL: &str = "remu.bulk";

/// A display the host can share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayInfo {
    /// Opaque, host-assigned. Only the host interprets it.
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// True for the host's primary display.
    #[serde(default)]
    pub primary: bool,
}

/// What the host is willing to let the controller do.
///
/// Sent by the host as soon as the channel opens and again whenever the user
/// changes a toggle, so the controller can grey out what will not work instead
/// of silently dropping the controller's input on the floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostPermissions {
    pub input: bool,
    pub files: bool,
    pub clipboard: bool,
}

impl Default for HostPermissions {
    fn default() -> Self {
        Self {
            input: true,
            files: true,
            clipboard: true,
        }
    }
}

/// Periodic link telemetry, host to controller, for the session status line.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStats {
    pub fps: f32,
    pub bitrate_kbps: u32,
    pub rtt_ms: u32,
    /// Encoded frames the host dropped because the link could not take them.
    #[serde(default)]
    pub dropped_frames: u32,
}

/// Why a file transfer was refused or aborted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransferError {
    /// The receiving side has file transfer switched off.
    NotPermitted,
    /// The receiving user declined this file.
    Declined,
    /// The file exceeds the receiver's size limit.
    TooLarge,
    /// The receiver could not create or write the destination file.
    WriteFailed,
    /// Bytes received did not match the sender's digest.
    ChecksumMismatch,
    /// The sender abandoned the transfer.
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ControlMessage {
    /// Controller to host. Ignored by a host with input disabled.
    Input {
        event: InputEvent,
    },
    #[serde(rename_all = "camelCase")]
    Chat {
        text: String,
        at: u64,
    },
    /// Clipboard text, either direction.
    Clipboard {
        text: String,
    },
    /// Host to controller: where the host's real cursor is, normalized, so the
    /// controller can draw it even when the captured frames omit the cursor.
    CursorPos {
        x: f32,
        y: f32,
    },
    /// Host to controller, on channel open and on display changes.
    #[serde(rename_all = "camelCase")]
    DisplayList {
        displays: Vec<DisplayInfo>,
        active: String,
    },
    /// Controller to host: share a different display.
    #[serde(rename_all = "camelCase")]
    SwitchDisplay {
        source_id: String,
    },
    /// Host to controller, whenever the host's toggles change.
    Permissions(HostPermissions),
    Stats(SessionStats),
    /// Announces an incoming file. The receiver answers `FileAccept` or `FileRefuse`.
    #[serde(rename_all = "camelCase")]
    FileMeta {
        transfer_id: Uuid,
        name: String,
        size: u64,
        mime: String,
    },
    #[serde(rename_all = "camelCase")]
    FileAccept {
        transfer_id: Uuid,
    },
    #[serde(rename_all = "camelCase")]
    FileRefuse {
        transfer_id: Uuid,
        reason: TransferError,
    },
    /// Sent after the final chunk. The digest lets the receiver prove the file
    /// arrived whole rather than trusting the byte count.
    #[serde(rename_all = "camelCase")]
    FileEnd {
        transfer_id: Uuid,
        sha256: String,
    },
    #[serde(rename_all = "camelCase")]
    FileCancel {
        transfer_id: Uuid,
        reason: TransferError,
    },
}

/// Strips a filename down to something safe to create in a download directory.
///
/// A peer controls this string, so it is treated as hostile: path separators,
/// parent-directory hops, NUL bytes, Windows reserved device names and leading
/// dots are all removed. A name that reduces to nothing becomes `"download"`.
pub fn sanitize_filename(raw: &str) -> String {
    const WINDOWS_RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];

    // Take only the final component, so "../../etc/passwd" and
    // "C:\\Windows\\system32\\x" both collapse to their last segment.
    let base = raw.rsplit(['/', '\\']).next().unwrap_or_default();

    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0'))
        .collect();

    // A name of only dots ("." / "..") is a directory reference, not a file.
    let trimmed = cleaned.trim().trim_start_matches('.').trim_end_matches('.');
    if trimmed.is_empty() {
        return "download".to_string();
    }

    let stem = trimmed.split('.').next().unwrap_or(trimmed);
    if WINDOWS_RESERVED
        .iter()
        .any(|r| r.eq_ignore_ascii_case(stem))
    {
        // Capped like every other path: a reserved stem followed by 400
        // emoji produced a 1605-byte name, straight past the limit
        // this function exists to enforce.
        return cap_length(&format!("_{trimmed}"));
    }

    cap_length(trimmed)
}

/// Longest name this function returns, in bytes.
///
/// Every mainstream filesystem — ext4, APFS, NTFS, XFS — caps one path
/// component at 255 *bytes*, not characters, and returns ENAMETOOLONG above
/// that. Counting characters instead let a peer send 200 emoji, 800 bytes, and
/// make the receiver's `create_new` fail after the whole file had been written.
/// The remaining bytes leave room for the " (1000)" the receiver appends when
/// the name is already taken.
const MAX_NAME_BYTES: usize = 248;

/// Longest name this function returns, in characters.
///
/// Independent of the byte cap and much stricter for ASCII: a name this long is
/// already unusable in a file dialog, whatever it encodes to.
const MAX_NAME_CHARS: usize = 200;

/// Longest tail after the last dot still treated as an extension worth keeping
/// when a name has to be shortened. A real extension is a handful of ASCII
/// characters; anything longer is just part of the name.
const MAX_EXTENSION_BYTES: usize = 16;

/// Shortens an over-long name to fit both caps, keeping the extension and
/// never splitting a UTF-8 character.
fn cap_length(name: &str) -> String {
    if name.len() <= MAX_NAME_BYTES && name.chars().count() <= MAX_NAME_CHARS {
        return name.to_string();
    }

    // The extension survives the cut when there is one: the receiver picks a
    // handler from it, and a name truncated mid-".pdf" opens in nothing.
    if let Some((stem, extension)) = name.rsplit_once('.') {
        if !stem.is_empty() && !extension.is_empty() && extension.len() <= MAX_EXTENSION_BYTES {
            let stem = take_within(
                stem,
                MAX_NAME_CHARS.saturating_sub(extension.chars().count() + 1),
                MAX_NAME_BYTES.saturating_sub(extension.len() + 1),
            );
            if !stem.is_empty() {
                return format!("{stem}.{extension}");
            }
        }
    }

    take_within(name, MAX_NAME_CHARS, MAX_NAME_BYTES)
}

/// The longest prefix of `s` within both budgets, cut on a character boundary.
///
/// Built character by character rather than by slicing: slicing a `&str` at a
/// byte offset that is not a boundary panics, and this runs on peer-supplied
/// text.
fn take_within(s: &str, max_chars: usize, max_bytes: usize) -> String {
    let mut out = String::with_capacity(max_bytes.min(s.len()));
    for c in s.chars().take(max_chars) {
        if out.len() + c.len_utf8() > max_bytes {
            break;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_control_message_shape() {
        let id = Uuid::new_v4();
        let cases = vec![
            ControlMessage::Input {
                event: InputEvent::ReleaseAll,
            },
            ControlMessage::Chat {
                text: "hi".into(),
                at: 1_700_000_000_000,
            },
            ControlMessage::Clipboard {
                text: "copied".into(),
            },
            ControlMessage::CursorPos { x: 0.5, y: 0.5 },
            ControlMessage::DisplayList {
                displays: vec![DisplayInfo {
                    id: "screen:0".into(),
                    name: "Built-in Retina".into(),
                    width: 3456,
                    height: 2234,
                    primary: true,
                }],
                active: "screen:0".into(),
            },
            ControlMessage::SwitchDisplay {
                source_id: "screen:1".into(),
            },
            ControlMessage::Permissions(HostPermissions::default()),
            ControlMessage::Stats(SessionStats {
                fps: 58.5,
                bitrate_kbps: 4200,
                rtt_ms: 18,
                dropped_frames: 3,
            }),
            ControlMessage::FileMeta {
                transfer_id: id,
                name: "report.pdf".into(),
                size: 91_233,
                mime: "application/pdf".into(),
            },
            ControlMessage::FileAccept { transfer_id: id },
            ControlMessage::FileRefuse {
                transfer_id: id,
                reason: TransferError::NotPermitted,
            },
            ControlMessage::FileEnd {
                transfer_id: id,
                sha256: "ab".repeat(32),
            },
            ControlMessage::FileCancel {
                transfer_id: id,
                reason: TransferError::Cancelled,
            },
        ];
        for case in cases {
            let json = serde_json::to_string(&case).unwrap();
            let back: ControlMessage = serde_json::from_str(&json).unwrap();
            assert_eq!(back, case, "round trip failed for {json}");
        }
    }

    #[test]
    fn tags_control_messages_in_kebab_case() {
        let json = serde_json::to_value(ControlMessage::SwitchDisplay {
            source_id: "screen:1".into(),
        })
        .unwrap();
        assert_eq!(json["kind"], "switch-display");
        assert_eq!(json["sourceId"], "screen:1");
    }

    #[test]
    fn strips_directory_traversal_from_incoming_filenames() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("/etc/shadow"), "shadow");
        assert_eq!(
            sanitize_filename(r"C:\Windows\system32\evil.dll"),
            "evil.dll"
        );
        assert_eq!(sanitize_filename("a/b/c/report.pdf"), "report.pdf");
    }

    #[test]
    fn replaces_names_that_reduce_to_nothing() {
        for bad in ["", ".", "..", "...", "   ", "/", r"\\", "/../"] {
            assert_eq!(sanitize_filename(bad), "download", "input {bad:?}");
        }
    }

    #[test]
    fn a_reserved_stem_is_still_length_capped() {
        // The reserved-name branch used to return early, straight past the
        // cap: "con." plus 400 emoji came back as 1605 bytes, which no
        // filesystem will accept and which is what let a peer make the
        // receiver fail after a file had been fully written.
        let hostile = format!("con.{}", "\u{1F600}".repeat(400));
        let safe = sanitize_filename(&hostile);
        assert!(safe.starts_with("_con"), "got {safe:?}");
        assert!(
            safe.len() <= 255,
            "reserved-name path returned {} bytes",
            safe.len()
        );
        // And it is still valid UTF-8 with no character split in half.
        assert!(std::str::from_utf8(safe.as_bytes()).is_ok());
    }

    #[test]
    fn defuses_windows_reserved_device_names() {
        assert_eq!(sanitize_filename("CON"), "_CON");
        assert_eq!(sanitize_filename("nul.txt"), "_nul.txt");
        assert_eq!(sanitize_filename("COM1.log"), "_COM1.log");
        // Not reserved -- must be left alone.
        assert_eq!(sanitize_filename("CONTRACT.pdf"), "CONTRACT.pdf");
    }

    #[test]
    fn removes_control_characters_and_shell_hostile_punctuation() {
        assert_eq!(sanitize_filename("re\u{0}port\n.pdf"), "report.pdf");
        assert_eq!(sanitize_filename("a:b*c?d.txt"), "abcd.txt");
    }

    #[test]
    fn keeps_ordinary_names_including_unicode_intact() {
        assert_eq!(
            sanitize_filename("Q3 résumé (final).pdf"),
            "Q3 résumé (final).pdf"
        );
        assert_eq!(sanitize_filename("تقرير.pdf"), "تقرير.pdf");
    }

    #[test]
    fn bounds_absurdly_long_names() {
        let long = format!("{}.pdf", "a".repeat(5000));
        assert_eq!(sanitize_filename(&long).chars().count(), 200);
    }

    /// Filesystems count bytes. A name capped at 200 *characters* of emoji is
    /// 800 bytes, which no mainstream filesystem will create -- and the
    /// receiver only found out after writing the whole file to its temporary.
    #[test]
    fn bounds_long_names_by_bytes_as_well_as_characters() {
        for raw in [
            "\u{1f642}".repeat(300),
            format!("{}.pdf", "\u{1f642}".repeat(300)),
            format!("{}.pdf", "\u{062a}".repeat(400)),
            format!("{}.tar.gz", "e\u{301}".repeat(400)),
        ] {
            let out = sanitize_filename(&raw);
            assert!(
                out.len() <= 255,
                "{} bytes for input of {} bytes",
                out.len(),
                raw.len()
            );
            assert!(!out.is_empty());
            // Cut on a character boundary: the string is still valid UTF-8 and
            // its last character is whole.
            assert_eq!(out, out.chars().collect::<String>());
        }
    }

    /// Shortening must not cost the extension: the receiver picks a handler
    /// from it.
    #[test]
    fn a_shortened_name_keeps_its_extension() {
        let emoji = sanitize_filename(&format!("{}.pdf", "\u{1f642}".repeat(300)));
        assert!(emoji.ends_with(".pdf"), "{emoji}");
        let ascii = sanitize_filename(&format!("{}.tar.gz", "a".repeat(5000)));
        assert!(ascii.ends_with(".gz"), "{ascii}");
    }
}
