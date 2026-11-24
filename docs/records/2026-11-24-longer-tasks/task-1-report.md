# Task 1 report

Implemented WorkspaceMode (ALL-table pattern), WorkspaceConfig (folder, normalized, private validate), AgentTemplate.workspace, TemplateRules.writing_tools + workspace validation, TemplateStore::save saving workspace.normalized(), SessionRecord.notes (serde default, skipped when empty), SessionStore::set_notes (mutate, Bump::Kept), reset clears notes. DiscoveredCatalog::new gained a writing_tools arg; main.rs fills it from registry specs with side_effects (empty today); test_support rules() does the same.

TDD: RED `scripts/hive-test.sh -- agents::` -> compile errors (E0422 WorkspaceConfig, E0609 notes/workspace, E0599 set_notes). GREEN same command -> `99 passed; 0 failed`. Full `scripts/hive-test.sh -- --tests` -> 488 + 7 + 10 + 6 + 2 + 5 passed, 0 failed. `--restore` run.

Files: agents/{template,session,session_store,template_store,dispatcher}.rs, runtime/{catalog_source,test_support}.rs, main.rs.

Concerns: none. Validation of the name is by chars (ASCII only allowed); save-path normalization has no dedicated test beyond normalized() unit test.
