# Contributing to OpenAlgo Desktop

A guide for developers picking up OpenAlgo Desktop: what it is, how to set up
and run it without touching anyone's real data, what CI checks, and the house
rules that decide whether a pull request is merged.

Work through sections 1 to 7 before you write code. Setup is where most time
is lost: the first Rust build alone takes a while.

If anything below fails, ask on the OpenAlgo Discord
(https://discord.com/invite/UPh7QPsNhP) or in a GitHub issue, with your
operating system, the exact command and the exact error.

- [1. What OpenAlgo Desktop is](#1-what-openalgo-desktop-is)
- [2. The one-page checklist](#2-the-one-page-checklist)
- [3. Prerequisites](#3-prerequisites)
- [4. Do you need a broker account?](#4-do-you-need-a-broker-account)
- [5. Running it](#5-running-it)
- [6. Several checkouts: worktrees and the shared build folder](#6-several-checkouts-worktrees-and-the-shared-build-folder)
- [7. Verify your setup](#7-verify-your-setup)
- [8. What CI checks](#8-what-ci-checks)
- [9. Code health](#9-code-health)
- [10. House rules](#10-house-rules)
- [11. Pull requests](#11-pull-requests)
- [12. Reading list](#12-reading-list)
- [13. If setup breaks](#13-if-setup-breaks)

## 1. What OpenAlgo Desktop is

OpenAlgo Desktop is a single-user, cross-platform desktop port of
[OpenAlgo web](https://github.com/marketcalls/openalgo): one trader installs
it on their own computer, signs in, connects one broker and trades. There is
no server to set up, no Python and no `.env` file.

- **Shell**: Tauri 2 (the window, the installers, the updater).
- **Backend**: Rust in `src-tauri/`: an axum HTTP server with Socket.IO
  (`socketioxide`), the market data feed server on port 8765, the service
  layer, the sandbox engine, 37 broker adapters, a native MCP server, and
  native Telegram and WhatsApp bots, on one tokio runtime.
- **Frontend**: the OpenAlgo web React 19 app in `src/` (TypeScript, Vite,
  Tailwind, TanStack Query, Zustand), carried over nearly verbatim.
- **Storage**: SQLite (`openalgo.db`, `logs.db`, `sandbox.db`) and DuckDB
  (`historify.duckdb`) in the app's data folder.

How the pieces fit is in [architecture.md](architecture.md). Three properties
shape almost every decision in the code:

1. **The compatibility contract.** Anything that works against OpenAlgo web
   must work against the desktop unchanged: `/api/v1` on
   `http://127.0.0.1:5000` and the feed on `ws://127.0.0.1:8765` answer field
   for field as the web does. The Python SDK, TradingView, Amibroker,
   Chartink, Excel and MCP clients depend on it.
2. **A long-lived process.** The app stays open all trading day through
   reconnects and sleep; anything leaked per request, tick or reconnect
   accumulates until it dies.
3. **Four platforms, all first-class.** Windows x64, macOS (Apple Silicon and
   Intel), Linux x64 and Linux ARM64. A change that only works on the machine
   it was written on is not done.

## 2. The one-page checklist

- [ ] Git, Node.js and Rust installed ([3](#3-prerequisites)), and the native
      libraries for your system
- [ ] Repository cloned; `npm ci` completed
- [ ] `npm run build` and `cargo run --example dev_server` start a server on
      `http://127.0.0.1:5500` that you can open in a browser
- [ ] The checks in [7](#7-verify-your-setup) pass
- [ ] Read [CLAUDE.md](../CLAUDE.md) and sections 9 and 10 here
- [ ] Optional: a broker account with API access ([4](#4-do-you-need-a-broker-account))

## 3. Prerequisites

The tools and the native libraries per system are in README,
[Running from source](../README.md#running-from-source-developers): Git,
Node.js 22 LTS (22.23.4+, 24.22.1+ or 26.12.1+, the October 2026 security
releases; Node 20 is end of life), and Rust through `rustup`.
The Rust version is pinned in `rust-toolchain.toml` at the repository root
(1.99.1, with rustfmt and clippy), so `rustup` installs the right one the
first time you build. On Linux the package list is the one CI uses
(`.github/actions/linux-tauri-deps`).

| Resource | Need |
| --- | --- |
| Disk | About 20 GB for the Rust build folder of one checkout; more with several (see [6](#6-several-checkouts-worktrees-and-the-shared-build-folder)) |
| Memory | Several GB free while the main library compiles. On an 8 GB machine, build with `CARGO_BUILD_JOBS=4` (or 2) and one cargo command at a time |
| Time | The first build takes many minutes (DuckDB and SQLite are compiled from source); later builds only rebuild what changed |

## 4. Do you need a broker account?

**Not to contribute.** The test suites run against fake brokers, and the
`dev_server` example runs the whole server with a temporary data folder. You
can create an account, sign in, browse the pages, generate an API key and
call `/api/v1` without a broker.

**Yes, to see market data and fills.** Prices, option chains, the feed on
8766 and sandbox fills all come from a connected broker, even in analyzer
mode. With a broker account (use the full app, `npm run tauri:dev`, which
starts the feed server; `dev_server` does not):

- A development build serves on port 5500, so a broker that signs in by
  redirect sends you back to the redirect URL registered with it, usually
  `http://127.0.0.1:5000/<broker>/callback`. Either register a second
  redirect URL for port 5500 in the broker's developer portal and set it for
  that broker on Broker Configuration, or test redirect sign-ins on an
  installed build. Form and saved-key brokers have no redirect.
- Stay in analyzer mode while you develop. Never place a Live order from a
  development build unless you mean to.
- `dev_server` starts with an empty account each time, so broker keys you
  enter there are gone at the next start.

## 5. Running it

```bash
git clone https://github.com/marketcalls/openalgo-desktop.git
cd openalgo-desktop
npm ci
```

### Ports

| Listener | Installed app | Development |
| --- | --- | --- |
| HTTP (the UI, `/api/v1`, broker callbacks, `/mcp`) | `127.0.0.1:5000` | `127.0.0.1:5500` |
| Market data feed | `127.0.0.1:8765` | `127.0.0.1:8766` |

**A debug build always uses 5500 and 8766**, so it can run beside an installed
copy or OpenAlgo web on 5000 and 8765. A release build uses them only when
`OPENALGO_DESKTOP_DEV_PORTS=1` is set. The only other environment variable
the app reads is `OPENALGO_MCP_TOKEN`, and only in the `mcp` subcommand (plus
`APPIMAGE`, which the AppImage runtime sets on Linux). There is no `.env`.

### The server alone, with throwaway data (recommended)

```bash
npm run build                                   # the interface, into dist/
cd src-tauri && cargo run --example dev_server  # then open http://127.0.0.1:5500
```

`src-tauri/examples/dev_server.rs` runs the app's HTTP server and the
services behind it, with no window, in a **temporary data folder** with an
in-memory key store, so it never touches a real data folder. It prints its
address, its process id and the folder. It starts only the HTTP server and
the session expiry task: the market data feed on 8766, the health sampler,
the traffic and latency monitor, the strategy and OpenScript schedulers and
Chartink are not started, so pages that need them show their empty or
fallback states. Run the full app for those. Every start begins with an empty account; stop it
with Ctrl+C. A browser tab left open across a restart still believes it is
signed in and shows empty states such as "No API key generated": reload it.

A debug build reads `dist/` from disk, so after a frontend change run
`npm run build` again and reload.

### The frontend with live reload

`npm run dev` starts Vite on `http://localhost:5173` and proxies the server's
paths to `OPENALGO_DEV_BACKEND` (by default `http://127.0.0.1:5500`, the
`dev_server` above). Frontend changes then reload at once.

### The full app

```bash
npm run tauri:dev
```

This opens the app window (on the Vite dev server) and restarts the app when
Rust changes. **It uses the same data folder as the installed app**
(`~/Library/Application Support/com.openalgo.desktop` on macOS; README,
[Where your data lives](../README.md#where-your-data-lives-and-backups)):
close the installed app first and back that folder up if it holds an account
you care about, or use `dev_server` instead.

### Rules for anything you run

- **Never run the app binary, in any mode or subcommand, against a real data
  folder** you care about. A development build migrates it, and an older
  installed version then refuses to open it.
- **Never bind 5000 or 8765 from a test**: a test that starts a listener uses
  ephemeral ports (`AppState::pin_listener_ports`, port 0), because a debug
  build puts 5500 and 8766 back on every settings reload.
- **Only one process can hold 5500 or 8766.** A "port in use" message means a
  `dev_server` or a test child is still running: find it with
  `lsof -nP -iTCP:5500 -sTCP:LISTEN` and stop it by its process id. Never
  `pkill` by pattern; it kills other people's runs.

## 6. Several checkouts: worktrees and the shared build folder

Working on more than one branch at a time, use `git worktree` rather than
several clones, and share one Rust build folder so dependencies compile once:

```bash
git worktree add .claude/worktrees/<topic> -b <branch> origin/master
export CARGO_TARGET_DIR=<repository>/src-tauri/target    # the same for every worktree
```

What sharing costs, and how to live with it:

- **Builds queue.** "Blocking waiting for file lock on build directory" is
  another checkout's build. Wait; never delete the lock or kill the other
  cargo.
- **One build at a time.** Two worktrees building the crate at once overwrite
  each other's build-script output and force each other to recompile. On a
  machine with 8 GB of memory, also keep `CARGO_BUILD_JOBS` low.
- **`target/debug/openalgo-desktop` belongs to whoever built last.** A test
  that runs the binary uses `env!("CARGO_BIN_EXE_openalgo-desktop")`, an empty
  environment with a temporary home and data folder, ephemeral ports, and a
  guard that kills and reaps the child (model:
  `src-tauri/tests/it/mcp_subcommand.rs`).
- **"capability with identifier ... not found"** after switching worktrees
  is the shared build-script output naming another worktree. Rebase the
  older worktree onto master; CLAUDE.md, "One shared build folder", has the
  immediate fix.
- **A new worktree has no `node_modules` and no `dist/`.** Run `npm ci` in it
  when you need the frontend tools, and for a Rust-only build create the
  stand-in page CI uses:
  `mkdir -p dist && test -f dist/index.html || echo '<!doctype html><title>ci</title>' > dist/index.html`.
- **The folder grows past 30 GB.** With no build running, delete old
  `target/debug/deps` and `build` entries and `incremental`; never the whole
  folder, and never anything under a running build.

The `parallel-work` skill
([.claude/skills/parallel-work/SKILL.md](../.claude/skills/parallel-work/SKILL.md))
has the full procedure, including merging through a temporary worktree.

## 7. Verify your setup

From the repository root:

```bash
npx biome ci ./src                 # lint and format of the frontend
npx tsc -b                         # type check
npx vitest run                     # frontend unit and component tests
npx playwright test --project=chromium   # end-to-end, if the UI changed (needs npx playwright install chromium once)
python3 scripts/ci/code_health.py  # the code-health gate
```

From `src-tauri/`:

```bash
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked                # unit, integration and doc tests
```

CI runs the Rust tests with `cargo nextest` (`cargo nextest run --locked
--all-targets --config-file ../.github/nextest.toml --profile ci`); plain
`cargo test` covers the same tests. While iterating, run a filtered set
(`cargo test --locked --lib <module>` or `--test it <name>`) and the full
suite once before pushing.

## 8. What CI checks

`.github/workflows/ci.yml` runs on every pull request and every push to
`master`. The `ci-ok` job passes only when every other job passed; it is the
one required check.

| Job | Checks |
| --- | --- |
| `rust-lint` | `cargo fmt --check` and `cargo clippy --all-targets --locked -D warnings` (Ubuntu) |
| `rust-test` | The Rust suite with nextest and the doc tests on Ubuntu 22.04 x64, Ubuntu 22.04 ARM64, macOS and Windows; plus the key-vault (KEY-01) tests by name |
| `rust-coverage` | Line coverage against the ratchet in `.github/coverage-baseline.txt`, and per-area floors in `.github/coverage-areas.json` |
| `frontend` | Biome, `tsc -b`, the chart-engine reference checks for the pinned `openalgo-charts`, the Vite build, and the CI scripts' own tests |
| `frontend-test` | Vitest with coverage thresholds |
| `e2e` | Playwright on Chromium against `vite preview` |
| `secret-scan` | gitleaks over the history (`.github/gitleaks.toml`) |
| `code-health` | `scripts/ci/code_health.py` ([9](#9-code-health)) |
| `tauri-build` | The installers: Windows NSIS, macOS Apple Silicon and Intel, Linux x64 and ARM64 `.deb` and AppImage |
| `install-smoke`, `native-e2e` | Each installer installed on a fresh machine and started as a trader would: account created, signed in, a wrong password refused, the window's page loaded, the packaged `mcp` subcommand; on Ubuntu the app driven through its window |

Other workflows: `security.yml` (dependency advisories, weekly and on
lockfile changes), `codeql.yml`, `perf.yml` (nightly benchmarks and
endurance), `soak.yml` (weekly leak soak), `native-e2e-test-build.yml` (a
sandbox order through the app's window on Linux and Windows, after each push
to master) and `release.yml` (see [releases/README.md](releases/README.md)).
What they guarantee, and the repository rules behind them, are in
[release/branch-protection.md](release/branch-protection.md).

A green run on your machine proves nothing for the other three systems: CI is
the check.

## 9. Code health

Strict, and enforced by the `code-health` job (`scripts/ci/code_health.py`):

| Rule | Limit |
| --- | --- |
| A Rust file | 1,000 lines of non-test code |
| A Rust function | 100 lines; 60 in an HTTP route or a Tauri command |
| Database access in an HTTP route or a Tauri command | None: call a service |
| `#[allow(..)]` or `#[expect(..)]` in production code | No new one |
| A `pub fn` that nothing calls | None |
| A desktop frontend file | 1,000 lines; files carried over from the web are exempt |
| `unwrap`, `expect`, `todo!`, `dbg!`, `println!`, `eprintln!` in production code | None |

What was over a limit when the gate started is listed in
`.github/code-health-baseline.json`. That list is a ratchet:

- **Nothing new joins it.** A change that would put something over a limit
  restructures first: a module directory with a short `mod.rs` that says
  what lives where, a long function split into named steps.
- **A listed item may shrink, and the shrink is written back**:
  `python3 scripts/ci/code_health.py --update`, committed in the same change.
  CI fails until you do.
- **A listed item never grows** without the maintainer's
  `code-health-exception` label.

Also checked in review: thin handlers (parse, call one service, shape the
reply), one place for one thing (search before writing a helper), no dead
code (`pub(crate)` unless something outside needs it), a parameter struct or
a boxed error instead of a lint suppression, names and comments that say
what a thing is in trading terms and why it is so. Files carried over from
the web keep the web's shape so web changes still apply by diff; desktop
additions go in a separate component or module.

## 10. House rules

These are what pull requests are sent back for.

**The web is the contract.** A change to `/api/v1` or the feed matches the
web field for field. The sources, in order: the golden fixtures recorded from
a live web instance in `tests/fixtures/web/`, then the web's docs, then the
web's code. A contract test that disagrees with a fixture is a desktop bug.
The web repository is read-only reference.

**Tests are part of the change.** A change ships with its tests in the same
commit: Rust unit tests with recorded payloads, HTTP contract tests against
the fixtures, WebSocket protocol tests, migration tests on a populated
database, Vitest and accessibility tests for the frontend. Wait on an event,
never on a sleep; a test that fails one run in five is a bug.

**Business logic lives in services.** HTTP handlers and Tauri commands
validate, call one service and shape the reply. A new side effect of an order
is a new event-bus subscriber, never a line in the order path.

**Sandbox mode is a product.** Every order and account path short-circuits to
the sandbox engine in analyzer mode, with the web's analyze-mode replies.

**Resource hygiene.** One shared HTTP client with timeouts; database
connections never held across a network `.await`; every spawned task owned
and aborted on shutdown; every cache bounded with an expiry; every channel
bounded; every subscription removed on the error path too. After a change to
databases, sockets, feeds, tasks or caches, run through the `fd-audit` skill
and say so in the pull request.

**Schema changes are numbered, idempotent migrations** that check before
altering, never overwrite a value the trader set, and backfill from existing
data. A new store goes into `db::upgrade::STORES` in the same change.

**Secrets.** No API key, token, password, account id or e-mail in code,
logs, fixtures or screenshots: fixtures use `<APIKEY>`, `<USER_ID>` and
`<EMAIL>`. Secrets travel in types whose `Debug` is redacted.

**Messages are written for a trader.** Name the cause and the next action.
Never show a status code, an exception, a protocol term or an endpoint; the
detail goes to the log.

**Vocabulary.** "Sandbox mode" or "analyzer mode", never "paper trading" or
"virtual trading". Never "arm", "armed" or "arming" where a trader reads it:
an alert is Active or Stopped, a destination is Live or Sandbox.

**Adding a page** is three registrations in one change: the route in the
frontend router (`src/App.tsx`), the same path in the server's SPA list
(`src-tauri/src/server/spa.rs`), and the navigation entry
(`src/config/navigation.ts`).

**No icons or emojis anywhere**: source, comments, logs, commits, pull
requests, changelogs.

**Conventional Commits**: `feat:`, `fix:`, `docs:`, `refactor:`, `test:`,
`chore:`, `ci:`. A refactor is its own commit with no behaviour change.

**On AI assistance.** Using an AI coding assistant is common here, and the
repository's `CLAUDE.md` and `.claude/skills/` are written for it. What is
not fine is a pull request you have not read, run and tested yourself.

## 11. Pull requests

- One theme per pull request. A new broker is self-contained and fine as
  one.
- Fill in `.github/pull_request_template.md`: what and why, how it was
  tested, compatibility with OpenAlgo web, and the checklist (four
  platforms, sandbox mode, no secrets, trader-facing messages, resource
  hygiene, migrations, commits).
- Open it as a draft until `ci-ok` is green.
- A pull request that changes what a trader sees updates the docs in the same
  change ([INDEX.md](INDEX.md) maps them), and a release-worthy change adds a
  line to `## Unreleased` in [CHANGELOG.md](../CHANGELOG.md).

## 12. Reading list

About 30 minutes; skimming is fine.

1. [CLAUDE.md](../CLAUDE.md): written for AI agents, but the most
   concentrated description of the invariants, conventions and known
   pitfalls anywhere in the repository. Read it.
2. [INDEX.md](INDEX.md), the map to the rest. Open only what you need.
3. [architecture.md](architecture.md) and, for market data,
   [websocket-architecture.md](websocket-architecture.md).
4. [port/frontend-backend-contract.md](port/frontend-backend-contract.md) if
   you touch a page, and [port/order-outcomes.md](port/order-outcomes.md) if
   you touch an order path.
5. [broker-integration-guide.md](broker-integration-guide.md) only if you are
   adding or changing a broker.
6. The skill for the task, in `.claude/skills/`: `broker-integration`,
   `web-sync` (carrying web changes over), `fd-audit`, `verify`,
   `security-audit`, `parallel-work`, `version-bump`, `chart-indicator`,
   `openscript`.

## 13. If setup breaks

The README's [Common problems](../README.md#common-problems) covers missing
WebKitGTK, MSVC and Xcode tools and the Rust toolchain. Also:

| Symptom | Fix |
| --- | --- |
| Port 5500 or 8766 already in use | A `dev_server`, a development app or a test child still runs. `lsof -nP -iTCP:5500 -sTCP:LISTEN` (macOS, Linux) or `netstat -ano \| findstr :5500` (Windows), then stop that process by its id |
| The browser at `127.0.0.1:5500` says the interface is not built | Run `npm run build` once; a debug build reads `dist/` from disk |
| A Rust build in a new worktree fails on `../dist` | Create the stand-in `dist/index.html` from [6](#6-several-checkouts-worktrees-and-the-shared-build-folder) |
| `capability with identifier <name> not found` | The shared build folder's build-script output names another worktree: rebase this one onto master, or see CLAUDE.md, "One shared build folder" |
| Builds take hours or the machine swaps | Several builds at once on a small machine. One cargo command at a time, `CARGO_BUILD_JOBS=4` or lower |
| The disk fills | The shared build folder: delete old `deps`, `build` and `incremental` entries while no build runs ([6](#6-several-checkouts-worktrees-and-the-shared-build-folder)) |
| clippy passes locally and fails in CI | Check `rustup show` uses 1.99.1 from `rust-toolchain.toml`; a newer clippy has other lints |
| A test passes on your machine and fails on another system in CI | A race or a platform difference, not a slow machine: wait on the event, not a sleep; CLAUDE.md, "Tests that pass locally and fail in CI", lists the ones found so far |
| `dev_server` shows "No API key generated" after a restart | Its account was wiped at the restart; reload the page and set it up again |
