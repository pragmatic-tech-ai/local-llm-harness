### Task 2: Gemma 4 tool-call parser

**Files:**
- Create: `backend/src/engine/tool_format.rs` (registered in `engine/mod.rs`)

**Interfaces:**
- Produces: `ParsedCall { id: String, name: String, arguments: Value, raw: String, parse_error: Option<String> }` (Serialize, `parse_error` skipped when `None`). `ParseEvent::{Call(ParsedCall), ResponseMarker}`. `GemmaToolFormat::template_supports(src: &str) -> bool` (true iff `src` contains both `tools` and `<|tool_call>`). `GemmaToolFormat::parse_value(text: &str) -> Result<Value, String>`. `GemmaToolFormat::parse_call(body: &str) -> Result<(String, Value), String>`, where `body` is the text between the markers (`call:NAME{…}`). `GemmaToolParser::new() -> Self`, `push(&mut self, text: &str) -> Vec<ParseEvent>`, `content(&self) -> String` (text outside call spans; nothing at or after `<|tool_response>`), `calls(&self) -> Vec<ParsedCall>`, `finished(&self) -> bool` (true once the response marker has been seen).

Value grammar (whitespace allowed between tokens): string = `<|"|>`…`<|"|>` (non-greedy); `true` / `false` / `null`; number = JSON number text (an `i64` when integral, otherwise `f64`); object = `{` [key `:` value (`,` key `:` value)*] `}`, where a key is either `<|"|>…<|"|>` or bare text up to `:` (trimmed); array = `[` [value (`,` value)*] `]`. Anything else is an error. Call ids are `call_1`, `call_2`, … per parser instance.

- [ ] **Step 1: Write failing tests:**

```rust
parse_value_covers_every_kind      // `{a:1,b:2.5,c:true,d:null,e:<|"|>x<|"|>,f:[1,<|"|>y<|"|>],g:{h:false}}` == json!({"a":1,"b":2.5,"c":true,"d":null,"e":"x","f":[1,"y"],"g":{"h":false}})
string_values_keep_structural_characters // `{q:<|"|>a{b},c:d[e]"f<|"|>}` == json!({"q":"a{b},c:d[e]\"f"})
parse_call_reads_name_and_args     // "call:read_file{max_bytes:100,path:<|\"|>notes.txt<|\"|>}" == ("read_file", json!({"max_bytes":100,"path":"notes.txt"}))
parse_call_with_no_args            // "call:get_time{}" == ("get_time", json!({}))
parser_strips_call_from_content    // push("Let me check.<|tool_call>call:get_time{}<tool_call|>") → one Call{id:"call_1",name:"get_time"}; content()=="Let me check."
parser_two_calls_get_sequential_ids
parses_when_fed_one_char_at_a_time // the read_file call string above, pushed 1 char per push → same ParsedCall as pushing it whole
malformed_call_reports_parse_error // "<|tool_call>call:x{a:}<tool_call|>" → Call with parse_error Some(_), raw == "call:x{a:}"
response_marker_ends_parsing       // push("<|tool_call>call:get_time{}<tool_call|><|tool_response>response:get_time{}") → [Call, ResponseMarker]; finished(); further push returns []; content()==""
template_supports_detects_gemma4   // true for the fixture; false for "{% for m in messages %}{{m.content}}{% endfor %}"
```

- [ ] **Step 2: Run, expecting FAIL (missing module):** `scripts/hive-test.sh -- engine::tool_format`.
- [ ] **Step 3: Implement** `GemmaToolFormat` and `GemmaToolParser`. The parser buffers any trailing text that could be the start of `<|tool_call>`, `<tool_call|>` or `<|tool_response>`, and holds it back until it can decide.
- [ ] **Step 4: Run, expecting PASS.**
- [ ] **Step 5: Commit:** `feat(worker): Gemma 4 tool-call syntax parser`.

