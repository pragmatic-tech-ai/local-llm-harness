# Longer tasks

**Status:** draft 2026-11-24, design playback approved by the owner ("looks good").
**Roadmap:** step 4, longer tasks (`docs/roadmap.md`)
**Builds on:**
- `docs/specs/2026-11-23-agent-templates-sessions-design.md`: templates, sessions and the runtime on :8779
- `docs/specs/2026-11-24-agent-trace-design.md`: the per-step trace
- `docs/specs/2026-11-24-context-compaction-design.md`: trim and summarize, the system-message note

## Intent

Today an agent can only look. Its built-in tools are `get_time`, `list_dir` and `read_file`,
and `ReadOnlyPolicy` refuses any tool with side effects. An agent that is asked to do a piece
of work that takes several steps has nowhere to put its results and nowhere to keep track of
where it is. Compaction (step 3) keeps a long turn inside the 8K window, but a trimmed or
summarised transcript loses details the agent needed to carry forward.

Step 4 gives agents somewhere to work and something to remember with:

- a **workspace** they can write files into;
- a **scratchpad** of notes that survives compaction;
- a small nudge out of **retry loops**, when the same failing call is repeated.

Steps 5 (planning) and 6 (several agents) build on all three.

## Goals

- **Writes, confined.** Agents can create, extend and delete files, but only inside their write
  folder. Nothing escapes the sandbox, the same as today's reads.
- **The template picks the workspace.** Each template chooses where its sessions write: a folder
  per session, one shared folder, or a named folder that several templates can share. The owner
  asked for all three ("all").
- **The tool list is the permission list.** A template that enables a write tool may use it. A
  template that does not, cannot. There is no separate policy to configure.
- **Notes survive compaction.** Notes live on the session, outside the transcript, and are shown
  to the model on every step, so trimming and summarising never lose them.
- **Fewer stuck loops.** When a call fails twice in a row with the same arguments, the model is
  told to try something else.
- **Everything is visible.** Write tools and note tools show in the trace like every other
  tool. The app shows a session's notes and lets a template choose its workspace.

## Non-goals

- No approval prompts. `Decision::Ask` stays unsupported. A tool the template enables is
  allowed.
- No disk quota per workspace beyond the per-call and per-file caps below.
- No moving, renaming or copying files, and no binary files. Files are UTF-8 text.
- No deleting folders that have contents.
- No notes shared between sessions, and no long-term memory.
- No editing of notes from the app. The panel only shows them.
- No web fetch. It stays in the roadmap's Postponed section.

## Anti-goals

- A write that lands outside the write folder, through `..`, an absolute path, the `/sandbox/`
  prefix, or a symlink or junction. One accepted limitation: `append_file` writes through a hard link
  already inside the write folder, because a hard link cannot be told from an ordinary file; only
  a user on the host can plant one there (no tool creates links).
- A read-only template that suddenly gains write access because of this change.
- A delete that removes more than one file or one empty folder.
- Notes that disappear when the transcript is trimmed or summarised.
- A retry nudge that blocks the call or ends the turn. It is a hint only; the step budget stays
  the only hard stop.

## Approaches considered

### Where an agent's files live

- **One shared workspace.** Simple, and everything an agent wrote is in one place. But sessions
  overwrite each other's files.
- **One folder per session.** Isolated and easy to clean up, but work cannot be handed from one
  session to the next.
- **The template chooses.** **Chosen by the owner ("all").** Templates pick `session`, `shared`
  or `named`.

### How writes are permitted

- **Keep a policy object, and add a "writes allowed" flag to the template.** That is two places
  to say one thing.
- **Allow whatever the template enables.** **My call.** The enabled list already gates every
  call (`tool not enabled for this agent: X`), so it becomes the only gate. `ReadOnlyPolicy` is
  replaced by an `EnabledPolicy` that allows every tool. The `ToolPolicy` seam stays, so
  approval prompts can come back later without changing the loop.

### Where notes live

- **In the transcript, as tool results.** Compaction would trim or summarise them away.
- **On the session record, attached to the system message.** **My call.** This is the same
  route the compaction summary already takes, so it does not break the alternation rule of
  Gemma 3's template.

## Design

### Template field `workspace`

`AgentTemplate` gains one field, a TOML table that is optional so existing templates keep
loading:

```toml
[workspace]
mode = "named"     # "session" | "shared" | "named"
name = "research"  # only for "named"
```

| Mode | Write folder | Removed when |
|---|---|---|
| `session` | `{sandbox}/sessions/{session id}/` | the session is deleted |
| `shared` | `{sandbox}/shared/` | never |
| `named` | `{sandbox}/workspaces/{name}/` | never |

Validation, with these error texts:

- an unknown mode gives `unknown workspace mode: {v}`;
- `named` without a name, or with an empty one, gives `workspace name is required for named mode`;
- a name that is not 1 to 64 characters of `A-Z a-z 0-9 _ -` gives `workspace name must be 1-64 letters, digits, _ or -`;
- a name on another mode is dropped silently. This is my call: switching the dropdown in the
  editor should not make a template invalid.

**Default.** A missing `[workspace]` table means `session`. The same holds for session snapshots
taken before this change. This covers both new templates and
existing ones. It is my call, made for safety: no existing template gains a shared folder by
accident. Existing templates enable no write tools, so for them it only changes where reads
resolve (see below).

Sessions snapshot the field with the rest of the template, as they do today. The folder is
created on first use, not when the session is created.

### Paths

Every file tool, read and write, takes paths relative to the session's write folder.

- **Reaching the rest of the sandbox.** A path that starts with `/sandbox/` (or is exactly
  `/sandbox`) is resolved against the sandbox root instead. Reads can reach the whole sandbox
  this way, including other workspaces, other sessions' folders and loose files like
  `notes.txt`. A session reading another session's files is intended: it is how one session
  picks up another's work. Write tools refuse it
  with `writes are confined to your workspace`.
- **Everything else stays as today.** Every other absolute or anchored path is refused, and
  `..` may not climb out of the folder it is relative to. Both use today's
  `Sandbox::resolve` rules and messages.
- **Writes and symlinks.** A write to a path that does not exist yet canonicalises its nearest
  existing ancestor, and that ancestor must lie inside the write folder. Every file tool,
  read or write, first creates the write folder if it is missing. That way the folder always
  exists, and is canonical, before any path is checked against it. Comparisons are made between
  canonical paths, as `Sandbox::resolve` does today, so Windows `\\?\` prefixes and case are
  handled the same way. A symlink or junction
  that leads out is refused with `path escapes the sandbox`.
- **Displayed paths.** Paths in tool results stay relative and never expose host paths. Paths
  inside the write folder are shown relative to it. Other sandbox paths are shown as
  `/sandbox/...`.
- **Reserved names.** `sessions`, `shared` and `workspaces` at the sandbox root are created by
  the runtime as needed. Files already there are left alone.

The tool descriptions tell the model about `/sandbox/`. The `read_file` and `list_dir`
descriptions say: "Paths are relative to your workspace; prefix `/sandbox/` to read elsewhere in
the sandbox."

**No sandbox configured.** When the catalog has no `[tools] sandbox`, the file tools are not
registered, as today. The new write tools follow the same rule. The note tools need no sandbox
and are always registered.

### Write tools

All four have `side_effects: true` and are confined to the write folder. Each creates the write
folder if it is missing.

| Tool | Arguments | Effect | Result |
|---|---|---|---|
| `write_file` | `path`, `content` (strings) | creates or overwrites; creates missing parent folders | `{path, bytes}` |
| `append_file` | `path`, `content` | appends; creates the file and its missing parent folders if needed | `{path, bytes}`, the file's new size |
| `make_dir` | `path` | creates the folder and its parents; no error if it exists | `{path}` |
| `delete_path` | `path` | deletes one file or one empty folder | `{path, deleted: "file" \| "dir"}` |

Limits and errors:

- **Content size.** `content` may be at most 262 144 bytes of UTF-8 per call, otherwise
  `content exceeds 262144 bytes`.
- **File size.** A file may be at most 1 048 576 bytes after an append, otherwise
  `file would exceed 1048576 bytes`.
- **Writing over a folder.** `write_file` or `append_file` on an existing folder gives
  `path is a directory`.
- **Making a folder over a file.** `make_dir` over an existing file gives `path is a file`.
- **Non-empty folders.** `delete_path` on one gives `directory is not empty`. On the write
  folder itself (`.` or empty path) it gives `cannot delete the workspace root`.
- **Missing paths.** `delete_path` on a missing path gives today's `no such path: {path}`.
- **Interrupted writes.** `write_file` writes a temp file in the same folder, then renames it,
  so a timeout or crash never leaves half a file.

All file work runs on `spawn_blocking`, as the read tools do today.

### Permission

- `ReadOnlyPolicy` is removed.
- `EnabledPolicy` returns `Allow` for every call. The executor already refuses tools that are
  not enabled before it consults the policy, so the template's tool list is the only gate.
- The executor's order stays: parse error, unknown tool, not enabled, policy, schema, run.
- The test `executor_denies_side_effects_under_read_only` becomes a test that a side-effect tool
  runs when enabled and is refused with `tool not enabled for this agent: X` when not.

### Notes (scratchpad)

Three tools, with `side_effects: false`. They change only the session's own state.

| Tool | Arguments | Result |
|---|---|---|
| `note_add` | `text` (string) | `{number, count}` |
| `note_list` | none | `{notes: [{number, text}]}` |
| `note_clear` | none | `{cleared: n}` |

Storage and limits:

- **Where notes live.** `SessionRecord.notes: Vec<String>`, serde default empty, skipped when
  empty. A note's number is its position, counted from 1.
- **Limits.**
  - Empty or whitespace-only text gives `note text is empty`.
  - Each note is at most 300 characters, otherwise `note exceeds 300 characters`.
  - A session holds at most 20 notes, otherwise `note limit reached (20); use note_clear to start over`.
  - These caps are my call. They keep the block under about 6 000 characters, roughly 1 500
    tokens, which is a fifth of the 8K window at most.
- **Saving.** A note is saved through the session store's own short lock as soon as the tool
  runs, whatever the turn's outcome. This is the same route `set_compaction` uses. A session
  deleted meanwhile is left alone, and the tool returns `session deleted`.
- **Reset.** `reset` clears the notes along with the transcript and the summary.

**What the model sees.** On every step, when the session has notes, the model's copy of the
system message ends with:

```
[Your notes]
1. first note
2. second note
```

- **Placement.** The block is separated from what comes before it by a blank line, and it comes
  after the compaction summary note when there is one. A session with no system prompt gets a
  system message holding just the block.
- **Model's copy only.** Like the summary note, the block changes only what is sent to the
  model. The stored transcript and the chat stay as they are.
- **Read fresh every step.** Notes are re-read before every step, so a note added in step 2 is
  in step 3's prompt.
- **Compaction.** Compaction measures the prompt with the block included. The block is part of
  the fixed system part, which compaction never trims. A fixed part that alone overflows the
  window is handled as compaction handles it today: the step is refused and the turn ends
  `window_full`. With the caps above this needs a very long system prompt, so it gets no new
  handling.

**How tools reach the session.** `ToolContext` gains the turn's context: its write folder, the
sandbox, and a session handle. The handle is a trait declared in `runtime` (`SessionHandle`):

- it reads and changes the notes;
- it answers whether the session still exists.

The server implements it over the session store and the hub, the same way
`SessionCompactionSink` works today. `TurnConfig` carries the workspace and the handle, and the
executor passes them into each call.

**Notification route.** The server's implementation sends `notes_changed` through the hub to
every connection attached to the session, the same route `trace_step` takes. `reset` sends it
too, with an empty list. Deleting a session sends none, because `sessions_changed` already
covers it.

**Writes after a delete.** Write tools ask the handle first. When the session no longer exists,
they refuse with `session deleted`, so a turn still running when its session is deleted cannot
recreate the folder. This is my call.

### Retry nudge

Within one turn, the loop remembers the last tool call that failed, as its name plus its
canonical arguments (`serde_json` value equality).

- **What counts as a failure.** Any `ToolOutcome::Err`, including the executor's own refusals:
  unknown tool, not enabled, invalid arguments, timeout. This is my call. A model that repeats
  a refused call is just as stuck.
- **Order.** Calls are taken in the order they are made, within a step and across steps.
- **What is stored.** The result with the hint is the tool result. It goes to the model and into
  the stored transcript alike.

- **When it fires.** The next call is identical and fails again. Its result then becomes
  `{"error": "<message>", "hint": "This call already failed twice; try something different."}`.
- **When it resets.** Any call that succeeds or differs replaces what the loop remembers. A new
  turn starts with nothing remembered.
- **Later repeats.** A third identical failure gets the hint again.
- **Trace.** The step records the call as failed, as today. `ToolRecord` gains
  `repeat_failure: bool`, serde default false and skipped when false, and the app shows it as a
  "repeated failure" tag on the tool line.

### Reset

`reset` clears the transcript, the summary and the notes. It leaves the workspace's files where
they are. This is my call: deleting files cannot be undone, and deleting the session removes a
`session` folder anyway.

### Deleting a session

`delete_session` removes `{sandbox}/sessions/{id}/` with everything in it, but only when the
session's snapshot mode is `session`. A failure to remove it is logged, as the trace-log delete
is today. It never fails the delete. Shared and named folders are never removed.

The removal must not follow junctions or symlinks out of the folder. It deletes links as links.

### Seeded templates

- **`Examples/Default`.** Gets an explicit `[workspace] mode = "session"` and keeps the
  read-only tools it has today. The seed lists every registry tool today (`rules.tools`). The
  seed now lists only the tools without side effects, plus the note tools.
- **`Examples/Worker`, new.** All write tools, all note tools, `list_dir`, `read_file` and
  `get_time`; workspace `session`; `step_budget = 16`; `context_strategy = "trim"`. Its system
  prompt says, in a few lines: you work in a private workspace folder; save results as files;
  keep a running plan with `note_add`; check your notes before you start each step.
- **When seeds are written.** Only when the template root is empty, as today. An existing
  install gets no `Worker` seed. That is fine: the owner can create one from the editor.

### App

- **Template editor.**
  - A Workspace row: a combo box with Session, Shared and Named.
  - A name text box, enabled only when Named is selected.
  - Save validation errors come back from the backend as today.
- **Tool list.** The four write tools and three note tools appear through `list_all_tools`
  without app changes. Each write tool is marked with a "writes" tag, from `side_effects`, so
  the owner can see which ones change files.
- **Session view: a Notes panel.**
  - A collapsible section under the chat that lists the numbered notes.
  - It is hidden when the session has none.
  - It is fed by `get_notes {id}` → `{notes: [string]}`, which needs no attachment, and by a
    `notes_changed {id, notes}` notification sent to attached connections whenever notes
    change, including on reset.
- **Trace panel.** The "repeated failure" tag described above.

### Wire summary

| Change | Shape |
|---|---|
| template field | `workspace: {mode: "session" \| "shared" \| "named", name?: string}` |
| tool spec in `list_all_tools` / `list_tools` | adds `side_effects: bool` if not already sent |
| new method | `get_notes {id}` → `{notes: [string]}` |
| new notification | `notes_changed {id, notes: [string]}` |
| trace `ToolRecord` | `repeat_failure?: true` |

### Existing templates

Relative reads move with this change. Today `read_file("notes.txt")` reads the sandbox root.
After it, the same path reads the session's own folder, so an existing read-only template has
to write `/sandbox/notes.txt`.

The owner approved this rule in the design playback. What softens it:

- the `read_file` and `list_dir` descriptions say so on every step;
- `list_dir` of an empty write folder returns, besides its empty `entries`, the line
  `note: your workspace is empty; prefix /sandbox/ to list the shared sandbox`.

## Left to the builder

- **Workspace type.** The Rust shape of the workspace type and of `ToolContext`'s new fields.
- **Resolve-for-write.** How it shares code with `Sandbox::resolve`.
- **Notes block wiring.** How the notes block is woven into the step's message assembly beside
  the compaction note, as long as it is measured.
- **Panel placement.** Where exactly the Notes panel sits in the session view, as long as it is
  collapsible and hidden when empty.
- **Test layout.** Unit tests sit beside the code, as in the existing modules. Live proof on the
  hive uses `Examples/Worker`.

## Live proof (after deploy)

The proof uses the `Examples/Worker` template on gemma-4.

1. Ask the Worker to write a short report into `report.md`, then add a line to it.
2. Confirm on the hive that the file is in `sandbox/sessions/{id}/`, and that the trace shows
   `write_file` and `append_file` as ok.
3. Ask it to note a plan of three steps, then force a trim. Ask what its plan is. It answers
   from the `[Your notes]` block, and the Notes panel shows the three notes.
4. Ask it to read a missing file twice. The second result carries the hint and the trace line
   shows "repeated failure".
5. Ask it to write to `/sandbox/x.txt` and to `../x.txt`. Both are refused.
6. Delete the session. Its folder is gone.
7. A Default session still reads `/sandbox/notes.txt`, and cannot call `write_file`.
