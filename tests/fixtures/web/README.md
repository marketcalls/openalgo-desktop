# Golden fixtures from OpenAlgo web

Real request and response pairs recorded from a running OpenAlgo web instance
on 2026-10-03 (Zerodha connected, analyzer mode), plus WebSocket transcripts.
They define the wire contract the desktop must reproduce; see CLAUDE.md.

- `rest/<endpoint>/<case>.json`: `{request, response}` with status code, headers and body.
- `websocket/*.jsonl`: one message per line with direction and timestamp.
- `INDEX.md`: every file with a note; `conventions.md`: observed rules and quirks.
- `ANALYZER_SESSION.md`: log of every order call made while recording, all in analyzer mode.
- `capture/`: the scripts used. They read the API key from `OA_APIKEY`; never hard-code one.
- `added_after_recording.json`: fields the web added to an answer after it was recorded,
  each with the web change that added it. The desktop sends them; the fixture tests take
  them out of a reply before comparing it with a recording that predates them, and fail
  when a reply lacks one. Remove an entry once its fixtures are recorded again.

Secrets and identities are replaced with `<APIKEY>`, `<USER_ID>` and `<EMAIL>`.
Market data is real and dated; contract tests compare shapes and types, not prices.

Three web defects were observed while recording and are documented in INDEX.md.
The desktop reproduces web shapes, not web bugs.
