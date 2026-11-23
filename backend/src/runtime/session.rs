//! A client's conversation with the runtime: the committed transcript and sampling config.

use crate::engine::{ChatMessage, SamplingConfig};

/// One per client connection. The transcript only ever holds whole, answered loops (see
/// `ToolLoop::run`), so it is always a sequence the chat template accepts.
pub struct RuntimeSession {
    transcript: Vec<ChatMessage>,
    pub sampling: SamplingConfig,
}

impl RuntimeSession {
    pub fn new() -> Self {
        Self {
            transcript: Vec::new(),
            sampling: SamplingConfig::default(),
        }
    }

    /// Forgets the conversation; sampling is kept.
    pub fn reset(&mut self) {
        self.transcript.clear();
    }

    pub fn transcript(&self) -> &[ChatMessage] {
        &self.transcript
    }

    /// Replaces the transcript with `transcript`, which must extend it by whole answered
    /// loops.
    pub fn commit(&mut self, transcript: Vec<ChatMessage>) {
        self.transcript = transcript;
    }
}

impl Default for RuntimeSession {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Role;

    #[test]
    fn reset_clears_transcript_but_keeps_sampling() {
        let mut session = RuntimeSession::new();
        session.sampling.temperature = 0.1;
        session.commit(vec![ChatMessage::new(Role::User, "q")]);
        assert_eq!(session.transcript().len(), 1);
        session.reset();
        assert!(session.transcript().is_empty());
        assert_eq!(session.sampling.temperature, 0.1);
    }
}
