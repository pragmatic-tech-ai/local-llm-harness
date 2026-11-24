# Task 5 report: app client for the trace

Files: app/src/renderer/src/agents.ts (types), RuntimeClient.ts, RuntimeClient.test.ts.

## Contract for Task 6 (import types from './agents')
- `type StepEnd = 'tool_calls'|'answered'|'budget'|'rejected'|'error'|'aborted'`
- `ToolTiming {name:string,id:string,ok:boolean,ms:number,bytes:number}`
- `StepRecord {v:number,kind:'step'|'turn',turn,step,at,prompt_tokens,n_ctx,out_tokens,think_tokens:number,load_ms:number|null,ttft_ms:number|null,gen_ms:number|null,end:StepEnd,tools:ToolTiming[],error:string|null}`
- `TurnRecord {v,kind,turn,at,steps,reason:string,total_ms,out_tokens,think_tokens,peak_fill:number,committed:boolean,user:string}`
- `TraceTurn {turn:number,steps:StepRecord[],summary:TurnRecord|null,reset_before:boolean}`
- `TracePage {turns:TraceTurn[],more:boolean,reset_pending:boolean}`
- Notes: `TraceStepNote {id,record:StepRecord}`, `TraceTurnNote {id,record:TurnRecord}`, `TraceWarning {id,message}`
- `RuntimeClient.GetTrace(id: string, before?: number, limit?: number): Promise<TracePage>` (wire `get_trace`, undefined params omitted, no attach needed)
- Signals: `OnTraceStep: TypedSignal<TraceStepNote>`, `OnTraceTurn: TypedSignal<TraceTurnNote>`, `OnTraceWarning: TypedSignal<TraceWarning>` (wire `trace_step`/`trace_turn`/`trace_warning`)

## TDD
RED: `npx vitest run src/renderer/src/RuntimeClient.test.ts` -> 2 failed (GetTrace rows, trace_notifications_raise_signals). GREEN: 19 passed. `npm test` 177 passed (16 files); `npm run typecheck:web` clean. LF endings verified.

## Fix round 1
Fixed all four Minors: `kind` is now the literal `'step'` on StepRecord and `'turn'` on TurnRecord; the StepRecord doc comment reworded to match; `trace_notifications_raise_signals` now collects per-signal lists and asserts each holds only its own payload; type imports in RuntimeClient.ts sorted alphabetically. `npm test` 177 passed, `typecheck:web` clean. Signatures otherwise unchanged (kind types narrowed only).
