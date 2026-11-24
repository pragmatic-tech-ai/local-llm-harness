//! A persisted agent session: one conversation started from a template.

use serde::{Deserialize, Serialize};

use super::template::{AgentTemplate, CompactionState};
use crate::engine::types::{ChatMessage, Role};
use crate::engine::SamplingConfig;

/// Current on-disk schema version of [`SessionRecord`].
const SCHEMA: u32 = 1;

/// One session as stored on disk (`{id}.json`). Times are unix seconds. Runtime state such as
/// "busy" is never part of the record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub schema: u32,
    pub id: String,
    pub title: String,
    pub template_id: String,
    pub snapshot: AgentTemplate,
    pub created: u64,
    pub updated: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling: Option<SamplingConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_thinking: Option<bool>,
    #[serde(default)]
    pub transcript: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<CompactionState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl SessionRecord {
    /// A fresh record for `snapshot` with the initial transcript.
    pub fn new(
        id: String,
        title: String,
        template_id: &str,
        snapshot: AgentTemplate,
        now: u64,
    ) -> Self {
        let transcript = Self::initial_transcript(&snapshot);
        Self {
            schema: SCHEMA,
            id,
            title,
            template_id: template_id.to_string(),
            snapshot,
            created: now,
            updated: now,
            sampling: None,
            enable_thinking: None,
            transcript,
            compaction: None,
            notes: Vec::new(),
        }
    }

    /// `[System(system_prompt)]` when the prompt is non-empty, otherwise empty.
    pub fn initial_transcript(snapshot: &AgentTemplate) -> Vec<ChatMessage> {
        if snapshot.system_prompt.is_empty() {
            Vec::new()
        } else {
            vec![ChatMessage::new(
                Role::System,
                snapshot.system_prompt.clone(),
            )]
        }
    }

    pub fn summary(&self) -> SessionSummary {
        SessionSummary {
            id: self.id.clone(),
            title: self.title.clone(),
            template_id: self.template_id.clone(),
            template_name: self.snapshot.name.clone(),
            runtime: self.snapshot.runtime.clone(),
            model: self.snapshot.model.clone(),
            updated: self.updated,
        }
    }
}

/// The list-view projection of a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub template_id: String,
    pub template_name: String,
    pub runtime: String,
    pub model: String,
    pub updated: u64,
}
