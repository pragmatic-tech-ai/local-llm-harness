# Agentic capabilities roadmap

Agreed 2026-11-23, after tool calling and the Agent Runtime component shipped
(`docs/specs/2026-11-23-tool-calling-runtime-design.md`). Tool approval is postponed: the
policy seam stays, unused.

## Hive constraints that shape the order

- **One model at a time.** The 6 GB GPU holds one loaded model. "Several agents" means one
  model in several roles; swapping models costs tens of seconds.
- **Small context.** About 8K tokens fills after a few tool results, so context management
  comes before long tasks and multi-agent work.

## Steps

1. **Agent definitions.** *Shipped 2026-11-23* as agent templates and sessions
   (`docs/specs/2026-11-23-agent-templates-sessions-design.md`). A template is a named
   bundle of system prompt, tool set, step budget, runtime + model + context size and
   sampling defaults, stored as a TOML file in groups and edited in the **Agents**
   capability. A session is started from a template (snapshotting it) and persisted with
   its transcript; the **Sessions** capability lists and opens sessions. The dispatcher
   loads a session's model on an explicit Load or on its first message, through the
   supervisor. Everything below hangs off this.
2. **Agent trace in the instrument panel.** *Shipped 2026-11-24* (`docs/specs/2026-11-24-agent-trace-design.md`). Per step: context fill, tokens, tool latency,
   and why the loop stopped. This makes every later capability measurable on the small
   model.
3. **Memory and context.** *Shipped 2026-11-24* (sessions persist transcripts; `docs/specs/2026-11-24-context-compaction-design.md` adds per-template trim/summarize compaction). Conversations survive reconnects, supervisor restarts and model
   swaps. When the window fills, old tool results are compacted or summarised instead of
   the turn failing with `window_full`.
4. **Longer tasks.** Per-agent step budgets, a scratchpad/notes tool, and write tools in
   the sandbox. The agent's tool list is its permission list, so no approval UI is needed.
   The agent can recover from tool errors.
5. **Planning.** A plan/todo tool and a plan-then-execute loop, with the plan visible in
   the UI.
6. **Multiple agents.** A coordinator hands subtasks to specialist agents on the same
   loaded model, each with its own prompt, tools and fresh context, and gets a summary
   back. Delegation is itself a tool call.
7. **MCP tool sources.** External tool servers plug into the existing `ToolSource` seam.
   This step is independent of the others and can move anywhere in the order.
8. **TODL graph connection.** Agents read and query the TODL graphs through tools, using
   step 7 or built-ins.

## Postponed

- Web fetch and research tools (owner idea, 2026-11-24). Open questions to settle first:
  HTTPS support (vendor rustls or call the hive's `curl.exe`), turning HTML into text
  (write our own or vendor a crate), how the agent finds pages (fetch only, keyless
  DuckDuckGo scraping, or a search API with a key such as Brave), refusing
  private and LAN addresses, and pages too large for the 8K window, which step 3 helps
  with.

- Tool approval (`Ask` policy, approval UI).
