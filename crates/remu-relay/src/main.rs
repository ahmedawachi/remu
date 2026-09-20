//! `remu-relay` — the signalling server binary.

#![forbid(unsafe_code)]

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use remu_relay::{primary_lan_ipv4, shutdown_signal, Relay, RelayConfig};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "remu-relay",
    version,
    about = "Signalling relay for Remu remote desktop"
)]
struct Cli {
    /// Address to bind. Defaults to every interface so peers on the same LAN
    /// can reach it.
    #[arg(long, env = "REMU_RELAY_HOST", default_value_t = IpAddr::V4(Ipv4Addr::UNSPECIFIED))]
    host: IpAddr,

    #[arg(long, env = "REMU_RELAY_PORT", default_value_t = 8765)]
    port: u16,

    /// Shared secret every client must present in `register`. Unset means the
    /// relay is open.
    #[arg(long, env = "REMU_RELAY_TOKEN", hide_env_values = true)]
    token: Option<String>,

    /// Sustained registrations allowed per source address, per minute. 0
    /// disables the limit.
    #[arg(long, env = "REMU_RELAY_REGISTER_RATE", default_value_t = 30.0)]
    register_rate: f64,

    /// Registrations a source address may make back to back.
    #[arg(long, env = "REMU_RELAY_REGISTER_BURST", default_value_t = 10)]
    register_burst: u32,

    /// Sustained messages allowed per source address, per second. 0 disables
    /// the limit.
    #[arg(long, env = "REMU_RELAY_MESSAGE_RATE", default_value_t = 50.0)]
    message_rate: f64,

    /// Messages a source address may send back to back.
    #[arg(long, env = "REMU_RELAY_MESSAGE_BURST", default_value_t = 200)]
    message_burst: u32,

    /// Seconds between keepalive pings on an idle connection.
    #[arg(long, env = "REMU_RELAY_HEARTBEAT_SECS", default_value_t = 20)]
    heartbeat_secs: u64,

    /// Seconds a connection may stay silent — pongs included — before it is
    /// dropped.
    #[arg(long, env = "REMU_RELAY_PONG_TIMEOUT_SECS", default_value_t = 60)]
    pong_timeout_secs: u64,

    /// Messages that may queue for one client before it is dropped as too slow.
    #[arg(long, env = "REMU_RELAY_SEND_QUEUE", default_value_t = 64)]
    send_queue: usize,

    /// Log filter, e.g. `info`, `debug`, or `remu_relay=debug,axum=warn`.
    /// `RUST_LOG` overrides it.
    #[arg(long, env = "REMU_RELAY_LOG_LEVEL", default_value = "info")]
    log_level: String,
}

impl Cli {
    fn into_config(self) -> RelayConfig {
        RelayConfig {
            host: self.host,
            port: self.port,
            token: self.token,
            registrations_per_minute: self.register_rate,
            registration_burst: self.register_burst,
            messages_per_second: self.message_rate,
            message_burst: self.message_burst,
            heartbeat: Duration::from_secs(self.heartbeat_secs),
            pong_timeout: Duration::from_secs(self.pong_timeout_secs),
            send_queue_depth: self.send_queue,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&cli.log_level))
        .with_context(|| format!("invalid log filter {:?}", cli.log_level))?;
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let guarded = cli.token.is_some();
    let config = cli.into_config();
    let relay = Relay::bind(config).await?;
    let addr = relay.local_addr();

    info!(%addr, token_required = guarded, "remu-relay listening");
    info!("  local:   ws://localhost:{}", addr.port());
    info!("  health:  http://localhost:{}/healthz", addr.port());
    if let Some(lan) = primary_lan_ipv4() {
        // The one line users actually need: what to type on the other machine.
        info!(
            "  LAN:     ws://{}:{}   <- share this with peers on this network",
            lan,
            addr.port()
        );
    }

    relay.serve_with_shutdown(shutdown_signal()).await?;
    info!("relay stopped");
    Ok(())
}
