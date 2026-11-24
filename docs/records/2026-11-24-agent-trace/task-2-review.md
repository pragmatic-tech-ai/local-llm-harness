### Spec Compliance
- ✅ Attribution rule: tool_format.rs `GemmaThinkCounter::push` adds `tokens` to `think` using the state at chunk start, then flips state on markers. Matches spec "Thinking tokens" (a straddling chunk counts on the side it started on).
- ✅ Prompt-tail opener: `GemmaThinkCounter::new` calls `OpenChannel::at_tail(prompt)` and compares `trim_end()` to `<|channel>thought`. Reuse is real; there is no duplicate tail parser. A non-thought opener (e.g. `<|channel>final`) starts outside.
- ✅ Seam: `ToolCallFormat::think_counter` is on the trait, `GemmaToolFormat` returns the Gemma counter, `NoThinkCounter` is always 0, and R1 `ThinkCounters::for_prompt` exists. `impl ToolCallFormat` has no other implementor in backend/src, so the trait addition breaks nothing.
- ✅ Split markers: CARRY_LEN = 16 = len("<|channel>thought") - 1 (17 - 1), which also covers the 10-byte closer. The carry is trimmed to a char boundary, so multibyte text is safe. A match is applied only if `at + marker.len() > carried`, i.e. it ends in the new text. A marker wholly inside the carry (possible only for the closer) is skipped, so it is never applied twice. A 16-byte carry cannot hold a whole opener, so the opener cannot double-apply. Markers are sorted by position before applying, so the final state is correct for chunks holding several markers. I found no false positive or double-count path. The only false positive is text that really contains the marker, which cannot be told apart from a marker.
- ✅ Spec ceiling: the brief says "think 3" for the first test. The rule gives 0 (opener chunk, started outside) + 2 ("let me") + 1 (closer chunk, started inside) = 3. The brief's prose (opener chunk + "let me" = 1 + 2) also gives 3, so the number is right and only the explanation is wrong. The spec is explicit, and the implementer was right to follow the rule. The other brief tests also pass by the rule.
- ✅ http: `completion_body` is a private, behavior-preserving extraction (same keys, `complete()` calls it). It is the minimal way to assert `n_probs > 0` without a live server, so the extraction is justified. The test also asserts the 2-entry `completion_probabilities` parse gives `tokens.len() == 2`.
- ⚠️ Cannot verify here: that `SseParser::parse_payload` yields 2 tokens for the test payload, and the hive test runs. The report claims 378/386 pass.

### Strengths
- Marker handling is dedup-safe and char-boundary-safe, and the split-marker test covers both the closer and the opener.
- Reuses `OpenChannel::at_tail`; helpers are associated functions (`CounterFeed`, `ThinkCounters`) with no free functions; constants are hoisted.

### Issues
#### Critical
None.
#### Important
None.
#### Minor
1. tool_format.rs `ThinkCounters::for_prompt` (about line 130): detection is `prompt.contains("<|turn>") || contains("<|channel>")`, anywhere in the prompt. A non-Gemma prompt whose user or tool text merely contains `<|turn>` or `<|channel>` gets the Gemma counter. Consequence: if that model's output also contains the markers, think_tokens will be nonzero when it should be 0. The initial state is safe, because `at_tail` looks only at the very end, so a user-message marker cannot start the counter inside. The `<|channel>` clause is redundant, since every Gemma 4 prompt carries `<|turn>`. Fix: anchor detection, e.g. `prompt.trim_end().ends_with("<|turn>model")` or the Gemma generation-prompt tail, or at least drop the `<|channel>` clause. Add a test where a non-Gemma prompt embeds `<|turn>` in user text.
2. tool_format.rs `GemmaToolFormat::CHANNEL_OPEN = "<|channel>"` and `GemmaThinkCounter::THINK_OPEN`/`CHANNEL_CLOSE` duplicate the literal that `OpenChannel::OPEN` (template.rs:76, private) already owns. Not wrong, but the channel syntax now has three homes. Fix: make `OpenChannel::OPEN` `pub(crate)` and build THINK_OPEN from it, or drop CHANNEL_OPEN (see 1).
3. tool_format.rs push: `format!` allocates a new window string on every chunk, and `.map_or(false, ..)` is the form clippy flags (`is_some_and`). Also `CARRY_LEN` is declared after `new`, away from the other consts. Fix: tidy; allocation per chunk is acceptable.
4. Tests: no test that a closer wholly in the carry is not re-applied (e.g. `"<channel|>"` then `"x<|channel>thought"` then more text), no multibyte-carry test, and no test of a chunk holding several markers in sequence. The implementer skipped the RED pass (report), so the tests were never seen failing. Fix: add the three tests.

### Assessment
**Task quality:** Approved
**Reasoning:** The counter logic, carry sizing, and `at_tail` reuse are correct, and both implementer concerns are right: follow the rule, and the `completion_body` extraction is a fair testability change. Only Minor polish remains. The `for_prompt` heuristic is loose but low-impact.
