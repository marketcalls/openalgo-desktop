"""Write .lane/105/coverage/goldens.tsv: per parity ID, the goldens that are
evidence for it (rules.md 13; it records the web's behaviour, never that a
node is built)."""

import collections
import glob
import json
import re
import sys

goldens, parity, out = sys.argv[1:4]
ITEM = re.compile(r"^- \[[ x]\] ([A-Z][A-Za-z0-9]*(?:-[A-Za-z0-9]+)*-\d+[a-z]?)\b")
ids = {m.group(1) for m in map(ITEM.match, open(parity)) if m}
used = collections.defaultdict(set)
for f in sorted(glob.glob(f"{goldens}/*/*.json")):
    g = json.load(open(f))
    rel = "tests/fixtures/web/flow/goldens/" + "/".join(f.split("/")[-2:])
    for p in g["parity"]:
        used[p].add(rel)
    de = g.get("desktop_expected")
    if de:
        used[de["parity"]].add(rel)
    if g["kind"] == "node" and f"N-{g['type']}-00" in ids:
        used[f"N-{g['type']}-00"].add(rel)
unknown = sorted(p for p in used if p not in ids)
if unknown:
    raise SystemExit(f"unknown parity IDs: {unknown}")
with open(out, "w") as fh:
    fh.write("# goldens lane: parity IDs with recorded web behaviour (evidence; does not mark a node built)\n")
    for p in sorted(used, key=lambda x: (x.split("-")[0], x)):
        fh.write(f"{p}\tdone\t{', '.join(sorted(used[p]))}\n")
print(f"{len(used)} IDs")
