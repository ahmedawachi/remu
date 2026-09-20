//! Failures the relay reports to its operator.
//!
//! Peer-facing failures are not errors in this sense: a client that misbehaves
//! is answered with a [`remu_proto::ErrorCode`] on the wire and the process
//! keeps running. Everything here is a reason the *server* could not start or
//! could not keep serving.

use std::net::SocketAddr;

#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    #[error("could not bind {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("could not read the listening address of the bound socket: {0}")]
    LocalAddr(#[source] std::io::Error),
    #[error("the accept loop stopped: {0}")]
    Serve(#[source] std::io::Error),
}
