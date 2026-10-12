#!/usr/bin/env python3
"""End-to-end through the installed app's own window, by WebDriver.

    python3 scripts/ci/native_e2e.py <installed app binary>

CI only (.github/workflows/native-e2e.yml): the app binds the shipped ports
5000 and 8765, the WebDriver server 4444 and on Windows the webview's
debugging port 9222. Needs `selenium` and, on Linux, `tauri-driver` on PATH
with WebKitWebDriver; on Windows, MSEDGEDRIVER naming the msedgedriver of
the installed WebView2's version, which the script attaches to the app's
webview.

Unlike the Playwright suite (a browser against `vite preview` with stubbed
answers), this drives the native webview of the installed release build,
against its real Rust server, sign-in and session cookie:

1. first start: the setup form creates the account;
2. a wrong password is refused and leaves the trader on the sign-in page;
3. the right one signs in; the signed-in shell (the navigation bar) shows on
   Profile, and the dashboard sends a trader with no broker to Connect;
4. after sign-out the dashboard sends the trader back to sign in.

The app gets a fresh, empty home and data folder. The password is random
per run and never printed.
"""

from __future__ import annotations

import os
import secrets
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

BASE = "http://127.0.0.1:5000"
DRIVER_URL = "http://127.0.0.1:4444"
WAIT_S = 60


def fresh_env(home: str) -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith("OPENALGO")}
    for var in ("HOME", "XDG_DATA_HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "APPDATA", "LOCALAPPDATA"):
        env[var] = os.path.join(home, var.lower())
        os.makedirs(env[var], exist_ok=True)
    return env


def wait_http(url: str, deadline_s: float) -> bool:
    end = time.monotonic() + deadline_s
    while time.monotonic() < end:
        try:
            with urllib.request.urlopen(url, timeout=3):
                return True
        except urllib.error.HTTPError:
            return True
        except (urllib.error.URLError, OSError):
            time.sleep(0.5)
    return False


def main(argv: list[str]) -> int:
    if len(argv) != 1:
        print(__doc__)
        return 2
    if os.environ.get("CI") != "true":
        print("native_e2e.py binds the shipped ports 5000 and 8765 and 4444; it runs only in CI.")
        return 2
    from selenium import webdriver
    from selenium.webdriver.common.by import By
    from selenium.webdriver.common.options import ArgOptions
    from selenium.webdriver.support import expected_conditions as ec
    from selenium.webdriver.support.ui import WebDriverWait

    app = os.path.abspath(argv[0])
    home = tempfile.mkdtemp(prefix="openalgo-e2e-")
    log_path = os.path.join(home, "driver-and-app.log")
    log = open(log_path, "wb")
    env = fresh_env(home)
    # Windows: attach msedgedriver to the app's WebView2 through a fixed
    # remote debugging port. Launched by msedgedriver (tauri-driver's way),
    # the app's webview, whose user data folder the app sets itself, never
    # left the port file the driver waits for.
    edge = os.environ.get("MSEDGEDRIVER")
    app_proc = None
    if edge:
        app_env = dict(env, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9222")
        app_proc = subprocess.Popen([app], env=app_env, stdout=log, stderr=subprocess.STDOUT)
        driver_proc = subprocess.Popen([edge, "--port=4444"], env=env, stdout=log, stderr=subprocess.STDOUT)
    else:
        cmd = ["tauri-driver"]
        native = os.environ.get("TAURI_NATIVE_DRIVER")
        if native:
            cmd += ["--native-driver", native]
        driver_proc = subprocess.Popen(cmd, env=env, stdout=log, stderr=subprocess.STDOUT)
    problems: list[str] = []
    session = None
    try:
        if not wait_http(DRIVER_URL + "/status", 30):
            raise RuntimeError("the WebDriver server did not answer on 4444")
        options = ArgOptions()
        if edge:
            if not wait_http("http://127.0.0.1:9222/json/version", WAIT_S):
                raise RuntimeError("the app's webview did not open its debugging port")
            options.set_capability("browserName", "webview2")
            options.set_capability("ms:edgeOptions", {"debuggerAddress": "127.0.0.1:9222"})
        else:
            options.set_capability("browserName", "wry")
            options.set_capability("tauri:options", {"application": app})
        session = webdriver.Remote(command_executor=DRIVER_URL, options=options)
        wait = WebDriverWait(session, WAIT_S)
        if not wait_http(BASE + "/auth/csrf-token", WAIT_S):
            raise RuntimeError("the app's server did not answer")

        def path() -> str:
            return session.current_url.split("127.0.0.1:5000", 1)[-1].split("?", 1)[0]

        def field(element_id: str, value: str) -> None:
            el = wait.until(ec.element_to_be_clickable((By.ID, element_id)))
            el.clear()
            el.send_keys(value)

        def submit() -> None:
            wait.until(ec.element_to_be_clickable((By.CSS_SELECTOR, "button[type=submit]"))).click()

        password = "Aa1!" + secrets.token_hex(12)

        # 1. First start: create the account.
        session.get(BASE + "/setup")
        field("username", "trader")
        field("email", "trader@example.com")
        field("password", password)
        field("confirmPassword", password)
        submit()
        wait.until(lambda d: path() == "/login")
        print("setup: account created, on /login")

        # 2. A wrong password is refused.
        field("username", "trader")
        field("password", password + "x")
        submit()
        time.sleep(2)
        if path() != "/login":
            problems.append(f"a wrong password left the sign-in page for {path()}")
        print(f"wrong password: still on {path()}")

        # 3. The right password signs in.
        field("password", password)
        submit()
        wait.until(lambda d: path() == "/broker")
        print("signed in: on /broker (no broker connected yet)")
        cookies = {c["name"] for c in session.get_cookies()}
        print(f"cookies after sign-in: {sorted(cookies)}")
        session.get(BASE + "/profile")
        wait.until(ec.visibility_of_element_located((By.CSS_SELECTOR, "[data-testid=navbar-row]")))
        print("signed-in shell: navigation bar on /profile")
        session.get(BASE + "/dashboard")
        wait.until(lambda d: path() == "/broker")
        print("dashboard without a broker: sent to /broker")

        # 4. Sign out.
        session.get(BASE + "/auth/logout")
        session.get(BASE + "/dashboard")
        wait.until(lambda d: path() == "/login")
        print("signed out: /dashboard sends to /login")
    except Exception as e:  # noqa: BLE001 - report every failure the same way
        problems.append(f"{type(e).__name__}: {e}")
        if session is not None:
            try:
                print(f"at {session.current_url}")
            except Exception:  # noqa: BLE001
                pass
    finally:
        if session is not None:
            try:
                session.quit()
            except Exception:  # noqa: BLE001
                pass
        for proc in (driver_proc, app_proc):
            if proc is None or proc.poll() is not None:
                continue
            proc.terminate()
            try:
                proc.wait(timeout=15)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=15)
        log.close()
        with open(log_path, "rb") as f:
            lines = f.read().decode("utf-8", "replace").splitlines()
        print("---- WebDriver server and app output (last 60 lines)")
        print("\n".join(lines[-60:]))
        print("----")
    for p in problems:
        print(f"FAILED: {p}")
    if problems:
        return 1
    print("OK: setup, refused wrong password, sign-in, signed-in shell and sign-out in the native window.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
