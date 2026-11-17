use std::sync::Arc;

use backend::engine::{Engine, HttpEngine};
use backend::rpc;
use clap::Parser;
use tokio::net::TcpListener;

/// Local LLM harness backend: JSON-RPC over WebSocket in front of a llama.cpp server.
#[derive(Debug, Parser)]
struct Config {
    /// Address:port the WebSocket server binds to.
    #[arg(long, env = "HARNESS_BIND", default_value = "127.0.0.1:8777")]
    bind: String,
    /// Upstream llama-server base URL.
    #[arg(long, env = "HARNESS_SERVER_URL")]
    server_url: String,
    /// Informational model label.
    #[arg(long, env = "HARNESS_MODEL")]
    model: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::parse();
    let engine: Arc<dyn Engine> = Arc::new(HttpEngine::new(config.server_url.clone()));
    let mut props = engine.props().await?;
    if let Some(model) = config.model {
        props.model_name = model;
    }
    let listener = TcpListener::bind(&config.bind).await?;
    println!(
        "listening on ws://{} (model {}, n_ctx {}, upstream {})",
        listener.local_addr()?,
        props.model_name,
        props.n_ctx,
        config.server_url
    );
    rpc::serve(listener, engine, props, None).await;
    Ok(())
}
