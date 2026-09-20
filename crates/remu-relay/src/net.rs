//! Working out what address to print so a user can share it.

use std::net::{IpAddr, Ipv4Addr, UdpSocket};

/// The IPv4 address a peer elsewhere on the network should dial, if there is
/// one.
///
/// Enumerating every interface needs `getifaddrs` on Unix and
/// `GetAdaptersAddresses` on Windows — unsafe FFI, or a dependency, for a
/// startup log line. Instead the routing table is asked which source address
/// it would use for an off-link destination. Connecting a UDP socket sends no
/// packet; it only binds the route, so this works offline and costs nothing.
pub fn primary_lan_ipv4() -> Option<Ipv4Addr> {
    // RFC 5737 TEST-NET-1: guaranteed never to be a real host.
    const OFF_LINK: (Ipv4Addr, u16) = (Ipv4Addr::new(192, 0, 2, 1), 9);

    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect(OFF_LINK).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(addr) if !addr.is_loopback() && !addr.is_unspecified() => Some(addr),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_reports_loopback_or_the_unspecified_address() {
        // A machine with no route at all legitimately has nothing to report,
        // so the only invariant is what a `Some` may contain.
        if let Some(addr) = primary_lan_ipv4() {
            assert!(!addr.is_loopback());
            assert!(!addr.is_unspecified());
            assert!(!addr.is_multicast());
        }
    }
}
