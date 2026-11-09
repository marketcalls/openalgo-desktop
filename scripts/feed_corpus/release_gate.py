#!/usr/bin/env python3
"""Whether the feed corpus lets 1.0.7 ship (audit C09, review R107-15).

    python3 scripts/feed_corpus/release_gate.py           print what holds it back
    python3 scripts/feed_corpus/release_gate.py --check   fail while anything does

1.0.7 ships only when:

- every deviation a case declares names the lead's or the maintainer's rule
  it follows (`"approval"`, a rule of `approvals.json`; see `approve.py`),
  and none still carries the old `approved` flag;
- no case declares an open gap (kind `open`);
- every capability the web offers (LTP, Quote, each depth level, order
  updates, per `broker.json`) for every broker of the catalog is covered
  by a case, and every broker of the catalog has its `broker.json`;
- every broker module of the harness builds its session's broker
  (`Feeds::with_broker`): the `WITHOUT_BROKER` list in
  `src-tauri/tests/it/feed_corpus/mod.rs` is empty;
- every case has the web's output (`web.json`) or says it is worked out by
  hand, and the corpus is exactly the 38 brokers of the catalog.

The steps the web replay has no behaviour for (`web.json` `uncompared`:
refused upgrades, a deliberate disconnect, a deep feed not made) are counted
here. They do not hold the release back.

A case's own result holds when its test passes (lane-test or CI); this gate
reads the corpus's declarations and coverage, not test results. It is
separate from `parity_matrix.py --check`, which only says whether the
generated matrix is current. Standard library only.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import parity_matrix as pm  # noqa: E402

MODULES = pm.ROOT / "src-tauri/tests/it/feed_corpus/mod.rs"
# The catalog's size: a shrunken or unread list would otherwise pass.
CATALOG_BROKERS = 38


def without_broker() -> list[str]:
    """The harness modules not yet building their broker (mod.rs)."""
    text = MODULES.read_text(encoding="utf-8")
    body = re.search(r"const WITHOUT_BROKER: &\[&str\] = &\[(.*?)\];", text, re.S)
    if not body:
        raise SystemExit("WITHOUT_BROKER not found in the corpus's mod.rs")
    return re.findall(r'"([a-z0-9_]+)"', body.group(1))


def catalog_holds() -> list[str]:
    """The corpus is exactly the catalog's brokers: a shrunken catalog would
    leave nothing to check, and a folder of no catalog broker escapes it."""
    catalog = pm.brokers()
    out = []
    if len(catalog) != CATALOG_BROKERS:
        out.append(f"the catalog lists {len(catalog)} brokers, not {CATALOG_BROKERS}")
    folders = {p.name for p in pm.CORPUS.iterdir() if p.is_dir()}
    out += [f"{name}: a corpus folder of no catalog broker" for name in sorted(folders - set(catalog))]
    return out


def uncompared() -> list[str]:
    """The steps the web replay does not model, case by case."""
    return [
        f"{web.parent.parent.name}/{web.parent.name} line {step['line']}: {step['step']}"
        for web in sorted(pm.CORPUS.glob("*/*/web.json"))
        for step in json.loads(web.read_text(encoding="utf-8")).get("uncompared", [])
    ]


def holds() -> dict[str, list[str]]:
    """What holds the release back, by reason."""
    rules = pm.approval_rules()
    found = {
        "pending": [],
        "old flag": [],
        "open": [],
        "uncovered": [],
        "without broker": [],
        "no web output": [],
        "catalog": catalog_holds(),
    }
    for broker in pm.brokers():
        bdir = pm.CORPUS / broker
        meta_path = bdir / "broker.json"
        if not meta_path.exists():
            found["uncovered"].append(f"{broker}: no broker.json (what the web offers is not recorded)")
            continue
        web = json.loads(meta_path.read_text(encoding="utf-8")).get("web")
        covered = set()
        for d in sorted(p for p in bdir.glob("*") if p.is_dir()):
            case = json.loads((d / "case.json").read_text(encoding="utf-8"))
            covered |= pm.covers(d)
            if not (d / "web.json").exists() and case.get("provenance", {}).get("method") != "by-hand":
                found["no web output"].append(f"{broker}/{d.name}")
            for dv in case.get("deviations", []):
                where = f"{broker}/{d.name} {dv.get('key')} [{dv.get('kind')}]"
                if dv.get("kind") == "open":
                    found["open"].append(f"{where}: {dv.get('reason', '')}")
                elif not pm.approved(dv, rules):
                    found["pending"].append(where)
                if "approved" in dv:
                    found["old flag"].append(where)
        for col, title in pm.COLUMNS:
            if pm.offered(web, col) and col not in covered:
                found["uncovered"].append(f"{broker}: {title} is offered by the web and no case covers it")
    found["without broker"] = [f"{b}: its harness module does not pass its broker" for b in without_broker()]
    return found


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--check", action="store_true", help="fail while anything holds 1.0.7 back")
    ap.add_argument("--all", action="store_true", help="list every item, not the first 20 of each reason")
    args = ap.parse_args()
    found = holds()
    titles = {
        "pending": "deviations naming no rule in force (approve.py set RULE ...)",
        "old flag": "deviations still carrying \"approved\" (approve.py set drops it)",
        "open": "open gaps (fix them; an open gap is never approved)",
        "uncovered": "capabilities the web offers with no case",
        "without broker": "harness modules without their broker (Feeds::with_broker)",
        "no web output": "cases without web.json that are not worked out by hand",
        "catalog": "the corpus is not exactly the catalog's brokers",
    }
    total = 0
    for reason, items in found.items():
        total += len(items)
        print(f"{titles[reason]}: {len(items)}")
        for item in items if args.all else items[:20]:
            print(f"  {item}")
        if len(items) > 20 and not args.all:
            print(f"  ... and {len(items) - 20} more (--all)")
    steps = uncompared()
    print(f"steps the web replay does not model (counted, not a hold): {len(steps)}")
    for step in steps if args.all else steps[:20]:
        print(f"  {step}")
    if total:
        print("1.0.7 cannot ship from this corpus yet.")
        return 1 if args.check else 0
    print("The feed corpus lets 1.0.7 ship.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
