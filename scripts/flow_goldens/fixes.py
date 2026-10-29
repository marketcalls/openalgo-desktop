"""The web's code with one WEB-DEFECT fixed, for desktop_expected.

Each fix rewrites one function of the web's own source with exact text
replacements (each must match once), compiles it in the module's globals and
installs it for one re-run. So a desktop_expected value is the web's behaviour
with only the decided fix applied, not a hand-written guess.
"""

from __future__ import annotations

import contextlib
import inspect
import textwrap

import harness

fx = harness.fx
fc = harness.fc
tcal = harness.tcal
ind = harness.ind


def _rewrite(owner, name, edits, module_globals):
    func = getattr(owner, name)
    src = inspect.getsource(func)
    for edit in edits:
        old, new = edit[0], edit[1]
        want = edit[2] if len(edit) > 2 else 1
        n = src.count(old)
        if n != want:
            raise SystemExit(f"fix for {name}: {old!r} found {n} times, expected {want}")
        src = src.replace(old, new)
    first = src.splitlines()[0]
    pad = first[: len(first) - len(first.lstrip())]
    src = "\n".join(line[len(pad):] if line.startswith(pad) else line for line in src.splitlines()) + "\n"
    scope: dict = {}
    exec(compile(src, f"<fix {name}>", "exec"), module_globals, scope)
    return scope[func.__name__]


FIXES = {
    # N-cancelOrder-02: store the result like every other action node.
    "cancel_store_output": [(fx.NodeExecutor, "execute_cancel_order", [
        ("        return result", "        self.store_output(node_data, result)\n        return result")])],
    # N-basketOrder-04: upper-case a row's symbol as placeOrder does.
    "basket_upper_symbol": [(fx.NodeExecutor, "execute_basket_order", [
        ('symbol = required_text(raw_order.get("symbol"), "symbol")',
         'symbol = required_text(raw_order.get("symbol"), "symbol").upper()')])],
    # N-basketOrder-06: send the trigger under the key the service reads.
    "basket_trigger_price": [(fx.NodeExecutor, "execute_basket_order", [
        ('"triggerprice": trigger_price,', '"trigger_price": trigger_price,')])],
    # N-dataCommon-14: interpolate the symbol once.
    "symbol_once": [
        (fx.NodeExecutor, m, [('symbol = self.context.interpolate(self.get_str(node_data, "symbol"))',
                               'symbol = self.get_str(node_data, "symbol")')])
        for m in ("execute_prior_period_ohlc", "execute_bar_offset", "execute_indicator")
    ],
    # N-indicator-03: a blank sourceSeries is absent.
    "blank_source_series": [(fx.NodeExecutor, "execute_indicator", [
        ('if source_series_raw not in (None, ""):',
         'if source_series_raw not in (None, "") and not (isinstance(source_series_raw, str) and not source_series_raw.strip()):')])],
    # IND-043 / N-indicator-11: any compute failure fails only the node.
    "indicator_any_error": [(fx.NodeExecutor, "execute_indicator", [
        ("            except ValueError as e:\n                self.log(f\"Indicator error: {e}\", \"error\")",
         "            except (ValueError, TypeError) as e:\n                self.log(f\"Indicator error: {e}\", \"error\")"),
        ("        except ValueError as e:\n            self.log(f\"Indicator error: {e}\", \"error\")",
         "        except (ValueError, TypeError) as e:\n            self.log(f\"Indicator error: {e}\", \"error\")")])],
    # N-margin-06: a basket that is blank before interpolation is absent.
    "margin_blank_absent": [(fx.NodeExecutor, "_parse_margin_positions", [
        ("if key in node_data and node_data[key] is not None",
         "if key in node_data and node_data[key] is not None and not (isinstance(node_data[key], str) and not node_data[key].strip())")])],
    # N-calendar-05: only SPECIAL_SESSION is special; only TRADING_HOLIDAY closes.
    "calendar_kinds": [(tcal, "_calendar_entries", [
        ('if kind == "SPECIAL_SESSION" or not (entry.get("closed_exchanges") or []):',
         'if kind == "SPECIAL_SESSION":')])],
    # decisions.md 4: the client maps the panel's 1d to D.
    "history_1d_is_d": [(fc.FlowOpenAlgoClient, "get_history", [
        ("        success, response, status_code = get_history(\n            symbol=symbol,",
         "        interval = \"D\" if interval == \"1d\" else interval\n        success, response, status_code = get_history(\n            symbol=symbol,")])],
}

STREAM_STORE = [
    # N-subscribeLtp-05 (and Quote, Depth): the variable holds the documented
    # value under the stripped name; the run log is unchanged.
    ("self.context.set_variable(output_var, ltp)", "self.context.set_variable(output_var.strip(), ltp)", 2),
    ("self.context.set_variable(output_var, quote_data)", "self.context.set_variable(output_var.strip(), quote_data)", 2),
    ("self.context.set_variable(output_var, depth_data)", "self.context.set_variable(output_var.strip(), depth_data)", 2),
]
_LOG_ONLY = (
    'self.log(f"Stored result in variable: {node_data.get(\'outputVariable\')}") '
    'if isinstance(node_data.get("outputVariable"), str) and node_data["outputVariable"].strip() else None'
)

FIXES.update({
    # IND-049 (decisions.md 10): an offset counts bars, a NaN bar reads None.
    "indicator_offset_counts_bars": [(ind, "compute_indicator", [
        ("            series = out[col].dropna()\n            try:\n                row[col] = round(float(series.iloc[offset]), 6)\n",
         "            series = out[col]\n            try:\n                _v = float(series.iloc[offset])\n                row[col] = None if _v != _v else round(_v, 6)\n")])],
    # IND-062 (decisions.md 10): a weekly/monthly bar of the current
    # week/month is still forming.
    "forming_week_month": [(ind, "_drop_forming_bar", [
        ('    if key in ("d", "1d", "day", "w", "week", "m", "month"):\n        if last_ts.date() == now.date():',
         '    if key in ("w", "week"):\n        if last_ts.isocalendar()[:2] == now.isocalendar()[:2]:\n            return records[:-1]\n        return records\n'
         '    if key in ("m", "month"):\n        if (last_ts.year, last_ts.month) == (now.year, now.month):\n            return records[:-1]\n        return records\n'
         '    if key in ("d", "1d", "day"):\n        if last_ts.date() == now.date():')])],
    # N-optionChain-03 (decisions.md 10): an explicit expiry wins over one
    # embedded in the underlying (the client sends the base symbol).
    "option_chain_explicit_expiry": [(fc.FlowOpenAlgoClient, "optionchain", [
        ("        success, response, status_code = get_option_chain(\n",
         "        from services.flow_node_contracts import parse_underlying_symbol as _pus\n"
         "        if expiry_date and _pus(underlying)[1]:\n"
         "            underlying = _pus(underlying)[0]\n"
         "        success, response, status_code = get_option_chain(\n")])],
    # N-mathExpression-11: an unresolved reference says so.
    "math_unresolved": [(fx.NodeExecutor, "execute_math_expression", [
        ("            # Step 2: Safely evaluate the expression\n",
         "            _left = list(dict.fromkeys(_UNRESOLVED_PATTERN.findall(interpolated))) if isinstance(interpolated, str) else []\n"
         "            if len(_left) == 1:\n"
         "                raise ValueError(f\"unresolved variable {_left[0]}\")\n"
         "            if _left:\n"
         "                raise ValueError(f\"unresolved variables {', '.join(_left)}\")\n"
         "            # Step 2: Safely evaluate the expression\n")])],
    # N-delay-05: a negative delay fails only this node.
    "delay_negative": [(fx.NodeExecutor, "execute_delay", [
        ("        time_module.sleep(delay_seconds)\n",
         "        if delay_seconds < 0:\n"
         "            return {\"status\": \"error\", \"message\": \"sleep length must be non-negative\"}\n"
         "        time_module.sleep(delay_seconds)\n")])],
    # N-httpRequest-07: carrier-grade NAT (100.64.0.0/10) is not public.
    "http_cgnat": [(fx.NodeExecutor, "_check_http_destination", [
        ("                or address.is_unspecified\n",
         "                or address.is_unspecified\n"
         "                or (address.version == 4 and address in __import__('ipaddress').ip_network('100.64.0.0/10'))\n")])],
    # N-httpRequest-19: a bad port fails only the node.
    "http_bad_port": [(fx.NodeExecutor, "_check_http_destination", [
        ("        host = parts.hostname\n",
         "        try:\n"
         "            parts.port\n"
         "        except ValueError:\n"
         "            return \"URL could not be parsed\"\n"
         "        host = parts.hostname\n")])],
    # N-httpRequest-11: an object body is sent as JSON.
    "http_object_body": [(fx.NodeExecutor, "execute_http_request", [
        ("        url = self.context.interpolate(url)\n",
         "        if isinstance(body, (dict, list)):\n"
         "            body = json.dumps(body)\n"
         "        url = self.context.interpolate(url)\n")])],
    # N-httpRequest-13: a non-2xx result names its status.
    "http_status_message": [(fx.NodeExecutor, "execute_http_request", [
        ('                    "data": response_data,\n                }\n',
         '                    "data": response_data,\n                }\n'
         '                if not response.is_success:\n'
         '                    result["message"] = f"HTTP {response.status_code}"\n')])],
    # N-priceCondition-05: a missing quote field fails closed.
    "price_condition_missing_field": [(fx.NodeExecutor, "execute_price_condition", [
        ("        if field == \"change_percent\":\n",
         "        _need = (\"ltp\", \"prev_close\") if field == \"change_percent\" else (field,)\n"
         "        for _k in _need:\n"
         "            if data.get(_k) is None or (_k == \"prev_close\" and field == \"change_percent\" and not data.get(_k)):\n"
         "                message = f\"the quote has no {_k}\"\n"
         "                self.log(f\"Price check aborted: {message}\", \"error\")\n"
         "                return {\"status\": \"error\", \"condition\": False, \"message\": message}\n"
         "        if field == \"change_percent\":\n")])],
    # N-timeWindow-04: only true / "true" / "1" invert.
    "time_window_invert_text": [(fx.NodeExecutor, "execute_time_window", [
        ('invert = bool(node_data.get("invertCondition", False))',
         'invert = (lambda v: v is True or (isinstance(v, str) and v.strip().lower() in ("true", "1")) or (isinstance(v, (int, float)) and not isinstance(v, bool) and v == 1))(node_data.get("invertCondition", False))')])],
    # N-positionCheck-08: a decimal quantity string parses.
    "position_quantity_float": [(fx.NodeExecutor, "execute_position_check", [
        ('quantity = int(result.get("quantity", 0) or 0)', 'quantity = int(float(result.get("quantity", 0) or 0))')])],
    # N-unsubscribe-08: an exception is a failure.
    "unsubscribe_exception_error": [(fx.NodeExecutor, "execute_unsubscribe", [
        ('                "message": "Unsubscribe completed (with warnings)",\n',
         '                "message": str(e),\n'),
        ('        except Exception as e:\n            self.log(f"Unsubscribe error: {str(e)}", "warning")\n            return {\n                "status": "success",',
         '        except Exception as e:\n            self.log(f"Unsubscribe error: {str(e)}", "warning")\n            return {\n                "status": "error",')])],
    # N-priceAlert-05 / -07: the failure carries its sentence as message.
    "price_alert_message": [(fx.NodeExecutor, "execute_price_alert", [
        ('        if result.get("status") != "success":\n            return {"status": "error", "condition": False}\n',
         '        if result.get("status") != "success":\n            return {"status": "error", "condition": False, "message": result.get("error") or result.get("message") or "the price lookup failed"}\n'),
        ('            return {"status": "error", "condition": False, "ltp": ltp}\n        else:',
         '            return {"status": "error", "condition": False, "ltp": ltp, "message": f"Price alert condition {condition_type!r} needs the previous tick and can only be evaluated by the price monitor; wire this node as the trigger."}\n        else:'),
        ('            return {"status": "error", "condition": False, "ltp": ltp}\n\n        self.log(f"Price alert:',
         '            return {"status": "error", "condition": False, "ltp": ltp, "message": f"Price alert has an unrecognized condition {node_data.get(\'condition\')!r}; it can never be true."}\n\n        self.log(f"Price alert:')])],
    # N-priceAlert-04: a webhook body cannot claim to be the monitor.
    "price_alert_untrusted_webhook": [(fx.NodeExecutor, "execute_price_alert", [
        ('if isinstance(webhook, dict) and webhook.get("trigger_type") == "price_alert":',
         'if isinstance(webhook, dict) and webhook.get("trigger_type") == "price_alert" and getattr(self.context, "trusted_price_alert", False):')])],
    "stream_documented_value": [
        (fx.NodeExecutor, "execute_subscribe_ltp", [STREAM_STORE[0], ("self.store_output(node_data, streaming_result)", _LOG_ONLY, 2)]),
        (fx.NodeExecutor, "execute_subscribe_quote", [STREAM_STORE[1], ("self.store_output(node_data, streaming_result)", _LOG_ONLY, 2)]),
        (fx.NodeExecutor, "execute_subscribe_depth", [STREAM_STORE[2], ("self.store_output(node_data, streaming_result)", _LOG_ONLY, 2)]),
    ],
    # N-gate-04 (decision 8): AND/OR read inputCount as the validator does.
    "gate_slot_count": [(harness, "_orig_chain", [
        ('        declared = node_data.get("inputCount")\n',
         '        declared = node_data.get("inputCount")\n'
         '        if node_type != "notGate":\n'
         '            try:\n'
         '                declared = max(int(declared), 1) if declared not in (None, "") else 2\n'
         '            except (TypeError, ValueError):\n'
         '                declared = 2\n')])],
})


def _module_globals(owner):
    if inspect.ismodule(owner):
        if owner is harness:
            return fx.__dict__  # harness._orig_chain is the executor's walk
        return owner.__dict__
    return inspect.getmodule(owner).__dict__


@contextlib.contextmanager
def applied(names):
    saved = []
    grouped: dict = {}
    try:
        for name in names or []:
            if name == "price_alert_branches":
                # N-logic-04: priceAlert is evaluated once per run like the
                # other branching nodes.
                saved.append((fx, "BRANCHING_NODE_TYPES", fx.BRANCHING_NODE_TYPES))
                fx.BRANCHING_NODE_TYPES = fx.BRANCHING_NODE_TYPES + ("priceAlert",)
                continue
            for owner, func, edits in FIXES[name]:
                grouped.setdefault((owner, func), []).extend(edits)
        for (owner, func), edits in grouped.items():
            saved.append((owner, func, getattr(owner, func)))
            setattr(owner, func, _rewrite(owner, func, edits, _module_globals(owner)))
        yield
    finally:
        for owner, func, value in reversed(saved):
            setattr(owner, func, value)
