"""Record the web agent's stream frames as the desktop's contract fixture.

Reads the web's `services/agent/frames.py` (no agno import, no I/O, so plain
Python loads it) and writes `src-tauri/tests/fixtures/agent/frames.json`: the
frame vocabulary in wire order, each frame's fields in declaration order with
their defaults, the response headers, the heartbeat comment, and sample frames
with the exact `data:` text the web writes for them. The desktop's contract
test (`agent::contract::frames`) fails on any drift.

Usage, from the desktop repository root:

    python3 -I scripts/agent/capture_web_frames.py /path/to/openalgo

Regenerate only from the web; never edit the fixture by hand.
"""

from __future__ import annotations

import dataclasses
import json
import sys
from enum import Enum
from pathlib import Path


def _default(field: dataclasses.Field) -> object:
    if field.default is not dataclasses.MISSING:
        value = field.default
        return value.value if isinstance(value, Enum) else value
    if field.default_factory is not dataclasses.MISSING:
        return field.default_factory()
    return {"required": True}


def _samples(frames) -> list[dict]:
    """Frames that exercise every field, unicode, nulls and nested key order."""
    built = [
        frames.Start(run_id="r-1", session_id="s-1", conversation_id=7),
        frames.Start(run_id="r-2", session_id="s-2", conversation_id=7, user_message_id=41),
        frames.Token(delta="NIFTY ₹ निफ्टी \"q\" \\ \n\t\u0001"),
        frames.ToolStart(id="call_1", name="get_quote", args={"symbol": "INFY", "exchange": "NSE"}),
        frames.ToolStart(id="call_2", name="get_funds"),
        frames.ToolEnd(
            id="call_1",
            name="get_quote",
            ok=True,
            result='{"ltp":1520.5,"symbol":"INFY"}',
            duration=0.25,
        ),
        frames.ToolEnd(id="call_3", name="get_history", ok=False, result="refused"),
        frames.ToolEnd(id="call_4", name="x", ok=True, result={"z": 1, "a": [1, 2.5, None]}),
        frames.Reasoning(delta="thinking"),
        frames.Ui(delta="root = Card()"),
        frames.Viz(),
        frames.Viz(
            kind="candles",
            spec={"symbol": "INFY", "candles": [[1, 2.0, 3, 1.5, 2.5, 100]], "a": True},
            title="INFY daily",
            source="history_service",
        ),
        frames.ChartCommand(),
        frames.ChartCommand(commands=[{"op": "clear"}, {"op": "line", "price": 101.25}]),
        frames.Confirm(
            run_id="r-1",
            session_id="s-1",
            requirements=[
                {
                    "id": "q-1",
                    "tool_call_id": "call_9",
                    "tool_name": "place_order",
                    "args": {"symbol": "SBIN", "quantity": 1},
                    "missing": [],
                    "kind": "confirmation",
                }
            ],
        ),
        frames.Notice(level=frames.NoticeLevel.WARNING, message="Sandbox mode is on."),
        frames.Usage(),
        frames.Usage(
            input_tokens=1200,
            output_tokens=80,
            total_tokens=1280,
            cached_tokens=1000,
            reasoning_tokens=12,
            cost_usd=0.0042,
            billing="metered",
            model="openai/gpt-4o-mini",
            ttft_ms=812.5,
        ),
        frames.Usage(input_tokens=5, billing="subscription", model="chatgpt/gpt-5.4"),
        frames.Error(message="Your OpenAI account has no credits left."),
        frames.Error(message="bad", kind=frames.ErrorKind.PROVIDER),
        frames.Done(),
        frames.Done(reason=frames.DoneReason.CANCELLED),
        frames.Done(reason=frames.DoneReason.INCOMPLETE),
    ]
    return [{"dict": json.loads(json.dumps(f.to_dict(), default=str)), "sse": frames.sse(f)} for f in built]


def main(web_root: str) -> None:
    sys.path.insert(0, web_root)
    import services.agent.frames as frames  # noqa: PLC0415 - the web is on the path only now

    fixture = {
        "_comment": (
            "Captured from the web's services/agent/frames.py by "
            "scripts/agent/capture_web_frames.py. Regenerate only from the web; the "
            "desktop's frames must match this file."
        ),
        "frame_types": list(frames.FRAME_TYPES),
        "fields": {
            name: [
                {"name": f.name, "default": _default(f)} for f in dataclasses.fields(cls)
            ]
            for name, cls in frames.FRAME_CLASSES.items()
        },
        "done_reasons": [r.value for r in frames.DoneReason],
        "notice_levels": [r.value for r in frames.NoticeLevel],
        "error_kinds": [r.value for r in frames.ErrorKind],
        "sse_headers": dict(frames.SSE_HEADERS),
        "heartbeat": frames.heartbeat(),
        "samples": _samples(frames),
    }
    out = Path(__file__).resolve().parents[2] / "src-tauri/tests/fixtures/agent/frames.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(fixture, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"wrote {out}")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: capture_web_frames.py /path/to/openalgo")
    main(sys.argv[1])
