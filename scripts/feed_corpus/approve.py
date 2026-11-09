#!/usr/bin/env python3
"""Record which lead or maintainer rule a corpus deviation follows.

    python3 scripts/feed_corpus/approve.py rules
    python3 scripts/feed_corpus/approve.py list [--broker B] [--case C] [--kind K] [--pending]
    python3 scripts/feed_corpus/approve.py set RULE --broker B [--case C] [--key K]
            [--kind K] [--reason-has TEXT] [--dry-run]
    python3 scripts/feed_corpus/approve.py add-rule ID --by lead|maintainer
            --source WHERE --quote TEXT [--date YYYY-MM-DD]

Every deviation a case declares (`case.json` `deviations`) names the rule it
follows in `"approval"`: an id in `src-tauri/tests/fixtures/feed_corpus/
approvals.json`, where each rule quotes the decision (who, when, where it
is written). `set` writes that id on every matching deviation and drops the
old `approved` flag; it refuses an open gap (kind `open` is never
approved) and a rule the registry does not have. `add-rule` adds a rule,
quoting the decision exactly as written (the lane record, the review-fixes
file, or the lead's message: say which in --source). The harness checks
every approval names a rule; `release_gate.py --check` fails while any
deviation has none.

A case.json is rewritten only when one of its deviations changes, in the
JSON layout it already has. Standard library only.
"""

from __future__ import annotations

import argparse
import datetime
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CORPUS = ROOT / "src-tauri/tests/fixtures/feed_corpus"
REGISTRY = CORPUS / "approvals.json"
DECIDERS = ("lead", "maintainer")


def load_registry() -> dict:
    return json.loads(REGISTRY.read_text(encoding="utf-8"))


def rules() -> dict:
    return load_registry()["rules"]


def dump(doc: dict, ascii_only: bool) -> str:
    return json.dumps(doc, indent=2, ensure_ascii=ascii_only) + "\n"


def read_case(path: Path) -> tuple[dict, bool]:
    """A case.json and whether its layout escapes non-ASCII text."""
    text = path.read_text(encoding="utf-8")
    doc = json.loads(text)
    return doc, dump(doc, True) == text


def cases(broker: str | None, case: str | None):
    for path in sorted(CORPUS.glob("*/*/case.json")):
        b, c = path.parent.parent.name, path.parent.name
        if (broker is None or b == broker) and (case is None or c == case):
            yield b, c, path


def matching(devs: list, args) -> list:
    out = []
    for d in devs:
        if args.key is not None and d.get("key") != args.key:
            continue
        if args.kind is not None and d.get("kind") != args.kind:
            continue
        if getattr(args, "reason_has", None) and args.reason_has not in d.get("reason", ""):
            continue
        out.append(d)
    return out


def cmd_rules(_args) -> int:
    for rid, r in sorted(rules().items()):
        print(f"{rid} ({r['by']}, {r['date']}, {r['source']}): {r['rule']}")
    return 0


def cmd_list(args) -> int:
    known = rules()
    for b, c, path in cases(args.broker, args.case):
        doc, _ = read_case(path)
        for d in matching(doc.get("deviations", []), args):
            approval = d.get("approval")
            state = approval if approval in known else ("UNKNOWN " + str(approval) if approval else "pending")
            if args.pending and approval in known:
                continue
            print(f"{b}/{c}  {d.get('key')}  [{d.get('kind')}]  {state}  {d.get('reason', '')}")
    return 0


def cmd_set(args) -> int:
    if args.rule not in rules():
        print(f"no rule {args.rule!r} in {REGISTRY.relative_to(ROOT)}: add it first (add-rule)")
        return 1
    changed = 0
    for b, c, path in cases(args.broker, args.case):
        doc, ascii_only = read_case(path)
        hits = matching(doc.get("deviations", []), args)
        touched = False
        for d in hits:
            if d.get("kind") == "open":
                print(f"{b}/{c} {d.get('key')}: an open gap is never approved; fix it instead")
                return 1
            if d.get("approval") == args.rule and "approved" not in d:
                continue
            d["approval"] = args.rule
            d.pop("approved", None)
            touched = True
            changed += 1
            print(f"{b}/{c} {d.get('key')} [{d.get('kind')}] -> {args.rule}")
        if touched and not args.dry_run:
            path.write_text(dump(doc, ascii_only), encoding="utf-8")
    print(f"{changed} deviations {'would change' if args.dry_run else 'changed'}")
    return 0


def cmd_add_rule(args) -> int:
    doc = load_registry()
    if args.id in doc["rules"]:
        print(f"rule {args.id} exists; a rule's quote is never edited")
        return 1
    if args.by not in DECIDERS:
        print(f"--by is one of {', '.join(DECIDERS)}")
        return 1
    if not args.quote.strip() or not args.source.strip():
        print("a rule quotes its decision (--quote) and says where it is written (--source)")
        return 1
    doc["rules"][args.id] = {
        "by": args.by,
        "date": args.date or datetime.date.today().isoformat(),
        "source": args.source,
        "rule": args.quote,
    }
    REGISTRY.write_text(json.dumps(doc, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"added rule {args.id}")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("rules", help="list the rules")
    for name in ("list", "set"):
        p = sub.add_parser(name)
        if name == "set":
            p.add_argument("rule")
            p.add_argument("--broker", required=True)
            p.add_argument("--reason-has", dest="reason_has")
            p.add_argument("--dry-run", action="store_true")
        else:
            p.add_argument("--broker")
            p.add_argument("--pending", action="store_true", help="only deviations with no known rule")
        p.add_argument("--case")
        p.add_argument("--key")
        p.add_argument("--kind")
    p = sub.add_parser("add-rule")
    p.add_argument("id")
    p.add_argument("--by", required=True)
    p.add_argument("--source", required=True)
    p.add_argument("--quote", required=True)
    p.add_argument("--date")
    args = ap.parse_args()
    return {"rules": cmd_rules, "list": cmd_list, "set": cmd_set, "add-rule": cmd_add_rule}[args.cmd](args)


if __name__ == "__main__":
    sys.exit(main())
