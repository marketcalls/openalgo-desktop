# Known security residuals

Risks that are reduced but not closed, with what bounds them. Each entry
names the code that carries the defence.

## Broker sign-in callbacks without `state` (login CSRF)

Brokers: dhan, shoonya, zebu, tradesmart, flattrade
(`catalog::callback_carries_state`). Their redirect does not reliably echo
the `state` OpenAlgo sent, so a callback cannot be tied to the sign-in that
started it by `state` alone. The session cookie is `SameSite=Lax`, so a
cross-site top-level GET to `/<broker>/callback?code=...` carries it: while
the trader has a sign-in pending, a page they visit could try to complete it
with a code for someone else's broker account. OpenAlgo web has the same
exposure for these brokers (its callbacks for them do not check `state`).

Defences (`services/broker_auth_service.rs`, `db/sqlite/oauth_state.rs`):

- Account binding: the account a sign-in returns must be the one configured
  for that broker (`catalog::configured_account`: the client id, or the
  `client_id:::key` prefix), else the account of the last session with that
  broker. A state-less callback with neither is refused. A mismatch is
  refused before anything is stored, so a connected session is never
  replaced by a forged callback.
- The fallback only matches the newest pending sign-in of that broker
  started from the same browser session, within 3 minutes of its start,
  once; every other pending sign-in of that broker and session is dropped
  when one is consumed.
- Dhan: the `consentAppId` the sign-in created is recorded with it; a
  callback that repeats a different consent is refused. Dhan's redirect
  normally carries only `tokenId`, and its consume call returns no consent
  id, so in that case the account binding is what verifies it (the
  `tokenId` is also only consumable with the trader's own app id and
  secret).
- A ready token on a callback address (tradesmart `access_token`) is
  accepted only when the signed-in trader pastes the address into OpenAlgo,
  never by redirect.

Residual: for a broker with no configured client id, the first ever
state-less sign-in is refused with a message asking for the client id, and
later ones are bound to the previous session's account. Brokers that echo
`state` are bound by `state`; the account check is not applied to them
because their client-id field means different things per broker (Fyers
stores the app id there).
