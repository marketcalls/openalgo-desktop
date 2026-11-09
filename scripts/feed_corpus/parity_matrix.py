#!/usr/bin/env python3
"""Write docs/port/feed-parity-matrix.md from the feed corpus.

    python3 scripts/feed_corpus/parity_matrix.py           write it
    python3 scripts/feed_corpus/parity_matrix.py --check   fail when it is stale

One row per broker of the desktop catalog (`brokers/catalog.rs`
ALL_BROKERS), one column per mode and depth and one for order updates. A
cell names the corpus cases that cover it and their result:

- what the web offers comes from `feed_corpus/<broker>/broker.json`
  (`web`: ltp, quote, depth levels, orders), with its web sources; a cell
  the web does not offer reads "not offered by the web";
- the modes and depths a case covers come from its script's subscribes,
  order updates from a `subscribe_orders`;
- a case "matches web" when it declares no deviation, and is checked
  against the web's own output when it has `web.json` (else it is worked
  by hand, marked "by hand"); otherwise it lists its deviations by kind,
  approved (`case.json` `deviations[].approval` names a rule of
  `approvals.json`, the lead's or the maintainer's decision it follows) or
  pending, and any `open` one is a gap 1.0.7 cannot ship with;
- a scenario the broker's stream cannot produce is named in its
  `broker.json` `not_applicable` (`{"partial-packet": "why"}`) and listed
  as not applicable, with the reason, instead of as not yet covered.

A case's result holds when its test passes (lane-test or CI). This is a
freshness check of the generated file; whether 1.0.7 can ship is
`release_gate.py --check`. Standard library only.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CORPUS = ROOT / "src-tauri/tests/fixtures/feed_corpus"
CATALOG = ROOT / "src-tauri/src/brokers/catalog.rs"
OUT = ROOT / "docs/port/feed-parity-matrix.md"

COLUMNS = [("ltp", "LTP"), ("quote", "Quote"), (5, "Depth 5"), (20, "Depth 20"), (30, "Depth 30"),
           (50, "Depth 50"), ("orders", "Order updates")]
MODES = {"ltp": 1, "quote": 2, "depth": 3}
HARNESS_CASE = ROOT / "src-tauri/tests/it/feed_corpus/harness/case.rs"
APPROVALS = CORPUS / "approvals.json"


def approval_rules() -> dict:
    """The rules a deviation's `approval` may name (approvals.json)."""
    return json.loads(APPROVALS.read_text(encoding="utf-8"))["rules"]


def approved(dv: dict, rules: dict) -> bool:
    """A deviation names the rule in force it follows (never an open gap,
    never a rule a later decision withdrew)."""
    rule = rules.get(dv.get("approval"))
    return dv.get("kind") != "open" and rule is not None and "superseded_by" not in rule


def scenarios() -> list[str]:
    """The scenario names, from the harness that checks them (case.rs)."""
    text = HARNESS_CASE.read_text(encoding="utf-8")
    body = re.search(r"const SCENARIOS: &\[&str\] = &\[(.*?)\];", text, re.S)
    if not body:
        raise SystemExit("SCENARIOS not found in the harness's case.rs")
    return re.findall(r'"([a-z-]+)"', body.group(1))


def deviation_kinds() -> list[str]:
    """The deviation kinds, from the harness that checks them (case.rs
    DEVIATION_KINDS, its named constants resolved)."""
    text = HARNESS_CASE.read_text(encoding="utf-8")
    body = re.search(r"const DEVIATION_KINDS: &\[&str\] = &\[(.*?)\];", text, re.S)
    if not body:
        raise SystemExit("DEVIATION_KINDS not found in the harness's case.rs")
    named = dict(re.findall(r'const ([A-Z_]+): &str = "([a-z-]+)";', text))
    kinds = []
    for item in re.findall(r'"[a-z-]+"|[A-Z_]+', body.group(1)):
        kind = item.strip('"') if item.startswith('"') else named.get(item)
        if kind is None:
            raise SystemExit(f"DEVIATION_KINDS names {item}, which case.rs does not define")
        kinds.append(kind)
    return kinds


# The brokers whose web feed is synthetic, so that its values are drawn at
# random and only they may declare `random-values` (harness/case.rs).
SYNTHETIC_FEEDS = ("dhan_sandbox",)


def pattern_matches(pattern: str, value: str) -> bool:
    """A scope's list item: an exact name, or a prefix ending in `*`
    (harness/approvals.rs `pattern_matches`)."""
    return value.startswith(pattern[:-1]) if pattern.endswith("*") else value == pattern


def clause_fits(
    clause: dict, broker: str, key: str, kind: str, instruments: list[dict], steps: set[str], allows: list
) -> bool:
    """Is this deviation within this clause of a rule's scope?"""
    if "allows" in clause and not all(a in clause["allows"] for a in allows):
        return False
    for field, value in (("brokers", broker), ("keys", key), ("kinds", kind)):
        if field in clause and not any(pattern_matches(p, value) for p in clause[field]):
            return False
    index = any(str(i.get("exchange", "")).endswith("_INDEX") for i in instruments)
    non_index = any(not str(i.get("exchange", "")).endswith("_INDEX") for i in instruments)
    if clause.get("instrument") == "index" and not index:
        return False
    if clause.get("instrument") == "non-index" and not non_index:
        return False
    return not ("after_step" in clause and clause["after_step"] not in steps)


def in_scope(
    scope: dict, broker: str, key: str, kind: str, instruments: list[dict], steps: set[str], allows: list
) -> bool:
    """Is this deviation within the words of its rule (`scope`, any of its
    `any_of` clauses when it has them)?"""
    clauses = scope.get("any_of", [scope])
    return any(clause_fits(c, broker, key, kind, instruments, steps, allows) for c in clauses)


def script_steps(case_dir: Path) -> set[str]:
    """The kinds of step a case's script has (a scope's `after_step`)."""
    return {
        json.loads(line)["step"]
        for line in (case_dir / "script.jsonl").read_text(encoding="utf-8").splitlines()
        if line.strip()
    }


def deviation_problems(
    name: str, case: dict, kinds: list[str], rules: dict | None = None, steps: set[str] | None = None
) -> list[str]:
    """What the matrix could not count in a case's deviations (as the
    harness's `deviation_problems` checks them)."""
    rules = approval_rules() if rules is None else rules
    out = []
    for i, dv in enumerate(case.get("deviations", []), 1):
        label = f"{name}: deviation {dv.get('key') or i}"
        if dv.get("kind") not in kinds:
            out.append(f"{label}: kind {dv.get('kind')!r} is not one of {', '.join(kinds)}")
        if dv.get("kind") == "random-values" and case.get("broker") not in SYNTHETIC_FEEDS:
            out.append(f"{label}: random-values is only for a synthetic feed ({', '.join(SYNTHETIC_FEEDS)})")
        if not str(dv.get("reason", "")).strip():
            out.append(f"{label} gives no reason")
        if not dv.get("allows"):
            out.append(f'{label} has no "allows" (run scripts/feed_corpus/predicates.py)')
        if "approved" in dv and not isinstance(dv["approved"], bool):
            out.append(f"{label}: approved is true or false")
        if "approval" in dv:
            if dv["approval"] not in rules:
                out.append(f"{label}: approval {dv['approval']!r} is no rule in approvals.json")
            elif "superseded_by" not in rules[dv["approval"]]:
                scope = rules[dv["approval"]].get("scope", {})
                fits = in_scope(
                    scope, str(case.get("broker", "")), str(dv.get("key", "")), str(dv.get("kind", "")),
                    case.get("instruments", []), steps or set(), dv.get("allows", []),
                )
                if not fits:
                    out.append(f"{label}: rule {dv['approval']!r} does not cover this deviation (its scope: "
                               f"{json.dumps(scope, sort_keys=True)})")
            else:
                newer = rules[dv["approval"]]["superseded_by"]
                out.append(f"{label}: approval {dv['approval']!r} cites a withdrawn rule: {newer!r} replaced it")
            if dv.get("kind") == "open":
                out.append(f"{label}: an open gap is never approved")
    return out


def brokers() -> list[str]:
    text = CATALOG.read_text(encoding="utf-8")
    body = re.search(r"pub const ALL_BROKERS: &\[&str\] = &\[(.*?)\];", text, re.S)
    if not body:
        raise SystemExit("ALL_BROKERS not found in catalog.rs")
    return re.findall(r'"([a-z0-9_]+)"', body.group(1))


def mode_of(v) -> int:
    if isinstance(v, int) and not isinstance(v, bool):
        return v
    return MODES.get(str(v).strip().lower(), 0)


def script(case_dir: Path) -> list[dict]:
    return [
        json.loads(line)
        for line in (case_dir / "script.jsonl").read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]


def subscribe_cell(msg: dict):
    """The matrix column a `subscribe` message asks for."""
    return {1: "ltp", 2: "quote"}.get(mode_of(msg.get("mode", 1))) or int(msg.get("depth", 5))


def asked(case_dir: Path) -> set:
    """The cells a case's script asks for, whatever the answer."""
    cells = set()
    for step in script(case_dir):
        msg = step.get("message") or {}
        if step.get("step") != "send":
            continue
        if msg.get("action") == "subscribe_orders":
            cells.add("orders")
        elif msg.get("action") == "subscribe":
            cells.add(subscribe_cell(msg))
    return cells


def accepted_items(step: dict) -> list[dict]:
    """The instruments of a `subscribe` step that were accepted: all of them
    with no declared reply (the harness then requires a success), else those
    the reply marks a success."""
    msg = step["message"]
    items = msg.get("symbols") or [msg]
    reply = step.get("reply")
    if reply is None:
        return items
    answers = reply.get("subscriptions")
    if answers is None:
        return items if reply.get("status") == "success" else []
    ok = {(a.get("symbol"), a.get("exchange")) for a in answers if a.get("status") == "success"}
    return [i for i in items if (i.get("symbol"), i.get("exchange")) in ok]


def received_frame(case_dir: Path, spec) -> dict:
    return json.loads((case_dir / spec).read_text(encoding="utf-8")) if isinstance(spec, str) else spec


def covers(case_dir: Path) -> set:
    """The cells a case really exercises: an accepted subscribe, and a frame
    of that instrument and mode the client received after it. A refused
    level, or a subscribe nothing came of, covers nothing."""
    cells = set()
    held: dict[str, list[tuple]] = {}
    for step in script(case_dir):
        client = step.get("client", "")
        if step.get("step") == "send":
            msg = step.get("message") or {}
            if msg.get("action") == "subscribe_orders":
                if step.get("reply") is None or step["reply"].get("status") == "success":
                    held.setdefault(client, []).append(("orders", None, None))
            elif msg.get("action") == "subscribe":
                cell = subscribe_cell(msg)
                for item in accepted_items(step):
                    held.setdefault(client, []).append((cell, item.get("symbol"), item.get("exchange")))
        elif step.get("step") == "receive":
            frame = received_frame(case_dir, step["frame"])
            for cell, symbol, exchange in held.get(client, []):
                if cell == "orders":
                    if frame.get("type") == "order_update":
                        cells.add(cell)
                elif (
                    frame.get("type") == "market_data"
                    and (frame.get("symbol"), frame.get("exchange")) == (symbol, exchange)
                    and frame.get("mode") == {"ltp": 1, "quote": 2}.get(cell, 3)
                ):
                    cells.add(cell)
    return cells


def result(case: dict, has_web: bool, rules: dict) -> tuple[str, int]:
    """The case's result, and its rank (higher is worse)."""
    devs = case.get("deviations", [])
    checked = "" if has_web else " (by hand)"
    if not devs:
        return "matches web" + checked, 0
    open_ = [d for d in devs if d["kind"] == "open"]
    pending = [d for d in devs if d["kind"] != "open" and not approved(d, rules)]
    if open_:
        return f"open gap: {', '.join(d['key'] for d in open_)}" + checked, 3
    if pending:
        return f"deviation pending approval: {', '.join(d['key'] for d in pending)}" + checked, 2
    return "approved deviation" + checked, 1


def web_offers() -> dict:
    """What the web offers, per broker (`web_offers.py`, read from its
    registries at the pinned commit)."""
    path = CORPUS / "web_offers.json"
    return json.loads(path.read_text(encoding="utf-8"))["brokers"] if path.exists() else {}


def capabilities(broker: str, meta: dict | None, offers: dict) -> dict | None:
    """The web's capabilities for a broker: its registries' depth levels and
    order updates, with LTP and Quote from `broker.json` (true for all 38).
    `None` where they are not recorded."""
    if meta is None or broker not in offers:
        return None
    web = meta.get("web") or {}
    return {
        "ltp": web.get("ltp"),
        "quote": web.get("quote"),
        "depth": offers[broker]["depth"],
        "orders": offers[broker]["orders"],
    }


def offered(web: dict | None, col) -> bool | None:
    """Whether the web offers a column (None: not recorded)."""
    if web is None:
        return None
    if col in ("ltp", "quote"):
        return bool(web.get(col))
    if col == "orders":
        return bool(web.get("orders"))
    depth = web.get("depth", [])
    levels = {d for v in depth.values() for d in v} if isinstance(depth, dict) else set(depth)
    return col in levels


def build() -> str:
    names = scenarios()
    kinds = deviation_kinds()
    rules = approval_rules()
    problems = []
    rows, devs, scen = [], [], []
    totals = {"cases": 0, "open": 0, "pending": 0}
    offers = web_offers()
    for broker in brokers():
        bdir = CORPUS / broker
        meta = json.loads((bdir / "broker.json").read_text()) if (bdir / "broker.json").exists() else None
        web = capabilities(broker, meta, offers)
        cases = sorted(d for d in bdir.glob("*") if d.is_dir()) if bdir.exists() else []
        cells = {c: [] for c, _ in COLUMNS}
        asked_for = {c: [] for c, _ in COLUMNS}
        seen = set()
        for d in cases:
            case = json.loads((d / "case.json").read_text())
            problems += deviation_problems(f"{broker}/{d.name}", case, kinds, rules, script_steps(d))
            res, rank = result(case, (d / "web.json").exists(), rules)
            totals["cases"] += 1
            for dv in case.get("deviations", []):
                if dv["kind"] == "open":
                    totals["open"] += 1
                elif not approved(dv, rules):
                    totals["pending"] += 1
                devs.append((broker, d.name, dv))
            seen |= set(case.get("scenarios", []))
            for cell in covers(d):
                if cell in cells:
                    cells[cell].append((d.name, res, rank))
            for cell in asked(d):
                if cell in asked_for:
                    asked_for[cell].append(d.name)
        line = [broker]
        for col, _ in COLUMNS:
            got = cells[col]
            off = offered(web, col)
            if off is False and asked_for[col]:
                # A case there shows what the desktop answers to it.
                line.append("not offered by the web (asked in: " + ", ".join(f"`{n}`" for n in asked_for[col]) + ")")
            elif got:
                worst = max(r for _, _, r in got)
                label = ["matches web", "approved deviation", "deviation pending approval", "open gap"][worst]
                listed = ", ".join(
                    f"`{n}`" + (" (by hand)" if res.endswith("(by hand)") else "") for n, res, _ in got
                )
                line.append(f"{label}: {listed}")
            elif off is False:
                line.append("not offered by the web")
            elif off is None:
                line.append("not recorded")
            else:
                line.append("**no case**")
        rows.append(line)
        missing = [s for s in names if s not in seen]
        if offered(web, "orders") is False:
            # A broker whose web adapter streams no order updates (Sahi,
            # HDFC) has no order scenario to cover.
            missing = [s for s in missing if not s.startswith("order-")]
            missing.append("(order updates: not offered by the web)")
        not_applicable = (meta or {}).get("not_applicable", {})
        unknown = sorted(set(not_applicable) - set(names))
        if unknown:
            problems.append(f"{broker} broker.json: not_applicable names no scenario: {unknown}")
        blank = sorted(s for s, why in not_applicable.items() if not str(why).strip())
        if blank:
            problems.append(f"{broker} broker.json: not_applicable gives no reason for {blank}")
        covered = sorted(set(not_applicable) & seen)
        if covered:
            problems.append(f"{broker} broker.json: not_applicable names scenarios a case covers: {covered}")
        missing = [s for s in missing if s not in not_applicable]
        missing += [f"({s}: not applicable: {why})" for s, why in sorted(not_applicable.items())]
        if cases:
            scen.append((broker, sorted(seen, key=names.index), missing))
    if problems:
        raise SystemExit("deviations the matrix cannot count:\n  " + "\n  ".join(problems))
    out = [
        "# Feed parity matrix",
        "",
        "Generated by `scripts/feed_corpus/parity_matrix.py` from the feed corpus",
        "(`src-tauri/tests/fixtures/feed_corpus`); do not edit by hand. A cell lists the",
        "corpus cases covering that broker, mode and depth, or its order updates, with",
        "the worst result among them; what the web offers is each broker's",
        "`broker.json`. A result holds when the case's test passes in CI. A deviation is",
        "approved when it names the rule it follows (`approvals.json`). 1.0.7 ships when",
        "`scripts/feed_corpus/release_gate.py --check` passes: every cell \"matches web\",",
        "\"approved deviation\" or \"not offered by the web\", and every module builds its broker.",
        "",
        f"Cases: {totals['cases']}. Open gaps: {totals['open']}. Deviations pending approval: {totals['pending']}.",
        "",
        "| Broker | " + " | ".join(t for _, t in COLUMNS) + " |",
        "| --- | " + " | ".join("---" for _ in COLUMNS) + " |",
    ]
    out += ["| " + " | ".join(r) + " |" for r in rows]
    out += ["", "## Scenarios covered", "", "| Broker | Covered | Not yet |", "| --- | --- | --- |"]
    out += [f"| {b} | {', '.join(c) or '-'} | {', '.join(m) or '-'} |" for b, c, m in scen]
    out += ["", "## Deviations", "", "| Broker | Case | Key | Kind | Approval | Reason |", "| --- | --- | --- | --- | --- | --- |"]
    for b, c, d in devs:
        reason = d["reason"].replace("|", "/")
        rule = f"`{d['approval']}`" if approved(d, rules) else "pending"
        out.append(f"| {b} | `{c}` | `{d['key']}` | {d['kind']} | {rule} | {reason} |")
    return "\n".join(out) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--check", action="store_true", help="fail when the matrix is stale")
    args = ap.parse_args()
    text = build()
    if args.check:
        if not OUT.exists() or OUT.read_text(encoding="utf-8") != text:
            print(f"{OUT.relative_to(ROOT)} is stale: run scripts/feed_corpus/parity_matrix.py")
            return 1
        return 0
    OUT.write_text(text, encoding="utf-8")
    print(f"wrote {OUT.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
