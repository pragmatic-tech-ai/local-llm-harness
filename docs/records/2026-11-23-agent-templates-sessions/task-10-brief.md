### Task 10: Agents capability (template tree and editor)

**Files:**
- Create: `views/AgentsModule.mu` (capability `Name = "Agents"`, with a new icon `views/icons/Agents.svg` in the `RuntimePlay.svg` style), `AgentsService.ts`, `AgentsTreeViewModel.ts`, `TemplateEditorViewModel.ts`, `views/AgentsPanel.mu`, `views/TemplateEditor.mu`, and their tests. Register the module in `app.mu`.

**Interfaces:**
- Consumes: Task 8 `RuntimeClient`. Use the same client instance as `SessionsService`, through a shared service; do not open a second socket.
- Produces:
  - `AgentsTreeViewModel` has `Roots` (`TemplateTreeNodeViewModel`, recursive: `Name`, `IsGroup`, `Children`, `Error`), `Selected`, and the commands `NewTemplate`, `NewGroup`, `Rename`, `Delete`, `MoveTo` and `Refresh`. It refreshes on `templates_changed`.
  - Selecting a template opens the editor in the content host.
  - `TemplateEditorViewModel` edits:
    - `Name`, `SystemPrompt`, `StepBudgetText`, `RuntimeChoices`/`Runtime`, `ModelChoices`/`Model` (filtered by runtime), `ContextSizeText`, `EnableThinking`, `TemperatureText`/`TopPText` (blank = none), `HostChoices`/`Host`
    - `Tools: ObservableCollection<ToolChoice {Name, Description, Enabled}>`
  - It has `SaveCommand` and `RevertCommand`, `IsDirty`, and an `ErrorText` set from the `save_template` error message. A broken template opens with `ErrorText` = its load error and the raw fields it could recover. Otherwise the fields start from the defaults.
  - New template: `Untitled` in the selected group. New group: `New group`.

- [ ] **Step 1: Write the failing tests:**

```ts
'tree_renders_groups_and_templates'
'selecting_template_opens_editor'
'editor_save_sends_fields_and_clears_dirty'
'editor_shows_validation_error'            // SaveTemplate rejects "unknown tool: x" → ErrorText
'model_choices_filter_by_runtime'
'delete_group_error_surfaces'              // "group is not empty"
'broken_template_opens_with_error'
'templates_changed_refreshes_tree'
```

- [ ] **Step 2–4: Run FAIL, implement, run PASS** (`npm test && npm run typecheck:web`).
- [ ] **Step 5: Commit:** `feat(app): Agents capability with template tree and editor`.

