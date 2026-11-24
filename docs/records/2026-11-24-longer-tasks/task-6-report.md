# Task 6 report

## Implemented
- agents.ts: WorkspaceMode, WorkspaceConfig, AgentTemplate.workspace?, ToolTiming.repeat_failure?, NotesResult, NotesChanged (ToolSpec.side_effects already existed).
- RuntimeClient: GetNotes(id), OnNotesChanged signal for `notes_changed`.
- TemplateEditorViewModel: WorkspaceChoices/Workspace/WorkspaceChoice/WorkspaceName/IsWorkspaceNameEnabled; in fingerprint (IsDirty/Revert); Save sends {mode} or {mode:'named', name(trimmed)}; absent loads as Session, empty name. Workspace combo + name box in TemplateEditor.mu.
- ToolChoice.Writes (from side_effects) plus "writes" tag in the tool row.
- SessionViewModel: Notes (`{n}. {text}`), HasNotes, IsNotesExpanded (default true); GetNotes on attach (stale reply dropped if a notes_changed arrived or the session changed); notes_changed for the shown id replaces list; Reset clears. Collapsible Notes panel in SessionView.mu, hidden when empty.
- TraceToolViewModel.RepeatText/HasRepeat + tag in TracePanel.mu.

## Tests
RED (3 files, before implementation): 9 failed | 54 passed. GREEN: `npm test` 17 files / 212 tests pass; typecheck:web and typecheck:node clean.
The existing save-payload test now expects `workspace: {mode:'session'}` (per the Save rule).

## Files
app/src/renderer/src: agents.ts, RuntimeClient.ts, TemplateEditorViewModel.ts(+test), ToolChoice.ts, SessionViewModel.ts(+test), TraceViewModel.ts(+test), views/{TemplateEditor,SessionView,TracePanel}.mu.

## Concerns
- `*.mu.js` are gitignored in this repo (not committed); compile:mu ran via pretest and succeeded.
- Notes panel layout/visuals not hand-checked in the running app.
- tool_choice test stubs client.ListAllTools directly.
