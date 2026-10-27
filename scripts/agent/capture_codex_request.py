"""Record what a web plan turn sends to the ChatGPT Codex backend.

A `chatgpt/` model on the web runs through LiteLLM's `chatgpt` provider: agno
calls `litellm.completion(...)` with the run's messages and tools, LiteLLM's
Responses bridge turns that into a Responses request, and the provider's own
`ChatGPTResponsesAPIConfig` rewrites it for Codex (its default instructions
prepended, `store` false, `stream` true, `reasoning.encrypted_content`
included, every key outside its allowed list dropped) and sends it with its
own headers (`originator`, the account id, a session id). None of that is in
the web's own code, so the desktop pins it from here.

The request goes to a local fake server on an ephemeral port that records it
and answers with a minimal stream; nothing leaves the machine. The sign-in is
a fake one: an auth file with an unsigned token carrying a far expiry and an
account id, in a temporary directory. The model cost map is the wheel's own
copy (no fetch at import).

Two turns are recorded, each called exactly as agno calls LiteLLM
(`agno.models.litellm.LiteLLM.invoke_stream` with the web's
`providers.sampling_kwargs`, which drop temperature and top_p, and the run's
reasoning effort in `request_params`):

- `first`: the system prompt, the trader's message and one tool;
- `after_tool`: the same, then the model's tool call and its result.

What is written (`src-tauri/tests/fixtures/agent/codex_request.json`): per
turn the method, the path, the headers (names lower-cased; the bearer token,
the session id and the user agent's platform part reduced to placeholders,
since they vary per call and per machine) and the JSON body as sent. Beside
it, `src-tauri/src/agent/chatgpt/codex_instructions.json`: LiteLLM's default
instructions verbatim, which the desktop sends the same way.

Usage, from the desktop repository root, with the web's own interpreter
(read-only use of its virtual environment):

    /path/to/openalgo/.venv/bin/python -I scripts/agent/capture_codex_request.py

Regenerate only from the web; never edit the fixture by hand.
"""

from __future__ import annotations

import base64
import json
import os
import re
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

OUT = Path("src-tauri/tests/fixtures/agent/codex_request.json")
INSTRUCTIONS = Path("src-tauri/src/agent/chatgpt/codex_instructions.json")

ACCOUNT_ID = "acct-fixture-0001"
SYSTEM_PROMPT = "You are the OpenAlgo agent. Answer from the platform's own tools."
USER_MESSAGE = "What is the last price of INFY on NSE?"
TOOL = {
    "type": "function",
    "function": {
        "name": "get_quote",
        "description": "Get the latest quote for one instrument.",
        "parameters": {
            "type": "object",
            "properties": {
                "symbol": {"type": "string", "description": "The symbol."},
                "exchange": {"type": "string", "description": "The exchange."},
            },
            "required": ["symbol", "exchange"],
        },
    },
}
CALL_ID = "call_fixture_1"
CALL_ARGS = '{"symbol": "INFY", "exchange": "NSE"}'
TOOL_RESULT = '{"ltp": 1500.5}'

#: A minimal Responses stream, enough for LiteLLM to finish the call.
ANSWER = (
    'event: response.created\ndata: {"type":"response.created","response":{"id":"resp_1",'
    '"object":"response","created_at":1,"status":"in_progress","model":"gpt-5.4","output":[]}}\n\n'
    'event: response.output_text.delta\ndata: {"type":"response.output_text.delta",'
    '"item_id":"msg_1","output_index":0,"content_index":0,"delta":"ok"}\n\n'
    'event: response.completed\ndata: {"type":"response.completed","response":{"id":"resp_1",'
    '"object":"response","created_at":1,"status":"completed","model":"gpt-5.4","output":[{'
    '"type":"message","id":"msg_1","role":"assistant","status":"completed","content":[{'
    '"type":"output_text","text":"ok","annotations":[]}]}],"usage":{"input_tokens":10,'
    '"output_tokens":1,"total_tokens":11}}}\n\n'
)

RECORDED: list[dict] = []


class _Fake(BaseHTTPRequestHandler):
    def do_POST(self) -> None:  # noqa: N802 - http.server's name
        length = int(self.headers.get("content-length") or 0)
        body = self.rfile.read(length)
        RECORDED.append(
            {
                "method": "POST",
                "path": self.path,
                "headers": {k.lower(): v for k, v in self.headers.items()},
                "body": json.loads(body or b"null"),
            }
        )
        payload = ANSWER.encode()
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *args) -> None:
        return None


def _b64(obj: dict) -> str:
    return base64.urlsafe_b64encode(json.dumps(obj).encode()).decode().rstrip("=")


def _fake_token() -> str:
    claims = {"exp": 4102444800, "https://api.openai.com/auth": {"chatgpt_account_id": ACCOUNT_ID}}
    return f"{_b64({'alg': 'none'})}.{_b64(claims)}.sig"


def _scrub(headers: dict) -> dict:
    out = dict(headers)
    if out.get("authorization", "").startswith("Bearer "):
        out["authorization"] = "Bearer <access_token>"
    if "session_id" in out:
        out["session_id"] = "<uuid>"
    ua = out.get("user-agent", "")
    match = re.match(r"^(\S+)/(\S+) \((.*)\) (\S+)$", ua)
    if match:
        out["user-agent"] = f"{match.group(1)}/{match.group(2)} (<os> <release>; <arch>) {match.group(4)}"
    for volatile in ("host", "content-length", "x-stainless-raw-response"):
        out.pop(volatile, None)
    return dict(sorted(out.items()))


def main() -> int:
    server = ThreadingHTTPServer(("127.0.0.1", 0), _Fake)
    port = server.server_address[1]
    threading.Thread(target=server.serve_forever, daemon=True).start()

    token_dir = tempfile.mkdtemp(prefix="codex-capture-")
    Path(token_dir, "auth.json").write_text(
        json.dumps(
            {
                "access_token": _fake_token(),
                "refresh_token": "rt-fixture",
                "id_token": _fake_token(),
                "expires_at": 4102444800,
                "account_id": ACCOUNT_ID,
            }
        )
    )
    os.environ["LITELLM_LOCAL_MODEL_COST_MAP"] = "True"
    os.environ["CHATGPT_TOKEN_DIR"] = token_dir
    os.environ["CHATGPT_API_BASE"] = f"http://127.0.0.1:{port}"
    # A server process has no terminal: the user agent's last part reads
    # "unknown" there, as it does under gunicorn.
    for name in list(os.environ):
        if name.startswith(("TERM", "ITERM", "WEZTERM", "KITTY", "ALACRITTY", "KONSOLE")) or name in (
            "VTE_VERSION",
            "WT_SESSION",
            "GNOME_TERMINAL_SCREEN",
        ):
            os.environ.pop(name)

    import litellm

    def turn(messages: list) -> None:
        stream = litellm.completion(
            model="chatgpt/gpt-5.4",
            messages=messages,
            tools=[TOOL],
            tool_choice="auto",
            stream=True,
            stream_options={"include_usage": True},
            reasoning_effort="medium",
        )
        for _ in stream:
            pass

    first = [
        {"role": "system", "content": SYSTEM_PROMPT},
        {"role": "user", "content": USER_MESSAGE},
    ]
    turn(first)
    turn(
        first
        + [
            {
                "role": "assistant",
                "content": "",
                "tool_calls": [
                    {"id": CALL_ID, "type": "function", "function": {"name": "get_quote", "arguments": CALL_ARGS}}
                ],
            },
            {"role": "tool", "content": TOOL_RESULT, "tool_call_id": CALL_ID, "name": "get_quote"},
        ]
    )
    server.shutdown()

    from importlib.metadata import version

    out = {
        "_comment": "Generated by scripts/agent/capture_codex_request.py from the web's LiteLLM. Do not edit.",
        "litellm": version("litellm"),
        "inputs": {
            "system": SYSTEM_PROMPT,
            "user": USER_MESSAGE,
            "tool": TOOL,
            "call_id": CALL_ID,
            "call_arguments": CALL_ARGS,
            "tool_result": TOOL_RESULT,
            "account_id": ACCOUNT_ID,
            "reasoning_effort": "medium",
        },
        "first": {**RECORDED[0], "headers": _scrub(RECORDED[0]["headers"])},
        "after_tool": {**RECORDED[1], "headers": _scrub(RECORDED[1]["headers"])},
    }
    OUT.write_text(json.dumps(out, indent=1, ensure_ascii=False) + "\n")
    from litellm.llms.chatgpt.common_utils import CHATGPT_DEFAULT_INSTRUCTIONS

    INSTRUCTIONS.write_text(
        json.dumps(
            {
                "_comment": (
                    f"LiteLLM {version('litellm')} CHATGPT_DEFAULT_INSTRUCTIONS "
                    "(litellm/llms/chatgpt/common_utils.py), verbatim: what a web plan turn sends "
                    "before its own system prompt. Recorded by scripts/agent/capture_codex_request.py; "
                    "the codex_request.json fixture pins it."
                ),
                "litellm": version("litellm"),
                "instructions": CHATGPT_DEFAULT_INSTRUCTIONS,
            },
            indent=1,
            ensure_ascii=True,
        )
    )
    print(f"wrote {OUT}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
