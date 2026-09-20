//! The Remu signalling relay.
//!
//! Clients connect over WebSocket, are allocated a nine-digit
//! [`remu_proto::PeerId`], and have their [`remu_proto::ClientToServer`]
//! envelopes forwarded to the peer they address. The relay never sees media,
//! never holds a key, and cannot read an unattended password: everything it
//! touches is the handshake that lets two peers find each other.
//!
//! The crate is a library as well as a binary so the integration tests can
//! bind a real server on port 0 and drive it with real clients:
//!
//! ```no_run
//! # async fn example() -> Result<(), remu_relay::RelayError> {
//! let relay = remu_relay::Relay::bind(remu_relay::RelayConfig {
//!     port: 0,
//!     ..Default::default()
//! })
//! .await?;
//! let addr = relay.local_addr();
//! relay.serve_with_shutdown(std::future::pending()).await
//! # }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

mod config;
mod connection;
mod error;
mod net;
mod rate;
mod registry;
mod server;

pub use config::{sanitize_alias, RelayConfig, MAX_ALIAS_CHARS};
pub use error::RelayError;
pub use net::primary_lan_ipv4;
pub use server::{shutdown_signal, Relay, RelayHandle};
