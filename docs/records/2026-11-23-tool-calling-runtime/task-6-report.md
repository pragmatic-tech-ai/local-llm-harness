# Task 6 report: worker link and connector

Built backend/src/runtime/worker.rs: Note, GenerateRequest, LinkError, WorkerLink, WsWorkerLink (one socket per call, id 1, lazy connect), WorkerConnector, ReadyWorkerUrl, SupervisorConnector. generate returns a BoxStream owning the socket (drop closes it); error/unparseable frame -> Err item then end; early close just ends the stream. HarnessManager::ready_worker_url (ws://127.0.0.1:{worker_port} only when snapshot state is Ready) plus ReadyWorkerUrl impl.

Tests (worker.rs): generate notes order, model_props, tokenize, connect failure, error frame, connector none. manager.rs: ready_worker_url_only_when_ready (before start None, Ready Some, failed start None).

TDD evidence: honest note - tests and implementation were written together and first run was green on the hive (no separate RED run).

Files: runtime/worker.rs (new), runtime/mod.rs, supervisor/manager.rs, rpc.rs (added test_support::TestServer::spawn; TestClient::connect reuses it).

Full lib on hive: 166 passed. Restored hive clone afterwards.

Self-review: no new free functions; no new crates; LF verified. Concern: classify treats any frame with id 1 as response; fine for one request per socket. Task 7 must treat a stream ending without turn_done/turn_rejected/turn_error as an error.

Reusable for Task 7: rpc::test_support::TestServer::spawn(engine, n_ctx) -> SocketAddr, FakeEngine, and WsWorkerLink::new(format!("ws://{addr}")).

## Fix report (review follow-ups)

Added CONNECT_TIMEOUT 5s, CALL_TIMEOUT 30s, STREAM_IDLE_TIMEOUT 120s as consts with a `with_timeouts` builder; errors "worker connect timed out", "worker did not answer within 30s", "worker idle for 120s". Documented truncation semantics on `WorkerLink::generate`. Reverted unrelated rustfmt reflows in manager.rs. Used `err.get("message")`; commented why a malformed frame aborts `call`.
New tests: call timeout (silent server), stream idle timeout, unparseable frame -> Err, clean close before response ends stream with no Err, dropping stream makes server observe close, error text asserted. Full lib on hive: 171 passed. Note: an abrupt TCP drop (no close handshake) surfaces as an Err item; only a clean close ends the stream silently. LF verified; --restore run.

Minors: timeout messages now render from configured durations (as_secs_f64); raw_server uses generous 5s timeouts and tests set only the timeout under test (idle 300ms). runtime::worker: 11 passed on hive; LF ok; restored.
