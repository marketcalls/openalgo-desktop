# Branch and tag protection for releases

This page lists the GitHub settings the maintainer should enable so that a
release can only be built from a commit whose CI passed, and so that history
on `master` cannot be rewritten. Agents never change these settings; they are
the maintainer's to apply (Settings needs the repository admin role).

State on 2026-10-12: the three rulesets below were applied with the
maintainer's approval, through the API commands at the end of this page:
24922903 `master history` (no bypass), 24922904 `master ci-ok` in the
transitional profile (repository admins bypass) and 24922905 `release tags`.
`GET /repos/marketcalls/openalgo-desktop/rules/branches/master` lists
`required_status_checks`, `deletion` and `non_fast_forward`. `master ci-ok`
moves to the strict profile (empty bypass list, work lands through pull
requests) once the current audit fix wave has merged; `CLAUDE.md` changes in
the same step. There are no tags or releases yet.

## What already holds without these settings

`.github/workflows/release.yml` starts with a `verify` job that every other
job waits for. On a pushed `v*` tag it refuses to build unless:

- the tag points at a commit reachable from `master`;
- the newest `ci-ok` check run created by GitHub Actions on that exact commit
  is `completed` with conclusion `success`;
- the newest CI run of a push of that exact commit to `master` that was not
  cancelled completed with success, with every installed-app job (see
  below) succeeded, none skipped;
- the tagged commit's dependencies pass an advisory scan made at release
  time (`cargo deny`, `cargo audit` and `npm audit --omit=dev`, against
  that day's advisory databases, gated as `security.yml` gates them; the
  accepted advisories are the documented ignores in `.github/deny.toml`):
  a green CI run or last week's scan can predate an advisory;
- the tag equals `v<version>` and every version location agrees
  (`.claude/skills/version-bump/check_versions.py --tag <tag>`), including a
  `## <version>` section at the top of `CHANGELOG.md`.

The gate can be run alone, without building or publishing anything:
Actions > Release > Run workflow, with the tag to check.

What the gate cannot do by itself: stop a red commit from landing on
`master`, stop a force push that rewrites `master`, or stop someone moving or
deleting a `v*` tag. Those need the settings below.

## Settings to enable

Rulesets are preferred over classic branch protection: they can be split so
that the history rules have no bypass at all. Settings > Rules > Rulesets >
New ruleset.

### 1. `master` history (no bypass)

| Field | Value |
| --- | --- |
| Ruleset name | `master history` |
| Enforcement status | Active |
| Bypass list | empty (nobody, not even admins) |
| Target branches | Include default branch (`master`) |
| Restrict deletions | on |
| Block force pushes | on |

Every other rule off. This keeps every commit that CI or a release checked on
`master` for good.

### 2. `master` requires `ci-ok`

| Field | Value |
| --- | --- |
| Ruleset name | `master ci-ok` |
| Enforcement status | Active |
| Target branches | Include default branch (`master`) |
| Require status checks to pass | on |
| Status checks that are required | `ci-ok`, source GitHub Actions (app id 15368) |
| Require branches to be up to date before merging | off (CI also runs on every push to `master`) |
| Bypass list | see below |

Only `ci-ok` is required: it is the aggregate job in `.github/workflows/ci.yml`
that fails unless every other CI job (Rust lint, tests on four systems, Rust
coverage ratchet, frontend checks and tests with coverage thresholds, e2e,
secret scan, code health, Tauri bundles, and the installed-app jobs below)
succeeded. Requiring the individual jobs instead
would break whenever a matrix entry is renamed.

The bypass list decides how work lands on `master`:

- **Strict (recommended once work goes through pull requests).** Bypass list
  empty. A required status check blocks any push whose commit has not already
  passed `ci-ok` on another ref, so changes land through a pull request (or a
  branch that CI has run on) and merge only when `ci-ok` is green.
- **Transitional (keeps today's direct pushes to `master`).** Bypass list:
  role `Repository admin`, bypass mode "Always". Pull requests from anyone
  else must pass `ci-ok`; direct pushes by an admin, including agents working
  with the maintainer's credentials, are not blocked, so a red commit can
  still land. The release gate still refuses to build from it.

`CLAUDE.md` currently tells agents to push to `master` at each checkpoint.
Choosing the strict profile means changing that instruction to "push a
branch, open a pull request, merge when `ci-ok` is green" in the same change.

### Merge queue (applied 2026-10-15 with the maintainer's approval)

`master ci-ok` also requires the merge queue: a PR merges only after `ci-ok`
passes on a temporary branch holding the latest master plus the PRs queued
ahead of it. Two PRs that were each green on their own base can therefore no
longer break master together (as #44 and #63 did on 2026-10-15). CI runs on
the `merge_group` event for this. Merge with `gh pr merge <n> --auto`; the
queue rebases and merges when its run is green.

### 3. `v*` tags

| Field | Value |
| --- | --- |
| Ruleset name | `release tags` |
| Ruleset type | Tag ruleset |
| Enforcement status | Active |
| Target tags | Include by pattern `v*` |
| Restrict creations | on |
| Restrict updates | on |
| Restrict deletions | on |
| Bypass list | role `Repository admin`, bypass mode "Always" |

Only the maintainer creates a release tag (the `version-bump` skill already
says an agent never does), and nobody moves or deletes one after the release
was built from it.

## The same settings through the API

For reference; the maintainer runs these, never an agent. Each command
creates one ruleset. Review the JSON before running it.

```bash
REPO=marketcalls/openalgo-desktop

# 1. master history, no bypass
gh api -X POST "repos/$REPO/rulesets" --input - <<'JSON'
{
  "name": "master history",
  "target": "branch",
  "enforcement": "active",
  "bypass_actors": [],
  "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
  "rules": [{"type": "deletion"}, {"type": "non_fast_forward"}]
}
JSON

# 2. master requires ci-ok (transitional profile shown: admins bypass;
#    for the strict profile use "bypass_actors": [])
gh api -X POST "repos/$REPO/rulesets" --input - <<'JSON'
{
  "name": "master ci-ok",
  "target": "branch",
  "enforcement": "active",
  "bypass_actors": [
    {"actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always"}
  ],
  "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
  "rules": [
    {
      "type": "required_status_checks",
      "parameters": {
        "strict_required_status_checks_policy": false,
        "required_status_checks": [{"context": "ci-ok", "integration_id": 15368}]
      }
    }
  ]
}
JSON

# 3. release tags
gh api -X POST "repos/$REPO/rulesets" --input - <<'JSON'
{
  "name": "release tags",
  "target": "tag",
  "enforcement": "active",
  "bypass_actors": [
    {"actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always"}
  ],
  "conditions": {"ref_name": {"include": ["refs/tags/v*"], "exclude": []}},
  "rules": [{"type": "creation"}, {"type": "update"}, {"type": "deletion"}]
}
JSON
```

`actor_id` 5 is the built-in `Repository admin` role; `integration_id` 15368
is the GitHub Actions app, the source of the `ci-ok` check run.

## Installed-app checks (install smoke and native E2E)

Two workflows run as jobs of every CI run, pull requests included: `ci.yml`
calls them once `tauri-build` has built the bundles, and `ci-ok` needs them.
The bundles, the workflows and the scripts all come from the commit under
test, and each job names that commit in its summary
(`scripts/ci/tested_commit.sh`; on a pull request, its head and the merge
commit the run built).

- `.github/workflows/install-smoke.yml`, on fresh runners: installs the
  `.deb` and the AppImage (Ubuntu 22.04, x64 and arm64), the NSIS setup
  (Windows Server 2022 and 2025) and the app from the `.dmg` (Apple Silicon
  macOS); starts each installed app with an empty data folder until it
  serves its HTTP API; creates the account and signs in through that HTTP
  on the shipped port, and checks that a wrong password is refused (not for
  the AppImage, which is started only); checks that the app's own window
  loaded its page from the app's server (the window's GET of `/` in the
  app's request log, `launch_smoke.py --window`), so a GUI that fails to
  start fails the job on every platform, macOS included; runs the packaged
  `mcp` subcommand over piped stdio, with no app and, after the sign-in,
  against the running app with a read-scope MCP token created on it
  (`launch_smoke.py
  --mcp-bridge`: protocol versions, the read scope's tools and no order
  tool, an order refused for its scope, two calls, a token revoked midway);
  checks the `.deb` dependency list; checks that both Mac apps (Apple
  Silicon and Intel) ask for no newer macOS than README "Supported systems"
  claims (`check_macos_min.py`); and on Windows checks that the data folder
  and its files are closed to Everyone, Users and the other broad groups
  (`check_windows_acl.ps1`, after showing that it fails on a folder open to
  Everyone). What CI cannot show is the maintainer's checklist,
  [Platform qualification](platform-qualification.md).
- `.github/workflows/native-e2e.yml`: the installed `.deb` driven through
  its own window by WebDriver (tauri-driver and WebKitWebDriver under Xvfb):
  account creation, a refused wrong password, sign-in, the signed-in shell
  and sign-out, against the app's real server and session cookie.

Outside `ci-ok`, after every push to `master`, by hand, and on pull
requests that change the harness itself:
`.github/workflows/native-e2e-test-build.yml` runs the same native script on
Linux (tauri-driver) and Windows Server 2025 (msedgedriver) against a
**test-only** build (`src-tauri/src/e2e.rs`, the `e2e` profile: the release
settings without link-time optimization), and adds a sandbox order
(`scripts/ci/native_e2e_sandbox.py`): log in to a scripted broker
(`src-tauri/src/brokers/scripted.rs`, the `e2e-broker` feature, in the place
of Dhan (Sandbox)) with its saved keys, check the API key is there, switch to
analyzer mode, place the ATM call from the Option Chain's order form, find
it in the Order Book with a sandbox order id, read it through
`/api/v1/orderbook` in the web's analyze-mode shape, and show by the
scripted broker's own count (read out at the test build's
`GET /e2e/scripted-broker`) that no order reached it. On Windows the build also has
the `e2e-debug-port` feature: msedgedriver needs WebView2's remote debugging
port, which the release app does not open, so the main window opens the
port the script gives it (`OPENALGO_E2E_DEBUG_PORT`, a free port per run, on
127.0.0.1 only, which the script checks). Guardrails: neither feature
compiles outside a debug build without `OPENALGO_E2E_BUILD=1`, which only
this workflow sets (its Linux job shows the same build failing on the guard
without it); with the feature on, the broker catalogue, the sign-in table
and every adapter but the one slot are unchanged (its lint job runs those
tests with the feature); `release.yml` never builds them; and the install smoke
and `release.yml` check every shipped binary for their strings
(`scripts/ci/check_no_e2e_build.py`), a check this workflow runs on its own
builds to show it finds them. It builds a second binary per system (the
Windows release-profile build took 39 minutes on 2026-10-20, run
39373651570), which is why it is not a job of every pull request; every pull
request runs the scripted market through the same routes in-process
(`src-tauri/tests/it/e2e_scripted_broker.rs`).

Cost: each job takes about a minute once its bundle exists (2026-10-20:
25-45 s of steps per install smoke job, about 70 s for the native run), so
a CI run ends about two minutes later than before, and uses one short
macOS job more. They ran as separate workflows after CI on `master` until
2026-10-20; a pull request was then merged before its installers were
tried, and two that were each green broke the smoke on `master` (#125 and
#111: the MCP tool list a read token sees).

The release gate (`scripts/ci/smoke_gate.py release`, run by `release.yml`)
counts only this repository's CI runs of a push of the tagged commit to
`master`; a pull request's run of the same commit built it merged into the
`master` of its day, so it does not count. The newest such run that was
not cancelled decides, and every installed-app job in it must have
succeeded (a skipped or missing one fails). A `master` run cancelled because
its pull request had already been tested has no verdict: re-run it (Re-run
all jobs) before tagging. To try a run's bundles again, re-run its
installed-app jobs; the bundles are kept 7 days.

The gate also requires the native order journey
(`native-e2e-test-build.yml`, outside `ci-ok`) green on the tagged commit:
the newest run of it for that commit on `master` that was not cancelled,
started by the push or by hand, with all three of its jobs passed. On
`master` every commit gets its own run of it (a newer push never cancels an
older one), so a failed or cancelled one is re-run, not waited out.

## Checking that it holds

```bash
gh api repos/marketcalls/openalgo-desktop/rules/branches/master   # lists the active rules
gh api repos/marketcalls/openalgo-desktop/rulesets                # the three rulesets
```

- A `git push --force` to `master` is rejected.
- A pull request whose CI fails cannot be merged.
- Actions > Release > Run workflow with a tag on a commit without a green
  `ci-ok` fails in `verify`, before any build job starts.

## Code signing certificates

Signing needs purchases and secrets only the maintainer can provide; the
release workflow already uses them when present and states in the release
notes what its check of each installer found:

- macOS: an Apple Developer ID Application certificate and notarization
  credentials (`APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`,
  `APPLE_SIGNING_IDENTITY`, and either `APPLE_ID`, `APPLE_PASSWORD`,
  `APPLE_TEAM_ID` or `APPLE_API_ISSUER`, `APPLE_API_KEY`,
  `APPLE_API_PRIVATE_KEY`).
- Windows: an Authenticode code-signing certificate
  (`WINDOWS_CERTIFICATE`, `WINDOWS_CERTIFICATE_PASSWORD`).

With signing configured, a build whose installer comes out unsigned (or, on
macOS, without a stapled notarization ticket) fails instead of publishing.
