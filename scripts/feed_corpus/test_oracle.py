#!/usr/bin/env python3
"""Probes of the web oracle (review R107-13, R107-14), run against the web tree:

    python3 scripts/feed_corpus/test_oracle.py --web ../openalgo

Each probe builds a case in a temporary folder from a checked-in one,
computes the web's output for it with `web_expected.compute`, and checks
what `web_expected.diff` (the harness's web check, mirrored) finds:

- two_packets_in_one_frame: a Kite frame carrying two quote packets for one
  instrument (LTP 1500.00 then 1501.00) is two web frames for the Quote
  client; desktop frames that repeat the first price must show the second
  price's difference, never pair twice with the first frame.
- unread_web_frame: the same frame with one receive leaves the second web
  frame unaccounted for, which the check names.
- repeated_subscribe: a client asking twice, and a second client asking for
  the same instrument and mode, reach the web adapter on every request, as
  the web server's `subscribe_client` does, and the first owner's release
  keeps the stream for the second.

CI has no web tree, so this runs by hand; the harness's own unit tests
(`harness/deliveries.rs`) hold the consumption rule in CI. Standard library
only.
"""

from __future__ import annotations

import argparse
import json
import shutil
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import web_expected as we  # noqa: E402

QUOTE_MODE = we.CORPUS / "zerodha" / "quote_mode"


def kite_frame(*ltps: int) -> str:
    """A Kite frame of quote packets for INFY, one per LTP in paise, as a
    `.hex` file (the quote_mode case's packet with its LTP changed)."""
    lines = (QUOTE_MODE / "frames" / "quote.hex").read_text().splitlines()
    packet = "".join(
        line.split("#", 1)[0].strip() for line in lines if line.strip() and not line.startswith("#")
    )[8:]  # past the count and length words of the one-packet frame
    out = [f"{len(ltps):04x}"]
    for ltp in ltps:
        body = packet[:8] + f"{ltp:08x}" + packet[16:]
        out.append(f"{len(body) // 2:04x}{body}")
    return "\n".join(out) + "\n"


def make_case(tmp: Path, ltps: tuple, receives: int) -> Path:
    case = tmp / "zerodha" / "probe"
    shutil.copytree(QUOTE_MODE, case)
    (case / "web.json").unlink(missing_ok=True)
    (case / "frames" / "two.hex").write_text(kite_frame(*ltps))
    steps = [json.loads(line) for line in (case / "script.jsonl").read_text().splitlines()[:2]]
    steps.append({"step": "push", "frame": "frames/two.hex"})
    steps += [{"step": "receive", "client": "a", "frame": "expected/quote.json"}] * receives
    (case / "script.jsonl").write_text("".join(json.dumps(s) + "\n" for s in steps))
    return case


def probe_two_packets(web: Path) -> list[str]:
    with tempfile.TemporaryDirectory() as tmp:
        case = make_case(Path(tmp), (150000, 150100), receives=2)
        found = we.diff(case, we.compute(case, web, "probe"))
    want = [line for line in found if line == "ltp: line 5: web 1501.0, desktop 1500.0"]
    return [] if want else [f"two_packets_in_one_frame: no ltp difference against the second web frame: {found}"]


def probe_unread(web: Path) -> list[str]:
    with tempfile.TemporaryDirectory() as tmp:
        case = make_case(Path(tmp), (150000, 150100), receives=1)
        found = we.diff(case, we.compute(case, web, "probe"))
    want = [line for line in found if line.startswith("unconsumed:") and "1501.0" in line]
    return [] if want else [f"unread_web_frame: the second web frame is not named: {found}"]


def probe_repeated_subscribe(web: Path) -> list[str]:
    """The adapter is asked on every subscribe, and owners are kept by
    (symbol, exchange, mode): a's release leaves b's stream."""
    calls = []
    with tempfile.TemporaryDirectory() as tmp:
        case = make_case(Path(tmp), (150000,), receives=0)
        glue_cls = we.GLUES["zerodha"]
        case_doc = json.loads((case / "case.json").read_text())
        glue = glue_cls(web, case_doc)
        original = glue.subscribe

        def counting(row, mode, depth):
            calls.append((row["symbol"], mode, depth))
            return original(row, mode, depth)

        glue.subscribe = counting
        sub = {"action": "subscribe", "symbols": [{"symbol": "INFY", "exchange": "NSE"}], "mode": 2}
        script = "".join(json.dumps(s) + "\n" for s in [
            {"step": "send", "client": "a", "message": sub},
            {"step": "send", "client": "a", "message": sub},
            {"step": "send", "client": "b", "message": sub},
            {"step": "send", "client": "a", "message": dict(sub, action="unsubscribe")},
            {"step": "push", "frame": "frames/two.hex"},
        ])
        points, _ = we.Replay(glue, case).run(script, [])
    problems = []
    if len(calls) != 3:
        problems.append(f"repeated_subscribe: the adapter was asked {len(calls)} times, not 3: {calls}")
    clients = sorted({d["client"] for d in points[-1]["deliveries"]})
    if clients != ["b"]:
        problems.append(f"repeated_subscribe: after a's release the frame goes to {clients}, not ['b']")
    return problems


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--web", required=True, type=Path, help="the web tree (read only)")
    args = ap.parse_args()
    problems = probe_two_packets(args.web) + probe_unread(args.web) + probe_repeated_subscribe(args.web)
    for p in problems:
        print(p)
    print("oracle probes: " + ("FAILED" if problems else "passed"))
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
