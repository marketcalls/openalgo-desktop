#!/usr/bin/env python3
"""Record the rows OpenAlgo web's Pocketful master contract builds, for the port.

The desktop parses Pocketful's exchange-wise contract in Rust
(``src-tauri/src/brokers/pocketful/master_contract.rs``). Every symtoken row
it builds from the fixture files must be the one the web builds: the same
symbol, broker symbol, name, exchanges, token, expiry, strike, lot size,
instrument type and tick size, in the same order. This script runs the web's
own ``process_pocketful_contract`` (``broker/pocketful/database/
master_contract_db.py``, marketcalls/openalgo#2467) over the fixture folder
and writes what it returns to ``master_rows.json`` there.

Run it against a read-only checkout. It reads that one file from the
checkout's committed ``--ref`` only (``git show``, so uncommitted work there
never reaches the golden), records the commit as ``web_commit``, and writes
nothing in the checkout:

    PYTHONDONTWRITEBYTECODE=1 python3 -I scripts/gen_pocketful_master_golden.py \\
        --web ../openalgo --ref 3f4c71e0f \\
        --fixtures src-tauri/tests/fixtures/brokers/pocketful

The processing functions need pandas and numpy only. The module's database,
Socket.IO, HTTP and logging imports are replaced by stand-ins before it is
loaded: nothing here opens a database or the network.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import logging
import subprocess
import sys
import tempfile
import types
from pathlib import Path

SOURCE = "broker/pocketful/database/master_contract_db.py"
COLUMNS = [
    "symbol",
    "brsymbol",
    "name",
    "exchange",
    "brexchange",
    "token",
    "expiry",
    "strike",
    "lotsize",
    "instrumenttype",
    "tick_size",
]


def _stub(name: str, **attrs) -> None:
    module = types.ModuleType(name)
    module.__dict__.update(attrs)
    sys.modules[name] = module


class _Column:
    def __init__(self, *args, **kwargs):
        pass


class _Session:
    def query_property(self):
        return None

    def remove(self):
        pass


def _declarative_base():
    class Base:
        metadata = types.SimpleNamespace(create_all=lambda **kwargs: None)

    return Base


def _install_stand_ins() -> None:
    """What the module imports besides pandas and numpy, inert."""
    _stub(
        "sqlalchemy",
        Column=_Column,
        Float=_Column,
        Index=_Column,
        Integer=_Column,
        Sequence=_Column,
        String=_Column,
    )
    _stub("sqlalchemy.ext")
    _stub("sqlalchemy.ext.declarative", declarative_base=_declarative_base)
    _stub("sqlalchemy.orm", scoped_session=lambda factory: _Session(), sessionmaker=lambda **k: None)
    _stub("database")
    _stub("database.engine_factory", create_db_engine=lambda url: None)
    _stub("extensions", socketio=None)
    _stub("utils")
    _stub("utils.httpx_client", get_httpx_client=None)
    _stub("utils.logging", get_logger=logging.getLogger)
    _stub("broker")
    _stub("broker.pocketful")
    _stub("broker.pocketful.api")
    _stub("broker.pocketful.api.baseurl", BASE_URL="https://algo.pocketful.in")


def _git(web: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(web), *args], check=True, capture_output=True, text=True
    ).stdout


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--web", type=Path, required=True, help="read-only web checkout")
    parser.add_argument("--ref", required=True, help="web commit to read the module from")
    parser.add_argument("--fixtures", type=Path, required=True, help="folder of the five files")
    args = parser.parse_args()

    commit = _git(args.web, "rev-parse", f"{args.ref}^{{commit}}").strip()
    source = _git(args.web, "show", f"{commit}:{SOURCE}")
    _install_stand_ins()
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "master_contract_db.py"
        path.write_text(source, encoding="utf-8")
        spec = importlib.util.spec_from_file_location("pocketful_master_contract_db", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        rows = module.process_pocketful_contract(str(args.fixtures))

    golden = {
        "web_commit": commit,
        "source": SOURCE,
        "rows": json.loads(rows[COLUMNS].to_json(orient="records")),
    }
    out = args.fixtures / "master_rows.json"
    out.write_text(json.dumps(golden, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"{len(golden['rows'])} rows from web {commit[:9]} -> {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
