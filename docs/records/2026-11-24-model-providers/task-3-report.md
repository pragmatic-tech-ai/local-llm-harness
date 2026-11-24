# Task 3 report

Implemented: AgentTemplate.repository (serde alias host, skipped when None); RepositoryKind, RuleModel, RepositoryRules; TemplateRules.hosts/models replaced by repositories (validate_model per kind, pragmatic_models helper for seeds); ModelRequirement::of reads repository; DiscoveredCatalog builds one Pragmatic RepositoryRules per host; list_catalog hosts unchanged; fixtures updated.

RED: `scripts/hive-test.sh -- agents::` -> compile errors (undeclared RepositoryKind, no field `repository`), TEST_EXIT=101.
GREEN: `scripts/hive-test.sh -- --tests` -> all pass, TEST_EXIT=0; then --restore.

Tests (in agents/template.rs): legacy_host_field_routes_to_hive, missing_repository_means_hive, unknown_repository_is_rejected, openrouter_model_validates_runtime_model_and_ctx, openrouter_unknown_list_refuses_save, pragmatic_validation_unchanged. template_store host case now expects `unknown repository: far`.

Concerns: pragmatic models are duplicated per host name (discovery is shared); OpenRouter wrong runtime reports `unknown runtime: X`. Old session snapshots with `host` read via the alias.
