# Security review: the built-in ngrok tunnel (2026-10-27)

Branch `pr/ngrok-tunnel` (base `87d975f2`, 1.0.2). Scope: what the 1.0.3
built-in tunnel adds (`services::tunnel`, `server::tunnel`,
`GET/POST /api/tunnel`, migration 096, the Profile settings), run through
the `security-audit` and `fd-audit` skills. Reviewer: Claude (agent) for
the maintainer; two findings came from the automated review of the pushed
branch and are fixed here.

## Scanners

| Tool | Command | Result |
| --- | --- | --- |
| cargo-deny 0.20.2 | `cargo deny --manifest-path src-tauri/Cargo.toml --config .github/deny.toml --locked check advisories bans licenses sources` | advisories ok, bans ok, licenses ok, sources ok (duplicates are warnings, as configured) |
| cargo-audit | not installed on this machine | not run; cargo-deny read the same RustSec database and found no advisory for any crate in the lock; CI's `security.yml` runs cargo-audit on the merge |
| npm audit (production) | `npm audit --omit=dev --audit-level=high` | 0 vulnerabilities (no npm dependency changed) |
| gitleaks | `gitleaks git --no-banner -c .github/gitleaks.toml --log-opts="origin/master..HEAD" .` | no leaks |
| trivy, CodeQL | not run here | CI runs both on the merge |

New crates: `ngrok` 0.19.0 (pinned exactly, MIT OR Apache-2.0) and its
pure-Rust dependencies (muxado, awaitdrop, proxy-protocol, tokio-retry,
tokio-socks, hyper-http-proxy, rustls-native-certs 0.7, futures-rustls,
hostname 0.3, and helpers). Built without its default `aws-lc-rs` and with
`ring`: the tree keeps one rustls (0.23.46, features ring, std, tls12,
logging); no aws-lc, native-tls or OpenSSL is added. The SDK verifies
ngrok's own CA bundled in the crate. It reads no environment variable
unless `authtoken_from_env` is called (it is not).

## Attack surface

| Surface | Control | Code | Proof |
| --- | --- | --- | --- |
| Who a tunnel request is | Marked before any layer (`ThroughBuiltInTunnel`): `Source::Tunnel` whatever its headers say | `server/source.rs`, `server/middleware.rs` | `server::tunnel::tests::a_tunnel_caller_is_never_this_computer_whatever_its_headers_say` |
| What is reachable | `/api/v1/*`, the strategy and Chartink webhooks, `/mcp`; broker callbacks sent on (307) to `127.0.0.1`; everything else a 404 notice before the router | `server::tunnel::reach` | `only_the_public_routes_are_reachable_by_their_exact_shape`, `the_app_its_sign_in_and_session_routes_stay_on_this_computer`, `the_public_routes_are_served_by_the_app`, `a_broker_redirect_is_sent_on_to_this_computer` |
| Holding places (finding 1) | 10 s headers, 10 s body, one request per connection, 90 s lifetime, 60 s answer wait; past 128 connections a caller is turned away at once | `server/tunnel/serve.rs`, `services/tunnel/supervisor.rs` | `a_slow_body_is_cut_off_at_its_limit_and_frees_its_place`, `an_idle_connection_is_closed_and_none_is_kept_alive`, `a_connection_is_closed_at_its_lifetime_whatever_it_is_doing`, `a_caller_past_the_cap_is_turned_away_at_once`, `a_normal_alert_is_served_while_slow_callers_hold_places` |
| Detached work (finding 2) | Work past the answer wait runs on (never cut off mid-order) holding one of 128 permits until it ends; none free: 503 at once | `serve::answer_within`, `services::tunnel::MAX_WORK` | `a_slow_answer_is_told_to_the_caller_and_the_work_runs_to_its_end`, `work_that_outlives_its_wait_holds_a_place_until_it_ends` |
| The authtoken | Sealed with the data key, bound to `settings.ngrok_authtoken`; opened only while signed in; never returned; in `drop_encrypted_values` and `witness`; dropped by an account reset | `db/sqlite/tunnel.rs`, `services/tunnel/settings.rs` | `the_authtoken_is_never_sent_back_or_logged`, `a_fresh_start_drops_only_the_encrypted_values`, `the_witness_is_the_first_encrypted_value`, `an_account_reset_closes_the_tunnel_and_forgets_the_authtoken` |
| Logs | ngrok's answer to a malformed token repeats it: only its code is logged; other answers with the token removed; the SDK's debug lines are below the `warn` default | `services/tunnel/ngrok_sdk.rs` | `an_answer_from_ngrok_is_logged_without_the_authtoken`, `the_sdk_against_a_server_that_hangs_up_is_unreachable` (all levels captured) |
| Lifecycle | Open only while signed in; closed at sign-out, fresh start, reset, quit | `services/tunnel/mod.rs`, `auth_service.rs`, `state.rs` | `the_tunnel_opens_at_sign_in_and_closes_at_sign_out`, `switched_off_it_stays_closed_at_sign_in_and_closes_when_turned_off` |
| Settings routes | `GET/POST /api/tunnel`: signed-in user, CSRF and same-origin on the write (session layer), validated authtoken and domain, nothing stored on a refusal | `server/routes/tunnel.rs` | `every_user_route_rejects_without_a_signed_in_session`, `a_domain_that_is_not_one_is_refused_and_nothing_is_saved` |

Resources (`fd-audit`): the supervisor is one task owned by the tunnel
(`Running::join`, cancelled through a child of the app's shutdown token and
awaited, bounded, at every stop); connections are a `JoinSet` drained then
aborted; detached work runs in the app's own `JoinSet` (`AppState::spawn`)
and holds a permit; the SDK's own reconnect is cancelled so nothing loops
behind the supervisor. Measured: `no_connection_outlives_its_caller_a_loss_or_a_stop`
counts live connections through 100 callers, a lost tunnel and a stop and
sees zero each time. `leak_grep.py --diff origin/master`: no new suspect
outside test code.

## Residuals

Recorded in `docs/security/known-residuals.md` ("The built-in ngrok
tunnel"): a stranger who learns the address and keeps opening slow
connections can fill the 128 places, each for at most 90 s (alerts then
are turned away at once, not left waiting); the automatic broker redirect
follows the tunnel while it is online, so a broker that checks the address
needs the tunnel's registered too or a fixed local one saved.
