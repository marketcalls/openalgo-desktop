#!/usr/bin/env python3
"""The packaged binary's `mcp` subcommand over piped stdio.

    python3 scripts/ci/mcp_stdio_smoke.py <path to the installed binary>

AI clients start `openalgo-desktop mcp` with stdin and stdout piped. The
release Windows executable is a GUI-subsystem program (no console), so
this checks, on the packaged binary itself, what tests/it/mcp_subcommand.rs
checks on a debug build:

1. With no token it exits 2, prints the trader-facing message on stderr and
   nothing on stdout.
2. With a token it answers the MCP `initialize` handshake on stdout (the
   bridge does that itself, without the app) and exits 0 when stdin closes.

It never contacts an app: --url points at a closed port. Every run gets an
empty, temporary home and data folder, and checks the bridge wrote nothing
there. It binds no port. CI runs it (.github/workflows/install-smoke.yml).

`against_app` drives the same binary against the running packaged app
(PLATFORM-07, MCP-04); scripts/ci/launch_smoke.py --mcp-bridge calls it
once the trader has signed in, with a token it created on the app.
"""

from __future__ import annotations

import json
import os
import queue
import subprocess
import sys
import tempfile
import threading
import time

CLOSED_URL = "http://127.0.0.1:9"
MISSING_TOKEN = "no token was given"
TOKEN = "oamcp_ci_smoke_placeholder"
WAIT_S = 30


def isolated_env(home: str, token: str | None) -> dict[str, str]:
    env = {k: os.environ[k] for k in ("PATH", "SystemRoot", "windir") if k in os.environ}
    for var in ("HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "XDG_DATA_HOME", "XDG_CONFIG_HOME", "TMPDIR"):
        env[var] = home
    if token is not None:
        env["OPENALGO_MCP_TOKEN"] = token
    return env


def fail(what: str, code: int | None, out: str, err: str) -> None:
    print(f"FAILED: {what}")
    print(f"exit code: {code}")
    print(f"stdout: {out!r}")
    print(f"stderr: {err!r}")
    sys.exit(1)


def without_token(exe: str) -> None:
    home = tempfile.mkdtemp(prefix="openalgo-mcp-smoke-")
    p = subprocess.run(
        [exe, "mcp", "--url", CLOSED_URL],
        input=b"",
        capture_output=True,
        env=isolated_env(home, None),
        timeout=WAIT_S,
    )
    out, err = p.stdout.decode("utf-8", "replace"), p.stderr.decode("utf-8", "replace")
    if p.returncode != 2:
        fail("without a token the bridge exits with code 2", p.returncode, out, err)
    if out:
        fail("without a token nothing is written to stdout", p.returncode, out, err)
    if MISSING_TOKEN not in err:
        fail("the missing-token message is on stderr", p.returncode, out, err)
    if os.listdir(home):
        fail(f"the bridge wrote under its home: {os.listdir(home)}", p.returncode, out, err)
    print("OK: no token -> exit 2, the message on stderr, nothing on stdout.")


def handshake(exe: str) -> None:
    home = tempfile.mkdtemp(prefix="openalgo-mcp-smoke-")
    p = subprocess.Popen(
        [exe, "mcp", "--url", CLOSED_URL],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=isolated_env(home, TOKEN),
    )
    lines: queue.Queue[str] = queue.Queue()
    errors: list[bytes] = []

    def read_stdout() -> None:
        assert p.stdout is not None
        for raw in p.stdout:
            lines.put(raw.decode("utf-8", "replace"))

    def read_stderr() -> None:
        assert p.stderr is not None
        errors.append(p.stderr.read())

    threading.Thread(target=read_stdout, daemon=True).start()
    threading.Thread(target=read_stderr, daemon=True).start()
    try:
        hello = {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "ci-smoke", "version": "1"},
            },
        }
        assert p.stdin is not None
        p.stdin.write((json.dumps(hello) + "\n").encode("utf-8"))
        p.stdin.flush()
        try:
            first = lines.get(timeout=WAIT_S)
        except queue.Empty:
            fail("the bridge answers initialize on stdout", p.poll(), "", b"".join(errors).decode())
        try:
            reply = json.loads(first)
        except ValueError:
            reply = {}
        server = (reply.get("result") or {}).get("serverInfo") or {}
        if reply.get("id") != 1 or server.get("name") != "openalgo":
            fail("the first stdout line is the initialize result from openalgo", p.poll(), first, "")
        # End of input ends the session and the process.
        p.stdin.close()
        code = p.wait(timeout=WAIT_S)
    finally:
        if p.poll() is None:
            p.kill()
            p.wait(timeout=WAIT_S)
    err = b"".join(errors).decode("utf-8", "replace")
    if code != 0:
        fail("closing stdin ends the bridge with code 0", code, first, err)
    if TOKEN in first or TOKEN in err:
        fail("the token is never echoed", code, first, err)
    if os.listdir(home):
        fail(f"the bridge wrote under its home: {os.listdir(home)}", code, first, err)
    print("OK: initialize answered over piped stdio; exit 0 at end of input.")


class Bridge:
    """One `mcp` process with piped stdio, read on a thread."""

    def __init__(self, exe: str, url: str, token: str) -> None:
        self.home = tempfile.mkdtemp(prefix="openalgo-mcp-smoke-")
        self.token = token
        self.proc = subprocess.Popen(
            [exe, "mcp", "--url", url],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=isolated_env(self.home, token),
        )
        self.lines: queue.Queue[str] = queue.Queue()
        self.stdout: list[str] = []
        self.errors: list[bytes] = []
        self.next_id = 0
        threading.Thread(target=self._read_stdout, daemon=True).start()
        threading.Thread(target=self._read_stderr, daemon=True).start()

    def _read_stdout(self) -> None:
        assert self.proc.stdout is not None
        for raw in self.proc.stdout:
            line = raw.decode("utf-8", "replace")
            self.stdout.append(line)
            self.lines.put(line)

    def _read_stderr(self) -> None:
        assert self.proc.stderr is not None
        self.errors.append(self.proc.stderr.read())

    def send(self, message: dict) -> None:
        assert self.proc.stdin is not None
        self.proc.stdin.write((json.dumps(message) + "\n").encode("utf-8"))
        self.proc.stdin.flush()

    def request(self, method: str, params: dict) -> dict:
        """The answer to one request; notifications before it are skipped."""
        self.next_id += 1
        self.send({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params})
        deadline = time.monotonic() + WAIT_S
        while True:
            try:
                reply = json.loads(self.lines.get(timeout=max(deadline - time.monotonic(), 0.01)))
            except (queue.Empty, ValueError):
                reply = {}
            if isinstance(reply, dict) and "id" not in reply and "method" in reply:
                continue
            break
        if not isinstance(reply, dict) or reply.get("id") != self.next_id:
            raise RuntimeError(f"no answer to {method}: {reply}")
        return reply

    def initialize(self, version: str) -> dict:
        reply = self.request("initialize", {
            "protocolVersion": version,
            "capabilities": {},
            "clientInfo": {"name": "ci-smoke", "version": "1"},
        })
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        return reply

    def call(self, tool: str, arguments: dict) -> tuple[bool, str]:
        """(isError, the text) of a tools/call."""
        result = self.request("tools/call", {"name": tool, "arguments": arguments}).get("result") or {}
        content = result.get("content") or [{}]
        return bool(result.get("isError")), str(content[0].get("text", ""))

    def close(self) -> list[str]:
        """Ends the session; what went wrong with how the process behaved."""
        problems: list[str] = []
        assert self.proc.stdin is not None
        try:
            self.proc.stdin.close()
        except OSError:
            pass  # the bridge already exited; its exit code says why
        try:
            code = self.proc.wait(timeout=WAIT_S)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait(timeout=WAIT_S)
            code = None
        err = b"".join(self.errors).decode("utf-8", "replace")
        if code != 0:
            problems.append(f"closing stdin should end the bridge with code 0, got {code}: {err[-400:]}")
        for line in self.stdout:
            try:
                ok = json.loads(line).get("jsonrpc") == "2.0"
            except ValueError:
                ok = False
            if not ok:
                problems.append(f"stdout carried something other than JSON-RPC: {line[:200]!r}")
        if any(self.token in x for x in self.stdout) or self.token in err:
            problems.append("the token was echoed")
        if os.listdir(self.home):
            problems.append(f"the bridge wrote under its home: {os.listdir(self.home)}")
        return problems


def against_app(exe: str, url: str, token: str, revoke) -> list[str]:
    """The packaged bridge against the running app (PLATFORM-07, MCP-04).

    initialize answers with the server's own protocol version, also for one
    it does not speak; tools/list lists the 49 tools; two tools that need no
    broker answer through the app; after `revoke()` the next call says the
    token was not accepted. Returns what went wrong.
    """
    problems: list[str] = []
    odd = Bridge(exe, url, token)
    try:
        agreed = (odd.initialize("1999-01-01").get("result") or {}).get("protocolVersion")
        if not agreed or agreed == "1999-01-01":
            problems.append(f"an unknown protocol version was not answered with a supported one: {agreed}")
    except (RuntimeError, OSError) as e:
        problems.append(f"the bridge failed: {e}")
    finally:
        problems += odd.close()

    b = Bridge(exe, url, token)
    try:
        init = b.initialize("2025-06-18").get("result") or {}
        if (init.get("serverInfo") or {}).get("name") != "openalgo" or not init.get("instructions"):
            problems.append(f"initialize did not come from openalgo with its instructions: {init}")
        tools = (b.request("tools/list", {}).get("result") or {}).get("tools") or []
        print(f"tools/list through the packaged bridge: {len(tools)} tools")
        if len(tools) != 49:
            problems.append(f"tools/list gave {len(tools)} tools, not 49")
        failed, text = b.call("get_openalgo_version", {})
        if failed or "OpenAlgo version" not in text:
            problems.append(f"get_openalgo_version was not answered by the app: {text[:300]}")
        failed, text = b.call("validate_order_constants", {})
        if failed or "exchanges" not in text:
            problems.append(f"validate_order_constants was not answered by the app: {text[:300]}")
        revoke()
        failed, text = b.call("get_funds", {})
        if not failed or "did not accept this AI client's token" not in text:
            problems.append(f"a revoked token was not told so: {text[:300]}")
    except (RuntimeError, OSError) as e:
        problems.append(f"the bridge failed: {e}")
    finally:
        problems += b.close()
    if not problems:
        print("OK: the packaged bridge served the running app: protocol versions, 49 tools, two calls, a revoked token.")
    return problems


def main(argv: list[str]) -> int:
    if len(argv) != 1:
        print(__doc__)
        return 2
    without_token(argv[0])
    handshake(argv[0])
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
