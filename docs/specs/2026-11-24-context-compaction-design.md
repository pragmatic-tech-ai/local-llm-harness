# Context compaction

**Status:** approved 2026-11-24 (owner "go"); implemented on feat/context-compaction
**Roadmap:** step 3, memory and context (`docs/roadmap.md`)
**Builds on:**
- `docs/specs/2026-11-23-agent-templates-sessions-design.md`: templates, sessions and the runtime on :8779
- `docs/specs/2026-11-24-agent-trace-design.md`: the per-step trace

## Intent

The hive's model has an 8K window. Every loop step sends the whole session transcript to the
worker: the system prompt, every earlier turn, and every tool call and result. Once that
crosses the window, the worker refuses the step and the turn ends `window_full`. Tool results
make up most of the growth; one `read_file` of a 4 KB file is about 1,000 tokens. Longer tasks,
planning and multiple agents (roadmap steps 4 to 6) all need a turn to keep going past this
point.

The first half of roadmap step 3 is already done. Conversations survive reconnects, supervisor
restarts and model swaps, because sessions persist their transcript on the hive. This spec
covers the second half: what happens when the window fills.

## Goals

- **Two strategies, chosen per template.** Templates choose a context strategy: `off` (today's
  behaviour), `trim` (mechanical) or `summarize` (summaries written by the model). They also set
  a threshold as a percentage of the window.
- **Only the model's copy changes.** Compaction changes only what is sent to the model. The
  stored transcript and the chat the user sees stay complete.
- **Every compaction is traced.** It is visible in the trace on the step it affected: its
  strategy, its token counts before and after, and what it did.
- **The same seam serves both.** Both strategies sit behind one seam, so a later strategy can be
  added without touching the loop.

## Non-goals

- No change to the stored transcript and no deleting of history.
- No memory carried between sessions, and no long-term knowledge store.
- No tuning of the summary prompt per model. There is one prompt, and the trace shows how well
  it works.
- No compaction of the current turn's own user message or the system prompt.

## Anti-goals

- A compaction that silently changes what the user sees in the chat.
- A summarize strategy that leaves the turn stuck when the summary call fails. It must fall back.
- Compaction that runs when the prompt is under the threshold. Off means off, and under the
  threshold nothing is touched.

## Approaches considered

- **Mechanical trimming.** It replaces old tool results with stubs, then drops the oldest turns.
  It is instant, predictable and easy to test, but the model loses the details it elided.
- **Summaries by the model.** It replaces the older conversation with a summary the model
  writes. More meaning survives, but each compaction costs an extra generation (10 to 30 s on the
  12B), and quality on a small model is uncertain.
- **Both, as a template option.** **Chosen by the owner.** Templates pick `off`, `trim` or
  `summarize`, and the trace shows how each one behaves on the hive.

## Design

### Template fields

`AgentTemplate` gains two fields. Both are optional in TOML, so existing templates keep
working.

| Field | Type | Default | Rule |
|---|---|---|---|
| `context_strategy` | `"off"` \| `"trim"` \| `"summarize"` | `"off"` | any other value gives the validation error `unknown context strategy: {v}` |
| `compact_at_pct` | integer | `75` | 10 to 95 inclusive; otherwise `compact_at_pct must be between 10 and 95` |

Sessions snapshot both fields with the rest of the template, as they do today. The seeded
`Examples/Default` template keeps `off`.

### When compaction runs

Before each loop step, the runtime measures the prompt it is about to send:

- **The measure call.** It uses a new worker method, `measure_prompt {messages, tools,
  enable_thinking}` → `{prompt_tokens, n_ctx}`. The method renders the prompt exactly as
  `generate` would, then tokenizes it. It does no generation and does not touch the KV cache.
- **The budget.** The budget is `floor(n_ctx × compact_at_pct / 100)`. The reservation is
  `max(max_tokens, 0)`, the same as `ContextFill::check`.
- **When nothing happens.** If `prompt_tokens + reservation ≤ budget`, or the strategy is `off`,
  the step is sent unchanged.
- **Otherwise.** The strategy builds a compacted message list, measures it again, and sends it.
  If even the compacted prompt plus the reservation does not fit in `n_ctx`, the worker's normal
  `window_full` rejection ends the turn, as it does today.

"Turn" below means a user message and every message after it up to the next user message. The
**current turn** is the one being run, and it is never compacted. The system message, if
present, is never compacted.

### Strategy: trim

Trim stores nothing. It is worked out fresh on every step from the full transcript.

1. Tool results in earlier turns are replaced, oldest first, with a stub:
   `[{tool} result elided: {bytes} bytes]`. The tool-call message that requested the result
   stays, so the call and result still pair up. This repeats until the prompt fits the budget.
2. If every earlier tool result is already elided and the prompt still does not fit, the oldest
   earlier turns are dropped whole, oldest first, until it fits or only the current turn is
   left.
3. If the current turn's own tool results alone overflow, trim cannot help, and the step goes to
   the worker as it is.

### Strategy: summarize

Summarize keeps the most recent part verbatim and replaces the older part with a summary.

- **What is kept.** The **kept tail** is the current turn plus the one turn before it. Everything
  between the system message and the kept tail is the **summarised span**.
- **The summary call.** It runs on the same worker link, as a separate `generate` with tools off
  and thinking off. The messages are a fixed system prompt (see below) and one user message
  holding the summarised span, rendered as plain text, one line per message
  (`{role}: {content}`, with tool results truncated to 2,000 characters each). Reasoning
  channels (`<|channel>…<channel|>`, and an opener left unclosed at the end) are dropped from
  that text, as the chat template drops them from a normal prompt; the stored transcript keeps
  them. `max_tokens` is `min(1024, n_ctx / 8)`.
- **What the model sees.** The summary note `[Summary of the earlier conversation]\n{summary}`
  rides in the system message: it is appended to the session's system prompt after a blank
  line, or, when the session has no system prompt, it is the whole system message. The kept
  tail follows. The note is never a separate user message, so the turns after the system
  message still alternate user/assistant, which strict chat templates such as Gemma 3's
  require. If the prompt still exceeds the budget, trim runs on the result. Trim never alters
  the system message, so the note is never elided. If measuring a summarised prompt fails
  (a template rejecting it, say), the step falls back to trim with the error in
  `summary_error`.
- **Persistence.** The summary is saved on the session as
  `compaction: {summary, covers, tokens, at}`.
  - `covers` is a transcript index: the summary replaces the messages from after the system
    message up to, not including, `covers` (the start of the turn before the current one when
    the summary was made). The trace's `summarized_messages` is that count.
  - `tokens` is the note text's own size, measured as the summary call renders it.
  - On later steps and turns, the saved summary is reused while it still covers a prefix of the
    transcript and the prompt fits.
  - When the window fills again, a new summary is made over the old summary text plus the
    messages since `covers`, and the saved state is replaced.
  - `reset` clears `compaction`. Session JSON stays at schema 1, because the new field is
    optional.
- **Fallback.** If the summary call fails (an error, a rejection, an empty reply, or a reply
  longer than its `max_tokens`), that step uses trim instead. The saved summary is left as it
  was, and the trace records the fallback. A summary that was made and saved, but whose prompt
  then fails to measure (for example a template rejecting it), stays saved because it is valid
  in itself. That step and later steps that reuse it fall back to trim with `summary_error`
  set, and make no new summary call.
- **The summary system prompt** (fixed copy):
  `Summarize the conversation below for your own later use. Keep facts, file names, numbers,
  decisions and open questions. Write plain sentences, no preamble.`

### Trace

`StepRecord` gains an optional `compaction` field, present only on steps where compaction ran.
The record version stays `v: 1`, because readers already skip unknown fields.

```jsonc
"compaction": {
  "strategy": "trim",          // trim | summarize
  "before_tokens": 7420,       // measured prompt before compaction
  "after_tokens": 5980,        // measured prompt actually sent
  "elided_results": 3,         // trim: tool results replaced with stubs
  "dropped_turns": 0,          // trim: whole turns dropped
  "summarized_messages": 0,    // summarize: messages covered by the summary in use
  "summary_ms": null,          // summarize: time of the summary call, null when reused
  "reused_summary": false,     // summarize: a saved summary was reused
  "fallback": false,           // summarize failed and trim was used
  "summary_prompt_tokens": null, "summary_out_tokens": null, "summary_error": null
}
```

The app's trace step row gains one line under the stats when `compaction` is present:

```
   ⟲ trim: 7 420 → 5 980 tokens · 3 results elided · 0 turns dropped
   ⟲ summarize: 7 420 → 3 100 tokens · covers 14 messages · 18.2 s
   ⟲ summarize (reused): 6 900 → 3 300 tokens · covers 14 messages
   ⟲ summarize failed → trim: 7 420 → 5 980 tokens · 3 results elided · 0 turns dropped
```

When trim ran on top of a summary, the summarize and reused lines end with
` · {e} results elided · {d} turns dropped`, only when either count is above 0.

### App

- **The template editor.** It gains a "Context" combo (Off, Trim, Summarize) and a
  "Compact at %" number field. The field validates 10 to 95, and a parse failure blocks Save,
  like the other number fields.
- **`agents.ts` and `RuntimeClient`.** `AgentTemplate` gains `context_strategy` and
  `compact_at_pct`, and `StepRecord` gains the optional `compaction`. No new methods or
  notifications are needed.
- **The chat view.** It does not change.

### Settled details (builder check)

These are the author's calls. The owner can overrule any of them.

- **A span too big for one summary call.** The summary call's input budget is
  `n_ctx − summary max_tokens − the render overhead of the summary prompt`. If the summarised
  span does not fit, it is summarised in chunks, oldest first:
  - The first chunk is as many whole messages as fit.
  - Each following call gets the running summary plus the next chunk, until the span is
    covered.
  - At most 4 calls are made. If the span still is not covered, the step falls back to trim.
  - `summary_ms` is the total time of all the calls.
- **What a reused summary covers.** The prompt is the system message, then the summary note,
  then every transcript message after `covers`, verbatim. There is no gap. Between
  re-summaries the verbatim part grows. When it no longer fits the budget, a new summary is
  made over the old summary text plus the messages from `covers` up to the kept tail. The kept
  tail (the current turn plus the one before it) is only the minimum verbatim part at the
  moment of summarising.
- **When the summary is saved.** The summarised span always lies inside the **committed**
  transcript, because it ends before the previous turn and the current turn is never part of
  it. So the summary is saved straight away, through a handle the loop gets for this purpose,
  whatever the turn's outcome. A rollback never rewrites the committed transcript; it only
  drops the current turn's scratch. `covers` therefore stays valid. `reset` clears the saved
  summary.
- **The summary call in the trace.** The `compaction` field also carries
  `summary_prompt_tokens`, `summary_out_tokens` and `summary_error` (text or null), summed
  over any chunked calls. A failed summary is visible with its reason, not just as a
  `fallback` flag.
- **Summary sampling.** It is fully fixed: temperature 0.2, top_p 0.9, top_k 40,
  repeat_penalty 1.0, the session's seed, the summary `max_tokens`, and thinking off. Nothing
  else is taken from the session. This keeps summaries comparable across runs, whatever the
  session's sampling is.
- **Measuring the summary call.** Summary calls are measured on their own, with no tools and
  thinking off, as they are sent: for the input budget, for chunk planning and for the saved
  `tokens`. The step's own measure (its tools and thinking flag) is used only for the step's
  prompts. Chunk ends are found by a binary search over the chunk's length, measuring the
  assembled call each time, so a long span costs about log2 of its line count measures per
  chunk, not one per message.
- **A reservation that reaches the budget.** When `max(max_tokens, 0)` alone is at least the
  budget, no prompt could fit it, so the target becomes the window: compaction runs only when
  `prompt_tokens + reservation > n_ctx`, and then removes only what is needed to fit `n_ctx`.
  When the reservation is at least `n_ctx`, nothing is compacted and the worker's own check
  decides.
- **The stub's byte count.** `{bytes}` is the length of the tool result's serialized JSON,
  the same measure as `bytes` in the trace's tool timings.

## Constraints

- No new crates and no npm install.
- Rust builds and tests run only on the hive, through `scripts/hive-test.sh`.
- House style:
  - Rust: idiomatic snake_case, rustfmt, helpers as associated functions, strings as consts.
  - TypeScript: OOP, Allman braces, no semicolons, PascalCase public members, literals as
    `private static readonly`, VMs extend Observable, and `.mu` labels stay inline.
- The stub, the summary note header and the summary system prompt are fixed copy, held as
  consts.
- Compaction never holds the session store's lock or the hub's lock across a worker call.
- Saving the summary uses the session store's existing atomic write.

## Left to the builder

- **How to reach the budget.** Trim may re-measure after each elision, or estimate each
  message's tokens with one `measure_prompt` per message and confirm with a final measure. The
  prompt it sends must always have been measured.
- **Where the strategies live.** A `runtime/compaction.rs` with a `CompactionStrategy` trait
  (`Off`, `Trim`, `Summarize`) is the suggested shape.
- **How the loop calls it.** The loop should call it through one method per step, keeping the
  loop's existing tests unchanged.
