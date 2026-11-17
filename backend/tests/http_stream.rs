use backend::engine::http::HttpEngine;
use backend::engine::{CompletionChunk, Engine, EngineError, Result, SamplingConfig};
use futures::StreamExt;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const HEAD: &str =
    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";

/// Minimal mock llama.cpp server: answers one request with the given body pieces, each written
/// as its own HTTP chunk (flushed, with a pause) so the client sees the exact byte boundaries.
/// When `terminate` is false the socket is closed without the final zero chunk, which is what a
/// server dying mid-stream looks like on the wire.
async fn serve(pieces: Vec<Vec<u8>>, terminate: bool) -> String {
    serve_capturing(pieces, terminate).await.0
}

/// Like `serve`, but also returns a handle that yields the raw request body the client sent.
async fn serve_capturing(
    pieces: Vec<Vec<u8>>,
    terminate: bool,
) -> (String, tokio::sync::oneshot::Receiver<String>) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let body = drain_request(&mut sock).await;
        let _ = tx.send(body);
        sock.write_all(HEAD.as_bytes()).await.unwrap();
        for b in pieces {
            sock.write_all(format!("{:x}\r\n", b.len()).as_bytes())
                .await
                .unwrap();
            sock.write_all(&b).await.unwrap();
            sock.write_all(b"\r\n").await.unwrap();
            sock.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        if terminate {
            sock.write_all(b"0\r\n\r\n").await.unwrap();
        }
        let _ = sock.shutdown().await;
    });
    (format!("http://{addr}"), rx)
}

async fn drain_request(sock: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = sock.read(&mut tmp).await.unwrap();
        buf.extend_from_slice(&tmp[..n]);
        if let Some(h) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..h]).to_lowercase();
            let len: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .map(|v| v.trim().parse().unwrap())
                .unwrap_or(0);
            if buf.len() >= h + 4 + len {
                return String::from_utf8_lossy(&buf[h + 4..h + 4 + len]).into_owned();
            }
        }
        if n == 0 {
            return String::new();
        }
    }
}

fn piece(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

async fn collect(url: String) -> Vec<Result<CompletionChunk>> {
    let engine = HttpEngine::new(url);
    let stream = engine
        .complete("hi", &SamplingConfig::default())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), stream.collect::<Vec<_>>())
        .await
        .expect("stream hung")
}

#[tokio::test]
async fn streams_chunks_in_order() {
    let url = serve(
        vec![
            piece("data: {\"content\":\"Hel\",\"stop\":false}\n\n"),
            piece("data: {\"content\":\"l\",\"stop\":false,\"completion_probabilities\":[{\"token\":\"l\",\"logprob\":-0.5,\"top_logprobs\":[{\"token\":\"l\",\"logprob\":-0.5},{\"token\":\"x\",\"logprob\":-2.0}]}]}\n\n"),
            piece("data: {\"content\":\"o\",\"stop\":false,\"completion_probabilities\":[{\"content\":\"o\",\"probs\":[{\"tok_str\":\"o\",\"prob\":0.5}]}]}\n\n"),
            piece("data: {\"content\":\"\",\"stop\":true}\n\n"),
        ],
        true,
    )
    .await;
    let items: Vec<CompletionChunk> = collect(url).await.into_iter().map(|r| r.unwrap()).collect();
    let text: String = items.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(text, "Hello");
    assert!(items.last().unwrap().stop);
    assert!(items[..items.len() - 1].iter().all(|c| !c.stop));
    assert_eq!(items[1].tokens[0].token, "l");
    assert_eq!(items[1].tokens[0].logprob, -0.5);
    assert_eq!(items[1].tokens[0].top[1], ("x".to_string(), -2.0));
    assert!((items[2].tokens[0].logprob - 0.5f32.ln()).abs() < 1e-6);
}

#[tokio::test]
async fn utf8_split_across_chunks_renders_whole() {
    let full = "data: {\"content\":\"a😀b\",\"stop\":false}\n\n"
        .as_bytes()
        .to_vec();
    let emoji_at = full.windows(4).position(|w| w == "😀".as_bytes()).unwrap();
    let split = emoji_at + 2; // mid-character
    let url = serve(
        vec![
            full[..split].to_vec(),
            full[split..].to_vec(),
            piece("data: {\"content\":\"\",\"stop\":true}\n\n"),
        ],
        true,
    )
    .await;
    let items: Vec<CompletionChunk> = collect(url).await.into_iter().map(|r| r.unwrap()).collect();
    let text: String = items.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(text, "a😀b");
    assert!(!text.contains('\u{FFFD}'));
}

#[tokio::test]
async fn interrupted_stream_ends_with_error() {
    let url = serve(
        vec![piece("data: {\"content\":\"par\",\"stop\":false}\n\n")],
        false,
    )
    .await;
    let items = collect(url).await;
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].as_ref().unwrap().text, "par");
    assert!(matches!(items[1], Err(EngineError::StreamInterrupted)));
}

#[tokio::test]
async fn request_body_asks_for_stream_and_n_probs() {
    let (url, body) = serve_capturing(
        vec![piece(
            "data: {\"content\":\"\",\"stop\":true}

",
        )],
        true,
    )
    .await;
    collect(url).await;
    let sent: serde_json::Value = serde_json::from_str(&body.await.unwrap()).unwrap();
    assert_eq!(sent["stream"], serde_json::json!(true));
    assert!(sent["n_probs"].as_u64().unwrap() > 0);
    assert_eq!(sent["prompt"], "hi");
}

#[tokio::test]
async fn invalid_utf8_line_is_protocol_error() {
    let mut bad = b"data: {\"content\":\"".to_vec();
    bad.extend_from_slice(&[0xF0, 0x9F]); // truncated emoji, then the line ends
    bad.extend_from_slice(
        b"\"}

",
    );
    let url = serve(vec![bad], true).await;
    let items = collect(url).await;
    assert_eq!(items.len(), 1);
    assert!(matches!(items[0], Err(EngineError::Protocol(_))));
}

#[tokio::test]
async fn legacy_token_missing_from_top_list_is_kept() {
    let url = serve(
        vec![
            piece("data: {\"content\":\"z\",\"stop\":false,\"completion_probabilities\":[{\"content\":\"z\",\"probs\":[{\"tok_str\":\"a\",\"prob\":0.5}]}]}

"),
            piece("data: {\"content\":\"\",\"stop\":true}

"),
        ],
        true,
    )
    .await;
    let items = collect(url).await;
    let first = items[0].as_ref().unwrap();
    assert_eq!(first.tokens.len(), 1);
    assert_eq!(first.tokens[0].token, "z");
    assert!(first.tokens[0].logprob <= 0.5f32.ln());
}
