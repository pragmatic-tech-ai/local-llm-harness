//! Gemma 4 tool-call syntax: value grammar, single-call parsing and a streaming parser.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

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

/// Stateless helpers for the Gemma 4 tool-call syntax.
pub struct GemmaToolFormat;

impl GemmaToolFormat {
    const CALL_OPEN: &'static str = "<|tool_call>";
    const CALL_CLOSE: &'static str = "<tool_call|>";
    const RESPONSE_OPEN: &'static str = "<|tool_response>";
    const QUOTE: &'static str = "<|\"|>";
    const CALL_PREFIX: &'static str = "call:";
    const MAX_DEPTH: usize = 64;

    /// True iff the chat template mentions both `tools` and the call marker.
    pub fn template_supports(src: &str) -> bool {
        src.contains("tools") && src.contains(Self::CALL_OPEN)
    }

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
    finished: bool,
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
            finished: false,
            closed: false,
        }
    }

    /// Text outside call spans, excluding held-back bytes; nothing at or after the response marker.
    pub fn content(&self) -> String {
        self.content.clone()
    }

    pub fn calls(&self) -> Vec<ParsedCall> {
        self.calls.clone()
    }

    /// True once `<|tool_response>` has been seen.
    pub fn finished(&self) -> bool {
        self.finished
    }

    pub fn push(&mut self, text: &str) -> Vec<ParseEvent> {
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
    pub fn finish(&mut self) -> Vec<ParseEvent> {
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
        self.finished = true;
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
            whole.push(text);
            whole.finish();
            let mut slow = GemmaToolParser::new();
            Self::chars(&mut slow, text);
            slow.finish();
            assert_eq!(slow.calls(), whole.calls(), "calls differ for {:?}", text);
            assert_eq!(
                slow.content(),
                whole.content(),
                "content differs for {:?}",
                text
            );
            assert_eq!(
                slow.finished(),
                whole.finished(),
                "finished differs for {:?}",
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
        assert!(p.finished());
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
        let p = TestFeed::both("<|tool_call>call:f{s:<|\"|>a<|tool_response>b<|\"|>}<tool_call|>");
        assert!(!p.finished());
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
        assert!(p.finished());
        assert!(p.push("more").is_empty());
        assert_eq!(p.content(), "");
        assert_eq!(p.calls().len(), 1);
    }

    #[test]
    fn text_before_response_marker_is_content() {
        let mut p = GemmaToolParser::new();
        TestFeed::chars(&mut p, "hi<|tool_response>junk");
        assert_eq!(p.content(), "hi");
        assert!(p.finished());
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
        assert!(GemmaToolFormat::template_supports(fixture));
        assert!(!GemmaToolFormat::template_supports(
            "{% for m in messages %}{{m.content}}{% endfor %}"
        ));
    }
}
