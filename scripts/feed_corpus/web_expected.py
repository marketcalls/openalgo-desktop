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
the `order_update` frame the web relays, and for each `send` the reply the
web server gives (`shadow.py`). The harness compares the case's expected
frames and replies with it (`tests/it/feed_corpus/harness/web.rs`).

`--check` recomputes and fails when a checked-in `web.json` differs (the
web tree moved, or a glue changed); `--diff` prints, without writing, every
key on which a case's expected frames differ from the web, which is the
deviation list the case must declare.

The web tree is read only. `web.json` is checked in, computed at one web
commit, the pin in `web_oracle.json` beside the corpus, which also records a
digest of this tool's own code (`tool_sha256`). CI's `web-oracle` job (a
requirement of ci-ok) takes the web's main branch, refuses a pin that main
has not reached, checks out that commit and runs `--check` and
`test_oracle.py`. Moving to a newer web commit is deliberate: `--repin` writes
the new commit into the pin, which must be one pushed to the web's main.

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

import web_offers  # noqa: E402
from web_adapters import GLUES  # noqa: E402
from webkit import read_body, read_frame, set_step  # noqa: E402
from webserver import WebServer  # noqa: E402

CORPUS = HERE.parent.parent / "src-tauri/tests/fixtures/feed_corpus"
PIN = CORPUS / "web_oracle.json"
MODES = {"ltp": 1, "quote": 2, "depth": 3}
# The tool's own code a web.json depends on, beside web_adapters/*.py.
TOOL_FILES = ("web_expected.py", "webkit.py", "webserver.py", "protolite.py", "shadow.py")
# Refused upgrades and a deliberate disconnect have no adapter counterpart
# in the replay (harness/web.rs `UNMODELLED_STEPS`).
UNMODELLED_STEPS = ("refuse_upgrades", "disconnect")


def tool_sha256() -> str:
    """A digest of the code that writes web.json (harness/inventory.rs reads
    the same files): a changed glue with the old web.json fails a test."""
    names = sorted([*TOOL_FILES, *(f"web_adapters/{p.name}" for p in (HERE / "web_adapters").glob("*.py"))])
    digest = hashlib.sha256()
    for name in names:
        digest.update(name.encode() + b"\0")
        digest.update((HERE / name).read_bytes().replace(b"\r\n", b"\n") + b"\0")
    return digest.hexdigest()


def read_pin() -> dict:
    return json.loads(PIN.read_text(encoding="utf-8"))


def write_pin(sha: str) -> None:
    PIN.write_text(json.dumps({"web_sha": sha, "tool_sha256": tool_sha256()}, indent=2) + "\n", encoding="utf-8")


def pin_problems(sha: str) -> list[str]:
    """Where the web tree in hand, or this tool's code, is not what the pin says."""
    pin = read_pin()
    out = []
    if pin.get("web_sha") != sha:
        out.append(f"the web tree is at {sha}; the corpus is pinned to {pin.get('web_sha')}")
    if pin.get("tool_sha256") != tool_sha256():
        out.append("web_oracle.json: tool_sha256 is not this tool's code: run web_expected.py")
    return out


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


def adapter_socket(step: dict) -> str:
    """The glue socket a step's frame goes to: a further pooled market
    connection (`main#2`, ...) is the market feed's (the replay runs one
    web adapter; the web's pool gives each connection its own)."""
    socket = step.get("socket", "main")
    return "main" if socket.startswith("main#") else socket


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
        # The script's steps the replay has no web behaviour for: named in
        # web.json and counted by the release gate, so none is taken for
        # compared.
        self.uncompared: list[dict] = []
        # What the web answers each send (`WebServer.send`).
        self.replies: list[dict] = []

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
                self.replies.append({"line": n, **self.server.send(client, step["message"])})
                if step["message"].get("action") == "subscribe_orders":
                    self.glue.subscribe_orders()
            elif kind == "close_client":
                self.server.close(client)
            elif kind == "drop_upstream":
                self.glue.reconnect(adapter_socket(step))
            elif kind == "push":
                frame, file_bytes = read_frame(self.case_dir, step["frame"])
                if file_bytes is not None:
                    inputs += file_bytes
                self.glue.push(adapter_socket(step), frame)
            elif kind == "http_serve":
                # A new answer (an order book the poller reads next) is the
                # web adapter's next poll.
                body, file_bytes = read_body(self.case_dir, step.get("body"))
                if file_bytes is not None:
                    inputs += file_bytes
                self.glue.serve(step.get("method"), step["path"], body)
            elif kind in UNMODELLED_STEPS:
                self.uncompared.append({"line": n, "step": kind})
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
    replay = Replay(glue_cls(web, case), case_dir)
    pushes, inputs = replay.run(script, case.get("http", []))
    not_made = [{"line": 0, "step": "deep_feed_not_made"}] if case.get("deep_feed_not_made") else []
    return {
        "generated_by": "scripts/feed_corpus/web_expected.py",
        "web_sha": sha,
        "adapter": case["broker"],
        "inputs_sha256": hashlib.sha256(inputs).hexdigest(),
        "instruments": case["instruments"],
        "uncompared": not_made + replay.uncompared,
        "replies": replay.replies,
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
        # The last delivery a frame paired with (its envelope is compared).
        self.last: dict | None = None
        # Frames the case says the web sends nothing for: (line, client, frame).
        self.web_false: list = []
        # Deliveries a silence declared the desktop does not send.
        self.declared_missing: set = set()

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
                self.last = d
                self.last_index = i
                return self.pub(d)
        # A richer publication stands in only for a mode with no stream of
        # its own at this point (harness/deliveries.rs `market_delivery`).
        if any(mode in d["serves"] for _, d in ds):
            return None
        rest = [(i, d) for i, d in ds if d["topic_mode"] >= mode and (i, f"fold{mode}") not in self.used]
        rest.sort(key=lambda x: (x[1]["topic_mode"] != mode, x[1]["topic_mode"], x[0]))
        if rest:
            i, d = rest[0]
            self.used.add((i, f"fold{mode}"))
            self.last = d
            self.last_index = i
            return self.pub(d)
        return None

    def web_only(self, client: str, symbol: str, exchange: str, mode: int):
        """The web frame a silence declares the desktop does not send."""
        p = self.market(client, symbol, exchange, mode)
        if p is not None:
            self.declared_missing.add(self.last_index)
        return p

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

    def close(self) -> list[str]:
        """What the end of a delivery point finds (harness/deliveries.rs)."""
        return self.unconsumed() + self.fan_out() + self.not_silent()

    def _touched(self, i: int) -> bool:
        return any(j == i for j, _ in self.used)

    def fan_out(self) -> list[str]:
        """The web's fan-out to a lower-mode holder that answers none of
        its held modes and that no frame paired with: a case declares it
        (deviation `fan-out`)."""
        return [
            f"fan-out: line {self.point['line']}: the web also sends client {d['client']} publication "
            f"{d['publication']} (topic mode {d['topic_mode']})"
            for i, d in enumerate(self.point["deliveries"])
            if not d["serves"] and not self._touched(i)
        ]

    def fan_out_seen(self) -> list[tuple]:
        """The fan-out, as data (`diff`'s `seen`)."""
        return [
            ("fan-out", {**frame_identity(self.pub(d)), "mode": d["topic_mode"], "client": d["client"]}, ABSENT, None)
            for i, d in enumerate(self.point["deliveries"])
            if not d["serves"] and not self._touched(i)
        ]

    def not_silent(self) -> list[str]:
        """A frame marked `"web": false` while the web does send that client
        a frame for the same instrument (or order id) that nothing paired."""
        out = []
        for line, client, frame in self.web_false:
            for i, d in enumerate(self.point["deliveries"]):
                declared = i in self.declared_missing
                if d["client"] != client or (self._touched(i) and not declared):
                    continue
                pub = self.pub(d)
                same = (
                    pub["frame"].get("orderid") == frame.get("orderid") if "order" in d
                    else frame.get("type") != "order_update"
                    and pub.get("symbol") == frame.get("symbol") and pub.get("exchange") == frame.get("exchange")
                    and (not declared or frame.get("mode") == d["mode"])
                )
                if same:
                    out.append(f"web-not-silent: line {line}: marked \"web\": false, but the web sends "
                               f"client {client} publication {d['publication']} at line {self.point['line']}")
        return out

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


# A side of a difference that has no value (harness/web.rs `ABSENT`).
ABSENT = "<absent>"


def side(obj: dict, key: str):
    return obj[key] if key in obj else ABSENT


def frame_identity(item: dict) -> dict:
    """A frame-level difference's value: which instrument and mode it is."""
    return {"symbol": item.get("symbol"), "exchange": item.get("exchange"), "mode": item.get("mode")}


def reply_differences(mine: dict, theirs: dict) -> list[tuple]:
    """Where the desktop's reply to a send differs from the web's, as
    `(key, web, desktop)` (harness/web.rs `reply_differences`): each top-level
    key, and each key of the items of a list both sides have at one length."""
    out = []
    for k in sorted(set(mine) | set(theirs)):
        m, t = side(mine, k), side(theirs, k)
        if isinstance(m, list) and isinstance(t, list) and len(m) == len(t) and all(
            isinstance(x, dict) for x in m + t
        ):
            for a, b in zip(m, t):
                for inner in sorted(set(a) | set(b)):
                    if json.dumps(side(a, inner), sort_keys=True) != json.dumps(side(b, inner), sort_keys=True):
                        out.append((f"reply.{k}.{inner}", side(b, inner), side(a, inner)))
        elif json.dumps(m, sort_keys=True) != json.dumps(t, sort_keys=True):
            out.append((f"reply.{k}", t, m))
    return out


def diff(case_dir: Path, out: dict, seen: list | None = None) -> list[str]:
    """What the harness's web check would find (harness/web.rs), as lines;
    `seen`, when given, also collects each difference as data: `(key, web,
    desktop, web data)`, what a deviation's `allows` is generated from."""
    seen = [] if seen is None else seen
    points = {p["line"]: p for p in out["pushes"]}
    replies = {r["line"]: r for r in out.get("replies", [])}
    case = json.loads((case_dir / "case.json").read_text())
    random_keys = {d["key"] for d in case.get("deviations", []) if d.get("kind") == "random-values"}
    lines = []
    current = None
    script = (case_dir / "script.jsonl").read_text(encoding="utf-8")
    for n, line in enumerate(script.splitlines(), 1):
        if not line.strip():
            continue
        step = json.loads(line)
        web_reply = replies.get(n)
        if web_reply and web_reply["from_adapter"] and web_reply["frames"] and step.get("reply") is not None:
            for key, web, desktop in reply_differences(step["reply"], web_reply["frames"][0]):
                lines.append(f"{key}: line {n}: web {json.dumps(web)}, desktop {json.dumps(desktop)}")
                seen.append((key, web, desktop, None))
        if n in points:
            if current is not None:
                lines += current.close()
                seen += current.fan_out_seen()
            current = Deliveries(points[n])
            continue
        if current is None:
            continue
        if step["step"] == "silence" and "web" in step:
            w = step["web"]
            p = current.web_only(step.get("client", ""), w["symbol"], w["exchange"], w["mode"])
            if p is None:
                lines.append(f"(line {n}: the web delivers no such frame either)")
            else:
                key = w.get("deviation", "missing-frame")
                lines.append(f"{key}: line {n}: web {json.dumps(p['data'], sort_keys=True)}, desktop nothing")
                seen.append((key, frame_identity({**w, "mode": w["mode"]}), ABSENT, p.get("data")))
        elif step["step"] == "receive":
            frame = step["frame"]
            frame = json.loads((case_dir / frame).read_text()) if isinstance(frame, str) else frame
            if step.get("web") is False:
                lines.append(f"frame: line {n}: the web sends nothing")
                seen.append(("frame", ABSENT, frame_identity(frame), None))
                current.web_false.append((n, step.get("client", ""), frame))
                continue
            client = step.get("client", "")
            if frame.get("type") == "order_update":
                p = current.order(client, frame.get("orderid"))
                mine, theirs = frame, (p["frame"] if p else None)
            else:
                mode = step.get("web_mode", frame.get("mode"))
                p = current.market(client, frame.get("symbol"), frame.get("exchange"), mode)
                mine, theirs = frame.get("data", {}), (p["data"] if p else None)
                if p is not None:
                    tag = current.last
                    if frame.get("mode") != tag["mode"]:
                        lines.append(f"envelope.mode: line {n}: web {tag['mode']}, desktop {frame.get('mode')}")
                        seen.append(("envelope.mode", tag["mode"], frame.get("mode"), None))
                    if frame.get("broker") != tag["broker"]:
                        lines.append(f"envelope.broker: line {n}: web {tag['broker']}, desktop {frame.get('broker')}")
                        seen.append(("envelope.broker", tag["broker"], frame.get("broker"), None))
            if theirs is None:
                lines.append(f"(line {n}: the web delivers nothing this frame could come from)")
                continue
            for k in sorted(set(mine) | set(theirs)):
                w, d = shown(theirs, k), shown(mine, k)
                if k in random_keys:
                    same = k in mine and k in theirs and shape(mine[k]) == shape(theirs[k])
                    lines.append(f"{k}{'' if same else ' (type)'}: line {n}: web {w}, desktop {d}")
                    seen.append((k if same else f"{k} (type)", side(theirs, k), side(mine, k), theirs))
                elif w != d:
                    lines.append(f"{k}: line {n}: web {w}, desktop {d}")
                    seen.append((k, side(theirs, k), side(mine, k), theirs))
    if current is not None:
        lines += current.close()
        seen += current.fan_out_seen()
    return lines


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--web", type=Path, help="the web tree (read only)")
    ap.add_argument("--web-sha", help="the web commit, when its .git cannot be read")
    ap.add_argument("--repin", action="store_true", help="make the web tree's commit the pinned one")
    ap.add_argument("--print-pin", action="store_true", help="print the pinned web commit (for CI) and stop")
    ap.add_argument("--check", action="store_true", help="fail when a web.json is stale")
    ap.add_argument("--diff", action="store_true", help="print the differences, write nothing")
    ap.add_argument("cases", nargs="*", type=Path)
    args = ap.parse_args()
    if args.print_pin:
        print(read_pin()["web_sha"])
        return 0
    if args.web is None:
        ap.error("--web is required")
    sha = web_sha(args.web, args.web_sha)
    if not args.repin and read_pin()["web_sha"] != sha:
        raise SystemExit(f"the web tree is at {sha}, the corpus is pinned to {read_pin()['web_sha']}: "
                         "check out that commit, or pass --repin to move the corpus to this one")
    cases = args.cases or sorted(
        d for d in CORPUS.glob("*/*") if d.is_dir() and d.parent.name in GLUES
    )
    stale = 0
    if args.check:
        for problem in pin_problems(sha):
            print(problem)
            stale += 1
        if web_offers.stale(args.web):
            print("web_offers.json is stale: run web_expected.py")
            stale += 1
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
    if not args.check and not args.diff:
        write_pin(sha)
        web_offers.write(args.web)
    return 1 if stale else 0


if __name__ == "__main__":
    sys.exit(main())
