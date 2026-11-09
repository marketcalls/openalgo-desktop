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
  `src-tauri/tests/it/feed_corpus/mod.rs` is empty.

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


def without_broker() -> list[str]:
    """The harness modules not yet building their broker (mod.rs)."""
    text = MODULES.read_text(encoding="utf-8")
    body = re.search(r"const WITHOUT_BROKER: &\[&str\] = &\[(.*?)\];", text, re.S)
    if not body:
        raise SystemExit("WITHOUT_BROKER not found in the corpus's mod.rs")
    return re.findall(r'"([a-z0-9_]+)"', body.group(1))


def holds() -> dict[str, list[str]]:
    """What holds the release back, by reason."""
    rules = pm.approval_rules()
    found = {"pending": [], "old flag": [], "open": [], "uncovered": [], "without broker": []}
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
    }
    total = 0
    for reason, items in found.items():
        total += len(items)
        print(f"{titles[reason]}: {len(items)}")
        for item in items if args.all else items[:20]:
            print(f"  {item}")
        if len(items) > 20 and not args.all:
            print(f"  ... and {len(items) - 20} more (--all)")
    if total:
        print("1.0.7 cannot ship from this corpus yet.")
        return 1 if args.check else 0
    print("The feed corpus lets 1.0.7 ship.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
