# SDD ledger — plan: docs/plans/2026-11-24-context-compaction.md
Spec: docs/specs/2026-11-24-context-compaction-design.md (owner "go"). Branch feat/context-compaction from main @1d30774b. Execution: subagent-driven (owner "go").

## Pre-flight scan
| pair / task | produces vs consumes | finding |
|---|---|---|
| T1→T5 | ContextStrategy, compact_at_pct, CompactionState, set_compaction → TurnConfig, Compactor, sink | ok |
| T2→T3 | PromptSize (worker.rs) used by PromptMeasure | T3 Consumes omits T2 → ruling R1 |
| T2→T4/T5 | WorkerLink::generate/measure_prompt | ok |
| T3→T4 | Trim, Turns, Compacted, CompactionRecord | ok |
| T4→T5 | Summarize::compact, SummarySink | ok |
| T3/T5→T6 | CompactionRecord JSON → TS | ok, field-for-field |
| T1 self | validation messages vs serde unknown variant | parse error text must contain the exact message → implementer may use custom deserialize |
| T5 self | ToolLoop::run gains &mut Compactor; existing tests use Compactor::off() | ok |
| T7 self | push needs owner go | stop point |

Ruling R1: T3 consumes T2's `PromptSize {prompt_tokens, n_ctx}` from runtime/worker.rs — cost: none
Ruling R2: an unknown context_strategy must surface as `unknown context strategy: {v}` even from TOML parse (custom Deserialize or validated string) — cost: none

Task 1: dispatching (BASE 1d30774b)
Task 1: dispatched (implementer a5cc03ab288b76326, sonnet)
Task 1: DONE_WITH_CONCERNS 466cc733 (no RED; no save_template RPC test for R2)
Task 1: review Needs fixes (3 Important: free test fn, rustfmt, duplicated names; 4 minor) → round 1. Ruling: out-of-range compact_at_pct (300, -1) gives the range message, not a serde type error — cost: none
Task 1: fix round 1 7028a9ed (controller spot-check)
Task 1: complete (commits 1d30774..7028a9e, review clean after 1 fix round)
Task 2: dispatched (BASE 7028a9ed, implementer a53c805887ee31594, sonnet)
Task 2: DONE_WITH_CONCERNS 9ab41360 (no RED; set_measure allow(dead_code) until T5)
Task 2: review APPROVED (0/0, 6 minor) → round 1. Ruling: keep set_measure + allow until T5 uses it — cost: none
Task 2: fix round 1 5d953322 (test + params share; controller spot-check)
Task 2: complete (commits 7028a9e..5d95332, review clean after 1 fix round)
Task 3: dispatched (BASE 5d953322, implementer a055456bd34f19a54, opus)
Task 3: DONE_WITH_CONCERNS ff5ab907. Ruling: over-budget-after-all-drops sends the most-trimmed measured prompt (it may still fit n_ctx) — cost: none; re-measure per edit accepted (spec allows)
Task 3: review Needs fixes (Important: duplicated byte measure; 3 minor) → round 1. Ruling: Trim always returns a record (only called over budget); docs fixed — cost: none
Task 3: fix round 1 3059b060 (controller spot-check)
Task 3: complete (commits 5d95332..3059b06, review clean after 1 fix round)
Task 4: dispatched (BASE 3059b060, implementer a8470cba7df839cd6, opus)
Task 4: DONE_WITH_CONCERNS 327724ae. Ruling: Summarize/Compactor take &SamplingConfig instead of seed — cost: none; covers = transcript index, summarized_messages = count replaced (app shows summarized_messages); max-tokens hit detected by token count, not stop flag
Task 4: review APPROVED (0/0, 5 minor) → round 1. Ruling: summary text from turn_done.content (worker's final text, matches ToolLoop) — cost: none
Task 4: fix round 1 821514cc (controller spot-check)
Task 4: complete (commits 3059b06..821514c, review clean after 1 fix round)
Task 5: dispatched (BASE 821514cc, implementer a028a9a7fcc2eaa18, opus). Ruling: Compactor::new takes SamplingConfig instead of seed — cost: none
Task 5: DONE 6d5c4fe8 (TurnConfig context fields unread by loop; failed measure silent)
Task 5: review APPROVED (0/0, 4 minor) → round 1. Ruling: first measure failure per turn logged to stderr (no trace record, per spec) — cost: none
Task 5: fix round 1 aceccf62 (controller spot-check)
Task 5: complete (commits 821514c..aceccf6, review clean after 1 fix round)
Task 6: dispatched (BASE aceccf62, implementer ac1dac8f43549cf1c, sonnet)
Task 6: DONE d35036d5 (no RED)
Task 6: review APPROVED (0/0, 5 minor) → round 1
Task 6: fix round 1 0c7a53fb. Ruling: trim record with fallback:true prints the plain trim line; reused with null summary_ms prints reused line — cost: none
Task 6: complete (commits aceccf6..0c7a53f, review clean after 1 fix round)
FINAL REVIEW: dispatched on 1d30774b9f963aa82859f8578b7b5ffe034cfdb3..0c7a53fb
FINAL REVIEW: with fixes (I1 summary note as user msg breaks Gemma 3 alternation; I2 summary measured with step tools/thinking; I3 thinking blocks in span text; 9 minor). Fix wave a6bf643c8162b1f6d (opus).
Ruling: summary note lives in the system message (appended, or a new system message) — supersedes spec's "one user message" — cost: none
Ruling: reservation >= budget → compaction targets the window (n_ctx − reservation) — cost: none
Ruling: summary sampling fully fixed (top_k 40, repeat_penalty 1.0 added) — cost: none
Ruling: remove unread TurnConfig context fields (supersedes T5 keep ruling) — cost: none
Ruling: chunk planning by binary search over prefix length — cost: none
FINAL FIX WAVE: 5c0d3720, 540ccb12; spec status 1 commit; scoped re-review dispatched
FINAL RE-REVIEW: all fixed; N1 (doc) fixed by controller 432b8908. Task 7 awaits owner go.
