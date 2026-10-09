# Known security residuals

Risks that are reduced but not closed, with what bounds them. Each entry
names the code that carries the defence.

## Broker sign-in: forged callbacks (login CSRF)

Every way a broker session is created or replaced, and what protects it
(`services/broker_auth_service.rs`, `db/sqlite/oauth_state.rs`,
`server/routes/broker.rs`, `server/middleware.rs`). The table-driven test
`server::tests::every_way_to_create_a_broker_session_refuses_a_forged_attempt`
tries each one the way a forger would.

| Entry point | Protection |
| --- | --- |
| `GET /<broker>/callback` with `state` | Single-use `state` (only its hash stored), matched to the broker, 10-minute expiry |
| `GET /<broker>/callback` without `state` (dhan, shoonya, zebu, tradesmart, flattrade, arrow, hdfcsky, hdfcsecurities, aliceblue) | Only the newest pending sign-in of that broker started by the same browser session, within 3 minutes, once; all other pending rows of that broker and session dropped with it. Dhan: a repeated `consentAppId` must be the one this sign-in created |
| `POST /<broker>/callback` (compositedge, rmoney `session=`) | Public and CSRF-exempt, so it needs the single-use `state` on its query; without a valid one it is refused |
| `POST /auth/broker/oauth/manual` (pasted address, tradesmart pasted token) | Signed-in user, CSRF token and same-origin check, and a pending sign-in for that broker started by the same browser session (by its `state`, else the newest one) |
| `POST /<broker>/callback` (in-app login form, TOTP brokers) | Signed-in user, CSRF token and same-origin check |
| `POST /<broker>/callback` with no fields (saved-key brokers: fivepaisaxts, ibulls, iifl, jainamxts, wisdom, deltaexchange, dhan_sandbox) | As the login form: signed-in user, CSRF token and same-origin check. The web signs these in on a `GET` of the callback; here that `GET` returns to the broker page and signs in nothing |
| `GET /<broker>/callback` without a query (definedge, nubra: the login OTP is texted as the page opens) | Signed-in user, a same-origin navigation only (`Sec-Fetch-Site` `same-origin` or `none`, else the app's own `Origin` or `Referer`), and the login limit. A cross-site open sends nothing, keeps a pending OTP and uses no login attempt; the page then offers Send OTP, the CSRF-checked `POST` above |
| `GET /<broker>/callback` without a query (redirect brokers; the web's first visit) | Creates no session: for the signed-in user it redirects to `/<broker>/initiate-oauth`, which starts a sign-in as the broker page does; otherwise it is refused like any callback without a pending sign-in |
| Session resume after restart | Only the encrypted stored session of this install, checked with the broker |

On every path, before anything is stored, the account the broker returns
must be the configured one (`catalog::configured_account`: the client id or
the `client_id:::key` prefix, for Dhan and the Noren family; the client id
entered in Profile for Arrow, HDFC Sky, HDFC Securities and AliceBlue), else the
account of the last session with that broker. A mismatch is refused and the
live session and its stored row are left as they were. Saving a broker's
settings again forgets an ended session's account, which is how a trader
switches accounts. A state-less callback with no known account is refused.

Residual: these brokers' redirects do not reliably echo `state`, and the
session cookie is `SameSite=Lax`, so a cross-site top-level GET to the
callback carries it. While the trader has a sign-in pending (3 minutes), a
page could try to complete it with a code for another account; the account
binding refuses that. What remains is a forged callback for the trader's
own account, which gains the forger nothing. Dhan's redirect normally
carries only `tokenId` and its consume call returns no consent id, so there
the account binding (and the trader's own app secret, without which a
`tokenId` cannot be consumed) does the verification. OpenAlgo web has the
same exposure for these brokers: its callbacks do not check `state`.

## Credentials and limits behind a tunnel

Behind a tunnel or proxy on this computer every caller arrives from
loopback and cannot be told apart (`server::middleware::classify`), so they
share one identity. A low limit on that identity would let any stranger
who finds the tunnel address use it up and block the trader's own tunnel
callers, TradingView alerts included. So every surface that takes a random
credential (API keys, MCP tokens, feed keys, strategy webhook tokens,
Chartink webhook ids) works in this order:

1. A resource guard on every request, 1000 a second per caller
   (`Bucket::Guard`), far above legitimate use.
2. This computer and devices on the network: the web's per-address limits
   (`/api/v1` 100 a second, 10 for orders; webhooks 100 a minute) before
   anything else. Nobody elsewhere can use them up.
3. The credential is checked first. It is cheap: an HMAC index lookup that
   misses for an invented API key (Argon2 only on an index hit, then
   cached), a digest lookup for MCP and strategy tokens, a locator lookup
   and a constant-time match for Chartink ids.
4. A valid credential is charged only to its own windows: behind a tunnel,
   its own copy of the per-address limit (`middleware::limiter_key`), plus
   the per-webhook windows. It is never refused because of other callers'
   traffic or failures.
5. An invalid one is charged to the caller's failure budget (ten a minute;
   one for this computer, one shared by every tunnel caller, one per
   network address; `middleware::failures_exhausted`). Once that is spent,
   the caller's further invalid attempts are refused at once, with no
   audit row, probe or log, while valid credentials still pass.

| Concern | What bounds it |
| --- | --- |
| Guessing | The credentials are random: API keys, MCP tokens and strategy webhook tokens 256 bits, Chartink ids random UUIDs (122 bits). Guessing one is not feasible at any request rate; the failure budget bounds noise, not the odds |
| Load from invented credentials | Each costs one cheap lookup, and every request first passes the resource guard |
| Memory | The limiter table holds 8192 entries. Invented credentials get no entry (their failures count per caller); only valid credentials get a window of their own. When the table is full, per-credential entries are evicted first, a live address entry (a lockout or a request window) is never dropped, and new addresses share one overflow bucket, so a full table fails closed |
| Passwords and authenticator codes | Guessable, so never handled this way: sign-in uses only per-source budgets (this computer, the tunnel, each network address with IPv6 grouped by /64; `ratelimit::LoginBackoff`) and the per-address request limit |

Residual: a stranger who finds the tunnel address and sustains 1000
requests a second uses up the resource guard all tunnel callers share,
delaying them while it lasts. This computer and devices on the network keep
their own guard. OpenAlgo web behind the same tunnel sees every caller as
127.0.0.1 and shares every limit among them.
