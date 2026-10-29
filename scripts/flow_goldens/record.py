"""Record every scenario into tests/fixtures/web/flow/goldens/<family>/.

Usage: <web>/.venv/bin/python -I record.py <out_dir> [filter]
(see harness.py for the web export it reads)
"""

from __future__ import annotations

import copy
import json
import logging
import os
import shutil
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import harness  # noqa: E402  (sets the environment before anything web loads)
import fixes  # noqa: E402
import lib  # noqa: E402
import scenarios_core  # noqa: E402,F401
import scenarios_data  # noqa: E402,F401
import scenarios_logic  # noqa: E402,F401
import scenarios_orders  # noqa: E402,F401
import scenarios_triggers  # noqa: E402,F401
import scenarios_workflows  # noqa: E402,F401

WEB_COMMIT = "96e5927e5"

# Module loggers other than the executor's (whose lines are the run log):
# what the web writes to its server log, kept as evidence, never compared.
SERVER_LOG: list = []


class _Capture(logging.Handler):
    def emit(self, record):
        if record.name != "services.flow_executor_service":
            SERVER_LOG.append([record.levelname.lower(), record.getMessage()])


root = logging.getLogger()
for h in list(root.handlers):
    root.removeHandler(h)
root.addHandler(_Capture())
root.setLevel(logging.INFO)
for name in list(logging.Logger.manager.loggerDict):
    lg = logging.getLogger(name)
    for h in list(lg.handlers):
        lg.removeHandler(h)
    lg.propagate = True


def run(s: dict):
    SERVER_LOG.clear()
    got = harness.run_node(s) if s["kind"] == "node" else harness.run_workflow(s)
    got["server_log"] = list(SERVER_LOG)
    return got


def expected_of(s: dict, got: dict) -> dict:
    if s["kind"] == "node":
        keys = ("calls", "outputs", "outputs_by", "vars_out", "conditions_out", "logs", "next", "error")
    else:
        keys = ("calls", "result", "record", "vars_out", "logs")
    out = {k: got[k] for k in keys}
    for extra in ("alerts", "feed", "subscriptions", "http_requests", "sleeps", "server_log"):
        if got[extra]:
            out[extra] = got[extra]
    return harness.to_json(out)


def desktop(s: dict, expected: dict, web_script: list) -> dict:
    d = copy.deepcopy(s["desktop_expected"])
    out = {"parity": d.pop("parity"), "why": d.pop("why")}
    names = d.pop("fix", None)
    script_extra = d.pop("script_extra", None)
    sets = d.pop("set", {})
    sets.update(d)  # any other key is an explicit replacement value
    if names:
        out["fix"] = names
        with fixes.applied(names):
            got = run(s)
        fixed = expected_of(s, got)
        for k, v in fixed.items():
            if k != "server_log" and expected.get(k) != v:
                out[k] = v
        for k in expected:
            if k not in fixed and k != "server_log":
                out[k] = None
        if harness.to_json(got["script"]) != web_script:
            out["script"] = harness.to_json(got["script"])
    if script_extra:
        out["script_extra"] = script_extra
    out.update(harness.to_json(sets))
    if set(out) <= {"parity", "why", "fix"}:
        return None  # the fix changes nothing in this scenario
    return out


def golden(s: dict) -> dict:
    got = run(s)
    expected = expected_of(s, got)
    if s["kind"] == "node":
        body = {
            "node": {"type": s["type"], "data": s["graph"]["nodes"][0]["data"]},
            "workflow_name": s["workflow_name"],
            "vars_in": s["vars_in"],
            "conditions_in": s["conditions_in"],
            "graph": s["graph"],
            "start": s["start"],
        }
    else:
        body = {"workflow": s["workflow"], "webhook": s["webhook"], "api_key": s["api_key"]}
        if s.get("special"):
            body["special"] = s["special"]
    if s.get("run_context"):
        body["run_context"] = s["run_context"]
    out = {
        "format": 1,
        "web_commit": WEB_COMMIT,
        "kind": s["kind"],
        "family": s["family"],
        "type": s["type"],
        "scenario": s["scenario"],
        "note": s["note"],
        "parity": s["parity"],
        "clock": s["clock"],
        "analyzer": s["analyzer"],
        **body,
        "env": s["env"],
        "script": harness.to_json(got["script"]),
        "alert_reply": s["alert_reply"],
        "expected": expected,
    }
    if s.get("source"):
        out["source"] = s["source"]
    if s["desktop_expected"]:
        de = desktop(s, expected, out["script"])
        if de:
            out["desktop_expected"] = de
    return harness.to_json(out)


def resolve_from(goldens: dict) -> None:
    """`$from:<file>` in desktop_expected takes that key from another golden
    of the same family (what the fixed node does is what that one does)."""
    for (fam, _), g in goldens.items():
        de = g.get("desktop_expected")
        if not de:
            continue
        for k, v in list(de.items()):
            if isinstance(v, str) and v.startswith("$from:"):
                other = goldens[(fam, v[len("$from:"):])]
                de[k] = copy.deepcopy(other["expected"][k])


def main() -> int:
    out_dir = sys.argv[1]
    only = sys.argv[2] if len(sys.argv) > 2 else None
    if not only and os.path.isdir(out_dir):
        for fam in os.listdir(out_dir):
            p = os.path.join(out_dir, fam)
            if os.path.isdir(p):
                shutil.rmtree(p)
    scenarios_orders.apply_auto()
    scenarios_data.apply_auto()
    scenarios_logic.apply_auto()
    goldens: dict = {}
    for s in lib.SCENARIOS:
        fname = f"{s['type']}__{s['scenario']}.json"
        key = (s["family"], fname)
        if key in goldens:
            raise SystemExit(f"duplicate scenario {key}")
        if only and only not in fname:
            continue
        goldens[key] = golden(s)
    resolve_from(goldens)
    for (fam, fname), g in goldens.items():
        d = os.path.join(out_dir, fam)
        os.makedirs(d, exist_ok=True)
        with open(os.path.join(d, fname), "w") as f:
            json.dump(g, f, indent=1, ensure_ascii=False, allow_nan=False)
            f.write("\n")
    print(f"recorded {len(goldens)} goldens", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
