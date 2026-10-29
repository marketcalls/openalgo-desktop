"""Logic, condition, alert and http nodes (lane nodes-logic)."""

from lib import FUNDS, POSITIONS, QUOTE, SESSION_EXPIRED, err, node, ok, reply

F = "logic"
ANY = {"$any": "string"}


def de(parity, why, fix=None, **sets):
    d = {"parity": parity, "why": why}
    if fix:
        d["fix"] = fix
    if sets:
        d["set"] = sets
    return d


# --- mathExpression ---------------------------------------------------------------

V = {"ltp": 812.5, "lot": 10, "brokerage": "20", "cfg": {"a": 1}, "levels": [1, 2]}
for nm, expr, extra in (
    ("happy", "({{ltp}} * {{lot}}) + {{brokerage}}", {"outputVariable": "cost"}),
    ("default_output_variable", "{{lot}} / 4", {}),
    ("integer_result_is_float", "2 + 3", {"outputVariable": "n"}),
    ("floor", "floor({{ltp}} / 3)", {"outputVariable": "n"}),
    ("power_and_mod", "2 ** 10 % 7 - -1", {"outputVariable": "n"}),
    ("large_number", "10 ** 20", {"outputVariable": "n"}),
    ("exponent_too_large", "2 ** 101", {}),
    ("division_by_zero", "{{lot}} / 0", {}),
    ("float_division_by_zero", "1.5 / 0", {}),
    ("modulo_by_zero", "7 % 0", {}),
    ("whitespace_only", "   ", {}),
    ("call_not_floor", "abs(-1)", {}),
    ("floor_two_args", "floor(1, 2)", {}),
    ("bare_name", "lot * 2", {}),
    ("string_constant", "'10' * 2", {}),
    ("boolean_constant", "True + 1", {}),
    ("shift_operator", "1 << 2", {}),
    ("invert_operator", "~1", {}),
    ("comparison", "1 < 2", {}),
    ("complex_constant", "1j * 2", {}),
    ("negative_exponent_too_large", "2 ** -101", {}),
    ("zero_to_negative_power", "0.0 ** -1", {}),
    ("int_too_large_for_float", "(10 ** 100) ** 4", {}),
    ("float_overflow", "1e300 ** 2", {}),
    ("floor_of_infinity", "floor(1e308 * 10)", {}),
    ("unclosed_paren", "(1 + 2", {}),
    ("dict_variable", "{{cfg}}", {}),
    ("list_variable", "{{levels}}", {}),
):
    node(F, "mathExpression", nm, {"expression": expr, **extra}, vars_in=V,
         parity=["N-mathExpression-01", "N-mathExpression-03"])
node(F, "mathExpression", "empty_expression", {"expression": ""}, parity=["N-mathExpression-02"])
node(F, "mathExpression", "number_expression", {"expression": 42}, parity=["N-mathExpression-12"])
node(F, "mathExpression", "syntax_error", {"expression": "2 +"}, vars_in=V, parity=["N-mathExpression-10"],
     desktop_expected=de("N-mathExpression-10", "decisions.md 5: keep only the `Invalid expression syntax: ` prefix of Python's SyntaxError text.",
                         outputs={"status": "error", "message": {"$prefix": "Invalid expression syntax: "}},
                         logs=[["info", "Evaluating: 2 +"], ["info", "Interpolated: 2 +"],
                               ["error", {"$prefix": "Math expression failed: Invalid expression syntax: "}],
                               ["error", {"$prefix": "Stopping this branch: mathExpression node failed (Invalid expression syntax: "}]],
                         error={"errors": [{"node": "n1", "type": "mathExpression", "message": {"$prefix": "Invalid expression syntax: "}}], "abort": None}))
node(F, "mathExpression", "unresolved_variable", {"expression": "{{missing}} * 2"}, vars_in=V, parity=["N-mathExpression-11"],
     desktop_expected=de("N-mathExpression-11", "WEB-DEFECT: an unresolved {{var}} reads `Unsupported expression type: Set`; the desktop names the unresolved variable.",
                         fix=["math_unresolved"]))

# --- delay / waitUntil ------------------------------------------------------------

for nm, data in (
    ("seconds", {"delayValue": 5, "delayUnit": "seconds"}),
    ("seconds_text", {"delayValue": "5"}),
    ("minutes", {"delayValue": 2, "delayUnit": "minutes"}),
    ("hours_capped", {"delayValue": 1, "delayUnit": "hours"}),
    ("unknown_unit_is_seconds", {"delayValue": 3, "delayUnit": "days"}),
    ("float_truncates", {"delayValue": 1.9}),
    ("legacy_milliseconds", {"delayMs": 1500}),
    ("default_one_second", {}),
):
    node(F, "delay", nm, data, parity=["N-delay-01", "N-delay-02", "N-delay-03"])
node(F, "delay", "fraction_text_aborts", {"delayValue": "1.5"}, parity=["N-delay-02", "N-logic-05"])
node(F, "delay", "unresolved_aborts", {"delayValue": "{{wait}}"}, parity=["N-delay-02", "N-logic-05"])
node(F, "delay", "negative", {"delayValue": -3}, parity=["N-delay-05"],
     desktop_expected=de("N-delay-05", "WEB-DEFECT: a negative delay raises from time.sleep and aborts the whole run; the desktop fails only this node.",
                         fix=["delay_negative"]))
for nm, data, clock in (
    ("waits", {"targetTime": "10:20"}, None),
    ("waits_with_seconds", {"targetTime": "10:15:30"}, None),
    ("already_passed", {"targetTime": "09:30"}, None),
    ("default_target_passed", {}, None),
    ("same_second", {"targetTime": "10:15:00"}, None),
    ("too_far", {"targetTime": "15:15"}, None),
    ("malformed_clamped", {"targetTime": "25:99"}, "2026-10-29T23:58:00"),
    ("text_parts_default", {"targetTime": "ab:cd"}, "2026-10-29T09:00:00"),
):
    node(F, "waitUntil", nm, data, clock=clock, parity=["N-waitUntil-01", "N-waitUntil-02", "N-waitUntil-03", "N-waitUntil-04"])

# --- log --------------------------------------------------------------------------

node(F, "log", "info_interpolated", {"message": "LTP {{q.data.ltp}} for {{sym}} at {{time}}", "level": "info"},
     vars_in={"q": {"data": {"ltp": 812.5}}, "sym": "SBIN"}, parity=["N-log-01", "E-13"])
node(F, "log", "warn_level", {"message": "careful", "level": "warn"}, parity=["N-log-01", "F-643"])
node(F, "log", "error_level", {"message": "bad", "level": "error"}, parity=["N-log-01"])
node(F, "log", "default_level", {"message": "plain"}, parity=["N-log-01"])
node(F, "log", "dict_and_unresolved", {"message": "cfg={{cfg}} flag={{flag}} none={{nothing}} miss={{missing.x}}"},
     vars_in={"cfg": {"a": 1, "b": [True, None, 1.5]}, "flag": False, "nothing": None}, parity=["N-log-01", "E-17"])
node(F, "log", "builtins", {"message": "{{timestamp}}|{{date}}|{{time}}|{{year}}-{{month}}-{{day}} {{hour}}:{{minute}}:{{second}}|{{weekday}}|{{weekday_num}}|q{{quarter}}|w{{week_of_year}}|d{{day_of_year}}|{{iso_timestamp}}|{{session_date}}"},
     parity=["E-12", "E-13"])
node(F, "log", "builtins_after_midnight", {"message": "{{date}} {{session_date}} {{weekday}}"}, clock="2026-11-03T01:45:00",
     parity=["E-12", "E-13"])
node(F, "log", "builtin_wins_over_variable", {"message": "{{date}} {{ date }}"}, vars_in={"date": "stored"},
     parity=["E-12"])
node(F, "log", "list_index_paths", {"message": "{{h.data[0].close}} {{h.data[-1].close}} {{h.data[5].close}} {{h.data.x}}"},
     vars_in={"h": {"data": [{"close": 1.0}, {"close": 2.5}]}}, parity=["E-14"])
node(F, "log", "empty_message", {}, parity=["N-log-01"])

# --- variable ---------------------------------------------------------------------

VV = {"count": 3, "label": "x", "payload": {"data": {"ltp": 812.5, "rows": [{"q": 1}, {"q": 2}]}}, "txt": "abc"}
for nm, data in (
    ("set_text", {"variableName": "msg", "operation": "set", "value": "LTP {{payload.data.ltp}}"}),
    ("set_number", {"variableName": "n", "operation": "set", "value": 5}),
    ("set_json_object", {"variableName": "obj", "operation": "set", "value": "{\"a\": 1, \"b\": [1, 2]}"}),
    ("set_json_list", {"variableName": "arr", "operation": "set", "value": "[1, 2.5, \"x\"]"}),
    ("set_bad_json_stays_text", {"variableName": "t", "operation": "set", "value": "{oops"}),
    ("default_operation_is_set", {"variableName": "t", "value": "hello"}),
    ("name_alias", {"name": "alias", "value": "v"}),
    ("get_path", {"variableName": "ltp", "operation": "get", "sourceVariable": "payload", "jsonPath": "data.ltp"}),
    ("get_list_path", {"variableName": "q2", "operation": "get", "sourceVariable": "payload", "jsonPath": "data.rows[1].q"}),
    ("get_whole", {"variableName": "copy", "operation": "get", "sourceVariable": " payload "}),
    ("get_missing_source", {"variableName": "x", "operation": "get", "sourceVariable": "nope"}),
    ("get_no_source", {"variableName": "x", "operation": "get"}),
    ("get_bad_path", {"variableName": "x", "operation": "get", "sourceVariable": "payload", "jsonPath": "data..ltp"}),
    ("get_bad_index", {"variableName": "x", "operation": "get", "sourceVariable": "payload", "jsonPath": "data.rows[a]"}),
    ("get_index_out_of_range", {"variableName": "x", "operation": "get", "sourceVariable": "payload", "jsonPath": "data.rows[5]"}),
    ("get_key_on_list", {"variableName": "x", "operation": "get", "sourceVariable": "payload", "jsonPath": "data.rows.q"}),
    ("get_missing_key", {"variableName": "x", "operation": "get", "sourceVariable": "payload", "jsonPath": "data.oi"}),
    ("get_path_not_text", {"variableName": "x", "operation": "get", "sourceVariable": "payload", "jsonPath": 5}),
    ("get_negative_index", {"variableName": "x", "operation": "get", "sourceVariable": "payload", "jsonPath": "data.rows[-1]"}),
    ("get_index_on_object", {"variableName": "x", "operation": "get", "sourceVariable": "payload", "jsonPath": "[0]"}),
    ("add", {"variableName": "count", "operation": "add", "value": "2"}),
    ("add_new_variable", {"variableName": "fresh", "operation": "add", "value": "2.5"}),
    ("subtract", {"variableName": "count", "operation": "subtract", "value": "{{count}}"}),
    ("multiply", {"variableName": "count", "operation": "multiply", "value": "1.5"}),
    ("divide", {"variableName": "count", "operation": "divide", "value": "4"}),
    ("divide_by_zero", {"variableName": "count", "operation": "divide", "value": "0"}),
    ("add_text_value", {"variableName": "count", "operation": "add", "value": "two"}),
    ("add_to_text_variable", {"variableName": "txt", "operation": "add", "value": "1"}),
    ("increment", {"variableName": "count", "operation": "increment"}),
    ("decrement_new", {"variableName": "fresh", "operation": "decrement"}),
    ("parse_json", {"variableName": "parsed", "operation": "parse_json", "value": "{\"k\": [true, null]}"}),
    ("parse_json_bad", {"variableName": "parsed", "operation": "parse_json", "value": "nope"}),
    ("stringify", {"variableName": "s", "operation": "stringify", "sourceVariable": "payload"}),
    ("stringify_missing", {"variableName": "s", "operation": "stringify", "sourceVariable": "nope"}),
    ("append_existing", {"variableName": "label", "operation": "append", "value": "-{{count}}"}),
    ("append_new", {"variableName": "newtxt", "operation": "append", "value": "start"}),
    ("unknown_operation", {"variableName": "x", "operation": "square", "value": "2"}),
    ("empty_name", {"variableName": "", "operation": "set", "value": "lost"}),
):
    node(F, "variable", nm, data, vars_in=VV, parity=["N-variable-01", "N-variable-02"])

# --- telegramAlert / whatsappAlert --------------------------------------------------

node(F, "telegramAlert", "happy", {"message": "Bought {{sym}} at {{q.data.ltp}}"},
     vars_in={"sym": "SBIN", "q": {"data": {"ltp": 812.5}}}, parity=["N-telegramAlert-01", "N-telegramAlert-02"])
node(F, "telegramAlert", "bot_stopped", {"message": "x"}, alert_reply={"status": "error", "error": "Telegram bot is stopped"},
     parity=["N-telegramAlert-03", "N-telegramAlert-05", "CL-22"])
node(F, "telegramAlert", "not_linked", {"message": "x"},
     alert_reply={"status": "error", "error": "No Telegram account linked for user: trader"}, parity=["CL-22"])
node(F, "telegramAlert", "empty_message", {}, parity=["N-telegramAlert-01"])
node(F, "telegramAlert", "notifications_disabled", {"message": "x"},
     alert_reply={"status": "error", "error": "Telegram notifications are disabled"}, parity=["CL-22"])
node(F, "telegramAlert", "client_raises", {"message": "x"}, alert_reply={"status": "error", "error": "Event loop is closed"},
     parity=["CL-22"])
node(F, "whatsappAlert", "to_self", {"message": "Filled {{sym}}"}, vars_in={"sym": "SBIN"},
     alert_reply={"status": "success", "data": {"sent": ["self"], "failed": [], "skipped": []}},
     parity=["N-whatsappAlert-01", "N-whatsappAlert-02"])
node(F, "whatsappAlert", "to_number", {"message": "Filled", "to": "{{phone}}"}, vars_in={"phone": "+91 98765 43210"},
     alert_reply={"status": "success", "data": {"sent": ["919876543210"], "failed": [], "skipped": []}},
     parity=["N-whatsappAlert-01"])
node(F, "whatsappAlert", "invalid_number", {"message": "x", "to": "abc"},
     alert_reply={"status": "error", "error": "Invalid WhatsApp phone number: abc"}, parity=["CL-21", "N-whatsappAlert-03"])
node(F, "whatsappAlert", "not_paired", {"message": "x"},
     alert_reply={"status": "error", "error": "WhatsApp is not paired or not connected. Pair the device from /whatsapp."},
     parity=["N-whatsappAlert-03", "CL-21"])

# --- httpRequest ------------------------------------------------------------------

PUBLIC = {"resolve": {"api.flow-golden.test": ["93.184.216.34"]}}
JSON_OK = {"status": 200, "body": "{\"ok\": true, \"id\": 7}", "content_type": "application/json"}
H = {"url": "http://api.flow-golden.test/v1/signal", "method": "GET", "outputVariable": "resp"}
node(F, "httpRequest", "get_json", H, env={"http": {**PUBLIC, "response": JSON_OK}},
     parity=["N-httpRequest-01", "N-httpRequest-02", "N-httpRequest-10", "N-httpRequest-12"])
node(F, "httpRequest", "get_text", {**H, "method": "get"},
     env={"http": {**PUBLIC, "response": {"status": 200, "body": "pong", "content_type": "text/plain"}}},
     parity=["N-httpRequest-12"])
node(F, "httpRequest", "post_json_body", {**H, "method": "POST", "body": "{\"symbol\": \"{{sym}}\", \"qty\": {{qty}}}",
                                          "headers": "{\"Authorization\": \"Bearer {{token}}\", \"X-Tag\": \"flow\"}"},
     vars_in={"sym": "SBIN", "qty": 10, "token": "t-123"},
     env={"http": {**PUBLIC, "response": JSON_OK, "record_headers": ["authorization", "x-tag"]}},
     parity=["N-httpRequest-03", "N-httpRequest-04", "N-httpRequest-05"])
node(F, "httpRequest", "post_text_body", {**H, "method": "POST", "body": "symbol={{sym}}"}, vars_in={"sym": "SBIN"},
     env={"http": {**PUBLIC, "response": JSON_OK}}, parity=["N-httpRequest-05"])
node(F, "httpRequest", "put_empty_body", {**H, "method": "PUT"}, env={"http": {**PUBLIC, "response": JSON_OK}},
     parity=["N-httpRequest-05"])
node(F, "httpRequest", "patch", {**H, "method": "PATCH", "body": "[1, 2]"}, env={"http": {**PUBLIC, "response": JSON_OK}},
     parity=["N-httpRequest-05"])
node(F, "httpRequest", "delete", {**H, "method": "DELETE"}, env={"http": {**PUBLIC, "response": {"status": 204, "body": ""}}},
     parity=["N-httpRequest-05"])
node(F, "httpRequest", "headers_object", {**H, "headers": {"X-Key": "{{k}}"}}, vars_in={"k": "v1"},
     env={"http": {**PUBLIC, "response": JSON_OK, "record_headers": ["x-key"]}}, parity=["N-httpRequest-04"])
node(F, "httpRequest", "header_value_cannot_inject", {**H, "headers": "{\"X-Note\": \"{{note}}\"}"},
     vars_in={"note": "x\", \"X-Injected\": \"yes"},
     env={"http": {**PUBLIC, "response": JSON_OK, "record_headers": ["x-note", "x-injected"]}}, parity=["N-httpRequest-04"])
node(F, "httpRequest", "headers_invalid_json", {**H, "headers": "{bad"}, parity=["N-httpRequest-04"])
node(F, "httpRequest", "headers_not_object", {**H, "headers": "[1]"}, parity=["N-httpRequest-04"])
node(F, "httpRequest", "query_redacted_in_log", {**H, "url": "http://user:pw@api.flow-golden.test:8080/v1?key={{k}}"},
     vars_in={"k": "secret-ish"}, env={"http": {**PUBLIC, "response": JSON_OK}}, parity=["N-httpRequest-06"])
node(F, "httpRequest", "url_interpolated_once", {**H, "url": "http://api.flow-golden.test/r/{{webhook.path}}"},
     vars_in={"webhook": {"path": "{{funds.data.availablecash}}"}, "funds": {"data": {"availablecash": 1}}},
     env={"http": {**PUBLIC, "response": JSON_OK}}, parity=["N-httpRequest-02"])
node(F, "httpRequest", "not_found_status", H, env={"http": {**PUBLIC, "response": {"status": 404, "body": "{\"error\": \"no\"}"}}},
     parity=["N-httpRequest-12", "N-httpRequest-13"],
     desktop_expected=de("N-httpRequest-13", "WEB-DEFECT: a non-2xx result has no message, so the run reads `node failed`; the desktop adds `message: HTTP {statusCode}`.",
                         fix=["http_status_message"]))
node(F, "httpRequest", "unsupported_method", {**H, "method": "HEAD"}, env={"http": PUBLIC}, parity=["N-httpRequest-05"])
node(F, "httpRequest", "no_url", {"method": "GET"}, parity=["N-httpRequest-01"])
node(F, "httpRequest", "url_not_text", {"url": 12345}, parity=["N-httpRequest-01"])
for nm, url, env in (
    ("refuses_loopback", "http://127.0.0.1:5000/api/v1/funds", None),
    ("refuses_private", "http://10.0.0.5/admin", None),
    ("refuses_metadata", "http://169.254.169.254/latest/meta-data/", None),
    ("refuses_name_to_private", "http://router.flow-golden.test/", {"resolve": {"router.flow-golden.test": ["192.168.1.1"]}}),
    ("refuses_ipv6_loopback", "http://[::1]/", None),
    ("refuses_scheme", "ftp://api.flow-golden.test/file", None),
    ("refuses_no_scheme", "api.flow-golden.test/x", None),
    ("refuses_no_host", "http:///path", None),
    ("refuses_unresolvable", "http://nowhere.flow-golden.test/", None),
):
    node(F, "httpRequest", nm, {**H, "url": url}, env={"http": env} if env else None, parity=["N-httpRequest-06", "N-httpRequest-07"])
node(F, "httpRequest", "port_not_a_number", {**H, "url": "http://api.flow-golden.test:ab/"}, env={"http": PUBLIC},
     parity=["N-httpRequest-19"],
     desktop_expected=de("N-httpRequest-19", "WEB-DEFECT: an invalid port raises out of the guard and aborts the run; the desktop fails only the node.",
                         fix=["http_bad_port"]))
node(F, "httpRequest", "carrier_grade_nat", {**H, "url": "http://tailnet.flow-golden.test/"},
     env={"http": {"resolve": {"tailnet.flow-golden.test": ["100.64.1.1"]}, "response": JSON_OK}},
     parity=["N-httpRequest-07"],
     desktop_expected=de("N-httpRequest-07", "WEB-DEFECT: 100.64.0.0/10 (carrier-grade NAT, the Tailscale range) is reachable on the web; the desktop refuses it like a private address (decisions.md 12).",
                         fix=["http_cgnat"]))
node(F, "httpRequest", "bad_port", {**H, "url": "http://api.flow-golden.test:99999/"}, env={"http": PUBLIC},
     parity=["N-httpRequest-19"],
     desktop_expected=de("N-httpRequest-19", "WEB-DEFECT: an invalid port raises out of the guard and aborts the run; the desktop fails only the node.",
                         fix=["http_bad_port"]))
node(F, "httpRequest", "object_body", {**H, "method": "POST", "body": {"symbol": "SBIN"}},
     env={"http": {**PUBLIC, "response": JSON_OK}}, parity=["N-httpRequest-11"],
     desktop_expected=de("N-httpRequest-11", "WEB-DEFECT: an object body raises TypeError and aborts the run; the desktop sends it as JSON.",
                         fix=["http_object_body"]))
node(F, "httpRequest", "read_timeout", {**H, "timeout": 100},
     env={"http": {**PUBLIC, "response": {"status": 200, "body": "{}", "delay": 0.6}}}, parity=["N-httpRequest-14"])
node(F, "httpRequest", "timeout_zero_is_default", {**H, "timeout": 0}, env={"http": {**PUBLIC, "response": JSON_OK}},
     parity=["N-httpRequest-14"])

# --- positionCheck ----------------------------------------------------------------

PC = {"symbol": "SBIN", "exchange": "NSE", "product": "MIS", "condition": "exists"}
for nm, extra, book in (
    ("exists_true", {}, POSITIONS),
    ("exists_false", {"symbol": "TCS"}, POSITIONS),
    ("not_exists", {"condition": "not_exists", "symbol": "TCS"}, POSITIONS),
    ("quantity_above", {"condition": "quantity_above", "threshold": 5}, POSITIONS),
    ("quantity_below", {"condition": "quantity_below", "threshold": "{{limit}}"}, POSITIONS),
    ("pnl_above", {"condition": "pnl_above", "threshold": 100}, POSITIONS),
    ("pnl_below", {"condition": "pnl_below", "threshold": -50.5}, POSITIONS),
    ("default_condition_and_product_nfo", {"symbol": "NIFTY25NOV2624000CE", "exchange": "NFO", "condition": "", "product": ""}, POSITIONS),
    ("empty_book", {}, ok(data=[])),
):
    node(F, "positionCheck", nm, {**PC, **extra}, vars_in={"limit": 20}, script=[reply("positionbook", book)],
         parity=["N-positionCheck-01", "N-positionCheck-02", "N-positionCheck-03", "N-positionCheck-04"])
node(F, "positionCheck", "unknown_condition", {**PC, "condition": "flat"}, script=[reply("positionbook", POSITIONS)],
     parity=["N-positionCheck-05"])
node(F, "positionCheck", "no_symbol", {**PC, "symbol": ""}, parity=["N-positionCheck-06"])
node(F, "positionCheck", "book_fails", PC, script=[reply("positionbook", SESSION_EXPIRED)], parity=["N-positionCheck-07"])
node(F, "positionCheck", "book_fails_without_text", PC, script=[reply("positionbook", {"status": "error"})],
     parity=["N-positionCheck-07"])
node(F, "positionCheck", "quantity_decimal_text", PC,
     script=[reply("positionbook", ok(data=[{"symbol": "SBIN", "exchange": "NSE", "product": "MIS", "quantity": "10.0", "pnl": "5"}]))],
     parity=["N-positionCheck-08"],
     desktop_expected=de("N-positionCheck-08", "WEB-DEFECT: int('10.0') raises and aborts the run; the desktop parses the quantity as a number.",
                         fix=["position_quantity_float"]))
node(F, "positionCheck", "threshold_unresolved", {**PC, "condition": "quantity_above", "threshold": "{{typo}}"},
     script=[reply("positionbook", POSITIONS)], parity=["N-positionCheck-08"],
     desktop_expected=de("N-positionCheck-08", "WEB-DEFECT: an unparseable threshold silently becomes 0.0; the desktop fails closed (the sentence is the lane's).",
                         outputs={"status": "error", "condition": False, "message": ANY},
                         logs={"$any": "logs"}, next=["pass"], conditions_out={},
                         error={"errors": [{"node": "n1", "type": "positionCheck", "message": ANY}], "abort": None}))

# --- fundCheck --------------------------------------------------------------------

node(F, "fundCheck", "met", {"minAvailable": 100000}, script=[reply("funds", FUNDS)],
     parity=["N-fundCheck-01", "N-fundCheck-03", "N-fundCheck-04", "N-fundCheck-05"])
node(F, "fundCheck", "not_met", {"minAvailable": "{{need}}"}, vars_in={"need": 500000}, script=[reply("funds", FUNDS)],
     parity=["N-fundCheck-01", "N-fundCheck-05"])
node(F, "fundCheck", "legacy_threshold", {"operator": "gte", "threshold": 1000}, script=[reply("funds", FUNDS)],
     parity=["N-fundCheck-02"])
node(F, "fundCheck", "legacy_default_operator", {"threshold": 1000}, script=[reply("funds", FUNDS)], parity=["N-fundCheck-02"])
node(F, "fundCheck", "legacy_less_than_refused", {"operator": "lt", "threshold": 1000}, parity=["N-fundCheck-02"])
node(F, "fundCheck", "no_minimum", {}, parity=["N-fundCheck-03"])
node(F, "fundCheck", "funds_fail", {"minAvailable": 1}, script=[reply("funds", SESSION_EXPIRED)], parity=["N-fundCheck-04"])
node(F, "fundCheck", "cash_missing_reads_zero", {"minAvailable": 0}, script=[reply("funds", ok(data={}))],
     parity=["N-fundCheck-05"])
node(F, "fundCheck", "minimum_unresolved", {"minAvailable": "{{typo}}"}, script=[reply("funds", FUNDS)],
     parity=["N-fundCheck-06"],
     desktop_expected=de("N-fundCheck-06", "WEB-DEFECT: an unparseable minimum becomes 0.0 and the guard passes; the desktop fails closed before reading funds (the sentence is the lane's).",
                         calls={"$any": "calls"}, outputs={"status": "error", "condition": False, "message": ANY},
                         logs={"$any": "logs"}, next=["pass"], conditions_out={},
                         error={"errors": [{"node": "n1", "type": "fundCheck", "message": ANY}], "abort": None}))

# --- priceCondition ---------------------------------------------------------------

PR = {"symbol": "SBIN", "exchange": "NSE", "field": "ltp", "operator": ">", "value": 800}
for nm, extra in (
    ("ltp_above", {}),
    ("open_below", {"field": "open", "operator": "<", "value": "805"}),
    ("high_gte", {"field": "high", "operator": ">=", "value": 818}),
    ("low_lte_word", {"field": "low", "operator": "lte", "value": 800}),
    ("prev_close_eq", {"field": "prev_close", "operator": "==", "value": "{{pc}}"}),
    ("change_percent", {"field": "change_percent", "operator": "!=", "value": 0}),
    ("legacy_threshold", {"value": None, "threshold": 900}),
    ("value_absent_is_zero", {"value": ""}),
):
    data = {**PR, **extra}
    data = {k: v for k, v in data.items() if v is not None}
    node(F, "priceCondition", nm, data, vars_in={"pc": 800}, script=[reply("quotes", QUOTE)],
         parity=["N-priceCondition-01", "N-priceCondition-02", "N-priceCondition-03", "N-priceCondition-04"])
node(F, "priceCondition", "bad_field", {**PR, "field": "vwap"}, parity=["N-priceCondition-01"])
node(F, "priceCondition", "bad_operator", {**PR, "operator": "=>"}, parity=["N-priceCondition-01"])
node(F, "priceCondition", "value_not_number", {**PR, "value": "{{level}}"}, parity=["N-priceCondition-02"])
node(F, "priceCondition", "quote_fails", PR, script=[reply("quotes", SESSION_EXPIRED)], parity=["N-priceCondition-03"])
node(F, "priceCondition", "missing_prev_close", {**PR, "field": "change_percent"},
     script=[reply("quotes", ok(data={"ltp": 812.5}))], parity=["N-priceCondition-05"],
     desktop_expected=de("N-priceCondition-05", "WEB-DEFECT: a quote without prev_close evaluates change_percent as 0.0; the desktop fails closed.",
                         fix=["price_condition_missing_field"]))
node(F, "priceCondition", "missing_field", {**PR, "field": "high"}, script=[reply("quotes", ok(data={"ltp": 812.5}))],
     parity=["N-priceCondition-05"],
     desktop_expected=de("N-priceCondition-05", "WEB-DEFECT: a quote missing the chosen field evaluates on 0.0; the desktop fails closed.",
                         fix=["price_condition_missing_field"]))

# --- varCondition -----------------------------------------------------------------

VC = {"leftValue": "{{rsi.latest.value}}", "operator": "<", "rightValue": "30"}
VCV = {"rsi": {"latest": {"value": 27.4}}, "pdh": {"pdh": 818}}
for nm, extra in (
    ("below", {}),
    ("above_word", {"operator": "gt"}),
    ("literal_zero_left", {"leftValue": 0, "operator": "==", "rightValue": "0.0"}),
    ("float_text", {"leftValue": " 1e2 ", "operator": ">=", "rightValue": "{{pdh.pdh}}"}),
    ("not_equal", {"operator": "!=", "rightValue": 27.4}),
    ("default_operator", {"operator": ""}),
):
    node(F, "varCondition", nm, {**VC, **extra}, vars_in=VCV, parity=["N-varCondition-01", "N-varCondition-02"])
node(F, "varCondition", "empty_left", {**VC, "leftValue": ""}, vars_in=VCV, parity=["N-varCondition-03"])
node(F, "varCondition", "unresolved_right", {**VC, "rightValue": "{{threshold}}"}, vars_in=VCV, parity=["N-varCondition-03"])
node(F, "varCondition", "not_a_number", {**VC, "leftValue": "high"}, vars_in=VCV, parity=["N-varCondition-03"])
node(F, "varCondition", "bad_operator", {**VC, "operator": "~"}, vars_in=VCV, parity=["N-varCondition-04"])
node(F, "varCondition", "none_operand", {**VC, "leftValue": None}, vars_in=VCV, parity=["N-varCondition-03"])

# --- timeWindow / timeCondition -----------------------------------------------------

for nm, data, clock in (
    ("inside", {"startTime": "09:15", "endTime": "15:30"}, None),
    ("outside", {"startTime": "11:00", "endTime": "15:30"}, None),
    ("inverted", {"startTime": "09:15", "endTime": "15:30", "invertCondition": True}, None),
    ("defaults", {}, "2026-10-29T15:31:00"),
    ("edge_inclusive", {"startTime": "10:15", "endTime": "10:15"}, None),
    ("overnight_inside", {"startTime": "22:00", "endTime": "02:00"}, "2026-10-29T23:30:00"),
    ("overnight_outside", {"startTime": "22:00", "endTime": "02:00"}, None),
):
    node(F, "timeWindow", nm, data, clock=clock, parity=["N-timeWindow-01", "N-timeWindow-02", "N-timeWindow-03", "N-logic-09"])
node(F, "timeWindow", "invert_text_false", {"startTime": "09:15", "endTime": "15:30", "invertCondition": "false"},
     parity=["N-timeWindow-04"],
     desktop_expected=de("N-timeWindow-04", "WEB-DEFECT: bool('false') is True, so the text inverts the window; the desktop reads only true/'true'/'1'.",
                         fix=["time_window_invert_text"]))
for nm, data, clock in (
    ("after", {"targetTime": "09:30", "operator": ">="}, None),
    ("before", {"targetTime": "09:30", "operator": "<="}, None),
    ("strictly_after_seconds", {"targetTime": "10:14:59", "operator": ">"}, None),
    ("strictly_before", {"targetTime": "10:15:01", "operator": "<", "conditionType": "exit"}, None),
    ("equal_minute", {"targetTime": "10:15:45", "operator": "=="}, "2026-10-29T10:15:20"),
    ("defaults", {}, None),
):
    node(F, "timeCondition", nm, data, clock=clock, parity=["N-timeCondition-01", "N-timeCondition-02", "N-timeCondition-03"])
node(F, "timeCondition", "bad_operator", {"targetTime": "09:30", "operator": "after"}, parity=["N-timeCondition-04"])

# --- fields that are NOT interpolated ----------------------------------------------

node(F, "timeWindow", "times_not_interpolated", {"startTime": "{{open}}", "endTime": "{{close}}"},
     vars_in={"open": "11:00", "close": "12:00"}, parity=["N-timeWindow-01"])
node(F, "timeCondition", "target_not_interpolated", {"targetTime": "{{t}}", "operator": ">="}, vars_in={"t": "11:00"},
     parity=["N-timeCondition-01"])
node(F, "waitUntil", "target_not_interpolated", {"targetTime": "{{t}}"}, vars_in={"t": "10:20"}, parity=["N-waitUntil-01"])
node(F, "positionCheck", "interpolated_symbol", {**PC, "symbol": "{{webhook.symbol}}", "exchange": "{{webhook.ex}}"},
     vars_in={"webhook": {"symbol": "SBIN", "ex": "NSE"}}, script=[reply("positionbook", POSITIONS)], parity=["N-positionCheck-01"])
node(F, "priceCondition", "interpolated_symbol_field_operator", {**PR, "symbol": "{{s}}", "field": "{{f}}", "operator": "{{o}}"},
     vars_in={"s": "SBIN", "f": "high", "o": ">"}, script=[reply("quotes", QUOTE)], parity=["N-priceCondition-01"])
node(F, "whatsappAlert", "unresolved_to_is_text", {"message": "x", "to": "{{nobody}}"},
     alert_reply={"status": "error", "error": "Invalid WhatsApp phone number: {{nobody}}"}, parity=["N-whatsappAlert-01"])
