# Task 6 report: app template fields and the trace line

Status: DONE. Commit message `feat(app): context strategy in the template editor and compaction in the trace`.

## Built
- `agents.ts`: `ContextStrategy`, `CompactionRecord` (mirrors the task-5 JSON field for field), `AgentTemplate.context_strategy?` / `compact_at_pct?` (optional, so old templates read as off/75), `StepRecord.compaction?: CompactionRecord | null`.
- `TemplateEditorViewModel`: `ContextChoices` (Off/Trim/Summarize), `Context`, `ContextChoice`, `CompactAtText` (default 75), `CompactAtError`/`HasCompactAtError` (whole number 10..95), in fingerprint (IsDirty/Revert), `HasFieldErrors` (blocks Save), Fill, Defaults and Build (sends `context_strategy`, `compact_at_pct`; sampling/host preservation unchanged).
- `TraceStepViewModel`: `CompactionText`, `HasCompaction`, using `TraceNumbers.Thousands/Seconds`. Fallback appends `  ({summary_error})` when set; `summarized_messages` is the "covers" count.
- Views: `TemplateEditor.mu` (Context combo, Compact at % field and error, after Context size), `TracePanel.mu` (compaction line under the stats).

## Tests
Added: editor_loads_and_saves_context_fields (also checks Revert and that top_k/seed survive Save), compact_at_out_of_range_blocks_save, old_template_without_fields_shows_off_and_75, compaction_text_formats_each_variant (4 variants plus the error suffix), step_without_compaction_has_no_line. The existing save test now expects the two new fields.
Result: `npm test` 17 files / 202 passed; `typecheck:web` and `typecheck:node` clean (.mu compiled 14 files).
TDD note: implementation was written before the first test run, so there was no separate RED run. The only failure seen was my own test using plain spaces instead of U+2009, then fixed.

## Owner live UI check
1. Open an old template: Context shows Off, Compact at % shows 75, not dirty.
2. Set Context to Summarize and Compact at to 9 or 96: error shows and Save is disabled; 10 and 95 are accepted.
3. Save, reopen: values persist; Revert restores them.
4. Run a long turn with Trim, then with Summarize: the dim `⟲` line shows under the step stats, with thin-space thousands.
5. A step with no compaction shows no extra line.

## Fix round 1
1. Merged the two `./TraceViewModel` imports in the trace test.
2. `HasCompaction` is now a direct null/undefined check on the record.
3. Two assertions added to `compaction_text_formats_each_variant`. Decisions:
   - A trim record with `fallback: true` prints the plain trim line (the flag only means something for summarize), and ignores `summary_error`.
   - A reused summary with `summary_ms: null` prints the reused line, which has no time field.
4. No change (process note).
5. Wrapped the TemplateEditor.mu header comment.
Evidence: `npm test` all pass (17 files); `typecheck:web` clean.
