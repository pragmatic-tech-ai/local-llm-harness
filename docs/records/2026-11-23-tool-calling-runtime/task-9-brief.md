### Task 9: App — chat through the runtime, tool blocks, tools indicator

**Files:**
- Modify: `app/src/renderer/src/SupervisorClient.ts`, `HarnessPickerViewModel.ts`, `HarnessClient.ts`, `messages.ts`, `ChatViewModel.ts`, `views/Chat.mu`
- Test: `SupervisorClient.test.ts`, `HarnessPickerViewModel.test.ts`, `HarnessClient.test.ts`, `ChatViewModel.test.ts`

**Interfaces:**
- Produces:
  - `HarnessInfo.chat_url?: string`. The picker's `OnReady` raises `info.chat_url ?? info.url`.
  - `HarnessClient`:
    - signals `OnToolCall: TurnSignal<ToolCallNote>`, `OnToolResult: TurnSignal<ToolResultNote>`, `OnLoopDone: TurnSignal<LoopDone>`
    - `ListTools(): Promise<ToolInfo[]>` (method `list_tools`)
    - `ModelProps` type gains `supports_tools?: boolean`
    - types:
      - `ToolCallNote { id: string, name: string, arguments: unknown, raw: string, parse_error?: string }`
      - `ToolResultNote { id: string, name: string, ok: boolean, content: unknown }`
      - `LoopDone { steps: number, reason: string }`
      - `ToolInfo { name: string, description: string, side_effects: boolean }`
  - `messages.ts`:
    - `ToolCallMessage(name: string, argumentsText: string)`, with `Name` and `Arguments`
    - `ToolResultMessage(name: string, ok: boolean, contentText: string)`, with `Name`, `Ok`, `Content` and `StatusText` (`'ok'` / `'failed'`)
  - `ChatViewModel.ToolsText: string`:
    - `'tools: get_time, list_dir, read_file'` when `supports_tools` is true and `ListTools` is non-empty
    - otherwise, and on any failure, `'tools: off'`

`ChatViewModel` turn handling replaces the "first `turn_done` ends the turn" rule:
- A private `busy` flag is set in `Send` and cleared only when the `SendTurn` promise settles. It is the guard for `Send`.
- `segment` is the live `AssistantMessage`:
  - `Send` creates the first one, as today.
  - On `turn_started` while busy with no `segment`, a new `AssistantMessage` is added.
  - `turn_chunk` appends to `segment`.
  - `turn_done` sets `segment = null`.
- `tool_call` adds a `ToolCallMessage`, with arguments as `JSON.stringify`.
- `tool_result` adds a `ToolResultMessage`.
- `turn_error` and `turn_rejected` append their existing notes to `segment`, then set `segment = null`.
- `loop_done` with a reason other than `answered` adds `new AssistantMessage('[not kept: <reason>]')`.

This still works against a plain worker, which never sends `loop_done`.

- [ ] **Step 1: Write failing tests:**

```ts
// HarnessPickerViewModel
'ready_raises_chat_url_when_present'           // info has url ws://w and chat_url ws://r → ready == ['ws://r']
'ready_falls_back_to_url'
// HarnessClient
'tool_notifications_raise_signals'             // tool_call/tool_result/loop_done frames → each signal raised with params
'list_tools_sends_method'                      // sent frame method == 'list_tools'
// ChatViewModel (fake client over FakeSocket, as the existing tests do)
'multi_step_turn_renders_call_result_and_answer' // turn_started, chunk "", tool_call get_time, turn_done, tool_result ok, turn_started, chunk "It's 14:05.", turn_done, loop_done answered, response → Transcript types [User, Assistant, ToolCall, ToolResult, Assistant]; last Text "It's 14:05."
'send_resolves_after_loop_done_not_turn_done'  // after the first turn_done, Send is still blocked (a second Send adds nothing); unblocked after the response
'not_kept_loop_adds_note'                      // loop_done step_limit → last item Text "[not kept: step_limit]"
'tools_text_reflects_runtime'                  // supports_tools true + list [get_time] → 'tools: get_time'; supports_tools false → 'tools: off'
```

- [ ] **Step 2: Run, expecting FAIL:** `cd app && npx vitest run`.
- [ ] **Step 3: Implement.** In `Chat.mu`, add `DataTemplate`s for `ToolCallMessage` (`→ {Name} {Arguments}`, mono font) and `ToolResultMessage` (`← {Name} {StatusText}: {Content}`, `@Fg2`), reusing the assistant bubble style. Add a `TextBlock` bound to `$ToolsText` (`Style = @BodySm, Foreground = @Fg2`) in the chat header.
- [ ] **Step 4: Run, expecting PASS:** `cd app && npm test && npm run typecheck:web`.
- [ ] **Step 5: Commit:** `feat(app): chat via the agent runtime with tool call/result blocks`.

