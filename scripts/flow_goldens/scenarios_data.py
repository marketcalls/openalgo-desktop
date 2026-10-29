"""Data nodes (lane nodes-data)."""

from lib import (
    DEPTH, EXPIRY, FUNDS, HOLDINGS, HOLIDAYS, INTERVALS, MARGIN, MULTIQUOTES, OPTION_CHAIN, OPTION_SYMBOL,
    ORDER_STATUS, ORDERBOOK, POSITIONS, POSITIONS_EMPTY, QUOTE, SESSION_EXPIRED, SYMBOL, SYNTHETIC, TIMINGS,
    TRADEBOOK, daily_bars, err, intraday_bars, node, ok, reply,
)

F = "data"
ANY = {"$any": "string"}

# --- getQuote / getDepth / getOrderStatus -----------------------------------------

node(F, "getQuote", "happy", {"symbol": "SBIN", "exchange": "NSE", "outputVariable": "q"},
     script=[reply("quotes", QUOTE)], parity=["N-getQuote-01", "N-getQuote-02", "N-getQuote-03", "CL-12"])
node(F, "getQuote", "default_exchange_lowercase_symbol", {"symbol": "sbin"}, script=[reply("quotes", QUOTE)],
     parity=["N-getQuote-01"])
node(F, "getQuote", "interpolated_symbol", {"symbol": "{{webhook.symbol}}", "exchange": "{{webhook.exchange}}",
                                            "outputVariable": "q"},
     vars_in={"webhook": {"symbol": "INFY", "exchange": "NSE"}}, script=[reply("quotes", QUOTE)], parity=["N-getQuote-01"])
node(F, "getQuote", "unresolved_symbol_sent_as_text", {"symbol": "{{sym}}", "outputVariable": "q"},
     script=[reply("quotes", err("Symbol {{sym}} not found on NSE", 404))], parity=["N-dataCommon-09", "N-dataCommon-02"])
node(F, "getQuote", "failure_stored_and_stops", {"symbol": "SBIN", "outputVariable": "q"},
     script=[reply("quotes", SESSION_EXPIRED)], parity=["N-getQuote-03", "N-dataCommon-02", "N-dataCommon-03"])
node(F, "getQuote", "analyzer_live_data", {"symbol": "SBIN", "outputVariable": "q"}, analyzer=True,
     script=[reply("quotes", QUOTE)], parity=["N-dataCommon-05"])
node(F, "getDepth", "happy", {"symbol": "SBIN", "exchange": "NSE", "outputVariable": "d"},
     script=[reply("depth", DEPTH)], parity=["N-getDepth-01", "N-getDepth-02", "N-getDepth-03"])
node(F, "getDepth", "failure", {"symbol": "SBIN", "outputVariable": "d"}, script=[reply("depth", SESSION_EXPIRED)],
     parity=["N-getDepth-03"])
node(F, "getOrderStatus", "happy", {"orderId": "{{orderResult.orderid}}", "outputVariable": "os"},
     vars_in={"orderResult": {"orderid": "250408000123456"}}, script=[reply("orderstatus", ORDER_STATUS)],
     parity=["N-getOrderStatus-01", "N-getOrderStatus-02", "N-getOrderStatus-03"])
node(F, "getOrderStatus", "empty_order_id", {"orderId": ""},
     script=[reply("orderstatus", err("Order ID is missing", 400))], parity=["N-getOrderStatus-01"])
node(F, "getOrderStatus", "failure", {"orderId": "1", "outputVariable": "os"},
     script=[reply("orderstatus", err("Order not found", 404))], parity=["N-getOrderStatus-03"])

# --- openPosition -----------------------------------------------------------------

node(F, "openPosition", "happy", {"symbol": "SBIN", "exchange": "NSE", "outputVariable": "pos"},
     script=[reply("positionbook", POSITIONS)], parity=["N-openPosition-01", "N-openPosition-02", "N-openPosition-03", "CL-15"])
node(F, "openPosition", "default_product_nfo", {"symbol": "NIFTY25NOV2624000CE", "exchange": "NFO", "outputVariable": "pos"},
     script=[reply("positionbook", POSITIONS)], parity=["N-openPosition-01", "C-08"])
node(F, "openPosition", "product_mismatch", {"symbol": "SBIN", "exchange": "NSE", "product": "CNC", "outputVariable": "pos"},
     script=[reply("positionbook", POSITIONS)], parity=["N-openPosition-02"])
node(F, "openPosition", "product_type_key", {"symbol": "TCS", "exchange": "NSE", "outputVariable": "pos"},
     script=[reply("positionbook", ok(data=[{"symbol": "TCS", "exchange": "NSE", "product_type": "MIS",
                                             "quantity": "7", "pnl": -12.25}]))], parity=["N-openPosition-02"])
node(F, "openPosition", "case_sensitive_symbol", {"symbol": "sbin", "outputVariable": "pos"},
     script=[reply("positionbook", POSITIONS)], parity=["N-openPosition-02"])
node(F, "openPosition", "empty_book", {"symbol": "SBIN", "outputVariable": "pos"},
     script=[reply("positionbook", POSITIONS_EMPTY)], parity=["N-openPosition-02"])
node(F, "openPosition", "book_fails", {"symbol": "SBIN", "outputVariable": "pos"},
     script=[reply("positionbook", SESSION_EXPIRED)], parity=["N-openPosition-02"])

# --- history ----------------------------------------------------------------------

D60 = daily_bars(60)
H = {"symbol": "SBIN", "exchange": "NSE", "interval": "D", "startDate": "2026-09-01", "endDate": "2026-10-28",
     "outputVariable": "h"}
node(F, "history", "explicit_range", H, script=[reply("history", ok(data=D60[-5:]))],
     parity=["N-history-01", "N-history-02", "N-history-03", "N-history-04", "CL-13"])
node(F, "history", "days_range", {"symbol": "SBIN", "exchange": "NSE", "interval": "5m", "days": 3, "outputVariable": "h"},
     script=[reply("history", ok(data=intraday_bars(5, 5)))], parity=["N-history-02", "N-dataCommon-10"])
node(F, "history", "days_as_text", {"symbol": "SBIN", "interval": "15m", "days": "{{n}}", "outputVariable": "h"},
     vars_in={"n": 2}, script=[reply("history", ok(data=intraday_bars(3, 15)))], parity=["N-history-02"])
node(F, "history", "no_range", {"symbol": "SBIN", "outputVariable": "h"}, script=[reply("history", ok(data=D60[-3:]))],
     parity=["N-history-01", "N-history-02", "N-history-05"])
node(F, "history", "range_clamped", {**H, "interval": "1m", "startDate": "2026-01-01", "endDate": "2026-10-28"},
     script=[reply("history", ok(data=intraday_bars(3, 1)))], parity=["N-history-02", "N-dataCommon-15"])
node(F, "history", "rows_trimmed", {**H, "startDate": "2025-06-01"}, script=[reply("history", ok(data=daily_bars(230)))],
     parity=["N-history-03"])
node(F, "history", "daily_1d", {**H, "interval": "1d"}, script=[reply("history", ok(data=D60[-2:]))],
     parity=["N-history-05", "F-564"],
     desktop_expected={"parity": "N-history-05",
                       "why": "decisions.md 4: the desktop maps the web panel's 1d to D before /api/v1/history.",
                       "fix": ["history_1d_is_d"]})
node(F, "history", "failure", H, script=[reply("history", err("Interval '2h' is not supported", 400))],
     parity=["N-history-04"])

# --- strategyPnl ------------------------------------------------------------------

LEGS = [
    {"strategy": "Golden Flow", "symbol": "SBIN", "exchange": "NSE", "product": "MIS", "quantity": 10.0,
     "average_price": 800.0, "realized_pnl": 50.0, "today_realized_pnl": 50.0},
    {"strategy": "Golden Flow", "symbol": "INFY", "exchange": "NSE", "product": "MIS", "quantity": 0.0,
     "average_price": 0.0, "realized_pnl": -20.5, "today_realized_pnl": 0.0},
    {"strategy": "Golden Flow", "symbol": "TCS", "exchange": "NSE", "product": "MIS", "quantity": -5.0,
     "average_price": 3500.0, "realized_pnl": 0.0, "today_realized_pnl": 0.0},
    {"strategy": "other", "symbol": "SBIN", "exchange": "NSE", "product": "MIS", "quantity": 1.0,
     "average_price": 1.0, "realized_pnl": 1.0, "today_realized_pnl": 1.0},
]
node(F, "strategyPnl", "workflow_name_default", {"outputVariable": "pnl"}, env={"strategy_book": LEGS},
     script=[reply("positionbook", POSITIONS)],
     parity=["N-strategyPnl-01", "N-strategyPnl-02", "N-strategyPnl-03", "N-strategyPnl-04", "N-strategyPnl-05"])
node(F, "strategyPnl", "named_strategy_all_priced", {"strategy": "other", "outputVariable": "pnl"},
     env={"strategy_book": LEGS}, script=[reply("positionbook", POSITIONS)], parity=["N-strategyPnl-03"])
node(F, "strategyPnl", "unknown_strategy", {"strategy": "nobody", "outputVariable": "pnl"}, env={"strategy_book": LEGS},
     script=[reply("positionbook", POSITIONS)], parity=["N-strategyPnl-03"])
node(F, "strategyPnl", "last_price_fallback", {"outputVariable": "pnl"}, env={"strategy_book": LEGS[:1]},
     script=[reply("positionbook", ok(data=[{"symbol": "SBIN", "exchange": "NSE", "product": "MIS", "last_price": 801.35,
                                             "quantity": 10}]))], parity=["N-strategyPnl-03"])
node(F, "strategyPnl", "book_unavailable", {"outputVariable": "pnl"},
     env={"strategy_book": {"unavailable": "Strategy book is not initialized; per-strategy P&L is unavailable"}},
     parity=["N-strategyPnl-02", "N-strategyPnl-05"])
node(F, "strategyPnl", "positions_unavailable", {"outputVariable": "pnl"}, env={"strategy_book": LEGS},
     script=[reply("positionbook", SESSION_EXPIRED)], parity=["N-strategyPnl-02", "N-strategyPnl-05"])

# --- priorPeriodOhlc --------------------------------------------------------------

PP = {"symbol": "SBIN", "exchange": "NSE", "period": "previous_day", "outputVariable": "pd"}
node(F, "priorPeriodOhlc", "previous_day", PP, script=[reply("history", ok(data=daily_bars(8, include_today=(2026, 10, 29))))],
     parity=["N-priorPeriodOhlc-01", "N-priorPeriodOhlc-02", "N-priorPeriodOhlc-03", "N-priorPeriodOhlc-04", "N-dataCommon-12"])
node(F, "priorPeriodOhlc", "previous_hour", {**PP, "period": "previous_hour"},
     script=[reply("history", ok(data=intraday_bars(6, 60, last=(2026, 10, 29, 10, 15))))], parity=["N-priorPeriodOhlc-03"])
node(F, "priorPeriodOhlc", "previous_week", {**PP, "period": "previous_week"},
     script=[reply("history", ok(data=daily_bars(15)))], parity=["N-priorPeriodOhlc-03"])
node(F, "priorPeriodOhlc", "previous_month", {**PP, "period": "previous_month"},
     script=[reply("history", ok(data=daily_bars(42)))], parity=["N-priorPeriodOhlc-03"])
node(F, "priorPeriodOhlc", "default_period", {"symbol": "SBIN"}, script=[reply("history", ok(data=daily_bars(5)))],
     parity=["N-priorPeriodOhlc-01"])
node(F, "priorPeriodOhlc", "missing_symbol", {"period": "previous_day"}, parity=["N-priorPeriodOhlc-01"])
node(F, "priorPeriodOhlc", "only_today", PP, script=[reply("history", ok(data=daily_bars(1, end=(2026, 10, 29))))],
     parity=["N-priorPeriodOhlc-03", "N-priorPeriodOhlc-04"])
node(F, "priorPeriodOhlc", "previous_hour_only_current_hour", {**PP, "period": "previous_hour"},
     script=[reply("history", ok(data=intraday_bars(1, 60, last=(2026, 10, 29, 10, 0))))], parity=["N-priorPeriodOhlc-03"])
node(F, "priorPeriodOhlc", "no_records", PP, script=[reply("history", ok(data=[]))], parity=["N-priorPeriodOhlc-03"])
node(F, "priorPeriodOhlc", "unknown_period", {**PP, "period": "previous_year"},
     script=[reply("history", ok(data=daily_bars(5)))], parity=["N-priorPeriodOhlc-03"])
node(F, "priorPeriodOhlc", "history_fails", PP, script=[reply("history", err("Broker does not support D", 400))],
     parity=["N-dataCommon-13"])
node(F, "priorPeriodOhlc", "history_fails_db_source", {**PP, "source": "db"},
     script=[reply("history", err("No data in Historify for SBIN", 404))], parity=["N-dataCommon-13"])
node(F, "priorPeriodOhlc", "symbol_interpolated_once", {**PP, "symbol": "{{webhook.symbol}}"},
     vars_in={"webhook": {"symbol": "{{secret}}"}, "secret": "SBIN"},
     script=[reply("history", ok(data=daily_bars(5)))], parity=["N-dataCommon-14"],
     desktop_expected={
         "parity": "N-dataCommon-14",
         "why": "WEB-DEFECT: the symbol is interpolated twice, so a payload value holding {{...}} is resolved again; the desktop interpolates once and sends the text as received.",
         "fix": ["symbol_once"],
     })

# --- barOffset --------------------------------------------------------------------

BOF = {"symbol": "SBIN", "exchange": "NSE", "interval": "D", "offsetBars": 0, "outputVariable": "bar1"}
node(F, "barOffset", "latest_closed_daily", BOF,
     script=[reply("history", ok(data=daily_bars(8, include_today=(2026, 10, 29))))],
     parity=["N-barOffset-01", "N-barOffset-02", "N-barOffset-03", "N-barOffset-04"])
node(F, "barOffset", "three_back", {**BOF, "offsetBars": 3}, script=[reply("history", ok(data=daily_bars(12)))],
     parity=["N-barOffset-02"])
node(F, "barOffset", "intraday_forming_dropped", {**BOF, "interval": "5m", "offsetBars": 1},
     script=[reply("history", ok(data=intraday_bars(10, 5, last=(2026, 10, 29, 10, 13))))], parity=["N-barOffset-03"])
node(F, "barOffset", "negative_offset_is_zero", {**BOF, "offsetBars": -4}, script=[reply("history", ok(data=daily_bars(6)))],
     parity=["N-barOffset-01"])
node(F, "barOffset", "offset_clamped", {**BOF, "offsetBars": 500}, script=[reply("history", ok(data=daily_bars(30)))],
     parity=["N-barOffset-01", "N-barOffset-04", "N-dataCommon-15"])
node(F, "barOffset", "not_enough_history", {**BOF, "offsetBars": 10}, script=[reply("history", ok(data=daily_bars(4)))],
     parity=["N-barOffset-04"])
node(F, "barOffset", "weekly_forming_week", {**BOF, "interval": "W"},
     script=[reply("history", ok(data=daily_bars(6, end=(2026, 10, 26))))], parity=["N-barOffset-03", "IND-062"],
     note="Weekly bars stamped at each week's Monday here (built from the daily generator); the last is the current week.")
node(F, "barOffset", "missing_symbol", {"interval": "D"}, parity=["N-barOffset-01"])
node(F, "barOffset", "history_fails", BOF, script=[reply("history", err("Invalid symbol", 400))], parity=["N-dataCommon-13"])

# --- indicator --------------------------------------------------------------------

IN = {"indicatorName": "sma", "symbol": "SBIN", "exchange": "NSE", "interval": "D", "lookbackBars": 50,
      "tailBars": 5, "params": "{\"period\": 10}", "outputVariable": "ind"}
D80 = daily_bars(80)
node(F, "indicator", "sma_history", IN, script=[reply("history", ok(data=D80))],
     parity=["N-indicator-01", "N-indicator-07", "N-indicator-08", "N-dataCommon-12"])
node(F, "indicator", "rsi_offset", {**IN, "indicatorName": "rsi", "params": "{\"period\": 14}", "offsetBars": 2},
     script=[reply("history", ok(data=D80))], parity=["N-indicator-08"])
node(F, "indicator", "macd_multi_output", {**IN, "indicatorName": "macd", "params": ""},
     script=[reply("history", ok(data=D80))], parity=["N-indicator-08", "N-indicator-02"])
node(F, "indicator", "params_object_normalised", {**IN, "params": {"period": 5}}, script=[reply("history", ok(data=D80))],
     parity=["N-indicator-02"])
node(F, "indicator", "params_invalid_json", {**IN, "params": "{period: 5}"}, parity=["N-indicator-02"])
node(F, "indicator", "params_not_object", {**IN, "params": "[5]"}, parity=["N-indicator-02"])
node(F, "indicator", "params_interpolated", {**IN, "params": "{\"period\": {{n}}}"}, vars_in={"n": 7},
     script=[reply("history", ok(data=D80))], parity=["N-indicator-02"])
node(F, "indicator", "unknown_indicator", {**IN, "indicatorName": "superduper"}, script=[reply("history", ok(data=D80))],
     parity=["N-indicator-07"])
node(F, "indicator", "unsupported_indicator", {**IN, "indicatorName": "vi", "params": ""},
     script=[reply("history", ok(data=D80))], parity=["N-indicator-07", "IND-033"])
node(F, "indicator", "nested_trailing_nan", {"indicatorName": "sma", "sourceSeries": "{{prices}}",
                                             "params": "{\"period\": 2}", "tailBars": 2, "outputVariable": "nn"},
     vars_in={"prices": [1.0, 2.0, 3.0, "nan"]}, parity=["N-indicator-05", "IND-049"])
node(F, "indicator", "nested_all_nan_no_value", {"indicatorName": "sma", "sourceSeries": "{{prices}}",
                                                 "params": "{\"period\": 2}", "tailBars": 2, "outputVariable": "nn"},
     vars_in={"prices": ["nan", "nan", "nan"]}, parity=["N-indicator-09"])
node(F, "indicator", "ichimoku_lagging_span", {**IN, "indicatorName": "ichimoku", "params": "", "tailBars": 3},
     script=[reply("history", ok(data=D80))], parity=["N-indicator-08", "IND-049"])
node(F, "indicator", "two_series_refused", {**IN, "indicatorName": "crossover"}, script=[reply("history", ok(data=D80))],
     parity=["N-indicator-07"])
node(F, "indicator", "missing_symbol", {k: v for k, v in IN.items() if k != "symbol"}, parity=["N-indicator-07"])
node(F, "indicator", "history_fails", IN, script=[reply("history", err("Too many requests", 429))],
     parity=["N-dataCommon-13"])
node(F, "indicator", "no_history_rows", IN, script=[reply("history", ok(data=[]))], parity=["N-indicator-07"])
node(F, "indicator", "unknown_param_key", {**IN, "params": "{\"window\": 5}"}, script=[reply("history", ok(data=D80))],
     parity=["N-indicator-11", "IND-043"],
     desktop_expected={
         "parity": "IND-043",
         "why": "WEB-DEFECT: a TypeError from the library escapes the node and aborts the whole run; the desktop fails only this node with `Indicator error: {e}` (the text after the prefix is the desktop's own).",
         "fix": ["indicator_any_error"],
         "set": {"outputs": {"status": "error", "message": ANY},
                 "logs": [["info", "Computing sma for SBIN (D, 50 bars)"], ["error", {"$prefix": "Indicator error: "}],
                          ["error", {"$prefix": "Stopping this branch: indicator node failed ("}]],
                 "error": {"errors": [{"node": "n1", "type": "indicator", "message": ANY}], "abort": None}},
     })
NEST = [101.0, 102.5, 101.75, 103.0, 104.25, 103.5, 105.0, 106.0, 105.5, 107.25, 108.0, 107.5]
node(F, "indicator", "nested_numbers", {"indicatorName": "sma", "sourceSeries": "{{prices}}", "params": "{\"period\": 3}",
                                        "tailBars": 3, "outputVariable": "nested"},
     vars_in={"prices": NEST}, parity=["N-indicator-04", "N-indicator-05", "N-indicator-06"])
node(F, "indicator", "nested_from_indicator_series", {"indicatorName": "ema", "sourceSeries": "{{rsi1.series}}",
                                                      "params": "{\"period\": 2}", "tailBars": 2, "outputVariable": "n2"},
     vars_in={"rsi1": {"series": [{"value": v} for v in NEST[:6]] + [{"value": None}]}}, parity=["N-indicator-05"])
node(F, "indicator", "nested_from_history_close", {"indicatorName": "sma", "sourceSeries": "{{h.data}}",
                                                   "params": "{\"period\": 2}", "tailBars": 2, "outputVariable": "n3"},
     vars_in={"h": {"data": daily_bars(6)}}, parity=["N-indicator-05"])
node(F, "indicator", "nested_source_field", {"indicatorName": "sma", "sourceSeries": "{{h.data}}", "sourceField": "high",
                                             "params": "{\"period\": 2}", "tailBars": 2, "outputVariable": "n4"},
     vars_in={"h": {"data": daily_bars(6)}}, parity=["N-indicator-05"])
node(F, "indicator", "nested_json_text", {"indicatorName": "sma", "sourceSeries": "[1, 2, 3, \"4\", 5]",
                                          "params": "{\"period\": 2}", "tailBars": 2, "outputVariable": "n5"},
     parity=["N-indicator-04", "N-indicator-05"])
node(F, "indicator", "nested_array_in_json", {"indicatorName": "sma", "sourceSeries": [3, 4, 5, 6],
                                              "params": "{\"period\": 2}", "tailBars": 2, "outputVariable": "n6"},
     parity=["N-indicator-04"])
node(F, "indicator", "nested_not_json", {"indicatorName": "sma", "sourceSeries": "{{missing.series}}"},
     parity=["N-indicator-04"])
node(F, "indicator", "nested_not_array", {"indicatorName": "sma", "sourceSeries": "{{cfg}}"}, vars_in={"cfg": {"a": 1}},
     parity=["N-indicator-04"])
node(F, "indicator", "nested_non_numeric", {"indicatorName": "sma", "sourceSeries": "{{prices}}"},
     vars_in={"prices": [1.0, 2.0, "n/a"]}, parity=["N-indicator-05"])
node(F, "indicator", "nested_too_short", {"indicatorName": "sma", "sourceSeries": "{{prices}}"},
     vars_in={"prices": [1.0]}, parity=["N-indicator-06"])
node(F, "indicator", "nested_multi_input_refused", {"indicatorName": "atr", "sourceSeries": "{{prices}}"},
     vars_in={"prices": NEST}, parity=["N-indicator-06"])
node(F, "indicator", "all_null_result", {**IN, "params": "{\"period\": 60}", "lookbackBars": 30},
     script=[reply("history", ok(data=daily_bars(30)))], parity=["N-indicator-09"])
node(F, "indicator", "whitespace_source_series", {**IN, "sourceSeries": "  "}, script=[reply("history", ok(data=D80))],
     parity=["N-indicator-03"],
     desktop_expected={
         "parity": "N-indicator-03",
         "why": "WEB-DEFECT: a whitespace-only sourceSeries enters nested mode; the desktop treats it as absent (as the validator does) and computes from history.",
         "fix": ["blank_source_series"],
     })

# --- account books ----------------------------------------------------------------

for t, ep, rep in (("orderBook", "orderbook", ORDERBOOK), ("tradeBook", "tradebook", TRADEBOOK),
                   ("positionBook", "positionbook", POSITIONS), ("holdings", "holdings", HOLDINGS),
                   ("funds", "funds", FUNDS)):
    node(F, t, "happy", {"outputVariable": "book"}, script=[reply(ep, rep)],
         parity=[f"N-{t}-01", "CL-14", "N-dataCommon-02"])
    node(F, t, "failure", {"outputVariable": "book"}, script=[reply(ep, SESSION_EXPIRED)], parity=[f"N-{t}-01", "N-dataCommon-03"])
    node(F, t, "analyzer", {"outputVariable": "book"}, analyzer=True, script=[reply(ep, {**rep, "mode": "analyze"})],
         parity=["N-dataCommon-05"])

# --- symbol / optionSymbol / expiry / intervals / multiQuotes ----------------------

node(F, "symbol", "happy", {"symbol": "SBIN", "exchange": "NSE", "outputVariable": "info"}, script=[reply("symbol", SYMBOL)],
     parity=["N-symbol-01"])
node(F, "symbol", "defaults", {}, script=[reply("symbol", err("Symbol not found", 404))], parity=["N-symbol-01"])
OS = {"underlying": "NIFTY", "exchange": "NSE_INDEX", "expiryDate": "25NOV26", "offset": "ATM", "optionType": "CE",
      "outputVariable": "opt"}
node(F, "optionSymbol", "happy", OS, script=[reply("optionsymbol", OPTION_SYMBOL)],
     parity=["N-optionSymbol-02", "N-optionSymbol-03", "N-common-05", "N-common-06"])
node(F, "optionSymbol", "expiry_embedded_in_underlying", {**OS, "underlying": "NIFTY25NOV26FUT", "expiryDate": ""},
     script=[reply("optionsymbol", OPTION_SYMBOL)], parity=["N-optionSymbol-01", "C-12"])
node(F, "optionSymbol", "no_expiry", {**OS, "expiryDate": ""}, parity=["N-optionSymbol-01", "N-common-06"])
node(F, "optionSymbol", "bad_expiry", {**OS, "expiryDate": "2026-11-25"}, parity=["N-optionSymbol-01"])
node(F, "optionSymbol", "blank_underlying", {**OS, "underlying": " "}, parity=["N-optionSymbol-01", "N-common-06"])
node(F, "optionSymbol", "unresolved_underlying", {**OS, "underlying": "{{webhook.u}}", "expiryDate": "{{webhook.e}}"},
     parity=["N-optionSymbol-01", "N-common-06"])
node(F, "optionSymbol", "interpolated", {**OS, "underlying": "{{webhook.u}}", "expiryDate": "{{exp.data[1]}}",
                                         "offset": "{{webhook.o}}"},
     vars_in={"webhook": {"u": "BANKNIFTY", "o": "ITM2"}, "exp": {"data": ["04NOV26", "25nov26"]}},
     script=[reply("optionsymbol", OPTION_SYMBOL)], parity=["N-optionSymbol-02"])
node(F, "optionSymbol", "failure", OS, script=[reply("optionsymbol", err("Invalid offset", 400))], parity=["N-optionSymbol-03"])
node(F, "expiry", "options", {"symbol": "NIFTY", "exchange": "NFO", "instrumenttype": "options", "outputVariable": "exp"},
     script=[reply("expiry", EXPIRY)], parity=["N-expiry-01"])
node(F, "expiry", "futures_upper_case", {"symbol": "CRUDEOIL", "exchange": "MCX", "instrumenttype": "FUTURES",
                                         "outputVariable": "exp"},
     script=[reply("expiry", ok(data=["17-NOV-26", "16-DEC-26"]))], parity=["N-expiry-01"])
node(F, "expiry", "unknown_type_is_options", {"instrumenttype": "weeklies"}, script=[reply("expiry", EXPIRY)],
     parity=["N-expiry-01"])
node(F, "expiry", "failure", {"symbol": "NIFTY"}, script=[reply("expiry", SESSION_EXPIRED)], parity=["N-expiry-01"])
node(F, "intervals", "happy", {"outputVariable": "iv"}, script=[reply("intervals", INTERVALS)], parity=["N-intervals-01"])
node(F, "intervals", "failure", {}, script=[reply("intervals", SESSION_EXPIRED)], parity=["N-intervals-01"])
node(F, "multiQuotes", "csv", {"symbols": "SBIN, INFY ,,", "exchange": "NSE", "outputVariable": "mq"},
     script=[reply("multiquotes", MULTIQUOTES)], parity=["N-multiQuotes-01", "CL-23"])
node(F, "multiQuotes", "interpolated", {"symbols": "{{watch}}", "outputVariable": "mq"}, vars_in={"watch": "SBIN,INFY"},
     script=[reply("multiquotes", MULTIQUOTES)], parity=["N-multiQuotes-01"])
node(F, "multiQuotes", "empty", {"symbols": ""},
     script=[reply("multiquotes", err({"symbols": ["Shorter than minimum length 1."]}, 400))], parity=["N-multiQuotes-01"])

# --- optionChain / syntheticFuture --------------------------------------------------

OC = {"underlying": "NIFTY", "exchange": "NSE_INDEX", "expiryDate": "25NOV26", "strikeCount": 5, "outputVariable": "chain"}
node(F, "optionChain", "happy", OC, script=[reply("optionchain", OPTION_CHAIN)],
     parity=["N-optionChain-01", "N-optionChain-02", "CL-16"])
node(F, "optionChain", "default_strike_count", {k: v for k, v in OC.items() if k != "strikeCount"},
     script=[reply("optionchain", OPTION_CHAIN)], parity=["N-optionChain-01"])
node(F, "optionChain", "embedded_expiry", {**OC, "underlying": "NIFTY25NOV26FUT", "expiryDate": ""},
     script=[reply("optionchain", OPTION_CHAIN)], parity=["N-optionChain-04", "C-12"])
node(F, "optionChain", "embedded_and_explicit_expiry", {**OC, "underlying": "NIFTY25NOV26FUT", "expiryDate": "30DEC26"},
     script=[reply("optionchain", OPTION_CHAIN)], parity=["N-optionChain-03"],
     note="The node sends both; the web's chain service then lets the embedded expiry win (N-optionChain-03, fixed below the node).")
node(F, "optionChain", "missing_expiry", {**OC, "expiryDate": ""}, parity=["N-optionChain-01", "N-common-06"])
node(F, "optionChain", "failure", OC, script=[reply("optionchain", err("No strikes found for NIFTY expiring 25NOV26. Please check expiry date or update master contract.", 404))],
     parity=["N-optionChain-02"])
SF = {"underlying": "NIFTY", "exchange": "NSE_INDEX", "expiryDate": "25NOV26", "outputVariable": "syn"}
node(F, "syntheticFuture", "happy", SF, script=[reply("syntheticfuture", SYNTHETIC)], parity=["N-syntheticFuture-01"])
node(F, "syntheticFuture", "embedded_expiry_still_needs_date", {**SF, "underlying": "NIFTY25NOV26FUT", "expiryDate": ""},
     parity=["N-common-06"])
node(F, "syntheticFuture", "failure", SF, script=[reply("syntheticfuture", SESSION_EXPIRED)], parity=["N-syntheticFuture-01"])

# --- calendar / holidays / timings ------------------------------------------------

HOL = {"2026": [
    {"date": "2026-11-09", "holiday_type": "TRADING_HOLIDAY", "closed_exchanges": ["NSE", "BSE", "NFO"]},
    {"date": "2026-11-08", "holiday_type": "SPECIAL_SESSION", "closed_exchanges": []},
    {"date": "2026-11-21", "holiday_type": "SETTLEMENT_HOLIDAY", "closed_exchanges": []},
    {"date": "2026-10-02", "holiday_type": "TRADING_HOLIDAY", "closed_exchanges": ["NSE", "BSE", "NFO", "MCX"]},
]}
node(F, "calendar", "session_date_default", {"outputVariable": "cal"}, env={"calendar_holidays": HOL},
     parity=["N-calendar-01", "N-calendar-02"])
node(F, "calendar", "before_session_boundary", {"outputVariable": "cal"}, env={"calendar_holidays": HOL},
     clock="2026-11-02T02:30:00", parity=["N-calendar-02", "E-12"])
node(F, "calendar", "first_day_after_holiday_monday", {"date": "2026-11-10", "outputVariable": "cal"},
     env={"calendar_holidays": HOL}, parity=["N-calendar-03"])
node(F, "calendar", "special_session_sunday", {"date": "2026-11-08", "outputVariable": "cal"},
     env={"calendar_holidays": HOL}, parity=["N-calendar-03"])
node(F, "calendar", "settlement_holiday_saturday", {"date": "2026-11-21", "outputVariable": "cal"},
     env={"calendar_holidays": HOL}, parity=["N-calendar-05"],
     desktop_expected={
         "parity": "N-calendar-05",
         "why": "WEB-DEFECT: a SETTLEMENT_HOLIDAY (no closed exchanges) is classed as a special session, so this Saturday reads as a trading day; the desktop treats only SPECIAL_SESSION as special.",
         "fix": ["calendar_kinds"],
     })
node(F, "calendar", "date_from_variable", {"date": "{{webhook.day}}T09:15:00", "outputVariable": "cal"},
     vars_in={"webhook": {"day": "2026-12-31"}}, env={"calendar_holidays": {"2026": [], "2027": []}}, parity=["N-calendar-01"])
node(F, "calendar", "bad_date", {"date": "31/12/2026"}, parity=["N-calendar-04"])
node(F, "calendar", "holidays_raise", {"date": "2026-11-09", "outputVariable": "cal"},
     env={"calendar_holidays": {"2026": "raise"}}, parity=["N-calendar-04"])
node(F, "calendar", "holidays_unavailable", {"date": "2026-11-09", "outputVariable": "cal"},
     env={"calendar_holidays": {"2026": "unavailable"}}, parity=["N-calendar-03"])
node(F, "holidays", "year", {"year": 2026, "outputVariable": "hol"}, script=[reply("market/holidays", HOLIDAYS)],
     parity=["N-holidays-01", "CL-19"])
node(F, "holidays", "current_year", {"outputVariable": "hol"}, script=[reply("market/holidays", HOLIDAYS)],
     parity=["N-holidays-01"])
node(F, "holidays", "year_text", {"year": "{{y}}"}, vars_in={"y": "2027"}, script=[reply("market/holidays", HOLIDAYS)],
     parity=["N-holidays-01"])
node(F, "holidays", "failure", {"year": 1999}, script=[reply("market/holidays", err("Year must be between 2020 and 2050", 400))],
     parity=["N-holidays-01", "N-holidays-02"])
node(F, "timings", "date", {"date": "2026-11-02", "outputVariable": "tm"}, script=[reply("market/timings", TIMINGS)],
     parity=["N-timings-01", "CL-19"])
node(F, "timings", "today", {"outputVariable": "tm"}, script=[reply("market/timings", TIMINGS)],
     parity=["N-timings-01", "N-dataCommon-10"])
node(F, "timings", "failure", {"date": "tomorrow"}, script=[reply("market/timings", err("Invalid date format. Use YYYY-MM-DD", 400))],
     parity=["N-timings-01", "N-timings-02"])

# --- margin -----------------------------------------------------------------------

MG = {"symbol": "SBIN", "exchange": "NSE", "quantity": 10, "price": 812.5, "product": "MIS", "action": "BUY",
      "priceType": "LIMIT", "outputVariable": "mg"}
node(F, "margin", "single_position", MG, script=[reply("margin", MARGIN)], parity=["N-margin-01", "N-margin-02", "CL-20"])
node(F, "margin", "single_position_defaults", {"symbol": "NIFTY25NOV2624000CE", "exchange": "NFO"},
     script=[reply("margin", MARGIN)], parity=["N-margin-01", "C-08"])
BASKET_POS = [{"symbol": "nifty25nov2624000ce", "exchange": "NFO", "action": "sell", "quantity": 65, "product": "NRML",
               "pricetype": "MARKET"},
              {"symbol": "NIFTY25NOV2624500CE", "exchange": "NFO", "action": "BUY", "quantity": "65", "product": "NRML",
               "pricetype": "LIMIT", "price": "42.5", "triggerPrice": 0, "note": "hedge"}]
import json as _json  # noqa: E402

node(F, "margin", "basket_json", {"positionsJson": _json.dumps(BASKET_POS), "outputVariable": "mg"},
     script=[reply("margin", MARGIN)], parity=["N-margin-03", "N-margin-04"])
node(F, "margin", "basket_from_variable", {"positionsJson": "{{legs}}", "outputVariable": "mg"}, vars_in={"legs": BASKET_POS},
     script=[reply("margin", MARGIN)], parity=["N-margin-03"])
node(F, "margin", "basket_positions_key_object", {"positions": BASKET_POS[0]}, script=[reply("margin", MARGIN)],
     parity=["N-margin-03"])
node(F, "margin", "basket_unresolved", {"positionsJson": "{{legs}}"}, parity=["N-margin-03"])
node(F, "margin", "basket_empty_after_interpolation", {"positionsJson": "{{blank}}"}, vars_in={"blank": "  "},
     parity=["N-margin-03"])
node(F, "margin", "basket_blank_falls_back", {**MG, "positionsJson": ""}, script=[reply("margin", MARGIN)],
     parity=["N-margin-06"],
     desktop_expected={
         "parity": "N-margin-06",
         "why": "WEB-DEFECT: a blank positionsJson (what the editor writes for an emptied basket) fails the run; the desktop treats it as absent and prices the single position.",
         "fix": ["margin_blank_absent"],
     })
node(F, "margin", "basket_invalid_json", {"positionsJson": "[{\"symbol\": \"SBIN\",]"}, parity=["N-margin-03"])
node(F, "margin", "basket_not_array", {"positionsJson": "42"}, parity=["N-margin-03"])
node(F, "margin", "basket_empty_array", {"positionsJson": "[]"}, parity=["N-margin-03"])
node(F, "margin", "basket_non_object", {"positionsJson": "[{\"symbol\": \"SBIN\"}, 3]"}, parity=["N-margin-03"])
for nm, pos in (
    ("position_bad_exchange", {**BASKET_POS[0], "exchange": "NYSE"}),
    ("position_quantity_zero", {**BASKET_POS[0], "quantity": 0}),
    ("position_missing_product", {k: v for k, v in BASKET_POS[0].items() if k != "product"}),
    ("position_limit_without_price", {**BASKET_POS[0], "pricetype": "LIMIT"}),
    ("position_unresolved_symbol", {**BASKET_POS[0], "symbol": "{{pick}}"}),
):
    node(F, "margin", nm, {"positionsJson": _json.dumps([pos])}, parity=["N-margin-04"])
node(F, "margin", "nothing_to_price", {"exchange": "NSE"}, parity=["N-margin-05"])
node(F, "margin", "failure", MG, script=[reply("margin", err("Margin calculation not supported by broker", 501))],
     parity=["N-margin-02"])


# IND-049 / IND-062 (decisions.md 10) and N-optionChain-03: written only where
# the fix changes what the web did.
import lib as _lib  # noqa: E402

_AUTO = {
    "indicator": ("IND-049", "decisions.md 10: an offset counts bars, not non-null values; a NaN bar reads None (IND-049).",
                  ["indicator_offset_counts_bars"]),
    "barOffset": ("IND-062", "decisions.md 10: a weekly or monthly bar of the current week or month is still forming and is skipped (IND-062).",
                  ["forming_week_month"]),
    "optionChain": ("N-optionChain-03", "decisions.md 10: an explicit expiryDate wins over one embedded in the underlying; the desktop client sends the base symbol with it.",
                    ["option_chain_explicit_expiry"]),
}


def apply_auto():
    for _s in _lib.SCENARIOS:
        if _s["type"] in _AUTO and _s["kind"] == "node":
            pid, why, fx_ = _AUTO[_s["type"]]
            if _s["desktop_expected"]:
                _s["desktop_expected"]["fix"] = list(_s["desktop_expected"].get("fix") or []) + fx_
            else:
                _s["desktop_expected"] = {"parity": pid, "why": why, "fix": fx_}

# --- {{...}} in the remaining fields ----------------------------------------------

_W = {"webhook": {"symbol": "INFY", "ex": "NSE", "n": "3", "day": "2026-11-02", "u": "NIFTY", "exp": "25NOV26",
                  "it": "futures", "qty": "50", "px": "1880.5"}}
node(F, "getDepth", "interpolated", {"symbol": "{{webhook.symbol}}", "exchange": "{{webhook.ex}}", "outputVariable": "d"},
     vars_in=_W, script=[reply("depth", DEPTH)], parity=["N-getDepth-01"])
node(F, "symbol", "interpolated", {"symbol": "{{webhook.symbol}}", "exchange": "{{webhook.ex}}"}, vars_in=_W,
     script=[reply("symbol", SYMBOL)], parity=["N-symbol-01"])
node(F, "expiry", "interpolated", {"symbol": "{{webhook.u}}", "exchange": "NFO", "instrumenttype": "{{webhook.it}}"},
     vars_in=_W, script=[reply("expiry", EXPIRY)], parity=["N-expiry-01"])
node(F, "openPosition", "interpolated", {"symbol": "{{webhook.symbol}}", "exchange": "{{webhook.ex}}", "outputVariable": "pos"},
     vars_in=_W, script=[reply("positionbook", POSITIONS)], parity=["N-openPosition-01"])
node(F, "optionChain", "interpolated", {"underlying": "{{webhook.u}}", "exchange": "NSE_INDEX", "expiryDate": "{{webhook.exp}}",
                                        "strikeCount": "{{webhook.n}}"},
     vars_in=_W, script=[reply("optionchain", OPTION_CHAIN)], parity=["N-optionChain-01"])
node(F, "syntheticFuture", "interpolated", {"underlying": "{{webhook.u}}", "exchange": "NSE_INDEX", "expiryDate": "{{webhook.exp}}"},
     vars_in=_W, script=[reply("syntheticfuture", SYNTHETIC)], parity=["N-syntheticFuture-01"])
node(F, "timings", "interpolated", {"date": "{{webhook.day}}"}, vars_in=_W, script=[reply("market/timings", TIMINGS)],
     parity=["N-timings-01"])
node(F, "margin", "single_position_interpolated", {"symbol": "{{webhook.symbol}}", "exchange": "{{webhook.ex}}",
                                                   "quantity": "{{webhook.qty}}", "price": "{{webhook.px}}", "action": "sell"},
     vars_in=_W, script=[reply("margin", MARGIN)], parity=["N-margin-01", "CL-20"])
node(F, "strategyPnl", "interpolated_strategy", {"strategy": "{{webhook.tag}}", "outputVariable": "pnl"},
     vars_in={"webhook": {"tag": "other"}}, env={"strategy_book": LEGS}, script=[reply("positionbook", POSITIONS)],
     parity=["N-strategyPnl-01"])
node(F, "barOffset", "interpolated", {**BOF, "symbol": "{{webhook.symbol}}", "offsetBars": "{{webhook.n}}"}, vars_in=_W,
     script=[reply("history", ok(data=daily_bars(12)))], parity=["N-barOffset-01"])
node(F, "history", "interpolated", {"symbol": "{{webhook.symbol}}", "exchange": "{{webhook.ex}}", "interval": "D",
                                    "startDate": "{{s}}", "endDate": "{{e}}", "outputVariable": "h"},
     vars_in={**_W, "s": "2026-10-01", "e": "2026-10-28"}, script=[reply("history", ok(data=D60[-3:]))], parity=["N-history-01"])
