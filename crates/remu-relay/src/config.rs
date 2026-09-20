//! Runtime knobs, shared by the binary's CLI and by the integration tests.

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

/// The longest alias the relay will store and echo back.
///
/// The alias is attacker-controlled and ends up in another peer's UI, so it is
/// bounded here rather than trusted: 64 characters is more than any sensible
/// machine name and small enough that a million of them still cost nothing.
pub const MAX_ALIAS_CHARS: usize = 64;

/// How many random draws the allocator makes before giving up on finding a
/// free ID. With a 900-million-wide space this only ever trips if the relay is
/// holding a truly implausible number of clients, and it is a bounded loop
/// rather than a hang.
pub const ID_ALLOCATION_ATTEMPTS: u32 = 200;

#[derive(Debug, Clone)]
pub struct RelayConfig {
    pub host: IpAddr,
    pub port: u16,
    /// Shared secret required in `register`, if the operator set one.
    pub token: Option<String>,
    /// Sustained registrations allowed per source IP, per minute. Zero
    /// disables the limiter.
    pub registrations_per_minute: f64,
    /// Registrations a source IP may make back to back before the sustained
    /// rate applies.
    pub registration_burst: u32,
    /// Sustained messages allowed per source IP, per second. Zero disables the
    /// limiter.
    pub messages_per_second: f64,
    pub message_burst: u32,
    /// How often the relay pings an idle connection.
    pub heartbeat: Duration,
    /// How long a connection may go without any frame from the client — pong
    /// included — before it is evicted.
    pub pong_timeout: Duration,
    /// Messages that may be queued for one client before it is treated as too
    /// slow to keep. Bounded so one stalled reader cannot grow the relay's
    /// memory without limit.
    pub send_queue_depth: usize,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            host: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            port: 8765,
            token: None,
            registrations_per_minute: 30.0,
            registration_burst: 10,
            messages_per_second: 50.0,
            message_burst: 200,
            heartbeat: Duration::from_secs(20),
            pong_timeout: Duration::from_secs(60),
            send_queue_depth: 64,
        }
    }
}

/// Trims an alias to something safe to store and to show in another peer's UI.
///
/// Returns `None` for an alias that is empty once cleaned, so the wire form
/// omits the field entirely instead of carrying an empty string.
pub fn sanitize_alias(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_ALIAS_CHARS)
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_an_alias_that_is_only_whitespace_or_control_characters() {
        assert_eq!(sanitize_alias(""), None);
        assert_eq!(sanitize_alias("   "), None);
        assert_eq!(sanitize_alias("\u{0}\u{7}\n"), None);
    }

    #[test]
    fn strips_control_characters_without_mangling_unicode() {
        assert_eq!(
            sanitize_alias("Reception\u{7} PC").as_deref(),
            Some("Reception PC")
        );
        assert_eq!(sanitize_alias("  مكتب أحمد ").as_deref(), Some("مكتب أحمد"));
    }

    #[test]
    fn caps_a_long_alias_by_characters_not_bytes() {
        let long = "é".repeat(200);
        let capped = sanitize_alias(&long).expect("non-empty");
        assert_eq!(capped.chars().count(), MAX_ALIAS_CHARS);
    }
}
