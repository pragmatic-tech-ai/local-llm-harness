# Task 6 report

Implemented: NoteUsage (tool_loop.rs) parses turn_done.usage; StepMeter takes estimated from turn_started, skips reasoning-flagged chunks for the Gemma counter, and lets usage override prompt/out/think tokens and set cost (estimated cleared). StepRecord gains cost/estimated, CompactionRecord gains summary_cost (all serde default, skipped when empty). Summarizer swaps each call's counted tokens for usage and sums cost.

Evidence: compile-first RED (tests reference new fields; written alongside impl, so no separate RED run captured). `hive-test.sh -- runtime::` 313 passed; `-- --tests` 616 passed, 0 failed, no warnings; `--restore` RESTORED; rustfmt --check clean.
Tests: usage_overrides_counted_tokens_and_sets_cost, estimated_flag_when_no_usage, reasoning_chunks_do_not_count_as_gemma_thinking, pragmatic_steps_unchanged, summary_cost_recorded, new_fields_skipped_when_empty.

Files: backend/src/runtime/{tool_loop,trace,compaction}.rs.
Concerns: no separate RED run (impl written before the first test run). Without usage, a step's out_tokens for OpenRouter reasoning chunks is 0 (tokens empty).
