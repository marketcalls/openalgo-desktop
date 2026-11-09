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
from webserver import WebServer  # noqa: E402

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
    """A case's script against the web: the web server's own subscription,
    release and delivery methods (`webserver.WebServer`, lifted from
    `websocket_proxy/server.py`) in front of the broker's web adapter (the
    case's glue). Every step that makes the adapter publish (a push, an
    answer a poller reads, or any other step) is a delivery point, with
    what each client gets from it."""

    def __init__(self, glue, case_dir: Path):
        self.glue = glue
        self.case_dir = case_dir
        self.server = WebServer(glue)

    def run(self, script: str, answers: list) -> tuple[list, bytes]:
        """Replay `script` with the fake broker API's first `answers`
        (`case.json` `http`); the inputs are hashed as the harness hashes
        them: the script, each step's file, then each answer's file."""
        inputs = bytearray(script.encode("utf-8"))
        points = []
        first = [(a, read_body(self.case_dir, a.get("body"))) for a in answers]
        set_step(0)
        self.glue.start()
        for a, (body, _) in first:
            self.glue.serve(a.get("method"), a["path"], body)
        for n, line in enumerate(script.splitlines(), 1):
            if not line.strip():
                continue
            # The frozen clock reads this step's time while it runs.
            set_step(n)
            step = json.loads(line)
            kind = step["step"]
            client = step.get("client", "")
            self.glue.published = []
            if kind == "send":
                self.server.send(client, step["message"])
                if step["message"].get("action") == "subscribe_orders":
                    self.glue.subscribe_orders()
            elif kind == "close_client":
                self.server.close(client)
            elif kind == "drop_upstream":
                self.glue.reconnect(step.get("socket", "main"))
            elif kind == "push":
                frame, file_bytes = read_frame(self.case_dir, step["frame"])
                if file_bytes is not None:
                    inputs += file_bytes
                self.glue.push(step.get("socket", "main"), frame)
            elif kind == "http_serve":
                # A new answer (an order book the poller reads next) is the
                # web adapter's next poll.
                body, file_bytes = read_body(self.case_dir, step.get("body"))
                if file_bytes is not None:
                    inputs += file_bytes
                self.glue.serve(step.get("method"), step["path"], body)
            published = self.glue.published
            if kind in ("push", "http_serve") or published:
                point = {"line": n, "published": published, "deliveries": self.server.deliver(published)}
                if kind not in ("push", "http_serve"):
                    point["step"] = kind
                points.append(point)
        for _, (_, file_bytes) in first:
            if file_bytes is not None:
                inputs += file_bytes
        return points, bytes(inputs)


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


class Deliveries:
    """One delivery point's web deliveries, each consumed once per held
    mode it serves (harness/web.rs `Deliveries`)."""

    def __init__(self, point: dict):
        self.point = point
        self.used: set = set()

    def pub(self, d: dict) -> dict:
        return self.point["published"][d["publication"]]

    def market(self, client: str, symbol: str, exchange: str, mode: int):
        """The publication a client's frame at `mode` answers: the next
        delivery serving that held mode, else (the web's fan-out beyond the
        modes held) the next of that mode or the lowest richer one."""
        ds = [
            (i, d) for i, d in enumerate(self.point["deliveries"])
            if d["client"] == client and "order" not in d
            and self.pub(d).get("symbol") == symbol and self.pub(d).get("exchange") == exchange
        ]
        for i, d in ds:
            if mode in d["serves"] and (i, mode) not in self.used:
                self.used.add((i, mode))
                return self.pub(d)
        rest = [(i, d) for i, d in ds if d["topic_mode"] >= mode and (i, f"fold{mode}") not in self.used]
        rest.sort(key=lambda x: (x[1]["topic_mode"] != mode, x[1]["topic_mode"], x[0]))
        if rest:
            i, d = rest[0]
            self.used.add((i, f"fold{mode}"))
            return self.pub(d)
        return None

    def order(self, client: str, orderid):
        ds = [
            (i, d) for i, d in enumerate(self.point["deliveries"])
            if d["client"] == client and "order" in d and (i, "order") not in self.used
        ]
        pick = next(((i, d) for i, d in ds if self.pub(d)["frame"].get("orderid") == orderid), None)
        pick = pick or (ds[0] if ds else None)
        if pick is None:
            return None
        self.used.add((pick[0], "order"))
        return self.pub(pick[1])

    def unconsumed(self) -> list[str]:
        """Every frame the web sends a client that answers one of its held
        modes and no step of the case accounts for (a receive, or a silence
        naming it). One web frame may answer several held modes (the
        desktop sends a frame per held mode), but it is sent once, so one
        of them accounts for it."""
        out = []
        for i, d in enumerate(self.point["deliveries"]):
            if not d["serves"] or any((i, s) in self.used for s in d["serves"]):
                continue
            pub = self.pub(d)
            what = (
                f"order update {pub['frame'].get('orderid')}" if "order" in d
                else f"{pub['exchange']} {pub['symbol']} at mode {d['mode']} (publication {d['publication']}, "
                f"topic mode {d['topic_mode']}): {json.dumps(pub['data'], sort_keys=True)}"
            )
            out.append(f"unconsumed: line {self.point['line']}: the web delivers to client {d['client']} {what}")
        return out


def diff(case_dir: Path, out: dict) -> list[str]:
    """What the harness's web check would find (harness/web.rs)."""
    points = {p["line"]: p for p in out["pushes"]}
    case = json.loads((case_dir / "case.json").read_text())
    random_keys = {d["key"] for d in case.get("deviations", []) if d.get("kind") == "random-values"}
    lines = []
    current = None
    script = (case_dir / "script.jsonl").read_text(encoding="utf-8")
    for n, line in enumerate(script.splitlines(), 1):
        if not line.strip():
            continue
        step = json.loads(line)
        if n in points:
            if current is not None:
                lines += current.unconsumed()
            current = Deliveries(points[n])
            continue
        if current is None:
            continue
        if step["step"] == "silence" and "web" in step:
            w = step["web"]
            p = current.market(step.get("client", ""), w["symbol"], w["exchange"], w["mode"])
            if p is None:
                lines.append(f"(line {n}: the web delivers no such frame either)")
            else:
                lines.append(f"missing-frame: line {n}: web {json.dumps(p['data'], sort_keys=True)}, desktop nothing")
        elif step["step"] == "receive":
            frame = step["frame"]
            frame = json.loads((case_dir / frame).read_text()) if isinstance(frame, str) else frame
            if step.get("web") is False:
                lines.append(f"frame: line {n}: the web sends nothing")
                continue
            client = step.get("client", "")
            if frame.get("type") == "order_update":
                p = current.order(client, frame.get("orderid"))
                mine, theirs = frame, (p["frame"] if p else None)
            else:
                mode = step.get("web_mode", frame.get("mode"))
                p = current.market(client, frame.get("symbol"), frame.get("exchange"), mode)
                mine, theirs = frame.get("data", {}), (p["data"] if p else None)
            if theirs is None:
                lines.append(f"(line {n}: the web delivers nothing this frame could come from)")
                continue
            for k in sorted(set(mine) | set(theirs)):
                w, d = shown(theirs, k), shown(mine, k)
                if k in random_keys:
                    same = k in mine and k in theirs and shape(mine[k]) == shape(theirs[k])
                    lines.append(f"{k}{'' if same else ' (type)'}: line {n}: web {w}, desktop {d}")
                elif w != d:
                    lines.append(f"{k}: line {n}: web {w}, desktop {d}")
    if current is not None:
        lines += current.unconsumed()
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
