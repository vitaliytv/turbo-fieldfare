use std::{net::IpAddr, str::FromStr};

use clap::Parser;
use tracing_subscriber::EnvFilter;
use turbofieldfare_openai_api::{ApiConfig, serve};

#[derive(Debug, Parser)]
#[command(
    name = "turbofieldfare",
    about = "TurboFieldfare Rust OpenAI mock server"
)]
struct Arguments {
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    #[arg(long, default_value_t = 8080)]
    port: u16,

    #[arg(long, default_value = "gemma-4-26b-a4b-it")]
    model: String,

    #[arg(long, default_value_t = 8)]
    queue_limit: usize,

    #[arg(long, default_value_t = 5_000)]
    heartbeat_ms: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let arguments = Arguments::parse();
    let host = IpAddr::from_str(&arguments.host)?;
    if !host.is_loopback() {
        return Err("--host must be a loopback address".into());
    }

    let config = ApiConfig {
        model_id: arguments.model,
        queue_limit: arguments.queue_limit,
        heartbeat_interval: std::time::Duration::from_millis(arguments.heartbeat_ms),
        ..ApiConfig::default()
    };
    serve((host, arguments.port).into(), config).await?;
    Ok(())
}
