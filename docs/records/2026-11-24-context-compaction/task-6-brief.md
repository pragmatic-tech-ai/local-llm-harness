### Task 6: App: template fields and the trace line

**Files:**
- Modify: `app/src/renderer/src/agents.ts` (`AgentTemplate.context_strategy` and `compact_at_pct`; `StepRecord.compaction?: CompactionRecord | null`)
- Modify: `app/src/renderer/src/TemplateEditorViewModel.ts` and `views/TemplateEditor.mu`, with their tests
- Modify: `app/src/renderer/src/TraceViewModel.ts` (the step's compaction line) and `views/TracePanel.mu`, with their tests

**Interfaces:**
- Produces:
  - `TemplateEditorViewModel`:
    - `ContextChoices` (`Off`, `Trim`, `Summarize`, mapping to `off`, `trim`, `summarize`) and `Context`.
    - `CompactAtText`, which defaults to `75`. Outside 10 to 95, or failing to parse, it shows a field error and disables Save.
    - Both fields take part in `IsDirty` and `Revert`.
    - Save sends `context_strategy` and `compact_at_pct`.
  - `TraceStepViewModel.CompactionText` is `''` when there is no record. Otherwise it is exactly one of these, with thousands separated by U+2009 as elsewhere in the trace:
    - trim: `⟲ trim: {before} → {after} tokens · {e} results elided · {d} turns dropped`
    - summarize: `⟲ summarize: {before} → {after} tokens · covers {n} messages · {s} s`, with one decimal
    - reused: `⟲ summarize (reused): {before} → {after} tokens · covers {n} messages`
    - fallback: `⟲ summarize failed → trim: {before} → {after} tokens · {e} results elided · {d} turns dropped`, plus `  ({summary_error})` when set
  - `HasCompaction`, which the view uses for visibility.

- [ ] **Step 1: Write the failing tests:**
  - `editor_loads_and_saves_context_fields`
  - `compact_at_out_of_range_blocks_save`
  - `old_template_without_fields_shows_off_and_75`
  - `compaction_text_formats_each_variant` (four cases, with the spec's example numbers `7 420 → 5 980`)
  - `step_without_compaction_has_no_line`
- [ ] **Step 2–4:** Run vitest on the touched tests and see them fail, implement, then see them pass. Then run `npm test && npm run typecheck:web && npm run typecheck:node`.
- [ ] **Step 5: Commit** with the message `feat(app): context strategy in the template editor and compaction in the trace`.

