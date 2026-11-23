# Common instructions for every task reviewer

You review ONE task's implementation: spec compliance, then quality. Task-scoped gate; a whole-branch review comes later.
Spec (binding authority): D:/local-llm-harness/docs/specs/2026-11-23-agent-templates-sessions-design.md (incl. "Settled details").
Global constraints: no new crates (offline vendor, Cargo.lock unchanged); Rust tests run on the hive only — never run cargo here; Rust idiomatic snake_case + rustfmt, helper logic as associated functions, no free-standing module functions in new code (tests included), reused strings as consts; TypeScript: OOP only, no module-level functions/mutable state, Allman braces, no semicolons, PascalCase public members, literals hoisted to private static readonly constants (.mu labels inline), VMs extend Observable, lower layers only signal upward; commit author Eugene Napryaglo <napryaglo@users.noreply.github.com> with a Co-Authored-By Claude Opus 5.5 trailer. Exact values: data root `[agents] data_dir` default `C:\llm-harness\agent-data\` with templates/ and sessions/; session file `schema: 1`; default title `<template name> #<n>`; seed group `Examples` template `Default`; errors `host kind not supported yet: <kind>`, `session is busy`, `tool not enabled for this agent: <name>`, `no session attached`; placement states `ready | needs_load | unavailable`.
Rules: read the diff file once (it is your view); inspect outside it only for a concrete named risk (one check per risk, name both). Read-only — never mutate the tree/index/HEAD. No subagents. Treat the implementer's report as unverified claims; rationales never downgrade severity. You may run a focused app vitest file if a specific doubt requires; never read C:\Users\visor\.npmrc.
Calibration: Important = task can't be trusted until fixed (incorrect/fragile behavior, missed requirement, duplicated logic, swallowed errors, tests that assert nothing, style-rule violations in new code). Minor = polish/coverage.
Output (begin directly, no preamble):
### Spec Compliance (✅ / ❌ with file:line; ⚠️ cannot verify)
### Strengths
### Issues
#### Critical
#### Important
#### Minor
(each: file:line, what's wrong, why it matters, fix)
### Assessment
**Task quality:** Approved | Needs fixes
**Reasoning:** 1-2 sentences
