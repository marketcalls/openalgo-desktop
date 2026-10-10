# Branch and tag protection for releases

This page lists the GitHub settings the maintainer should enable so that a
release can only be built from a commit whose CI passed, and so that history
on `master` cannot be rewritten. Agents never change these settings; they are
the maintainer's to apply (Settings needs the repository admin role).

State on 2026-10-10 (queried read-only): `master` is not protected
(`GET /repos/marketcalls/openalgo-desktop/branches/master/protection` returns
404 "Branch not protected"), and there are no rulesets, tags or releases.

## What already holds without these settings

`.github/workflows/release.yml` starts with a `verify` job that every other
job waits for. On a pushed `v*` tag it refuses to build unless:

- the tag points at a commit reachable from `master`;
- the newest `ci-ok` check run created by GitHub Actions on that exact commit
  is `completed` with conclusion `success`;
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
