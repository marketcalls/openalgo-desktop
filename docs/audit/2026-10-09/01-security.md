# 01 - Security review of master (2026-10-09)

Review of `master` at commit `f60dc53`. File and line references point at
that commit.

**Status (updated 2026-10-09).** Every finding is fixed on `master` except
S-04 (a documented residual) and S-17 (dependency status). The commits are
in the table below and under each finding, with the tests that guard each
fix. Each fix was checked by disabling it locally and watching its tests
fail, then pass with the fix restored; the disabled states were never
committed. What remains after a fix is listed as "Residual" under the
finding.

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
| S-01 | High | Unauthenticated account wipe and takeover through `POST /auth/reset-account` | Fixed in `69ed11e` |
| S-02 | Medium | Any web page can lock the trader out of sign-in and lock local API clients out of `/api/v1` | Fixed in `4e6c801`, `8fb3cef`, `b922cd1` |
| S-03 | Medium | Behind a tunnel every caller is 127.0.0.1: per-address limits, bans, allowlists and the Remote MCP gate stop working | Fixed in `4e6c801`, `b922cd1`, `2697f4b` |
| S-04 | Medium | Forged state-less broker callbacks for the trader's own account | Residual (documented, unchanged) |
| S-05 | Low | `POST /setup` is exempt from CSRF and the same-origin check | Fixed in `4e6c801` |
| S-06 | Low | Webhook secrets are written in plaintext to the traffic log | Fixed in `166cb1d` |
| S-07 | Low | Reset account leaves MCP tokens, strategy webhooks and Chartink webhooks live | Fixed in `69ed11e` |
| S-08 | Low | The remote-app capability trusts every port on loopback | Fixed in `01c1adc`, `1d15e2b` |
| S-09 | Low | Socket.IO connections outlive logout | Fixed in `b542b6e`, `4e6c801` |
| S-10 | Low | Feed server accepts any web page and has no failed-key throttle | Fixed in `5ecabb1`, `8f14c84`, `2697f4b` |
| S-11 | Info | `pending_oauth` consume is a read then delete, not one statement | Fixed in `eed39c3` |
| S-12 | Info | A key check racing a key regeneration can cache the old key for 5 minutes | Fixed in `eed39c3` |
| S-13 | Info | Requests without a `Host` header skip the Host check | Fixed in `b542b6e` |
| S-14 | Info | `/webhook/` CSRF exemption has no route behind it | Fixed in `b542b6e` |
| S-15 | Info | The MCP "require approval" setting is stored but has no effect | Fixed in `eed39c3` (labelled) |
| S-16 | Info | Session cookie has no `Secure` flag when served through an HTTPS tunnel | Fixed in `4e6c801` |
| S-17 | Info | Development dependency advisories (npm, dev only) and accepted RustSec advisories | Dependency status, unchanged |

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

**Status: fixed in `69ed11e`.** The account is reset only from the desktop
window (a Tauri command whose caller must be the main window on the app's
own page); `POST /auth/reset-account` no longer exists. Test:
`server::tests::security::s01_account_reset_is_not_reachable_over_http`.

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

**Status: fixed in `4e6c801`, `8fb3cef` and `b922cd1`.**
- Sign-in (`/auth/login`, `/auth/login/totp`, the authenticator step of the
  password reset, the 2FA settings, the current password on a password
  change) must come from the app's own page: `Sec-Fetch-Site` same-origin or
  none, else an `Origin`, else a `Referer`, naming the app. A request with
  none of them is accepted only from this computer. Anything else is refused
  before anything is counted or checked (`middleware::sign_in_origin_ok`).
- The per-address request limit (5 a minute, 25 an hour) stays, counted per
  address only. On top of it, one failure budget per source (this computer,
  the tunnel, each network address, IPv6 by /64), whatever name is typed:
  5 free failures, then 30 s doubling to a 5-minute cap; a delay, never a
  lockout. The attempt is claimed and counted under the lock that checks
  the wait, before the password is verified, so parallel requests cannot
  slip through (`ratelimit::LoginBackoff`). A name that is not the account's
  is verified against a fixed stand-in Argon2 hash with the same parameters
  and gets the same answer, so neither the response, its timing nor the
  delays tell which names exist.
- `/api/v1`: a cross-site browser request gets the web's 403 "Invalid
  openalgo apikey" before the rate limit or the key is counted; a valid key
  without a broker session is not a failure; this computer is never locked
  out by bad keys. The 100-per-second limit and the 403 body are unchanged.
- Every surface that takes a random credential (API keys, MCP tokens, feed
  keys, strategy and Chartink webhook addresses) checks it first (a cheap
  lookup; Argon2 only on an HMAC index hit). A valid credential is charged
  only to its own windows (behind a tunnel, its own copy of the
  per-address limit) and is never refused by other callers' traffic or
  failures. An invalid one, a key in the URL included, is charged to the
  caller's failure budget (ten a minute; this computer, the tunnel as one,
  each network address); once spent, that caller's further invalid
  attempts are refused at once, with no audit row, probe or log. A
  resource guard of 1000 requests a second per caller covers every
  request. When the limiter's table fills, per-credential windows go
  first and a live address entry is never dropped; new addresses then
  share one overflow bucket (before, a full table dropped every lockout).
  Passwords and codes are never handled this way. The rationale is in
  `docs/security/known-residuals.md`.
- Tests (`server::tests::security`): `s02_cross_site_login_posts_do_not_lock_out_sign_in`,
  `s02_sign_ins_not_from_the_apps_page_are_refused_before_counting`,
  `s02_local_guessing_waits_longer_each_time_up_to_five_minutes`,
  `s02_remote_failures_never_delay_a_local_sign_in`,
  `s02_tunnel_guesses_with_new_names_and_passwords_share_one_budget`,
  `s02_parallel_attempts_cannot_slip_through`,
  `s02_totp_guessing_waits_on_the_same_budget`,
  `s02_unknown_user_names_are_delayed_too`,
  `s02_unknown_names_and_wrong_passwords_are_indistinguishable`,
  `s02_image_requests_with_a_bad_key_do_not_lock_out_local_programs`,
  `s02_a_key_in_the_url_counts_like_one_in_the_body`,
  `s02_bad_keys_never_refuse_a_valid_key`,
  `s02_bad_mcp_tokens_share_the_failure_budget_of_bad_keys`,
  `s02_tunnel_failures_never_refuse_a_valid_mcp_token`,
  `s02_cross_site_requests_do_not_use_up_the_local_rate_limit`,
  `s02_valid_key_without_a_broker_session_is_not_a_failure`,
  `s02_cross_site_callback_images_do_not_use_up_the_sign_in_limit`;
  `server::ratelimit::tests` (including
  `per_credential_windows_never_flush_a_lockout`,
  `a_full_table_fails_closed_into_one_bucket`); `tests/it/feed_app.rs`
  `failed_feed_keys_are_counted_like_api_keys`.
- Residual: a remote caller can delay sign-in from its own source (all
  tunnel callers share one budget, since they cannot be told apart), never
  from this computer. Someone holding many network addresses gets a budget
  per address (IPv6 grouped by /64).

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

**Status: fixed in `4e6c801`, `b922cd1` and `2697f4b`.** The caller is classified once per request by
the outermost layer (`server::source::classify`, stored as `Source`; the
feed's handshake uses the same function) and every
control reads the stored value. A socket peer that is not loopback is
`Lan(ip)` and its headers are never read. A loopback peer is `Local` only
with no forwarding-type header (any `x-forwarded-*`, `Forwarded`, `Via`,
`X-Real-IP`, `CF-Connecting-IP`, `True-Client-IP`, `X-Client-IP`,
`Fastly-Client-IP`, `X-Original-Forwarded-For` and others, matched
case-insensitively, empty or repeated) and a `Host` naming the app's own
loopback address and port; anything else is `Tunnel`. No forwarding header
is ever read as an address: tunnel callers are one shared identity, never
local, never banned and matching no IP allowlist. Bans and allowlists apply
to network peers only, compared as canonical addresses (`::ffff:1.2.3.4` is
`1.2.3.4`, IPv6 in one spelling; logs.db migration 012 rewrites stored
bans), in the one monitor layer every HTTP surface passes and at the feed
server's accept. For webhook addresses, strategy tokens and API keys, tunnel
failures are counted per credential (an HMAC prefix under a per-process
key), so a stranger's bad attempts never block a correct one: the
credential is checked first, a valid one is limited only by its own
windows, and failures count against the caller's budget, which only ever
refuses invalid attempts (see S-02). The user docs
and the strategy webhook page say that through a tunnel the app cannot see
callers' addresses, so allowlists do not apply there. Tests:
`s03_no_forwarding_header_is_read_as_an_address`,
`s03_header_variants_and_foreign_hosts_are_tunnel_requests`,
`s03_every_control_reads_the_stored_source`,
`s03_remote_mcp_switch_applies_to_tunnel_callers`,
`s03_bad_keys_through_a_tunnel_do_not_block_a_valid_key`,
`s03_bad_webhook_calls_through_a_tunnel_do_not_block_a_good_one`,
`s03_bad_chartink_calls_through_a_tunnel_do_not_block_a_good_one`,
`s03_tunnel_failures_never_refuse_a_valid_webhook`,
`s03_spoofed_forwarded_address_does_not_pass_the_webhook_allowlist`,
`s03_the_shared_tunnel_identity_is_never_banned`,
`s03_a_ban_holds_in_every_spelling_of_the_address`,
`s03_a_banned_address_is_refused_on_every_surface`,
`s03_forged_forwarding_headers_never_match_or_escape_a_ban`,
`s03_http_and_the_feed_classify_callers_identically`,
`server::addr::tests`, `db::sqlite::monitor::tests::stored_bans_are_rewritten_in_one_spelling`,
`tests/it/feed_behaviour.rs` `a_refused_address_is_closed_before_the_handshake`.
Residual: a plain port forwarder that adds no header (socat, ssh -R) makes
its callers look local (documented); an IP allowlist no longer matches a
program on this computer.

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

**Status: residual, unchanged.** Documented in
`docs/security/known-residuals.md`.

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

**Status: fixed in `4e6c801`.** `/setup` needs the session's CSRF token and a
same-origin request like every other write, and is refused from anywhere but
this computer (a tunnel, a proxy, another device). Test:
`s05_setup_needs_the_page_token_and_this_computer`; the setup lifecycle test
in `server::tests` now uses the page's token.

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

**Status: fixed in `166cb1d`.** The traffic log and the 404 tracker store
`/strategy/webhook/<redacted>` and `/chartink/webhook/<redacted>`; logs.db
migration 011 scrubs what earlier builds wrote. Tests:
`s06_webhook_secrets_never_reach_the_traffic_log` (end to end),
`db::sqlite::monitor::tests::stored_webhook_secrets_are_redacted_once`,
`services::monitor::tests::webhook_secrets_never_reach_the_traffic_log`.

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

**Status: fixed in `69ed11e`.** The reset revokes MCP tokens, rotates
strategy and Chartink webhook addresses and unlinks Telegram and WhatsApp in
the same transaction. Test: `s07_account_reset_revokes_every_outside_credential`.

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

**Status: fixed in `01c1adc` and `1d15e2b`.** The static `remote-app`
capability is gone; the same permissions are granted at run time to
`http://127.0.0.1:<port>` and `http://localhost:<port>` of the bound port
(plus the Vite dev server in development), never to the bundled page.
Tauri cannot take a run-time grant back, so after a port change the old
grant stays registered; it is unreachable because the main window loads no
page on this computer except exactly 127.0.0.1 or localhost on the live
port, and nothing on this computer while no listener is bound. The live
port is one value, set from the bound socket after a successful bind and 0
before the first bind, after a failed bind and once the listener stops or
dies; the window check and the account reset command fail closed on it.
Tests: `commands::reset_tests::app_permissions_are_scoped_to_the_listening_port`,
`runtime_app_permissions_exist_in_the_acl_manifests`,
`the_app_page_is_exactly_the_live_port_on_loopback`,
`after_a_port_change_only_the_new_port_is_trusted`,
`server::tests::security::s08_the_live_port_is_the_bound_listener_and_fails_closed`.

### S-09 Low: Socket.IO connections outlive logout

Code: `src-tauri/src/server/socketio.rs:13-24` checks the session only at
connect. `auth.rs:324-358` clears sessions and publishes `ForceLogout` but does
not disconnect sockets.

Exploit scenario. On a LAN setup, a second device that was signed in keeps
receiving order, position and analyzer pushes after the trader logs out, if
its page ignores the `force_logout` event.

Recommended fix. On logout, password change and reset, disconnect every
Socket.IO client (`io.disconnect_all` or per-socket disconnect).

**Status: fixed in `b542b6e` (sign-out; test made robust to Engine.IO pings in
`63df1ef`) and completed in `4e6c801`.** Every time a browser session ends
(sign-out, password change or reset, account reset, the daily boundary, id
rotation, eviction) an owned task closes every Socket.IO connection whose
session is no longer signed in; a connection is accepted only for a
signed-in session (the socket is already registered when that is checked,
so a sign-out racing it finds it); and every push re-checks the session of
each connection at the moment of sending. Tests:
`s09_live_update_connection_is_closed_on_sign_out`,
`s09_connections_of_a_rotated_session_are_closed`,
`s09_reset_password_change_and_expiry_close_connections`,
`s09_a_connection_racing_a_sign_out_ends_closed`,
`s09_other_sessions_stay_connected`. The desktop has no "sign out other
sessions" action.

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

**Status: fixed in `5ecabb1`, tightened in `8f14c84`.** The policy is read from
the live settings at every handshake. The `Host` of every upgrade must name
the feed on this computer (127.0.0.1, localhost or [::1] on the feed's
port), one of this machine's own interface addresses while LAN access is
on, or a configured public tunnel host, so a DNS-rebinding page (which
names its own host) is refused with or without an `Origin`. Programs
without an `Origin` (SDK, Amibroker) connect as on the web and still need
the API key. A browser `Origin` must be the app's own page (exactly
127.0.0.1 or localhost on the port the HTTP server is bound to now, an
interface address on that port with LAN access, localhost:5173 in
development), the public tunnel host, or the feed's own address on this
computer (Python websocket-client's default Origin; no page is served
there). One network address may hold at most 32 connections. Tests:
`feed::server::origin_tests::only_the_apps_own_pages_and_programs_are_let_in`,
`lan_access_accepts_this_machines_own_addresses_only`, `tests/it/feed_behaviour.rs`
`s10_browser_pages_from_other_sites_cannot_open_the_feed`,
`s10_the_public_tunnel_host_opens_the_feed`. Failed `authenticate` keys
count against the caller's failure budget `/api/v1` uses, the caller
classified from the handshake by the classifier HTTP requests go through
(`server::source::classify`); once the budget is spent, a failed key still
gets the web's error frame and the connection is closed with 4401. A valid
key is never refused by it, and every `authenticate` passes the resource
guard first (see S-02, `8fb3cef`, `b922cd1`, `2697f4b`). Tests:
`s02_feed_tunnel_failures_never_refuse_a_valid_key`,
`s03_http_and_the_feed_classify_callers_identically`. Residual:
loopback is not capped per address (every local program shares it).
Availability only.

### S-11 Info: `pending_oauth` consume is a read then delete

Code: `src-tauri/src/db/sqlite/oauth_state.rs:166-198`. The select and the
delete are separate statements on a pooled connection, so two callbacks with
the same `state` arriving together could both pass. The broker refuses a
reused code, so there is no practical impact. Use
`DELETE ... RETURNING` to make it single-use by construction.

**Status: fixed in `eed39c3`.** Both consume paths remove and return the row in
one `DELETE ... RETURNING`. Test:
`db::sqlite::oauth_state::tests::a_state_is_single_use_under_concurrent_callbacks`
(four connections behind a barrier, 200 rounds; the old select-then-delete
let three callbacks take one state in the first round).

### S-12 Info: key check racing a key regeneration

Code: `src-tauri/src/services/apikey_service.rs:101-121` and `:142-150`. A
check that read the database before a regeneration and stores its result
after the cache was cleared keeps the old key valid for up to 5 minutes. Tag
cache entries with a generation number bumped by `regenerate`.

**Status: fixed in `eed39c3`.** The key cache carries a generation that
`clear()` bumps; an answer computed before a regeneration is dropped. Test:
`services::apikey_service::tests::a_check_racing_a_regeneration_cannot_cache_the_old_key`.

### S-13 Info: requests without a `Host` header skip the Host check

Code: `src-tauri/src/server/middleware.rs:176-188`. Browsers always send
`Host`, so DNS rebinding is still blocked; only hand-made HTTP/1.0 requests
skip it. Refuse a missing `Host` for consistency.

**Status: fixed in `b542b6e`.** A request over a real connection must name the
host (`Host`, or `:authority` for HTTP/2, checked the same way). Test:
`s13_a_connection_without_a_host_header_is_refused`. The integration test
harnesses now send `Host` like every real client.

### S-14 Info: dead CSRF exemption

Code: `src-tauri/src/server/middleware.rs:205` exempts `/webhook/`, which has
no route. Remove it so a future route there is not exempt by accident.

**Status: fixed in `b542b6e`.** The exemption is removed. Test:
`s14_webhook_prefix_is_not_exempt_from_csrf`.

### S-15 Info: "require approval" has no effect

Code: `src-tauri/src/mcp/store.rs:38`, `:271`; `src-tauri/src/mcp/admin.rs:149`.
The setting is saved and shown but nothing reads it (on the web it applies to
OAuth client registration, which the desktop does not have yet). Hide it or
label it "applies when remote sign-in for AI clients arrives".

**Status: fixed in `eed39c3` (labelled).** The Remote MCP page says the
setting has no effect until AI clients can sign in on their own. Test:
`src/pages/admin/RemoteMcp.test.tsx`.

### S-16 Info: session cookie has no `Secure` flag through an HTTPS tunnel

Code: `src-tauri/src/server/middleware.rs:112-118`. Correct for plain
loopback. When the page is served through the HTTPS tunnel host, add
`Secure`.

**Status: fixed in `4e6c801`.** Through an `https` tunnel host the session cookie
gets `Secure`; plain loopback is unchanged. Test:
`s16_session_cookie_is_secure_through_an_https_tunnel`.

### S-17 Dependency status

- `cargo deny --config .github/deny.toml --locked check advisories bans licenses sources`: advisories ok, bans ok, licenses ok, sources ok. 55 duplicate-version warnings (hygiene only). Four advisories are accepted in `.github/deny.toml` (RUSTSEC-2026-0049, -0098, -0099, -0104), all `rustls-webpki 0.102.8` pulled only by `rumqttc 0.25.1` for the IIFL Capital feed; the feed verifies with the patched `rustls-webpki 0.103.15`. Review date 2027-01-31.
- `npm audit --omit=dev`: 0 vulnerabilities in shipped dependencies.
- `npm audit` (all): 4 high, all one chain in development tooling: `braces` (GHSA-vfj7-8cjw-p6xm, deep-nesting denial of service) through `micromatch`, `jest-message-util` and `expect`, pulled by `@types/jest`. Not shipped in the app. Fix with `npm audit fix` or by dropping `@types/jest` if Vitest types are enough.
- Versions of note: `tauri 2.12.1`, `tauri-plugin-shell 2.4.0` (past the 2.2.1 open-scope fix), `axum 0.8.9`, `rustls 0.23.45`, `aes-gcm 0.10.3`, `argon2 0.5.3`, `keyring 3.6.3`.

**Status: unchanged.** `npm audit --omit=dev` on 2026-10-09: 0 vulnerabilities
in shipped dependencies.

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
