#!/usr/bin/env python3
"""Record what OpenAlgo web's Flow logic nodes compute, for the desktop's port.

The desktop's Math Expression, Variable and HTTP Request nodes reproduce
Python behaviour the web's executor inherits from the interpreter: the
`ast`-walking evaluator `_safe_eval_math` with Python's integer, float and
complex arithmetic, `json.loads` (its accepted forms and error sentences),
`urllib.parse.urlsplit` (as `_redact_url` and the destination check read a
URL) and the `ipaddress` classes the destination check refuses. This script
runs the web's own `_safe_eval_math` and `_redact_url` (taken from the
committed executor) and the interpreter's own `json` and `ipaddress` over a
corpus, and writes what they answer to `logic_vectors.json`, which
`src/flow/nodes/tests/vectors.rs` replays against the Rust port.

Run it with the web's own interpreter (Python 3.12) against a read-only
checkout; it reads only the committed `--ref` and writes nothing there:

    PYTHONDONTWRITEBYTECODE=1 ../openalgo/.venv/bin/python -I \\
        scripts/gen_flow_logic_vectors.py --web ../openalgo \\
        --ref 96e5927e5 --out src-tauri/tests/fixtures/flow
"""

from __future__ import annotations

import argparse
import ast
import ipaddress
import json
import math
import subprocess
import sys
from pathlib import Path

MATH = [
    # literals
    "1", "0", "00", "0_0", "1_000", "0x10", "0X1f", "0o17", "0b101", "1e3", ".5", "5.", "1.5e-3",
    "1_0.2_5", "1e400", "-1e400", "1E+2", "0e0", "123456789012345678901234567890",
    "9007199254740993", "2**53 + 1", "(2**60 + 1) - 2**60", "10 ** 20", "10 ** 22", "10 ** 23",
    "2 ** 1023 * 2", "2 ** 100 * 2 ** 100 * 2 ** 100 * 2 ** 100 * 2 ** 100 * 2 ** 100 * 2 ** 100 * 2 ** 100 * 2 ** 100 * 2 ** 100 * 2 ** 100",
    # operators and precedence
    "2 + 3", "2 - 3", "2 * 3", "7 / 2", "-7 / 2", "1 / 3", "2 / 3", "10 / 4", "6 / 3",
    "-2 ** 2", "(-2) ** 2", "2 ** 3 ** 2", "2 ** -1", "2 ** -2 ** 2", "-2 ** -2",
    "+3", "-(-3)", "- - 3", "+-+3", "2 * -3", "2 ** 10 % 7 - -1", "1 + 2 * 3 - 4 / 2",
    "(1 + 2) * 3", "((((1))))", "  2+3  ", "(1 +\n 2)", "(1 + # c\n 2)", "1 + 2 # trailing",
    "1 \\\n + 2",
    # modulo
    "-7 % 3", "7 % -3", "-7 % -3", "7 % 3", "-7.5 % 2", "7.5 % -2", "5 % 2.5", "-0.0 % 5", "0.0 % -5",
    "10 ** 30 % 7", "-(10 ** 30) % 7", "(2**70) % (2**35 + 3)", "-(2**70) % (2**35 + 3)",
    "(2**70) % -(2**35 + 3)", "5.5 % 1e300", "1e300 % 7",
    # true division of integers, exact rounding
    "(10**30) / 3", "(10**30 + 1) / (10**15)", "(2**80 + 1) / 2**27", "(3 * 2**60) / 2**3",
    "1 / (10**400)", "(10**400) / (10**399)", "(10**400) / 3", "(2**1100) / (2**1000)",
    "(2**1100) / 3", "(2**2000) / (2**10)", "(2**1074 + 1) / 2**2148", "1 / 2**1074", "1 / 2**1075",
    "3 / 2**1075", "(2**53 + 1) / 1", "(2**54 + 2) / 2", "(2**54 + 6) / 2", "-(10**30) / 7",
    "0 / 5", "0 / -5", "-0.0 / 5", "0.0 / -5",
    # floor
    "floor(3.9)", "floor(2 + 2.8)", "floor(-2.5)", "floor(5)", "floor(-0.5)", "floor(1e20)",
    "floor(1e300) / 10**290", "floor(10**30 / 7)", "floor(  7.2  )", "floor((1, 2)[0])",
    "floor(1e308 * 10)", "floor(-1e308 * 10)", "floor(1e308 * 10 - 1e308 * 10)",
    "floor(((-8) ** 0.5))",
    # pow
    "2 ** 100", "2 ** 101", "2 ** -100", "2 ** -101", "2.5 ** 2", "4 ** 0.5", "0 ** 0", "0.0 ** 0",
    "0 ** -1", "0.0 ** -1", "0 ** -1.5", "0.0 ** 2", "(-0.0) ** 3", "(-0.0) ** 2", "(-2) ** 3",
    "(-2.0) ** 3", "(-2.0) ** 4", "(-8) ** 0.5", "(-8) ** (1/3)", "(-1) ** 0.5", "(-8.0) ** 0.5 + 1",
    "(-8) ** 0.5 * 2", "(-8) ** 0.5 - (-8) ** 0.5", "(-8) ** 0.5 / 2", "2 / (-8) ** 0.5",
    "(-8) ** 0.5 / 0", "((-8) ** 0.5) ** 2", "((-8) ** 0.5) ** 0.5", "((-8) ** 0.5) ** -1",
    "((-1) ** 0.5) ** 101", "((-1) ** 0.5) ** 100.5", "(0 * (-1) ** 0.5) ** -1",
    "2 ** ((-1) ** 0.5)", "(-8) ** 0.5 % 2", "2 % (-8) ** 0.5", "-((-8) ** 0.5)", "+((-8) ** 0.5)",
    "1e300 ** 2", "10.0 ** 308", "10.0 ** 309", "10.0 ** -310", "10.0 ** -400", "(-10.0) ** 309",
    "1e308 * 10", "-1e308 * 10", "1e308 * 10 - 1e308 * 10", "1e308 * 10 * 0", "(1e308 * 10) ** 0",
    "(1e308 * 10) ** -1", "(-1e308 * 10) ** 3", "(-1e308 * 10) ** 2", "(-1e308 * 10) ** -3",
    "1 ** (1e308 * 10)", "0.5 ** (1e308 * 10)", "2 ** (1e308 * 10)", "0.5 ** (-1e308 * 10)",
    "(1e308 * 10 - 1e308 * 10) ** 0", "(1e308 * 10 - 1e308 * 10) ** 2", "1 ** (1e308 * 10 - 1e308 * 10)",
    "2 ** (1e308 * 10 - 1e308 * 10)", "(-1) ** 1e18", "(-1.0) ** 3.0", "1.0 ** 101",
    "(10 ** 100) ** 4", "(10 ** 100) ** 3", "(10 ** 100) ** -1", "(10 ** 400) ** -1", "(10 ** 400) * 1.0",
    "(10 ** 400) + 0.5", "(10 ** 400) / 2", "(10 ** 400) - (10 ** 400) + 0.5", "2 ** 1024 / 1",
    "float_value_here" if False else "2 ** 100.5", "2 ** -100.0", "2 ** 100.0", "2 ** 101.0",
    "1e300 ** -2", "5 ** (10 ** 100 - 10 ** 100 + 2)",
    # division by zero
    "1 / 0", "1.5 / 0", "1 / 0.0", "0 / 0", "(1e308 * 10 - 1e308 * 10) / 0", "7 % 0", "7.5 % 0",
    "7 % 0.0", "(10 ** 400) % 0", "(10 ** 400) / 0",
    # refusals
    "", "   ", "a", "lot * 2", "floor", "True", "False + 1", "None", "...", "'10' * 2", "\"x\"",
    "b'x'", "b'\\x00\\n'", "'a' 'b'", "1j", "1j * 2", "1.5J", "0j", "1e16j", "f'{1}'", "'a' f'b'",
    "abs(-1)", "ceil(1.1)", "math.floor(2.2)", "floor()", "floor(1, 2)", "floor(x=1)",
    "floor(unknown(1))", "__import__('os').system('echo unsafe')", "floor(*[1])", "floor(**{})",
    "(floor)(2.5)", "floor(2.5)(1)", "floor(1).real", "1 // 2", "1 ^ 2", "1 << 2", "1 >> 2",
    "1 | 2", "1 & 2", "1 @ 2", "~1", "not 1", "not (1 / 0)", "1 < 2", "1 == 1", "1 < 2 < 3",
    "1 in 2", "1 is 1", "1 and 2", "1 or 2", "1 if 2 else 3", "[1]", "(1, 2)", "()", "(1,)",
    "1, 2", "{}", "{1: 2}", "{1}", "{**{}}", "[x for x in y]", "{x for x in y}",
    "{x: 1 for x in y}", "(x for x in y)", "floor(x for x in y)", "lambda: 1", "lambda x, y=2: x",
    "x.y", "x[0]", "x[1:2]", "x[::2, 1]", "(x := 1)", "1 + (2 if 3 else 4)", "-[1]", "+{}",
    "(1 / 0) // 2", "x // (1 / 0)", "(1 / 0) + x", "x + (1 / 0)", "2 ** (1 / 0)", "(1 / 0) ** 200",
    "2 ** 200 ** 0", "2 ** x", "2 ** 1000 / 0", "floor(1 / 0)", "{{x}} * 2", "{{x}}",
    "{'a': 1}", "[1, 2]", "(1 / 0, 2)", "await x", "(yield)", "1 + 2 * (3 - x)", "*a", "*a, b",
    "f(a)(b)", "1 .real", "1.0.real", "1if 1 else 2",
    # syntax errors
    "2 +", "(1 + 2", "1 + 2)", "[1, 2", "(1]", "1 2", "1 = 2", "1 +* 2", "01", "1__0", "1_",
    "0x", "0b2", "0o8", "1e", "'abc", "'''abc", "1;2", "1\n2", "x := 1", "lambda", "1 +\n2",
    "$", "1 ! 2", "f(x=1, 2)", "f(x for x in y, 1)", "[*]", "1.e", "..", "1..2", "a b",
    "if", "else", "(1 if 2)", "1 + ", "()()", "1 <> 2", "1 === 2",
]

JSON = [
    "{bad", "nope", "{\"a\":1,}", "[1,]", "\"abc", "\"a\\x\"", "{\"a\" 1}", "{\"a\":1 \"b\"}",
    "1 2", "\ufeff1", "\"\\u12\"", "\"\\u12zz\"", "[", "", "  ", "-", "01", "1.", "NaN",
    "-Infinity", "Infinity", "1e400", "\"a\tb\"", "{\"ok\": true}", "[1, 2.5, \"x\"]",
    "{\"k\": [true, null]}", "  {\"a\" : 1 }  ", "{\"a\": {\"b\": [1, {\"c\": \"d\"}]}}",
    "\"\\ud83d\\ude00\"", "\"\\u00e9\"", "\"caf\u00e9\"", "\"\\/\\b\\f\\n\\r\\t\\\"\\\\\"",
    "-0", "-0.0", "1E5", "1e-5", "0.1", "123456789012345678901234567890", "[1,2,]", "{\"a\":}",
    "{,}", "{\"a\"", "{\"a\":", "{\"a\":1", "[1", "[1,", "true", "false", "null", "tru",
    "nul", "\"x\" junk", "\n\n  x", "[\n1,\n2\n,]", "{\"a\":1}\n", "{1: 2}", "{'a': 1}",
    "\"\\", "\"abc\\", "\"\\u\"", "[-]", "[-x]", "1.5e", "2.", ".5", "+1", "0x10", "[1 2]",
    "{\"a\":1,\"a\":2}", "\"\u2028\"", "{\"a\": NaN}", "[Infinity, -Infinity]",
]

URLS = [
    "https://api.example.com/send?token=SECRET", "http://user:pw@Api.Example.COM:8080/v1?key=x#frag",
    "http://[::1]/", "http://[::1]:8080/a", "HTTP://EXAMPLE.com", "http://example.com",
    "http://h:99999/", "http://h:ab/", "http://h:0/", "http://h:/", "http:///path", "api.x/x",
    "ftp://h/x", "12345", "http://[::1/", "http://::1]/", "http://a@b@c/", "http://a.com\\@b.com/",
    "  http://h/x", "http://h/x y", "http://h/r/{{x}}", "https://h?q", "https://h#f?q",
    "//h/x", "mailto:x@y", "http:h", "http://[v1.fe]/", "http://[1.2.3.4]/", "http://[zz]/",
    "http://h\t/x", "http://h:080/", "http://xn--caf-dma.com/", "http://caf\u00e9.com/",
]


def safe_eval_from(web: Path, ref: str):
    text = subprocess.run(
        ["git", "-C", str(web), "show", f"{ref}:services/flow_executor_service.py"],
        check=True, capture_output=True, text=True,
    ).stdout
    tree = ast.parse(text)
    wanted = {"_safe_eval_math", "_redact_url"}
    found = {}
    for node in ast.walk(tree):
        if isinstance(node, ast.FunctionDef) and node.name in wanted:
            src = ast.get_source_segment(text, node)
            lines = src.splitlines()
            # dedent the method to module level and drop decorators/self
            body = "\n".join(line[4:] if line.startswith("    ") else line for line in lines)
            ns: dict = {}
            exec(body, ns)  # noqa: S102 -- the web's own committed function
            found[node.name] = ns[node.name]
    return found["_safe_eval_math"], found["_redact_url"]


def describe(value) -> dict:
    if isinstance(value, float):
        return {"float": repr(value)}
    return {"repr": repr(value)}


def math_vectors(safe_eval) -> list:
    out = []
    for expr in MATH:
        try:
            out.append({"expr": expr, "ok": repr(safe_eval(None, expr))})
        except Exception as e:  # noqa: BLE001 -- every failure is a vector
            out.append({"expr": expr, "err": str(e), "kind": type(e).__name__})
    return out


def json_vectors() -> list:
    out = []
    for text in JSON:
        try:
            out.append({"text": text, "ok": repr(json.loads(text))})
        except json.JSONDecodeError as e:
            out.append({"text": text, "err": str(e)})
    return out


def url_vectors(redact) -> list:
    from urllib.parse import urlsplit

    out = []
    for url in URLS:
        row: dict = {"url": url}
        try:
            parts = urlsplit(url)
            row["scheme"] = parts.scheme
            row["hostname"] = parts.hostname
            try:
                row["port"] = parts.port
            except ValueError as e:
                row["port_error"] = str(e)
            try:
                row["redacted"] = redact(url)
            except ValueError as e:
                row["redact_error"] = str(e)
        except ValueError as e:
            row["split_error"] = str(e)
        out.append(row)
    return out


def address_vectors() -> list:
    nets4 = ["0.0.0.0/8", "10.0.0.0/8", "100.64.0.0/10", "127.0.0.0/8", "169.254.0.0/16",
             "172.16.0.0/12", "192.0.0.0/24", "192.0.2.0/24", "192.88.99.0/24", "192.168.0.0/16",
             "198.18.0.0/15", "198.51.100.0/24", "203.0.113.0/24", "224.0.0.0/4", "240.0.0.0/4"]
    nets6 = ["::1/128", "::/128", "::ffff:0:0/96", "64:ff9b::/96", "64:ff9b:1::/48", "100::/64",
             "2001::/23", "2001:db8::/32", "2002::/16", "3fff::/20", "fc00::/7", "fe80::/10",
             "ff00::/8", "::/8", "100::/8", "200::/7", "400::/6", "800::/5", "1000::/4", "4000::/3",
             "6000::/3", "8000::/3", "a000::/3", "c000::/3", "e000::/4", "f000::/5", "f800::/6",
             "fe00::/9", "2001:1::1/128", "2001:1::2/128", "2001:3::/32", "2001:4:112::/48",
             "2001:20::/28", "2001:30::/28", "fec0::/10", "2000::/3"]
    addrs = {"255.255.255.255", "192.0.0.9", "192.0.0.10", "192.0.0.170", "192.0.0.171",
             "8.8.8.8", "1.1.1.1", "93.184.216.34", "100.63.255.255", "100.128.0.0", "11.0.0.0",
             "9.255.255.255", "172.15.255.255", "172.32.0.0", "192.167.255.255", "192.169.0.0",
             "198.17.255.255", "198.20.0.0", "223.255.255.255"}
    for n in nets4:
        net = ipaddress.ip_network(n)
        addrs.update({str(net.network_address), str(net.broadcast_address), str(net.network_address + 1)})
    for n in nets6:
        net = ipaddress.ip_network(n)
        addrs.update({str(net.network_address), str(net[-1]), str(net.network_address + 1)})
        if int(net.network_address) > 0:
            addrs.add(str(net.network_address - 1))
        if int(net[-1]) < 2**128 - 1:
            addrs.add(str(net[-1] + 1))
    for a in ["::ffff:127.0.0.1", "::ffff:8.8.8.8", "::ffff:10.0.0.1", "::ffff:100.64.0.1",
              "::ffff:224.0.0.1", "::ffff:169.254.169.254", "::127.0.0.1", "::8.8.8.8",
              "64:ff9b::808:808", "64:ff9b::7f00:1", "2606:4700:4700::1111", "2001:4860:4860::8888",
              "2404:6800::1", "2001:1::3", "2001:2::1", "2001:3::1", "2001:4:112::1", "2001:20::1",
              "2001:30::1", "2001:40::1", "fd00:ec2::254", "ff02::1", "::2"]:
        addrs.add(str(ipaddress.ip_address(a)))
    out = []
    for a in sorted(addrs, key=lambda s: (":" in s, s)):
        ip = ipaddress.ip_address(a)
        web = (ip.is_private or ip.is_loopback or ip.is_link_local or ip.is_reserved
               or ip.is_multicast or ip.is_unspecified)
        out.append({"address": str(ip), "web_refuses": web})
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--web", required=True, type=Path)
    ap.add_argument("--ref", required=True)
    ap.add_argument("--out", required=True, type=Path)
    a = ap.parse_args()
    if sys.version_info[:2] != (3, 12):
        print("run with the web's Python 3.12", file=sys.stderr)
        return 2
    commit = subprocess.run(["git", "-C", str(a.web), "rev-parse", "--short=9", a.ref],
                            check=True, capture_output=True, text=True).stdout.strip()
    safe_eval, redact = safe_eval_from(a.web, a.ref)
    data = {
        "web_commit": commit,
        "python": sys.version.split()[0],
        "math": math_vectors(safe_eval),
        "json": json_vectors(),
        "urls": url_vectors(redact),
        "addresses": address_vectors(),
    }
    a.out.mkdir(parents=True, exist_ok=True)
    (a.out / "logic_vectors.json").write_text(json.dumps(data, indent=1, ensure_ascii=False) + "\n")
    print(f"math {len(data['math'])}, json {len(data['json'])}, urls {len(data['urls'])}, "
          f"addresses {len(data['addresses'])}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
