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
"""

from __future__ import annotations

import json
import os
import queue
import subprocess
import sys
import tempfile
import threading

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


def main(argv: list[str]) -> int:
    if len(argv) != 1:
        print(__doc__)
        return 2
    without_token(argv[0])
    handshake(argv[0])
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
