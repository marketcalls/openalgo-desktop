#!/usr/bin/env python3
"""Which CI run an install smoke may install, and whether a release may ship.

    python3 scripts/ci/smoke_gate.py source <ci run id>
    python3 scripts/ci/smoke_gate.py release <commit sha>

`source` (.github/workflows/install-smoke.yml, before anything is
downloaded): the CI run must be a successful run of this repository's CI
workflow for a push to master. It prints the run's commit and writes
`sha` and `run_id` to $GITHUB_OUTPUT when that is set. A run id that is not
digits only, a pull request's run, another branch's or another
repository's run, or one that did not succeed is refused.

`release` (the `verify` job of .github/workflows/release.yml): among the
Install smoke runs of this repository's install-smoke.yml on master, those
named after a CI run ("... CI run <digits>") whose CI run passes the
`source` checks and built <commit sha> are candidates; nothing else in a
run's name is trusted. The newest candidate decides: it must have
completed with success, and every expected job must have succeeded (a
skipped job does not count).

It reads the GitHub API through `gh api` (GH_TOKEN, GITHUB_REPOSITORY).
SMOKE_GATE_FIXTURES=<dir> answers each API path from <dir> instead, for
the tests: the file name is the path with "/", "?", "&" and "=" replaced by
"_", plus ".json".
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys

DEFAULT_BRANCH = "master"
CI_WORKFLOW = ".github/workflows/ci.yml"
SMOKE_WORKFLOW = ".github/workflows/install-smoke.yml"
# The job names of install-smoke.yml; keep in step with it.
EXPECTED_JOBS = (
    "linux (x86_64)",
    "linux (aarch64)",
    "windows (windows-2022)",
    "windows (windows-2025)",
    "macos (aarch64)",
)
RUN_ID = re.compile(r"^[0-9]+$")
TITLE = re.compile(r"^Install smoke of .*CI run ([0-9]+)\)?$")


class Refused(Exception):
    pass


def api(path: str):
    fixtures = os.environ.get("SMOKE_GATE_FIXTURES")
    if fixtures:
        name = re.sub(r"[/?&=]", "_", path) + ".json"
        try:
            with open(os.path.join(fixtures, name), encoding="utf-8") as f:
                return json.load(f)
        except FileNotFoundError:
            raise Refused(f"no such API object: {path}") from None
    out = subprocess.run(["gh", "api", path], capture_output=True, text=True)
    if out.returncode != 0:
        raise Refused(f"GitHub API {path}: {out.stderr.strip()[:200]}")
    return json.loads(out.stdout)


def repo() -> str:
    r = os.environ.get("GITHUB_REPOSITORY", "")
    if not re.match(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$", r):
        raise Refused(f"GITHUB_REPOSITORY is not owner/name: {r!r}")
    return r


def master_ci_run(run_id: str) -> dict:
    """The CI run, if it is a successful CI push to master in this repository."""
    if not RUN_ID.match(run_id or ""):
        raise Refused(f"the CI run id must be digits only, got {run_id!r}")
    r = repo()
    run = api(f"repos/{r}/actions/runs/{run_id}")
    checks = {
        "workflow": (run.get("path"), CI_WORKFLOW),
        "event": (run.get("event"), "push"),
        "branch": (run.get("head_branch"), DEFAULT_BRANCH),
        "repository": ((run.get("head_repository") or {}).get("full_name"), r),
        "conclusion": (run.get("conclusion"), "success"),
    }
    wrong = [f"{k} is {got!r}, not {want!r}" for k, (got, want) in checks.items() if got != want]
    if wrong:
        raise Refused(f"CI run {run_id} is not a successful CI push to {DEFAULT_BRANCH}: " + "; ".join(wrong))
    sha = run.get("head_sha") or ""
    if not re.match(r"^[0-9a-f]{40}$", sha):
        raise Refused(f"CI run {run_id} has no commit sha")
    return run


def source(run_id: str) -> int:
    run = master_ci_run(run_id)
    sha = run["head_sha"]
    print(f"Bundles of {sha} (CI run {run_id})")
    out = os.environ.get("GITHUB_OUTPUT")
    if out:
        with open(out, "a", encoding="utf-8") as f:
            f.write(f"sha={sha}\nrun_id={run_id}\n")
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        server = os.environ.get("GITHUB_SERVER_URL", "https://github.com")
        with open(summary, "a", encoding="utf-8") as f:
            f.write(
                f"### Bundles under test\n\nCommit `{sha}`, built by CI run "
                f"[{run_id}]({server}/{repo()}/actions/runs/{run_id}).\n"
            )
    return 0


def release(sha: str) -> int:
    if not re.match(r"^[0-9a-f]{40}$", sha or ""):
        raise Refused(f"not a full commit sha: {sha!r}")
    r = repo()
    runs = api(f"repos/{r}/actions/workflows/install-smoke.yml/runs?branch={DEFAULT_BRANCH}&per_page=100")
    candidates = []
    for run in runs.get("workflow_runs", []):
        if run.get("path") != SMOKE_WORKFLOW or run.get("head_branch") != DEFAULT_BRANCH:
            continue
        m = TITLE.match(run.get("display_title") or "")
        if not m:
            continue
        try:
            ci = master_ci_run(m.group(1))
        except Refused as e:
            print(f"  ignored run {run.get('id')}: {e}")
            continue
        if ci["head_sha"] == sha:
            candidates.append(run)
    if not candidates:
        raise Refused(
            f"no Install smoke run on {DEFAULT_BRANCH} installed the bundles of a successful "
            f"CI push of {sha}"
        )
    newest = max(candidates, key=lambda run: (run.get("created_at") or "", run.get("id") or 0))
    url = newest.get("html_url")
    if newest.get("status") != "completed" or newest.get("conclusion") != "success":
        raise Refused(
            f"the newest Install smoke of {sha} is {newest.get('status')}/{newest.get('conclusion')}: {url}"
        )
    run_id = newest.get("id")
    if not isinstance(run_id, int):
        raise Refused(f"the Install smoke run has no numeric id: {run_id!r}")
    jobs = {j.get("name"): j.get("conclusion") for j in api(f"repos/{r}/actions/runs/{run_id}/jobs?per_page=100").get("jobs", [])}
    missing = [f"{name}: {jobs.get(name, 'absent')}" for name in EXPECTED_JOBS if jobs.get(name) != "success"]
    if missing:
        raise Refused(f"the newest Install smoke of {sha} did not pass every platform ({', '.join(missing)}): {url}")
    print(f"Install smoke of {sha}: every platform passed: {url}")
    return 0


def main(argv: list[str]) -> int:
    if len(argv) != 2 or argv[0] not in ("source", "release"):
        print(__doc__)
        return 2
    try:
        return source(argv[1]) if argv[0] == "source" else release(argv[1])
    except Refused as e:
        print(f"::error title=Install smoke gate::{e}")
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
