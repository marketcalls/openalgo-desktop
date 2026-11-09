#!/usr/bin/env python3
"""What each corpus deviation allows (review 2, O3), generated from what it shows.

    python3 scripts/feed_corpus/predicates.py                  write every case's `allows`
    python3 scripts/feed_corpus/predicates.py CASE_DIR ...     only these cases
    python3 scripts/feed_corpus/predicates.py --check          fail when one is not what the frames show

A deviation names a key; `allows` says which differences on that key it
covers, so a value outside what was seen fails the case
(`harness/web.rs`). Each item is either a named class, for a difference with
no freedom in it, or the exact `[web, desktop]` pair seen (a side with no value
is written `"<absent>"`):

- `zero-left-out`: the web sends 0, the desktop leaves the key out;
- `null-left-out`: the web sends null, the desktop leaves the key out;
- `change-from-zero-ltp`: the web's `change` or `change_percent` is computed from
  an `ltp` of 0 in the same frame, and the desktop leaves it out;
- `same-shape`: a `random-values` key, drawn at random on both sides.

A key with one deviation is regenerated here. A key split by cause (several
deviations with distinct `cause`, each with its own reason and approval) keeps
the `allows` it was split with; this tool checks them. Reads each case's
checked-in `web.json`, so it needs no web tree. Standard library only.
"""

from __future__ import annotations

import argparse
import collections
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import approve  # noqa: E402
import web_expected as we  # noqa: E402

ABSENT = we.ABSENT
RANDOM_VALUES = "random-values"


def is_number(v) -> bool:
    return isinstance(v, (int, float)) and not isinstance(v, bool)


def from_zero_ltp(key: str, web, data) -> bool:
    """A change the web computed from an `ltp` of 0 in the same frame."""
    if not (is_number(web) and isinstance(data, dict)):
        return False
    ltp, close = data.get("ltp"), data.get("close")
    if not (is_number(ltp) and is_number(close)) or ltp != 0:
        return False
    return (key == "change" and web == -close) or (key == "change_percent" and web == -100)


def classify(key: str, web, desktop, data):
    """The item that allows this difference: a named class, or the exact pair."""
    if desktop == ABSENT:
        if is_number(web) and web == 0:
            return "zero-left-out"
        if web is None:
            return "null-left-out"
        if from_zero_ltp(key, web, data):
            return "change-from-zero-ltp"
    return [web, desktop]


def matches(item, key: str, web, desktop, data) -> bool:
    """As `allow_matches` in harness/web.rs."""
    if isinstance(item, list):
        return len(item) == 2 and same(item[0], web) and same(item[1], desktop)
    if item == "same-shape":
        return True
    if desktop != ABSENT:
        return False
    return (
        (item == "zero-left-out" and is_number(web) and web == 0)
        or (item == "null-left-out" and web is None)
        or (item == "change-from-zero-ltp" and from_zero_ltp(key, web, data))
    )


def same(a, b) -> bool:
    """JSON equality that tells 1000 from 1000.0, as the harness does."""
    return json.dumps(a, sort_keys=True) == json.dumps(b, sort_keys=True)


def differences(case_dir: Path) -> dict:
    """What the case shows against the web, by deviation key."""
    seen: list = []
    we.diff(case_dir, json.loads((case_dir / "web.json").read_text(encoding="utf-8")), seen)
    found = collections.defaultdict(list)
    for key, web, desktop, data in seen:
        found[key].append((web, desktop, data))
    return found


def sort_key(item) -> tuple:
    return (isinstance(item, list), json.dumps(item, sort_keys=True))


def generated(key: str, kind: str, found: list) -> list:
    """A single deviation's `allows`, from every difference on its key."""
    if kind == RANDOM_VALUES:
        return ["same-shape"]
    items: dict[str, object] = {}
    for web, desktop, data in found:
        item = classify(key, web, desktop, data)
        items[json.dumps(item, sort_keys=True)] = item
    return sorted(items.values(), key=sort_key)


def violations(case_dir: Path) -> list[str]:
    """Where a case's `allows` are not what its frames show: a difference no
    deviation allows, or something allowed that no frame shows."""
    case = json.loads((case_dir / "case.json").read_text(encoding="utf-8"))
    found = differences(case_dir)
    deviations = case.get("deviations", [])
    used = {id(item): False for dv in deviations for item in dv.get("allows", [])}
    out = []
    for key, items in found.items():
        entries = [dv for dv in deviations if dv.get("key") == key]
        if not entries:
            continue
        for web, desktop, data in items:
            hit = next(
                (item for dv in entries for item in dv.get("allows", []) if matches(item, key, web, desktop, data)),
                None,
            )
            if hit is None:
                out.append(f"{key}: web {json.dumps(web)}, desktop {json.dumps(desktop)} is outside what it allows")
            else:
                used[id(hit)] = True
    for dv in deviations:
        for item in dv.get("allows", []):
            if not used[id(item)]:
                out.append(f"{dv.get('key')}: allows {json.dumps(item)[:120]}, which no frame shows")
    return out


def write_case(case_dir: Path) -> bool:
    """Regenerate a case's `allows`; whether the file changed."""
    if not (case_dir / "web.json").exists():
        return False
    path = case_dir / "case.json"
    doc, ascii_only = approve.read_case(path)
    deviations = doc.get("deviations", [])
    found = differences(case_dir)
    keys = collections.Counter(dv.get("key") for dv in deviations)
    changed = False
    for dv in deviations:
        if keys[dv.get("key")] > 1 or dv.get("key") not in found:
            continue
        allows = generated(dv["key"], dv.get("kind", ""), found[dv["key"]])
        if dv.get("allows") != allows:
            dv["allows"] = allows
            changed = True
    if changed:
        path.write_text(approve.dump(doc, ascii_only), encoding="utf-8")
    return changed


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--check", action="store_true", help="fail when an `allows` is not what the frames show")
    ap.add_argument("cases", nargs="*", type=Path)
    args = ap.parse_args()
    cases = [c.resolve() for c in args.cases] or sorted(
        d for d in we.CORPUS.glob("*/*") if d.is_dir() and (d / "web.json").exists()
    )
    if args.check:
        problems = [f"{c.parent.name}/{c.name}: {p}" for c in cases for p in violations(c)]
        for problem in problems:
            print(problem)
        print(f"{len(problems)} problems in {len(cases)} cases")
        return 1 if problems else 0
    changed = sum(write_case(c) for c in cases)
    print(f"{changed} of {len(cases)} cases rewritten")
    return 0


if __name__ == "__main__":
    sys.exit(main())
