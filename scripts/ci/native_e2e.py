#!/usr/bin/env python3
"""End-to-end through the installed app's own window, by WebDriver.

    python3 scripts/ci/native_e2e.py [--sandbox-order] <app binary>

CI only: the app binds the shipped ports 5000 and 8765 and the WebDriver
server binds 4444. macOS has no WebDriver for WKWebView.

- Linux: `tauri-driver` (on PATH) starts the app with WebKitWebDriver: the
  installed .deb in every CI run (.github/workflows/native-e2e.yml), the
  E2E test build after pushes to master
  (.github/workflows/native-e2e-test-build.yml).
- Windows (native-e2e-test-build.yml): the E2E test build
  (src-tauri/src/e2e.rs), never a release. MSEDGEDRIVER names the
  msedgedriver of the installed WebView2's version. The script picks a
  free port, starts the app with OPENALGO_E2E_DEBUG_PORT set to it, checks
  the webview's debugging server listens on 127.0.0.1 only, and attaches
  msedgedriver to it (debuggerAddress).

Unlike the Playwright suite (a browser against `vite preview` with stubbed
answers), this drives the native webview of the installed release build,
against its real Rust server, sign-in and session cookie:

1. first start: the setup form creates the account;
2. a wrong password is refused and leaves the trader on the sign-in page;
3. the right one signs in; the signed-in shell (the navigation bar) shows on
   Profile, and the dashboard sends a trader with no broker to Connect;
4. with --sandbox-order (the test build only): a sandbox order placed
   from the order form, found in the order book, never at the broker
   (scripts/ci/native_e2e_sandbox.py);
5. after sign-out the dashboard sends the trader back to sign in.

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


def free_port() -> int:
    import socket

    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def listen_addresses(port: int) -> set[str]:
    """Local addresses something listens on at `port` (Windows netstat)."""
    out = subprocess.run(["netstat", "-ano", "-p", "TCP"], capture_output=True, text=True, timeout=30).stdout
    found = set()
    for line in out.splitlines():
        cols = line.split()
        if len(cols) >= 4 and cols[0] == "TCP" and cols[3] == "LISTENING":
            addr, _, p = cols[1].rpartition(":")
            if p == str(port):
                found.add(addr)
    return found


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
    # A Windows runner's console is cp1252: print what it cannot show (the
    # order book's rupee sign) escaped, rather than stop on it.
    sys.stdout.reconfigure(errors="backslashreplace")
    sandbox_order = "--sandbox-order" in argv
    argv = [a for a in argv if a != "--sandbox-order"]
    if len(argv) != 1:
        print(__doc__)
        return 2
    if os.environ.get("CI") != "true":
        print("native_e2e.py binds the shipped ports 5000 and 8765 and 4444; it runs only in CI.")
        return 2
    from selenium import webdriver
    from selenium.common.exceptions import StaleElementReferenceException
    from selenium.webdriver.common.by import By
    from selenium.webdriver.common.options import ArgOptions
    from selenium.webdriver.support import expected_conditions as ec
    from selenium.webdriver.support.ui import WebDriverWait

    app = os.path.abspath(argv[0])
    home = tempfile.mkdtemp(prefix="openalgo-e2e-")
    log_path = os.path.join(home, "driver-and-app.log")
    log = open(log_path, "wb")
    edge = os.environ.get("MSEDGEDRIVER")
    app_proc = None
    if edge:
        # Windows: the test build opens its webview's debugging port on the
        # port it is given; msedgedriver attaches to it. The runner's own
        # profile is kept (the runner is fresh).
        port = free_port()
        app_env = {k: v for k, v in os.environ.items() if not k.startswith("OPENALGO")}
        app_env["OPENALGO_E2E_DEBUG_PORT"] = str(port)
        app_proc = subprocess.Popen([app], env=app_env, stdout=log, stderr=subprocess.STDOUT)
        driver_proc = subprocess.Popen([edge, "--port=4444"], stdout=log, stderr=subprocess.STDOUT)
    else:
        # tauri-driver starts the app (and the platform's WebDriver) itself.
        driver_proc = subprocess.Popen(["tauri-driver"], env=fresh_env(home), stdout=log, stderr=subprocess.STDOUT)
    problems: list[str] = []
    session = None
    try:
        if not wait_http(DRIVER_URL + "/status", 30):
            raise RuntimeError("the WebDriver server did not answer on 4444")
        options = ArgOptions()
        if edge:
            if not wait_http(f"http://127.0.0.1:{port}/json/version", WAIT_S):
                raise RuntimeError("the test build's webview did not open its debugging port")
            addresses = listen_addresses(port)
            print(f"debugging port {port} listens on {sorted(addresses)}")
            if addresses != {"127.0.0.1"}:
                problems.append(f"the debugging port listens beyond 127.0.0.1: {sorted(addresses)}")
            options.set_capability("browserName", "webview2")
            options.set_capability("ms:edgeOptions", {"debuggerAddress": f"127.0.0.1:{port}"})
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
            # A field the page replaced while it was being typed into is
            # found again and typed into afresh.
            for attempt in range(3):
                el = wait.until(ec.element_to_be_clickable((By.ID, element_id)))
                try:
                    el.clear()
                    el.send_keys(value)
                    return
                except StaleElementReferenceException:
                    if attempt == 2:
                        raise

        def submit() -> None:
            wait.until(ec.element_to_be_clickable((By.CSS_SELECTOR, "button[type=submit]"))).click()

        password = "Aa1!" + secrets.token_hex(12)

        # 1. First start: create the account.
        session.get(BASE + "/setup")
        field("username", "trader")
        field("email", "trader@example.com")
        field("password", password)
        field("confirmPassword", password)
        setup_form = session.find_element(By.ID, "confirmPassword")
        submit()
        # The address reads /login while the setup page is still shown: the
        # router keeps it until the lazily loaded sign-in page is ready, and
        # both pages have a #username field. Typing into the setup page's
        # failed on Linux (master runs of 1d8ff5b03); wait for it to go.
        wait.until(lambda d: path() == "/login")
        wait.until(ec.staleness_of(setup_form))
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

        # 4. A sandbox order, in the test build.
        if sandbox_order:
            import native_e2e_sandbox

            problems += native_e2e_sandbox.run(session, wait, BASE, path)

        # 5. Sign out.
        session.get(BASE + "/auth/logout")
        session.get(BASE + "/dashboard")
        wait.until(lambda d: path() == "/login")
        print("signed out: /dashboard sends to /login")
    except Exception as e:  # noqa: BLE001 - report every failure the same way
        problems.append(f"{type(e).__name__}: {e}")
        if session is not None:
            try:
                print(f"at {session.current_url}")
                # What the window showed, for the workflow's failure artifact.
                shot = os.path.join(os.environ.get("RUNNER_TEMP", home), "native-e2e-failure.png")
                session.save_screenshot(shot)
                print(f"screenshot: {shot}")
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
        print("---- WebDriver and app output (last 60 lines)")
        print("\n".join(lines[-60:]))
        print("----")
    for p in problems:
        print(f"FAILED: {p}")
    if problems:
        return 1
    sandbox = " a sandbox order from the order form," if sandbox_order else ""
    print(f"OK: setup, refused wrong password, sign-in, signed-in shell,{sandbox} and sign-out in the native window.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
