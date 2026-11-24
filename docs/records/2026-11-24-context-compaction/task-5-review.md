### Spec Compliance (✅ / ❌ with file:line; ⚠️ cannot verify)

- ✅ Trigger rule. `Compactor::prepare` (diff 134-151): `Off` returns early without measuring. budget = `n_ctx * pct / 100`, which is floor in integer division. reservation = `max(max_tokens, 0)`. `Trim::fits` is `prompt + reservation <= budget` (compaction.rs:134), so compaction runs only on a strict `>`.
- ✅ A failed measure means the step is sent uncompacted with no record (diff 143-145). A strategy `Err` takes the same path (diff 191). Test `measure_failure_sends_uncompacted_and_continues` covers 2 steps.
- ✅ Only the sent copy changes. tool_loop.rs `run` (diff 1089-1102) sends `compacted.messages`. `scratch` is never written by the compactor, and the tool results are appended to `scratch` as before. The trim test (diff 1508-1509) and the summarize test (diff 1627-1628) both assert that the committed transcript is whole, and the server test checks the stored transcript (diff 685-686).
- ✅ `committed_len` / `current_turn_start` on every step. The server passes `rec.transcript.len()` (diff 411), and `run` receives `&rec.transcript` (diff 469), so both values are the length of the same slice. The loop fixes `current_turn_start = scratch.len()` before the user push (diff 1067), and it stays constant while `scratch` grows over later steps. This matches Task 4's reuse window `covers <= min(committed_len, current_turn_start)`.
- ✅ Mid-turn reuse. `KeepingSink` copies the state only after the inner save succeeds (diff 239-243). `self.saved` is replaced after each Summarize call (diff 185-187), so step 2 passes the new state. Test `summary_saved_mid_turn_is_reused_by_later_steps` asserts one summary call, one save, step 1 not reused and step 2 `reused_summary`.
- ✅ Reset. `SessionStore::reset` clears `compaction` (session_store.rs:225-229), and reset is refused while busy (server.rs:678-685, test `reset_and_turn_exclude_each_other`), so a running turn cannot save after a reset. The next turn reads `rec.compaction` fresh at `send_turn`. `reset_clears_saved_summary` checks memory, disk and a fresh span from message 0 with "OLD" absent.
- ✅ Template edit. `SessionView::turn_config` reads `rec.snapshot` (diff 389-398). The test changes the template to Off after create, and the turn still records trim.
- ✅ Deleted session. Delete is allowed mid-turn (server.rs:927-936). `SessionCompactionSink::save` checks `contains` first, then on a `set_compaction` error checks `contains` again (diff 353-363). `mutate` and `delete` both run under the store map lock (session_store.rs:191-203, 243-254), so a late save cannot recreate a deleted file. Covered by `compaction_sink_leaves_a_deleted_session_alone`.
- ✅ The trace record lands on the right step. `meter.compaction` is set on the meter of the step being prepared (diff 1099), after `steps += 1` and before `generate`. It flows through `finish` and also through `failed_step` when `generate` fails. The loop test asserts records[0] and records[1] separately.
- ✅ No hub or store lock is held across worker calls. `prepare` runs inside `run`, which holds no hub guard, and the sink takes only the store's short internal lock.
- ✅ Summary notes are not relayed. The summary generate is consumed in `Summarizer`. Both the loop test (diff 1613-1626) and the server test (`turn_started` count 1) assert this.
- ✅ The existing loop tests pass `Compactor::off()`, through `Fixture.compactor` or inline, and their notification-order assertions are unchanged.
- ✅ Measure fidelity. `LinkMeasure` uses the step's tools and `enable_thinking`. `under_budget_sends_unchanged` asserts that the measure request equals the generate request.
- ⚠️ Hive test results (221/468/476 pass) are as the implementer reported them. Cargo was not run here.

### Strengths

- `KeepingSink` is a small, clean way to keep the newest state in the compactor without reading the store again, and it keeps a state only when the save really succeeded.
- The TOCTOU handling in `SessionCompactionSink` holds up because the store serializes `delete` and `mutate` under one lock.
- `prepare` runs before `meter.requested()`, so summary latency does not inflate TTFT.
- The tests are substantive. They check the exact stub text, before/after equal to the real measures, measure-request fidelity, and the count of summary calls across turns.

### Issues

#### Critical

None.

#### Important

None.

#### Minor

1. **tool_loop.rs:953-956 (diff), `TurnConfig.context_strategy` / `compact_at_pct` (implementer concern 1).** The loop never reads these fields. A `TurnConfig` and the `Compactor` handed to `run` can disagree, and the server test at diff 522-538 does exactly that: the snapshot strategy goes in the config, alongside `Compactor::off()`. The fields are not dead weight, though. The brief requires them, and `SessionView::compactor` builds the compactor from them (diff 402-415), so in production they are the single source. **Verdict: keep them; do not remove.** Fix: say on the two field docs that the loop does not read them, and that they configure the `Compactor` the caller builds (`SessionView::compactor`).

2. **compaction.rs `prepare`, diff 143-145 and 191 (implementer concern 2).** A failed measure is silent. That matches the spec ("sent uncompacted with no record"), and the spec puts no log on that path. But a session set to trim or summarize on a worker whose `measure_prompt` always fails never compacts. In the trace it looks exactly like "under budget", and the link error text is thrown away. **Verdict: the concern is valid but does not block the task.** Logging every step would be noise. Fix: add a `bool` field on `Compactor` and `eprintln!` the first measure error once per turn, as server.rs already does for trace-delete errors. Or defer this to an owner call.

3. **Tests never use a positive `max_tokens` (tool_loop.rs and server.rs compaction tests).** `SamplingConfig::default().max_tokens` is `-1` (engine/types.rs:22), so every new test runs with reservation 0. If the loop passed `0` instead of `config.sampling.max_tokens` (diff 1095), every test would still pass. The reservation is a global constraint. Fix: add one loop test with `max_tokens > 0` where `prompt <= budget` but `prompt + max_tokens > budget`, and assert that a record is produced.

4. **Repeated test literals not routed through consts.** In tool_loop.rs tests, `Compaction::CALL_ID` is `"call_1"`, yet `Fixture::echo_call("call_1")` repeats the literal (diff 1525, 1575). The server tests write `"OLD"`, `"kept"` and `"late"` inline, and `"kept"` appears twice (diff 698, 714, 756-758). The rule is "reused strings as consts". Fix: use `Compaction::CALL_ID`, and hoist the reused server-test summaries into `Compacting` consts.

### Assessment

**Task quality:** Approved

**Reasoning:** The wiring meets every global constraint and every hardest-checked point: index values on every step, an untouched scratch and commit, mid-turn reuse, reset, the snapshot strategy, the deleted-session sink and the step-level record. All four findings are polish or coverage.
