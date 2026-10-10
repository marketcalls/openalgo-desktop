#!/usr/bin/env python3
"""Start an installed OpenAlgo Desktop and wait until it serves its HTTP API.

    python3 scripts/ci/launch_smoke.py <command> [args...]

For CI runners only (.github/workflows/install-smoke.yml): a release build
binds the shipped ports 127.0.0.1:5000 and 8765, which on a developer's
machine may belong to OpenAlgo web or a running app. The script refuses to
run outside CI for that reason.

The app gets a fresh, empty home and data folder, so it starts as a first
install would. The script polls GET /auth/csrf-token (public, no side
effect) until it answers 200 with a token, then stops the app and reaps it.
It fails if the app exits first or does not answer within the deadline,
and prints the app's output either way.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

# SMOKE_URL exists to exercise this script against a stand-in server.
URL = os.environ.get("SMOKE_URL", "http://127.0.0.1:5000/auth/csrf-token")
DEADLINE_S = int(os.environ.get("SMOKE_DEADLINE_S", "90"))


def fresh_env(home: str) -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith("OPENALGO")}
    for var in ("HOME", "XDG_DATA_HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "APPDATA", "LOCALAPPDATA"):
        env[var] = os.path.join(home, var.lower())
        os.makedirs(env[var], exist_ok=True)
    return env


def answers() -> bool:
    try:
        with urllib.request.urlopen(URL, timeout=3) as r:
            body = json.loads(r.read().decode("utf-8"))
            return r.status == 200 and bool(body.get("csrf_token"))
    except (urllib.error.URLError, OSError, ValueError):
        return False


def stop(proc: subprocess.Popen) -> None:
    if proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=15)


def main(argv: list[str]) -> int:
    if not argv:
        print(__doc__)
        return 2
    if os.environ.get("CI") != "true":
        print("launch_smoke.py binds the shipped ports 5000 and 8765; it runs only in CI.")
        return 2
    if answers():
        print(f"Something already answers on {URL}; refusing to start a second app.")
        return 1
    home = tempfile.mkdtemp(prefix="openalgo-smoke-")
    log_path = os.path.join(home, "app.log")
    started = time.monotonic()
    with open(log_path, "wb") as log:
        proc = subprocess.Popen(argv, env=fresh_env(home), stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
        ok = False
        try:
            while time.monotonic() - started < DEADLINE_S:
                if proc.poll() is not None:
                    break
                if answers():
                    ok = True
                    break
                time.sleep(1)
        finally:
            exited = proc.poll()
            stop(proc)
    with open(log_path, "rb") as log:
        output = log.read().decode("utf-8", "replace")
    print("---- app output (last 80 lines)")
    print("\n".join(output.splitlines()[-80:]))
    print("----")
    elapsed = time.monotonic() - started
    if ok:
        print(f"OK: {URL} answered after {elapsed:.1f} s.")
        return 0
    if exited is not None:
        print(f"FAILED: the app exited with code {exited} after {elapsed:.1f} s without answering.")
    else:
        print(f"FAILED: no answer from {URL} within {DEADLINE_S} s.")
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
