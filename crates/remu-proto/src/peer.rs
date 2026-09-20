//! Peer and session identity.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

/// Lowest and highest allocatable peer ID. Nine digits, never leading-zero, so
/// the decimal form is always exactly nine characters and a human can read one
/// over the phone without ambiguity.
pub const PEER_ID_MIN: u32 = 100_000_000;
pub const PEER_ID_MAX: u32 = 999_999_999;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    #[error("peer id must be exactly 9 digits (got {0:?})")]
    Malformed(String),
    #[error("peer id {0} is outside the allocatable range")]
    OutOfRange(u32),
}

/// A nine-digit desk identifier, the thing a user reads out to a colleague.
///
/// Serialized as a JSON *string* rather than a number: it is an opaque
/// identifier, not a quantity, and keeping it a string stops any future
/// JavaScript client from reformatting or rounding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PeerId(u32);

impl PeerId {
    pub fn new(raw: u32) -> Result<Self, IdError> {
        if (PEER_ID_MIN..=PEER_ID_MAX).contains(&raw) {
            Ok(Self(raw))
        } else {
            Err(IdError::OutOfRange(raw))
        }
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    /// Groups the digits for display: `123456789` -> `123 456 789`.
    pub fn grouped(self) -> String {
        let s = self.0.to_string();
        format!("{} {} {}", &s[0..3], &s[3..6], &s[6..9])
    }

    /// Parses user input leniently: any non-digit characters are separators, so
    /// `"123 456 789"`, `"123-456-789"` and `"123456789"` all resolve.
    pub fn parse_lenient(input: &str) -> Result<Self, IdError> {
        let digits: String = input.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.len() != 9 {
            return Err(IdError::Malformed(input.to_string()));
        }
        let raw: u32 = digits
            .parse()
            .map_err(|_| IdError::Malformed(input.to_string()))?;
        Self::new(raw)
    }
}

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for PeerId {
    type Err = IdError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 9 || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(IdError::Malformed(s.to_string()));
        }
        Self::new(s.parse().map_err(|_| IdError::Malformed(s.to_string()))?)
    }
}

impl Serialize for PeerId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for PeerId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Identifies one connection attempt between two peers. Distinct from the peer
/// IDs so that a stale `bye` or `ice` from an abandoned attempt cannot disturb
/// a newer session with the same peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(Uuid);

impl SessionId {
    pub fn random() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_ids_outside_the_nine_digit_range() {
        assert!(PeerId::new(99_999_999).is_err());
        assert!(PeerId::new(1_000_000_000).is_err());
        assert!(PeerId::new(PEER_ID_MIN).is_ok());
        assert!(PeerId::new(PEER_ID_MAX).is_ok());
    }

    #[test]
    fn groups_digits_for_display() {
        assert_eq!(PeerId::new(123_456_789).unwrap().grouped(), "123 456 789");
        assert_eq!(PeerId::new(100_000_000).unwrap().grouped(), "100 000 000");
    }

    #[test]
    fn parses_however_the_user_typed_it() {
        let want = PeerId::new(123_456_789).unwrap();
        for input in ["123456789", "123 456 789", "123-456-789", " 123.456.789 "] {
            assert_eq!(
                PeerId::parse_lenient(input).unwrap(),
                want,
                "input {input:?}"
            );
        }
        assert!(PeerId::parse_lenient("12345678").is_err());
        assert!(PeerId::parse_lenient("1234567890").is_err());
        assert!(PeerId::parse_lenient("").is_err());
    }

    #[test]
    fn strict_parse_rejects_separators_and_short_ids() {
        assert!("123 456 789".parse::<PeerId>().is_err());
        assert!("012345678".parse::<PeerId>().is_err());
        assert_eq!("123456789".parse::<PeerId>().unwrap().get(), 123_456_789);
    }

    #[test]
    fn round_trips_through_json_as_a_string() {
        let id = PeerId::new(987_654_321).unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"987654321\"");
        assert_eq!(serde_json::from_str::<PeerId>(&json).unwrap(), id);
    }

    #[test]
    fn rejects_a_json_id_that_is_not_nine_digits() {
        assert!(serde_json::from_str::<PeerId>("\"42\"").is_err());
        assert!(serde_json::from_str::<PeerId>("123456789").is_err());
    }
}
