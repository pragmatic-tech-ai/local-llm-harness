# Task 1 report: HTTPS for the backend

Done. Commit: see `git log -1` on feat/model-providers ("build: reqwest native-tls (Windows SChannel), vendored").

- Cargo.toml: reqwest features gain "native-tls". `cargo vendor vendor` (only cargo use) added 22 crate dirs incl. native-tls, schannel, tokio-native-tls (plus openssl/security-framework etc. for other targets). Cargo.lock updated.
- backend/tests/https_smoke.rs: ignored test `https_get_openrouter_models_list` (reqwest::get, status 200, `data` array); URL/keys are consts.
- scripts/hive-test.sh: tar now always includes backend/Cargo.lock plus vendor files from `git diff --name-only --diff-filter=d origin/main -- backend/vendor` and untracked vendor files (via tar -T); --restore `git clean -fd` now includes backend/vendor.

Hive results:
- `scripts/hive-test.sh -- --tests`: TEST_EXIT=0, offline build OK, all suites pass, smoke test ignored.
- `-- --tests -- --ignored https_get_openrouter_models_list`: `test https_get_openrouter_models_list ... ok` (0.25s, TLS works).
- `--features llamacpp`: 571 passed, 0 failed, TEST_EXIT=0.
- `--restore`: verified the hive clone is clean afterwards (vendor dirs removed).

Concerns: the first two runs happened before the restore-script fix landed (restore initially did not clean vendor; fixed and re-restored). Vendor files compared against local origin/main ref (not fetched). Test file is a free `#[tokio::test]` fn, as in the other tests.
