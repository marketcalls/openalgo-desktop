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
