"""Streaming, trigger and gate-adjacent nodes (lane triggers)."""

from lib import DEPTH, QUOTE, SESSION_EXPIRED, node, ok, reply

F = "triggers"
ANY = {"$any": "string"}


def de(parity, why, fix=None, **sets):
    d = {"parity": parity, "why": why}
    if fix:
        d["fix"] = fix
    if sets:
        d["set"] = sets
    return d


node(F, "webhookTrigger", "logs_and_continues", {"symbol": "SBIN", "exchange": "NSE"},
     vars_in={"webhook": {"symbol": "SBIN", "action": "BUY"}}, parity=["N-webhookTrigger-00", "N-webhookTrigger-01"])
node(F, "orderUpdateTrigger", "logs_and_continues", {"status": "complete", "trigger": "once"},
     vars_in={"webhook": {"trigger_type": "order_update", "orderid": "1", "order_status": "complete"}},
     parity=["N-orderUpdateTrigger-00", "N-orderUpdateTrigger-01"])

# --- priceAlert (the node; its monitor is triggers' own) ---------------------------

PA = {"symbol": "SBIN", "exchange": "NSE", "condition": "above", "price": 800, "outputVariable": "alert"}
STREAM_FIX = ["price_alert_message"]
for nm, extra in (
    ("above_true", {}),
    ("above_false", {"price": 900}),
    ("below_editor_word", {"condition": "below", "price": 900}),
    ("greater_than_canonical", {"condition": "greater_than"}),
    ("crosses_above_level_test", {"condition": "crosses_above"}),
    ("crosses_below_level_test", {"condition": "Crosses-Below", "price": 900}),
    ("crossing_within_tolerance", {"condition": "crossing", "price": 812.0}),
    ("crossing_outside_tolerance", {"condition": "crosses", "price": 800.0}),
    ("inside_channel", {"condition": "inside_channel", "priceLower": 800, "priceUpper": "{{hi}}"}),
    ("entering_channel_outside", {"condition": "entering_channel", "priceLower": 820, "priceUpper": 830}),
    ("outside_channel", {"condition": "outside_channel", "priceLower": 820, "priceUpper": 830}),
    ("exiting_channel_inside", {"condition": "exiting_channel", "priceLower": 800, "priceUpper": 830}),
    ("default_condition", {"condition": ""}),
    ("no_output_variable", {"outputVariable": ""}),
):
    node(F, "priceAlert", nm, {**PA, **extra}, vars_in={"hi": 815}, script=[reply("quotes", QUOTE)],
         parity=["N-priceAlert-01", "N-priceAlert-02", "N-priceAlert-05", "N-priceAlert-06", "N-priceAlert-08"])
for cond in ("moving_up", "moving_down_percent"):
    node(F, "priceAlert", f"{cond}_needs_monitor", {**PA, "condition": cond}, script=[reply("quotes", QUOTE)],
         parity=["N-priceAlert-07"],
         desktop_expected=de("N-priceAlert-07", "WEB-DEFECT: the failure has no message, so the run reads `node failed`; the desktop puts the logged sentence in `message`.",
                             fix=STREAM_FIX))
node(F, "priceAlert", "unknown_condition", {**PA, "condition": "near"}, script=[reply("quotes", QUOTE)],
     parity=["N-priceAlert-07"],
     desktop_expected=de("N-priceAlert-07", "WEB-DEFECT: the failure has no message; the desktop puts the logged sentence in `message`.",
                         fix=STREAM_FIX))
node(F, "priceAlert", "condition_absent_unknown", {k: v for k, v in PA.items() if k != "condition"} | {"condition": None},
     script=[reply("quotes", QUOTE)], parity=["N-priceAlert-07"],
     desktop_expected=de("N-priceAlert-07", "WEB-DEFECT: the failure has no message; the desktop puts the logged sentence in `message`.",
                         fix=STREAM_FIX))
node(F, "priceAlert", "quote_fails", PA, script=[reply("quotes", SESSION_EXPIRED)], parity=["N-priceAlert-05"],
     desktop_expected=de("N-priceAlert-05", "WEB-DEFECT: the client's error text is dropped (`node failed`); the desktop carries it as `message`.",
                         fix=STREAM_FIX))
node(F, "priceAlert", "no_symbol", {**PA, "symbol": ""}, parity=["N-priceAlert-03"])
node(F, "priceAlert", "fired_by_monitor", PA, run_context={"trigger": "price_alert"},
     vars_in={"webhook": {"trigger_type": "price_alert", "trigger_price": 812.75, "symbol": "SBIN"}},
     parity=["N-priceAlert-04"])
node(F, "priceAlert", "webhook_claims_monitor", {**PA, "price": 900},
     vars_in={"webhook": {"trigger_type": "price_alert", "trigger_price": 1}}, script=[reply("quotes", QUOTE)],
     parity=["N-priceAlert-04"],
     desktop_expected=de("N-priceAlert-04", "WEB-DEFECT: a webhook body claiming trigger_type price_alert skips the price check; on the desktop only a monitor-fired run (a run-context flag, never a payload field) is trusted.",
                         fix=["price_alert_untrusted_webhook"]))
node(F, "priceAlert", "price_unresolved", {**PA, "price": "{{level}}"}, script=[reply("quotes", QUOTE)],
     parity=["N-priceAlert-09"],
     desktop_expected=de("N-priceAlert-09", "WEB-DEFECT: an unparseable price becomes 0.0, so `above` is true on any positive LTP; the desktop fails closed (the sentence is the lane's).",
                         calls={"$any": "calls"}, outputs={"$any": "dict"}, vars_out={},
                         logs={"$any": "logs"}, next=["pass"], conditions_out={},
                         error={"errors": [{"node": "n1", "type": "priceAlert", "message": ANY}], "abort": None}))

# --- subscribeLtp / subscribeQuote / subscribeDepth -------------------------------

FEED = {"username": "trader", "broker": "fakebroker"}
LTP_TICK = {"symbol": "SBIN", "exchange": "NSE", "mode": 1, "data": {"ltp": 812.65}}
SL = {"symbol": "SBIN", "exchange": "NSE", "outputVariable": "ltp"}
SFIX = de("N-subscribeLtp-05", "WEB-DEFECT (decisions.md 9): store_output overwrites the documented value with the whole result dict; the desktop stores the documented value under the stripped outputVariable (the run log is unchanged).",
          fix=["stream_documented_value"])
node(F, "subscribeLtp", "websocket_nested", SL, env={"feed": {**FEED, "ticks": [LTP_TICK]}},
     parity=["N-subscribeLtp-01", "N-subscribeLtp-02", "N-subscribeLtp-03", "N-stream-02", "N-stream-09"], desktop_expected=SFIX)
node(F, "subscribeLtp", "websocket_top_level", {**SL, "outputVariable": " px "},
     env={"feed": {**FEED, "ticks": [{"symbol": "SBIN", "exchange": "NSE", "mode": 1, "ltp": 812.7}]}},
     parity=["N-subscribeLtp-03"], desktop_expected=SFIX)
node(F, "subscribeLtp", "no_output_variable_keeps_scalar", {"symbol": "SBIN", "exchange": "NSE"},
     env={"feed": {**FEED, "ticks": [LTP_TICK]}}, parity=["N-subscribeLtp-05"])
node(F, "subscribeLtp", "wrong_frames_then_rest", SL,
     env={"feed": {**FEED, "ticks": [{**LTP_TICK, "mode": 2}, {**LTP_TICK, "symbol": "INFY"}, {**LTP_TICK, "exchange": "BSE"}]}},
     script=[reply("quotes", QUOTE)], parity=["N-stream-09", "N-subscribeLtp-04"], desktop_expected=SFIX)
node(F, "subscribeLtp", "invalid_api_key_rest", SL, env={"feed": {"username": None}}, script=[reply("quotes", QUOTE)],
     parity=["N-stream-07", "N-subscribeLtp-04"], desktop_expected=SFIX)
node(F, "subscribeLtp", "connection_fails_rest", SL, env={"feed": {**FEED, "connect_error": "WebSocket proxy is not running"}},
     script=[reply("quotes", QUOTE)], parity=["N-stream-08"], desktop_expected=SFIX)
node(F, "subscribeLtp", "subscribe_rejected_rest", SL, env={"feed": {**FEED, "subscribe_error": "Subscription limit reached"}},
     script=[reply("quotes", QUOTE)], parity=["N-stream-10", "N-subscribeLtp-04"], desktop_expected=SFIX)
node(F, "subscribeLtp", "rest_fails", SL, env={"feed": {**FEED, "connect_error": "down"}}, script=[reply("quotes", SESSION_EXPIRED)],
     parity=["N-subscribeLtp-04"])
node(F, "subscribeLtp", "rest_empty_data", SL, env={"feed": {**FEED, "connect_error": "down"}},
     script=[reply("quotes", ok(data={}))], parity=["N-subscribeLtp-04"], desktop_expected=SFIX)
node(F, "subscribeLtp", "websocket_error", SL, env={"feed": {**FEED, "verify_raise": "auth database is locked"}},
     script=[reply("quotes", QUOTE)], parity=["N-stream-10"], desktop_expected=SFIX)
node(F, "subscribeLtp", "rest_raises", SL, env={"feed": {**FEED, "connect_error": "down"}},
     script=[{"endpoint": "quotes", "reply": {"$raise": "connection reset by broker"}}], parity=["N-subscribeLtp-04", "CL-26"],
     note="The web client lets a service exception through; the desktop client never raises (CL-26), so this golden is web evidence only.")
node(F, "subscribeLtp", "rest_fails_without_text", SL, env={"feed": {**FEED, "connect_error": "down"}},
     script=[reply("quotes", {"status": "error", "code": 500})], parity=["N-subscribeLtp-04"])
node(F, "subscribeLtp", "no_symbol", {"exchange": "NSE"}, parity=["N-subscribeLtp-01"])
QT = {"symbol": "SBIN", "exchange": "NSE", "mode": 2, "data": {"ltp": 812.5, "open": 806.0, "high": 818.0, "low": 805.25,
                                                                "prev_close": 800.0, "volume": 1250000}}
SQ = {"symbol": "SBIN", "exchange": "NSE", "outputVariable": "quote"}
QFIX = {**SFIX, "parity": "N-subscribeQuote-04"}
node(F, "subscribeQuote", "websocket", SQ, env={"feed": {**FEED, "ticks": [QT]}},
     parity=["N-subscribeQuote-01", "N-subscribeQuote-02"], desktop_expected=QFIX)
node(F, "subscribeQuote", "websocket_top_level_close", SQ,
     env={"feed": {**FEED, "ticks": [{"symbol": "SBIN", "exchange": "NSE", "mode": 2, "ltp": 0, "close": 799.5, "volume": 10}]}},
     parity=["N-subscribeQuote-02"], desktop_expected=QFIX)
node(F, "subscribeQuote", "rest_fallback", SQ, env={"feed": {**FEED, "connect_error": "down"}}, script=[reply("quotes", QUOTE)],
     parity=["N-subscribeQuote-03"], desktop_expected=QFIX)
node(F, "subscribeQuote", "rest_empty_data", SQ, env={"feed": {**FEED, "connect_error": "down"}},
     script=[reply("quotes", ok(data={}))], parity=["N-subscribeQuote-03"], desktop_expected=QFIX)
node(F, "subscribeQuote", "rest_fails", SQ, env={"feed": {**FEED, "connect_error": "down"}},
     script=[reply("quotes", SESSION_EXPIRED)], parity=["N-subscribeQuote-03"])
node(F, "subscribeQuote", "rest_raises", SQ, env={"feed": {**FEED, "connect_error": "down"}},
     script=[{"endpoint": "quotes", "reply": {"$raise": "connection reset by broker"}}], parity=["N-subscribeQuote-03", "CL-26"],
     note="Web evidence only: the desktop client never raises (CL-26).")
node(F, "subscribeQuote", "rest_fails_without_text", SQ, env={"feed": {**FEED, "connect_error": "down"}},
     script=[reply("quotes", {"status": "error", "code": 500})], parity=["N-subscribeQuote-03"])
node(F, "subscribeQuote", "no_symbol", {}, parity=["N-subscribeQuote-01"])
DT = {"symbol": "SBIN", "exchange": "NSE", "mode": 3, "data": {"ltp": 812.5, "totalbuyqty": 210, "totalsellqty": 200,
      "bids": [{"price": 812.4, "quantity": 150}], "asks": [{"price": 812.6, "quantity": 120}]}}
SD = {"symbol": "SBIN", "exchange": "NSE", "outputVariable": "depth"}
DFIX = {**SFIX, "parity": "N-subscribeDepth-04"}
node(F, "subscribeDepth", "websocket", SD, env={"feed": {**FEED, "ticks": [DT]}},
     parity=["N-subscribeDepth-01", "N-subscribeDepth-02"], desktop_expected=DFIX)
node(F, "subscribeDepth", "empty_frame_skipped", SD,
     env={"feed": {**FEED, "ticks": [{"symbol": "SBIN", "exchange": "NSE", "mode": 3, "data": {"ltp": 1}}, DT]}},
     parity=["N-stream-09"], desktop_expected=DFIX)
node(F, "subscribeDepth", "fyers_buy_sell", SD,
     env={"feed": {**FEED, "ticks": [{"symbol": "SBIN", "exchange": "NSE", "mode": 3, "ltp": 812.5,
                                      "depth": {"buy": [{"price": 812.4, "volume": 150}], "sell": [{"price": 812.6, "quantity": 120}]}}]}},
     parity=["N-subscribeDepth-02"], desktop_expected=DFIX)
node(F, "subscribeDepth", "rest_fallback", SD, env={"feed": {**FEED, "connect_error": "down"}}, script=[reply("depth", DEPTH)],
     parity=["N-subscribeDepth-03"], desktop_expected=DFIX)
node(F, "subscribeDepth", "rest_empty_data", SD, env={"feed": {**FEED, "connect_error": "down"}},
     script=[reply("depth", ok(data={}))], parity=["N-subscribeDepth-03"], desktop_expected=DFIX)
node(F, "subscribeDepth", "rest_fails", SD, env={"feed": {**FEED, "connect_error": "down"}},
     script=[reply("depth", SESSION_EXPIRED)], parity=["N-subscribeDepth-03"])
node(F, "subscribeDepth", "rest_raises", SD, env={"feed": {**FEED, "connect_error": "down"}},
     script=[{"endpoint": "depth", "reply": {"$raise": "connection reset by broker"}}], parity=["N-subscribeDepth-03", "CL-26"],
     note="Web evidence only: the desktop client never raises (CL-26).")
node(F, "subscribeDepth", "rest_fails_without_text", SD, env={"feed": {**FEED, "connect_error": "down"}},
     script=[reply("depth", {"status": "error", "code": 500})], parity=["N-subscribeDepth-03"])
node(F, "subscribeDepth", "no_symbol", {}, parity=["N-subscribeDepth-01"])

# --- unsubscribe ------------------------------------------------------------------

node(F, "unsubscribe", "all", {"streamType": "all"}, env={"feed": FEED}, parity=["N-unsubscribe-01", "N-unsubscribe-03", "N-unsubscribe-07"])
node(F, "unsubscribe", "default_all", {}, env={"feed": FEED}, parity=["N-unsubscribe-01", "N-unsubscribe-03"])
for st, mode in (("ltp", "LTP"), ("quote", "Quote"), ("depth", "Depth"), ("candles", "Quote")):
    node(F, "unsubscribe", f"symbol_{st}", {"symbol": "SBIN", "exchange": "NSE", "streamType": st}, env={"feed": FEED},
         parity=["N-unsubscribe-05", "N-unsubscribe-07"])
node(F, "unsubscribe", "stream_without_symbol", {"streamType": "ltp"}, env={"feed": FEED}, parity=["N-unsubscribe-04"],
     desktop_expected=de("N-unsubscribe-04", "decisions.md 9: `all` gives back only this workflow's subscriptions, so the refusal says that instead of warning about the instance's whole feed and the Sandbox engine's.",
                         fix=["unsubscribe_refusal_own"]))
node(F, "unsubscribe", "invalid_api_key", {"streamType": "all"}, env={"feed": {"username": None}}, parity=["N-unsubscribe-02"])
node(F, "unsubscribe", "no_connection", {"symbol": "SBIN", "streamType": "ltp"},
     env={"feed": {**FEED, "connect_error": "down"}}, parity=["N-unsubscribe-02"])
node(F, "unsubscribe", "rejected", {"symbol": "SBIN", "streamType": "ltp"},
     env={"feed": {**FEED, "unsubscribe_error": "Not subscribed"}}, parity=["N-unsubscribe-06"])
node(F, "unsubscribe", "rejected_all", {"streamType": "all"}, env={"feed": {**FEED, "unsubscribe_error": ""}},
     parity=["N-unsubscribe-06"])
node(F, "unsubscribe", "exception", {"symbol": "SBIN", "streamType": "quote"},
     env={"feed": {**FEED, "unsubscribe_raise": "proxy went away"}}, parity=["N-unsubscribe-08"],
     desktop_expected=de("N-unsubscribe-08", "WEB-DEFECT: an exception is reported as success; the desktop reports it as a failure with the exception text.",
                         fix=["unsubscribe_exception_error"]))

node(F, "subscribeLtp", "interpolated", {"symbol": "{{webhook.symbol}}", "exchange": "{{webhook.ex}}", "outputVariable": "ltp"},
     vars_in={"webhook": {"symbol": "SBIN", "ex": "NSE"}}, env={"feed": {**FEED, "ticks": [LTP_TICK]}},
     parity=["N-subscribeLtp-01"], desktop_expected=SFIX)
node(F, "unsubscribe", "interpolated", {"symbol": "{{webhook.symbol}}", "exchange": "NSE", "streamType": "{{st}}"},
     vars_in={"webhook": {"symbol": "SBIN"}, "st": "depth"}, env={"feed": FEED}, parity=["N-unsubscribe-05"])
node(F, "priceAlert", "interpolated_symbol_and_price", {**PA, "symbol": "{{webhook.symbol}}", "price": "{{lvl}}"},
     vars_in={"webhook": {"symbol": "SBIN"}, "lvl": "810"}, script=[reply("quotes", QUOTE)], parity=["N-priceAlert-01"])


# decisions.md 9: `unsubscribe all` releases only this workflow's subscriptions.
for _s in __import__("lib").SCENARIOS:
    if _s["type"] == "unsubscribe" and _s["graph"]["nodes"][0]["data"].get("streamType", "all") == "all" \
            and not _s["desktop_expected"]:
        _s["desktop_expected"] = de(
            "N-unsubscribe-03",
            "decisions.md 9: `all` releases only the subscriptions this workflow holds (none here), never the instance's whole feed, so nothing is refused.",
            fix=["unsubscribe_all_own"])
