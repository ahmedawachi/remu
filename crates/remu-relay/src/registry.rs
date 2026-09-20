//! Who is connected, and which peers are mid-session with whom.
//!
//! The registry owns every piece of shared state in the relay and is the only
//! thing behind a lock, so the connection tasks stay lock-free between
//! messages. It knows nothing about WebSockets: a connection is just an outbox
//! plus a way to ask for its eviction, which is what makes it unit-testable.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use rand::Rng;
use remu_proto::{PeerId, SessionId, PEER_ID_MAX, PEER_ID_MIN};
use tokio::sync::mpsc;
use tokio::sync::Notify;

use crate::config::ID_ALLOCATION_ATTEMPTS;
use remu_proto::ServerToClient;

/// Identifies one TCP connection for the lifetime of the process.
///
/// Cleanup is keyed on this as well as on the peer ID so a late teardown from
/// a dead connection can never evict the peer that has since reclaimed its ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnId(u64);

impl ConnId {
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// What a connection's writer task consumes.
#[derive(Debug)]
pub enum Outbound {
    Message(ServerToClient),
    /// Send a close frame with this WebSocket status code and hang up. Kept
    /// free of any axum type so the registry stays transport-agnostic.
    Close {
        code: u16,
        reason: &'static str,
    },
}

/// The half of a connection the registry needs in order to reach a peer.
#[derive(Debug, Clone)]
pub struct PeerHandle {
    pub outbox: mpsc::Sender<Outbound>,
    pub evict: Arc<Notify>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DeliveryError {
    /// Nobody is registered under that ID.
    Offline,
    /// The destination's bounded queue is full: it is not reading fast enough,
    /// so it is being evicted rather than allowed to consume memory.
    Congested,
}

/// No free ID was found. Only reachable if the relay is holding an
/// implausible share of the 900-million-wide space.
#[derive(Debug, PartialEq, Eq)]
pub struct IdUnavailable;

#[derive(Debug)]
struct PeerEntry {
    conn: ConnId,
    alias: Option<String>,
    handle: PeerHandle,
    /// Sessions this peer is part of, as (partner, session). Used to tell the
    /// partner when this peer vanishes — the Node original never did, leaving
    /// the other side staring at a frozen screen.
    sessions: HashSet<(PeerId, SessionId)>,
}

#[derive(Debug)]
pub struct Registry {
    peers: Mutex<HashMap<PeerId, PeerEntry>>,
    started: Instant,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            peers: Mutex::new(HashMap::new()),
            started: Instant::now(),
        }
    }

    pub fn started_at(&self) -> Instant {
        self.started
    }

    pub fn client_count(&self) -> usize {
        self.peers.lock().len()
    }

    pub fn is_online(&self, id: PeerId) -> bool {
        self.peers.lock().contains_key(&id)
    }

    /// `None` when the peer is not connected, `Some(alias)` when it is.
    pub fn lookup(&self, id: PeerId) -> Option<Option<String>> {
        self.peers.lock().get(&id).map(|p| p.alias.clone())
    }

    pub fn register(
        &self,
        conn: ConnId,
        preferred: Option<PeerId>,
        alias: Option<String>,
        handle: PeerHandle,
    ) -> Result<PeerId, IdUnavailable> {
        let mut peers = self.peers.lock();
        let id = match preferred {
            // Honouring a free preferred ID is what lets a client keep its
            // number across a network blip, so saved contacts stay reachable.
            Some(wanted) if !peers.contains_key(&wanted) => wanted,
            _ => Self::free_id(&peers)?,
        };
        peers.insert(
            id,
            PeerEntry {
                conn,
                alias,
                handle,
                sessions: HashSet::new(),
            },
        );
        Ok(id)
    }

    fn free_id(peers: &HashMap<PeerId, PeerEntry>) -> Result<PeerId, IdUnavailable> {
        let mut rng = rand::thread_rng();
        for _ in 0..ID_ALLOCATION_ATTEMPTS {
            let raw = rng.gen_range(PEER_ID_MIN..=PEER_ID_MAX);
            let Ok(candidate) = PeerId::new(raw) else {
                continue;
            };
            if !peers.contains_key(&candidate) {
                return Ok(candidate);
            }
        }
        Err(IdUnavailable)
    }

    pub fn deliver(&self, to: PeerId, msg: ServerToClient) -> Result<(), DeliveryError> {
        let peers = self.peers.lock();
        let Some(entry) = peers.get(&to) else {
            return Err(DeliveryError::Offline);
        };
        match entry.handle.outbox.try_send(Outbound::Message(msg)) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                // `notify_one` rather than `notify_waiters`: it leaves a permit
                // behind, so the writer is evicted even if it happened to be
                // mid-send rather than parked on the notification.
                entry.handle.evict.notify_one();
                Err(DeliveryError::Congested)
            }
            // The writer task has already gone; the peer is on its way out and
            // its entry will be removed by its own cleanup.
            Err(mpsc::error::TrySendError::Closed(_)) => Err(DeliveryError::Offline),
        }
    }

    /// Records that `a` and `b` are talking, in both directions.
    pub fn note_session(&self, a: PeerId, b: PeerId, session: SessionId) {
        let mut peers = self.peers.lock();
        if let Some(entry) = peers.get_mut(&a) {
            entry.sessions.insert((b, session));
        }
        if let Some(entry) = peers.get_mut(&b) {
            entry.sessions.insert((a, session));
        }
    }

    /// Forgets a session that ended cleanly, so nobody gets a second `bye`
    /// when one of the two later disconnects.
    pub fn end_session(&self, a: PeerId, b: PeerId, session: SessionId) {
        let mut peers = self.peers.lock();
        if let Some(entry) = peers.get_mut(&a) {
            entry.sessions.remove(&(b, session));
        }
        if let Some(entry) = peers.get_mut(&b) {
            entry.sessions.remove(&(a, session));
        }
    }

    /// Frees the ID and tells every partner still in a session with it that it
    /// is gone. Returns the partners notified, for the log line.
    ///
    /// A no-op unless `conn` still owns `id`: a teardown that lost the race
    /// with a reconnect must not disturb the live connection.
    pub fn disconnect(&self, conn: ConnId, id: PeerId) -> Vec<PeerId> {
        let mut peers = self.peers.lock();
        match peers.get(&id) {
            Some(entry) if entry.conn == conn => {}
            _ => return Vec::new(),
        }
        let Some(entry) = peers.remove(&id) else {
            return Vec::new();
        };

        let mut notified = Vec::new();
        for (partner, session) in entry.sessions {
            let Some(other) = peers.get_mut(&partner) else {
                continue;
            };
            other.sessions.remove(&(id, session));
            let bye = ServerToClient::Bye {
                from: id,
                session_id: session,
            };
            // Best effort: a partner whose queue is full is already being
            // evicted and will learn the hard way.
            if other.handle.outbox.try_send(Outbound::Message(bye)).is_ok() {
                notified.push(partner);
            }
        }
        notified
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(n: u32) -> PeerId {
        PeerId::new(n).unwrap()
    }

    struct Client {
        handle: PeerHandle,
        inbox: mpsc::Receiver<Outbound>,
    }

    fn client(depth: usize) -> Client {
        let (outbox, inbox) = mpsc::channel(depth);
        Client {
            handle: PeerHandle {
                outbox,
                evict: Arc::new(Notify::new()),
            },
            inbox,
        }
    }

    fn bye(from: PeerId, session: SessionId) -> ServerToClient {
        ServerToClient::Bye {
            from,
            session_id: session,
        }
    }

    fn received(inbox: &mut mpsc::Receiver<Outbound>) -> Option<ServerToClient> {
        match inbox.try_recv() {
            Ok(Outbound::Message(msg)) => Some(msg),
            _ => None,
        }
    }

    #[test]
    fn honours_a_preferred_id_only_while_it_is_free() {
        let registry = Registry::new();
        let wanted = peer(123_456_789);

        let first = client(4);
        let got = registry
            .register(ConnId::next(), Some(wanted), None, first.handle)
            .unwrap();
        assert_eq!(got, wanted);

        let second = client(4);
        let other = registry
            .register(ConnId::next(), Some(wanted), None, second.handle)
            .unwrap();
        assert_ne!(other, wanted, "a taken ID must not be handed out twice");
        assert_eq!(registry.client_count(), 2);
    }

    #[test]
    fn allocates_ids_inside_the_nine_digit_range() {
        let registry = Registry::new();
        for _ in 0..50 {
            let id = registry
                .register(ConnId::next(), None, None, client(4).handle)
                .unwrap();
            assert!((PEER_ID_MIN..=PEER_ID_MAX).contains(&id.get()));
        }
        assert_eq!(registry.client_count(), 50, "every ID must be distinct");
    }

    #[test]
    fn reports_a_peer_as_offline_once_it_disconnects_and_reallocates_its_id() {
        let registry = Registry::new();
        let conn = ConnId::next();
        let id = registry
            .register(conn, Some(peer(100_000_001)), None, client(4).handle)
            .unwrap();
        assert!(registry.is_online(id));

        assert!(registry.disconnect(conn, id).is_empty());
        assert!(!registry.is_online(id));
        assert_eq!(
            registry.deliver(id, bye(peer(200_000_000), SessionId::random())),
            Err(DeliveryError::Offline)
        );

        let again = registry
            .register(ConnId::next(), Some(id), None, client(4).handle)
            .unwrap();
        assert_eq!(again, id, "a freed ID can be claimed again");
    }

    #[test]
    fn tells_a_partner_when_the_peer_it_was_talking_to_vanishes() {
        let registry = Registry::new();
        let mut alice = client(4);
        let mut bob = client(4);
        let alice_conn = ConnId::next();
        let a = registry
            .register(alice_conn, None, None, alice.handle.clone())
            .unwrap();
        let b = registry
            .register(ConnId::next(), None, None, bob.handle.clone())
            .unwrap();
        let session = SessionId::random();
        registry.note_session(a, b, session);

        let notified = registry.disconnect(alice_conn, a);
        assert_eq!(notified, vec![b]);
        assert_eq!(received(&mut bob.inbox), Some(bye(a, session)));
        assert_eq!(received(&mut alice.inbox), None);
    }

    #[test]
    fn does_not_repeat_a_bye_for_a_session_that_already_ended() {
        let registry = Registry::new();
        let mut bob = client(4);
        let alice_conn = ConnId::next();
        let a = registry
            .register(alice_conn, None, None, client(4).handle)
            .unwrap();
        let b = registry
            .register(ConnId::next(), None, None, bob.handle.clone())
            .unwrap();
        let session = SessionId::random();
        registry.note_session(a, b, session);
        registry.end_session(a, b, session);

        assert!(registry.disconnect(alice_conn, a).is_empty());
        assert_eq!(received(&mut bob.inbox), None);
    }

    #[test]
    fn a_stale_teardown_leaves_the_reconnected_peer_alone() {
        let registry = Registry::new();
        let stale_conn = ConnId::next();
        let id = registry
            .register(stale_conn, Some(peer(111_111_111)), None, client(4).handle)
            .unwrap();
        registry.disconnect(stale_conn, id);

        let fresh_conn = ConnId::next();
        registry
            .register(fresh_conn, Some(id), None, client(4).handle)
            .unwrap();

        registry.disconnect(stale_conn, id);
        assert!(
            registry.is_online(id),
            "the reconnected peer must survive the old connection's cleanup"
        );
    }

    #[tokio::test]
    async fn evicts_a_destination_whose_queue_is_full_instead_of_buffering() {
        let registry = Registry::new();
        let slow = client(1);
        let id = registry
            .register(ConnId::next(), None, None, slow.handle.clone())
            .unwrap();
        let session = SessionId::random();

        assert!(registry
            .deliver(id, bye(peer(222_222_222), session))
            .is_ok());
        assert_eq!(
            registry.deliver(id, bye(peer(222_222_222), session)),
            Err(DeliveryError::Congested)
        );
        // The permit left by the failed delivery is what tears the slow
        // connection down; without it the relay would buffer for it forever.
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            slow.handle.evict.notified(),
        )
        .await
        .expect("a congested peer must be marked for eviction");
    }

    #[test]
    fn a_lookup_returns_the_alias_of_an_online_peer_only() {
        let registry = Registry::new();
        let conn = ConnId::next();
        let id = registry
            .register(conn, None, Some("Reception PC".into()), client(4).handle)
            .unwrap();
        assert_eq!(registry.lookup(id), Some(Some("Reception PC".into())));
        registry.disconnect(conn, id);
        assert_eq!(registry.lookup(id), None);
    }
}
