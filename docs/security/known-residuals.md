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
| `GET /<broker>/initiate-oauth`, and `GET /<broker>/callback` without a query (redirect brokers; the web's first visit) | Creates no session; records a pending sign-in (what a state-less callback completes) only for the signed-in user on a same-origin navigation (`Sec-Fetch-Site` `same-origin` or `none`, else the app's own `Origin` or `Referer`; the app sends `Referrer-Policy: same-origin`). Any other open, another site's included, goes to the broker page and changes nothing: no pending row, none used up |
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

1. A resource guard on every request, this computer's included, 1000
   a second per caller (`Bucket::Guard`), far above legitimate use. Over
   it, a route that checks a credential still serves a valid one and
   refuses an invalid one (429); any other page is served only to a
   signed-in session.
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
| Memory | The limiter table holds 8192 entries. Invented credentials get no entry (their failures count per caller); only valid credentials get a window of their own. A foreign IPv6 device is counted under its /64 (it can take any address in it), so rotating addresses takes one entry; devices in this machine's own prefix are counted one by one. When the table is full, request windows go first (oldest first; they hold no failures), and a failure count inside its window is never dropped. Only when every entry is live do new callers share one bounded overflow entry per kind of limit, which is never evicted: it refuses invalid attempts, while a caller with a valid credential (or a signed-in session, for pages) is still served. This computer and the tunnel identity never use it |
| Passwords and authenticator codes | Guessable, so never handled this way: sign-in uses only per-source budgets (this computer, the tunnel, each network address with IPv6 grouped by /64; `ratelimit::LoginBackoff`) and the per-address request limit |

Availability guarantees, held by one table-driven test across every
surface (`server::tests::security::availability_guarantees_hold_on_every_surface`):
this computer (a loopback peer; only loopback ever is) with a valid
credential or session is never refused by a ban, budget, overflow or
monitor rule; a valid credential from any source is never refused because
of other callers; bans and automatic bans apply only to devices on the
network, keyed as they are counted, never to this computer, the tunnel
identity or exactly one of this machine's own addresses, are capped (10,000
automatic bans) and expire; invalid
traffic stays bounded; sign-in keeps its per-source budget. The web's
per-address request limits on `/api/v1` (100 a second) still apply to
this computer, as on the web. The market data feed keeps a pool of 64
connections for this computer's programs, apart from `max_connections`
for every other caller, so devices on the network and tunnel callers can
never fill it. Before the handshake, loopback connections (this computer
and the tunnel, not yet told apart) share a pending pool; a flood of slow
handshakes through the public tunnel could fill it and delay a local
client, but each holds its place for at most the 3-second loopback
handshake deadline, and the local client succeeds on retry. After the
handshake this computer has its reserved pool.

Residual: a stranger who finds the tunnel address can use up the guard all
tunnel callers share. Callers with a valid credential or a signed-in
session are still served; only pages for callers who are not signed in
(the sign-in page through the tunnel) are refused while it lasts. OpenAlgo
web behind the same tunnel sees every caller as 127.0.0.1 and shares every
limit among them.

Keying: an IPv4 device by its address; an IPv6 device inside a prefix
this machine's own interfaces hold, or a link-local one, by its own
address (every device on the trader's network shares that prefix), plus
an aggregate for that whole /64 (all of link-local, `fe80::/10`, as one)
at five times one device's limit, in every limit the app keeps: the
limiter charges both tiers itself (`ratelimit::RateLimiter::admit`), so
no surface can charge one only, and the aggregate does not depend on the
list of interfaces; any other IPv6 device by its /64, which one device can
rotate through (an address whose prefix is not known as ours falls back to
this, after the interfaces are read again). Bans inside
our own prefix are per address; the prefix itself is never banned, and
when ten or more of its devices are banned the trader gets a health
alert instead. This machine's addresses are read again at most every 30
seconds, so an address it gives up stops being treated as its own.

Residual: a foreign IPv6 /64 is one identity, so devices behind another
network's /64 share one budget, and an automatic ban of one covers the
/64.

Residual: inside this machine's own IPv6 /64 (and link-local), bans and
limits are per address, so a device on the LAN rotating its address
escapes a ban on its old one and gets fresh per-address windows. It stays
bounded: every limit also charges the /64's aggregate (five times one
device's limit: failure budgets, request windows, the resource guard,
sign-in windows and feed connections), and it still cannot use an invalid
credential; shared windows such as the aggregate refuse only invalid
credentials, so it cannot get a valid neighbour refused. Ten or more
banned addresses in one such /64 raise a health alert for the trader. It
needs a device on the trader's network with LAN access turned on, which is
off by default. The test
`server::tests::security::s02_every_surface_charges_our_networks_aggregate`
proves the aggregate on every surface, and
`s02_rotating_through_our_own_ipv6_network_hits_the_aggregate_cap` that a
neighbour with a valid key is still served once it is spent. When the
automatic ban list is full (10,000 bans, each expiring), no ban in force
is lifted to make room: new bans are refused and the trader gets a health
alert, while the per-/64 aggregate still caps the traffic and valid
credentials are never refused. All of this needs a device on the
trader's network with LAN access turned on.

Residual: an IPv4 device on the network is counted per address. Rotating
IPv4 addresses on a LAN takes DHCP or ARP abuse, which the network itself
would show; each address it takes still has its own budgets, and the
limiter's table bound and overflow still cap the total.
