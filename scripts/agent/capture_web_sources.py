"""Record the web agent's "where to look for company figures" prompt section.

Renders the web's `services/agent/web_prompt.py` (`web_prompt_section`) for a
few preferred-source lists and writes
`src-tauri/tests/fixtures/agent/web_sources.json`, which the desktop's
`agent::websearch::prompt` tests reproduce byte for byte. The module is pure
text: nothing connects, reads `.env` or writes a file; bytecode is not written.

Usage, with the web's own environment on a private copy of the web tree (never
the read-only checkout), from a scratch directory: the web's logger creates
`log/errors.jsonl` in the working directory. The fixture is written beside
this script's repository whatever the working directory is.

    cd "$(mktemp -d)" && /path/to/openalgo/.venv/bin/python -I -B \
        /path/to/openalgo-desktop/scripts/agent/capture_web_sources.py /path/to/web-tree

Regenerate only from the web; never edit the fixture by hand.
"""

from __future__ import annotations

import sys

sys.dont_write_bytecode = True

import json  # noqa: E402 - after the bytecode switch
import subprocess  # noqa: E402
from pathlib import Path  # noqa: E402

#: The lists rendered: the shipped default, two sources, a list without the
#: one source whose pages are built from a symbol, and none at all.
CASES = {
    "default": ["screener.in", "scanx.trade", "tickertape.in"],
    "two": ["screener.in", "scanx.trade"],
    "without_screener": ["scanx.trade"],
    "none": [],
}


def main(web_root: str) -> None:
    root = Path(web_root).resolve()
    sys.path.insert(0, str(root))
    from services.agent.web_prompt import web_prompt_section

    sections = {}
    for name, sources in CASES.items():
        section = web_prompt_section(sources)
        sections[name] = {
            "sources": sources,
            "section": None
            if section is None
            else {
                "key": section.key,
                "title": section.title,
                "body": section.body,
                "pinned": section.pinned,
                "order": section.order,
            },
        }
    try:
        commit = subprocess.run(
            ["git", "-C", str(root), "rev-parse", "--short=8", "HEAD"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        commit = "unknown"
    fixture = {
        "_comment": (
            "Captured from the web's services/agent/web_prompt.py by "
            "scripts/agent/capture_web_sources.py. Regenerate only from the web."
        ),
        "web_commit": commit,
        "cases": sections,
    }
    repo = Path(__file__).resolve().parents[2]
    out = repo / "src-tauri/tests/fixtures/agent/web_sources.json"
    out.write_text(json.dumps(fixture, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"wrote {out}: {len(sections)} cases")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: capture_web_sources.py /path/to/web-tree")
    main(sys.argv[1])
