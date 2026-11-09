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
  updates, per `web_offers.json`, read from the web's own registries) for
  every broker of the catalog is covered by a case whose subscribe was
  accepted and which received a frame for it, and every broker of the
  catalog has its `broker.json`;
- every broker module of the harness builds its session's broker
  (`Feeds::with_broker`): the `WITHOUT_BROKER` list in
  `src-tauri/tests/it/feed_corpus/mod.rs` is empty;
- every case has the web's output (`web.json`) or says it is worked out by
  hand, and the corpus is exactly the 38 brokers of the catalog.

The steps the web replay has no behaviour for (`web.json` `uncompared`:
refused upgrades, a deliberate disconnect, a deep feed not made), and the
sends the web adapter's own method could not answer, are held to a ratchet
per kind (`uncompared_ceiling.json`): the release is held above a
ceiling, and while a ceiling sits above the count (`--update` lowers it, in
the same change). A step in a case the kind's rule names (`raised_by`, the
rule's `scope.cases`) is not counted. With `--base-ref`, a ceiling higher than
the base's needs a rule of approvals.json named under `raised_by`.

A case's own result holds when its test passes (lane-test or CI); this gate
reads the corpus's declarations and coverage, not test results. It is
separate from `parity_matrix.py --check`, which only says whether the
generated matrix is current. Standard library only.
"""

from __future__ import annotations

import argparse
import collections
import json
import re
import subprocess
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
    out = []
    for web in sorted(pm.CORPUS.glob("*/*/web.json")):
        doc = json.loads(web.read_text(encoding="utf-8"))
        name = f"{web.parent.parent.name}/{web.parent.name}"
        out += [f"{name} line {step['line']}: {step['step']}" for step in doc.get("uncompared", [])]
        out += [f"{name} line {r['line']}: no_web_reply" for r in doc.get("replies", []) if not r["from_adapter"]]
    return out


def ceiling_path() -> Path:
    return pm.CORPUS / "uncompared_ceiling.json"


def read_ceiling() -> dict:
    path = ceiling_path()
    return json.loads(path.read_text(encoding="utf-8")) if path.exists() else {"ceilings": {}, "raised_by": {}}


def base_ceilings(ref: str) -> dict | None:
    """The ceilings at `ref`; `None` where that commit has none yet."""
    path = ceiling_path().relative_to(pm.ROOT).as_posix()
    done = subprocess.run(["git", "show", f"{ref}:{path}"], capture_output=True, text=True, cwd=pm.ROOT, check=False)
    return json.loads(done.stdout)["ceilings"] if done.returncode == 0 else None


def named_cases(kind: str, doc: dict, rules: dict) -> set[str]:
    """The cases the rule under `raised_by` names for this kind."""
    rule = rules.get(doc.get("raised_by", {}).get(kind), {})
    return set(rule.get("scope", {}).get("cases", []))


def uncompared_counts(doc: dict, rules: dict) -> collections.Counter:
    """The steps the web replay does not model, per kind, without those in
    a case the kind's rule names."""
    counts: collections.Counter = collections.Counter()
    for line in uncompared():
        place, kind = line.rsplit(": ", 1)
        if place.rsplit(" line ", 1)[0] not in named_cases(kind, doc, rules):
            counts[kind] += 1
    return counts


def ceiling_problems(counts: dict, doc: dict, rules: dict, base: dict | None = None) -> list[str]:
    """The ratchet: no kind above its ceiling, no ceiling above its count,
    and no ceiling above the base's without a rule in force."""
    ceilings, raised_by = doc.get("ceilings", {}), doc.get("raised_by", {})
    out = []
    for kind in sorted(set(counts) | set(ceilings)):
        now, ceiling = counts.get(kind, 0), ceilings.get(kind, 0)
        if now > ceiling:
            out.append(f"{kind}: {now} steps, above its ceiling of {ceiling}")
        elif now < ceiling:
            out.append(f"{kind}: {now} steps, and its ceiling still says {ceiling}: run release_gate.py --update")
        if base is not None and ceiling > base.get(kind, 0):
            rule = raised_by.get(kind)
            if rule not in rules or "superseded_by" in rules[rule]:
                out.append(f"{kind}: its ceiling rose from {base.get(kind, 0)} to {ceiling} with no rule in force "
                           "named under raised_by")
    return out


def update_ceiling() -> None:
    """Lower each ceiling to its count; a ceiling is never raised here."""
    doc = read_ceiling()
    counts = uncompared_counts(doc, pm.approval_rules())
    doc["ceilings"] = {kind: min(n, counts.get(kind, 0)) for kind, n in doc["ceilings"].items()}
    ceiling_path().write_text(json.dumps(doc, indent=2) + "\n", encoding="utf-8")


def holds(base: dict | None = None) -> dict[str, list[str]]:
    """What holds the release back, by reason."""
    rules = pm.approval_rules()
    offers = pm.web_offers()
    found = {
        "pending": [],
        "old flag": [],
        "open": [],
        "uncovered": [],
        "without broker": [],
        "no web output": [],
        "catalog": catalog_holds(),
        "uncompared": ceiling_problems(uncompared_counts(read_ceiling(), rules), read_ceiling(), rules, base),
    }
    for broker in pm.brokers():
        bdir = pm.CORPUS / broker
        meta_path = bdir / "broker.json"
        if not meta_path.exists():
            found["uncovered"].append(f"{broker}: no broker.json (what the web offers is not recorded)")
            continue
        meta = json.loads(meta_path.read_text(encoding="utf-8"))
        web = pm.capabilities(broker, meta, offers)
        problem = pm.web_problem(meta)
        if problem is not None:
            found["uncovered"].append(f"{broker}: broker.json: {problem}")
        elif web is None:
            found["uncovered"].append(f"{broker}: web_offers.json does not record it (run web_expected.py)")
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
    ap.add_argument("--update", action="store_true", help="lower the uncompared ceilings to the counts, and stop")
    ap.add_argument("--base-ref", help="refuse an uncompared ceiling above this commit's without a rule")
    args = ap.parse_args()
    if args.update:
        update_ceiling()
        print(f"wrote {ceiling_path().relative_to(pm.ROOT)}")
        return 0
    found = holds(base_ceilings(args.base_ref) if args.base_ref else None)
    titles = {
        "pending": "deviations naming no rule in force (approve.py set RULE ...)",
        "old flag": "deviations still carrying \"approved\" (approve.py set drops it)",
        "open": "open gaps (fix them; an open gap is never approved)",
        "uncovered": "capabilities the web offers with no case",
        "without broker": "harness modules without their broker (Feeds::with_broker)",
        "no web output": "cases without web.json that are not worked out by hand",
        "catalog": "the corpus is not exactly the catalog's brokers",
        "uncompared": "steps the web replay does not model, against their ceilings",
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
    print(f"steps the web replay does not model: {len(steps)}")
    for step in steps if args.all else steps[:20]:
        print(f"  {step}")
    if total:
        print("1.0.7 cannot ship from this corpus yet.")
        return 1 if args.check else 0
    print("The feed corpus lets 1.0.7 ship.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
