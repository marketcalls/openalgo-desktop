"""Record the web's chart geometry on the web tests' bars, as the desktop's fixture.

Runs the web's `services/agent/chart_geometry.py` (pure functions, no I/O) on
the bars the web's chart tests build (`test/test_agent_chart_surface.py`:
`zigzag_closes`, the toolkit's `history_rows` shape: open at the previous
close, high and low three points beyond the body) and writes, per case, the
significant swing points, the structure verdict, the support and resistance
levels, the contained and the touch-only rails, the demand and supply zones,
the consolidation, the candlestick hits and the chart patterns. The desktop's
`agent::chart_geometry` is pinned against it, number for number
(`chart_geometry::tests::the_web_geometry_is_reproduced`).

The web is never modified: the module's source is executed with its one
import (`utils.logging`) replaced by the standard logger, and no bytecode is
written. Only the standard library is needed.

Usage, from the desktop repository root:

    python3 -I scripts/agent/capture_web_chart_geometry.py ../openalgo \
        > src-tauri/tests/fixtures/agent/chart_geometry.json
"""
import json, sys, types
sys.dont_write_bytecode = True
web = sys.argv[1]
stub = types.ModuleType("utils.logging")
import logging
stub.get_logger = lambda name: logging.getLogger(name)
utils = types.ModuleType("utils"); utils.logging = stub
sys.modules["utils"] = utils; sys.modules["utils.logging"] = stub
src = open(web + "/services/agent/chart_geometry.py").read()
geom = types.ModuleType("chart_geometry"); geom.__file__ = "chart_geometry.py"
sys.modules["chart_geometry"] = geom
exec(compile(src, "chart_geometry.py", "exec"), geom.__dict__)

DAY = 86400.0
START = 1_785_000_000.0

def zigzag(levels, leg):
    out = [levels[0]]
    for a, b in zip(levels, levels[1:]):
        for step in range(1, leg + 1):
            out.append(a + (b - a) * step / leg)
    return out

def history_bars(closes):
    times, opens, highs, lows = [], [], [], []
    for i, c in enumerate(closes):
        p = closes[i - 1] if i else c
        times.append(START + i * DAY); opens.append(p)
        highs.append(max(p, c) + 3.0); lows.append(min(p, c) - 3.0)
    return geom.Bars(tuple(times), tuple(opens), tuple(highs), tuple(lows), tuple(closes))

def pv(p):
    return [p.index, p.price]

def fit(f):
    return None if f is None else {"from": f["from"].index, "to": f["to"].index,
        "slope": f["slope"], "touches": f["touches"], "r2": f["r2"], "breaks": f["breaks"]}

cases = {
    "web_closes": zigzag([100, 130, 100, 130, 100, 130, 105, 128, 115], 8),
    "converging": zigzag([100, 140, 105, 136, 110, 132, 115, 128, 118], 8),
    "lower_highs": zigzag([80, 70, 121, 100, 119, 98, 117, 60, 91, 55], 6),
}
out = {"start": START, "day": DAY, "cases": {}}
for name, closes in cases.items():
    bars = history_bars(closes)
    lo, hi = 0, len(bars) - 1
    highs, lows = geom.significant_pivots(bars, lo, hi)
    out["cases"][name] = {
        "closes": closes,
        "significant_highs": [pv(p) for p in highs],
        "significant_lows": [pv(p) for p in lows],
        "structure": geom.structure(highs, lows),
        "levels": geom.support_resistance(bars, lo, hi, reference=115.0),
        "resistance": fit(geom.fit_line(highs, bars=bars, side="resistance")),
        "support": fit(geom.fit_line(lows, bars=bars, side="support")),
        "loose_resistance": fit(geom.fit_line(highs)),
        "demand": geom.zone(bars, "demand", lo, hi),
        "supply": geom.zone(bars, "supply", lo, hi),
        "range": geom.consolidation(bars, lo, hi),
        "candles": [[h["name"], h["bias"], h["index"], h["price"], h["bars"]]
                    for h in geom.candlestick_patterns(bars, 30, lo, hi)],
        "chart_patterns": [[p["name"], p["bias"], [[a["time"], a["price"]] for a in p["anchors"]]]
                           for p in geom.chart_patterns(bars, highs, lows, lo, hi)],
    }
print(json.dumps(out, indent=1))
