#!/usr/bin/env python3
"""Record the golden values the Agent's indicator port is checked against.

The desktop computes the Agent's 127 indicators in Rust
(``src-tauri/src/agent/indicators``) on ``oa_core``, the Rust core of
``openalgo.ta``. The numbers must be the ones OpenAlgo web answers with, so
this script runs the web's own code over fixed candles and writes what it
returns:

* ``candles.csv``: synthetic but realistic OHLCV in several shapes (a trend, a
  range, an intraday series with gaps, flat bars and zero-volume bars, a
  series whose first bars are missing, and four bars to check what every
  indicator does with too little history), plus a benchmark close for ``beta``
  and ``correlation`` and two signal columns for the pair utilities.
* ``registry.json``: the web's registry (``services/agent/indicators``), and
  each entry as the discovery tools render it.
* ``raw_<shape>.json``: every ``ta.<name>(...)`` call, to 10 significant
  digits, and every error's type and message.
* ``compute.json``: the web's ``compute()`` over cleaned frames, including
  every refusal sentence; its search, alias, window-sizing and rounding
  answers; and the scan condition parser's and evaluator's answers.

Run it with the openalgo version the web pins (``uv.lock``), in a scratch
virtual environment, never the web's own:

    uv venv <scratch>/oa && VIRTUAL_ENV=<scratch>/oa uv pip install openalgo==2.0.5
    <scratch>/oa/bin/python scripts/gen_agent_indicator_goldens.py \
        --web ../openalgo --out src-tauri/tests/fixtures/agent_indicators

``--web`` is a read-only OpenAlgo web checkout: the registry, descriptions and
compute modules are copied from it into a temporary folder and imported from
there, so nothing is written to the checkout.

One deliberate deviation is applied before anything is recorded (lead
decision, 1.0.4): the web registers ``vi`` and ``ulcerindex`` as broken, a
verdict measured on openalgo 2.0.3, but the 2.0.5 build the web pins computes
both. The desktop enables them, so the registry and the ``compute()`` goldens
are recorded with both marked ``ok`` and their measured warm-up;
``registry.json`` keeps the web's original entries under ``web_overrides``.
"""

from __future__ import annotations

import argparse
import ast
import csv
import json
import math
import shutil
import sys
import tempfile
from pathlib import Path

import numpy as np
import pandas as pd

IST = "Asia/Kolkata"
SIG_DIGITS = 10

# ---------------------------------------------------------------------------
# The web's modules, copied into a scratch package
# ---------------------------------------------------------------------------

_PARSE_NAMES = {
    "_CONDITION_RE",
    "_CROSS_RE",
    "_CONDITION_HELP",
}
_PARSE_FUNCS = {"parse_condition", "_pick", "_latest_finite", "evaluate_condition"}


#: Desktop deviations from the web registry, applied before recording.
DESKTOP_ENABLED = {
    "vi": "",
    "ulcerindex": "signal_type is not validated by the library.",
}


def measured_warmup(ta, registry, defaults, name: str, bars: dict) -> int:
    """First bar where every output of ``name`` is finite, with the defaults."""
    spec = registry.REGISTRY[name]
    result = getattr(ta, name)(*[bars[c] for c in spec.inputs], **defaults.get(name, {}))
    parts = list(result) if isinstance(result, tuple) else [result]
    finite = np.all([np.isfinite(np.asarray(part, dtype=float)) for part in parts], axis=0)
    return int(np.argmax(finite))


def enable_desktop_entries(ta, registry, defaults, bars: dict) -> dict:
    """Mark the entries the desktop enables ``ok``; return the web's originals."""
    import dataclasses

    originals = {}
    for name, note in DESKTOP_ENABLED.items():
        spec = registry.REGISTRY[name]
        originals[name] = {"status": spec.status, "note": spec.note, "warmup": spec.warmup}
        registry.REGISTRY[name] = dataclasses.replace(
            spec,
            status="ok",
            note=note,
            warmup=measured_warmup(ta, registry, defaults, name, bars),
        )
    return originals


def load_web(web: Path, scratch: Path):
    """Copy the web's indicator modules into ``scratch`` and import them."""
    pkg = scratch / "services"
    (pkg / "agent" / "indicators").mkdir(parents=True)
    (pkg / "agent" / "tools").mkdir(parents=True)
    for init in (pkg, pkg / "agent", pkg / "agent" / "tools"):
        (init / "__init__.py").write_text("")
    for name in ("__init__", "registry", "descriptions", "compute"):
        shutil.copy(web / f"services/agent/indicators/{name}.py", pkg / "agent" / "indicators")

    service = ast.parse((web / "services/indicator_service.py").read_text())
    defaults = next(
        node
        for node in service.body
        if isinstance(node, ast.Assign)
        and any(getattr(t, "id", "") == "REQUIRED_PARAM_DEFAULTS" for t in node.targets)
    )
    (pkg / "indicator_service.py").write_text(ast.unparse(defaults) + "\n")

    tools = ast.parse((web / "services/agent/tools/indicators.py").read_text())
    kept = ["import re", "from collections.abc import Mapping", "from typing import Any"]
    for node in tools.body:
        if isinstance(node, ast.Assign) and any(
            getattr(t, "id", "") in _PARSE_NAMES for t in node.targets
        ):
            kept.append(ast.unparse(node))
        if isinstance(node, ast.FunctionDef) and node.name in _PARSE_FUNCS:
            kept.append(ast.unparse(node))
    (pkg / "agent" / "tools" / "parse.py").write_text("\n\n".join(kept) + "\n")

    sys.path.insert(0, str(scratch))
    from services.agent.indicators import compute, descriptions, registry
    from services.agent.tools import parse
    from services.indicator_service import REQUIRED_PARAM_DEFAULTS

    return registry, descriptions, compute, parse, REQUIRED_PARAM_DEFAULTS


# ---------------------------------------------------------------------------
# Candles
# ---------------------------------------------------------------------------


def _daily_stamps(start: str, count: int) -> list[int]:
    days = pd.bdate_range(start=start, periods=count, tz=IST)
    return [int(day.timestamp()) for day in days]


def _intraday_stamps(start: str, count: int) -> list[int]:
    stamps: list[int] = []
    day = pd.Timestamp(start, tz=IST)
    while len(stamps) < count:
        if day.weekday() < 5:
            moment = day + pd.Timedelta(hours=9, minutes=15)
            close = day + pd.Timedelta(hours=15, minutes=30)
            while moment < close and len(stamps) < count:
                stamps.append(int(moment.timestamp()))
                moment += pd.Timedelta(minutes=5)
        day += pd.Timedelta(days=1)
    return stamps


def _bars(closes: np.ndarray, rng: np.random.Generator, wick: float, gap: float) -> dict:
    n = closes.size
    opens = np.empty(n)
    opens[0] = closes[0] * (1 + rng.normal(0, gap))
    opens[1:] = closes[:-1] * (1 + rng.normal(0, gap, n - 1))
    top = np.maximum(opens, closes)
    bottom = np.minimum(opens, closes)
    highs = top * (1 + np.abs(rng.normal(0, wick, n)))
    lows = bottom * (1 - np.abs(rng.normal(0, wick, n)))
    returns = np.abs(np.diff(np.log(closes), prepend=np.log(closes[0])))
    volume = rng.lognormal(13.5, 0.35, n) * (1 + 40 * returns)
    return {
        "open": np.round(opens, 2),
        "high": np.round(highs, 2),
        "low": np.round(lows, 2),
        "close": np.round(closes, 2),
        "volume": np.round(volume),
    }


def _with_aux(rng: np.random.Generator, bars: dict, bench: np.ndarray) -> dict:
    close = bars["close"]
    aux = pd.Series(close).rolling(8, min_periods=1).mean().to_numpy()
    bars["bench"] = np.round(bench, 2)
    bars["aux"] = np.round(aux, 2)
    bars["sig_up"] = (close > bars["aux"]).astype(float)
    bars["sig_dn"] = (close < bars["aux"]).astype(float)
    # Keep high and low honest after rounding.
    bars["high"] = np.maximum.reduce([bars["high"], bars["open"], bars["close"]])
    bars["low"] = np.minimum.reduce([bars["low"], bars["open"], bars["close"]])
    return bars


def shape_trend(rng: np.random.Generator) -> tuple[list[int], dict]:
    n = 300
    market = np.cumsum(rng.normal(0.0004, 0.009, n))
    own = 1.2 * market + np.cumsum(rng.normal(0.0003, 0.008, n))
    closes = 1450.0 * np.exp(own)
    bench = 22000.0 * np.exp(market)
    return _daily_stamps("2025-01-01", n), _with_aux(rng, _bars(closes, rng, 0.006, 0.003), bench)


def shape_range(rng: np.random.Generator) -> tuple[list[int], dict]:
    n = 200
    level = np.empty(n)
    level[0] = 0.0
    for i in range(1, n):
        level[i] = 0.85 * level[i - 1] + rng.normal(0, 0.006)
    cycle = 0.035 * np.sin(np.arange(n) * 2 * math.pi / 37)
    closes = 512.0 * np.exp(level + cycle)
    bench = 48000.0 * np.exp(0.5 * cycle + np.cumsum(rng.normal(0, 0.004, n)))
    return _daily_stamps("2025-03-03", n), _with_aux(rng, _bars(closes, rng, 0.005, 0.002), bench)


def shape_gaps(rng: np.random.Generator) -> tuple[list[int], dict]:
    n = 200
    stamps = _intraday_stamps("2025-06-02", n)
    steps = rng.normal(0, 0.0018, n)
    days = [pd.Timestamp(s, unit="s", tz=IST).date() for s in stamps]
    for i in range(1, n):
        if days[i] != days[i - 1]:
            steps[i] += rng.choice([-1, 1]) * rng.uniform(0.02, 0.04)
    closes = 2875.0 * np.exp(np.cumsum(steps))
    bench = 24500.0 * np.exp(np.cumsum(0.6 * steps + rng.normal(0, 0.001, n)))
    bars = _bars(closes, rng, 0.0012, 0.0004)
    for i in range(1, n):
        if days[i] != days[i - 1]:
            bars["open"][i] = np.round(closes[i] * (1 + rng.normal(0, 0.002)), 2)
    # Flat bars (one price all bar) and a run of identical closes.
    for i in (17, 18, 61, 140):
        bars["open"][i] = bars["high"][i] = bars["low"][i] = bars["close"][i]
    for i in range(95, 101):
        bars["close"][i] = bars["close"][94]
    # Quiet bars: no trade printed.
    for i in (33, 34, 120):
        bars["volume"][i] = 0.0
    return stamps, _with_aux(rng, bars, bench)


def shape_nan_head(rng: np.random.Generator) -> tuple[list[int], dict]:
    n = 120
    closes = 860.0 * np.exp(np.cumsum(rng.normal(0.0005, 0.011, n)))
    bench = 23000.0 * np.exp(np.cumsum(rng.normal(0.0003, 0.008, n)))
    bars = _with_aux(rng, _bars(closes, rng, 0.006, 0.003), bench)
    for column in bars:
        bars[column][:6] = np.nan
    return _daily_stamps("2025-02-03", n), bars


def shape_tiny(rng: np.random.Generator) -> tuple[list[int], dict]:
    n = 4
    closes = 101.0 * np.exp(np.cumsum(rng.normal(0, 0.01, n)))
    bench = 99.0 * np.exp(np.cumsum(rng.normal(0, 0.01, n)))
    return _daily_stamps("2025-04-01", n), _with_aux(rng, _bars(closes, rng, 0.006, 0.003), bench)


SHAPES = {
    "trend": shape_trend,
    "range": shape_range,
    "gaps": shape_gaps,
    "nan_head": shape_nan_head,
    "tiny": shape_tiny,
}
COLUMNS = ("open", "high", "low", "close", "volume", "bench", "aux", "sig_up", "sig_dn")


def build_candles() -> dict[str, tuple[list[int], dict]]:
    rng = np.random.default_rng(20261027)
    return {name: build(rng) for name, build in SHAPES.items()}


def write_csv(path: Path, candles: dict) -> None:
    with path.open("w", newline="") as handle:
        writer = csv.writer(handle, lineterminator="\n")
        writer.writerow(("shape", "timestamp", *COLUMNS))
        for shape, (stamps, bars) in candles.items():
            for i, stamp in enumerate(stamps):
                row = [shape, stamp]
                for column in COLUMNS:
                    value = bars[column][i]
                    row.append("" if math.isnan(value) else repr(float(value)))
                writer.writerow(row)


def read_csv(path: Path) -> dict[str, tuple[list[int], dict]]:
    """Read the candles back, so every value used is the one on disk."""
    out: dict[str, tuple[list[int], dict]] = {}
    with path.open() as handle:
        for row in csv.DictReader(handle):
            stamps, bars = out.setdefault(row["shape"], ([], {c: [] for c in COLUMNS}))
            stamps.append(int(row["timestamp"]))
            for column in COLUMNS:
                bars[column].append(float(row[column]) if row[column] else float("nan"))
    return {
        shape: (stamps, {c: np.array(v, dtype=np.float64) for c, v in bars.items()})
        for shape, (stamps, bars) in out.items()
    }


def frame_for(stamps: list[int], bars: dict, columns=("open", "high", "low", "close", "volume")):
    """The frame the web's candle_frame builds: IST index, float OHLCV."""
    index = pd.to_datetime(stamps, unit="s", utc=True).tz_convert(IST)
    return pd.DataFrame({c: bars[c] for c in columns}, index=index).astype("float64")


# ---------------------------------------------------------------------------
# Raw ta calls
# ---------------------------------------------------------------------------

PAIR_INPUTS = {
    "crossover": ("close", "aux"),
    "crossunder": ("close", "aux"),
    "cross": ("close", "aux"),
    "exrem": ("sig_up", "sig_dn"),
    "flip": ("sig_up", "sig_dn"),
    "valuewhen": ("sig_up", "close"),
    "correlation": ("close", "bench"),
    "beta": ("close", "bench"),
}

#: Extra parameter sets per indicator: enums, conditional outputs, the
#: second branch of every switch, and periods other than the default.
VARIANTS: dict[str, list[dict]] = {
    "sma": [{"period": 5}, {"period": 50}],
    "ema": [{"period": 9}, {"period": 50}],
    "wma": [{"period": 9}],
    "dema": [{"period": 9}],
    "tema": [{"period": 9}],
    "hma": [{"period": 9}, {"period": 20}],
    "vwma": [{"period": 20}],
    "zlema": [{"period": 21}],
    "alma": [{"period": 9, "offset": 0.5, "sigma": 4.0}],
    "kama": [{"length": 10, "fast_length": 3, "slow_length": 20}],
    "t3": [{"period": 5, "v_factor": 0.9}],
    "frama": [{"period": 16}],
    "trima": [{"period": 9}, {"period": 10}],
    "mcginley": [{"period": 10}],
    "vidya": [{"period": 9, "alpha": 0.3}],
    "alligator": [{"jaw_period": 21, "jaw_shift": 13, "teeth_period": 13, "lips_period": 8}],
    "ma_envelopes": [{"ma_type": "EMA", "percentage": 1.5}, {"period": 10, "ma_type": "SMA"}],
    "supertrend": [{"period": 7, "multiplier": 2.0}],
    "ichimoku": [{"displacement": 1}, {"displacement": 2}, {"conversion_periods": 5, "base_periods": 10}],
    "ckstop": [{"p": 5, "x": 1.5, "q": 4}],
    "rsi": [{"period": 2}, {"period": 21}],
    "macd": [{"fast_period": 5, "slow_period": 13, "signal_period": 4}],
    "stochastic": [{"k_period": 5, "smooth_k": 1, "d_period": 5}],
    "cci": [{"period": 14}],
    "williams_r": [{"period": 21}],
    "elderray": [{"period": 21}],
    "fisher": [{"length": 5}],
    "crsi": [{"lenrsi": 5, "lenupdown": 3, "lenroc": 20}],
    "atr": [{"period": 5}],
    "bbands": [{"period": 10, "std_dev": 1.5}],
    "keltner": [{"ema_period": 10, "atr_period": 5, "multiplier": 1.5}],
    "donchian": [{"period": 10}],
    "chaikin": [{"ema_period": 5, "roc_period": 3}],
    "natr": [{"period": 7}],
    "ultimate_oscillator": [{"period1": 5, "period2": 10, "period3": 20}],
    "massindex": [{"length": 25}],
    "bbpercent": [{"period": 10, "std_dev": 1.0}],
    "bbwidth": [{"period": 10, "std_dev": 3.0}],
    "chandelier_exit": [{"period": 10, "multiplier": 2.0}],
    "hv": [{"length": 20, "annual": 252, "per": 1}, {"length": 5, "annual": 365, "per": 7}],
    "ulcerindex": [{"return_signal": True}, {"return_signal": True, "signal_type": "EMA"}],
    "starc": [{"ma_period": 6, "atr_period": 10, "multiplier": 2.0}],
    "obv_smoothed": [
        {"ma_type": "SMA"},
        {"ma_type": "SMA + Bollinger Bands"},
        {"ma_type": "EMA"},
        {"ma_type": "SMMA (RMA)"},
        {"ma_type": "WMA"},
        {"ma_type": "VWMA", "ma_length": 10},
    ],
    "vwap": [{"source": "hl2"}, {"source": "close"}, {"source": "ohlc4"}, {"source": "other"}],
    "mfi": [{"period": 5}],
    "cmf": [{"period": 10}],
    "emv": [{"length": 5, "divisor": 100000000}],
    "force_index": [{"length": 2}],
    "nvi_with_ema": [{"ema_length": 20}],
    "pvi": [{"initial_value": 1000.0}],
    "pvi_with_signal": [{"signal_type": "SMA", "signal_length": 10}, {"signal_length": 10}],
    "volosc": [{"short_length": 3, "long_length": 21}],
    "vroc": [{"period": 10}],
    "kvo": [{"trig_len": 5, "fast_x": 10, "slow_x": 20}],
    "rvol": [{"period": 5}],
    "cmo": [{"period": 9}],
    "trix": [{"length": 9}],
    "uo_oscillator": [{"period1": 3, "period2": 6, "period3": 12}],
    "awesome_oscillator": [{"fast_period": 3, "slow_period": 10}],
    "accelerator_oscillator": [{"period": 3}],
    "ppo": [{"fast_period": 5, "slow_period": 10, "signal_period": 3}],
    "po": [{"ma_type": "EMA"}, {"fast_period": 3, "slow_period": 7}],
    "dpo": [{"is_centered": True}, {"period": 10}],
    "aroon_oscillator": [{"period": 25}],
    "stochrsi": [{"rsi_period": 7, "stoch_period": 7, "k_period": 2, "d_period": 2}],
    "rvi": [{"period": 4}],
    "cho": [{"fast_period": 5, "slow_period": 20}],
    "chop": [{"period": 1}, {"period": 7}],
    "kst": [{"roclen1": 5, "roclen2": 7, "roclen3": 9, "roclen4": 11, "smalen1": 3, "siglen": 3}],
    "tsi": [{"long_period": 10, "short_period": 5, "signal_period": 4}],
    "vi": [{"period": 7}],
    "stc": [{"fast_length": 10, "slow_length": 20, "cycle_length": 5}],
    "gator_oscillator": [{"jaw_period": 21, "teeth_period": 13, "lips_period": 8}],
    "coppock": [{"wma_length": 5, "long_roc_length": 8, "short_roc_length": 4}],
    "linreg": [{"period": 5}],
    "lrslope": [{"period": 20, "interval": 2}],
    "correlation": [{"period": 5}],
    "beta": [{"period": 30}],
    "variance": [
        {"mode": "LR"},
        {"return_components": True},
        {"mode": "LR", "return_components": True, "lookback": 10, "ema_period": 5},
    ],
    "tsf": [{"period": 5}],
    "median": [{"period": 4}, {"period": 7}],
    "median_bands": [{"median_length": 4, "atr_length": 7, "atr_mult": 1.5}],
    "mode": [{"period": 15, "bins": 5}],
    "adx": [{"period": 7}],
    "aroon": [{"period": 14}],
    "psar": [{"acceleration": 0.01, "maximum": 0.1}],
    "dmi": [{"period": 7}],
    "fractals": [{"periods": 3}],
    "rwi": [{"period": 7}],
    "mom": [{"period": 3}],
    "rocp": [{"period": 3}],
    "rocr": [{"period": 3}],
    "rocr100": [{"period": 3}],
    "midpoint": [{"period": 5}],
    "apo": [{"ma_type": "EMA"}, {"ma_type": "junk", "fast_period": 5}],
    "midprice": [{"period": 5}],
    "plus_dm": [{"period": 1}, {"period": 7}],
    "minus_dm": [{"period": 1}, {"period": 7}],
    "dx": [{"period": 7}],
    "adxr": [{"period": 7}],
    "stochf": [{"fastk_period": 9, "fastd_period": 5}],
    "linregangle": [{"period": 5}],
    "linregintercept": [{"period": 5}],
    "highest": [{"period": 5}],
    "lowest": [{"period": 5}],
    "change": [{"length": 3}],
    "roc": [{"length": 5}],
    "stdev": [{"period": 5}],
    "valuewhen": [{"n": 2}, {"n": 3}],
    "rising": [{"length": 3}],
    "falling": [{"length": 3}],
}


def round_sig(value: float) -> float:
    return float(f"{value:.{SIG_DIGITS}g}")


def output_values(part) -> list:
    raw = part.tolist() if isinstance(part, (pd.Series, np.ndarray)) else list(part)
    out = []
    for item in raw:
        if isinstance(item, (bool, np.bool_)):
            out.append(bool(item))
        else:
            number = float(item)
            out.append(None if math.isnan(number) else ("inf" if number == math.inf else ("-inf" if number == -math.inf else round_sig(number))))
    return out


def raw_call(ta, registry, name: str, bars: dict, index, params: dict) -> dict:
    spec = registry.REGISTRY[name]
    inputs = list(PAIR_INPUTS.get(name, spec.inputs))
    series = [pd.Series(bars[column], index=index) for column in inputs]
    case = {"name": name, "params": params, "inputs": inputs}
    try:
        result = getattr(ta, name)(*series, **params)
    except Exception as exc:  # noqa: BLE001
        case["error"] = {"kind": type(exc).__name__, "message": str(exc)}
        return case
    parts = list(result) if isinstance(result, tuple) else [result]
    case["outputs"] = [output_values(part) for part in parts]
    return case


def record_raw(ta, registry, defaults, candles) -> dict[str, dict]:
    files = {}
    for shape, (stamps, bars) in candles.items():
        index = pd.to_datetime(stamps, unit="s", utc=True).tz_convert(IST)
        cases = []
        for name in sorted(registry.REGISTRY):
            base = dict(defaults.get(name, {}))
            cases.append(raw_call(ta, registry, name, bars, index, base))
            if shape in ("trend", "gaps"):
                for variant in VARIANTS.get(name, []):
                    cases.append(raw_call(ta, registry, name, bars, index, {**base, **variant}))
        files[shape] = {"shape": shape, "bars": len(stamps), "cases": cases}
    return files


# ---------------------------------------------------------------------------
# The web's compute() and discovery
# ---------------------------------------------------------------------------


def jsonable(value):
    """Python values as the web's json.dumps renders them; floats kept exact."""
    if isinstance(value, dict):
        return {str(k): jsonable(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [jsonable(v) for v in value]
    if isinstance(value, (np.bool_,)):
        return bool(value)
    if isinstance(value, (np.integer,)):
        return int(value)
    if isinstance(value, (np.floating,)):
        return float(value)
    return value


#: The frames the compute goldens run over, by key. The Rust test rebuilds
#: each from candles.csv with the same recipe: a shape, a row range, columns
#: dropped, and columns replaced by one value.
FRAMES = {
    "trend": {"shape": "trend"},
    "range": {"shape": "range"},
    "gaps": {"shape": "gaps"},
    "short": {"shape": "range", "end": 30},
    "empty": {"shape": "trend", "end": 0},
    "no_volume": {"shape": "trend", "drop": ["volume"]},
    "zero_volume": {"shape": "trend", "fill": {"volume": 0.0}},
    "nan_open": {"shape": "trend", "fill": {"open": "nan"}},
}

#: The comparison closes for beta and correlation, by key: a shape's bench
#: column, optionally with every bar ``i % 7 == 3`` missing and one extra bar
#: three days after the last (``gappy``), moved ``shift_days`` later, or cut
#: to a row range.
SECONDS = {
    "bench": {"shape": "trend"},
    "gappy_bench": {"shape": "trend", "gappy": True},
    "disjoint": {"shape": "trend", "shift_days": 4000},
    "leading": {"shape": "trend", "start": 40},
    "short_bench": {"shape": "range", "end": 30},
}


def build_frame(candles, recipe):
    stamps, bars = candles[recipe["shape"]]
    frame = frame_for(stamps, bars)
    frame = frame.iloc[recipe.get("start", 0) : recipe.get("end")]
    frame = frame.drop(columns=recipe.get("drop", []))
    for column, value in recipe.get("fill", {}).items():
        frame = frame.assign(**{column: float(value)})
    return frame


def build_second(candles, recipe):
    stamps, bars = candles[recipe["shape"]]
    values = list(bars["bench"])
    stamps = list(stamps)
    if recipe.get("gappy"):
        keep = [i for i in range(len(stamps)) if i % 7 != 3]
        extra = stamps[-1] + 86400 * 3
        stamps = [stamps[i] for i in keep] + [extra]
        values = [values[i] for i in keep] + [values[-1]]
    stamps = [s + 86400 * recipe.get("shift_days", 0) for s in stamps]
    lo, hi = recipe.get("start", 0), recipe.get("end")
    index = pd.to_datetime(stamps[lo:hi], unit="s", utc=True).tz_convert(IST)
    return pd.Series(values[lo:hi], index=index, dtype="float64")


def compute_case(compute, candles, label, name, frame, params=None, second=None, last_n=10):
    case = {"label": label, "name": name, "params": params, "last_n": last_n,
            "frame": frame, "second": second}
    data = build_frame(candles, FRAMES[frame])
    other = build_second(candles, SECONDS[second]) if second else None
    try:
        result = compute.compute(name, data, params, second_series=other, last_n=last_n)
    except compute.IndicatorError as exc:
        case["error"] = str(exc)
    else:
        case["result"] = jsonable(result)
    return case


def record_compute(registry, compute, parse, candles) -> dict:
    stamps, bars = candles["trend"]
    frame = frame_for(stamps, bars)
    cases = []
    for name in sorted(registry.REGISTRY):
        second = "bench" if registry.REGISTRY[name].needs_second_series else None
        cases.append(compute_case(compute, candles, "default", name, "trend", None, second))

    extra = [
        ("alias length", "rsi", "trend", {"length": 21}, None, 10),
        ("alias fast slow signal", "macd", "trend", {"fast": 10, "slow": 20, "signal": 5}, None, 10),
        ("alias std", "bbands", "trend", {"std": 2.5}, None, 10),
        ("alias stdev upper case", "bbands", "trend", {"STDEV": 1.5, " Period ": 10}, None, 10),
        ("alias fast on kama", "kama", "trend", {"fast": 3}, None, 10),
        ("alias ambiguous kept", "ppo", "trend", {"length": 5}, None, 10),
        ("alias with real name too", "rsi", "trend", {"length": 21, "period": 9}, None, 10),
        ("numbers as text", "supertrend", "trend", {"period": "7", "multiplier": "2.5"}, None, 10),
        ("float period whole", "sma", "trend", {"period": 20.0}, None, 10),
        ("bool param text", "dpo", "trend", {"is_centered": "yes"}, None, 10),
        ("bool param number", "dpo", "trend", {"is_centered": 1}, None, 10),
        ("bool param false text", "variance", "trend", {"return_components": "no"}, None, 10),
        ("none param skipped", "rsi", "trend", {"period": None}, None, 10),
        ("required default applied", "sma", "trend", {}, None, 10),
        ("required default with other", "roc", "trend", None, None, 10),
        ("conditional bollinger", "obv_smoothed", "trend", {"ma_type": "SMA + Bollinger Bands"}, None, 10),
        ("conditional components", "variance", "trend", {"return_components": True}, None, 10),
        ("conditional components text", "variance", "trend", {"return_components": "true"}, None, 10),
        ("alias uo", "uo_oscillator", "trend", None, None, 10),
        ("last_n one", "macd", "trend", None, None, 1),
        ("last_n zero", "rsi", "trend", None, None, 0),
        ("last_n negative", "rsi", "trend", None, None, -4),
        ("last_n over length", "rsi", "short", None, None, 500),
        ("range rsi", "rsi", "range", None, None, 10),
        ("gaps vwap", "vwap", "gaps", None, None, 10),
        ("gaps fractals", "fractals", "gaps", None, None, 25),
        ("gaps rising", "rising", "gaps", {"length": 2}, None, 12),
        ("gaps supertrend", "supertrend", "gaps", None, None, 15),
        ("gaps mfi zero volume bars", "mfi", "gaps", None, None, 10),
        ("beta gappy bench", "beta", "trend", {"period": 60}, "gappy_bench", 10),
        ("correlation leading gap", "correlation", "trend", None, "leading", 10),
        ("short frame warm-up", "macd", "short", None, None, 10),
        ("short frame stochrsi", "stochrsi", "short", None, None, 10),
        ("enabled vi", "vi", "trend", {"period": 7}, None, 10),
        ("enabled ulcerindex signal", "ulcerindex", "trend", {"return_signal": True}, None, 10),
        # Refusals.
        ("unknown indicator", "bollinger", "trend", None, None, 10),
        ("pair utility", "crossover", "trend", None, None, 10),
        ("pair exrem", "exrem", "trend", None, None, 10),
        ("unknown param", "rsi", "trend", {"foo": 1}, None, 10),
        ("unknown param none valid", "obv", "trend", {"period": 3}, None, 10),
        ("not whole", "rsi", "trend", {"period": 14.5}, None, 10),
        ("not a number", "rsi", "trend", {"period": "abc"}, None, 10),
        ("not a number list", "rsi", "trend", {"period": [14]}, None, 10),
        ("negative", "rsi", "trend", {"period": -3}, None, 10),
        ("zero", "rsi", "trend", {"period": 0}, None, 10),
        ("negative zero", "rsi", "trend", {"period": -0.0}, None, 10),
        ("bool as period", "rsi", "trend", {"period": True}, None, 10),
        ("period as text float", "rsi", "trend", {"period": "1_0"}, None, 10),
        ("float not a number", "bbands", "trend", {"std_dev": "wide"}, None, 10),
        ("float from text", "bbands", "trend", {"std_dev": " 2.5 "}, None, 10),
        ("enum refused", "po", "trend", {"ma_type": "WMA"}, None, 10),
        ("enum refused number", "variance", "trend", {"mode": 5}, None, 10),
        ("enum accepted", "pvi_with_signal", "trend", {"signal_type": "SMA", "signal_length": 20}, None, 10),
        ("free text accepted", "apo", "trend", {"ma_type": "junk"}, None, 10),
        ("empty frame", "rsi", "empty", None, None, 10),
        ("missing volume", "obv", "no_volume", None, None, 10),
        ("missing volume vwma", "vwma", "no_volume", None, None, 10),
        ("zero volume", "mfi", "zero_volume", None, None, 10),
        ("empty open", "bop", "nan_open", None, None, 10),
        ("no second series", "beta", "trend", None, None, 10),
        ("second series disjoint", "correlation", "trend", None, "disjoint", 10),
        ("period longer than frame", "rsi", "short", {"period": 30}, None, 10),
        ("period equal to frame", "sma", "short", {"period": 30}, None, 10),
        ("library period too long", "stochrsi", "short", {"rsi_period": 20, "stoch_period": 20}, None, 10),
        ("library default too long", "lrslope", "short", None, None, 10),
        ("library negative multiplier", "bbands", "trend", {"std_dev": -1}, None, 10),
        ("library offset", "alma", "trend", {"offset": 2}, None, 10),
        ("library sigma", "alma", "trend", {"sigma": 0}, None, 10),
        ("library psar", "psar", "trend", {"acceleration": 0.5, "maximum": 0.2}, None, 10),
        ("library psar negative", "psar", "trend", {"acceleration": -0.5}, None, 10),
        ("library frama odd", "frama", "trend", {"period": 15}, None, 10),
        ("library kama fast slow", "kama", "trend", {"fast_length": 40}, None, 10),
        ("library macd fast slow", "macd", "trend", {"fast_period": 30}, None, 10),
        ("library fractals", "fractals", "trend", {"periods": 1}, None, 10),
        ("library supertrend multiplier", "supertrend", "trend", {"multiplier": 0}, None, 10),
        ("library keltner multiplier", "keltner", "trend", {"multiplier": -2}, None, 10),
        ("library ckstop x", "ckstop", "trend", {"x": 0}, None, 10),
        ("library emv divisor", "emv", "trend", {"divisor": 1}, None, 10),
        ("library lrslope period", "lrslope", "short", {"period": 29}, None, 10),
        ("library hv length", "hv", "short", {"length": 29}, None, 10),
        ("library beta period", "beta", "short", {"period": 29}, "short_bench", 10),
        ("library vidya period", "vidya", "short", {"period": 29}, None, 10),
        ("library cmo period", "cmo", "short", {"period": 29}, None, 10),
        ("library crsi", "crsi", "short", None, None, 10),
        ("library kama length", "kama", "short", {"length": 29}, None, 10),
        ("library ichimoku displacement big", "ichimoku", "short", {"displacement": 40}, None, 10),
        ("library variance mode", "variance", "short", {"mode": "LR"}, None, 10),
        ("library obv bands", "obv_smoothed", "trend", {"ma_type": "SMA + Bollinger Bands", "bb_mult": -1}, None, 10),
    ]
    for label, name, data, params, second, last_n in extra:
        cases.append(compute_case(compute, candles, label, name, data, params, second, last_n))

    required = []
    for name in sorted(registry.REGISTRY):
        spec = registry.REGISTRY[name]
        required.append({"name": name, "params": None, "want": 10, "bars": compute.required_bars(spec, None, 10)})
    for name, params, want in [
        ("rsi", {"period": 50}, 10),
        ("rsi", {"period": 5}, 10),
        ("rsi", {"length": 40}, 5),
        ("sma", {"period": 200}, 10),
        ("sma", {"period": "abc"}, 10),
        ("beta", {"period": 300}, 10),
        ("macd", {"fast_period": 30, "slow_period": 60}, 120),
        ("bbands", {"std_dev": 4}, 10),
        ("lrslope", None, 1),
        ("ichimoku", {"lagging_span2_periods": 80}, 10),
    ]:
        spec = registry.REGISTRY[name]
        required.append({"name": name, "params": params, "want": want, "bars": compute.required_bars(spec, params, want)})

    aliases = []
    for name, params in [
        ("rsi", {"length": 21}),
        ("macd", {"fast": 1, "slow": 2, "signal": 3}),
        ("kama", {"fast": 3, "slow": 5}),
        ("ppo", {"length": 5}),
        ("rsi", {"length": 21, "period": 9}),
        ("bbands", {"STD": 2, "Stddev": 3}),
        ("bbands", {"deviation": 1}),
        ("supertrend", {"Length": 7}),
    ]:
        aliases.append({"name": name, "params": params, "resolved": compute.resolve_param_aliases(registry.REGISTRY[name], params)})

    searches = []
    for query, category in [
        ("", ""), ("bollinger", ""), ("trend strength", ""), ("money flow", ""), ("volatility", ""),
        ("RSI", ""), ("", "volume"), ("", "Utility"), ("average", "trend"), ("zzz", ""), (" adx ", ""),
        ("trap", ""), ("broken", ""),
    ]:
        searches.append({"query": query, "category": category, "names": [s.name for s in compute.search_specs(query, category)]})

    rounding = []
    for value in [0.1234565, 0.1234575, 1.0000005, 2.5e-7, 1.5e-6, -0.0000005, 123456.7891234,
                  1e-7, 0.5, 1234567890.1234567, 3.0000004999999, 1e16, -2.675e-6, 0.0, -0.0,
                  0.0078125, 0.0234375, -0.0078125, 1.0078125, 2.5, 1e300, -1e-300]:
        rounding.append({"value": value, "rounded": round(value, 6)})

    conditions = []
    for text in ["rsi < 30", " RSI<=30.5 ", "adx > 25", "adx >= -1", "macd_line == 0", "x < 1e3", "rsi < 30 and adx > 25",
                 "crossover(macd_line, signal_line)", "crossunder( a ,b )", "cross(k_percent,d_percent)",
                 "CROSSOVER(a, b)", "crossover(a)", "", "rsi<", "__import__('os')", "rsi < -.5", "rsi < 5."]:
        conditions.append({"text": text, "parsed": parse.parse_condition(text)})

    evaluations = []
    rsi_result = compute.compute("rsi", frame, None, last_n=5)
    macd_result = compute.compute("macd", frame, None, last_n=5)
    single = {"indicator": "rsi", "outputs": ["rsi"], "values": {"rsi": [None, None, 41.5, 29.0, None]}}
    warm = {"indicator": "rsi", "outputs": ["rsi"], "values": {"rsi": [None, None]}}
    flags = {"indicator": "rising", "outputs": ["rising"], "values": {"rising": [False, True]}}
    crossing = {"indicator": "macd", "outputs": ["macd_line", "signal_line"],
                "values": {"macd_line": [1.0, None, -0.5, 0.25], "signal_line": [0.0, 0.1, 0.0, 0.0]}}
    flat_cross = {"indicator": "macd", "outputs": ["macd_line", "signal_line"],
                  "values": {"macd_line": [0.0, 0.0], "signal_line": [0.0, 0.0]}}
    down = {"indicator": "macd", "outputs": ["macd_line", "signal_line"],
            "values": {"macd_line": [0.5, -0.5], "signal_line": [0.0, 0.0]}}
    for condition, result in [
        ("rsi < 101", rsi_result), ("RSI > 101", rsi_result), ("rsi == 0", rsi_result),
        ("rsi < 30", single), ("rsi <= 29", single), ("rsi >= 29", single), ("rsi > 30", warm),
        ("signal_line > 0", macd_result), ("histogram < 0", macd_result), ("adx > 25", macd_result),
        ("crossover(macd_line, signal_line)", macd_result), ("cross(macd_line, signal_line)", macd_result),
        ("crossover(macd_line, signal_line)", crossing), ("crossunder(macd_line, signal_line)", crossing),
        ("cross(macd_line, signal_line)", flat_cross), ("crossunder(macd_line, signal_line)", down),
        ("cross(macd_line, signal_line)", down), ("crossover(macd_line, nope)", crossing),
        ("crossover(rsi, rsi)", warm), ("rising > 0", flags), ("crossover(MACD_LINE, Signal_Line)", crossing),
    ]:
        parsed = parse.parse_condition(condition)
        hit, detail = parse.evaluate_condition(parsed, result)
        evaluations.append({"condition": condition, "result": jsonable(result), "hit": hit, "detail": jsonable(detail)})

    return {
        "frames": FRAMES,
        "seconds": SECONDS,
        "cases": cases,
        "required_bars": required,
        "aliases": aliases,
        "searches": searches,
        "rounding": rounding,
        "conditions": conditions,
        "evaluations": evaluations,
        "condition_help": parse._CONDITION_HELP,
    }


def record_registry(registry, descriptions, compute, defaults, originals) -> dict:
    specs = []
    for spec in registry.REGISTRY.values():
        specs.append({
            "name": spec.name,
            "category": spec.category,
            "inputs": list(spec.inputs),
            "outputs": list(spec.outputs),
            "params": [
                {"name": p.name, "kind": p.kind, "default": p.default,
                 "enum": list(p.enum) if p.enum else None, "required": p.required}
                for p in spec.params.values()
            ],
            "status": spec.status,
            "alias_of": spec.alias_of,
            "warmup": spec.warmup,
            "note": spec.note,
            "series_args": list(spec.series_args),
            "needs_second_series": spec.needs_second_series,
            "conditional_outputs": {k: list(v) for k, v in spec.conditional_outputs.items()} if spec.conditional_outputs else None,
            "description": descriptions.describe(spec.name),
            "brief": compute.spec_to_dict(spec),
            "verbose": compute.spec_to_dict(spec, verbose=True),
        })
    return {
        "openalgo": __import__("openalgo").__version__,
        "categories": list(registry.CATEGORIES),
        "category_counts": registry.CATEGORY_COUNTS,
        "required_param_defaults": defaults,
        "web_overrides": originals,
        "specs": specs,
    }


def dump(path: Path, payload) -> None:
    text = json.dumps(payload, separators=(",", ":"), allow_nan=False)
    path.write_text(text + "\n")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--web", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()

    from openalgo import ta

    with tempfile.TemporaryDirectory() as scratch:
        registry, descriptions, compute, parse, defaults = load_web(args.web.resolve(), Path(scratch))
        args.out.mkdir(parents=True, exist_ok=True)
        write_csv(args.out / "candles.csv", build_candles())
        candles = read_csv(args.out / "candles.csv")
        originals = enable_desktop_entries(ta, registry, defaults, candles["trend"][1])
        dump(
            args.out / "registry.json",
            record_registry(registry, descriptions, compute, defaults, originals),
        )
        for shape, payload in record_raw(ta, registry, defaults, candles).items():
            dump(args.out / f"raw_{shape}.json", payload)
        dump(args.out / "compute.json", record_compute(registry, compute, parse, candles))


if __name__ == "__main__":
    main()
