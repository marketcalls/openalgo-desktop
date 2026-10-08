---
name: security-audit
description: Run OpenAlgo Desktop's periodic security review and write it up as a dated markdown report under docs/audit/<date>/. Use for the monthly review, before a release or a v* tag, after a dependency bump (Cargo.lock, package-lock.json), after adding a public route, webhook, broker callback, Tauri capability or secret store, or when asked for a security check, vulnerability scan, dependency audit or audit report.
---

# Security audit (desktop)

Threat model: single user, local machine. Whoever controls the OS account
controls the app. The job is to keep secrets off disk in usable form, out of
logs, and away from anything that is not the signed-in user: another local
process, a web page in the trader's browser, a LAN peer when the trader binds
beyond loopback, a forged broker callback. Weight triage accordingly; IDOR
and role bypass do not apply.

Apply the `verify` skill throughout: every control below is checked by
running it (a test, a request, a scanner), never by reading alone, and a tool
that did not run is reported as not run.

## 1. Dependency and secret scanners

Run each, record the command, the tool version and the result. CI runs the
same set in `.github/workflows/security.yml` (weekly and on lockfile
changes), `ci.yml` (gitleaks on every push) and `codeql.yml`.

```bash
# Rust advisories, licenses, bans, sources (the single place advisories are accepted)
cargo deny --manifest-path src-tauri/Cargo.toml --config .github/deny.toml --locked \
  check advisories bans licenses sources

# RustSec, with the ignores taken from deny.toml (exactly what security.yml does)
ignore_args=(); while IFS= read -r id; do ignore_args+=(--ignore "$id"); done \
  < <(grep -vE '^[[:space:]]*#' .github/deny.toml | grep -oE 'RUSTSEC-[0-9]{4}-[0-9]{4}' | sort -u)
cargo audit --file src-tauri/Cargo.lock "${ignore_args[@]}"

# JavaScript: production dependencies, fail on high or critical
npm audit --omit=dev --audit-level=high
npm audit --omit=dev --json > /tmp/npm-audit.json   # triage moderate and low too

# Secrets in the full history, and in a branch's own commits
gitleaks git --no-banner -c .github/gitleaks.toml --redact .
gitleaks git --no-banner -c .github/gitleaks.toml --log-opts="origin/master..HEAD" .

# Filesystem: lockfile vulnerabilities and secrets, with the accepted list
trivy fs --scanners vuln,secret --severity CRITICAL,HIGH,MEDIUM \
  --skip-dirs node_modules,src-tauri/target,dist --ignorefile .trivyignore .

# CodeQL (rust, javascript-typescript, actions): read the open alerts
gh api "repos/marketcalls/openalgo-desktop/code-scanning/alerts?state=open&per_page=100" \
  --jq '.[] | [.rule.security_severity_level, .rule.id, .most_recent_instance.location.path, .most_recent_instance.location.start_line] | @tsv'
```

`cargo-deny` and `gitleaks` are installed on the maintainer's Mac;
`cargo-audit` and `trivy` may not be. If one is missing, install it or read
the latest CI run's artifact (`gh run list --workflow security.yml`, then
`gh run download <id>`), and say which you did. An unrun scanner is a gap in
the report, not a pass. CodeQL config: `.github/codeql/codeql-config.yml`
excludes `src-tauri/tests/**`, `*.test.ts(x)` and `e2e/**`; an alert inside a
`#[cfg(test)]` module in `src-tauri/src` is dismissed as "used in tests" only
after checking it really sits in a test module.

## 2. The attack surface, control by control

For each, name the code, run the test that proves it, and try one hostile
request yourself against a test server (never the trader's running app, never
ports 5000/8765/5500/8766 on this machine; the integration harnesses bind
ephemeral ports).

| Surface | Control | Code | Proof to run |
| --- | --- | --- | --- |
| `/api/v1/*` | API key in the JSON body (the `X-API-KEY` header alone is a 400, as on the web); HMAC index plus Argon2 with a short cache; 403 on a bad key; failures throttled per IP; 100/s per IP | `server/api_v1/`, `services/apikey_service.rs`, `security/hashing.rs`, `server/ratelimit.rs` | `cargo test --lib server::tests::bad_api_keys_are_throttled_per_ip`, `api_rate_limit_is_100_per_second_per_ip_with_flask_body`; `tests/it/api_v1_contract.rs` |
| `/api/v1/telegram/notify`, `/api/v1/whatsapp/notify` | the only endpoints that also read `X-API-KEY` (web parity) | `server/api_v1/notify.rs` | the notify tests in `tests/it/messaging.rs` |
| Browser session routes | every route declared in `server/routes/mod.rs` `table()` with an `Access` level; `require_user` / `require_user_for_json`; cookie `HttpOnly`, `SameSite=Lax`; CSRF token (`X-CSRFToken` or form `csrf_token`) plus `sec-fetch-site` same-origin check on every write; `csrf_exempt` list in `server/middleware.rs` | `server/middleware.rs`, `server/form.rs`, `session/web.rs` | `every_user_route_rejects_without_a_signed_in_session`, `public_route_list_is_exactly_the_reviewed_one`, the CSRF tests in `server/tests.rs` |
| Broker callbacks `/{broker}/callback`, `/{broker}/initiate-oauth`, `/auth/broker/oauth/manual` | single-use hashed `state` (10 min), state-less brokers matched to the same browser session (3 min), account binding (`catalog::configured_account`), XTS POST callback needs `state` | `services/broker_auth_service.rs`, `db/sqlite/oauth_state.rs`, `server/routes/broker.rs`, `brokers/catalog.rs` | `every_way_to_create_a_broker_session_refuses_a_forged_attempt`; the table in `docs/security/known-residuals.md` must match it |
| Strategy webhook `POST /strategy/webhook/{token}` | the URL token is the credential; rate limit and lockout | `strategy/webhook.rs`, `server/ratelimit.rs` | `cargo test --locked --test strategy webhook` (`src-tauri/tests/strategy.rs`) |
| Chartink webhook `POST /chartink/webhook/{webhook_id}` | constant-time id compare, per-locator rate limit, lockout after repeated wrong addresses | `chartink/webhook.rs` | `tests/it/chartink.rs` |
| Telegram, WhatsApp | no inbound HTTP: Telegram long-polls (the webhook is deleted), WhatsApp is a client session; the Telegram bot's `gate` (`messaging/telegram/bot.rs`) answers only the linked user in a private chat | `messaging/telegram/`, `messaging/whatsapp/` | `cargo test --lib messaging::telegram`, `tests/it/messaging.rs` |
| `/mcp` (HTTP) and the `mcp` subcommand | bearer `oamcp_` token, only its SHA-256 stored, shown once; scopes; per-token rate limit; `OPENALGO_MCP_TOKEN` read by the subcommand only, never logged or on the command line | `mcp/http.rs`, `mcp/store.rs`, `mcp/stdio.rs` | `tests/it/mcp_security.rs`, `mcp_subcommand.rs` |
| OpenScript runner host `/openscript/runner/host/{run}/*` | each call carries its run's secret (handed to the runner page in its URL fragment) | `server/routes/openscript_runner.rs`, `trading/runner/` | `tests/it/trading_runner.rs` |
| Custom indicators `/custom-indicators/*` | signed-in user, `.js` names only, regular files inside the folder (no symlink escape), 4 MiB cap; the module runs with the app's privileges by design | `server/routes/custom_indicators.rs`, `trading/indicators.rs` | `cargo test --lib trading::indicators`, `tests/it/trading_files.rs` |
| Feed server `ws://127.0.0.1:8765` | `authenticate` with the API key (constant-time), 4401 close on auth timeout, nothing but `ping` before auth | `feed/auth.rs`, `feed/server.rs` | `tests/it/feed_conformance.rs`, `feed_behaviour.rs` |
| Bind host and LAN toggle | loopback by default; binding beyond it is an explicit setting | `config.rs`, `server/mod.rs` | settings tests; confirm with `lsof -iTCP -sTCP:LISTEN -P` on a test instance |
| Tauri shell | CSP in `src-tauri/tauri.conf.json` (`default-src 'self'`, `frame-ancestors 'none'`, `object-src 'none'`), `withGlobalTauri: false`; capabilities `src-tauri/capabilities/default.json` (bundled start page) and `remote-app.json` (the local server's UI, `http://127.0.0.1:*` and `http://localhost:*` only: window basics and `shell:allow-open`); devtools off in release | `tauri.conf.json`, `capabilities/` | read the diff of both files since the last audit; any new permission needs a reason |

When a route is added as public, `public_route_list_is_exactly_the_reviewed_one`
fails until the list in `server/tests.rs` is updated; the review of that
change is part of this audit.

## 3. Secrets at rest, in logs, on the wire

- **Keys.** The data-encryption key and the API-key pepper live in the OS
  keychain (`security/keystore.rs`, service `com.openalgo.desktop`); with no
  keychain (headless Linux, some Raspberry Pi setups) a password-derived key,
  announced in the UI. Never a hard-coded or obfuscated key.
- **Ciphertext.** Broker credentials and tokens are AES-256-GCM with
  associated data binding each ciphertext to its row and column
  (`security/crypto.rs` `encrypt(plaintext, &Aad)`). Check a ciphertext
  copied to another row or column fails to decrypt (the crypto tests).
- **Nothing returns a stored secret.** No endpoint or Tauri command returns
  a saved broker secret, password or API key in plaintext
  (`/auth/broker-config` returns `broker_api_key: null`).
- **Logs.** `Secret` (`security/secret.rs`) prints `[REDACTED]`; broker
  errors go through `brokers::common::redact`; request bodies stored in
  `logs.db` go through `db::sqlite::logs::redact`; client-reported URLs
  through `services::error_log::sanitize_url`; `lib.rs::log_filter` pins
  `tungstenite` at info. Drive a failing login and a feed reconnect with
  sentinel credentials and scan the output and a test data directory's
  `logs.db` with `.claude/skills/verify/leak_scan.py`.
- **Files.** Database files are owner-only (`security/fsperm.rs`
  `restrict_db_files`).
- **Fixtures.** Only `<APIKEY>`, `<USER_ID>`, `<EMAIL>` placeholders; gitleaks
  enforces it.

## 4. Accepting a residual

A finding that is reduced but not closed, or a scanner hit that is
unreachable, is accepted in writing, never by silence:

- **Code-level residual** (a control with a known gap): an entry in
  `docs/security/known-residuals.md` naming the entry point, the protection,
  the code that carries it, the test that proves it, and what remains.
- **Rust advisory**: an `ignore` entry in `.github/deny.toml` `[advisories]`
  with `{ id = "RUSTSEC-...", reason = "<why unreachable>; review by YYYY-MM-DD" }`.
  `security.yml` takes the cargo-audit ignores from that file, so it is the
  single place.
- **Trivy finding**: the id in `.trivyignore` with a comment above it saying
  why it is accepted and what would let it be removed, plus a review date.
- **CodeQL alert**: dismissed in GitHub with the reason, only after the check
  in section 1.
- **npm advisory** in a production dependency at high or critical: not
  accepted; upgrade or replace. Moderate and low: list them in the report.

Every review date that has passed is a finding in the next audit.

## 5. The report

Write `docs/audit/<YYYY-MM-DD>/security.md` (one file; add more only for a
large audit, following `docs/audit/2026-10-03/` and its `README.md`):

```markdown
# Security audit <YYYY-MM-DD>

Commit: <sha>. Scope: <what changed since the last audit>. Auditor: <who>.

## Summary
<counts by severity; what is fixed, what is accepted, what is open>

## Scanners
| Tool | Version | Command | Result |
<one row each: cargo deny, cargo audit, npm audit, gitleaks, trivy, CodeQL; "not run" with the reason>

## Findings
### <ID>. <title> (<Critical|High|Medium|Low>)
- Where: <file:line>
- What an attacker needs and gets: <one paragraph>
- Evidence: <the command or test that showed it; output trimmed>
- Fix: <change, with the test that fails without it> | Accepted: <link to the residual entry>

## Controls verified
<surface -> test run -> result, for the table in section 2>

## Residuals reviewed
<each entry in known-residuals.md, deny.toml and .trivyignore: still valid? review date?>
```

Severity: what an attacker on this machine, on the LAN (when bound beyond
loopback) or behind a web page the trader visits can do with it. A secret in
a log or a forged session is high; a missing header on a loopback-only route
is low.

**Hold the report off master until its findings are fixed.** Commit it on a
branch (`audit/<date>`), fix the findings there or on their own branches,
and merge the report with the fixes (`docs/audit/2026-10-03/README.md`: "The
security report is held back until its findings are fixed."). A published
open finding in a public repository is a how-to.
