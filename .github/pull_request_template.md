## What and why

<!-- One or two sentences: what this change does and the problem it solves. Link the issue if there is one. -->

## How it was tested

<!-- Commands run and what they covered. New behaviour ships with its tests in this PR. -->

- [ ] `cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings` and `cargo nextest run` pass in `src-tauri/`
- [ ] `npx biome ci ./src`, `npx tsc -b` and `npx vitest run` pass
- [ ] Playwright (`npx playwright test --project=chromium`) passes, if the UI changed
- [ ] Ran the app on: <!-- Windows / macOS / Linux x64 / Linux arm64 -->

## Compatibility with OpenAlgo web

- [ ] `/api/v1/*` requests and responses are unchanged, or match the web field for field (golden fixtures in `tests/fixtures/` updated or added)
- [ ] WebSocket feed protocol unchanged, or matches the web
- [ ] Not applicable

## Checklist

- [ ] Works on Windows, macOS, Linux x64 and Linux arm64; no platform-only dependency added
- [ ] Sandbox (analyzer) mode handled: order and account paths short-circuit to the sandbox engine
- [ ] No secrets, API keys, account ids or emails in code, logs, fixtures or screenshots (fixtures use `<APIKEY>`, `<USER_ID>`, `<EMAIL>`)
- [ ] User-facing messages are written for a trader: cause and next action, no status codes or stack traces
- [ ] Resource hygiene checked for new tasks, sockets, caches, channels and subscriptions (bounded, owned, released on the error path)
- [ ] Schema changes are numbered, idempotent migrations tested on a populated database
- [ ] Commits follow Conventional Commits; no emojis
