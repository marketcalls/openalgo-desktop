#!/usr/bin/env python3
"""Compute the web's output for feed corpus cases (audit C09).

    python3 scripts/feed_corpus/web_expected.py --web ../openalgo [CASE ...]
    python3 scripts/feed_corpus/web_expected.py --web ../openalgo --check
    python3 scripts/feed_corpus/web_expected.py --web ../openalgo --diff CASE

For each case folder (`src-tauri/tests/fixtures/feed_corpus/<broker>/<case>`;
every case with a glue when none is named), the case's script is replayed
against the web broker adapter's own decoding and normalising methods,
lifted from the web tree offline (`webkit.py`: no app import, database,
network or broker login), and `web.json` is written beside the case: for
each push, by its script line, what the web adapter publishes (topic, mode
and the `data` the web forwards to 8765 clients) and, for order updates,
the `order_update` frame the web relays. The harness compares the case's
expected frames with it (`tests/it/feed_corpus/harness/web.rs`).

`--check` recomputes and fails when a checked-in `web.json` differs (the
web tree moved, or a glue changed); `--diff` prints, without writing, every
key on which a case's expected frames differ from the web, which is the
deviation list the case must declare.

The web tree is read only. `web.json` is checked in; CI never runs this
script (it has no web tree), so it is provenance, not a build step.

Adding a broker: `web_adapters/<broker>.py`, a `webkit.Glue` that names
the web methods (`sources`) and builds the state they read the way the
web's own tests do, and a line in `web_adapters/__init__.py`. Where a web
adapter cannot be run offline cleanly (its normaliser needs a live socket
or the database at every step), write the case `"method": "by-hand"`,
name the web file and line its values are worked from in
`provenance.notes`, and leave `web.json` out.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from web_adapters import GLUES  # noqa: E402
from webkit import read_body, read_frame, set_step  # noqa: E402

CORPUS = HERE.parent.parent / "src-tauri/tests/fixtures/feed_corpus"
MODES = {"ltp": 1, "quote": 2, "depth": 3}


def web_sha(web: Path, given: str | None) -> str:
    """The web tree's commit, read from its .git files (git is not run)."""
    if given:
        return given
    git = web / ".git"
    head = (git / "HEAD").read_text().strip()
    if not head.startswith("ref: "):
        return head
    ref = head[5:]
    loose = git / ref
    if loose.exists():
        return loose.read_text().strip()
    for line in (git / "packed-refs").read_text().splitlines():
        if line.endswith(" " + ref):
            return line.split()[0]
    raise SystemExit("cannot read the web tree's commit; pass --web-sha")


def mode_of(v) -> int:
    if isinstance(v, int) and not isinstance(v, bool):
        return v
    return MODES[str(v).strip().lower()]


def symbols(message: dict) -> list[tuple[str, str]]:
    items = message.get("symbols") or [message]
    return [(s["symbol"], s["exchange"]) for s in items]


class Replay:
    """A case's script against the web adapter, with the web server's
    reference counting: the adapter is asked to subscribe a symbol and
    mode when the first client asks, and to unsubscribe when the last
    one lets go."""

    def __init__(self, glue, case_dir: Path):
        self.glue = glue
        self.case_dir = case_dir
        self.held: dict[tuple, int] = {}
        self.by_client: dict[str, set] = {}

    def subscribe(self, client: str, message: dict) -> None:
        mode = mode_of(message.get("mode", 1))
        depth = int(message.get("depth", 5))
        for sym, exch in symbols(message):
            key = (sym, exch, mode, depth)
            if key in self.by_client.setdefault(client, set()):
                continue
            self.by_client[client].add(key)
            self.held[key] = self.held.get(key, 0) + 1
            if self.held[key] == 1:
                self.glue.subscribe(self.row(sym, exch), mode, depth)

    def release(self, client: str, key: tuple) -> None:
        self.by_client[client].discard(key)
        self.held[key] -= 1
        if self.held[key] == 0:
            del self.held[key]
            self.glue.unsubscribe(self.row(key[0], key[1]), key[2], key[3])

    def unsubscribe(self, client: str, message: dict) -> None:
        mode = mode_of(message.get("mode", 1))
        for sym, exch in symbols(message):
            for key in [k for k in self.by_client.get(client, set()) if k[:3] == (sym, exch, mode)]:
                self.release(client, key)

    def row(self, symbol: str, exchange: str) -> dict:
        row = self.glue.master.by_symbol(symbol, exchange)
        if row is None:
            raise SystemExit(f"{symbol} on {exchange} is not in the case's instruments")
        return row

    def run(self, script: str, answers: list) -> tuple[list, bytes]:
        """Replay `script` with the fake broker API's first `answers`
        (`case.json` `http`); the inputs are hashed as the harness hashes
        them: the script, each step's file, then each answer's file."""
        inputs = bytearray(script.encode("utf-8"))
        pushes = []
        first = [(a, read_body(self.case_dir, a.get("body"))) for a in answers]
        set_step(0)
        self.glue.start()
        for a, (body, _) in first:
            self.glue.serve(a.get("method"), a["path"], body)
        self.glue.published = []
        for n, line in enumerate(script.splitlines(), 1):
            if not line.strip():
                continue
            # The frozen clock reads this step's time while it runs.
            set_step(n)
            step = json.loads(line)
            kind = step["step"]
            client = step.get("client", "")
            if kind == "send":
                action = step["message"].get("action")
                if action == "subscribe":
                    self.subscribe(client, step["message"])
                elif action == "unsubscribe":
                    self.unsubscribe(client, step["message"])
                elif action == "unsubscribe_all":
                    for key in list(self.by_client.get(client, set())):
                        self.release(client, key)
                elif action == "subscribe_orders":
                    self.glue.subscribe_orders()
            elif kind == "close_client":
                for key in list(self.by_client.get(client, set())):
                    self.release(client, key)
            elif kind == "drop_upstream":
                self.glue.reconnect(step.get("socket", "main"))
            elif kind == "push":
                frame, file_bytes = read_frame(self.case_dir, step["frame"])
                if file_bytes is not None:
                    inputs += file_bytes
                self.glue.published = []
                self.glue.push(step.get("socket", "main"), frame)
                pushes.append({"line": n, "published": self.glue.published})
            elif kind == "http_serve":
                # A new answer (an order book the poller reads next) is the
                # web adapter's next poll.
                body, file_bytes = read_body(self.case_dir, step.get("body"))
                if file_bytes is not None:
                    inputs += file_bytes
                self.glue.published = []
                self.glue.serve(step.get("method"), step["path"], body)
                pushes.append({"line": n, "published": self.glue.published})
        for _, (_, file_bytes) in first:
            if file_bytes is not None:
                inputs += file_bytes
        return pushes, bytes(inputs)


def compute(case_dir: Path, web: Path, sha: str) -> dict:
    case = json.loads((case_dir / "case.json").read_text())
    glue_cls = GLUES.get(case["broker"])
    if glue_cls is None:
        raise SystemExit(f"{case_dir}: no glue for {case['broker']} in web_adapters/")
    script = (case_dir / "script.jsonl").read_text(encoding="utf-8")
    pushes, inputs = Replay(glue_cls(web, case), case_dir).run(script, case.get("http", []))
    return {
        "generated_by": "scripts/feed_corpus/web_expected.py",
        "web_sha": sha,
        "adapter": case["broker"],
        "inputs_sha256": hashlib.sha256(inputs).hexdigest(),
        "instruments": case["instruments"],
        "pushes": pushes,
    }


def mode_pair(published: list, mode: int, frame: dict):
    market = [
        p for p in published
        if p["kind"] == "market"
        and all(k not in p or p[k] == frame.get(k) for k in ("symbol", "exchange"))
    ]
    same = [p for p in market if p["mode"] == mode]
    if same:
        return same[0]
    richer = sorted((p for p in market if p["mode"] > mode), key=lambda p: p["mode"])
    return richer[0] if richer else None


def shown(obj: dict, key: str) -> str:
    """A value as JSON text, which tells 1000 from 1000.0 as the harness does."""
    return json.dumps(obj[key], sort_keys=True) if key in obj else "absent"


def shape(v):
    """A value's JSON types at every level (harness/web.rs `shape`); a
    `<random:..>` placeholder is the type it names."""
    if isinstance(v, dict):
        return {k: shape(x) for k, x in v.items()}
    if isinstance(v, list):
        return [shape(x) for x in v]
    if isinstance(v, str):
        return v[len("<random:"):-1] if v.startswith("<random:") and v.endswith(">") else "string"
    if isinstance(v, bool):
        return "bool"
    if isinstance(v, float):
        return "float"
    if isinstance(v, int):
        return "int"
    return None


def diff(case_dir: Path, out: dict) -> list[str]:
    """What the harness's web check would find (harness/web.rs)."""
    pushes = {p["line"]: p["published"] for p in out["pushes"]}
    case = json.loads((case_dir / "case.json").read_text())
    random_keys = {d["key"] for d in case.get("deviations", []) if d.get("kind") == "random-values"}
    lines = []
    last = None
    orders_seen = 0
    script = (case_dir / "script.jsonl").read_text(encoding="utf-8")
    for n, line in enumerate(script.splitlines(), 1):
        if not line.strip():
            continue
        step = json.loads(line)
        if step["step"] in ("push", "http_serve"):
            last, orders_seen = n, 0
        elif step["step"] == "silence" and "web" in step and last is not None:
            w = step["web"]
            p = mode_pair(pushes[last], w["mode"], w)
            if p is None:
                lines.append(f"(line {n}: the web publishes no such frame either)")
            else:
                lines.append(f"missing-frame: line {n}: web {json.dumps(p['data'], sort_keys=True)}, desktop nothing")
        elif step["step"] == "receive" and last is not None:
            frame = step["frame"]
            frame = json.loads((case_dir / frame).read_text()) if isinstance(frame, str) else frame
            if step.get("web") is False:
                lines.append(f"frame: line {n}: the web sends nothing")
                continue
            if frame.get("type") == "order_update":
                theirs = [p for p in pushes[last] if p["kind"] == "order"][orders_seen:orders_seen + 1]
                orders_seen += 1
                mine, theirs = frame, (theirs[0]["frame"] if theirs else None)
            else:
                p = mode_pair(pushes[last], step.get("web_mode", frame.get("mode")), frame)
                mine, theirs = frame.get("data", {}), (p["data"] if p else None)
            if theirs is None:
                lines.append(f"(line {n}: the web publishes nothing this frame could come from)")
                continue
            for k in sorted(set(mine) | set(theirs)):
                w, d = shown(theirs, k), shown(mine, k)
                if k in random_keys:
                    same = k in mine and k in theirs and shape(mine[k]) == shape(theirs[k])
                    lines.append(f"{k}{'' if same else ' (type)'}: line {n}: web {w}, desktop {d}")
                elif w != d:
                    lines.append(f"{k}: line {n}: web {w}, desktop {d}")
    return lines


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--web", required=True, type=Path, help="the web tree (read only)")
    ap.add_argument("--web-sha", help="the web commit, when its .git cannot be read")
    ap.add_argument("--check", action="store_true", help="fail when a web.json is stale")
    ap.add_argument("--diff", action="store_true", help="print the differences, write nothing")
    ap.add_argument("cases", nargs="*", type=Path)
    args = ap.parse_args()
    sha = web_sha(args.web, args.web_sha)
    cases = args.cases or sorted(
        d for d in CORPUS.glob("*/*") if d.is_dir() and d.parent.name in GLUES
    )
    stale = 0
    for case_dir in cases:
        case_dir = case_dir.resolve()
        out = compute(case_dir, args.web, sha)
        text = json.dumps(out, indent=2, allow_nan=False) + "\n"
        target = case_dir / "web.json"
        name = f"{case_dir.parent.name}/{case_dir.name}"
        if args.diff:
            found = diff(case_dir, out)
            print(f"{name}: {len(found)} differences")
            for line in found:
                print("  " + line)
        elif args.check:
            if not target.exists() or target.read_text() != text:
                print(f"{name}: web.json is stale")
                stale += 1
        else:
            target.write_text(text)
            print(f"{name}: wrote web.json ({len(out['pushes'])} pushes)")
    return 1 if stale else 0


if __name__ == "__main__":
    sys.exit(main())
