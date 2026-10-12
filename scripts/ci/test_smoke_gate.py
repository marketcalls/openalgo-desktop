#!/usr/bin/env python3
"""Tests of scripts/ci/smoke_gate.py against fabricated GitHub API answers.

    python3 scripts/ci/test_smoke_gate.py

No network: every API path is answered from a temporary fixture folder
(SMOKE_GATE_FIXTURES), and a path with no fixture is refused, which is how
these tests see that a crafted id never reaches the API.
"""

from __future__ import annotations

import io
import json
import os
import re
import sys
import tempfile
import unittest
from contextlib import redirect_stdout

sys.dont_write_bytecode = True  # leave no __pycache__ in the checkout
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import smoke_gate  # noqa: E402

REPO = "marketcalls/openalgo-desktop"
SHA = "6c804e51853ac878b95d82c622faa5bb49f0cd69"
OTHER_SHA = "aa73a473aec96ac273a5ed6a649f341a17e4d1f2"
ALL_JOBS = list(smoke_gate.EXPECTED_JOBS)


def ci_run(run_id, sha=SHA, **over):
    run = {
        "id": run_id,
        "path": ".github/workflows/ci.yml",
        "event": "push",
        "head_branch": "master",
        "head_repository": {"full_name": REPO},
        "conclusion": "success",
        "head_sha": sha,
    }
    run.update(over)
    return run


def smoke_run(run_id, title, created, conclusion="success", **over):
    run = {
        "id": run_id,
        "path": ".github/workflows/install-smoke.yml",
        "head_branch": "master",
        "display_title": title,
        "status": "completed",
        "conclusion": conclusion,
        "created_at": created,
        "html_url": f"https://example.invalid/runs/{run_id}",
    }
    run.update(over)
    return run


class Gate(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp()
        os.environ["SMOKE_GATE_FIXTURES"] = self.dir
        os.environ["GITHUB_REPOSITORY"] = REPO
        os.environ.pop("GITHUB_OUTPUT", None)
        os.environ.pop("GITHUB_STEP_SUMMARY", None)

    def put(self, path, body):
        name = re.sub(r"[/?&=]", "_", path) + ".json"
        with open(os.path.join(self.dir, name), "w", encoding="utf-8") as f:
            json.dump(body, f)

    def put_ci(self, run):
        self.put(f"repos/{REPO}/actions/runs/{run['id']}", run)

    def put_smoke(self, runs, jobs_by_run=None):
        self.put(f"repos/{REPO}/actions/workflows/install-smoke.yml/runs?branch=master&per_page=100", {"workflow_runs": runs})
        for run_id, jobs in (jobs_by_run or {}).items():
            self.put(
                f"repos/{REPO}/actions/runs/{run_id}/jobs?per_page=100",
                {"jobs": [{"name": n, "conclusion": c} for n, c in jobs.items()]},
            )

    def run_gate(self, *argv):
        out = io.StringIO()
        with redirect_stdout(out):
            code = smoke_gate.main(list(argv))
        return code, out.getvalue()

    # ---------------------------------------------------------------- source

    def test_source_accepts_a_successful_master_push(self):
        self.put_ci(ci_run(100))
        code, out = self.run_gate("source", "100")
        self.assertEqual(code, 0, out)
        self.assertIn(SHA, out)

    def test_source_refuses_a_non_digit_id(self):
        code, out = self.run_gate("source", "100abc")
        self.assertEqual(code, 1)
        self.assertIn("digits only", out)

    def test_source_refuses_a_path_traversal_id(self):
        # A fixture exists where the traversal would land; it must not be read.
        self.put(f"repos/{REPO}/check-suites/5", ci_run(5))
        code, out = self.run_gate("source", "1/../../check-suites/5")
        self.assertEqual(code, 1)
        self.assertIn("digits only", out)

    def test_source_refuses_a_pull_request_run(self):
        self.put_ci(ci_run(101, event="pull_request", head_branch="pr/x"))
        code, out = self.run_gate("source", "101")
        self.assertEqual(code, 1)
        self.assertIn("event is 'pull_request'", out)

    def test_source_refuses_a_failed_run(self):
        self.put_ci(ci_run(102, conclusion="failure"))
        code, out = self.run_gate("source", "102")
        self.assertEqual(code, 1)
        self.assertIn("conclusion", out)

    def test_source_refuses_a_fork_and_another_workflow(self):
        self.put_ci(ci_run(103, head_repository={"full_name": "someone/fork"}))
        self.put_ci(ci_run(104, path=".github/workflows/soak.yml"))
        self.assertEqual(self.run_gate("source", "103")[0], 1)
        self.assertEqual(self.run_gate("source", "104")[0], 1)

    # --------------------------------------------------------------- release

    def test_release_accepts_a_green_smoke_of_master_bundles(self):
        self.put_ci(ci_run(100))
        self.put_smoke(
            [smoke_run(900, f"Install smoke of {SHA} (CI run 100)", "2026-10-12T01:00:00Z")],
            {900: {name: "success" for name in ALL_JOBS}},
        )
        code, out = self.run_gate("release", SHA)
        self.assertEqual(code, 0, out)

    def test_release_accepts_a_hand_run_named_after_the_ci_run(self):
        self.put_ci(ci_run(100))
        self.put_smoke(
            [smoke_run(901, "Install smoke of CI run 100", "2026-10-12T01:00:00Z")],
            {901: {name: "success" for name in ALL_JOBS}},
        )
        self.assertEqual(self.run_gate("release", SHA)[0], 0)

    def test_release_refuses_when_a_newer_run_failed(self):
        self.put_ci(ci_run(100))
        self.put_smoke(
            [
                smoke_run(900, f"Install smoke of {SHA} (CI run 100)", "2026-10-12T01:00:00Z"),
                smoke_run(902, "Install smoke of CI run 100", "2026-10-12T03:00:00Z", conclusion="failure"),
            ],
            {900: {name: "success" for name in ALL_JOBS}},
        )
        code, out = self.run_gate("release", SHA)
        self.assertEqual(code, 1)
        self.assertIn("completed/failure", out)

    def test_release_refuses_a_skipped_windows_job(self):
        self.put_ci(ci_run(100))
        jobs = {name: "success" for name in ALL_JOBS}
        jobs["windows (windows-2025)"] = "skipped"
        self.put_smoke([smoke_run(903, f"Install smoke of {SHA} (CI run 100)", "2026-10-12T01:00:00Z")], {903: jobs})
        code, out = self.run_gate("release", SHA)
        self.assertEqual(code, 1)
        self.assertIn("windows (windows-2025): skipped", out)

    def test_release_refuses_a_run_missing_a_platform(self):
        self.put_ci(ci_run(100))
        jobs = {name: "success" for name in ALL_JOBS if not name.startswith("macos")}
        self.put_smoke([smoke_run(904, f"Install smoke of {SHA} (CI run 100)", "2026-10-12T01:00:00Z")], {904: jobs})
        code, out = self.run_gate("release", SHA)
        self.assertEqual(code, 1)
        self.assertIn("macos (aarch64): absent", out)

    def test_release_ignores_a_crafted_title(self):
        self.put(f"repos/{REPO}/check-suites/5", ci_run(5))
        self.put_smoke(
            [
                smoke_run(905, "Install smoke of CI run 1/../../check-suites/5", "2026-10-12T01:00:00Z"),
                smoke_run(906, f"Install smoke of {SHA}", "2026-10-12T02:00:00Z"),
            ],
            {905: {n: "success" for n in ALL_JOBS}, 906: {n: "success" for n in ALL_JOBS}},
        )
        code, out = self.run_gate("release", SHA)
        self.assertEqual(code, 1)
        self.assertIn("no Install smoke run", out)

    def test_release_ignores_a_pull_request_ci_run_of_the_same_commit(self):
        self.put_ci(ci_run(107, event="pull_request", head_branch="pr/x"))
        self.put_smoke(
            [smoke_run(907, "Install smoke of CI run 107", "2026-10-12T01:00:00Z")],
            {907: {n: "success" for n in ALL_JOBS}},
        )
        code, out = self.run_gate("release", SHA)
        self.assertEqual(code, 1)
        self.assertIn("ignored run 907", out)

    def test_release_ignores_a_smoke_run_on_another_branch_or_of_another_commit(self):
        self.put_ci(ci_run(100))
        self.put_ci(ci_run(108, sha=OTHER_SHA))
        self.put_smoke(
            [
                smoke_run(908, "Install smoke of CI run 100", "2026-10-12T01:00:00Z", head_branch="wip/x"),
                smoke_run(909, f"Install smoke of {OTHER_SHA} (CI run 108)", "2026-10-12T02:00:00Z"),
            ],
            {908: {n: "success" for n in ALL_JOBS}, 909: {n: "success" for n in ALL_JOBS}},
        )
        self.assertEqual(self.run_gate("release", SHA)[0], 1)

    def test_release_refuses_a_short_or_odd_sha(self):
        self.assertEqual(self.run_gate("release", "6c804e5")[0], 1)


if __name__ == "__main__":
    unittest.main(verbosity=2)
