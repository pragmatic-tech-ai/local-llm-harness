//! Tool-call formats: the `ToolCallFormat`/`ToolCallParser` seam the worker uses, and the
//! Gemma 4 syntax behind it (value grammar, single-call parsing and a streaming parser).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use super::template::OpenChannel;

/// One tool call decoded from model output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
    /// The text between the call markers, verbatim.
    pub raw: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parse_error: Option<String>,
}

/// Something the streaming parser decided about.
#[derive(Debug, Clone, PartialEq)]
pub enum ParseEvent {
    Call(ParsedCall),
    ResponseMarker,
}

/// A model family's tool-call syntax, as the worker needs it: whether a chat template renders
/// tools in it, and a parser for the model's output.
pub trait ToolCallFormat: Send + Sync {
    /// True iff the chat template source `src` renders tools in this syntax.
    fn template_supports(&self, src: &str) -> bool;

    /// A fresh streaming parser for one generation.
    fn new_parser(&self) -> Box<dyn ToolCallParser>;

    /// A fresh thinking-token counter for one generation that continues `prompt`.
    fn think_counter(&self, prompt: &str) -> Box<dyn ThinkCounter>;
}

/// Counts the tokens of one generation that were spent inside a thinking channel.
pub trait ThinkCounter: Send {
    /// Feeds one streamed chunk: its text and the number of tokens it carried.
    fn push(&mut self, text: &str, tokens: usize);

    /// Tokens counted as thinking so far.
    fn think_tokens(&self) -> usize;
}

/// The counter for formats without channel markers: always zero.
#[derive(Default)]
pub struct NoThinkCounter;

impl ThinkCounter for NoThinkCounter {
    fn push(&mut self, _text: &str, _tokens: usize) {}

    fn think_tokens(&self) -> usize {
        0
    }
}

/// Counts thinking tokens in Gemma 4 output. A chunk is attributed to the state the counter
/// was in at the start of the chunk; the state then flips on each opener or closer found in
/// the chunk. A short carry of the previous tail lets a marker split across chunks be seen.
pub struct GemmaThinkCounter {
    inside: bool,
    think: usize,
    carry: String,
}

impl GemmaThinkCounter {
    const THINK_OPEN: &'static str = "<|channel>thought";
    const CHANNEL_CLOSE: &'static str = "<channel|>";

    /// Starts inside a thinking channel when `prompt` ends with the `thought` opener.
    pub fn new(prompt: &str) -> Self {
        let inside = OpenChannel::at_tail(prompt)
            .map_or(false, |opener| opener.trim_end() == Self::THINK_OPEN);
        Self {
            inside,
            think: 0,
            carry: String::new(),
        }
    }

    /// Bytes of tail kept between chunks: one short of the longest marker.
    const CARRY_LEN: usize = Self::THINK_OPEN.len() - 1;
}

impl ThinkCounter for GemmaThinkCounter {
    fn push(&mut self, text: &str, tokens: usize) {
        if self.inside {
            self.think += tokens;
        }
        let carried = self.carry.len();
        let window = format!("{}{}", self.carry, text);
        let mut markers: Vec<(usize, bool)> = Vec::new();
        for (marker, opens) in [(Self::THINK_OPEN, true), (Self::CHANNEL_CLOSE, false)] {
            for (at, _) in window.match_indices(marker) {
                // Markers wholly inside the carry were already applied.
                if at + marker.len() > carried {
                    markers.push((at, opens));
                }
            }
        }
        markers.sort_by_key(|(at, _)| *at);
        for (_, opens) in markers {
            self.inside = opens;
        }
        let mut start = window.len().saturating_sub(Self::CARRY_LEN);
        while !window.is_char_boundary(start) {
            start += 1;
        }
        self.carry = window[start..].to_string();
    }

    fn think_tokens(&self) -> usize {
        self.think
    }
}

/// Picks the thinking counter for a prompt, for callers that hold no format instance.
pub struct ThinkCounters;

impl ThinkCounters {
    /// A Gemma counter when the prompt carries Gemma 4 turn/channel syntax, else a no-op one.
    pub fn for_prompt(prompt: &str) -> Box<dyn ThinkCounter> {
        if prompt.contains(GemmaToolFormat::TURN_OPEN)
            || prompt.contains(GemmaToolFormat::CHANNEL_OPEN)
        {
            GemmaToolFormat.think_counter(prompt)
        } else {
            Box::new(NoThinkCounter)
        }
    }
}

/// Streaming parser over one generation's output.
pub trait ToolCallParser: Send {
    /// Feeds streamed text; returns what was decided by it.
    fn push(&mut self, text: &str) -> Vec<ParseEvent>;

    /// Signals end of stream; reports anything still undecided.
    fn finish(&mut self) -> Vec<ParseEvent>;

    /// The text outside call spans.
    fn content(&self) -> String;

    /// The calls decoded so far.
    fn calls(&self) -> Vec<ParsedCall>;
}

/// The tool-call formats the worker knows.
pub struct ToolCallFormats;

impl ToolCallFormats {
    const KNOWN: &'static [&'static dyn ToolCallFormat] = &[&GemmaToolFormat];

    /// The format a chat template renders tools in, if it is a known one.
    pub fn detect(src: &str) -> Option<&'static dyn ToolCallFormat> {
        Self::KNOWN
            .iter()
            .copied()
            .find(|format| format.template_supports(src))
    }
}

/// A parser for generations made without tools: every byte is content, nothing is a call.
#[derive(Default)]
pub struct PlainTextParser {
    content: String,
}

impl PlainTextParser {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ToolCallParser for PlainTextParser {
    fn push(&mut self, text: &str) -> Vec<ParseEvent> {
        self.content.push_str(text);
        Vec::new()
    }

    fn finish(&mut self) -> Vec<ParseEvent> {
        Vec::new()
    }

    fn content(&self) -> String {
        self.content.clone()
    }

    fn calls(&self) -> Vec<ParsedCall> {
        Vec::new()
    }
}

/// The Gemma 4 tool-call syntax.
pub struct GemmaToolFormat;

impl ToolCallFormat for GemmaToolFormat {
    /// True iff the chat template mentions both `tools` and the call marker.
    fn template_supports(&self, src: &str) -> bool {
        src.contains("tools") && src.contains(Self::CALL_OPEN)
    }

    fn new_parser(&self) -> Box<dyn ToolCallParser> {
        Box::new(GemmaToolParser::new())
    }

    fn think_counter(&self, prompt: &str) -> Box<dyn ThinkCounter> {
        Box::new(GemmaThinkCounter::new(prompt))
    }
}

impl GemmaToolFormat {
    const CALL_OPEN: &'static str = "<|tool_call>";
    const CALL_CLOSE: &'static str = "<tool_call|>";
    const RESPONSE_OPEN: &'static str = "<|tool_response>";
    const TURN_OPEN: &'static str = "<|turn>";
    const CHANNEL_OPEN: &'static str = "<|channel>";
    const QUOTE: &'static str = "<|\"|>";
    const CALL_PREFIX: &'static str = "call:";
    const MAX_DEPTH: usize = 64;

    /// Parses one value of the Gemma 4 grammar; trailing non-whitespace is an error.
    pub fn parse_value(text: &str) -> Result<Value, String> {
        let mut reader = ValueReader::new(text);
        let value = reader.value(0)?;
        reader.skip_ws();
        if reader.at_end() {
            Ok(value)
        } else {
            Err(format!("unexpected trailing text at byte {}", reader.pos))
        }
    }

    /// Parses `call:NAME{…}` (the text between the call markers).
    pub fn parse_call(body: &str) -> Result<(String, Value), String> {
        let rest = body
            .trim()
            .strip_prefix(Self::CALL_PREFIX)
            .ok_or_else(|| "tool call does not start with `call:`".to_string())?;
        let brace = rest
            .find('{')
            .ok_or_else(|| "tool call has no argument object".to_string())?;
        let name = rest[..brace].trim();
        if name.is_empty() {
            return Err("tool call has no name".to_string());
        }
        let args = Self::parse_value(&rest[brace..])?;
        Ok((name.to_string(), args))
    }

    /// Best-effort name for a call whose arguments failed to parse.
    fn salvage_name(body: &str) -> String {
        body.trim()
            .strip_prefix(Self::CALL_PREFIX)
            .and_then(|rest| rest.split('{').next())
            .map(|n| n.trim().to_string())
            .unwrap_or_default()
    }

    /// Looks at the start of `rest` against `delims`: a complete delimiter (its index),
    /// a proper prefix of one (more input needed), or neither.
    fn classify(rest: &str, delims: &[&str]) -> Match {
        for (i, d) in delims.iter().enumerate() {
            if rest.starts_with(d) {
                return Match::Full(i);
            }
        }
        if delims.iter().any(|d| d.starts_with(rest)) {
            Match::Partial
        } else {
            Match::None
        }
    }
}

enum Match {
    Full(usize),
    Partial,
    None,
}

/// Recursive-descent reader for the value grammar.
struct ValueReader<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> ValueReader<'a> {
    fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn at_end(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn skip_ws(&mut self) {
        let trimmed = self.rest().trim_start();
        self.pos = self.src.len() - trimmed.len();
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += c.len_utf8();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: char) -> Result<(), String> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(format!("expected `{}` at byte {}", c, self.pos))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > GemmaToolFormat::MAX_DEPTH {
            return Err("value nested too deeply".to_string());
        }
        self.skip_ws();
        if self.rest().starts_with(GemmaToolFormat::QUOTE) {
            return self.string().map(Value::String);
        }
        match self.peek() {
            Some('{') => self.object(depth),
            Some('[') => self.array(depth),
            Some(c) if c == '-' || c.is_ascii_digit() => self.number(),
            Some(_) => self.literal(),
            None => Err("expected a value, found end of input".to_string()),
        }
    }

    fn string(&mut self) -> Result<String, String> {
        let quote = GemmaToolFormat::QUOTE;
        self.pos += quote.len();
        let end = self
            .rest()
            .find(quote)
            .ok_or_else(|| "unterminated string".to_string())?;
        let s = self.rest()[..end].to_string();
        self.pos += end + quote.len();
        Ok(s)
    }

    fn literal(&mut self) -> Result<Value, String> {
        for (word, value) in [
            ("true", Value::Bool(true)),
            ("false", Value::Bool(false)),
            ("null", Value::Null),
        ] {
            if self.rest().starts_with(word) {
                self.pos += word.len();
                return Ok(value);
            }
        }
        Err(format!("unexpected input at byte {}", self.pos))
    }

    fn number(&mut self) -> Result<Value, String> {
        let rest = self.rest();
        let len = rest
            .find(|c: char| !matches!(c, '0'..='9' | '-' | '+' | '.' | 'e' | 'E'))
            .unwrap_or(rest.len());
        let text = &rest[..len];
        let integral = !text.contains(['.', 'e', 'E']);
        let parsed = if integral {
            text.parse::<i64>()
                .ok()
                .map(Number::from)
                .or_else(|| text.parse::<u64>().ok().map(Number::from))
        } else {
            None
        };
        let number = match parsed {
            Some(n) => n,
            None => text
                .parse::<f64>()
                .ok()
                .and_then(Number::from_f64)
                .ok_or_else(|| format!("invalid number `{}`", text))?,
        };
        self.pos += len;
        Ok(Value::Number(number))
    }

    fn key(&mut self) -> Result<String, String> {
        self.skip_ws();
        if self.rest().starts_with(GemmaToolFormat::QUOTE) {
            return self.string();
        }
        let colon = self
            .rest()
            .find([':', ',', '{', '}'])
            .filter(|&i| self.rest().as_bytes()[i] == b':')
            .ok_or_else(|| format!("object key without `:` at byte {}", self.pos))?;
        let key = self.rest()[..colon].trim().to_string();
        if key.is_empty() {
            return Err("empty object key".to_string());
        }
        self.pos += colon;
        Ok(key)
    }

    fn object(&mut self, depth: usize) -> Result<Value, String> {
        self.expect('{')?;
        let mut map = Map::new();
        self.skip_ws();
        if self.eat('}') {
            return Ok(Value::Object(map));
        }
        loop {
            let key = self.key()?;
            self.skip_ws();
            self.expect(':')?;
            let value = self.value(depth + 1)?;
            map.insert(key, value);
            self.skip_ws();
            if self.eat(',') {
                continue;
            }
            self.expect('}')?;
            return Ok(Value::Object(map));
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, String> {
        self.expect('[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.eat(']') {
            return Ok(Value::Array(items));
        }
        loop {
            items.push(self.value(depth + 1)?);
            self.skip_ws();
            if self.eat(',') {
                continue;
            }
            self.expect(']')?;
            return Ok(Value::Array(items));
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Text,
    InCall,
}

/// Incremental parser over streamed model output.
///
/// Markers and `<|"|>` delimiters may be split across pushes at any point; text that could
/// still be the start of a marker is held back (not in `content()`) until it is decided.
/// Strings inside a call are opaque: `<tool_call|>` inside `<|"|>…<|"|>` does not end the call.
///
/// Unterminated calls: until `finish()` is called, a call without its closing marker is
/// simply not in `calls()`. `finish()` (end of stream) reports it as a `Call` carrying a
/// `parse_error`, so the runtime can tell the model rather than silently dropping it; it
/// also releases any held-back marker-prefix text into `content()`.
pub struct GemmaToolParser {
    pending: String,
    mode: Mode,
    /// Scan state for the call body in `pending` (resumes where the last push stopped).
    scan_pos: usize,
    in_string: bool,
    content: String,
    calls: Vec<ParsedCall>,
    /// Set once the stream is over (response marker or `finish()`); later pushes are ignored.
    closed: bool,
}

impl GemmaToolParser {
    const TEXT_DELIMS: [&'static str; 2] =
        [GemmaToolFormat::CALL_OPEN, GemmaToolFormat::RESPONSE_OPEN];
    const OUTSIDE_DELIMS: [&'static str; 3] = [
        GemmaToolFormat::CALL_CLOSE,
        GemmaToolFormat::QUOTE,
        GemmaToolFormat::RESPONSE_OPEN,
    ];
    const QUOTE_AT: usize = 1;
    const RESPONSE_AT: usize = 2;
    const INSIDE_DELIMS: [&'static str; 1] = [GemmaToolFormat::QUOTE];
    const UNTERMINATED: &'static str = "unterminated tool call";

    pub fn new() -> Self {
        Self {
            pending: String::new(),
            mode: Mode::Text,
            scan_pos: 0,
            in_string: false,
            content: String::new(),
            calls: Vec::new(),
            closed: false,
        }
    }

    /// Releases decided text; returns true if a marker was consumed (state changed).
    fn step_text(&mut self, events: &mut Vec<ParseEvent>) -> bool {
        let mut i = 0;
        while let Some(off) = self.pending[i..].find('<') {
            i += off;
            match GemmaToolFormat::classify(&self.pending[i..], &Self::TEXT_DELIMS) {
                Match::Full(which) => {
                    let delim_len = Self::TEXT_DELIMS[which].len();
                    self.content.push_str(&self.pending[..i]);
                    self.pending.drain(..i + delim_len);
                    if which == 0 {
                        self.mode = Mode::InCall;
                        self.scan_pos = 0;
                        self.in_string = false;
                    } else {
                        self.end_with_response(events);
                    }
                    return true;
                }
                Match::Partial => {
                    self.content.push_str(&self.pending[..i]);
                    self.pending.drain(..i);
                    return false;
                }
                Match::None => i += 1,
            }
        }
        self.content.push_str(&self.pending);
        self.pending.clear();
        false
    }

    /// Looks for the closing marker outside strings; returns true once a call completed.
    fn step_call(&mut self, events: &mut Vec<ParseEvent>) -> bool {
        let mut i = self.scan_pos;
        while let Some(off) = self.pending[i..].find('<') {
            i += off;
            let delims: &[&str] = if self.in_string {
                &Self::INSIDE_DELIMS
            } else {
                &Self::OUTSIDE_DELIMS
            };
            match GemmaToolFormat::classify(&self.pending[i..], delims) {
                Match::Full(which) if self.in_string || which == Self::QUOTE_AT => {
                    self.in_string = !self.in_string;
                    i += GemmaToolFormat::QUOTE.len();
                }
                Match::Full(which) => {
                    let body = self.pending[..i].to_string();
                    let delim_len = Self::OUTSIDE_DELIMS[which].len();
                    self.pending.drain(..i + delim_len);
                    self.mode = Mode::Text;
                    self.scan_pos = 0;
                    let call = self.build_call(body, None);
                    events.push(ParseEvent::Call(call));
                    if which == Self::RESPONSE_AT {
                        // The response opener outside a string implicitly ends the call.
                        self.end_with_response(events);
                    }
                    return true;
                }
                Match::Partial => {
                    self.scan_pos = i;
                    return false;
                }
                Match::None => i += 1,
            }
        }
        self.scan_pos = self.pending.len();
        false
    }

    fn end_with_response(&mut self, events: &mut Vec<ParseEvent>) {
        self.pending.clear();
        self.closed = true;
        events.push(ParseEvent::ResponseMarker);
    }

    fn build_call(&mut self, body: String, forced_error: Option<String>) -> ParsedCall {
        let id = format!("call_{}", self.calls.len() + 1);
        let parsed = match forced_error {
            Some(e) => Err(e),
            None => GemmaToolFormat::parse_call(&body),
        };
        let call = match parsed {
            Ok((name, arguments)) => ParsedCall {
                id,
                name,
                arguments,
                raw: body,
                parse_error: None,
            },
            Err(e) => ParsedCall {
                id,
                name: GemmaToolFormat::salvage_name(&body),
                arguments: Value::Null,
                raw: body,
                parse_error: Some(e),
            },
        };
        self.calls.push(call.clone());
        call
    }
}

impl ToolCallParser for GemmaToolParser {
    fn push(&mut self, text: &str) -> Vec<ParseEvent> {
        let mut events = Vec::new();
        if self.closed {
            return events;
        }
        self.pending.push_str(text);
        loop {
            let progressed = match self.mode {
                Mode::Text => self.step_text(&mut events),
                Mode::InCall => self.step_call(&mut events),
            };
            if !progressed || self.closed {
                break;
            }
        }
        events
    }

    /// Signals end of stream; see the type docs for the unterminated-call behaviour.
    fn finish(&mut self) -> Vec<ParseEvent> {
        let mut events = Vec::new();
        if self.closed {
            return events;
        }
        self.closed = true;
        match self.mode {
            Mode::Text => {
                let held = std::mem::take(&mut self.pending);
                self.content.push_str(&held);
            }
            Mode::InCall => {
                let body = std::mem::take(&mut self.pending);
                let call = self.build_call(body, Some(Self::UNTERMINATED.to_string()));
                events.push(ParseEvent::Call(call));
                self.mode = Mode::Text;
                self.scan_pos = 0;
                self.in_string = false;
            }
        }
        events
    }

    /// Text outside call spans, excluding held-back bytes; nothing at or after the response marker.
    fn content(&self) -> String {
        self.content.clone()
    }

    fn calls(&self) -> Vec<ParsedCall> {
        self.calls.clone()
    }
}

impl Default for GemmaToolParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const READ_FILE: &str =
        "<|tool_call>call:read_file{max_bytes:100,path:<|\"|>notes.txt<|\"|>}<tool_call|>";

    struct TestFeed;

    impl TestFeed {
        fn chars(parser: &mut GemmaToolParser, text: &str) -> Vec<ParseEvent> {
            let mut events = Vec::new();
            for c in text.chars() {
                events.extend(parser.push(&c.to_string()));
            }
            events
        }

        /// Parses `text` whole and char-by-char; asserts both agree and returns the whole-fed parser.
        fn both(text: &str) -> GemmaToolParser {
            let mut whole = GemmaToolParser::new();
            let mut whole_events = whole.push(text);
            whole_events.extend(whole.finish());
            let mut slow = GemmaToolParser::new();
            let mut slow_events = Self::chars(&mut slow, text);
            slow_events.extend(slow.finish());
            assert_eq!(slow_events, whole_events, "events differ for {:?}", text);
            assert_eq!(slow.calls(), whole.calls(), "calls differ for {:?}", text);
            assert_eq!(
                slow.content(),
                whole.content(),
                "content differs for {:?}",
                text
            );
            whole
        }
    }

    #[test]
    fn parse_value_accepts_u64_beyond_i64() {
        let v = GemmaToolFormat::parse_value("{n:18446744073709551615}").unwrap();
        assert_eq!(v["n"].as_u64(), Some(u64::MAX));
        assert!(GemmaToolFormat::parse_value("{n:99999999999999999999}").is_ok());
    }

    #[test]
    fn malformed_bare_key_errors() {
        assert!(GemmaToolFormat::parse_value("{a,b:1}").is_err());
        assert!(GemmaToolFormat::parse_value("{a}").is_err());
        assert!(GemmaToolFormat::parse_value("{a{b:1}:2}").is_err());
        assert!(GemmaToolFormat::parse_value("{x:1,y}").is_err());
    }

    #[test]
    fn response_marker_inside_call_ends_it_implicitly() {
        let text = "hi <|tool_call>call:get_time{}<|tool_response>response:x{}";
        let p = TestFeed::both(text);
        assert_eq!(p.content(), "hi ");
        let calls = p.calls();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].parse_error.is_none());
        assert_eq!(calls[0].name, "get_time");
        let mut q = GemmaToolParser::new();
        let events = TestFeed::chars(&mut q, text);
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], ParseEvent::Call(_)));
        assert_eq!(events[1], ParseEvent::ResponseMarker);
    }

    #[test]
    fn response_marker_inside_string_is_opaque() {
        let text = "<|tool_call>call:f{s:<|\"|>a<|tool_response>b<|\"|>}<tool_call|>";
        let p = TestFeed::both(text);
        let events = GemmaToolParser::new().push(text);
        assert!(!events.contains(&ParseEvent::ResponseMarker));
        assert_eq!(p.calls()[0].arguments, json!({"s":"a<|tool_response>b"}));
    }

    #[test]
    fn finish_closes_parser_and_ids_stop_advancing() {
        let mut p = GemmaToolParser::new();
        p.push("<|tool_call>call:a{}<tool_call|>");
        p.finish();
        assert!(p.push("<|tool_call>call:b{}<tool_call|>").is_empty());
        assert!(p.finish().is_empty());
        assert_eq!(p.calls().len(), 1);
        assert_eq!(p.content(), "");
    }

    #[test]
    fn unterminated_string_at_finish_reports_error() {
        let mut p = GemmaToolParser::new();
        p.push("<|tool_call>call:f{s:<|\"|>abc<tool_call|>");
        assert!(p.calls().is_empty());
        let events = p.finish();
        assert_eq!(events.len(), 1);
        let calls = p.calls();
        assert_eq!(calls[0].name, "f");
        assert_eq!(
            calls[0].parse_error.as_deref(),
            Some("unterminated tool call")
        );
    }

    #[test]
    fn whole_and_char_fed_agree_over_corpus() {
        let corpus = [
            "plain text",
            "",
            "a <|x b <|to",
            "before<|tool_call>call:a{x:1}<tool_call|>after",
            "t<|tool_call>call:a{}<tool_call|><|tool_call>call:b{y:[1,2]}<tool_call|>",
            "text then<|tool_response>response:a{}",
            "t <|tool_call>call:a{}<tool_call|> more<|tool_response>r",
            "<|tool_call>call:a{s:<|\"|>}<tool_call|><|\"|>}<tool_call|>",
            "héllo ✓ <|tool_call>call:a{s:<|\"|>ü<|\"|>}<tool_call|>",
            "<|tool_call>call:a{bad",
        ];
        for text in corpus {
            TestFeed::both(text);
        }
    }

    #[test]
    fn stray_close_marker_in_text_stays_content() {
        let p = TestFeed::both("a <tool_call|> b");
        assert_eq!(p.content(), "a <tool_call|> b");
        assert!(p.calls().is_empty());
    }

    #[test]
    fn quote_delimiter_as_last_chunk_before_brace() {
        let mut p = GemmaToolParser::new();
        p.push("<|tool_call>call:f{s:<|\"|>x");
        p.push("<|\"|>");
        assert!(p.calls().is_empty());
        p.push("}<tool_call|>");
        assert_eq!(p.calls()[0].arguments, json!({"s":"x"}));
        assert!(p.calls()[0].parse_error.is_none());
    }

    #[test]
    fn parse_value_covers_every_kind() {
        let v = GemmaToolFormat::parse_value(
            "{a:1,b:2.5,c:true,d:null,e:<|\"|>x<|\"|>,f:[1,<|\"|>y<|\"|>],g:{h:false}}",
        )
        .unwrap();
        assert_eq!(
            v,
            json!({"a":1,"b":2.5,"c":true,"d":null,"e":"x","f":[1,"y"],"g":{"h":false}})
        );
        assert!(v["a"].is_i64());
    }

    #[test]
    fn parse_value_allows_whitespace_and_quoted_keys() {
        let v =
            GemmaToolFormat::parse_value(" { <|\"|>k k<|\"|> : [ 1 , -2 ] , z : 1e3 } ").unwrap();
        assert_eq!(v, json!({"k k":[1,-2],"z":1000.0}));
    }

    #[test]
    fn parse_value_rejects_garbage() {
        assert!(GemmaToolFormat::parse_value("{a:}").is_err());
        assert!(GemmaToolFormat::parse_value("{a:1} x").is_err());
        assert!(GemmaToolFormat::parse_value("{a:<|\"|>open}").is_err());
        assert!(GemmaToolFormat::parse_value("[1,").is_err());
        assert!(GemmaToolFormat::parse_value("bogus").is_err());
        assert!(GemmaToolFormat::parse_value("").is_err());
        assert!(GemmaToolFormat::parse_value("-").is_err());
        let deep = "[".repeat(500);
        assert!(GemmaToolFormat::parse_value(&deep).is_err());
    }

    #[test]
    fn string_values_keep_structural_characters() {
        let v = GemmaToolFormat::parse_value("{q:<|\"|>a{b},c:d[e]\"f<|\"|>}").unwrap();
        assert_eq!(v, json!({"q":"a{b},c:d[e]\"f"}));
    }

    #[test]
    fn parse_call_reads_name_and_args() {
        let (name, args) =
            GemmaToolFormat::parse_call("call:read_file{max_bytes:100,path:<|\"|>notes.txt<|\"|>}")
                .unwrap();
        assert_eq!(name, "read_file");
        assert_eq!(args, json!({"max_bytes":100,"path":"notes.txt"}));
    }

    #[test]
    fn parse_call_with_no_args() {
        let (name, args) = GemmaToolFormat::parse_call("call:get_time{}").unwrap();
        assert_eq!(name, "get_time");
        assert_eq!(args, json!({}));
    }

    #[test]
    fn parser_strips_call_from_content() {
        let mut p = GemmaToolParser::new();
        let events = p.push("Let me check.<|tool_call>call:get_time{}<tool_call|>");
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParseEvent::Call(c) => {
                assert_eq!(c.id, "call_1");
                assert_eq!(c.name, "get_time");
                assert!(c.parse_error.is_none());
            }
            other => panic!("unexpected {:?}", other),
        }
        assert_eq!(p.content(), "Let me check.");
    }

    #[test]
    fn parser_two_calls_get_sequential_ids() {
        let mut p = GemmaToolParser::new();
        p.push("<|tool_call>call:a{}<tool_call|> and <|tool_call>call:b{}<tool_call|>");
        let calls = p.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            (calls[0].id.as_str(), calls[0].name.as_str()),
            ("call_1", "a")
        );
        assert_eq!(
            (calls[1].id.as_str(), calls[1].name.as_str()),
            ("call_2", "b")
        );
        assert_eq!(p.content(), " and ");
    }

    #[test]
    fn parses_when_fed_one_char_at_a_time() {
        let mut whole = GemmaToolParser::new();
        whole.push(READ_FILE);
        let mut slow = GemmaToolParser::new();
        let events = TestFeed::chars(&mut slow, READ_FILE);
        assert_eq!(events.len(), 1);
        assert_eq!(slow.calls(), whole.calls());
        assert_eq!(slow.calls()[0].name, "read_file");
        assert_eq!(slow.content(), "");
    }

    #[test]
    fn close_marker_inside_string_does_not_end_call() {
        let text = "héllo ✓ <|tool_call>call:f{s:<|\"|>ü<tool_call|>x<|\"|>}<tool_call|> tail";
        let mut whole = GemmaToolParser::new();
        whole.push(text);
        let mut slow = GemmaToolParser::new();
        TestFeed::chars(&mut slow, text);
        for p in [&whole, &slow] {
            let calls = p.calls();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].arguments, json!({"s":"ü<tool_call|>x"}));
            assert!(calls[0].parse_error.is_none());
            assert_eq!(p.content(), "héllo ✓  tail");
        }
    }

    #[test]
    fn malformed_call_reports_parse_error() {
        let mut p = GemmaToolParser::new();
        let events = p.push("<|tool_call>call:x{a:}<tool_call|>");
        match &events[0] {
            ParseEvent::Call(c) => {
                assert!(c.parse_error.is_some());
                assert_eq!(c.raw, "call:x{a:}");
                assert_eq!(c.name, "x");
            }
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn response_marker_ends_parsing() {
        let mut p = GemmaToolParser::new();
        let events =
            p.push("<|tool_call>call:get_time{}<tool_call|><|tool_response>response:get_time{}");
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], ParseEvent::Call(_)));
        assert_eq!(events[1], ParseEvent::ResponseMarker);
        assert!(p.push("more").is_empty());
        assert_eq!(p.content(), "");
        assert_eq!(p.calls().len(), 1);
    }

    #[test]
    fn text_before_response_marker_is_content() {
        let mut p = GemmaToolParser::new();
        let events = TestFeed::chars(&mut p, "hi<|tool_response>junk");
        assert_eq!(p.content(), "hi");
        assert_eq!(events, [ParseEvent::ResponseMarker]);
    }

    #[test]
    fn held_back_marker_prefix_is_not_content_until_decided() {
        let mut p = GemmaToolParser::new();
        p.push("a <|tool_c");
        assert_eq!(p.content(), "a ");
        p.push("all>call:f{}<tool_call|>");
        assert_eq!(p.content(), "a ");
        assert_eq!(p.calls().len(), 1);
    }

    #[test]
    fn false_marker_prefix_is_released() {
        let mut p = GemmaToolParser::new();
        p.push("a <|to");
        assert_eq!(p.content(), "a ");
        p.push("y");
        assert_eq!(p.content(), "a <|toy");
        assert!(p.calls().is_empty());
        p.push(" <<|tool_call>call:f{}<tool_call|>");
        assert_eq!(p.content(), "a <|toy <");
        assert_eq!(p.calls().len(), 1);
    }

    #[test]
    fn finish_releases_held_prefix_as_content() {
        let mut p = GemmaToolParser::new();
        p.push("a <|tool");
        assert_eq!(p.content(), "a ");
        assert!(p.finish().is_empty());
        assert_eq!(p.content(), "a <|tool");
    }

    #[test]
    fn unterminated_call_is_excluded_until_finish_then_reported_with_error() {
        let mut p = GemmaToolParser::new();
        p.push("x<|tool_call>call:f{a:1");
        assert!(p.calls().is_empty());
        assert_eq!(p.content(), "x");
        let events = p.finish();
        assert_eq!(events.len(), 1);
        let calls = p.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].name, "f");
        assert_eq!(calls[0].raw, "call:f{a:1");
        assert_eq!(
            calls[0].parse_error.as_deref(),
            Some("unterminated tool call")
        );
        assert_eq!(p.content(), "x");
        assert!(p.finish().is_empty());
    }

    #[test]
    fn parsed_call_serde_round_trip_and_skips_none_error() {
        let call = ParsedCall {
            id: "call_1".into(),
            name: "f".into(),
            arguments: json!({"a":1}),
            raw: "call:f{a:1}".into(),
            parse_error: None,
        };
        let text = serde_json::to_string(&call).unwrap();
        assert!(!text.contains("parse_error"));
        assert_eq!(serde_json::from_str::<ParsedCall>(&text).unwrap(), call);
    }

    #[test]
    fn template_supports_detects_gemma4() {
        let fixture = include_str!("../../tests/fixtures/gemma4-chat-template.jinja");
        let plain = "{% for m in messages %}{{m.content}}{% endfor %}";
        assert!(GemmaToolFormat.template_supports(fixture));
        assert!(!GemmaToolFormat.template_supports(plain));
        assert!(ToolCallFormats::detect(fixture).is_some());
        assert!(ToolCallFormats::detect(plain).is_none());
    }

    #[test]
    fn detected_format_parses_through_the_trait() {
        let fixture = include_str!("../../tests/fixtures/gemma4-chat-template.jinja");
        let format = ToolCallFormats::detect(fixture).expect("gemma");
        let mut parser = format.new_parser();
        let events = parser.push(READ_FILE);
        assert_eq!(events.len(), 1);
        assert_eq!(parser.calls()[0].name, "read_file");
        assert_eq!(parser.content(), "");
    }

    #[test]
    fn plain_text_parser_keeps_everything_as_content() {
        let mut parser = PlainTextParser::new();
        assert!(parser.push("hi ").is_empty());
        assert!(parser.push(READ_FILE).is_empty());
        assert!(parser.push("<|tool_response>x").is_empty());
        assert!(parser.finish().is_empty());
        assert!(parser.calls().is_empty());
        assert_eq!(parser.content(), format!("hi {READ_FILE}<|tool_response>x"));
    }

    struct CounterFeed;

    impl CounterFeed {
        const MODEL_TURN: &'static str = "<|turn>model\n";

        fn run(prompt: &str, chunks: &[(&str, usize)]) -> usize {
            let mut counter = GemmaThinkCounter::new(prompt);
            for (text, tokens) in chunks {
                counter.push(text, *tokens);
            }
            counter.think_tokens()
        }
    }

    #[test]
    fn think_counter_counts_inside_thought_channel() {
        let chunks = [
            ("<|channel>thought\n", 1),
            ("let me", 2),
            ("<channel|>", 1),
            ("Answer.", 2),
        ];
        assert_eq!(CounterFeed::run(CounterFeed::MODEL_TURN, &chunks), 3);
    }

    #[test]
    fn think_counter_starts_inside_when_prompt_ends_with_opener() {
        let prompt = "<|turn>model\n<|channel>thought\n";
        assert_eq!(
            CounterFeed::run(prompt, &[("hmm", 1), ("<channel|>Hi", 2)]),
            3
        );
    }

    #[test]
    fn marker_split_across_chunks_is_seen() {
        let chunks = [
            ("<|channel>thought\n", 1),
            ("<chan", 1),
            ("nel|>", 1),
            ("after", 5),
        ];
        assert_eq!(CounterFeed::run(CounterFeed::MODEL_TURN, &chunks), 2);
        let split_open = [("<|chan", 1), ("nel>thought\nx", 1), ("y", 4)];
        assert_eq!(CounterFeed::run(CounterFeed::MODEL_TURN, &split_open), 4);
    }

    #[test]
    fn answer_only_step_has_zero_think() {
        let chunks = [("Hello", 1), (" world", 2)];
        assert_eq!(CounterFeed::run(CounterFeed::MODEL_TURN, &chunks), 0);
    }

    #[test]
    fn no_think_counter_is_always_zero() {
        let mut counter = NoThinkCounter;
        counter.push("<|channel>thought\nx", 3);
        assert_eq!(counter.think_tokens(), 0);
    }

    #[test]
    fn for_prompt_picks_gemma_counter_for_gemma_syntax() {
        let mut counter = ThinkCounters::for_prompt("<|turn>model\n<|channel>thought\n");
        counter.push("hmm", 4);
        assert_eq!(counter.think_tokens(), 4);
    }

    #[test]
    fn for_prompt_picks_no_counter_for_other_syntax() {
        let mut counter = ThinkCounters::for_prompt("<start>assistant\n");
        counter.push("<|channel>thought\nhmm", 4);
        counter.push("more", 4);
        assert_eq!(counter.think_tokens(), 0);
    }

    #[test]
    fn gemma_format_owns_its_counter() {
        let mut counter = GemmaToolFormat.think_counter("<|channel>thought\n");
        counter.push("x", 2);
        assert_eq!(counter.think_tokens(), 2);
    }
}
