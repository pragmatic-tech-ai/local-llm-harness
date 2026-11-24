### Spec Compliance
- ✅ agents.ts: CompactionRecord matches the task-5 JSON field for field (nullables: summary_ms, summary_prompt_tokens, summary_out_tokens, summary_error). `StepRecord.compaction?: CompactionRecord | null` covers the absent-not-null serialization. `AgentTemplate` fields are optional, so old templates still read.
- ✅ Four line formats (TraceViewModel.ts:561-565, 588-615) match the spec; fallback appends `  ({summary_error})`; "covers" uses `summarized_messages`; numbers reuse `TraceNumbers.Thousands/Seconds`. U+2009 confirmed in the test file at lines 389-397.
- ✅ Editor: ParsePercent accepts whole 10..95 only; a parse failure goes through HasFieldErrors and Build, so Save is blocked. Both fields are in Fingerprint (IsDirty/CanRevert/Revert via Fill). Build keeps sampling/host as loaded. Missing fields give Off/75.
- ✅ `.mu` follows the existing patterns (label + ComboBox/TextBox + danger error TextBlock; the compaction TextBlock matches StatsText styling). Labels inline.
- ⚠️ Live render not verified (owner check).

### Strengths
Tight, pattern-following change. Tests cover all four variants, the error suffix, null/absent, bounds (9/96/abc/blank/7.5/10/95), Revert, and top_k/seed surviving Save.

### Issues
#### Critical
None.
#### Important
None.
#### Minor
- TraceViewModel.test.ts:3-4: two imports from './TraceViewModel'; merge into one.
- TraceViewModel.ts:617-620: `HasCompaction` rebuilds the full string via `CompactionText`; use a direct null/undefined check on the record.
- TraceViewModel.ts:588: the only mapping for a trim-strategy record that has `fallback: true` is the plain trim line; fine, but there is no test for the reused-with-null-summary_ms or a trim record, only the four spec cases. Optional.
- Process: no RED run was done (report admits it). Risk is low because the tests reference members that did not exist, but the rule skipped a step. No code change needed.
- TemplateEditor.mu:2 header comment is now a long line; wrap it.

### Assessment
**Task quality:** Approved
**Reasoning:** Types, formats, validation, dirty/revert, and Save preservation all match the spec and JSON; only polish items remain.
