# No OS keychain: the key vault in openalgo.db (KEY-01)

Maintainer decision, 2026-10-13. Binding for macOS, Windows and Linux alike.

## Why

Earlier builds kept the data-encryption key and the API-key pepper in the
operating system's keychain (macOS Keychain, Windows Credential Manager,
Linux Secret Service), falling back to a password-derived key in
`vault.json` only where no keychain existed. Unsigned builds change their
code identity on every update, so macOS asked for keychain access again
after each one, and a refused prompt locked the trader out of the account
("OpenAlgo cannot open your system keychain..."). This is finding KEY-01.

## The decisions

1. Every platform behaves the same: no keychain for new installs, and no
   keychain use after the one-time move of an existing install.
2. The keys are protected by the trader's OpenAlgo login password
   (Argon2id). The wrapped key material lives in `openalgo.db`, not in a
   separate file and not in a keychain. A copied database is useless
   without the password.
3. The keys are unlocked at sign-in and kept, with the values decrypted
   with them, in memory until logout or until the app quits. Then they are
   wiped (zeroized) and every cache is cleared.
4. Caching follows the web (`database/auth_db.py`,
   `utils/thread_safe_cache.py`): every cache is bounded with a TTL;
   readers fill through the generation they read before their query;
   writers clear after their commit.
5. Existing keychain installs move their keys once, at the first sign-in
   with the new build. Existing password-mode installs move `vault.json`
   the same way. Every step is crash-safe.

Accepted consequences, explained to the trader on the sign-in page, in
README and in `docs/user/troubleshooting.md`:

- Until the trader signs in after OpenAlgo starts (and after a logout),
  `/api/v1`, the market data feed, webhooks, AI clients (MCP) and the broker
  session are unavailable.
- A forgotten password cannot be recovered. Setting a new one keeps the
  account, settings and history; broker keys are entered again, a new API
  key is issued, and two-factor login (TOTP) is set up again.

## The vault

Migration `082_key_vault` (`src-tauri/src/db/sqlite/key_vault.rs`):

```sql
CREATE TABLE key_vault (
    id INTEGER PRIMARY KEY CHECK (id = 1),   -- one row
    version INTEGER NOT NULL,                -- 1
    kdf TEXT NOT NULL,                       -- 'argon2id-m19456-t2-p1'
    salt TEXT NOT NULL,                      -- base64, 16 random bytes
    nonce TEXT NOT NULL,                     -- base64, 12 bytes
    ciphertext TEXT NOT NULL,                -- AES-256-GCM(data key || pepper)
    source TEXT NOT NULL,                    -- setup | keychain | vault_file | legacy_file | fresh
    keychain_cleanup TEXT NOT NULL DEFAULT 'none',  -- none | pending | done | failed | abandoned
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

- Key encryption key: Argon2id(password, salt) with m = 19 MiB, t = 2,
  p = 1, the parameters of the password hash. It is zeroized after use.
- Wrapped: the 32-byte data key followed by the 32-byte pepper, sealed with
  AES-256-GCM under associated data `openalgo:v1:vault` (the same as the
  retired `vault.json`, so the two formats wrap identically).
- A new salt on every wrap. A damaged row (unknown version or KDF, bad
  base64, wrong length) is an error, never read as a wrong password.
- `source` and `keychain_cleanup` are bookkeeping, not secrets: where the
  keys came from, and whether old keychain entries are still to be deleted.

The password hash (`users.password_hash`, Argon2id with the pepper) sits in
the same database. A password change rewrites the hash and the wrapping in
**one SQLite transaction**, so no state exists in which one changed and
the other did not. The file staging of the old password mode
(`vault.next.json`, promote, roll back, settle at sign-in) is gone.

What is encrypted with the data key (AES-256-GCM, associated data bound to
table, column and row): the TOTP secret, the OpenAlgo API key (for the API
Key page), saved broker keys, stored broker sessions, the Telegram bot token
and linked users' API keys, and the WhatsApp session. The API key's lookup
index and hash, and the password hash, use the pepper. MCP tokens and
webhook addresses are SHA-256 digests and need no key.

## Lock and unlock

| Moment | Keys in memory | What happens |
| --- | --- | --- |
| App start | no | No keychain call, no decryption. The database is opened and migrated; the vault row is not opened. |
| Sign-in, password step | staged, not installed | The install's key state is read first, without the name or the password (see "What a sign-in tells"). The vault is unwrapped with the typed password and the stored hash is checked with the pepper it carries. A failure installs nothing, and every way out costs the same two Argon2 runs. With TOTP required the unwrapped keys wait in the pending sign-in (a 5-minute browser session) and are used only to check the code; the session poll drops them once the 5 minutes are over. |
| Sign-in completes (session issued) | yes | Installed in the same step that issues the signed-in session (`issue_if_current`, so a password change racing the sign-in voids both). Then the stored broker session resumes, and Telegram and WhatsApp start if they were on. |
| Account setup | no | New keys, the vault, the account and its API key in one transaction, then the keys leave memory: the setup page sends the trader to sign in, and the keys come back with that session like every other time. (Only the test harness, which opens unlocked, keeps its own.) |
| Second device signs in | yes (unchanged) | The password still unwraps the vault; the keys in memory are the same. |
| Logout | wiped | Every browser session ends; the broker session is revoked (memory first); the keys are dropped (the raw key and pepper are zeroed when the last copy goes, the AES key schedule too with `aes/zeroize`) together with every browser session, under the lock a sign-in issues its session under (`lock_with`), so no session issued during the revoke outlives its keys; Telegram and WhatsApp end as at an app exit, keeping their "on" setting, and come back at the next sign-in; every cache is cleared after the lock, and `on_logout` runs on every broker adapter, not only the logged-in one. The lock bumps the credentials generation, so a sign-in that was being checked when the logout landed gets no session. |
| App quit | wiped | Process exit; the shutdown path runs the same lock. |
| Password change | yes (unchanged) | Hash and wrapping in one transaction; every browser session ends (as before) and every cache is cleared. The keys do not change, so they stay. |
| Forgotten password (Set a new password) | no | New keys, new wrapping, new hash, encrypted values dropped, in one transaction. The trader signs in with the new password. |
| Account reset | no | As before, plus the vault row is deleted in the same transaction. |
| 03:00 IST boundary | yes | As before: the broker session ends and every browser session ends. The keys are not wiped: the maintainer's rule is logout or quit. A trader who wants the API off overnight logs out. Recorded as a deliberate choice. |

While locked:

| Surface | Answer |
| --- | --- |
| `/api/v1/*` | 503 with the web's envelope: `{"status": "error", "message": "Sign in to OpenAlgo to use the API. The API is off after OpenAlgo starts and after you log out, until you sign in again."}`. Checked in the `/api/v1` layer after the rate limit and before any key check; nothing counts against the caller. Deviation from the web, which has no locked state: a distinct 503 tells a client the key was not even checked (a wrong key stays 403). |
| `/mcp` (HTTP) | The token is checked first (a digest lookup that needs no key), then 503 `{"error": "locked", "error_description": ...}`. Open event streams end at the next keepalive. |
| MCP stdio (`openalgo-desktop mcp`) | The forwarder shows the same sentence as the tool's answer (`isError`), never "the answer was lost". |
| Market data feed (`authenticate`) | The web's error frame, `AUTHENTICATION_ERROR`, with the locked sentence; the socket stays unauthenticated; nothing counts. |
| Strategy and Chartink webhooks | The address is checked first; a valid one gets 503 with the locked sentence (in each webhook's own error shape). Nothing is placed. |
| Broker resume | Runs only after a password sign-in has installed the keys; before that the stored session cannot be decrypted and nothing resumes. |
| Telegram and WhatsApp bots | Not started until sign-in; stopped at logout. |

## Migration of existing installs

The state at start-up is read from the folder and the database, never from
the keychain:

```
                       +-------------------------------+
                       | users row?                    |
                       +-------------------------------+
                         | no                   | yes
                         v                      v
                    NoAccount            key_vault row?
                 (setup makes the        | yes      | no
                  vault; 0 keychain      v          v
                  calls)              Vault     vault.json or vault.next.json?
                                                 | yes             | no
                                                 v                 v
                                          FromVaultFile     secrets.dat?
                                                            | yes        | no
                                                            v            v
                                                     FromLegacyFile  FromKeychain
```

Each `From*` state becomes `Vault` at the first sign-in in the OpenAlgo
Desktop window that succeeds. A sign-in from anywhere else (a browser or a
program on this computer, another device, a tunnel) moves nothing and reads
no keychain: it gets one fixed answer, "Sign in once in the OpenAlgo Desktop
window...", whatever name and password it sent.

1. **Read the old keys.** `FromVaultFile`: check `vault.json` (then
   `vault.next.json`, a password change an older build left half done)
   without a password, then open them with the typed password.
   `FromLegacyFile`: decode `secrets.dat`. `FromKeychain`: read `data-key`
   and `api-key-pepper` from the keychain: the one last prompt. It is read
   at every such sign-in in the window whatever name was typed, so whether
   the prompt appears tells nothing about the name.
2. **Prove them.** Keys from `secrets.dat` or the keychain must open the
   first encrypted row (the TOTP secret, else the API key, else broker
   keys, else a broker session) before any password is looked at: keys that
   open no row are another install's (see failures), the same for every
   name and password. Then the stored password hash must verify with the
   pepper. A wrong password installs nothing.
3. **Copy into the database.** One `BEGIN IMMEDIATE` transaction inserts the
   vault row (wrapped with the typed password; `source` says where from;
   for the keychain `keychain_cleanup = 'pending'`), reads it back inside
   the transaction and unwraps and decrypts the witness again, then commits
   with `synchronous = FULL` so the commit is on disk before step 5.
4. **Verify after the commit.** A new connection reads the row, unwraps it
   and decrypts the witness.
5. **Delete the source.** `vault.json` and `vault.next.json`, or
   `secrets.dat` (after the AAD re-encryption it carries), are removed and
   the folder synced. Keychain entries are deleted once and
   `keychain_cleanup` becomes `done`; if the keychain refuses, `failed`, and
   it is never touched again (the troubleshooting page says how to remove
   them by hand).

Crash at every step:

| Stopped after | Left behind | Next sign-in |
| --- | --- | --- |
| 1 or 2 | the old source, no row | starts over from step 1 |
| 3 before COMMIT | the old source, no row (rolled back) | starts over from step 1 |
| 3 (committed) or 4 | row and old source | `Vault`: unwraps the row; the cleanup of step 5 runs then. The keychain is not read. |
| 5, keychain deletion not reached | row with `pending` | `Vault`; the deletion is attempted once, then `done` or `failed` |
| 5 done | row | `Vault` |

The source is never deleted before the row is committed, on disk and
verified. The keychain is never read once a row exists.

## Failures

| Situation | What the trader sees | Choices |
| --- | --- | --- |
| Keychain locked or access refused (`FromKeychain`) | "OpenAlgo needs to read your keys from the system keychain one last time to move them into its own database, and the keychain refused or is locked." | **Allow the keychain once and retry** (signs in again with what was typed; choose Allow when the system asks). **Start fresh**. |
| Keychain has no entries, or entries that open no row (a folder from another computer, a reset) | "The keys for this account are not in this computer's keychain." | **Start fresh** (or restore the folder on the computer it came from). |
| Wrong password | "Invalid credentials", as before | - |
| Any of these, from anywhere but the OpenAlgo Desktop window | "Invalid credentials" (the fixed "Sign in once in the OpenAlgo Desktop window" while keys still have to move): the same for every name and password | Sign in in the desktop window |
| Damaged vault row or unreadable `vault.json` | "OpenAlgo could not read the keys saved with your account." | **Start fresh** |
| A write fails during a move | The sign-in fails with "try again"; the source is untouched | retry |
| Forgotten password | Forgot password page | **Set a new password** (same operation as Start fresh) or Reset account |

**Start fresh** and **Set a new password** are one operation
(`AuthService::start_fresh`), allowed only from the OpenAlgo Desktop window
on this computer after a native confirmation (a Tauri command, like Reset
account), never over HTTP: nothing reaching the server over the network can
replace the password. In one transaction: new random keys; the vault
wrapped with the new password (`source = fresh`; `keychain_cleanup =
abandoned` when the keychain was unreadable, so it is never touched); the
new password hash; a new API key (the order mode is kept); a new TOTP
secret with every "require the authenticator" switch off; and every
encrypted value dropped (`key_vault::drop_encrypted_values`): saved broker
keys (the broker id, client id and redirect URL stay), stored broker
sessions, pending broker sign-ins, the Telegram bot token and linked users'
saved keys, the WhatsApp pairing. Kept: the account (name and email),
settings, strategies, webhook and Chartink addresses, AI client tokens,
watchlists, logs, history and sandbox mode, none of which is encrypted.
Every browser session ends and every cache is cleared. Then the new vault
is unwrapped and the new TOTP secret decrypted (verification), and old
`vault.json`, `vault.next.json` and `secrets.dat` are deleted.

The TOTP reset of the web cannot work here: the TOTP secret is encrypted
with the data key, which is locked while nobody is signed in. The
`/auth/reset-password` route is retired with the web page it served.

## What a sign-in tells (security review)

A background review of the first Phase B push found that a sign-in could
tell a right password from a wrong one. Every failure is now built so that
it cannot:

- **The app window.** The local server cannot tell the OpenAlgo Desktop
  window from a browser tab or a program on the same computer: all are
  loopback, with the same host and origin. Each start makes a random proof
  (`security::window`); the shell command `window_proof` gives it to the
  main window on the app's own page only, and its sign-in page sends it in
  the `X-OpenAlgo-Window` header. `middleware::sign_in_from` accepts it
  only from this computer.
- **One answer for everyone else.** `Opened::Unreadable` (keys that cannot
  be read, whatever was typed) and `Opened::FailedAfterProof` (the password
  was proven, then a write or a check failed) reach the app window with the
  ways out. Anyone else gets the 401 "Invalid credentials" a wrong password
  gets, spending the same budget; while keys still have to move, the one
  fixed `SIGN_IN_IN_APP_WINDOW_FIRST`.
- **No answer depends on what was typed before the proof.** `key_state`
  reads the account's hash, the vault row (checked without a password by
  `key_vault::check`), the vault files, `secrets.dat` or the keychain, and
  proves earlier keys on the data, before it looks at the name or the
  password. So a damaged vault, a locked keychain or another install's keys
  answer the same for every name and password, also in the app window.
  Recovery choices that need the password (a move that failed after the
  proof) appear only for the right password. The keychain-unreadable and
  damaged-vault choices cannot wait for a proof (the pepper the hash needs
  is in the keychain or the vault), so the app window gets them for any
  input; they say nothing about the input.
- **The same work on every path.** `hashing::pad_argon2_runs` ends every
  sign-in with stand-in Argon2 runs up to `SIGN_IN_ARGON2_RUNS` (two: open
  the vault, check the hash; two per vault file in that state), so no early
  return before Argon2 shows in the timing.
- **No system text in answers or in the error log.** Answers carry fixed
  sentences (`AppError::client_message`). WARN and ERROR lines land in the
  error log the Diagnostics page serves, so what the keychain says (its item
  names, file paths, the system's own text) is logged at INFO only.
- **Key material never prints.** Keychain reads come back as `Secret`
  (redacted, zeroed); the peppered password and a vault file's bytes are
  zeroed after use.

## Caches against the web

The web's rules (`database/auth_db.py` lines 160-175, `invalidate_user_cache`
and `invalidate_user_auth_cache`, `utils/thread_safe_cache.py`) and the
desktop after this change. `crate::cache::TtlCache` is the Rust
`LockedTTLCache`: maxsize, TTL, `generation`, `fill`, `invalidate`,
`clear`, `get_or_load`.

| Cache | Web | Desktop | Holds | Cleared by | Race guard | Difference, if any |
| --- | --- | --- | --- | --- | --- | --- |
| Verified API keys | 1024, 10 h, user id | `ApiKeyCache.verified`: 1024, 10 h, the account name | HMAC of the presented key under a per-process key -> account name | regeneration, logout (lock), password change or reset, start fresh, account reset, broker sign-in or switch (`persist`, `revoke_held`) | generations read before the query; `fill` drops a stale answer | Keyed by a per-process HMAC, not the key: stronger than the web's key-as-key. |
| Invalid API keys | 512, 5 min | `ApiKeyCache.invalid`: 512, 5 min | HMAC -> nothing | same | same | - |
| Order mode | 128, 60 s, invalidated on change | `ApiKeyCache.order_mode`: 128, 60 s | `auto` / `semi_auto` | `set_order_mode` after its write; every clear above | generation read before the query | Was read from the database on every order; now cached as on the web. |
| Broker auth and feed tokens | 1024 each, TTL until the next 03:00 IST (5 min to 24 h) | `AppState.broker_session`: one slot | the decrypted session (`Secret`) | logout, switch, boundary, account reset, start fresh | `try_resume` re-reads the stored row under `session_lock` before installing; the boundary clears only the session it read (compare and clear) | Exact boundary instead of a clamped TTL: no 5-minute tail after 03:00. One slot because one broker is logged in at a time. |
| Broker name | 1024, 50 min | `ServerConfig.active_broker` in memory | the active broker id | reloaded after every write | writers reload after their commit | A settings snapshot, not a TTL cache. |
| Symbol master | loaded at login, cleared at logout | `SymbolResolver`: one generation, bounded by the master | broker symbols and tokens (no secret) | broker session end (logout, switch, boundary) | a load is accepted only if no `clear` ran since the loader read the clear generation (`load_master_if`) | - |
| Instrument facts (`/trading`) | - | `FactsCache`: 256, 60 s, tagged with the master generation | facts from the master and the calendar | logout; any master change makes entries miss | `TtlCache` generation; entries from an older master are ignored | Desktop only. |
| Quotes | per-symbol in the feed proxy | feed bridge `last_tick`, held instruments only; adapter snapshots live with their socket | ticks | broker session end (`bridge.reset`, socket teardown) | filled only for held instruments from the live socket | The bridge's subscription bookkeeping (`applied`) can be re-inserted after a reset by a subscribe in flight; recorded for the feed work (it holds no quote or credential). |
| Capability store (frontend) | per page | `brokerStore` capabilities | what the logged-in broker supports | a session change pushed by the server (`active_sessions_update`), logout | a reply for an earlier broker is ignored (STATE-01) | - |
| Browser sessions | Flask session | `WebSessionStore`: 256, anonymous 30 min idle, pending TOTP 5 min | session id -> user, CSRF token, pending sign-in (with staged keys) | logout, password change or reset, start fresh, account reset, boundary; a pending sign-in older than 5 minutes loses its staged keys at the next 30-second session poll (`drop_stale_sign_ins`), whether or not its browser comes back | the logout clears them under the lock a sign-in issues its session under | - |
| Broker adapter state | module globals per broker | per adapter | e.g. XTS market keys, OTP tokens, funds; Kotak's Holdings costs of carried positions (`TtlCache`, 8 sessions, 5 min, as the web's) | `on_logout` of every adapter at the lock, not only the active one | Kotak's costs: `TtlCache` generation, a holdings read answering after a logout is not kept | Symbol-to-id lookups of a few adapters (Arrow, HDFC Sky, IndMoney, Samco, Motilal traits) stay: bounded, no account data. |

Not caches in this sense, unchanged: rate limiter and sign-in backoff
windows (bounded, per source), MCP and webhook rate windows, the analyzer
mode memo (written under the reader's lock), the monitor's ban list.

## Tests (acceptance)

| Acceptance item | Test |
| --- | --- |
| New install: password mode, zero keychain calls, vault in openalgo.db | `security::tests::a_new_install_never_touches_the_keychain` |
| Keychain move: copied, verified, entries deleted, rows readable, never read again | `server::tests::key_vault::keychain_keys_move_once_and_the_keychain_is_never_read_again` |
| A stop at every step recovers | `server::tests::key_vault::a_stop_at_every_step_of_the_move_recovers` |
| Unreadable keychain: retry and start fresh | `server::tests::key_vault::an_unreadable_keychain_offers_retry_and_start_fresh` |
| No caller but the app window can tell a right password from a wrong one | `server::tests::key_vault::nobody_but_the_app_window_can_tell_a_right_password_from_a_wrong_one`, `server::tests::key_vault::the_ways_out_need_the_password_and_the_app_window` |
| Every sign-in costs the same work | `security::tests::every_sign_in_costs_the_same_work_whatever_it_finds` |
| No system text in answers or the error log; no key prints | `server::tests::key_vault::the_error_log_never_carries_what_the_keychain_said`, `error::tests::key_and_os_errors_never_reach_an_answer`, `security::keystore::tests::a_keychain_read_never_prints_the_key`, `security::window::tests::a_proof_matches_itself_only_and_never_prints` |
| `vault.json` move | `server::tests::key_vault::a_password_mode_vault_file_moves_into_the_database_once` |
| Locked before sign-in and after logout | `server::tests::key_vault::locked_before_sign_in_and_after_logout_every_surface_refuses` |
| Quitting wipes the keys | `server::tests::key_vault::quitting_wipes_the_keys` |
| Password change: one transaction, fault test | `server::tests::key_vault::a_password_change_writes_hash_and_vault_together` |
| Forgotten password | `server::tests::key_vault::a_new_password_without_the_old_one_drops_only_encrypted_values` |
| Caches | `services::apikey_service::tests::*`, `cache::tests::*`, master and facts race tests |
| The OS keychain is never written | `security::keystore::tests::the_os_keychain_is_never_written` (`KeyringStore` has no write code) |
| macOS, Windows and Linux | CI on four systems: the full suite, then the step "KEY-01 keys without the OS keychain, on this system" in `rust-test`, which runs the tests above by name and lists each one (nextest profile `key01`) |

## Tests and the harness

The in-memory keystore of the test harness and the dev server
(`MemoryKeyStore`, `KeyStore::harness_keys`) opens the app unlocked with keys
it keeps, as the keychain mode of earlier builds did, so the many feature
tests need no sign-in. Account setup wraps those keys into the vault, so a
reopened harness app still opens what it wrote. The KEY-01 tests use
`CountingKeyStore`, which behaves like the OS keychain and counts every
call; production uses the OS keychain (`KeyringStore`) for the one-time move
only.

