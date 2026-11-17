"""Check that a broker's master contract holds every Nifty 500 stock under its OpenAlgo symbol.

OpenAlgo's equity symbol is the exchange's base symbol (docs/prompt/symbol-format.md),
which is exactly the Symbol column of NSE's index constituent list. A master
contract that stores anything else for a constituent leaves a trader who pastes
the list (Historify bulk add, a watchlist, a scanner universe) short by those
symbols, with nothing to say why. The case that prompted this check (web
openalgo#2463): Zerodha stored HFCL-BE and EMBASSY-RR, Kite's tradingsymbols for
stocks traded in the BE and RR series, so 495 of the Nifty 500 were found and
five were not.

Desktop port of the web's script of the same name. The web reads its own
db/openalgo.db by default; the desktop's master lives in the trader's data
folder, which no agent tool may open (CLAUDE.md, "Never run the app binary ...
against the real data folder"). So there is no default source: name one.

* --db FILE: the openalgo.db of a throwaway data folder, such as the one the
  dev_server example prints ("data in /var/folders/..."), after signing in to
  the broker there so its master contract downloads into symtoken. Opened
  read-only. A path inside the trader's real data folder is refused before
  anything is opened.
* --rows-csv FILE: a CSV with symbol, brsymbol and exchange columns, for
  checking an adapter's parsed rows before anything is written.

The constituent list is downloaded from NSE, or read from --nifty-csv. Rows NSE
marks as placeholders (an ISIN beginning DUM, such as DUMMYHEG) are reported
and skipped: they are not listed instruments.

Prints NIFTY 500 COMPLETE and exits 0 when every constituent is found exactly
once on NSE; otherwise lists each one that is not, with what the master
contract holds instead, and exits 1.

Usage (from the repository root):
    python3 .claude/skills/broker-integration/scripts/check_nifty500.py --db <dev data folder>/openalgo.db
    python3 .claude/skills/broker-integration/scripts/check_nifty500.py --rows-csv rows.csv
    python3 .claude/skills/broker-integration/scripts/check_nifty500.py --db ... --nifty-csv ind_nifty500list.csv
"""

from __future__ import annotations

import argparse
import csv
import io
import os
import sqlite3
import sys
import urllib.request
from collections import defaultdict
from pathlib import Path

NIFTY500_URL = "https://archives.nseindia.com/content/indices/ind_nifty500list.csv"
# NSE's archive refuses requests without a browser-like User-Agent.
USER_AGENT = (
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/126.0 Safari/537.36"
)
# tauri.conf.json "identifier": the app's data folder is named after it.
APP_IDENTIFIER = "com.openalgo.desktop"


def real_data_folders() -> list[Path]:
    """Where the installed app keeps its data (Tauri's app_data_dir), on every platform."""
    home = Path.home()
    folders = [
        home / "Library" / "Application Support" / APP_IDENTIFIER,
        Path(os.environ.get("XDG_DATA_HOME") or home / ".local" / "share") / APP_IDENTIFIER,
    ]
    for var in ("APPDATA", "LOCALAPPDATA"):
        if os.environ.get(var):
            folders.append(Path(os.environ[var]) / APP_IDENTIFIER)
    return folders


def _canonical(path: Path) -> str:
    return os.path.normcase(os.path.realpath(path))


def refuse_real_data_folder(db: Path) -> None:
    """Exit before anything is opened when `db` lies in the trader's real data folder."""
    target = _canonical(db)
    for folder in real_data_folders():
        root = _canonical(folder)
        if target == root or target.startswith(root + os.sep):
            sys.exit(
                f"{db} is in the trader's data folder ({folder}), which this check never opens. "
                "Sign in to the broker in the dev_server example and pass the openalgo.db of the "
                "data folder it prints, or pass --rows-csv."
            )


def load_constituents(nifty_csv: str | None) -> list[dict[str, str]]:
    """The constituent rows: Company Name, Industry, Symbol, Series, ISIN Code."""
    if nifty_csv:
        text = Path(nifty_csv).read_text(encoding="utf-8-sig")
    else:
        request = urllib.request.Request(
            NIFTY500_URL, headers={"User-Agent": USER_AGENT, "Accept": "text/csv,*/*"}
        )
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                text = response.read().decode("utf-8-sig")
        except OSError as exc:
            sys.exit(
                f"Could not download the Nifty 500 list from NSE ({exc}). Download "
                f"{NIFTY500_URL} in a browser and pass it with --nifty-csv."
            )
    rows = list(csv.DictReader(io.StringIO(text)))
    if not rows or "Symbol" not in rows[0]:
        sys.exit("The Nifty 500 file has no Symbol column. Is it ind_nifty500list.csv?")
    return rows


def load_rows_csv(rows_csv: str) -> list[tuple[str, str, str]]:
    with open(rows_csv, encoding="utf-8-sig", newline="") as handle:
        reader = csv.DictReader(handle)
        if {"symbol", "brsymbol", "exchange"} - set(reader.fieldnames or []):
            sys.exit(f"{rows_csv} needs the columns symbol, brsymbol and exchange.")
        return [(r["symbol"], r["brsymbol"], r["exchange"]) for r in reader]


def load_symtoken(db: Path) -> tuple[list[tuple[str, str, str]], str]:
    """The desktop's stored master (db::sqlite::symbol) and the broker that owns it."""
    refuse_real_data_folder(db)
    if not db.is_file():
        sys.exit(f"{db} does not exist. Pass the openalgo.db of a dev_server data folder.")
    conn = sqlite3.connect(f"{db.resolve().as_uri()}?mode=ro", uri=True)
    try:
        rows = conn.execute("SELECT symbol, brsymbol, exchange FROM symtoken").fetchall()
        try:
            owner = conn.execute(
                "SELECT broker, row_count, stored_at FROM symtoken_owner WHERE id = 1"
            ).fetchone()
        except sqlite3.Error:
            owner = None
    except sqlite3.Error as exc:
        sys.exit(f"{db} has no readable symbol master ({exc}). Has the broker's master downloaded?")
    finally:
        conn.close()
    label = f"{db} symtoken"
    if owner:
        label += f" (broker: {owner[0]}, {owner[1]} rows stored {owner[2]})"
    return rows, label


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--db", help="openalgo.db of a throwaway (dev_server) data folder")
    source.add_argument("--rows-csv", help="symbol,brsymbol,exchange rows to check instead")
    parser.add_argument("--nifty-csv", help="a saved ind_nifty500list.csv instead of downloading")
    args = parser.parse_args()

    if args.db:
        master, source_label = load_symtoken(Path(args.db))
    else:
        master, source_label = load_rows_csv(args.rows_csv), args.rows_csv
    constituents = load_constituents(args.nifty_csv)

    nse_count: dict[str, int] = defaultdict(int)
    by_brsymbol: dict[str, list[str]] = defaultdict(list)
    nse_rows = []
    other_exchanges: dict[str, set[str]] = defaultdict(set)
    for symbol, brsymbol, exchange in master:
        if exchange == "NSE":
            nse_count[symbol] += 1
            by_brsymbol[brsymbol].append(symbol)
            nse_rows.append((symbol, brsymbol))
        else:
            other_exchanges[symbol].add(exchange)

    placeholders = [r for r in constituents if r.get("ISIN Code", "").startswith("DUM")]
    listed = [r for r in constituents if r not in placeholders]

    missing, duplicated = [], []
    for row in listed:
        symbol = row["Symbol"].strip().upper()
        if nse_count.get(symbol, 0) == 0:
            missing.append(row)
        elif nse_count[symbol] > 1:
            duplicated.append(symbol)

    print(f"Master contract: {source_label}, {len(nse_rows)} NSE rows")
    print(
        f"Nifty 500 list: {len(constituents)} rows, {len(listed)} listed"
        + (
            f", {len(placeholders)} NSE placeholder(s) skipped: "
            + ", ".join(f"{r['Symbol']} ({r.get('Company Name', '')})" for r in placeholders)
            if placeholders
            else ""
        )
    )
    print(f"Found on NSE: {len(listed) - len(missing) - len(duplicated)} of {len(listed)}")

    for row in missing:
        symbol = row["Symbol"].strip().upper()
        prefix = symbol + "-"
        near = sorted(
            {
                f"symbol {s} (brsymbol {b})"
                for s, b in nse_rows
                if s.startswith(prefix) or b.startswith(prefix)
            }
            | {f"symbol {s} (brsymbol {symbol})" for s in by_brsymbol.get(symbol, [])}
        )
        print(
            f"\nNOT FOUND  {symbol}  (Series {row.get('Series', '?')}, {row.get('Company Name', '')})"
        )
        if near:
            print(f"  NSE holds: {'; '.join(near[:4])}")
            print(
                "  Fix the master contract: the OpenAlgo symbol is the base symbol "
                f"{symbol}; the broker's tradingsymbol belongs in brsymbol only."
            )
        elif symbol in other_exchanges:
            print(
                f"  Only on: {', '.join(sorted(other_exchanges[symbol]))}. Check the exchange mapping."
            )
        else:
            print(
                "  No NSE row carries this symbol. Check that the broker's NSE cash "
                "segment is loaded in full and that the symbol has not been renamed."
            )

    for symbol in duplicated:
        print(f"\nDUPLICATED  {symbol}: {nse_count[symbol]} NSE rows share this symbol.")

    if missing or duplicated:
        print(f"\nNIFTY 500 INCOMPLETE: {len(missing)} not found, {len(duplicated)} duplicated")
        return 1
    print("\nNIFTY 500 COMPLETE")
    return 0


if __name__ == "__main__":
    sys.exit(main())
