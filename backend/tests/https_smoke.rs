use serde_json::Value;

const MODELS_URL: &str = "https://openrouter.ai/api/v1/models";
const DATA_KEY: &str = "data";
const NOT_ARRAY: &str = "response body has no `data` array";
const REQUEST_FAILED: &str = "https request";
const NOT_JSON: &str = "json body";
const OK_STATUS: u16 = 200;
const BAD_STATUS: &str = "the models list did not answer 200";

/// Proves reqwest can complete a TLS handshake (native-tls) from this machine.
/// Needs network access, so it only runs with `-- --ignored`.
#[tokio::test]
#[ignore]
async fn https_get_openrouter_models_list() {
    let resp = reqwest::get(MODELS_URL).await.expect(REQUEST_FAILED);
    assert_eq!(resp.status().as_u16(), OK_STATUS, "{BAD_STATUS}");
    let body: Value = resp.json().await.expect(NOT_JSON);
    assert!(body[DATA_KEY].is_array(), "{NOT_ARRAY}");
}
