use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::tool::{SessionHandle, Tool, ToolContext, ToolOutcome, ToolSpec};

/// Limits and messages shared by the note tools.
struct NoteRules;

impl NoteRules {
    const MAX_NOTES: usize = 20;
    const MAX_CHARS: usize = 300;
    const NO_SESSION: &'static str = "notes need a session";
    const SESSION_DELETED: &'static str = "session deleted";
    const EMPTY: &'static str = "note text is empty";
    const TOO_LONG: &'static str = "note exceeds 300 characters";
    const LIMIT: &'static str = "note limit reached (20); use note_clear to start over";
    const TEXT_ARG: &'static str = "text";
    const TEXT_MISSING: &'static str = "text must be a string";

    fn session(ctx: &ToolContext) -> Result<&Arc<dyn SessionHandle>, String> {
        ctx.scope
            .session
            .as_ref()
            .ok_or_else(|| Self::NO_SESSION.to_string())
    }

    fn current(session: &Arc<dyn SessionHandle>) -> Result<Vec<String>, String> {
        session
            .notes()
            .ok_or_else(|| Self::SESSION_DELETED.to_string())
    }

    fn outcome(result: Result<Value, String>) -> ToolOutcome {
        match result {
            Ok(v) => ToolOutcome::Ok(v),
            Err(e) => ToolOutcome::Err(e),
        }
    }
}

/// `note_add`: appends a note to the session's scratchpad.
pub struct NoteAddTool;

impl NoteAddTool {
    fn add(args: &Value, ctx: &ToolContext) -> Result<Value, String> {
        let session = NoteRules::session(ctx)?;
        let mut notes = NoteRules::current(session)?;
        let text = args
            .get(NoteRules::TEXT_ARG)
            .and_then(Value::as_str)
            .ok_or_else(|| NoteRules::TEXT_MISSING.to_string())?;
        if text.trim().is_empty() {
            return Err(NoteRules::EMPTY.to_string());
        }
        if text.chars().count() > NoteRules::MAX_CHARS {
            return Err(NoteRules::TOO_LONG.to_string());
        }
        if notes.len() >= NoteRules::MAX_NOTES {
            return Err(NoteRules::LIMIT.to_string());
        }
        notes.push(text.to_string());
        let count = notes.len();
        if !session.set_notes(notes) {
            return Err(NoteRules::SESSION_DELETED.to_string());
        }
        Ok(json!({"number": count, "count": count}))
    }
}

#[async_trait]
impl Tool for NoteAddTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "note_add".into(),
            description: "Add a short note (up to 300 characters, 20 notes max) to your scratchpad. Notes stay visible to you on every turn.".into(),
            parameters: json!({
                "type": "object",
                "properties": {"text": {"type": "string", "description": "The note text."}},
                "required": ["text"]
            }),
            side_effects: false,
        }
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> ToolOutcome {
        NoteRules::outcome(Self::add(&args, ctx))
    }
}

/// `note_list`: returns the numbered notes.
pub struct NoteListTool;

impl NoteListTool {
    fn list(ctx: &ToolContext) -> Result<Value, String> {
        let notes = NoteRules::current(NoteRules::session(ctx)?)?;
        let numbered: Vec<Value> = notes
            .iter()
            .enumerate()
            .map(|(i, text)| json!({"number": i + 1, "text": text}))
            .collect();
        Ok(json!({"notes": numbered}))
    }
}

#[async_trait]
impl Tool for NoteListTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "note_list".into(),
            description: "List the notes on your scratchpad, numbered.".into(),
            parameters: json!({"type": "object", "properties": {}}),
            side_effects: false,
        }
    }

    async fn call(&self, _args: Value, ctx: &ToolContext) -> ToolOutcome {
        NoteRules::outcome(Self::list(ctx))
    }
}

/// `note_clear`: empties the scratchpad.
pub struct NoteClearTool;

impl NoteClearTool {
    fn clear(ctx: &ToolContext) -> Result<Value, String> {
        let session = NoteRules::session(ctx)?;
        let cleared = NoteRules::current(session)?.len();
        if !session.set_notes(Vec::new()) {
            return Err(NoteRules::SESSION_DELETED.to_string());
        }
        Ok(json!({"cleared": cleared}))
    }
}

#[async_trait]
impl Tool for NoteClearTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "note_clear".into(),
            description: "Remove every note from your scratchpad.".into(),
            parameters: json!({"type": "object", "properties": {}}),
            side_effects: false,
        }
    }

    async fn call(&self, _args: Value, ctx: &ToolContext) -> ToolOutcome {
        NoteRules::outcome(Self::clear(ctx))
    }
}

/// Renders the notes as the block the loop shows the model each turn.
pub struct NotesBlock;

impl NotesBlock {
    pub const HEADER: &'static str = "[Your notes]";

    /// None when there are no notes.
    pub fn text(notes: &[String]) -> Option<String> {
        if notes.is_empty() {
            return None;
        }
        let mut out = Self::HEADER.to_string();
        for (i, note) in notes.iter().enumerate() {
            out.push_str(&format!("\n{}. {note}", i + 1));
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::tool::ToolScope;
    use std::sync::Mutex;

    /// A session whose notes are None once deleted.
    struct FakeSession {
        notes: Mutex<Option<Vec<String>>>,
    }

    impl FakeSession {
        fn alive() -> Arc<Self> {
            Arc::new(Self {
                notes: Mutex::new(Some(Vec::new())),
            })
        }
    }

    impl SessionHandle for FakeSession {
        fn notes(&self) -> Option<Vec<String>> {
            self.notes.lock().unwrap().clone()
        }

        fn set_notes(&self, notes: Vec<String>) -> bool {
            let mut guard = self.notes.lock().unwrap();
            if guard.is_none() {
                return false;
            }
            *guard = Some(notes);
            true
        }

        fn exists(&self) -> bool {
            self.notes.lock().unwrap().is_some()
        }
    }

    struct Calls;

    impl Calls {
        fn ctx(session: Option<Arc<FakeSession>>) -> ToolContext {
            ToolContext {
                call_id: "c1".into(),
                scope: ToolScope {
                    workspace: None,
                    session: session.map(|s| s as Arc<dyn SessionHandle>),
                },
            }
        }

        async fn add(ctx: &ToolContext, text: &str) -> ToolOutcome {
            NoteAddTool.call(json!({"text": text}), ctx).await
        }

        fn err(msg: &str) -> ToolOutcome {
            ToolOutcome::Err(msg.to_string())
        }
    }

    #[tokio::test]
    async fn note_add_numbers_from_one() {
        let ctx = Calls::ctx(Some(FakeSession::alive()));
        assert_eq!(
            Calls::add(&ctx, "a").await,
            ToolOutcome::Ok(json!({"number": 1, "count": 1}))
        );
        assert_eq!(
            Calls::add(&ctx, "b").await,
            ToolOutcome::Ok(json!({"number": 2, "count": 2}))
        );
    }

    #[tokio::test]
    async fn note_list_returns_numbered_notes() {
        let ctx = Calls::ctx(Some(FakeSession::alive()));
        Calls::add(&ctx, "a").await;
        Calls::add(&ctx, "b").await;
        assert_eq!(
            NoteListTool.call(json!({}), &ctx).await,
            ToolOutcome::Ok(
                json!({"notes": [{"number": 1, "text": "a"}, {"number": 2, "text": "b"}]})
            )
        );
    }

    #[tokio::test]
    async fn note_clear_reports_count_and_empties() {
        let session = FakeSession::alive();
        let ctx = Calls::ctx(Some(session.clone()));
        assert_eq!(
            NoteClearTool.call(json!({}), &ctx).await,
            ToolOutcome::Ok(json!({"cleared": 0}))
        );
        Calls::add(&ctx, "a").await;
        Calls::add(&ctx, "b").await;
        assert_eq!(
            NoteClearTool.call(json!({}), &ctx).await,
            ToolOutcome::Ok(json!({"cleared": 2}))
        );
        assert_eq!(session.notes(), Some(Vec::new()));
    }

    #[tokio::test]
    async fn empty_note_is_refused() {
        let ctx = Calls::ctx(Some(FakeSession::alive()));
        assert_eq!(Calls::add(&ctx, "").await, Calls::err("note text is empty"));
        assert_eq!(
            Calls::add(&ctx, "  \n").await,
            Calls::err("note text is empty")
        );
    }

    #[tokio::test]
    async fn note_limit_counts_chars_not_bytes() {
        let ctx = Calls::ctx(Some(FakeSession::alive()));
        assert!(matches!(
            Calls::add(&ctx, &"я".repeat(300)).await,
            ToolOutcome::Ok(_)
        ));
        assert_eq!(
            Calls::add(&ctx, &"я".repeat(301)).await,
            Calls::err("note exceeds 300 characters")
        );
    }

    #[tokio::test]
    async fn twenty_first_note_is_refused() {
        let ctx = Calls::ctx(Some(FakeSession::alive()));
        for i in 0..20 {
            assert!(matches!(
                Calls::add(&ctx, &format!("n{i}")).await,
                ToolOutcome::Ok(_)
            ));
        }
        assert_eq!(
            Calls::add(&ctx, "one more").await,
            Calls::err("note limit reached (20); use note_clear to start over")
        );
    }

    #[tokio::test]
    async fn notes_without_session_scope_fail() {
        let ctx = Calls::ctx(None);
        let want = Calls::err("notes need a session");
        assert_eq!(Calls::add(&ctx, "a").await, want);
        assert_eq!(NoteListTool.call(json!({}), &ctx).await, want);
        assert_eq!(NoteClearTool.call(json!({}), &ctx).await, want);
    }

    #[tokio::test]
    async fn notes_on_deleted_session_fail() {
        let session = FakeSession::alive();
        let ctx = Calls::ctx(Some(session.clone()));
        *session.notes.lock().unwrap() = None;
        let want = Calls::err("session deleted");
        assert_eq!(Calls::add(&ctx, "a").await, want);
        assert_eq!(NoteListTool.call(json!({}), &ctx).await, want);
        assert_eq!(NoteClearTool.call(json!({}), &ctx).await, want);
    }

    #[test]
    fn notes_block_text_format() {
        assert_eq!(
            NotesBlock::text(&["a".into(), "b".into()]),
            Some("[Your notes]\n1. a\n2. b".into())
        );
        assert_eq!(NotesBlock::text(&[]), None);
    }
}
