# Task 9 report: app chat through the agent runtime

## Built
- SupervisorClient: HarnessInfo.chat_url?; picker OnReady raises chat_url ?? url (plain-worker fallback kept).
- HarnessClient: OnToolCall/OnToolResult/OnLoopDone signals, ListTools(), ModelProps.supports_tools?, ToolCallNote/ToolResultNote/LoopDone/ToolInfo types.
- messages.ts: ToolCallMessage, ToolResultMessage (StatusText ok/failed).
- ChatViewModel: busy flag + turnToken (cleared only when the SendTurn promise settles), live `segment`, new segment on turn_started, tool_call/tool_result rows, turn_error/turn_rejected append to segment then null it, loop_done != answered adds "[not kept: reason]", ToolsText ('tools: a, b' or 'tools: off') via model_props.supports_tools then list_tools, any failure -> off.
- Chat.mu: templates for both tool messages (Consolas, result @Fg2), ToolsText TextBlock (BodySm, Fg2) docked Top.

## TDD
RED: 7 new tests failed before implementation (vitest). GREEN: 111/111 pass; typecheck:web and typecheck:node clean.

## Tests
New: ready_raises_chat_url_when_present, ready_falls_back_to_url, tool_notifications_raise_signals, list_tools_sends_method, multi_step_turn_renders_call_result_and_answer, send_resolves_after_loop_done_not_turn_done, not_kept_loop_adds_note, tools_text_reflects_runtime.
Adjusted existing tests whose premise ("turn_done frees Send") the new rule intentionally removes: they now answer the send_turn response before the next Send (Rig.Respond / FinishTurn); the sampling-failure test id shifted 2->3 because Bind now issues one extra model_props; the late-rejection test now uses Bind() to drop the old turn.

## Self-review / concerns
- ChatViewModel issues its own model_props request in addition to the panel's (one extra frame per Bind).
- A send_turn failure after turn_error/turn_rejected adds no duplicate note (reported flag); failure between passes opens a new assistant message for the note.
- Brief's template text "-> {Name} {Arguments}" has no string interpolation precedent in .mu, so each part is a separate TextBlock in a DockPanel. No visual check done (not run in Electron).
- The brief mentions a "chat header"; none existed, so the ToolsText line sits at the top of the chat DockPanel.
- ToolsText is 'off' when list_tools is empty, per brief.

## Review follow-ups (fix report)
1. AbandonTurn now uses AppendNote when busy, so [disconnected] appears between loop steps (new item when segment is null). Test disconnect_between_loop_steps_still_notes. The late-rejection test's indices shifted by one for this reason.
2. tool_call with parse_error renders `raw  [parse error: msg]` as Arguments; undefined/null arguments and content render ''. Tests tool_call_with_parse_error_shows_raw_and_error, tool_call_without_arguments_renders_empty.
3. One model_props per Bind: InstrumentPanelViewModel exposes OnModelProps (signal, raised after its fetch); ChatViewModel subscribes (down-call/upward signal only) and calls list_tools when supports_tools. Test model_props_is_fetched_once_per_bind. SurfacesSamplingFailureOnPanel id restored to 2.
4. Test rebind_with_live_segment_notes_old_and_leaves_new_untouched.
5. Rig helpers IdOf(method)/Respond(method)/Flush() replace hard-coded ids and inline setTimeout in the tests added or changed here.
Evidence: npm test 116/116 passed, typecheck:web clean.

Test tightening: rebind test now uses a connected clientB and a stale chunk on the old client; panel Rig gained IdOf (used in SurfacesSamplingFailureOnPanel, which also asserts one model_props) and FinishTurn calls Flush. 116/116, typecheck clean.
