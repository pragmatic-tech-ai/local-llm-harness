//! Chat-template rendering: turns chat messages into a prompt string using a model's own Jinja
//! chat template.

use minijinja::{context, Environment, Error, ErrorKind};
use serde_json::Value;

use super::types::ChatMessage;

const TEMPLATE_NAME: &str = "chat";

#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    #[error("template compile error: {0}")]
    Compile(String),
    #[error("template render error: {0}")]
    Render(String),
}

/// A compiled chat template.
pub struct ChatTemplate {
    env: Environment<'static>,
}

impl ChatTemplate {
    /// Compiles a Jinja chat template. Registers `raise_exception(msg)`, which real model
    /// templates (Gemma, Llama) call to reject unsupported conversations.
    pub fn from_jinja(src: &str) -> Result<Self, TemplateError> {
        let mut env = Environment::new();
        // Real templates use Python-style string methods (.strip(), .startswith()) and
        // helpers such as strftime_now.
        env.set_unknown_method_callback(minijinja_contrib::pycompat::unknown_method_callback);
        minijinja_contrib::add_to_environment(&mut env);
        env.add_function("raise_exception", |msg: String| -> Result<String, Error> {
            Err(Error::new(ErrorKind::InvalidOperation, msg))
        });
        env.add_template_owned(TEMPLATE_NAME, src.to_owned())
            .map_err(|e| TemplateError::Compile(e.to_string()))?;
        Ok(Self { env })
    }

    /// Renders `messages` into a prompt string.
    pub fn render(
        &self,
        messages: &[ChatMessage],
        add_generation_prompt: bool,
        enable_thinking: bool,
        tools: &[Value],
    ) -> Result<String, TemplateError> {
        let msgs: Vec<_> = messages
            .iter()
            .map(|m| minijinja::Value::from_serialize(m.template_view()))
            .collect();
        let tools = minijinja::Value::from_serialize(tools);
        let tmpl = self
            .env
            .get_template(TEMPLATE_NAME)
            .map_err(|e| TemplateError::Render(e.to_string()))?;
        tmpl.render(context! {
            messages => msgs,
            add_generation_prompt => add_generation_prompt,
            enable_thinking => enable_thinking,
            tools => tools,
            // llama.cpp adds the BOS token itself at tokenization, so the template must not.
            bos_token => "",
        })
        .map_err(|e| TemplateError::Render(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::types::{FunctionCall, Role, ToolCall};

    fn msgs() -> Vec<ChatMessage> {
        vec![
            ChatMessage::new(Role::System, "be nice"),
            ChatMessage::new(Role::User, "hi"),
        ]
    }

    const CHAT: &str = "{% for message in messages %}<start>{{ message.role }}\n{{ message.content }}<end>{% endfor %}{% if add_generation_prompt %}<start>assistant\n{% endif %}";

    #[test]
    fn renders_roles_and_generation_prompt() {
        let t = ChatTemplate::from_jinja(CHAT).unwrap();
        assert_eq!(
            t.render(&msgs(), true, false, &[]).unwrap(),
            "<start>system\nbe nice<end><start>user\nhi<end><start>assistant\n"
        );
        assert_eq!(
            t.render(&msgs(), false, false, &[]).unwrap(),
            "<start>system\nbe nice<end><start>user\nhi<end>"
        );
    }

    #[test]
    fn enable_thinking_variable_is_available() {
        let t = ChatTemplate::from_jinja("{% if enable_thinking %}THINK{% endif %}").unwrap();
        assert_eq!(t.render(&msgs(), false, true, &[]).unwrap(), "THINK");
        assert_eq!(t.render(&msgs(), false, false, &[]).unwrap(), "");
    }

    #[test]
    fn enable_thinking_ignored_when_unreferenced() {
        let t = ChatTemplate::from_jinja(CHAT).unwrap();
        assert_eq!(
            t.render(&msgs(), true, true, &[]).unwrap(),
            t.render(&msgs(), true, false, &[]).unwrap()
        );
    }

    #[test]
    fn bad_template_errors_not_panics() {
        assert!(matches!(
            ChatTemplate::from_jinja("{% if %}{{ unclosed"),
            Err(TemplateError::Compile(_))
        ));
    }

    #[test]
    fn raise_exception_yields_render_error() {
        let t = ChatTemplate::from_jinja("{{ raise_exception('nope') }}").unwrap();
        assert!(matches!(
            t.render(&msgs(), false, false, &[]),
            Err(TemplateError::Render(_))
        ));
    }

    #[test]
    fn bos_token_is_empty() {
        let t = ChatTemplate::from_jinja("[{{ bos_token }}]").unwrap();
        assert_eq!(t.render(&msgs(), false, false, &[]).unwrap(), "[]");
    }

    #[test]
    fn python_style_string_methods_resolve() {
        let t = ChatTemplate::from_jinja(
            "{% for m in messages %}{{ m.content.strip() }}{% if m.role.startswith('us') %}!{% endif %}{% endfor %}",
        )
        .unwrap();
        let m = vec![ChatMessage::new(Role::User, "  hi  ")];
        assert_eq!(t.render(&m, false, false, &[]).unwrap(), "hi!");
    }

    const GEMMA4: &str = include_str!("../../tests/fixtures/gemma4-chat-template.jinja");

    fn time_tool() -> Value {
        serde_json::json!({"type":"function","function":{"name":"get_time","description":"Current time","parameters":{"type":"object","properties":{}}}})
    }

    #[test]
    fn gemma4_declares_tools_in_system_turn() {
        let t = ChatTemplate::from_jinja(GEMMA4).unwrap();
        let m = vec![ChatMessage::new(Role::User, "what time is it?")];
        let prompt = t.render(&m, true, false, &[time_tool()]).unwrap();
        assert!(
            prompt.contains("declaration:get_time{description:<|\"|>Current time<|\"|>"),
            "{prompt}"
        );
    }

    #[test]
    fn gemma4_renders_assistant_tool_calls_and_tool_results() {
        let t = ChatTemplate::from_jinja(GEMMA4).unwrap();
        let mut assistant = ChatMessage::new(Role::Assistant, "");
        assistant.tool_calls = Some(vec![ToolCall {
            id: "call_1".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "get_time".into(),
                arguments: serde_json::json!({}),
            },
        }]);
        let mut tool = ChatMessage::new(Role::Tool, "");
        tool.tool_call_id = Some("call_1".into());
        tool.tool_result = Some(serde_json::json!({"utc": "2026-11-23T13:05:00Z"}));
        let m = vec![ChatMessage::new(Role::User, "q"), assistant, tool];
        let prompt = t.render(&m, true, false, &[time_tool()]).unwrap();
        assert!(
            prompt.contains("<|tool_call>call:get_time{}<tool_call|>"),
            "{prompt}"
        );
        assert!(
            prompt.contains("<|tool_response>response:get_time{utc:<|\"|>2026-11-23T13:05:00Z<|\"|>}<tool_response|>"),
            "{prompt}"
        );
    }

    #[test]
    fn gemma4_without_tools_has_no_declarations() {
        let t = ChatTemplate::from_jinja(GEMMA4).unwrap();
        let m = vec![ChatMessage::new(Role::User, "hi")];
        let prompt = t.render(&m, true, false, &[]).unwrap();
        assert!(!prompt.contains("declaration:"), "{prompt}");
    }
}
