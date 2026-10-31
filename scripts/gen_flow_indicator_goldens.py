#!/usr/bin/env python3
"""Record the golden values the Flow indicator service port is checked against.

The desktop's Flow nodes compute with a Rust port of the web's
``services/indicator_service.py`` (``src-tauri/src/flow/indicator_service``).
The answers must be the web's, so this script runs the web's own module, and
the web executor's own Indicator, Bar Offset and Prior Period OHLC nodes, over
fixed candles and writes what they return:

* ``compute.json``: ``compute_indicator`` over every indicator and candle shape
  of ``agent_indicators/candles.csv``, the lookback, tail and offset
  arithmetic, nested mode, parameter binding, and every refusal.
* ``params.json``: what a float does to each integer parameter.
* ``types.json``: every parameter given values of each Python type (text,
  null, lists, objects, fractions), with each mode of the methods whose
  other parameters change how one is read.
* ``window.json``: ``clamp_bars``, ``calendar_days_for_bars``,
  ``history_window``, ``clamp_history_range`` and ``trim_records``.
* ``bars.json``: ``bar_at_offset`` and ``prior_period_ohlc``.
* ``executor.json``: the web executor's ``execute_indicator``,
  ``execute_bar_offset`` and ``execute_prior_period_ohlc`` on node data, with
  their logs and the history window they asked for.
* ``catalogue.json``: ``list_supported_indicators`` and
  ``is_single_series_indicator``.
* ``editor.json``: the web editor's indicator categories, catalogue entries
  and parameter fields (``frontend/src/lib/flow/constants.ts``).

Candles come from ``src-tauri/tests/fixtures/agent_indicators/candles.csv``; a
case names them by a recipe (shape, row range, columns dropped, filled or
written as text, timestamps written as ISO text) that the Rust tests rebuild
the same way. Volumes are integers and missing cells null, as history returns
them.

Run it with the web's own interpreter or a scratch one with the openalgo
version the web pins (``uv.lock``); nothing is installed and nothing is written
to the checkout (``-B`` keeps bytecode out of it). The web's modules are copied
into a temporary folder and imported from there:

    <web>/.venv/bin/python -I -B scripts/gen_flow_indicator_goldens.py \\
        --web ../openalgo --out src-tauri/tests/fixtures/flow_indicators

Time is IST (the web's server-local time), and "now" and "today" are frozen
per case.
"""

from __future__ import annotations

import argparse
import ast
import csv
import json
import logging
import math
import os
import re
import shutil
import sys
import tempfile
import textwrap
import time
from datetime import date, datetime, timedelta, timezone
from pathlib import Path

os.environ["TZ"] = "Asia/Kolkata"
time.tzset()
# Every fetch goes to the stub client: no cache between cases.
os.environ["FLOW_HISTORY_CACHE_TTL"] = "0"

SHAPES = ("trend", "range", "gaps", "nan_head", "tiny")
OHLCV = ("open", "high", "low", "close", "volume")
TODAY = date(2026, 10, 30)

# ---------------------------------------------------------------------------
# The web's modules, copied into a scratch package
# ---------------------------------------------------------------------------

_EXECUTOR_METHODS = (
    "get_str",
    "get_int",
    "store_output",
    "_history_error",
    "execute_indicator",
    "execute_bar_offset",
    "execute_prior_period_ohlc",
)


def load_web(web: Path, scratch: Path):
    """Copy the web's indicator service and its two helpers into ``scratch``."""
    services = scratch / "services"
    services.mkdir()
    (services / "__init__.py").write_text("")
    shutil.copy(web / "services/indicator_service.py", services / "indicator_service.py")
    utils = scratch / "utils"
    utils.mkdir()
    (utils / "__init__.py").write_text("")
    shutil.copy(web / "utils/env_config.py", utils / "env_config.py")
    (utils / "logging.py").write_text(
        "import logging\n\ndef get_logger(name):\n    return logging.getLogger(name)\n"
    )
    sys.path.insert(0, str(scratch))
    from services import indicator_service

    return indicator_service


def executor_class(web: Path):
    """The web's ``NodeExecutor`` methods the three nodes use, on a stub."""
    tree = ast.parse((web / "services/flow_executor_service.py").read_text())
    node_executor = next(
        n for n in tree.body if isinstance(n, ast.ClassDef) and n.name == "NodeExecutor"
    )
    methods = [
        ast.unparse(n)
        for n in node_executor.body
        if isinstance(n, ast.FunctionDef) and n.name in _EXECUTOR_METHODS
    ]
    source = "class Executor:\n" + "\n\n".join(textwrap.indent(m, "    ") for m in methods)
    namespace = {"json": json, "datetime": datetime, "timedelta": timedelta}
    exec(compile(source, "flow_executor_service.NodeExecutor", "exec"), namespace)  # noqa: S102
    return namespace["Executor"]


class StubContext:
    """The variable store: whole ``{{name}}`` tokens resolve, text passes."""

    def __init__(self, variables: dict):
        self.variables = variables
        self.stored = {}

    def interpolate(self, text):
        return text

    def resolve_raw(self, text):
        if isinstance(text, str):
            stripped = text.strip()
            if stripped.startswith("{{") and stripped.endswith("}}"):
                key = stripped[2:-2].strip()
                if key in self.variables:
                    return self.variables[key]
        return text

    def set_variable(self, name, value):
        self.stored[name] = value


class StubClient:
    """Answers every history request with the case's records."""

    api_key = None

    def __init__(self, response):
        self.response = response
        self.requests = []

    def get_history(self, **kwargs):
        self.requests.append(kwargs)
        return self.response


def freeze(svc, now: datetime):
    """Make the service module's ``date.today()`` and ``datetime.now()`` fixed."""

    class FrozenDate(date):
        @classmethod
        def today(cls):
            return cls(now.year, now.month, now.day)

    class FrozenDatetime(datetime):
        @classmethod
        def now(cls, tz=None):
            return cls(now.year, now.month, now.day, now.hour, now.minute, now.second)

    svc.date = FrozenDate
    svc.datetime = FrozenDatetime


# ---------------------------------------------------------------------------
# Candles and recipes
# ---------------------------------------------------------------------------


def load_shapes(candles: Path) -> dict:
    shapes: dict = {}
    with candles.open() as handle:
        for row in csv.DictReader(handle):
            shape = shapes.setdefault(row["shape"], {"timestamp": [], **{c: [] for c in OHLCV}})
            shape["timestamp"].append(int(row["timestamp"]))
            for column in OHLCV:
                cell = row[column]
                shape[column].append(float(cell) if cell else None)
    return shapes


def iso_ist(epoch: int) -> str:
    return datetime.fromtimestamp(epoch).strftime("%Y-%m-%dT%H:%M:%S+05:30")


def records(shapes: dict, recipe: dict) -> list[dict]:
    """The history records a recipe names; the Rust tests build the same."""
    shape = shapes[recipe["shape"]]
    n = len(shape["timestamp"])
    start, end = recipe.get("start", 0), min(recipe.get("end", n), n)
    out = []
    for i in range(start, end):
        stamp = shape["timestamp"][i]
        record = {"timestamp": iso_ist(stamp) if recipe.get("iso") else stamp}
        for column in OHLCV:
            if column in recipe.get("drop", []):
                continue
            value = shape[column][i]
            if column in recipe.get("fill", {}):
                value = recipe["fill"][column]
            if column == "volume" and value is not None:
                value = int(value)
            if column in recipe.get("text", []) and value is not None:
                value = repr(value)
            record[column] = value
        out.append(record)
    return out


# ---------------------------------------------------------------------------
# JSON
# ---------------------------------------------------------------------------


def jsonable(value):
    """Floats JSON cannot carry are written as text the tests read back."""
    if isinstance(value, float) and not math.isfinite(value):
        return "nan" if math.isnan(value) else ("inf" if value > 0 else "-inf")
    if isinstance(value, dict):
        return {str(k): jsonable(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [jsonable(v) for v in value]
    if hasattr(value, "item"):
        return jsonable(value.item())
    return value


def outcome(call):
    """``{"result": ...}`` or ``{"error": {"kind", "message"}}``. An object's
    address in a message (`<... object at 0x10a4...>`) is masked, so a
    re-recording only changes what changed."""
    try:
        return {"result": jsonable(call())}
    except Exception as error:  # noqa: BLE001 - every exception is an answer
        message = re.sub(r" at 0x[0-9a-f]+", " at 0x...", str(error))
        return {"error": {"kind": type(error).__name__, "message": message}}


def pairs(params: dict) -> list:
    return [[k, v] for k, v in params.items()]


def write(out: Path, name: str, payload) -> None:
    text = json.dumps(jsonable(payload), indent=1, ensure_ascii=False)
    (out / name).write_text(text + "\n")
    print(f"{name}: {len(text) // 1024} KB")


# ---------------------------------------------------------------------------
# compute_indicator
# ---------------------------------------------------------------------------


class Compute:
    """Builds ``compute.json``: one case per call of ``compute_indicator``."""

    def __init__(self, svc, shapes):
        self.svc = svc
        self.shapes = shapes
        self.cases: list[dict] = []

    def candles(self, name, params=None, recipe=None, lookback=100, tail=5, offset=0, tag=""):
        recipe = recipe or {"shape": "trend"}
        rows = records(self.shapes, recipe)
        params = params or {}
        self.cases.append(
            {
                "tag": tag,
                "name": name,
                "params": pairs(params),
                "candles": recipe,
                "lookback_bars": lookback,
                "tail_bars": tail,
                "offset_bars": offset,
                **outcome(
                    lambda: self.svc.compute_indicator(
                        rows,
                        name,
                        params=dict(params),
                        lookback_bars=lookback,
                        tail_bars=tail,
                        offset_bars=offset,
                    )
                ),
            }
        )

    def stamped(self, label, stamps, name="sma", tag="timestamps"):
        """``name`` over 30 trend bars whose timestamps are ``stamps`` (the
        web's ``pd.to_datetime`` reads the column; ``MISSING`` drops the key)."""
        recipe = {"shape": "trend", "end": 30}
        rows = records(self.shapes, recipe)
        for row, stamp in zip(rows, stamps):
            if stamp == MISSING:
                row.pop("timestamp")
            else:
                row["timestamp"] = stamp
        self.cases.append(
            {
                "tag": tag,
                "label": label,
                "name": name,
                "params": [["period", 5]] if name == "sma" else [],
                "candles": recipe,
                "timestamps": stamps,
                "lookback_bars": 100,
                "tail_bars": 5,
                "offset_bars": 0,
                **outcome(
                    lambda: self.svc.compute_indicator(
                        rows, name, params={"period": 5} if name == "sma" else {}
                    )
                ),
            }
        )

    def nested(self, name, values, params=None, tail=5, offset=0, tag=""):
        params = params or {}
        self.cases.append(
            {
                "tag": tag,
                "name": name,
                "params": pairs(params),
                "nested": values,
                "tail_bars": tail,
                "offset_bars": offset,
                **outcome(
                    lambda: self.svc.compute_indicator(
                        None,
                        name,
                        params=dict(params),
                        tail_bars=tail,
                        source_series=list(values),
                        offset_bars=offset,
                    )
                ),
            }
        )


def compute_cases(svc, ta, shapes) -> list[dict]:
    c = Compute(svc, shapes)
    names = svc.list_supported_indicators()
    refused = sorted(svc.TWO_SERIES_INDICATORS) + sorted(svc.UNSUPPORTED_INDICATORS)

    # Every indicator with its defaults, on every shape.
    for shape in SHAPES:
        for name in names + refused:
            c.candles(name, recipe={"shape": shape}, tag="sweep")

    # Names as a trader may type them, and names that are not indicators.
    for name in (" RSI ", "Supertrend", "MACD\t", "nosuch", " NoSuch ", "", "TechnicalAnalysis",
                 " CrossOver ", "VALUEWHEN", " Vi ", "Median_Bands"):
        c.candles(name, tag="names")
    # Python attributes that are not indicators: the web reaches them through
    # getattr; the desktop answers them as unknown (a recorded difference).
    for name in ("__init__", "__class__", "_sma", "__dict__", " __Init__ "):
        c.candles(name, tag="dunder")

    # The name's refusals come before anything about the candles is read.
    for name in ("crossover", " VI ", "nosuch", "__init__"):
        c.candles(name, recipe={"shape": "trend", "end": 0}, tag="order")
        c.candles(name, recipe={"shape": "trend", "drop": ["high", "low", "close"]}, tag="order")
        c.stamped(f"order {name.strip()}", ["yesterday"] * 30, name=name, tag="order")

    # Names the Agent's tools accept as aliases, and re-cased keys, are
    # unknown keywords to the web's methods.
    for name, params in (
        ("sma", {"length": 14}),
        ("ema", {"length": 9}),
        ("macd", {"fast": 12}),
        ("macd", {"slow": 26}),
        ("macd", {"signal": 9}),
        ("bbands", {"std": 2}),
        ("bbands", {"stdev": 2}),
        ("bbands", {"stddev": 2}),
        ("bbands", {"deviation": 2}),
        ("rsi", {"Period": 14}),
        ("sma", {"PERIOD": 5}),
        ("supertrend", {"Multiplier": 3}),
    ):
        c.candles(name, params, tag="aliases")

    # Lookback, tail and offset arithmetic.
    for name in ("sma", "rsi", "macd", "supertrend", "ichimoku", "bbands", "rising", "fractals"):
        for lookback in (1, 20, 150, 500, 0, -5):
            c.candles(name, lookback=lookback, tag="lookback")
        for tail in (1, 3, 12, 0, -2, 250):
            c.candles(name, tail=tail, tag="tail")
        for offset in (1, 7, 160, 250, -3):
            c.candles(name, offset=offset, tag="offset")
    c.candles("sma", recipe={"shape": "trend", "end": 30}, lookback=100, tail=40, tag="short")
    c.candles("ema", recipe={"shape": "trend", "end": 60}, lookback=5, tail=8, offset=3, tag="short")

    # Parameters: explicit values, the library's own refusals, defaults.
    explicit = [
        ("sma", {"period": 9}),
        ("sma", {"period": 50}),
        ("sma", {"period": 0}),
        ("sma", {"period": -3}),
        ("sma", {"period": 151}),
        ("sma", {"period": True}),
        ("ema", {"period": 21}),
        ("rsi", {"period": 2}),
        ("rsi", {"period": 14.0}),
        ("roc", {"length": 5}),
        ("rising", {"length": 3}),
        ("falling", {"length": 2}),
        ("change", {"length": 400}),
        ("change", {"length": 3}),
        ("macd", {"fast_period": 5, "slow_period": 35, "signal_period": 5}),
        ("macd", {"slow_period": 26, "fast_period": 30}),
        ("supertrend", {"period": 7, "multiplier": 2}),
        ("supertrend", {"multiplier": 2.5}),
        ("bbands", {"period": 10, "std_dev": 1}),
        ("bbands", {"std_dev": 1.5}),
        ("apo", {"ma_type": "EMA"}),
        ("po", {"ma_type": "EMA"}),
        ("po", {"ma_type": "zzz"}),
        ("ma_envelopes", {"ma_type": "EMA", "percentage": 1}),
        ("obv_smoothed", {"ma_type": "SMA + Bollinger Bands"}),
        ("obv_smoothed", {"ma_type": "EMA"}),
        ("vwap", {"source": "close"}),
        ("vwap", {"source": "hl2", "anchor": "Week"}),
        ("variance", {"mode": "LR"}),
        ("variance", {"return_components": True}),
        ("variance", {"return_components": 1}),
        ("dpo", {"is_centered": True}),
        ("dpo", {"is_centered": "yes"}),
        ("volosc", {"check_volume_validity": False}),
        ("volosc", {"check_volume_validity": 0}),
        ("psar", {"acceleration": 1}),
        ("fractals", {"periods": 3}),
        ("kama", {"fast_length": 40}),
        ("ichimoku", {"displacement": 10}),
        ("hv", {"length": 20, "annual": 252}),
        ("emv", {"divisor": 100000}),
        ("lrslope", {"period": 50, "interval": 2}),
        ("pvi_with_signal", {"signal_length": 50, "signal_type": "SMA"}),
    ]
    for name, params in explicit:
        c.candles(name, params, tag="params")

    # Python's argument binding: unknown names and series names, in order.
    for name in names:
        c.candles(name, {"zzz_bogus": 1}, tag="binding")
    for name, params in (
        ("sma", {"data": 5}),
        ("atr", {"high": 1}),
        ("vwma", {"volume": 1, "period": 3}),
        ("sma", {"bogus_one": 1, "bogus_two": 2}),
        ("sma", {"period": 5.5, "bogus": 1}),
        ("macd", {"fast_period": 3, "close": 1}),
    ):
        c.candles(name, params, tag="binding")

    # A float for each integer parameter, whole and fractional; an int for
    # each float parameter.
    for name in names:
        for spec in signature_params(svc, ta, name):
            pname, default = spec
            if isinstance(default, bool):
                continue
            if isinstance(default, int):
                c.candles(name, {pname: float(default)}, tag="float_for_int")
                c.candles(name, {pname: default + 0.5}, tag="float_for_int")
            elif isinstance(default, float) and default > 0 and default == int(default):
                c.candles(name, {pname: int(default)}, tag="int_for_float")

    # Values of the wrong type: the web stops the run (a deliberate deviation
    # in wording on the desktop; the tests check the run stops).
    for name, params in (
        ("sma", {"period": None}),
        ("sma", {"period": "14"}),
        ("rsi", {"period": "14"}),
        ("adxr", {"period": "14"}),
        ("macd", {"fast_period": [12]}),
        ("bbands", {"std_dev": "2"}),
        ("apo", {"ma_type": 1}),
        ("po", {"ma_type": None}),
    ):
        c.candles(name, params, tag="mistyped")

    # Records as history may carry them.
    c.candles("obv", recipe={"shape": "trend", "drop": ["volume"]}, tag="records")
    c.candles("atr", recipe={"shape": "trend", "drop": ["high", "low"]}, tag="records")
    c.candles("sma", recipe={"shape": "trend", "drop": ["open", "high", "low", "volume"]}, tag="records")
    c.candles("sma", recipe={"shape": "trend", "end": 0}, tag="records")
    c.candles("rsi", recipe={"shape": "trend", "text": ["close"]}, tag="records")
    c.candles("atr", recipe={"shape": "trend", "iso": True}, tag="records")
    c.candles("vwap", recipe={"shape": "trend", "fill": {"volume": 0}}, tag="records")
    c.candles("obv", recipe={"shape": "trend", "fill": {"volume": None}}, tag="records")
    c.candles("rsi", recipe={"shape": "trend", "fill": {"close": "n/a"}}, tag="records")
    c.candles("adx", recipe={"shape": "gaps", "start": 150}, lookback=30, tag="records")

    # The timestamp column as pandas reads it (IND-045).
    for label, stamps in timestamp_columns(shapes).items():
        c.stamped(label, stamps)

    # Nested mode: an indicator over another indicator's series.
    trend = shapes["trend"]["close"]
    rsi = [round(v, 6) for v in ta.rsi(trend, 14)[-150:]]
    closes = trend[-150:]
    for name in names:
        if svc.is_single_series_indicator(name):
            c.nested(name, closes, tag="nested_sweep")
    for name in ("sma", "ema", "wma", "stdev", "highest", "lowest", "macd", "bbands", "rising"):
        c.nested(name, rsi, {"period": 9} if name in ("sma", "ema", "wma", "stdev") else {}, tag="nested")
    c.nested("sma", rsi[-5:], {"period": 3}, tag="nested")
    c.nested("sma", rsi[-5:], tag="nested")
    c.nested("sma", [50.0, 51.0], {"period": 2}, tag="nested")
    c.nested("sma", [50.0], {"period": 1}, tag="nested")
    c.nested("sma", [], tag="nested")
    c.nested("ema", [float("nan"), 50.0, 51.0, 52.5, 49.0], {"period": 2}, tag="nested")
    c.nested("ema", rsi, tail=12, offset=4, tag="nested")
    # A nested series is used whole: no warm-up trim, no 200-bar clamp.
    long_series = [round(100 + 10 * math.sin(i / 7) + i / 50, 4) for i in range(450)]
    c.nested("sma", trend, {"period": 5}, tail=3, tag="nested_long")
    c.nested("ema", long_series, {"period": 20}, tail=3, offset=400, tag="nested_long")
    c.nested("rsi", long_series, tail=3, tag="nested_long")
    # Values with more than six decimals, read back one by one (sma of
    # period 1): Python's round() to 6 places, which a naive
    # floor(x * 1e6 + 0.5) / 1e6 or no rounding at all would not give.
    c.nested("sma", ROUNDING_VALUES, {"period": 1}, tail=len(ROUNDING_VALUES), tag="rounding")
    for name in ("atr", "hv", "vwma", "obv", "adx", "crossover", "vi", "nosuch"):
        c.nested(name, rsi, tag="nested_refused")
    return c.cases


MISSING = {"missing": True}

#: Values whose 6-place rounding is not what a naive rounding gives, plus
#: ordinary ones with more than six decimals.
ROUNDING_VALUES = [
    1.0000005, 0.1234565, 1.2345675, 2.0000025, 1000.0000015, 0.0000005,
    1.6666668, 1.66666649, 3.14159265358979, 123456.7654325, -0.0000015,
    -2.5000005, 0.9999995, 42.4242424242,
]


def timestamp_columns(shapes) -> dict:
    """Timestamp columns of every shape a history source could send."""
    epochs = shapes["trend"]["timestamp"][:30]
    iso = [iso_ist(t) for t in epochs]
    return {
        "ints": list(epochs),
        "floats": [float(t) for t in epochs],
        "null_first": [None] + epochs[1:],
        "missing_first": [MISSING] + epochs[1:],
        "bools": [True] * 30,
        "iso_offset": iso,
        "iso_z": [datetime.fromtimestamp(t, tz=timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ") for t in epochs],
        "iso_space": [t.replace("T", " ").replace("+05:30", "") for t in iso],
        "iso_minutes": [t[:16] for t in iso],
        "iso_fraction": [t.replace("+05:30", ".250000+05:30") for t in iso],
        "date_only": [t[:10] for t in iso],
        "slash_dates": [t[:10].replace("-", "/") for t in iso],
        "empty_first": [""] + iso[1:],
        "epoch_text": [str(t) for t in epochs],
        "bad_first": ["yesterday"] + iso[1:],
        "bad_later": iso[:5] + ["yesterday"] + iso[6:],
        "date_then_datetime": [iso[0][:10]] + iso[1:],
        "datetime_then_date": iso[:-1] + [iso[-1][:10]],
        "mixed_offsets": [iso[0].replace("+05:30", "+00:00")] + iso[1:],
        "int_then_iso": [epochs[0]] + iso[1:],
        "list_first": [[1]] + epochs[1:],
        "huge_first": [10**19] + epochs[1:],
    }


def signature_params(svc, ta, name):
    """``(name, default)`` of each scalar parameter, required ones at their fill."""
    import inspect

    out = []
    for p in inspect.signature(getattr(ta, name)).parameters.values():
        if p.name in svc._SERIES_PARAM_TO_COLUMN:
            continue
        default = p.default
        if default is inspect.Parameter.empty:
            default = svc.REQUIRED_PARAM_DEFAULTS[name][p.name]
        out.append((p.name, default))
    return out


#: Methods whose parameters are read differently by another parameter's
#: value: each mode is recorded as well as the defaults.
MODES = {
    "obv_smoothed": [{"ma_type": m} for m in (
        "SMA", "SMA + Bollinger Bands", "EMA", "SMMA (RMA)", "WMA", "VWMA", "sma", "zzz")],
    "variance": [{"return_components": True}, {"mode": "LR"}, {"return_components": "yes"}],
    "pvi_with_signal": [{"signal_type": t} for t in ("SMA", "sma", "EMA", "ema", "zzz")],
}


def probes(default):
    """Values of every Python type a node's params JSON can carry, around
    the parameter's default."""
    if isinstance(default, bool):
        return [0, 1, 0.0, "", "False", "yes", None, [], [1], {}]
    if isinstance(default, int):
        return [default + 0.5, float(default), str(default), f" {default} ", f"{default}.5",
                f"+{default}", "1_0", "abc", "", None, [default], {"v": default}, True]
    if isinstance(default, float):
        return [str(default), str(int(default)), " 1e1 ", "abc", "", None, [default],
                [default, default], [], ["2", "x"], {"v": default}, True, int(default) or 1]
    return [1, 1.5, None, True, [default], {"v": default}, default.lower(), default.upper(),
            f" {default} ", ""]


def type_cases(svc, ta, shapes) -> list[dict]:
    """``compute_indicator`` with every parameter set to values of each type:
    the latest row, or the exception, as the web answers."""
    rows = records(shapes, {"shape": "trend"})

    def answer(name, params):
        try:
            out = svc.compute_indicator(rows, name, dict(params))
            return {"result": jsonable({k: out.get(k) for k in ("status", "latest", "message")})}
        except Exception as error:  # noqa: BLE001 - every exception is an answer
            return {"error": {"kind": type(error).__name__, "message": str(error)}}

    cases = []
    for name in svc.list_supported_indicators():
        for mode in [{}] + MODES.get(name, []):
            for pname, default in signature_params(svc, ta, name):
                if pname in mode:
                    continue
                for value in probes(default):
                    params = [*mode.items(), (pname, value)]
                    cases.append({"name": name, "params": [[k, v] for k, v in params],
                                  **answer(name, dict(params))})
    return cases


def float_params(svc, ta, shapes) -> list[dict]:
    """What a fractional float does to each integer parameter."""
    rows = records(shapes, {"shape": "trend"})
    out = []
    for name in svc.list_supported_indicators():
        for pname, default in signature_params(svc, ta, name):
            if isinstance(default, bool) or not isinstance(default, int):
                continue
            answer = outcome(lambda: svc.compute_indicator(rows, name, {pname: default + 0.5}))
            refusal = answer.get("error") if "error" in answer else None
            out.append({"name": name, "param": pname, "refusal": refusal})
    return out


# ---------------------------------------------------------------------------
# Window, bars, catalogue
# ---------------------------------------------------------------------------

INTERVALS = (
    "1m", "3m", "5m", "10m", "15m", "30m", "1h", "60m", "2h", "3h", "4h",
    "D", "d", "1d", "day", "W", "w", "week", "M", "m", "month",
    "Q", "q", "quarter", "Y", "y", "year", "2m", "weird", " D", "1H",
)


def window_cases(svc) -> dict:
    freeze(svc, datetime(2026, 10, 30, 11, 0))
    clamp = [{"requested": v, "result": svc.clamp_bars(v)} for v in (-5, 0, 1, 37, 199, 200, 201, 10**6)]
    calendar = [
        {"bars": bars, "interval": interval, "result": svc.calendar_days_for_bars(bars, interval)}
        for interval in INTERVALS
        for bars in (1, 6, 37, 100, 200, 250)
    ]
    window = []
    for interval in INTERVALS:
        for bars in (1, 5, 100, 200, 500, 0, -4):
            window.append(
                {"lookback_bars": bars, "lookback_days": None, "interval": interval,
                 "result": list(svc.history_window(lookback_bars=bars, interval=interval))}
            )
    for days in (5, 10, 21, 60, 0, -3, 4000, 5000, 10000):
        window.append(
            {"lookback_bars": 100, "lookback_days": days, "interval": "D",
             "result": list(svc.history_window(lookback_days=days, interval="D"))}
        )
    for end in ("2024-02-29", "2026-01-01"):
        window.append(
            {"lookback_bars": 200, "lookback_days": None, "interval": "W", "end": end,
             "result": list(svc.history_window(lookback_bars=200, interval="W", end_date=end))}
        )
    ranges = []
    for start, end, interval in (
        ("2016-01-01", "2026-10-30", "1m"),
        ("2016-01-01", "2026-10-30", "D"),
        ("2026-10-01", "2026-10-30", "5m"),
        ("2026-10-29", "2026-10-30", "1m"),
        ("2020-01-01 09:15:00", "2026-10-30 15:30:00", "15m"),
        ("20200101", "20261030", "1h"),
        ("not a date", "2026-10-30", "1m"),
        ("2026-10-01", "", "1m"),
        ("2000-01-01", "2026-10-30", "W"),
        ("2000-01-01", "2026-10-30", "Y"),
        ("20261001", "20261030", "D"),
        ("2026-10-01T09:15:00+05:30", "2026-10-30T15:30:00+05:30", "D"),
        ("2026-10-01 garbage after", "2026-10-30 end", "D"),
    ):
        ranges.append(
            {"start": start, "end": end, "interval": interval,
             "result": list(svc.clamp_history_range(start, end, interval))}
        )
    trim = [
        {"count": n, "kept": len(svc.trim_records([{"i": i} for i in range(n)])),
         "first": (svc.trim_records([{"i": i} for i in range(n)]) or [{"i": None}])[0]["i"]}
        for n in (0, 1, 199, 200, 201, 900)
    ]
    logged = logged_cases(svc)
    return {"logged": logged, "clamp_bars": clamp, "calendar_days": calendar, "history_window": window,
            "clamp_history_range": ranges, "trim_records": trim, "today": TODAY.isoformat()}


class Lines(logging.Handler):
    """Every record the service logs, as (level, message)."""

    def __init__(self):
        super().__init__(level=logging.DEBUG)
        self.lines = []

    def emit(self, record):
        self.lines.append([record.levelname, record.getMessage()])


def logged(svc, call):
    """The log lines one call writes."""
    handler = Lines()
    log = logging.getLogger(svc.__name__)
    log.setLevel(logging.DEBUG)
    log.addHandler(handler)
    try:
        outcome(call)
    finally:
        log.removeHandler(handler)
    return handler.lines


def logged_cases(svc) -> list[dict]:
    """The server log lines the window helpers write (decision 35)."""
    rows = lambda n: [{"i": i} for i in range(n)]
    calls = [
        ("clamp_bars", [500, "bars"], lambda: svc.clamp_bars(500)),
        ("clamp_bars", [201, "offset bars"], lambda: svc.clamp_bars(201, "offset bars")),
        ("clamp_bars", [200, "bars"], lambda: svc.clamp_bars(200)),
        ("trim_records", [201], lambda: svc.trim_records(rows(201))),
        ("trim_records", [200], lambda: svc.trim_records(rows(200))),
        ("history_window", [500, None, "1m"], lambda: svc.history_window(lookback_bars=500, interval="1m")),
        ("history_window", [200, None, "Y"], lambda: svc.history_window(lookback_bars=200, interval="Y")),
        ("history_window", [100, 5000, "D"], lambda: svc.history_window(lookback_days=5000, interval="D")),
        ("clamp_history_range", ["2016-01-01", "2026-10-30", "1m"], lambda: svc.clamp_history_range("2016-01-01", "2026-10-30", "1m")),
        ("clamp_history_range", ["2026-10-01", "2026-10-30", "D"], lambda: svc.clamp_history_range("2026-10-01", "2026-10-30", "D")),
    ]
    return [{"call": name, "args": args, "lines": logged(svc, call)} for name, args, call in calls]


def bars_cases(svc, shapes) -> dict:
    offsets = []
    daily = {"shape": "trend"}
    intraday = {"shape": "gaps"}
    last_daily = datetime.fromtimestamp(shapes["trend"]["timestamp"][-1])
    last_intraday = datetime.fromtimestamp(shapes["gaps"]["timestamp"][-1])
    nows = {
        "daily_same_day": (daily, last_daily.replace(hour=15)),
        "daily_next_day": (daily, last_daily + timedelta(days=1)),
        "daily_next_month": (daily, last_daily + timedelta(days=31)),
        "intraday_forming": (intraday, last_intraday + timedelta(minutes=2)),
        "intraday_closed": (intraday, last_intraday + timedelta(minutes=5)),
        "intraday_later": (intraday, last_intraday + timedelta(hours=3)),
        "intraday_before": (intraday, last_intraday - timedelta(minutes=10)),
    }
    for label, (recipe, now) in nows.items():
        rows = records(shapes, recipe)
        for interval in ("D", "day", "W", "week", "M", "month", " 5M ", "5m", "15m", "1h", "weird"):
            for offset in (0, 1, 5, 199, 250, -1):
                offsets.append(
                    {"label": label, "candles": recipe, "now": now.isoformat(), "interval": interval,
                     "offset": offset,
                     **outcome(lambda: svc.bar_at_offset(rows, offset, interval=interval, now=now))}
                )
    for recipe in ({"shape": "trend", "iso": True}, {"shape": "trend", "end": 0}):
        rows = records(shapes, recipe)
        offsets.append(
            {"label": "records", "candles": recipe, "now": nows["daily_next_day"][1].isoformat(),
             "interval": "D", "offset": 0,
             **outcome(lambda: svc.bar_at_offset(rows, 0, interval="D", now=nows["daily_next_day"][1]))}
        )
    # Every interval key at the edge of its forming window.
    gaps = records(shapes, intraday)
    minutes = {"1m": 1, "3m": 3, "5m": 5, "10m": 10, "15m": 15, "30m": 30, "1h": 60,
                "60m": 60, "2h": 120, "3h": 180, "4h": 240}
    for key, length in minutes.items():
        for spelled in (key, f" {key.upper()} "):
            for label, now in (("forming", last_intraday + timedelta(minutes=length - 1)),
                               ("closed", last_intraday + timedelta(minutes=length))):
                offsets.append(
                    {"label": f"edge_{label}", "candles": intraday, "now": now.isoformat(),
                     "interval": spelled, "offset": 0,
                     **outcome(lambda: svc.bar_at_offset(gaps, 0, interval=spelled, now=now))}
                )
    daily_rows = records(shapes, daily)
    for spelled in ("d", "D", "1d", "1D", "day", "DAY", " d ", "q", "y", "2m", "25m", "1s", ""):
        for label, now in (("same_day", last_daily.replace(hour=15)),
                           ("next_day", last_daily + timedelta(days=1)),
                           ("next_week", last_daily + timedelta(days=7))):
            offsets.append(
                {"label": f"keys_{label}", "candles": daily, "now": now.isoformat(),
                 "interval": spelled, "offset": 0,
                 **outcome(lambda: svc.bar_at_offset(daily_rows, 0, interval=spelled, now=now))}
            )
    bad = [{"timestamp": "yesterday", "close": 1}]
    offsets.append(
        {"label": "bad_timestamp", "records": bad, "now": "2026-10-30T11:00:00", "interval": "D",
         "offset": 0, **outcome(lambda: svc.bar_at_offset(bad, 0, interval="D", now=datetime(2026, 10, 30, 11)))}
    )

    prior = []
    first_daily = datetime.fromtimestamp(shapes["trend"]["timestamp"][0])
    daily_nows = [
        last_daily + timedelta(days=1),
        last_daily.replace(hour=10),
        last_daily + timedelta(days=9),
        last_daily + timedelta(days=40),
        first_daily.replace(hour=9),
        datetime(2025, 3, 3, 10),
        datetime(2025, 6, 1, 10),
    ]
    intraday_nows = [
        last_intraday + timedelta(minutes=1),
        last_intraday.replace(minute=0) + timedelta(hours=1, minutes=5),
        last_intraday + timedelta(days=1),
        datetime.fromtimestamp(shapes["gaps"]["timestamp"][0]),
    ]
    periods = ("previous_hour", "previous_day", "previous_week", "previous_month", "previous_year")
    for recipe, nows_for in (
        ({"shape": "trend"}, daily_nows),
        ({"shape": "trend", "iso": True}, daily_nows[:3]),
        ({"shape": "trend", "drop": ["volume"]}, daily_nows[:1]),
        ({"shape": "gaps"}, intraday_nows),
        ({"shape": "nan_head", "end": 12}, [datetime(2025, 2, 17, 10)]),
        ({"shape": "trend", "end": 0}, daily_nows[:1]),
    ):
        rows = records(shapes, recipe)
        for now in nows_for:
            freeze(svc, now)
            for period in periods:
                prior.append(
                    {"candles": recipe, "now": now.isoformat(), "period": period,
                     **outcome(lambda: svc.prior_period_ohlc(rows, period))}
                )
    stamps = [
        1771905600, 1771905600.75, "1771905600", " 1771905600.5 ", "2026-02-24T03:45:00Z",
        "2026-02-24T03:45:00+00:00", "2026-02-23T23:15:00-04:30", "2026-02-24T09:15:00+0530",
        "2026-02-24T09:15:00.123456+05:30", "2026-02-24 09:15:00", "2026-02-24T09:15",
        "2026-02-24T09", "2026-02-24", "20260224", "20260224T091500", "2026-02-24 09:15:00Z",
        "yesterday", "", None, True, "2026-13-01",
    ]
    parsed = [
        {"value": v, **outcome(lambda: svc._parse_timestamp(v).isoformat())} for v in stamps
    ]
    return {"bar_at_offset": offsets, "prior_period_ohlc": prior, "parse_timestamp": parsed}


def catalogue_cases(svc, ta) -> dict:
    names = [n for n in dir(ta) if not n.startswith("_")]
    probes = names + [" RSI ", "Vwma", "nosuch", ""]
    return {
        "supported": svc.list_supported_indicators(),
        "single_series": {n: svc.is_single_series_indicator(n) for n in probes},
        "two_series": sorted(svc.TWO_SERIES_INDICATORS),
        "unsupported": sorted(svc.UNSUPPORTED_INDICATORS),
        "required_defaults": svc.REQUIRED_PARAM_DEFAULTS,
    }


# ---------------------------------------------------------------------------
# The web executor's nodes
# ---------------------------------------------------------------------------


def run_node(svc, Executor, method, node_data, variables=None, history=None, now=None):
    """One node run on the stub executor: its return, logs, stored output and
    the history window it asked for."""
    freeze(svc, now or datetime(2026, 10, 30, 11, 0))
    executor = Executor()
    executor.context = StubContext(variables or {})
    executor.client = StubClient(history if history is not None else {"status": "success", "data": []})
    logs = []
    executor.log = lambda message, level="info": logs.append([level, message])
    answer = outcome(lambda: getattr(executor, method)(node_data))
    return {
        **answer,
        "logs": logs,
        "history_requests": executor.client.requests,
        "stored": jsonable(executor.context.stored),
    }


def executor_cases(svc, Executor, shapes) -> dict:
    trend = records(shapes, {"shape": "trend"})
    gaps = records(shapes, {"shape": "gaps"})
    ok = {"status": "success", "data": trend}
    rsi_rows = svc.compute_indicator(trend, "rsi", {"period": 14}, tail_bars=60)["series"]
    macd_rows = svc.compute_indicator(trend, "macd", {}, tail_bars=60)["series"]
    variables = {
        "rsi.series": rsi_rows,
        "macd.series": macd_rows,
        "h.data": trend[-60:],
        "numbers": [float(r["close"]) for r in trend[-40:]],
        "texts": [str(r["close"]) for r in trend[-40:]],
        "holes": [None, {"value": None}, {"value": 50.5}, {"out0": 49.0}, 51.25, {"close": "52"}],
        "bad": [1.0, 2.0, "abc"],
        "nested_list": [1.0, [2.0]],
        "flags": [True, False, 1, 0.5],
        "scalar": 42,
        "dict": {"value": 1},
    }
    cases = []

    responses = {
        "trend": ok,
        "gaps": {"status": "success", "data": gaps},
        "empty": {"status": "success", "data": []},
    }

    def node(method, data, label, history="trend", now=None):
        """``history`` names the response: a recipe label above, or an error dict."""
        response = responses[history] if isinstance(history, str) else history
        cases.append(
            {"label": label, "method": method, "node_data": data, "history": history,
             "now": (now or datetime(2026, 10, 30, 11, 0)).isoformat(),
             **run_node(svc, Executor, method, data, variables, response, now)}
        )

    base = {"symbol": "SBIN", "exchange": "NSE", "interval": "D", "outputVariable": "ind"}
    for label, extra in (
        ("defaults", {}),
        ("rsi_string_params", {"indicatorName": "rsi", "params": '{"period": 9}'}),
        ("empty_params", {"indicatorName": "sma", "params": ""}),
        ("blank_params", {"indicatorName": "sma", "params": "   "}),
        ("object_params", {"indicatorName": "ema", "params": {"period": 5}}),
        ("bad_json", {"indicatorName": "sma", "params": "{period: 5}"}),
        ("list_params", {"indicatorName": "sma", "params": "[5]"}),
        ("number_params", {"indicatorName": "sma", "params": "5"}),
        ("tail_offset", {"indicatorName": "supertrend", "params": "{}", "tailBars": 10, "offsetBars": 3}),
        ("lookback_text", {"indicatorName": "macd", "lookbackBars": "40", "tailBars": "3"}),
        ("big_lookback", {"indicatorName": "atr", "lookbackBars": 500}),
        ("two_series", {"indicatorName": "crossover"}),
        ("unknown", {"indicatorName": "Nope"}),
        ("library_value_error", {"indicatorName": "sma", "params": '{"period": 0}'}),
        ("library_type_error", {"indicatorName": "sma", "params": '{"bogus": 1}'}),
        ("no_symbol", {"symbol": "", "indicatorName": "rsi"}),
        # The params text as json.loads reads it: NaN and Infinity are
        # floats, keys keep their written order, a repeated key its place.
        ("nan_period", {"indicatorName": "sma", "params": '{"period": NaN}'}),
        ("quoted_nan_period", {"indicatorName": "sma", "params": '{"period": "NaN"}'}),
        ("nan_length", {"indicatorName": "roc", "params": '{"length": NaN}'}),
        ("overflow_period", {"indicatorName": "rsi", "params": '{"period": 1e999}'}),
        ("nan_std_dev", {"indicatorName": "bbands", "params": '{"std_dev": NaN}'}),
        ("infinity_std_dev", {"indicatorName": "bbands", "params": '{"std_dev": Infinity}'}),
        ("minus_infinity_multiplier", {"indicatorName": "supertrend", "params": '{"multiplier": -Infinity}'}),
        ("nan_ma_type", {"indicatorName": "obv_smoothed", "params": '{"ma_type": NaN}'}),
        ("nan_signal_type", {"indicatorName": "pvi_with_signal", "params": '{"signal_type": NaN}'}),
        ("nan_flag", {"indicatorName": "dpo", "params": '{"is_centered": NaN}'}),
        ("nan_in_list", {"indicatorName": "macd", "params": '{"fast_period": [NaN]}'}),
        ("nan_top_level", {"indicatorName": "sma", "params": "NaN"}),
        ("infinity_top_level", {"indicatorName": "sma", "params": " -Infinity "}),
        ("lowercase_nan", {"indicatorName": "sma", "params": '{"period": nan}'}),
        ("written_order", {"indicatorName": "sma", "params": '{"zzz": 1, "aaa": 2}'}),
        ("repeated_key", {"indicatorName": "sma", "params": '{"period": 5, "zzz": 1, "period": NaN}'}),
        ("nested_object", {"indicatorName": "sma", "params": '{"period": {"a": NaN, "b": [1, {"c": "}"}]}}'}),
    ):
        node("execute_indicator", {**base, **extra}, label)
    node("execute_indicator", {**base, "indicatorName": "rsi"}, "history_error",
         history={"status": "error", "message": "Interval not supported"})
    node("execute_indicator", {**base, "indicatorName": "rsi", "source": "db"}, "history_error_db",
         history={"status": "error", "message": "No data"})
    node("execute_indicator", {**base, "indicatorName": "rsi"}, "empty_history",
         history="empty")

    nested = {"indicatorName": "sma", "params": '{"period": 5}', "outputVariable": "n"}
    for label, extra in (
        ("rsi_series", {"sourceSeries": "{{rsi.series}}"}),
        ("macd_out0", {"sourceSeries": "{{macd.series}}"}),
        ("macd_field", {"sourceSeries": "{{macd.series}}", "sourceField": "out2"}),
        ("history_close", {"sourceSeries": "{{h.data}}"}),
        ("history_high", {"sourceSeries": "{{h.data}}", "sourceField": "high"}),
        ("history_missing_field", {"sourceSeries": "{{h.data}}", "sourceField": "nope"}),
        ("numbers", {"sourceSeries": "{{numbers}}"}),
        ("texts", {"sourceSeries": "{{texts}}"}),
        ("holes", {"sourceSeries": "{{holes}}", "params": '{"period": 2}'}),
        ("bad", {"sourceSeries": "{{bad}}"}),
        ("nested_list", {"sourceSeries": "{{nested_list}}"}),
        ("flags", {"sourceSeries": "{{flags}}", "params": '{"period": 2}'}),
        ("json_text", {"sourceSeries": "[1, 2, 3, 4, 5, 6]"}),
        ("json_text_rows", {"sourceSeries": '[{"value": 1}, {"value": 2}, {"value": 3}]', "params": '{"period": 2}'}),
        ("json_text_object", {"sourceSeries": '{"value": 1}'}),
        ("unresolved", {"sourceSeries": "{{missing.series}}"}),
        ("not_json", {"sourceSeries": "1, 2, 3"}),
        ("scalar", {"sourceSeries": "{{scalar}}"}),
        ("dict", {"sourceSeries": "{{dict}}"}),
        ("list_value", {"sourceSeries": [3.0, 4.0, 5.0, 6.0, 7.0, 8.0]}),
        ("number_value", {"sourceSeries": 7}),
        ("multi_series", {"sourceSeries": "{{rsi.series}}", "indicatorName": "atr"}),
        ("too_short", {"sourceSeries": "[5]"}),
        ("tail_offset", {"sourceSeries": "{{rsi.series}}", "tailBars": 8, "offsetBars": 2}),
    ):
        node("execute_indicator", {**nested, **extra}, "nested_" + label, history="empty")

    for label, data, history, now in (
        ("daily", {"symbol": "SBIN", "interval": "D", "offsetBars": 2}, "trend", datetime(2026, 10, 30, 11)),
        ("daily_text_offset", {"symbol": "SBIN", "interval": "D", "offsetBars": "4"}, "trend", None),
        ("too_far", {"symbol": "SBIN", "interval": "D", "offsetBars": 400}, "trend", None),
        ("negative", {"symbol": "SBIN", "interval": "D", "offsetBars": -3}, "trend", None),
        ("intraday_forming", {"symbol": "SBIN", "interval": "5m", "offsetBars": 0}, "gaps",
         datetime.fromtimestamp(shapes["gaps"]["timestamp"][-1]) + timedelta(minutes=2)),
        ("no_symbol", {"symbol": "", "interval": "D"}, "trend", None),
        ("history_error", {"symbol": "SBIN", "interval": "3m"}, {"status": "error", "message": "bad interval"}, None),
    ):
        node("execute_bar_offset", data, "bar_" + label, history=history, now=now)

    last_daily = datetime.fromtimestamp(shapes["trend"]["timestamp"][-1])
    for label, data, history, now in (
        ("day", {"symbol": "SBIN", "period": "previous_day"}, "trend", last_daily + timedelta(days=1)),
        ("week", {"symbol": "SBIN", "period": "previous_week"}, "trend", last_daily + timedelta(days=3)),
        ("month", {"symbol": "SBIN", "period": "previous_month"}, "trend", last_daily + timedelta(days=3)),
        ("hour", {"symbol": "SBIN", "period": "previous_hour"}, "gaps",
         datetime.fromtimestamp(shapes["gaps"]["timestamp"][-1]) + timedelta(minutes=1)),
        ("unknown", {"symbol": "SBIN", "period": "previous_quarter"}, "trend", None),
        ("empty", {"symbol": "SBIN", "period": "previous_day"}, "empty", None),
        ("no_symbol", {"symbol": "", "period": "previous_day"}, "trend", None),
    ):
        node("execute_prior_period_ohlc", data, "prior_" + label, history=history, now=now)
    return {"variables": variables, "cases": cases}


def editor_lists(web: Path) -> dict:
    """The web editor's indicator lists (``frontend/src/lib/flow/constants.ts``):
    categories, catalogue entries and parameter fields, as written."""
    text = (web / "frontend/src/lib/flow/constants.ts").read_text().replace("\r\n", "\n")

    def block(start, end):
        rest = text[text.index(start):]
        return rest[: rest.index(end)]

    categories = re.findall(r"^  '([^']+)',$", block("export const INDICATOR_CATEGORIES", "] as const"), re.M)
    catalog = [
        {"value": v, "label": l, "category": c}
        for v, l, c in re.findall(
            r"\{ value: '([^']+)', label: '([^']+)', category: '([^']+)' \}",
            block("export const INDICATOR_CATALOG", "] as const"),
        )
    ]
    params_text = block("export const INDICATOR_PARAMS", "\n}\n")
    starts = [(m.start(), m.group(1)) for m in re.finditer(r"^  ([a-z0-9_]+): \[", params_text, re.M)]
    params = {}
    for i, (at, name) in enumerate(starts):
        end = starts[i + 1][0] if i + 1 < len(starts) else len(params_text)
        fields = []
        for f in re.finditer(r"\{\s*name: '([^']+)',\s*label: '([^']+)',\s*type: '(\w+)',\s*default: ([^,}\n]+?),?\s*(?:choices: \[([^\]]*)\],?\s*)?\}", params_text[at:end], re.S):
            choices = re.findall(r"'([^']*)'", f.group(5)) if f.group(5) is not None else None
            fields.append({"name": f.group(1), "label": f.group(2), "type": f.group(3),
                           "default": f.group(4).strip(), "choices": choices})
        params[name] = fields
    return {"categories": categories, "catalog": catalog, "params": params}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--web", type=Path, required=True, help="read-only OpenAlgo web checkout")
    parser.add_argument("--out", type=Path, required=True, help="fixture folder to write")
    parser.add_argument(
        "--candles",
        type=Path,
        default=Path(__file__).resolve().parent.parent
        / "src-tauri/tests/fixtures/agent_indicators/candles.csv",
    )
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    shapes = load_shapes(args.candles)
    with tempfile.TemporaryDirectory() as scratch:
        svc = load_web(args.web.resolve(), Path(scratch))
        from openalgo import __version__ as openalgo_version
        from openalgo import ta

        Executor = executor_class(args.web.resolve())
        freeze(svc, datetime(2026, 10, 30, 11, 0))
        meta = {"openalgo": openalgo_version, "timezone": "Asia/Kolkata"}
        write(args.out, "catalogue.json", {**meta, **catalogue_cases(svc, ta)})
        write(args.out, "compute.json", {**meta, "cases": compute_cases(svc, ta, shapes)})
        write(args.out, "params.json", {**meta, "float_for_int": float_params(svc, ta, shapes)})
        write(args.out, "types.json", {**meta, "cases": type_cases(svc, ta, shapes)})
        write(args.out, "editor.json", {**meta, **editor_lists(args.web.resolve())})
        write(args.out, "window.json", {**meta, **window_cases(svc)})
        write(args.out, "bars.json", {**meta, **bars_cases(svc, shapes)})
        write(args.out, "executor.json", {**meta, **executor_cases(svc, Executor, shapes)})


if __name__ == "__main__":
    main()
