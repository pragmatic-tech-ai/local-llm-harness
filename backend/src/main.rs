use backend::agents::dispatcher::{Dispatcher, ExplicitLoadPolicy};
use backend::agents::host::{LocalSupervisorHost, ModelHost};
use backend::agents::session_store::SessionStore;
use backend::agents::template::TemplateRules;
use backend::agents::template_store::TemplateStore;
use backend::engine::{build_engine, EngineConfig, EngineKind};
use backend::rpc;
use backend::runtime::builtin::{BuiltinSource, Sandbox};
use backend::runtime::executor::ToolExecutor;
use backend::runtime::hub::Hub;
use backend::runtime::policy::ReadOnlyPolicy;
use backend::runtime::registry::ToolRegistry;
use backend::runtime::server::{CatalogView, PlacementWatcher, RuntimeDeps, RuntimeServer};
use backend::runtime::tool_loop::ToolLoop;
use backend::supervisor::catalog::{Catalog, HostConfigs};
use backend::supervisor::control;
use backend::supervisor::discovery::discover;
use backend::supervisor::manager::{HarnessManager, ManagerConfig, ProcessLauncher, WsProbe};
use clap::builder::BoolishValueParser;
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
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
    /// Address:port the supervisor's agent runtime server binds to.
    #[arg(long, env = "HARNESS_RUNTIME_BIND", default_value = "0.0.0.0:8779")]
    runtime_bind: String,
}

impl Config {
    const TOOL_TIMEOUT: Duration = Duration::from_secs(10);

    /// The runtime's tool registry; a missing or unusable sandbox leaves only the sandbox-free
    /// tools.
    fn tool_registry(catalog: &Catalog) -> Result<ToolRegistry, Box<dyn std::error::Error>> {
        let sandbox = catalog
            .tools
            .as_ref()
            .and_then(|tools| match Sandbox::new(&tools.sandbox) {
                Ok(sandbox) => Some(sandbox),
                Err(e) => {
                    eprintln!("tools sandbox unavailable: {e}");
                    None
                }
            });
        Ok(ToolRegistry::new(vec![Box::new(BuiltinSource::new(
            sandbox,
        ))])?)
    }

    /// Binds the runtime server's port. A failure degrades rather than aborts: the control
    /// plane stays up and no `chat_url` is published.
    async fn bind_runtime(addr: &str) -> Option<TcpListener> {
        match TcpListener::bind(addr).await {
            Ok(listener) => Some(listener),
            Err(e) => {
                eprintln!(
                    "runtime bind {addr} failed: {e}; chat through the runtime is unavailable"
                );
                None
            }
        }
    }

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
    let registry = Arc::new(Config::tool_registry(&catalog)?);
    let executor = Arc::new(ToolExecutor::new(
        registry.clone(),
        Box::new(ReadOnlyPolicy),
        Config::TOOL_TIMEOUT,
    ));
    let tool_loop = Arc::new(ToolLoop::new(executor, registry.clone()));
    let runtime_listener = Config::bind_runtime(&config.runtime_bind).await;
    let runtime_addr = runtime_listener
        .as_ref()
        .map(TcpListener::local_addr)
        .transpose()?;
    // The manager owns its catalog while the control server shares one via Arc: one clone.
    let shared = Arc::new(catalog.clone());
    let manager = Arc::new(HarnessManager::new(
        catalog,
        ProcessLauncher::new(),
        WsProbe,
        ManagerConfig {
            chat_port: runtime_addr.map(|addr| addr.port()),
            ..ManagerConfig::default()
        },
    ));
    if let (Some(listener), Some(addr)) = (runtime_listener, runtime_addr) {
        // Interim wiring; the supervisor-wiring task replaces it with the tested builder.
        let hosts: Vec<Arc<dyn ModelHost>> = HostConfigs::build(&shared)?
            .into_iter()
            .map(|h| {
                Arc::new(LocalSupervisorHost::new(h.name, manager.clone(), &shared))
                    as Arc<dyn ModelHost>
            })
            .collect();
        let models = discover(&shared);
        let rules = TemplateRules {
            runtimes: shared.runtimes.iter().map(|r| r.name.clone()).collect(),
            models: models
                .iter()
                .map(|m| (m.display_name.clone(), m.runtimes.clone()))
                .collect(),
            tools: registry.specs().into_iter().map(|s| s.name).collect(),
        };
        let catalog = CatalogView {
            runtimes: shared
                .runtimes
                .iter()
                .map(|r| serde_json::json!({ "name": r.name }))
                .collect(),
            models: models
                .iter()
                .map(|m| serde_json::json!({ "name": m.display_name, "runtimes": m.runtimes }))
                .collect(),
            hosts: hosts.iter().map(|h| h.name().to_string()).collect(),
        };
        let data_dir = shared.agents.data_dir();
        match (
            TemplateStore::open(&data_dir.join("templates"), rules),
            SessionStore::open(&data_dir.join("sessions")),
        ) {
            (Ok(templates), Ok(sessions)) => {
                let deps = RuntimeDeps {
                    templates: Arc::new(templates),
                    sessions: Arc::new(sessions),
                    dispatcher: Arc::new(Dispatcher::new(hosts, Box::new(ExplicitLoadPolicy))),
                    registry,
                    tool_loop,
                    catalog,
                    hub: Arc::new(Hub::new()),
                };
                println!(
                    "runtime listening on ws://{} ({} tools)",
                    addr,
                    deps.registry.specs().len()
                );
                PlacementWatcher::spawn(&deps);
                tokio::spawn(RuntimeServer::serve(listener, deps));
            }
            (Err(e), _) | (_, Err(e)) => eprintln!("agents data unavailable: {e}"),
        }
    }
    let listener = TcpListener::bind(&config.supervisor_bind).await?;
    println!(
        "supervisor listening on ws://{} (advertise {}, {} runtimes, {} model dirs)",
        listener.local_addr()?,
        shared.advertise_host,
        shared.runtimes.len(),
        shared.model_dirs.len()
    );
    control::serve(listener, manager, shared).await;
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

    #[tokio::test]
    async fn runtime_bind_failure_degrades_to_none() {
        let taken = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = taken.local_addr().unwrap().to_string();
        assert!(Config::bind_runtime(&addr).await.is_none());
        assert!(Config::bind_runtime("127.0.0.1:0").await.is_some());
    }

    #[test]
    fn runtime_bind_defaults_to_8779() {
        let config = Config::try_parse_from(["backend", "--supervisor"]).unwrap();
        assert_eq!(config.runtime_bind, "0.0.0.0:8779");
    }
}
