### Task 2: Thinking-token counter and per-engine token counts

**Files:**
- Modify: `backend/src/engine/tool_format.rs`. The counter sits next to the format seam that already knows the Gemma channel markers.
- Modify: `backend/src/engine/http.rs` tests

**Interfaces:**
- Produces:
  - `pub trait ThinkCounter: Send { fn push(&mut self, text: &str, tokens: usize); fn think_tokens(&self) -> usize; }`
  - `pub struct GemmaThinkCounter`, built with `GemmaThinkCounter::new(prompt: &str)`. It starts inside a thinking channel when `OpenChannel::at_tail(prompt)` is a thinking opener, and when the opener is `<|channel>thought`.
  - `pub struct NoThinkCounter`, which always returns 0.
  - The existing format type gains `fn think_counter(&self, prompt: &str) -> Box<dyn ThinkCounter>`. Gemma 4 returns the Gemma counter; formats without channels return `NoThinkCounter`.
  - Rule: tokens are counted as thinking while the counter is inside a thinking channel. A chunk is attributed to the state the counter was in **at the start of the chunk**. The state then flips on each opener (`<|channel>thought`) or closer (`<channel|>`) found in the chunk text. The counter keeps a short carry of the previous chunk's tail, so a marker split across chunks is still seen.

- [ ] **Step 1: Write the failing tests** in `tool_format.rs`:
  - `think_counter_counts_inside_thought_channel`: chunks `"<|channel>thought\n"`(1), `"let me"`(2), `"<channel|>"`(1), `"Answer."`(2). This gives think 3 (the opener chunk plus "let me") and total 6.
  - `think_counter_starts_inside_when_prompt_ends_with_opener`: a prompt ending `"<|channel>thought\n"`, then chunks `"hmm"`(1), `"<channel|>Hi"`(2). This gives think 3, because the closer chunk started inside.
  - `marker_split_across_chunks_is_seen`: chunks `"<chan"`, `"nel|>"` after an opener close the channel.
  - `answer_only_step_has_zero_think`: no markers gives 0.
  - `no_think_counter_is_always_zero`.
- [ ] **Step 2:** Add the test `http_chunk_has_one_token_info_per_generated_token` in `engine/http.rs` tests. It parses an SSE payload with `completion_probabilities` of 2 entries and asserts `tokens.len() == 2`. It also asserts that the request body built for `complete` has `n_probs` > 0. The in-process engines (llamacpp, candle) push exactly one `TokenInfo` per sampled token (`llamacpp.rs:286`, `candle.rs:290`). Their tests need a real model, so Task 7's live proof checks them: `out_tokens` > 0 on both runtimes.
- [ ] **Step 3:** Run `scripts/hive-test.sh -- tool_format http`. Expected: FAIL.
- [ ] **Step 4: Implement**, then run `scripts/hive-test.sh` (whole lib) and `scripts/hive-test.sh --features llamacpp` (the counter is in a shared module). Expected: PASS. Then run `--restore`.
- [ ] **Step 5: Commit** `feat(engine): thinking-token counter behind the tool-format seam`.

