//! The OpenRouter provider: models run through OpenRouter's chat completions API.
//!
//! [`api`] is the HTTP client (the model list and the streaming chat call, with a small SSE
//! reader), [`link`] is the [`WorkerLink`](crate::runtime::worker::WorkerLink) a session's steps
//! run through (each `generate` stream is a `turn`), and [`provider`] is the always-ready
//! [`Provider`](crate::providers::Provider) that hands those links out for one OpenRouter
//! repository.

pub mod api;
pub mod link;
pub mod provider;
mod turn;

pub use api::{ApiError, OpenRouterApi, OpenRouterModel, SseReader, SseStream};
pub use link::{Calibration, OpenRouterLink};
pub use provider::OpenRouterProvider;

/// A plain-HTTP fake of OpenRouter for tests: serves canned responses, one per connection, and
/// captures each request.
#[cfg(test)]
pub(crate) struct FakeHttp;

/// One canned response: a status and the body, written in `parts` with a flush and a short
/// pause after each so the client reads them as separate network chunks. The body is
/// close-delimited.
#[cfg(test)]
pub(crate) struct Canned {
    pub status: u16,
    pub parts: Vec<String>,
}

/// A request the fake received: its head (request line and headers) and its body.
#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct Captured {
    pub head: String,
    pub body: String,
}

#[cfg(test)]
impl Captured {
    /// The body as JSON.
    pub(crate) fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).expect("request body is JSON")
    }

    /// The value of header `name` (case-insensitive), if sent.
    pub(crate) fn header(&self, name: &str) -> Option<String> {
        self.head.lines().skip(1).find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_string())
        })
    }
}

#[cfg(test)]
impl Canned {
    pub(crate) fn new(status: u16, parts: &[&str]) -> Self {
        Canned {
            status,
            parts: parts.iter().map(|p| p.to_string()).collect(),
        }
    }
}

#[cfg(test)]
impl FakeHttp {
    const HEAD_END: &'static [u8] = b"\r\n\r\n";
    const CONTENT_LENGTH: &'static str = "content-length";
    const PAUSE: std::time::Duration = std::time::Duration::from_millis(30);

    /// Serves `responses` in order, one per accepted connection. Returns the base url
    /// (`http://127.0.0.1:{port}`) and a receiver of the captured requests.
    pub(crate) async fn serve(
        responses: Vec<Canned>,
    ) -> (String, tokio::sync::mpsc::UnboundedReceiver<Captured>) {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            for canned in responses {
                let Ok((mut tcp, _)) = listener.accept().await else {
                    return;
                };
                let captured = Self::read_request(&mut tcp).await;
                let _ = tx.send(captured);
                let head = format!(
                    "HTTP/1.1 {} X\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                    canned.status
                );
                if tcp.write_all(head.as_bytes()).await.is_err() {
                    continue;
                }
                for part in canned.parts {
                    if tcp.write_all(part.as_bytes()).await.is_err() {
                        break;
                    }
                    let _ = tcp.flush().await;
                    tokio::time::sleep(Self::PAUSE).await;
                }
                let _ = tcp.shutdown().await;
            }
        });
        (format!("http://{addr}"), rx)
    }

    async fn read_request(tcp: &mut tokio::net::TcpStream) -> Captured {
        use tokio::io::AsyncReadExt;
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        let head_end = loop {
            if let Some(at) = buffer
                .windows(Self::HEAD_END.len())
                .position(|w| w == Self::HEAD_END)
            {
                break at;
            }
            let n = tcp.read(&mut chunk).await.unwrap_or(0);
            if n == 0 {
                break buffer.len();
            }
            buffer.extend_from_slice(&chunk[..n]);
        };
        let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
        let length = head
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.trim()
                    .eq_ignore_ascii_case(Self::CONTENT_LENGTH)
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        let body_start = (head_end + Self::HEAD_END.len()).min(buffer.len());
        let mut body = buffer[body_start..].to_vec();
        while body.len() < length {
            let n = tcp.read(&mut chunk).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        }
        Captured {
            head,
            body: String::from_utf8_lossy(&body).to_string(),
        }
    }
}
