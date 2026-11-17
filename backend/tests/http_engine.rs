use backend::engine::http::HttpEngine;
use backend::engine::{Engine, EngineError};
use serde_json::json;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn tokenize_maps_llama_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/tokenize"))
        .and(body_json(json!({"content": "Hello world"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"tokens": [9906, 1917]})))
        .mount(&server)
        .await;

    let engine = HttpEngine::new(server.uri());
    let result = engine.tokenize("Hello world").await.unwrap();
    assert_eq!(result.count, 2);
    assert_eq!(result.tokens, vec![9906, 1917]);
}

#[tokio::test]
async fn detokenize_maps_llama_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/detokenize"))
        .and(body_json(json!({"tokens": [9906, 1917]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"content": "Hello world"})))
        .mount(&server)
        .await;

    let engine = HttpEngine::new(server.uri());
    assert_eq!(
        engine.detokenize(&[9906, 1917]).await.unwrap(),
        "Hello world"
    );
}

#[tokio::test]
async fn props_reads_n_ctx() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/props"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "default_generation_settings": {"n_ctx": 4096},
            "model": "llama.gguf"
        })))
        .mount(&server)
        .await;

    let engine = HttpEngine::new(server.uri());
    let props = engine.props().await.unwrap();
    assert_eq!(props.n_ctx, 4096);
    assert_eq!(props.model_name, "llama.gguf");
}

#[tokio::test]
async fn non_2xx_is_protocol_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/props"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let engine = HttpEngine::new(server.uri());
    assert!(matches!(
        engine.props().await,
        Err(EngineError::Protocol(_))
    ));
}

#[tokio::test]
async fn connection_refused_is_typed() {
    // Bind then drop a listener to obtain a port with nothing listening.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let engine = HttpEngine::new(format!("http://127.0.0.1:{port}"));
    assert!(matches!(
        engine.tokenize("x").await,
        Err(EngineError::ConnectionRefused)
    ));
}
