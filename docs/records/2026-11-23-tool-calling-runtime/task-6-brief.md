### Task 6: Worker link and connector

**Files:**
- Create: `backend/src/runtime/worker.rs`
- Modify: `backend/src/supervisor/manager.rs` (add `ready_worker_url`)

**Interfaces:**
- Produces:
  - `Note { method: String, params: Value }`
  - `GenerateRequest { messages: Vec<ChatMessage>, tools: Vec<Value>, enable_thinking: bool, sampling: SamplingConfig }` (Serialize, matching Task 3's wire params)
  - `LinkError(String)`
  - `#[async_trait] trait WorkerLink: Send + Sync`, with methods:
    - `async fn model_props(&self) -> Result<Value, LinkError>`
    - `async fn tokenize(&self, text: &str) -> Result<Value, LinkError>`
    - `async fn generate(&self, req: &GenerateRequest) -> Result<BoxStream<'static, Result<Note, LinkError>>, LinkError>` — the stream yields every notification and ends when the response frame arrives. An error frame becomes `Err(LinkError)`. A socket closed before the response simply ends the stream.
  - `WsWorkerLink::new(url: String)`: one websocket connection per call, request id `1`.
  - `trait WorkerConnector: Send + Sync { fn connect(&self) -> Option<Arc<dyn WorkerLink>>; }`
  - `trait ReadyWorkerUrl: Send + Sync { fn ready_worker_url(&self) -> Option<String>; }`
  - `SupervisorConnector::new(source: Arc<dyn ReadyWorkerUrl>)`, implementing `WorkerConnector` with `WsWorkerLink`.
  - `HarnessManager::ready_worker_url(&self) -> Option<String>`: `ws://127.0.0.1:{catalog.worker_port}` when the current harness is `Ready`, otherwise `None`. Also implement `ReadyWorkerUrl` for `HarnessManager<L, P>`.

- [ ] **Step 1: Write failing tests.** Run the real worker `rpc::serve` with Task 3's scripted `FakeEngine` (make that fake `pub(crate)` under `#[cfg(test)]` so it can be reused) on `127.0.0.1:0`:

```rust
ws_link_generate_yields_notes_in_order // methods == ["turn_started","turn_chunk","turn_done"]
ws_link_model_props_round_trips        // contains "supports_tools"
ready_worker_url_only_when_ready       // manager test: before start → None; after a Ready start → Some("ws://127.0.0.1:8777")
connector_returns_none_without_ready_worker
```

- [ ] **Step 2: Run, expecting FAIL.** **Step 3: Implement.** **Step 4: Run, expecting PASS:** `scripts/hive-test.sh -- runtime::worker supervisor::manager`.
- [ ] **Step 5: Commit:** `feat(runtime): worker link and supervisor connector`.

