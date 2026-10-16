#!/usr/bin/env python3
"""Start an installed OpenAlgo Desktop and wait until it serves its HTTP API.

    python3 scripts/ci/launch_smoke.py [--first-sign-in] [--real-home] [--mcp-bridge] <command> [args...]

For CI runners only (.github/workflows/install-smoke.yml): a release build
binds the shipped ports 127.0.0.1:5000 and 8765, which on a developer's
machine may belong to OpenAlgo web or a running app. The script refuses to
run outside CI for that reason.

The app gets a fresh, empty home and data folder, so it starts as a first
install would. With --real-home it keeps the runner's own home instead
(the runner itself is fresh, so the data folder is empty either way). The
app uses no OS keychain on any system (KEY-01), so neither choice changes
where its keys go. The script polls GET /auth/csrf-token (public, no side
effect) until it answers 200 with a token.

With --first-sign-in it then does what a trader does on first start,
through the app's own HTTP on the shipped port: creates the account
(POST /setup), signs in (POST /auth/login), and checks that the session
is signed in (GET /auth/session-status). It also checks that a wrong
password is refused, so the sign-in check can fail. The password is random
per run and never printed. KEY-01: the keys are locked with the password
in openalgo.db, so session-status must say "locked" at the start and after
the account is created, and not once the trader signed in.

With --mcp-bridge, once signed in, it creates an MCP token through the
app's own HTTP (POST /api/mcp/tokens, as the API Key page does) and drives
the same binary's `mcp` subcommand against the running app
(scripts/ci/mcp_stdio_smoke.py `against_app`), revoking the token midway.

Then it stops the app and reaps it. It fails if the app exits first, does
not answer within the deadline, or a sign-in step fails, and prints the
app's output either way.
"""

from __future__ import annotations

import http.cookiejar
import json
import os
import secrets
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

# SMOKE_BASE exists to exercise this script against a stand-in server.
BASE = os.environ.get("SMOKE_BASE", "http://127.0.0.1:5000").rstrip("/")
URL = BASE + "/auth/csrf-token"
DEADLINE_S = int(os.environ.get("SMOKE_DEADLINE_S", "90"))
USERNAME = "smoke"
EMAIL = "smoke@example.com"


def fresh_env(home: str, real_home: bool) -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith("OPENALGO")}
    if real_home:
        return env
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


class Client:
    """One browser-like session: a cookie jar and the CSRF token."""

    def __init__(self) -> None:
        self.opener = urllib.request.build_opener(
            urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar())
        )

    def call(self, method: str, path: str, form: dict[str, str] | None = None) -> tuple[int, dict]:
        headers = {"Accept": "application/json"}
        data = None
        if form is not None:
            token = self.csrf()
            data = urllib.parse.urlencode({**form, "csrf_token": token}).encode("utf-8")
            headers["Content-Type"] = "application/x-www-form-urlencoded"
            headers["X-CSRFToken"] = token
        req = urllib.request.Request(BASE + path, data=data, method=method, headers=headers)
        try:
            with self.opener.open(req, timeout=30) as r:
                status, raw = r.status, r.read()
        except urllib.error.HTTPError as e:
            status, raw = e.code, e.read()
        try:
            body = json.loads(raw.decode("utf-8")) if raw else {}
        except ValueError:
            body = {"unparsed": raw[:200].decode("utf-8", "replace")}
        return status, body if isinstance(body, dict) else {"value": body}

    def call_json(self, method: str, path: str, body: dict | None = None) -> tuple[int, dict]:
        """A JSON request with the session's CSRF token (the app's own pages)."""
        token = self.csrf()
        headers = {"Accept": "application/json", "X-CSRFToken": token, "Content-Type": "application/json"}
        data = json.dumps(body).encode("utf-8") if body is not None else None
        req = urllib.request.Request(BASE + path, data=data, method=method, headers=headers)
        try:
            with self.opener.open(req, timeout=30) as r:
                status, raw = r.status, r.read()
        except urllib.error.HTTPError as e:
            status, raw = e.code, e.read()
        try:
            parsed = json.loads(raw.decode("utf-8")) if raw else {}
        except ValueError:
            parsed = {"unparsed": raw[:200].decode("utf-8", "replace")}
        return status, parsed if isinstance(parsed, dict) else {"value": parsed}

    def csrf(self) -> str:
        req = urllib.request.Request(URL, headers={"Accept": "application/json"})
        with self.opener.open(req, timeout=30) as r:
            return json.loads(r.read().decode("utf-8"))["csrf_token"]


def mcp_bridge(exe: str):
    """The check run with the signed-in session (--mcp-bridge)."""

    def check(browser: Client) -> list[str]:
        import mcp_stdio_smoke

        status, body = browser.call_json("POST", "/api/mcp/tokens", {"name": "install smoke", "scope": "read"})
        token, row = body.get("token"), body.get("data") or {}
        print(f"POST /api/mcp/tokens: {status} {body.get('status')}")
        if status != 200 or not token or "id" not in row:
            return [f"creating an MCP token failed: {status} {body.get('message', '')}"]

        def revoke() -> None:
            s, b = browser.call_json("DELETE", f"/api/mcp/tokens/{row['id']}")
            print(f"DELETE /api/mcp/tokens/<id>: {s} {b.get('status')}")

        return mcp_stdio_smoke.against_app(exe, BASE, token, revoke)

    return check


def first_sign_in(then=None) -> list[str]:
    """Creates the account and signs in; returns what went wrong, if anything.
    `then(browser)` runs with the signed-in session and adds its problems."""
    # Meets the app's rules: upper and lower case, a digit, one of !@#$%^&*.
    password = "Aa1!" + secrets.token_hex(12)
    problems: list[str] = []

    browser = Client()

    def locked(when: str, expected: bool) -> None:
        status, body = browser.call("GET", "/auth/session-status")
        print(f"GET /auth/session-status {when}: {status} locked={body.get('locked')} key_mode={body.get('key_mode')}")
        if body.get("locked") is not expected or body.get("key_mode") != "password":
            problems.append(f"{when} the keys should be {'locked' if expected else 'open'}: {status} {body}")

    locked("at the start", True)
    status, body = browser.call("POST", "/setup", {
        "username": USERNAME,
        "email": EMAIL,
        "password": password,
        "confirm_password": password,
    })
    print(f"POST /setup: {status} {body.get('status')} {body.get('message', '')}")
    if status != 200 or body.get("status") != "success":
        return problems + [f"creating the account failed: {status} {body}"]
    locked("after the account was created", True)

    status, body = browser.call("POST", "/auth/login", {"username": USERNAME, "password": password})
    print(f"POST /auth/login: {status} {body.get('status')} {body.get('message', '')}")
    if status != 200 or body.get("status") != "success":
        problems.append(f"signing in failed: {status} {body}")

    status, body = browser.call("GET", "/auth/session-status")
    print(f"GET /auth/session-status: {status} authenticated={body.get('authenticated')}")
    if status != 200 or body.get("authenticated") is not True:
        problems.append(f"the session is not signed in after the sign-in: {status} {body}")
    locked("after the sign-in", False)
    if then is not None and not problems:
        problems += then(browser)

    # The check must be able to fail: a wrong password is refused and leaves
    # a fresh session signed out.
    stranger = Client()
    status, body = stranger.call("POST", "/auth/login", {"username": USERNAME, "password": password + "x"})
    print(f"POST /auth/login with a wrong password: {status} {body.get('status')}")
    if body.get("status") == "success":
        problems.append("a wrong password was accepted")
    status, body = stranger.call("GET", "/auth/session-status")
    if body.get("authenticated") is not False:
        problems.append(f"a refused sign-in left a signed-in session: {status} {body}")
    return problems


def stop(proc: subprocess.Popen) -> None:
    if proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=15)


def main(argv: list[str]) -> int:
    flags = set()
    while argv and argv[0] in ("--first-sign-in", "--real-home", "--mcp-bridge"):
        flags.add(argv.pop(0))
    sign_in = "--first-sign-in" in flags or "--mcp-bridge" in flags
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
    problems: list[str] = []
    with open(log_path, "wb") as log:
        proc = subprocess.Popen(argv, env=fresh_env(home, "--real-home" in flags), stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
        ok = False
        try:
            while time.monotonic() - started < DEADLINE_S:
                if proc.poll() is not None:
                    break
                if answers():
                    ok = True
                    break
                time.sleep(1)
            answered_after = time.monotonic() - started
            if ok and sign_in:
                problems = first_sign_in(mcp_bridge(argv[0]) if "--mcp-bridge" in flags else None)
                if proc.poll() is not None:
                    problems.append(f"the app exited with code {proc.poll()} during the sign-in")
        finally:
            exited = proc.poll()
            stop(proc)
    with open(log_path, "rb") as log:
        output = log.read().decode("utf-8", "replace")
    print("---- app output (last 80 lines)")
    print("\n".join(output.splitlines()[-80:]))
    print("----")
    if ok and not problems:
        what = " and the first sign-in worked" if sign_in else ""
        print(f"OK: {URL} answered after {answered_after:.1f} s{what}.")
        return 0
    for p in problems:
        print(f"FAILED: {p}")
    if not ok:
        elapsed = time.monotonic() - started
        if exited is not None:
            print(f"FAILED: the app exited with code {exited} after {elapsed:.1f} s without answering.")
        else:
            print(f"FAILED: no answer from {URL} within {DEADLINE_S} s.")
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
