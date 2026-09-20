//! Binding, routing and the lifetime of the server itself.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{ConnectInfo, State, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::{any, get};
use axum::{Json, Router};
use remu_proto::{PeerId, MAX_SIGNALING_MESSAGE_BYTES};
use tokio::net::TcpListener;
use tracing::info;

use crate::config::RelayConfig;
use crate::connection;
use crate::error::RelayError;
use crate::rate::RateLimiter;
use crate::registry::Registry;

#[derive(Debug)]
pub struct AppState {
    pub config: RelayConfig,
    pub registry: Registry,
    pub limiter: RateLimiter,
}

pub type Shared = Arc<AppState>;

/// A bound, not-yet-serving relay.
///
/// Binding is separate from serving so a caller — the binary, or a test on
/// port 0 — can learn the real listening address before traffic starts.
#[derive(Debug)]
pub struct Relay {
    listener: TcpListener,
    local_addr: SocketAddr,
    state: Shared,
}

/// A cheap, cloneable view of a running relay's state, for health endpoints
/// and tests.
#[derive(Debug, Clone)]
pub struct RelayHandle {
    state: Shared,
}

impl RelayHandle {
    pub fn connected_clients(&self) -> usize {
        self.state.registry.client_count()
    }

    pub fn is_online(&self, peer: PeerId) -> bool {
        self.state.registry.is_online(peer)
    }

    pub fn uptime(&self) -> Duration {
        self.state.registry.started_at().elapsed()
    }
}

impl Relay {
    pub async fn bind(config: RelayConfig) -> Result<Self, RelayError> {
        let addr = SocketAddr::new(config.host, config.port);
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|source| RelayError::Bind { addr, source })?;
        let local_addr = listener.local_addr().map_err(RelayError::LocalAddr)?;
        let limiter = RateLimiter::from_config(&config);
        Ok(Self {
            listener,
            local_addr,
            state: Arc::new(AppState {
                config,
                registry: Registry::new(),
                limiter,
            }),
        })
    }

    /// The address actually bound, which is what resolves port 0.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn handle(&self) -> RelayHandle {
        RelayHandle {
            state: Arc::clone(&self.state),
        }
    }

    /// Serves until `shutdown` resolves.
    pub async fn serve_with_shutdown<F>(self, shutdown: F) -> Result<(), RelayError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let app = router(Arc::clone(&self.state));
        axum::serve(
            self.listener,
            // Connection info, not `X-Forwarded-For`: the rate limiter must
            // key on an address the client cannot choose for itself.
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(RelayError::Serve)
    }
}

fn router(state: Shared) -> Router {
    Router::new()
        // `any` rather than `get`: the upgrade arrives as GET over HTTP/1.1 and
        // as CONNECT over HTTP/2, and axum's extractor handles both.
        .route("/", any(ws_handler))
        .route("/healthz", get(healthz))
        .with_state(state)
}

async fn healthz(State(state): State<Shared>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "uptimeSeconds": state.registry.started_at().elapsed().as_secs(),
        "clients": state.registry.client_count(),
    }))
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(state): State<Shared>,
) -> Response {
    // The cap is enforced here, in tungstenite, so an oversized message is
    // refused while it is still arriving rather than after it is buffered.
    ws.max_message_size(MAX_SIGNALING_MESSAGE_BYTES)
        .max_frame_size(MAX_SIGNALING_MESSAGE_BYTES)
        .on_upgrade(move |socket| connection::run(socket, peer.ip(), state))
}

/// Resolves on SIGINT, or on SIGTERM where the platform has it.
pub async fn shutdown_signal() {
    let interrupt = async {
        if tokio::signal::ctrl_c().await.is_err() {
            // Without a signal handler the only sane behaviour is to keep
            // serving and let the supervisor kill the process.
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                sigterm.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = interrupt => info!("interrupted; shutting down"),
        _ = terminate => info!("terminated; shutting down"),
    }
}
