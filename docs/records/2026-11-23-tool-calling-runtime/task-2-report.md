# Task 2 report: Gemma 4 tool-call parser

## Implemented
`backend/src/engine/tool_format.rs` (registered in `engine/mod.rs`): `ParsedCall` (Serialize+Deserialize, `parse_error` default/skip-if-none), `ParseEvent`, `GemmaToolFormat` (`template_supports`, `parse_value`, `parse_call`, private helpers), `GemmaToolParser` (`new/push/content/calls/finished` plus `finish`). Value grammar via a private recursive-descent `ValueReader` (depth cap 64).

## Design decisions
- Streaming: `pending` buffer + `Match::{Full,Partial,None}` classification against delimiter sets. Text mode holds back only a suffix that is a proper prefix of `<|tool_call>`/`<|tool_response>`; as soon as it cannot be a marker it is released. Held-back text is never in `content()`.
- Call end: scanned for `<tool_call|>` OUTSIDE `<|"|>` strings (state `in_string`, resumable `scan_pos`), so `<tool_call|>` inside a string does not end the call (tested whole and char-by-char, with multibyte chars).
- Unterminated call: `calls()` excludes it until `finish()` (end of stream) is called; `finish()` then reports it as a `Call` with `parse_error = "unterminated tool call"` (raw = text so far, name salvaged) and also releases any held-back marker prefix into `content()`. `finish()` is idempotent and a no-op after the response marker. Task 3 should call `finish()` at stream end.
- Malformed calls: `Call` with `parse_error`, `arguments = null`, best-effort `name`; ids still sequential.

## Tests (19 in module)
All brief tests plus: whitespace/quoted keys, garbage rejection incl. depth bomb, string-with-close-marker, held-back prefix then real marker, false prefix released (`"a <|to"` then `"y"` -> `"a <|toy"`), finish releasing held prefix, unterminated call, text before response marker, serde round trip.

## TDD evidence
Deviation: no separate RED run. My first file write was truncated by the shell, and I did not run the brief's module-missing RED step. The first full run was GREEN-ish: `scripts/hive-test.sh -- engine::tool_format` gave 18 passed, 1 failed (`false_marker_prefix_is_released`). My test was wrong: `<|x` is not a prefix of any marker, so it is correctly released immediately. I changed the test to use `<|to` (a real prefix). Then `scripts/hive-test.sh` (whole lib): `98 passed; 0 failed`, TEST_EXIT=0. `--restore` run afterwards.

## Files
- backend/src/engine/tool_format.rs (new)
- backend/src/engine/mod.rs (+`pub mod tool_format;`)

## Concerns
- `<tool_call|>` appearing as stray text in Text mode is treated as literal content.
- Bare object keys run up to the first `:` (per brief), so a bare key containing odd characters is accepted.

## Review follow-ups (fix commit `fix(worker): task 2 review follow-ups`)
1. `feed_chars` free fn moved to associated fns on test struct `TestFeed` (`chars`, `both` = whole vs char-fed equivalence).
2. `<|tool_response>` outside a string in call mode now ends the call implicitly: body parsed normally, Call emitted, then ResponseMarker, parser closed. Tests: `response_marker_inside_call_ends_it_implicitly` (whole and char-fed), `response_marker_inside_string_is_opaque`.
3. Integers above i64::MAX parse as u64 first (`parse_value_accepts_u64_beyond_i64`; beyond u64 falls back to f64).
4. Bare key search bounded at first of `: , { }`; only `:` accepted (`malformed_bare_key_errors`).
5. New `closed` flag set by `finish()` and response marker; later `push`/`finish` return [] and ids stop (`finish_closes_parser_and_ids_stop_advancing`).
6. Added: `unterminated_string_at_finish_reports_error`, `whole_and_char_fed_agree_over_corpus` (10 inputs, incl. response marker after text), `stray_close_marker_in_text_stays_content`, `quote_delimiter_as_last_chunk_before_brace`.

Evidence: `scripts/hive-test.sh` (whole lib) -> `test result: ok. 107 passed; 0 failed`, TEST_EXIT=0; rustfmt --check clean; `--restore` run. (No separate RED run for these follow-ups; tests were added together with the fixes.)
