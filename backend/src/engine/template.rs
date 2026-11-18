//! Chat-template rendering: turns chat messages into a prompt string using a model's own Jinja
//! chat template.

use minijinja::{context, Environment, Error, ErrorKind};

use super::types::{ChatMessage, Role};

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
    ) -> Result<String, TemplateError> {
        let msgs: Vec<_> = messages
            .iter()
            .map(|m| context! { role => Self::role_str(&m.role), content => &m.content })
            .collect();
        let tmpl = self
            .env
            .get_template(TEMPLATE_NAME)
            .map_err(|e| TemplateError::Render(e.to_string()))?;
        tmpl.render(context! {
            messages => msgs,
            add_generation_prompt => add_generation_prompt,
            enable_thinking => enable_thinking,
            // llama.cpp adds the BOS token itself at tokenization, so the template must not.
            bos_token => "",
        })
        .map_err(|e| TemplateError::Render(e.to_string()))
    }

    fn role_str(role: &Role) -> &'static str {
        match role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msgs() -> Vec<ChatMessage> {
        vec![
            ChatMessage {
                role: Role::System,
                content: "be nice".into(),
            },
            ChatMessage {
                role: Role::User,
                content: "hi".into(),
            },
        ]
    }

    const CHAT: &str = "{% for message in messages %}<start>{{ message.role }}\n{{ message.content }}<end>{% endfor %}{% if add_generation_prompt %}<start>assistant\n{% endif %}";

    #[test]
    fn renders_roles_and_generation_prompt() {
        let t = ChatTemplate::from_jinja(CHAT).unwrap();
        assert_eq!(
            t.render(&msgs(), true, false).unwrap(),
            "<start>system\nbe nice<end><start>user\nhi<end><start>assistant\n"
        );
        assert_eq!(
            t.render(&msgs(), false, false).unwrap(),
            "<start>system\nbe nice<end><start>user\nhi<end>"
        );
    }

    #[test]
    fn enable_thinking_variable_is_available() {
        let t = ChatTemplate::from_jinja("{% if enable_thinking %}THINK{% endif %}").unwrap();
        assert_eq!(t.render(&msgs(), false, true).unwrap(), "THINK");
        assert_eq!(t.render(&msgs(), false, false).unwrap(), "");
    }

    #[test]
    fn enable_thinking_ignored_when_unreferenced() {
        let t = ChatTemplate::from_jinja(CHAT).unwrap();
        assert_eq!(
            t.render(&msgs(), true, true).unwrap(),
            t.render(&msgs(), true, false).unwrap()
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
            t.render(&msgs(), false, false),
            Err(TemplateError::Render(_))
        ));
    }

    #[test]
    fn bos_token_is_empty() {
        let t = ChatTemplate::from_jinja("[{{ bos_token }}]").unwrap();
        assert_eq!(t.render(&msgs(), false, false).unwrap(), "[]");
    }

    #[test]
    fn python_style_string_methods_resolve() {
        let t = ChatTemplate::from_jinja(
            "{% for m in messages %}{{ m.content.strip() }}{% if m.role.startswith('us') %}!{% endif %}{% endfor %}",
        )
        .unwrap();
        let m = vec![ChatMessage {
            role: Role::User,
            content: "  hi  ".into(),
        }];
        assert_eq!(t.render(&m, false, false).unwrap(), "hi!");
    }
}
