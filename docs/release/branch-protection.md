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
- the newest Install smoke run on `master` that installed the bundles of a
  successful CI push of that exact commit (see below) completed with
  success, with every platform's job succeeded (none skipped);
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
secret scan, Tauri bundles) succeeded. Requiring the individual jobs instead
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

## Install smoke

`.github/workflows/install-smoke.yml` runs after every successful CI push to
`master`, on the bundles that CI run built. On fresh runners it installs the
`.deb` and the AppImage (Ubuntu 22.04, x64 and arm64), the NSIS setup
(Windows Server 2022 and 2025) and the app from the `.dmg` (Apple Silicon
macOS); starts each installed app with an empty data folder until it serves
its HTTP API; creates the account and signs in through that HTTP on the
shipped port, and checks that a wrong password is refused (not for the
AppImage, which is started only); runs the packaged `mcp` subcommand over
piped stdio, with no app and, after the sign-in, against the running app
with an MCP token created on it (`launch_smoke.py --mcp-bridge`: protocol
versions, the 49 tools, two calls, a token revoked midway); and checks the
`.deb` dependency list.

Which bundles it may install, and which runs a release may rely on, is
decided by `scripts/ci/smoke_gate.py`, never by a run's name alone:

- Before anything is downloaded, the `source` job checks through the API
  that the CI run is a successful run of `ci.yml` for a push to `master` of
  this repository (digits-only id). A pull request's, another branch's or a
  fork's bundles are refused, also for a run started by hand.
- Each run is named "Install smoke of <sha> (CI run <id>)", or "Install
  smoke of CI run <id>" when run by hand. The release gate takes only the
  digits-only CI run id from the name, checks that CI run the same way and
  that it built the tagged commit, considers only runs of `install-smoke.yml`
  on `master`, and requires the newest such run to have succeeded with every
  platform's job (Linux x64 and arm64, Windows Server 2022 and 2025, macOS)
  succeeded.

The security review of 2026-10-12 found a bypass in the first draft of this
gate (never merged): it trusted the run's name, which a hand-run sets from
free text, passed the id from it into an API path unchecked, and accepted
any CI run with the right commit, a pull request's included.

### Making it part of `ci-ok`

Today the smoke is a separate workflow that starts when CI on `master`
finishes, so it cannot be one of the jobs `ci-ok` waits for, and a pull
request is merged before its installers are tried. To make it part of
`ci-ok` (a maintainer decision: it adds about 15 minutes of runners to every
CI run, pull requests included):

1. In `install-smoke.yml`, add `workflow_call:` to `on:`, and download the
   bundles from the calling run (drop `run-id` and `github-token` from the
   download steps when called).
2. In `ci.yml`, add a job
   `install-smoke: { needs: tauri-build, uses: ./.github/workflows/install-smoke.yml }`
   and add `install-smoke` to the `needs` of `ci-ok`.
3. Drop the `workflow_run` trigger, and the release gate's separate Install
   smoke step: `ci-ok` on the tagged commit then already includes it.

Until then the release gate is what makes it required.

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
