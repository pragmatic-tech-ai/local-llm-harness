# Agent trace in the instrument panel

**Status:** draft 2026-11-24, awaiting owner approval
**Roadmap:** step 2 (`docs/roadmap.md`)
**Builds on:** `docs/specs/2026-11-23-agent-templates-sessions-design.md` (sessions, the
runtime on :8779, the dispatcher)

## Intent

The hive runs a 12B model with an 8K window. Every agentic capability after this one
(context compaction, longer tasks, planning, multiple agents) has to fit that budget.
Before building them, the owner needs to see where each turn's budget goes. The trace
records, for every loop step, how full the window was, the tokens produced, the time
spent generating, the time each tool took, and why the loop stopped.

The owner wants all three uses kept open:
- watching the trace live while a turn runs;
- scrolling back through earlier turns in a session;
- coming back to them after an app or supervisor restart.

## Goals

- The runtime records a trace for **every** turn: answered, out of budget, window full,
  error, no harness, and aborted turns. A failed turn is the one most worth studying.
- The trace is persisted per session on the hive. It is the same from any window and
  survives restarts.
- The live trace and a replayed trace are the same records, so they render identically.
- The instrument panel shows a Trace section with turns listed newest first. Each turn
  expands to show its steps, and older turns load a page at a time.

## Non-goals

- No charts across sessions, no export and no comparison view. The log format must not
  rule these out.
- No logprobs or per-token data in the trace.
- No trace output in the chat area.
- No retention limit or compaction of the log, for now.

## Anti-goals

- A trace that only exists for successful turns.
- Timing measured in the app, where it would include network time and vary by window.
- A trace that changes what the loop does. Tracing never fails a turn: if writing the
  log fails, the turn goes on and the failure is reported as a trace warning.

## Approaches considered

- **A. Trace inside the session file.** Each committed turn carries its trace in the
  session JSON. This is the least machinery, but sessions commit only on `Answered`, so
  failed turns would have no trace, and session files grow with every turn. Rejected.
- **B. A separate append-only trace log per session.** The runtime appends one record per
  step and one per turn end to `sessions/{id}.trace.jsonl` as they happen. It emits the
  same records live, and a new `get_trace` method pages back through history. **Chosen by
  the owner.**
- **C. App-side storage built from the existing notifications.** Rejected: traces would
  differ per window and machine, and the app cannot time tools that run on the hive.

## Change to the shipped spec

The agent-templates spec settled "Per-turn instrument data (logprobs, timing) is not
persisted". That changes: **timing and token counts are persisted, in the trace log**.
Logprobs are still not persisted. Update that line in the shipped spec when this one
ships.

## Design

### Where the numbers come from

The runtime's ToolLoop already relays the worker's notifications for each step, and it
runs the tools itself, so it measures everything with a monotonic clock:

| Field | Source |
|---|---|
| prompt tokens, window size | the worker's `turn_started {prompt_tokens, n_ctx}` |
| time to first token | from the step's request to the worker until the first `turn_chunk` |
| generation time | from the first `turn_chunk` until `turn_done`/`turn_error`/`turn_rejected` |
| output tokens | the sum of `turn_chunk.tokens.len()`. All three engines put one `TokenInfo` per generated token in a chunk; the http engine always requests `n_probs`, so the list is filled. A test per engine pins this. |
| thinking tokens | the chunk tokens generated while inside a thinking channel; see the next paragraph |
| tokens per second | output tokens ÷ generation time (absent when generation time is 0) |
| tool latency, success, result size | measured around each `ToolExecutor` call; size is the serialized result's byte length |
| model load time | reported by `TurnLinks::link`, which now returns the link plus `Option<Duration>` for a load it performed (`None` for a plain lookup) |

**Thinking tokens.** The relayed chunks are raw text, with the Gemma channel markers
visible. Thinking tokens are the chunk tokens counted between a thinking-channel opener
(`<|channel>thought`, or the opener the prompt ends with, `OpenChannel::at_tail`) and the
next `<channel|>`. If a chunk straddles a marker, its tokens count on the side the chunk
started on. For a model or format with no channel markers, `think_tokens` is 0. The
counter sits behind the same `ToolCallFormat`-style seam as the parsing, so other
formats can supply their own.

### The trace log

- **Location.** The log lives next to the session file, at
  `{agents.data_dir}/sessions/{id}.trace.jsonl`. It is UTF-8 with one JSON object per
  line, LF-terminated, and append-only.
- **Writing.** Each record is written with a single append plus flush. The log is not
  fsynced on every record, because losing the last record on a power cut is acceptable.
- **Reading.** On a partial last line (a crash mid-write), that line is ignored.
- **Lifecycle.**
  - Deleting a session deletes its log.
  - `reset` keeps the log and appends a `reset` record, so the history stays honest.
  - A session with no log file has an empty trace.
- **Turn numbers.** Each session has its own turn counter, starting at 1. On first use
  after startup, the next number is found by reading the log's last turn record. It is
  lazy, per session, and cached.
- **Schema.** Every record carries `"v": 1`. Readers skip unknown kinds and unknown
  fields.

#### Records

```jsonc
// one per loop step, written when the step's tools (if any) have finished
{"v":1,"kind":"step","turn":3,"step":1,"at":1795474024627,
 "prompt_tokens":3102,"n_ctx":8192,
 "out_tokens":38,"think_tokens":0,
 "load_ms":null,            // ms spent loading the model before this step, else null
 "ttft_ms":1900,"gen_ms":2600,
 "end":"tool_calls",        // tool_calls | answered | budget | rejected | error | aborted
 "tools":[{"name":"read_file","id":"call_1","ok":true,"ms":4,"bytes":26}],
 "error":null}              // the turn_error/turn_rejected text when end is error/rejected

// one per turn, written after loop_done
{"v":1,"kind":"turn","turn":3,"at":1795474046000,
 "steps":2,"reason":"answered",  // the loop_done reason, verbatim
 "total_ms":21400,"out_tokens":59,"think_tokens":0,
 "peak_fill":0.41,               // max prompt_tokens/n_ctx over the steps
 "committed":true,               // whether the transcript was saved
 "user":"What does notes.txt say?"}  // the user message, truncated to 200 chars

// on reset
{"v":1,"kind":"reset","at":1795474100000}
```

- `at` is the wall-clock time in Unix milliseconds, recorded when the record is written.
- A step that never reached the worker (a step-0 link or load failure) is written as a
  step with `prompt_tokens`/`n_ctx` null, `end: "error"` and the error text.
- A turn aborted because no connection remains attached still gets its turn record, with
  `reason: "aborted"` and `committed: false`. This is the only reason the trace adds
  beyond the `loop_done` reasons.

#### Settled details (builder check)

These are the author's calls; the owner can overrule any of them.

- **Budget cut.** The model can ask for tools on the last step the budget allows, so the
  loop stops without running them. That step is recorded with `end: "budget"` and
  `tools: []`, so it never reads as "tools ran".
- **Aborted steps.** When the sink closes mid-step, the recorder sits inside the loop and
  flushes the in-flight step with what it measured so far, with `end: "aborted"`. It then
  writes the turn record. An aborted turn shows every step it ran, and `peak_fill`
  includes the last prompt.
- **Write before notify.** A record is appended to the log first, then sent live.
  - If the append fails, `trace_warning` is sent, best effort, and the record is still
    sent live.
  - If the sink is closed, nothing more is sent, but the log is still written. The step
    and turn records of an aborted turn reach the log whatever happens to the sink.
- **Live and history overlap.** The app deduplicates by `(turn, step)`, and a live
  record replaces the paged one. No ordering guarantee is needed from the server.
- **Resets at the edges.** The server reads the whole log for `get_trace`, so
  `reset_before` is exact for the oldest turn on a page. A reset with no later turn sets
  `reset_pending: true` in the first page's response, and the app shows the divider at
  the top.
- **Turn numbers.** A turn number is allocated in memory when the turn starts. On first
  use after startup, the counter becomes the highest `turn` in any record (step or
  turn), plus 1. The number is reserved as it is handed out, so turns whose records
  fail to write never share one. A log that cannot be read when the number is
  allocated runs the turn untraced, after one `trace_warning`. A turn that crashed
  before writing any record can reuse its number after a restart, which is harmless
  because nothing of it was written.

### Protocol (runtime :8779)

- **New notifications**, sent to the connections attached to the session, alongside the
  turn's other notifications:
  - `trace_step {id, record}`, sent right after the step record is appended;
  - `trace_turn {id, record}`, sent right after the turn record is appended, before the
    `send_turn` result;
  - `trace_warning {id, message}`, sent when writing the log failed. The turn continues.
- **New method** `get_trace {id, before?: number, limit?: number}`. It needs no attach.
  - It returns `{turns: TraceTurn[], more: bool, reset_pending: bool}`, newest turn first. `reset_pending` is only ever true on the first page.
  - Each `TraceTurn` is `{turn: number, steps: StepRecord[], summary: TurnRecord | null,
    reset_before: bool}`. `summary` is null for a turn still in progress or cut off by a
    crash.
  - `reset_before` is true when a `reset` record comes between this turn and the
    previous one.
  - `before` is an exclusive turn number. `limit` defaults to 20 and is capped at 100.
  - An unknown session gives `unknown session: {id}`.
- **`attach_session` is unchanged.** The app calls `get_trace` after attaching.

### App

- **RuntimeClient:**
  - `GetTrace(id, before?, limit?)`;
  - the signals `OnTraceStep`, `OnTraceTurn` and `OnTraceWarning`;
  - types in `agents.ts`: `StepRecord`, `TurnRecord` and `TraceTurn`.
- **`TraceViewModel`**, owned by the session view, next to the instrument panel:
  - `Turns: ObservableCollection<TraceTurnViewModel>`, newest first;
  - `LoadOlderCommand`, enabled while `more` is true;
  - a warning line for `trace_warning`.
  - On attach it loads the first page. Live records update the current turn's row in
    place: a turn row appears at its first `trace_step`, and its summary fills in on
    `trace_turn`. Switching sessions clears and reloads it. On reconnect it reloads the
    first page.
- **`TraceTurnViewModel`:**
  - `Expanded` (default: the newest turn is expanded, older ones are collapsed);
  - a header text: `Turn {n} · {reason} · {steps} steps · {total} s · peak {pct}%`, or
    `Turn {n} · running…` while it is in progress;
  - `Steps`;
  - a reset divider when `reset_before` is true.
- **Step row:**
  - `{step}  ctx {used}/{total} ({pct}%)`;
  - a small fill bar, reusing the context-bar style;
  - `out {n} (think {k})`, where `(think k)` appears only when k > 0;
  - `load {s} s` when present;
  - `ttft {s} s  gen {s} s  {r} t/s`;
  - then one sub-row per tool: `└ {name}  ok|failed  {ms} ms  {bytes} B`.
  - A step that ended in `rejected` or `error` shows its text in the warning colour.
- **Placement.** The trace goes in the instrument panel, below the existing context bar,
  and scrolls on its own.

```
Trace
▾ Turn 3 · answered · 2 steps · 21.4 s · peak 41%
    1  ctx 3 102/8 192 (38%)  ▇▇▇▇░░░░░░  out 38  ttft 1.9 s  gen 2.6 s  14.6 t/s
       └ read_file  ok  4 ms  26 B
    2  ctx 3 360/8 192 (41%)  ▇▇▇▇░░░░░░  out 21  ttft 1.2 s  gen 1.5 s  14.0 t/s
▸ Turn 2 · budget · 8 steps · 94.0 s · peak 88%
── reset ──
▸ Turn 1 · window_full · 3 steps · 30.2 s · peak 100%
  Load older…
```

## Constraints

- No new crates (the build is vendored and offline), and no npm install.
- Rust builds and tests run only on the hive, via `scripts/hive-test.sh`.
- House style:
  - Rust: idiomatic snake_case, rustfmt, helpers as associated functions, strings as
    consts.
  - TypeScript: OOP, Allman braces, no semicolons, PascalCase public members, literals as
    `private static readonly`, VMs extend Observable, and `.mu` labels stay inline.
- Timing uses `std::time::Instant`. Only `at` uses the wall clock.
- Writing the trace must not hold the session store's lock or the hub's lock.

## Left to the builder

- The Rust module layout. A `runtime/trace.rs` with a `TraceLog` per session, and a
  `TraceRecorder` the ToolLoop drives through `TurnLinks` or its sink, is the suggested
  shape.
- How the recorder hooks into the loop, as long as the loop's own tests and behaviour
  are unchanged.
- The exact panel layout, within the sketch above.
