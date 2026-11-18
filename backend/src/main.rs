use backend::engine::{build_engine, EngineConfig, EngineKind};
use backend::rpc;
use backend::supervisor::catalog::Catalog;
use backend::supervisor::control;
use backend::supervisor::manager::{HarnessManager, ManagerConfig, ProcessLauncher, WsProbe};
use clap::builder::BoolishValueParser;
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;

/// Local LLM harness backend: JSON-RPC over WebSocket in front of an inference engine.
#[derive(Debug, Parser)]
struct Config {
    /// Address:port the WebSocket server binds to.
    #[arg(long, env = "HARNESS_BIND", default_value = "127.0.0.1:8777")]
    bind: String,
    /// Which engine implementation to use.
    #[arg(long, env = "HARNESS_ENGINE", value_enum, default_value = "http")]
    engine: EngineKind,
    /// Upstream llama-server base URL (http engine).
    #[arg(long, env = "HARNESS_SERVER_URL")]
    server_url: Option<String>,
    /// Model label (http engine) or model file path (in-process engine).
    #[arg(long, env = "HARNESS_MODEL")]
    model: Option<String>,
    /// Context size for the in-process engine.
    #[arg(long, env = "HARNESS_N_CTX")]
    n_ctx: Option<u32>,
    /// GPU layers to offload for the in-process engine.
    #[arg(long, env = "HARNESS_NGL")]
    n_gpu_layers: Option<i32>,
    /// Path to tokenizer.json (candle engine).
    #[arg(long, env = "HARNESS_TOKENIZER")]
    tokenizer: Option<String>,
    /// Run as the supervisor (control server managing worker harnesses) instead of an engine.
    #[arg(long, env = "HARNESS_SUPERVISOR", value_parser = BoolishValueParser::new())]
    supervisor: bool,
    /// Address:port the supervisor control server binds to.
    #[arg(long, env = "HARNESS_SUPERVISOR_BIND", default_value = "0.0.0.0:8778")]
    supervisor_bind: String,
    /// Path to the supervisor catalog TOML (required with --supervisor).
    #[arg(long, env = "HARNESS_CATALOG")]
    catalog: Option<PathBuf>,
    /// Host advertised to clients; overrides the catalog's advertise_host.
    #[arg(long, env = "HARNESS_ADVERTISE_HOST")]
    advertise_host: Option<String>,
}

impl Config {
    fn engine_config(&self) -> EngineConfig {
        EngineConfig {
            kind: self.engine,
            server_url: self.server_url.clone(),
            model: self.model.clone(),
            n_ctx: self.n_ctx,
            n_gpu_layers: self.n_gpu_layers,
            tokenizer: self.tokenizer.clone(),
        }
    }
}

/// Load the catalog, apply the advertise-host override, and run the supervisor control server.
async fn run_supervisor(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let path = config
        .catalog
        .as_ref()
        .ok_or("--catalog (or HARNESS_CATALOG) is required with --supervisor")?;
    let mut catalog = Catalog::load(path)?;
    if let Some(host) = &config.advertise_host {
        catalog.advertise_host = host.clone();
    }
    // The manager owns its catalog while the control server shares one via Arc: one clone.
    let shared = Arc::new(catalog.clone());
    let manager = HarnessManager::new(
        catalog,
        ProcessLauncher::new(),
        WsProbe,
        ManagerConfig::default(),
    );
    let listener = TcpListener::bind(&config.supervisor_bind).await?;
    println!(
        "supervisor listening on ws://{} (advertise {}, {} runtimes, {} model dirs)",
        listener.local_addr()?,
        shared.advertise_host,
        shared.runtimes.len(),
        shared.model_dirs.len()
    );
    control::serve(listener, Arc::new(manager), shared).await;
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::parse();
    if config.supervisor {
        return run_supervisor(&config).await;
    }
    let (engine, mut props) = build_engine(&config.engine_config()).await?;
    // `--model` is a human label only for the HTTP engine; for the in-process llamacpp engine it
    // is the gguf file path, so overriding there would leak the absolute path to the client. Keep
    // the engine-computed label (the file stem) for llamacpp.
    if config.engine == EngineKind::Http {
        if let Some(model) = &config.model {
            props.model_name = model.clone();
        }
    }
    let listener = TcpListener::bind(&config.bind).await?;
    println!(
        "listening on ws://{} (model {}, n_ctx {}, upstream {})",
        listener.local_addr()?,
        props.model_name,
        props.n_ctx,
        config.server_url.as_deref().unwrap_or("in-process")
    );
    rpc::serve(listener, engine, props, None).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::builder::TypedValueParser;
    use clap::CommandFactory;
    use std::ffi::OsStr;

    #[test]
    fn supervisor_env_accepts_boolish_values() {
        let cmd = Config::command();
        let parser = BoolishValueParser::new();
        for (raw, want) in [("1", true), ("yes", true), ("0", false), ("false", false)] {
            let got = parser.parse_ref(&cmd, None, OsStr::new(raw)).unwrap();
            assert_eq!(got, want, "{raw}");
        }
        let config = Config::try_parse_from(["backend", "--supervisor"]).unwrap();
        assert!(config.supervisor);
    }

    #[test]
    fn supervisor_flag_parses_bind_and_catalog() {
        let config = Config::try_parse_from([
            "backend",
            "--supervisor",
            "--supervisor-bind",
            "0.0.0.0:9999",
            "--catalog",
            "C:/x/catalog.toml",
            "--advertise-host",
            "10.0.0.5",
        ])
        .unwrap();
        assert!(config.supervisor);
        assert_eq!(config.supervisor_bind, "0.0.0.0:9999");
        assert_eq!(config.catalog, Some(PathBuf::from("C:/x/catalog.toml")));
        assert_eq!(config.advertise_host.as_deref(), Some("10.0.0.5"));
    }
}
