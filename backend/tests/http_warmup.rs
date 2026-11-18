use std::time::Duration;

use backend::engine::{build_engine_with, EngineConfig, EngineKind, HttpWarmup};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn http_cfg(url: String) -> EngineConfig {
    EngineConfig {
        kind: EngineKind::Http,
        server_url: Some(url),
        model: None,
        n_ctx: None,
        n_gpu_layers: None,
        tokenizer: None,
    }
}

fn fast() -> HttpWarmup {
    HttpWarmup {
        retry_delay: Duration::from_millis(10),
        window: Duration::from_secs(5),
    }
}

#[tokio::test]
async fn http_build_retries_through_503_until_ready() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/props"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({"message": "Loading model"})))
        .up_to_n_times(3)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/props"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "default_generation_settings": {"n_ctx": 4096},
            "model_path": "m.gguf",
            "model_alias": "gemma",
            "model_ftype": "Q4_0"
        })))
        .mount(&server)
        .await;

    let (_engine, props) = build_engine_with(&http_cfg(server.uri()), fast())
        .await
        .unwrap();
    assert_eq!(props.n_ctx, 4096);
    assert_eq!(props.model_name, "m.gguf");
}

#[tokio::test]
async fn http_build_gives_up_after_window() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/props"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let warmup = HttpWarmup {
        retry_delay: Duration::from_millis(10),
        window: Duration::from_millis(100),
    };
    assert!(build_engine_with(&http_cfg(server.uri()), warmup)
        .await
        .is_err());
}
