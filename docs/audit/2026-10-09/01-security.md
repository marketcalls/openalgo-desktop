# 01 - Security review of master (2026-10-09)

Review of `master` at commit `f60dc53`. File and line references point at
that commit. Report only: nothing in this review has been fixed yet.

Scope: the Rust HTTP server and its middleware, browser sessions and CSRF,
sign-in and account recovery, broker sign-in (OAuth `state`, the state-less
fallback and account binding), API key handling, secrets at rest, logging and
redaction, the public webhooks (strategy, Chartink) and the Telegram and
WhatsApp bots, the MCP server (`src-tauri/src/mcp`), the OpenScript runner's
public host routes, rate limits, the market data feed server on 8765, the
Tauri configuration, and dependency status.

How to read it. Each finding has a severity, the code that carries it, a
concrete way to exploit it, and a recommended fix. "Residual" marks a risk
already documented in `docs/security/known-residuals.md`; everything else is
new in this review.

## Summary

| Severity | Count |
| --- | --- |
| Critical | 0 |
| High | 1 |
| Medium | 3 |
| Low | 6 |
| Info | 7 |

| ID | Severity | Title | Status |
| --- | --- | --- | --- |
| S-01 | High | Unauthenticated account wipe and takeover through `POST /auth/reset-account` | New |
| S-02 | Medium | Any web page can lock the trader out of sign-in and lock local API clients out of `/api/v1` | New |
| S-03 | Medium | Behind a tunnel every caller is 127.0.0.1: per-address limits, bans, allowlists and the Remote MCP gate stop working | New |
| S-04 | Medium | Forged state-less broker callbacks for the trader's own account | Residual (documented) |
| S-05 | Low | `POST /setup` is exempt from CSRF and the same-origin check | New |
| S-06 | Low | Webhook secrets are written in plaintext to the traffic log | New |
| S-07 | Low | Reset account leaves MCP tokens, strategy webhooks and Chartink webhooks live | New |
| S-08 | Low | The remote-app capability trusts every port on loopback | New |
| S-09 | Low | Socket.IO connections outlive logout | New |
| S-10 | Low | Feed server accepts any web page and has no failed-key throttle | New |
| S-11 | Info | `pending_oauth` consume is a read then delete, not one statement | New |
| S-12 | Info | A key check racing a key regeneration can cache the old key for 5 minutes | New |
| S-13 | Info | Requests without a `Host` header skip the Host check | New |
| S-14 | Info | `/webhook/` CSRF exemption has no route behind it | New |
| S-15 | Info | The MCP "require approval" setting is stored but has no effect | New |
| S-16 | Info | Session cookie has no `Secure` flag when served through an HTTPS tunnel | New |
| S-17 | Info | Development dependency advisories (npm, dev only) and accepted RustSec advisories | Dependency status |

No Critical issue was found. One High issue (S-01) needs a fix before the
1.0.0 release is announced to traders who expose the app through a tunnel or
on their LAN.

## Findings

### S-01 High: unauthenticated account wipe and takeover through `POST /auth/reset-account`

Code:
- `src-tauri/src/server/routes/mod.rs:101` declares `POST /auth/reset-account` as `Public`.
- `src-tauri/src/server/routes/auth.rs:692-718`: the handler checks only the `Reset` rate limit (15 per hour per address) and that the body says `confirm=RESET`. It then revokes the broker session and calls `AuthService::reset_account`.
- `src-tauri/src/services/auth_service.rs:235-260`: deletes the user, API keys, broker credentials, broker sessions and pending sign-ins, rotates the data key, and clears every browser session.
- `src-tauri/src/server/middleware.rs:262-316`: the CSRF check only needs some browser session and its token, and `GET /auth/csrf-token` (public) hands out both to anyone.
- The frontend never calls this route (no reference under `src/`).

Exploit scenario. A trader exposes the app through ngrok or a Cloudflare
tunnel so TradingView alerts reach `/api/v1/placesmartorder` (the usual
OpenAlgo setup, and what the `host_server` / `ngrok_allow` settings exist
for), or turns on LAN access. Anyone who can reach the port runs:

1. `GET /auth/csrf-token` and keep the `session` cookie and `csrf_token`.
2. `POST /auth/reset-account` with `X-CSRFToken: <token>` and `{"confirm":"RESET"}`.

The account, API key and broker credentials are deleted, the live broker
session is revoked mid-session, and every open sign-in is dropped. Strategies,
OpenScript runs and stops that rely on the broker session stop managing open
positions. Then `POST /setup` (now open again, see S-05) creates an account
with the attacker's password, giving them the UI, a new API key, the MCP admin
page and the trader's strategy, Chartink and log history. The same works from
any other local OS account or local process even with the default loopback
binding. Through a tunnel the per-address limit is shared by everyone (S-03),
so it does not slow this down.

Recommended fix. Do not serve this from the HTTP server at all. Make account
reset a Tauri command (the shell already has `require_user`-style commands in
`src-tauri/src/commands`) or a start-up page action, so only the person at the
desktop window can trigger it. If an HTTP route must stay, require all of:
the request is from a loopback socket address, the `Host` is `127.0.0.1` or
`localhost` (not the tunnel host), `Sec-Fetch-Site: same-origin`, and a
deliberate local confirmation such as a code shown only in the desktop window.
Add the route to a "never reachable through a tunnel" list and cover it with a
test that drives it with a tunnel `Host`.

### S-02 Medium: any web page can lock the trader out of sign-in and lock local API clients out of `/api/v1`

Code:
- `src-tauri/src/server/middleware.rs:377-387` (`login_limited`): 5 per minute and 25 per hour per address, counted on every attempt.
- Callers that count without any same-origin check: `POST /auth/login` (`auth.rs:132`, CSRF-exempt at `middleware.rs:204`), `GET /<broker>/callback` (`broker.rs:109`), `POST /<broker>/callback` for XTS (`broker.rs:142`).
- `src-tauri/src/server/api_v1/mod.rs:145-156` (`authorize`): 10 failed key checks per minute per address and the address is refused without checking the key. `GET /api/v1/ticker/{symbol}` takes `apikey` from the query string (`data.rs:202-232`), so a plain cross-site GET reaches it. `market_calendar.rs:65-76` behaves the same.
- Loopback is not exempt from either bucket, and in the default setup every local client (Python SDK, Amibroker, Excel, the trader's browser) is 127.0.0.1.

Exploit scenario. The trader has OpenAlgo open during market hours and visits
any web page with a few lines of script (an ad, a forum post, a compromised
site). The page fires 25 cross-site form POSTs to
`http://127.0.0.1:5000/auth/login` and, every minute, ten
`<img src="http://127.0.0.1:5000/api/v1/ticker/NSE:SBIN?apikey=x&interval=D&from=2026-01-01&to=2026-01-02">`
requests. No response needs to be readable. Result: the trader cannot sign in
to OpenAlgo for an hour, and every `/api/v1` call from their strategies is
answered "Invalid openalgo apikey" for as long as the page stays open,
including exits and stop orders sent by Amibroker or Python.

A smaller form of the same problem without an attacker: a valid key with no
broker session also counts as a failure (`api_v1/mod.rs:151-154`), so a
strategy polling before the broker login locks itself out for up to a minute
after the login.

Recommended fix. Refuse cross-site browser requests before they count:
answer `Sec-Fetch-Site: cross-site` (and a foreign `Origin`) on `/auth/login`,
broker callbacks that are not top-level navigations, and every `/api/v1` GET
with 403 without touching the limiter. Do not count a valid key without a
broker session as a key failure. Consider keying the `ApiKeyFail` lockout on
the presented key's digest plus address, so wrong keys cannot lock out the
right one.

### S-03 Medium: behind a tunnel every caller is 127.0.0.1

Code:
- `src-tauri/src/server/middleware.rs:31-36` and `:41-56`: the client address is always the socket peer. No forwarded header is read (correct by itself).
- `src-tauri/src/mcp/http.rs:317-333`: Remote MCP is "off" only for non-loopback peers.
- `src-tauri/src/strategy/webhook.rs:343` (`ip_allowed`) and the strategy webhook allowlist.
- `src-tauri/src/db/sqlite/monitor.rs:513-519`: loopback is never banned.
- All per-address buckets in `src-tauri/src/server/ratelimit.rs:47-59`.

Exploit scenario. With ngrok, cloudflared or a reverse proxy on the same
machine, every Internet caller arrives from 127.0.0.1. Then: the Remote MCP
switch does not stop Internet MCP calls (a token is still needed); the
strategy webhook IP allowlist either refuses TradingView or, once the trader
adds 127.0.0.1 to make it work, allows everyone; IP bans never apply; and one
Internet caller can exhaust the shared login, reset, API-key-failure and
webhook-failure buckets for everyone, including the trader (see S-02), for
example ten bad strategy webhook calls a minute block every real TradingView
alert.

Recommended fix. Treat a request whose `Host` is the configured
`host_server` as remote: apply the Remote MCP gate, refuse the account routes
(S-01, S-05), and say on the settings page that per-address limits and
allowlists cannot tell tunnel callers apart. Optionally honour
`X-Forwarded-For` only when the request came from loopback and the `Host` is
the configured tunnel host, and document that.

### S-04 Medium (residual): forged state-less broker callbacks

Code: `src-tauri/src/services/broker_auth_service.rs:198-296`,
`src-tauri/src/db/sqlite/oauth_state.rs:110-145`,
`src-tauri/src/server/middleware.rs:112-118` (`SameSite=Lax`).

Documented in `docs/security/known-residuals.md`. Re-checked: the state-less
path is limited to the brokers that drop `state`, to the newest pending
sign-in of the same browser session within 180 seconds, once; the account
returned must match the configured or last account, and a state-less callback
with no known account is refused. What remains is a forged callback for the
trader's own account, which gains the forger nothing. No change recommended
beyond keeping the table-driven test in `server/tests.rs` current.

### S-05 Low: `POST /setup` is exempt from CSRF and the same-origin check

Code: `src-tauri/src/server/middleware.rs:204` exempts `/setup`;
`src-tauri/src/server/routes/auth.rs:58-91`; the only gate is
`AuthService::setup` refusing when an account exists
(`auth_service.rs:62-67`).

Exploit scenario. Between installation and the trader finishing setup (or
right after S-01), a cross-site form POST from any page the trader has open
creates the account with the attacker's password. The trader then finds
"An account already exists" and has to reset. Through a tunnel or LAN the
attacker can do it directly.

Recommended fix. Remove `/setup` from the exemption and apply the
same-origin check, as for every other write; the setup page can fetch
`/auth/csrf-token` first like the login page does. Refuse setup through the
tunnel host.

### S-06 Low: webhook secrets are written in plaintext to the traffic log

Code: `src-tauri/src/services/monitor.rs:454-466` records the request path
for every request. The paths `/strategy/webhook/{token}` and
`/chartink/webhook/{webhook_id}` carry the whole credential. The strategy
token is otherwise stored only as a digest
(`src-tauri/src/strategy/store.rs:175`, `:393`).

Exploit scenario. A trader shares `logs.db` or a screenshot of the Traffic
page when asking for help, or backs up the data folder to a shared drive.
Anyone reading it can send orders through the strategy webhook.

Recommended fix. Record these paths with the secret segment replaced
(`/strategy/webhook/<redacted>`), and do the same for the 404 tracker.

### S-07 Low: reset account leaves other credentials live

Code: `src-tauri/src/services/auth_service.rs:239-246` deletes users, API
keys, broker credentials, broker sessions and pending sign-ins only.
`mcp_tokens`, the strategy webhook tokens (`sm_strategy`), Chartink webhook
ids and linked Telegram users survive.

Exploit scenario. A trader resets the account because they believe the
machine or a token leaked. AI clients holding MCP tokens and anyone holding a
webhook address keep working against the new account.

Recommended fix. In the same transaction, revoke every MCP token, rotate or
deactivate strategy and Chartink webhooks, and unlink Telegram users, and say
so in the confirmation text.

### S-08 Low: the remote-app capability trusts every port on loopback

Code: `src-tauri/capabilities/remote-app.json:6-8`
(`"urls": ["http://127.0.0.1:*", "http://localhost:*"]`).

Exploit scenario. If the main window is ever navigated to another program's
page on loopback (a link, a redirect from a broker page, a different local
server on another port), that page gets the window permissions and
`shell:allow-open`. The impact is small (window basics and opening http(s)
links) because `withGlobalTauri` is false and the command set is small, but
the trust is wider than needed.

Recommended fix. Narrow the remote URL to the configured port, or check the
origin in an `on_navigation` handler and keep the main window on the app's own
origin.

### S-09 Low: Socket.IO connections outlive logout

Code: `src-tauri/src/server/socketio.rs:13-24` checks the session only at
connect. `auth.rs:324-358` clears sessions and publishes `ForceLogout` but does
not disconnect sockets.

Exploit scenario. On a LAN setup, a second device that was signed in keeps
receiving order, position and analyzer pushes after the trader logs out, if
its page ignores the `force_logout` event.

Recommended fix. On logout, password change and reset, disconnect every
Socket.IO client (`io.disconnect_all` or per-socket disconnect).

### S-10 Low: the feed server accepts any web page and has no failed-key throttle

Code: `src-tauri/src/feed/server.rs:236` (256 connections in all) and
`src-tauri/src/feed/auth.rs:49-82`. There is no `Origin` check and failed
`authenticate` messages are not throttled.

Exploit scenario. A web page opens up to 256 WebSocket connections to
`ws://127.0.0.1:8765` and reopens them as they time out, so Amibroker and SDK
clients cannot connect. Guessing the API key is not practical (the key is
random and the lookup is an HMAC index), so this is availability only. The web
has the same exposure.

Recommended fix. Refuse handshakes whose `Origin` is a browser origin other
than the app's own, and cap connections per address.

### S-11 Info: `pending_oauth` consume is a read then delete

Code: `src-tauri/src/db/sqlite/oauth_state.rs:166-198`. The select and the
delete are separate statements on a pooled connection, so two callbacks with
the same `state` arriving together could both pass. The broker refuses a
reused code, so there is no practical impact. Use
`DELETE ... RETURNING` to make it single-use by construction.

### S-12 Info: key check racing a key regeneration

Code: `src-tauri/src/services/apikey_service.rs:101-121` and `:142-150`. A
check that read the database before a regeneration and stores its result
after the cache was cleared keeps the old key valid for up to 5 minutes. Tag
cache entries with a generation number bumped by `regenerate`.

### S-13 Info: requests without a `Host` header skip the Host check

Code: `src-tauri/src/server/middleware.rs:176-188`. Browsers always send
`Host`, so DNS rebinding is still blocked; only hand-made HTTP/1.0 requests
skip it. Refuse a missing `Host` for consistency.

### S-14 Info: dead CSRF exemption

Code: `src-tauri/src/server/middleware.rs:205` exempts `/webhook/`, which has
no route. Remove it so a future route there is not exempt by accident.

### S-15 Info: "require approval" has no effect

Code: `src-tauri/src/mcp/store.rs:38`, `:271`; `src-tauri/src/mcp/admin.rs:149`.
The setting is saved and shown but nothing reads it (on the web it applies to
OAuth client registration, which the desktop does not have yet). Hide it or
label it "applies when remote sign-in for AI clients arrives".

### S-16 Info: session cookie has no `Secure` flag through an HTTPS tunnel

Code: `src-tauri/src/server/middleware.rs:112-118`. Correct for plain
loopback. When the page is served through the HTTPS tunnel host, add
`Secure`.

### S-17 Dependency status

- `cargo deny --config .github/deny.toml --locked check advisories bans licenses sources`: advisories ok, bans ok, licenses ok, sources ok. 55 duplicate-version warnings (hygiene only). Four advisories are accepted in `.github/deny.toml` (RUSTSEC-2026-0049, -0098, -0099, -0104), all `rustls-webpki 0.102.8` pulled only by `rumqttc 0.25.1` for the IIFL Capital feed; the feed verifies with the patched `rustls-webpki 0.103.15`. Review date 2027-01-31.
- `npm audit --omit=dev`: 0 vulnerabilities in shipped dependencies.
- `npm audit` (all): 4 high, all one chain in development tooling: `braces` (GHSA-vfj7-8cjw-p6xm, deep-nesting denial of service) through `micromatch`, `jest-message-util` and `expect`, pulled by `@types/jest`. Not shipped in the app. Fix with `npm audit fix` or by dropping `@types/jest` if Vitest types are enough.
- Versions of note: `tauri 2.12.1`, `tauri-plugin-shell 2.4.0` (past the 2.2.1 open-scope fix), `axum 0.8.9`, `rustls 0.23.45`, `aes-gcm 0.10.3`, `argon2 0.5.3`, `keyring 3.6.3`.

## Checked and found sound

- **Route access table.** Every session route is declared with its access level in `server/routes/mod.rs` and the guard is applied from the table (`:805-825`); `server/tests.rs:521` pins the exact public list. Apart from S-01 and S-05, each public route is credential-checked in its handler.
- **CSRF.** Mutating requests outside the exempt list need the session's token (header or form field, constant-time compare, `server/form.rs:44`) and pass a same-origin check on `Sec-Fetch-Site` and `Origin` (`middleware.rs:244-260`). The in-app broker login form re-checks both (`broker.rs:158-178`).
- **DNS rebinding and CORS.** `host_check` accepts only loopback names, the configured port, IP literals when LAN is on, and the configured tunnel host. CORS reflects only those origins.
- **Sessions.** 256-bit random ids, HttpOnly, `SameSite=Lax`, bounded store (256), anonymous idle expiry, id rotation on sign-in, all sessions cleared on logout and at the daily boundary. Logout over GET refuses cross-site requests.
- **Sign-in throttling and TOTP.** Login and TOTP share 5 per minute and 25 per hour; TOTP pending state expires after 5 minutes; reset by TOTP is limited to 15 per hour and stores only a hash of the reset token.
- **Broker sign-in.** `state` is single-use, stored only as a hash, bound to the broker, 10-minute expiry, at most 16 pending; manual paste is bound to the same browser session; ready tokens are accepted only from a pasted address, never from a redirect; account binding is applied on every path. Matches `known-residuals.md`.
- **API keys.** HMAC-SHA256 lookup index plus Argon2 verification with a pepper from the keychain; the verification cache is keyed by an HMAC under a per-process random key, bounded (1024) and expires after 5 minutes; the key is read from the JSON body only, as on the web.
- **Secrets at rest.** AES-256-GCM with random 96-bit nonces and associated data naming table, column and row; data key and pepper in the OS keychain, or wrapped by an Argon2id key from the password in `vault.json` (0600) where there is no keychain; data directory 0700 and database files 0600 on Unix; the old XOR `secrets.dat` is migrated and deleted.
- **Logging and redaction.** Release level `info`; the HTTP trace span records method and path only at debug; `reqwest` errors drop the URL before they can be logged (`error.rs:78-84`); order and analyzer logs strip `apikey` (`services/core.rs:130`); MCP audit stores a hash of the arguments, never the arguments; `WebSession` and secret types have redacted `Debug`.
- **Strategy webhook.** Per-address rate and failure lockout and a per-token window before any lookup, token shape check, digest lookup with constant-time compare, optional IP allowlist, 413 before reading an oversize body, payload redacted before storage.
- **Chartink webhook.** UUIDv4 id, locator lookup with constant-time full compare, per-webhook lockout after 10 wrong ids, 16 KB cap, rate limits.
- **Telegram.** Long polling, no inbound webhook. Only `/start`, `/help` and `/link` work before linking; everything else needs a linked user in a private chat, and mode changes re-check the linked key.
- **WhatsApp.** Commands are accepted only from the paired owner's own messages outside groups, with reply caps.
- **MCP.** 256-bit `oamcp_` tokens stored as SHA-256 and compared in constant time; read and write scopes enforced per call, write kill switch, per-token rate limits (60 reads, 5 writes, 120 requests a minute), at most 4 event streams, `Origin` checked, audit row per call. The stdio bridge reads the token only from `OPENALGO_MCP_TOKEN` and never logs it.
- **OpenScript runner host routes.** Each call needs the run's 256-bit secret in the `X-Runner-Token` header (a custom header, so a cross-site page cannot send it without a CORS preflight that is refused), compared in constant time; log lines and messages are capped.
- **Rate limits.** `/api/v1` 100 per second, order routes 10 per second, per address, bounded table with sweeping; 429 bodies match the web.
- **Feed server.** Message and frame size limits, 256 connections, subscription cap per client, authentication timeout with the web's 4401 close.
- **Tauri configuration.** `withGlobalTauri: false`; strict CSP for bundled pages and for every page the server sends (`script-src 'self'`, `frame-ancestors 'none'`, `object-src 'none'`); no `devtools` feature, so devtools are off in release builds; `shell:allow-open` limited to `^https?://`, and the `open_external` command re-checks the scheme and the signed-in user; the default capability for the bundled start-up page is minimal.
