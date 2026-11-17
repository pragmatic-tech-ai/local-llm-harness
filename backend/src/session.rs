use std::sync::{Arc, Mutex};

use futures::stream::BoxStream;
use futures::StreamExt;

use crate::engine::{
    ChatMessage, CompletionChunk, Engine, EngineError, ModelProps, Result, Role, SamplingConfig,
};

/// How much of the context window the current conversation occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextFill {
    pub used: usize,
    pub total: usize,
}

/// Why a turn was refused before any generation started.
#[derive(Debug, thiserror::Error)]
pub enum TurnRejected {
    #[error("context window full: need {needed} of {total} tokens")]
    WindowFull { needed: usize, total: usize },
    #[error(transparent)]
    Engine(#[from] EngineError),
}

/// A conversation: owns the message list, renders prompts via the engine's chat template,
/// and refuses turns that would overflow the context window.
pub struct ChatSession {
    engine: Arc<dyn Engine>,
    props: ModelProps,
    messages: Arc<Mutex<Vec<ChatMessage>>>,
}

impl ChatSession {
    pub fn new(engine: Arc<dyn Engine>, props: ModelProps, system: Option<String>) -> Self {
        let messages = system
            .map(|content| {
                vec![ChatMessage {
                    role: Role::System,
                    content,
                }]
            })
            .unwrap_or_default();
        Self {
            engine,
            props,
            messages: Arc::new(Mutex::new(messages)),
        }
    }

    pub fn messages(&self) -> Vec<ChatMessage> {
        self.messages.lock().expect("messages lock").clone()
    }

    /// The exact prompt string for the current conversation (also what the UI displays).
    pub async fn build_prompt(&self, enable_thinking: bool) -> Result<String> {
        Self::render(&*self.engine, &self.messages(), enable_thinking).await
    }

    /// Current fill, measured on the prompt rendered with thinking off.
    pub async fn context_fill(&self) -> Result<ContextFill> {
        let prompt = self.build_prompt(false).await?;
        let used = self.engine.tokenize(&prompt).await?.count;
        Ok(ContextFill {
            used,
            total: self.props.n_ctx,
        })
    }

    /// Starts a turn. Rejects with `WindowFull` before any `complete` call when the prompt plus
    /// `max_tokens` headroom would not fit. On success the user message is recorded and the
    /// returned stream appends the assistant reply to the history when it finishes.
    pub async fn send(
        &mut self,
        user: &str,
        cfg: &SamplingConfig,
        enable_thinking: bool,
    ) -> std::result::Result<BoxStream<'static, Result<CompletionChunk>>, TurnRejected> {
        let user_message = ChatMessage {
            role: Role::User,
            content: user.to_string(),
        };
        let mut candidate = self.messages();
        candidate.push(user_message.clone());
        let prompt = Self::render(&*self.engine, &candidate, enable_thinking).await?;
        let used = self.engine.tokenize(&prompt).await?.count;
        let needed = used + cfg.max_tokens.max(0) as usize;
        let total = self.props.n_ctx;
        if needed > total {
            return Err(TurnRejected::WindowFull { needed, total });
        }

        let stream = self.engine.complete(&prompt, cfg).await?;
        self.messages
            .lock()
            .expect("messages lock")
            .push(user_message);

        let history = Arc::clone(&self.messages);
        let mut reply = String::new();
        Ok(stream
            .map(move |item| {
                if let Ok(chunk) = &item {
                    reply.push_str(&chunk.text);
                    if chunk.stop {
                        history.lock().expect("messages lock").push(ChatMessage {
                            role: Role::Assistant,
                            content: std::mem::take(&mut reply),
                        });
                    }
                }
                item
            })
            .boxed())
    }

    async fn render(
        engine: &dyn Engine,
        messages: &[ChatMessage],
        enable_thinking: bool,
    ) -> Result<String> {
        engine.apply_template(messages, true, enable_thinking).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::TokenizeResult;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeEngine {
        token_count: usize,
        complete_calls: AtomicUsize,
        template_calls: Mutex<Vec<(Vec<ChatMessage>, bool, bool)>>,
    }

    impl FakeEngine {
        fn new(token_count: usize) -> Arc<Self> {
            Arc::new(Self {
                token_count,
                complete_calls: AtomicUsize::new(0),
                template_calls: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl Engine for FakeEngine {
        async fn props(&self) -> Result<ModelProps> {
            unreachable!()
        }
        async fn tokenize(&self, _text: &str) -> Result<TokenizeResult> {
            Ok(TokenizeResult {
                tokens: vec![],
                count: self.token_count,
            })
        }
        async fn detokenize(&self, _tokens: &[i64]) -> Result<String> {
            unreachable!()
        }
        async fn complete(
            &self,
            _prompt: &str,
            _cfg: &SamplingConfig,
        ) -> Result<BoxStream<'static, Result<CompletionChunk>>> {
            self.complete_calls.fetch_add(1, Ordering::SeqCst);
            let chunks = ["Hel", "lo"].iter().enumerate().map(|(i, t)| {
                Ok(CompletionChunk {
                    text: t.to_string(),
                    tokens: vec![],
                    stop: i == 1,
                })
            });
            Ok(futures::stream::iter(chunks).boxed())
        }
        async fn apply_template(
            &self,
            messages: &[ChatMessage],
            add_generation_prompt: bool,
            enable_thinking: bool,
        ) -> Result<String> {
            self.template_calls.lock().unwrap().push((
                messages.to_vec(),
                add_generation_prompt,
                enable_thinking,
            ));
            Ok("SENTINEL".to_string())
        }
    }

    fn props(n_ctx: usize) -> ModelProps {
        ModelProps {
            n_ctx,
            model_name: "m".into(),
        }
    }

    fn msg(role: Role, content: &str) -> ChatMessage {
        ChatMessage {
            role,
            content: content.into(),
        }
    }

    #[tokio::test]
    async fn build_prompt_delegates_to_apply_template() {
        let engine = FakeEngine::new(1);
        let mut session = ChatSession::new(engine.clone(), props(100), Some("sys".into()));
        let mut stream = session
            .send("hi", &SamplingConfig::default(), true)
            .await
            .unwrap();
        while stream.next().await.is_some() {}
        engine.template_calls.lock().unwrap().clear();

        let prompt = session.build_prompt(true).await.unwrap();
        assert_eq!(prompt, "SENTINEL");
        let calls = engine.template_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].0,
            vec![
                msg(Role::System, "sys"),
                msg(Role::User, "hi"),
                msg(Role::Assistant, "Hello")
            ]
        );
        assert!(calls[0].1);
    }

    #[tokio::test]
    async fn thinking_toggle_is_forwarded() {
        let engine = FakeEngine::new(1);
        let session = ChatSession::new(engine.clone(), props(100), None);
        session.build_prompt(false).await.unwrap();
        session.build_prompt(true).await.unwrap();
        let calls = engine.template_calls.lock().unwrap();
        assert!(!calls[0].2);
        assert!(calls[1].2);
    }

    #[tokio::test]
    async fn context_fill_uses_props_n_ctx() {
        let engine = FakeEngine::new(10);
        let session = ChatSession::new(engine, props(4096), None);
        assert_eq!(
            session.context_fill().await.unwrap(),
            ContextFill {
                used: 10,
                total: 4096
            }
        );
    }

    #[tokio::test]
    async fn oversized_first_message_is_rejected() {
        let engine = FakeEngine::new(5000);
        let mut session = ChatSession::new(engine.clone(), props(4096), None);
        let result = session
            .send("huge", &SamplingConfig::default(), false)
            .await;
        assert!(matches!(
            result,
            Err(TurnRejected::WindowFull {
                needed: 5000,
                total: 4096
            })
        ));
        assert_eq!(engine.complete_calls.load(Ordering::SeqCst), 0);
        assert!(session.messages().is_empty());
    }

    #[tokio::test]
    async fn send_appends_user_and_assistant_reply() {
        let engine = FakeEngine::new(10);
        let mut session = ChatSession::new(engine, props(4096), None);
        let mut stream = session
            .send("hi", &SamplingConfig::default(), false)
            .await
            .unwrap();
        while stream.next().await.is_some() {}
        assert_eq!(
            session.messages(),
            vec![msg(Role::User, "hi"), msg(Role::Assistant, "Hello")]
        );
    }
}
