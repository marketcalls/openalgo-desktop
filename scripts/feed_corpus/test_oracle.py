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
- web_false_checked (review 2, O8): a frame marked `"web": false` while the
  web does send that instrument is named.
- fan_out_named (O9): the web's frame to a lower-mode holder, beyond what it
  holds, is named so that a case has to declare it.
- envelope_compared (O11): the mode and broker of the desktop's frame are
  compared with the web's.
- steps_not_replayed (O10): refused upgrades, a disconnect and a deep feed
  not made are listed, not taken for compared.
- random_values_scope (O7), gate_web_output (O1), gate_catalog (O12).
- reply_is_the_adapters, no_reply_is_not_a_success (O4): the web's reply to a
  send comes from the adapter's own method; without one it is recorded as none.
- web_false_beside_a_declared_frame (O8, Codex's counterexample): an expected
  LTP of 9999.0 marked `"web": false`, beside a silence naming the real 1500.0
  delivery, is one occurrence written as two.
- fold_never_reuses (O14): LTP, Quote, LTP on one of each: the third has none.
- broker_json_fails_closed (O15): a missing, null or malformed `web` holds.

- inline_body_hashed, pin_checked (O2): a case's inline answer joins its
  hash, and `--check` refuses a web tree or tool code other than the pin's.
- value_outside_what_was_seen, named_classes_exact (O3): a deviation covers
  the values it saw, not its key at any value.
- uncompared_ceiling (O10): the ratchet per kind of step not replayed.
- rule_scope (O5): a deviation outside the words of its rule is named.
- registries_read, offers_not_broker_json, a_refused_level_covers_nothing
  (O6): what the web offers comes from its registries, not `broker.json`,
  and a capability counts only with an accepted subscribe and a frame.

CI's `web-oracle` job runs this against the web tree at the pinned commit.
Standard library only.
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

import parity_matrix as pm  # noqa: E402
import predicates  # noqa: E402
import release_gate as gate  # noqa: E402
import web_expected as we  # noqa: E402
import web_offers  # noqa: E402
import shadow  # noqa: E402

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


def edit_case(case: Path, *, script_tail: list | None = None, case_json: dict | None = None) -> None:
    """The probe case's script from its first two steps on, and fields of its case.json."""
    if script_tail is not None:
        head = (case / "script.jsonl").read_text().splitlines()[:2]
        (case / "script.jsonl").write_text("".join(line + "\n" for line in head)
                                           + "".join(json.dumps(s) + "\n" for s in script_tail))
    if case_json:
        doc = json.loads((case / "case.json").read_text())
        (case / "case.json").write_text(json.dumps({**doc, **case_json}))


def expected_quote(case: Path) -> dict:
    return json.loads((case / "expected" / "quote.json").read_text())


PUSH = {"step": "push", "frame": "frames/two.hex"}


def probe_web_false(web: Path) -> list[str]:
    """The web sends a frame for this instrument, and the case says it sends
    nothing."""
    with tempfile.TemporaryDirectory() as tmp:
        case = make_case(Path(tmp), (150000,), receives=0)
        edit_case(case, script_tail=[PUSH, {"step": "receive", "client": "a", "frame": "expected/quote.json",
                                            "web": False}])
        found = we.diff(case, we.compute(case, web, "probe"))
    named = [line for line in found if line.startswith("web-not-silent:")]
    return [] if named else [f"web_false_checked: not named: {found}"]


def probe_reply_is_the_adapters(web: Path) -> list[str]:
    """Depth 30 on an XTS broker: the adapter's own method refuses it, and
    the reply the web server builds from that is what web.json records."""
    case = we.CORPUS / "compositedge" / "depth_thirty_and_fifty_refused"
    out = we.compute(case, web, "probe")
    frames = [f for r in out.get("replies", []) for f in r["frames"]]
    items = [i for f in frames for i in f.get("subscriptions", [])]
    wanted = "Invalid depth level 30. Must be 5 or 20"
    if any(i.get("status") == "error" and i.get("message") == wanted for i in items):
        return []
    return [f"reply_is_the_adapters: no reply says {wanted!r}: {frames}"]


def probe_no_reply_is_not_a_success(web: Path) -> list[str]:
    """A glue that returns nothing and an adapter method that cannot run give
    no web reply: the send is marked, and the harness does not compare it."""
    subscribe, unsubscribe = shadow.ShadowAdapter.subscribe, shadow.ShadowAdapter.unsubscribe
    shadow.ShadowAdapter.subscribe = lambda *_args: None
    shadow.ShadowAdapter.unsubscribe = lambda *_args: None
    try:
        out = we.compute(QUOTE_MODE, web, "probe")
    finally:
        shadow.ShadowAdapter.subscribe, shadow.ShadowAdapter.unsubscribe = subscribe, unsubscribe
    replies = out.get("replies", [])
    if replies and not replies[0]["from_adapter"]:
        return []
    return [f"no_reply_is_not_a_success: {replies[:1]}"]


def probe_web_false_beside_declared(web: Path) -> list[str]:
    """Codex's counterexample: the desktop's expected frame is wrong (9999.0),
    marked as a frame the web does not send, and a silence declares the web's
    real frame, so no value of it would ever be compared."""
    with tempfile.TemporaryDirectory() as tmp:
        case = make_case(Path(tmp), (150000,), receives=0)
        (case / "expected" / "wrong.json").write_text(json.dumps(
            {**expected_quote(case), "data": {**expected_quote(case)["data"], "ltp": 9999.0}}
        ))
        edit_case(case, script_tail=[
            PUSH,
            {"step": "receive", "client": "a", "frame": "expected/wrong.json", "web": False},
            {"step": "silence", "client": "a", "web": {"symbol": "INFY", "exchange": "NSE", "mode": 2}},
        ])
        found = we.diff(case, we.compute(case, web, "probe"))
    named = [line for line in found if line.startswith("web-not-silent:")]
    return [] if named else [f"web_false_beside_a_declared_frame: not named: {found}"]


def probe_fold_never_reuses(web: Path) -> list[str]:
    """One LTP and one Quote publication answer LTP and Quote once each."""
    sub = {"action": "subscribe", "symbols": [{"symbol": "INFY", "exchange": "NSE"}]}
    with tempfile.TemporaryDirectory() as tmp:
        case = make_case(Path(tmp), (150000,), receives=0)
        edit_case(case, script_tail=[{"step": "send", "client": "a", "message": dict(sub, mode=1)}, PUSH])
        out = we.compute(case, web, "probe")
    point = next(p for p in out["pushes"] if p["deliveries"])
    consumer = we.Deliveries(point)
    answers = [consumer.market("a", "INFY", "NSE", m) for m in (1, 2, 1)]
    modes = [a and a["mode"] for a in answers]
    return [] if modes == [1, 2, None] else [f"fold_never_reuses: requests [1, 2, 1] got {modes}"]


def probe_fan_out(web: Path) -> list[str]:
    """A client holding LTP also gets the web's Quote frame for the
    instrument, which answers nothing it holds."""
    sub = {"action": "subscribe", "symbols": [{"symbol": "INFY", "exchange": "NSE"}]}
    with tempfile.TemporaryDirectory() as tmp:
        case = make_case(Path(tmp), (150000,), receives=0)
        expected = expected_quote(case)
        ltp = dict(expected, mode=1)
        (case / "expected" / "ltp.json").write_text(json.dumps(ltp))
        edit_case(case, script_tail=[
            {"step": "send", "client": "b", "message": dict(sub, mode=1)},
            PUSH,
            {"step": "receive", "client": "a", "frame": "expected/quote.json"},
            {"step": "receive", "client": "b", "frame": "expected/ltp.json"},
        ])
        found = we.diff(case, we.compute(case, web, "probe"))
    named = [line for line in found if line.startswith("fan-out:") and "client b" in line]
    return [] if named else [f"fan_out_named: not named: {found}"]


def probe_envelope(web: Path) -> list[str]:
    """A frame naming another broker, and one tagged with another mode than
    the web's."""
    with tempfile.TemporaryDirectory() as tmp:
        case = make_case(Path(tmp), (150000,), receives=0)
        expected = expected_quote(case)
        (case / "expected" / "other.json").write_text(json.dumps(dict(expected, broker="fyers", mode=1)))
        edit_case(case, script_tail=[PUSH, {"step": "receive", "client": "a", "frame": "expected/other.json",
                                            "web_mode": 2}])
        found = we.diff(case, we.compute(case, web, "probe"))
    missing = [key for key in ("envelope.broker", "envelope.mode")
               if not any(line.startswith(key + ":") for line in found)]
    return [f"envelope_compared: {key} not compared: {found}" for key in missing]


def probe_steps_not_replayed(web: Path) -> list[str]:
    with tempfile.TemporaryDirectory() as tmp:
        case = make_case(Path(tmp), (150000,), receives=0)
        edit_case(case, script_tail=[{"step": "refuse_upgrades", "status": 429},
                                     {"step": "disconnect", "socket": "main"}],
                  case_json={"deep_feed_not_made": True})
        out = we.compute(case, web, "probe")
    want = [{"line": 0, "step": "deep_feed_not_made"}, {"line": 3, "step": "refuse_upgrades"},
            {"line": 4, "step": "disconnect"}]
    got = out.get("uncompared")
    return [] if got == want else [f"steps_not_replayed: {got}, not {want}"]


def probe_random_values_scope() -> list[str]:
    deviation = {"key": "ltp", "kind": "random-values", "reason": "drawn at random"}
    kinds = ["random-values"]
    for broker, wanted in (("zerodha", True), ("dhan_sandbox", False)):
        problems = pm.deviation_problems(broker, {"broker": broker, "deviations": [deviation]}, kinds, {})
        if any("synthetic" in p for p in problems) != wanted:
            return [f"random_values_scope: {broker}: {problems}"]
    return []


def temporary_corpus(tmp: Path, broker: str, *, method: str | None) -> None:
    """One broker folder with one case and no web.json."""
    case = tmp / broker / "probe"
    case.mkdir(parents=True)
    (tmp / broker / "broker.json").write_text("{}")
    (case / "script.jsonl").write_text("")
    (case / "case.json").write_text(json.dumps({"provenance": {"method": method} if method else {}}))


def with_corpus(tmp: Path, catalog: list[str]):
    """`gate.holds()` over the temporary corpus and this catalog."""
    corpus, brokers = pm.CORPUS, pm.brokers
    pm.CORPUS, pm.brokers = tmp, lambda: catalog
    try:
        return gate.holds()
    finally:
        pm.CORPUS, pm.brokers = corpus, brokers


def probe_broker_json_fails_closed() -> list[str]:
    """Missing, null or malformed `web` is a hold, not an empty obligation."""
    good = {"ltp": True, "quote": True, "depth": [5], "orders": False}
    offers = {"brokers": {"dhan": {"depth": [5], "orders": False, "source": "registry"}}}
    problems = []
    for name, meta, held in (
        ("missing", {}, True),
        ("null", {"web": None}, True),
        ("not an object", {"web": "yes"}, True),
        ("no flags", {"web": {"depth": [5], "orders": False}}, True),
        ("bad depth", {"web": {**good, "depth": "five"}}, True),
        ("no orders", {"web": {"ltp": True, "quote": True, "depth": [5]}}, True),
        ("complete", {"web": good}, False),
    ):
        with tempfile.TemporaryDirectory() as tmp:
            temporary_corpus(Path(tmp), "dhan", method="by-hand")
            (Path(tmp) / "dhan" / "broker.json").write_text(json.dumps(meta))
            (Path(tmp) / "web_offers.json").write_text(json.dumps(offers))
            items = with_corpus(Path(tmp), ["dhan"]).get("uncovered", [])
        if any("broker.json" in i for i in items) != held:
            problems.append(f"broker_json_fails_closed: {name}: {items}")
    return problems


def probe_gate_web_output() -> list[str]:
    with tempfile.TemporaryDirectory() as tmp:
        temporary_corpus(Path(tmp), "zerodha", method="web-script")
        temporary_corpus(Path(tmp), "fyers", method="by-hand")
        held = with_corpus(Path(tmp), ["zerodha", "fyers"]).get("no web output", [])
    return [] if held == ["zerodha/probe"] else [f"gate_web_output: {held}"]


def probe_gate_catalog() -> list[str]:
    problems = []
    with tempfile.TemporaryDirectory() as tmp:
        temporary_corpus(Path(tmp), "zerodha", method="by-hand")
        short = with_corpus(Path(tmp), ["zerodha"]).get("catalog", [])
        if not any("not 38" in item for item in short):
            problems.append(f"gate_catalog: a catalog of one broker passes: {short}")
        beside = with_corpus(Path(tmp), [f"b{i}" for i in range(38)]).get("catalog", [])
        if not any(item.startswith("zerodha:") for item in beside):
            problems.append(f"gate_catalog: a folder of no catalog broker passes: {beside}")
    return problems


def probe_registries_read(web: Path) -> list[str]:
    """The web's own registries, not what broker.json says."""
    problems = []
    offers = web_offers.compute(web)["brokers"]
    for broker, depth, orders, source in (
        ("compositedge", [5, 20], False, "registry"),
        ("dhan", [5, 20], True, "registry"),
        ("samco", [5], True, "registry"),
    ):
        want = {"depth": depth, "orders": orders, "source": source}
        if offers.get(broker) != want:
            problems.append(f"registries_read: {broker}: {offers.get(broker)}, not {want}")
    return problems


def probe_offers_not_broker_json() -> list[str]:
    """broker.json saying less than the registries hides nothing."""
    with tempfile.TemporaryDirectory() as tmp:
        temporary_corpus(Path(tmp), "dhan", method="by-hand")
        (Path(tmp) / "dhan" / "broker.json").write_text(json.dumps({"web": {"ltp": True, "quote": True, "depth": [5], "orders": False}}))
        (Path(tmp) / "web_offers.json").write_text(json.dumps({
            "brokers": {"dhan": {"depth": [5, 20], "orders": False, "source": "registry"}}
        }))
        held = with_corpus(Path(tmp), ["dhan"]).get("uncovered", [])
    return [] if any("Depth 20" in h for h in held) else [f"offers_not_broker_json: {held}"]


def probe_refused_level_covers_nothing() -> list[str]:
    """A refused subscribe, or an accepted one nothing came of, covers no
    capability; a frame received for an accepted one does."""
    message = {"action": "subscribe", "symbols": [{"symbol": "INFY", "exchange": "NSE"}], "mode": 3, "depth": 20}
    refused = {"status": "partial", "subscriptions": [{"symbol": "INFY", "exchange": "NSE", "status": "error"}]}
    frame = {"type": "market_data", "symbol": "INFY", "exchange": "NSE", "mode": 3}
    scripts = (
        ([{"step": "send", "client": "a", "message": message, "reply": refused},
          {"step": "receive", "client": "a", "frame": frame}], set()),
        ([{"step": "send", "client": "a", "message": message}], set()),
        ([{"step": "send", "client": "a", "message": message},
          {"step": "receive", "client": "a", "frame": frame}], {20}),
    )
    problems = []
    for steps, want in scripts:
        with tempfile.TemporaryDirectory() as tmp:
            (Path(tmp) / "script.jsonl").write_text("".join(json.dumps(s) + "\n" for s in steps))
            got = pm.covers(Path(tmp))
        if got != want:
            problems.append(f"a_refused_level_covers_nothing: {got}, not {want}: {steps}")
    return problems


def probe_value_outside_seen() -> list[str]:
    """The web's trade time moves by nine seconds: the deviation on `ltt` no
    longer covers it, and what it did allow is shown by no frame."""
    with tempfile.TemporaryDirectory() as tmp:
        case = Path(tmp) / "hdfcsky" / "no_trade_yet"
        shutil.copytree(we.CORPUS / "hdfcsky" / "no_trade_yet", case)
        before = predicates.violations(case)
        text = (case / "web.json").read_text()
        (case / "web.json").write_text(text.replace('"ltt": 1700000000', '"ltt": 1700000009'))
        after = predicates.violations(case)
    problems = [f"value_outside_what_was_seen: not clean before the change: {before}"] if before else []
    for wanted in ("is outside what it allows", "which no frame shows"):
        if not any(wanted in line for line in after):
            problems.append(f"value_outside_what_was_seen: {wanted!r} not found: {after}")
    return problems


def probe_named_classes_exact() -> list[str]:
    """A class leaves no freedom: 1 is no zero, a real change is not one from an invented zero."""
    absent = predicates.ABSENT
    frame = {"ltp": 0, "close": 1495.0}
    real = {"ltp": 1500.0, "close": 1495.0}
    cases = (
        ("zero-left-out", "oi", 0, absent, None, True),
        ("zero-left-out", "oi", 1, absent, None, False),
        ("zero-left-out", "oi", 0, 5, None, False),
        ("null-left-out", "ltt", None, absent, None, True),
        ("null-left-out", "ltt", "", absent, None, False),
        ("change-from-zero-ltp", "change", -1495.0, absent, frame, True),
        ("change-from-zero-ltp", "change", -1495.0, absent, real, False),
        ("change-from-zero-ltp", "change", -1495.0, -1495.0, frame, False),
        ("change-from-zero-ltp", "change_percent", -100.0, absent, frame, True),
        ("change-from-zero-ltp", "change_percent", -1495.0, absent, frame, False),
    )
    problems = []
    for item, key, web, desktop, data, want in cases:
        if predicates.matches(item, key, web, desktop, data) != want:
            problems.append(f"named_classes_exact: {item} {key} web {web} desktop {desktop} data {data}: not {want}")
    if predicates.classify("oi", 0, absent, None) != "zero-left-out":
        problems.append("named_classes_exact: a zero left out is not classified")
    if predicates.classify("oi", 7, absent, None) != [7, absent]:
        problems.append("named_classes_exact: any other value must be its exact pair")
    return problems


def probe_uncompared_ceiling() -> list[str]:
    """Above a ceiling, a ceiling left high, and a rise without a rule each
    fail; a rise naming a rule in force does not."""
    rules = {"a-rule": {"rule": "r"}, "old": {"rule": "r", "superseded_by": "a-rule"}}
    doc = {"ceilings": {"disconnect": 5}, "raised_by": {}}
    cases = (
        ("at the ceiling", {"disconnect": 5}, doc, None, 0),
        ("above", {"disconnect": 6}, doc, None, 1),
        ("a kind with no ceiling", {"disconnect": 5, "refuse_upgrades": 1}, doc, None, 1),
        ("left high", {"disconnect": 4}, doc, None, 1),
        ("a rise with no rule", {"disconnect": 5}, doc, {"disconnect": 4}, 1),
        ("a rise on a withdrawn rule", {"disconnect": 5}, {**doc, "raised_by": {"disconnect": "old"}},
         {"disconnect": 4}, 1),
        ("a rise with a rule", {"disconnect": 5}, {**doc, "raised_by": {"disconnect": "a-rule"}}, {"disconnect": 4}, 0),
    )
    problems = []
    for name, counts, ceiling, base, want in cases:
        got = gate.ceiling_problems(counts, ceiling, rules, base)
        if len(got) != want:
            problems.append(f"uncompared_ceiling: {name}: {got}")
    return problems


def probe_rule_scope() -> list[str]:
    """A rule's scope comes from its words: an index-only rule on an equity case is named (review 2, O5)."""
    equity = {"broker": "hdfcsky", "instruments": [{"symbol": "RELIANCE", "exchange": "NSE"}]}
    index = {"broker": "hdfcsky", "instruments": [{"symbol": "NIFTY", "exchange": "NSE_INDEX"}]}
    deviation = {"key": "ltt", "kind": "web-defect", "reason": "r", "approval": "no-packet-time-as-trade-time"}
    kinds = ["web-defect"]
    problems = []
    for case, covered in ((equity, False), (index, True)):
        found = pm.deviation_problems("probe", {**case, "deviations": [deviation]}, kinds)
        if (not any("does not cover" in p for p in found)) != covered:
            problems.append(f"rule_scope: {case['instruments']}: {found}")
    return problems


def probe_inline_body_hashed(web: Path) -> list[str]:
    """Two different inline answers cannot share a digest."""
    digests = []
    for body in ({"json": {"status": "ok"}}, {"json": {"status": "changed"}}):
        with tempfile.TemporaryDirectory() as tmp:
            case = make_case(Path(tmp), (150000,), receives=0)
            edit_case(case, case_json={"http": [{"method": "GET", "path": "/probe", "body": body}]})
            digests.append(we.compute(case, web, "probe")["inputs_sha256"])
    return [] if digests[0] != digests[1] else ["inline_body_hashed: an inline answer does not join the digest"]


def probe_pin_checked() -> list[str]:
    pin = we.read_pin
    we.read_pin = lambda: {"web_sha": "b" * 40, "tool_sha256": "c" * 64}
    try:
        found = we.pin_problems("a" * 40)
    finally:
        we.read_pin = pin
    return [] if len(found) == 2 else [f"pin_checked: {found}"]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--web", required=True, type=Path, help="the web tree (read only)")
    args = ap.parse_args()
    problems = (
        probe_two_packets(args.web)
        + probe_unread(args.web)
        + probe_repeated_subscribe(args.web)
        + probe_web_false(args.web)
        + probe_fan_out(args.web)
        + probe_envelope(args.web)
        + probe_steps_not_replayed(args.web)
        + probe_random_values_scope()
        + probe_gate_web_output()
        + probe_gate_catalog()
        + probe_value_outside_seen()
        + probe_named_classes_exact()
        + probe_reply_is_the_adapters(args.web)
        + probe_no_reply_is_not_a_success(args.web)
        + probe_web_false_beside_declared(args.web)
        + probe_fold_never_reuses(args.web)
        + probe_broker_json_fails_closed()
        + probe_uncompared_ceiling()
        + probe_rule_scope()
        + probe_registries_read(args.web)
        + probe_offers_not_broker_json()
        + probe_refused_level_covers_nothing()
        + probe_inline_body_hashed(args.web)
        + probe_pin_checked()
    )
    for p in problems:
        print(p)
    print("oracle probes: " + ("FAILED" if problems else "passed"))
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
