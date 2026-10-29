"""Core: start, group, the gates, and the walk itself (lane core)."""

from lib import ORDER, QUOTE, node, ok, reply, workflow

F = "core"


def de(parity, why, fix=None, **sets):
    d = {"parity": parity, "why": why}
    if fix:
        d["fix"] = fix
    if sets:
        d["set"] = sets
    return d


node(F, "start", "logs_and_continues", {"scheduleType": "daily", "time": "09:15", "days": [0, 1, 2, 3, 4]},
     parity=["N-start-00", "N-start-01"])
node(F, "group", "passes_through_silently", {"label": "Entries"}, parity=["N-group-00", "N-group-01"])
node(F, "fooBar", "unknown_type_continues", {"x": 1}, parity=["E-27"])

# --- gates ------------------------------------------------------------------------

for gate in ("andGate", "orGate"):
    for nm, inputs in (("true_true", [True, True]), ("true_false", [True, False]), ("false_false", [False, False]),
                       ("three_inputs", [True, True, False])):
        node(F, gate, nm, {"inputCount": len(inputs)}, inputs=inputs,
             parity=["N-gate-01", "N-gate-02", "N-gate-05" if gate == "andGate" else "N-gate-06", "N-gate-08"])
    node(F, gate, "no_inputs", {}, parity=["N-gate-05" if gate == "andGate" else "N-gate-06"],
         desktop_expected=de("N-gate-04", "decisions.md 8: an absent inputCount counts as 2 slots for AND/OR, so a gate with no wires fails instead of deciding.",
                             fix=["gate_slot_count"]))
    node(F, gate, "waiting_for_input", {"inputCount": 2}, inputs=[True, None], parity=["N-gate-02"])
    node(F, gate, "declared_more_than_wired", {"inputCount": 3}, inputs=[True, True], parity=["N-gate-03"])
    node(F, gate, "absent_count_one_wire", {}, inputs=[True], parity=["N-gate-04"],
         desktop_expected=de("N-gate-04", "WEB-DEFECT (decisions.md 8): an absent inputCount skips the check, so a default two-slot gate decides on one wire; the desktop counts it as 2 and refuses.",
                             fix=["gate_slot_count"]))
    node(F, gate, "text_count", {"inputCount": "3"}, inputs=[True, True], parity=["N-gate-03", "N-gate-04"],
         desktop_expected=de("N-gate-04", "decisions.md 8: AND/OR read inputCount as the validator does (int('3') = 3), so a text count is not ignored.",
                             fix=["gate_slot_count"]))
    node(F, gate, "boolean_count", {"inputCount": True}, inputs=[True, False], parity=["N-gate-03"])
    node(F, gate, "already_evaluated", {"inputCount": 2}, inputs=[True, True], parity=["N-gate-01"],
         nodes_extra=[], edges_extra=[])
for nm, inputs in (("true", [True]), ("false", [False]), ("extra_inputs_ignored", [False, True])):
    node(F, "notGate", nm, {}, inputs=inputs, parity=["N-gate-07", "N-gate-08"])
node(F, "notGate", "no_inputs", {}, parity=["N-gate-07"])
node(F, "notGate", "count_over_wired", {"inputCount": 2}, inputs=[True], parity=["N-gate-03"])
node(F, "notGate", "pending", {}, inputs=[None], parity=["N-gate-02"])

# The once-per-run guards: a gate or condition already evaluated returns.
import lib as _lib  # noqa: E402

for s in _lib.SCENARIOS:
    if s["type"] in ("andGate", "orGate") and s["scenario"] == "already_evaluated":
        s["conditions_in"]["n1"] = True
node(F, "priceCondition", "condition_once_per_run", {"symbol": "SBIN", "field": "ltp", "operator": ">", "value": 1},
     script=[reply("quotes", QUOTE)], parity=["N-logic-04", "E-22"])
for s in _lib.SCENARIOS:
    if s["type"] == "priceCondition" and s["scenario"] == "condition_once_per_run":
        s["conditions_in"]["n1"] = False

# --- whole workflows: the walk ----------------------------------------------------

POS = {"x": 0, "y": 0}


def n(id_, type_, data=None, x=0, y=0):
    return {"id": id_, "type": type_, "position": {"x": x, "y": y}, "data": data or {}}


def e(src, dst, handle=None, id_=None):
    edge = {"id": id_ or f"e-{src}-{dst}-{handle or 'x'}", "source": src, "target": dst}
    if handle:
        edge["sourceHandle"] = handle
    return edge


START = n("start", "start", {"scheduleType": "daily", "time": "09:15"})


def wf(name, nodes, edges):
    return {"name": name, "nodes": nodes, "edges": edges}


workflow(F, "linear_success", wf("Linear", [START, n("q", "getQuote", {"symbol": "SBIN", "exchange": "NSE", "outputVariable": "q"}),
                                            n("log", "log", {"message": "LTP {{q.data.ltp}}"})],
                                  [e("start", "q"), e("q", "log")]),
         script=[reply("quotes", QUOTE)], parity=["E-30", "E-31", "E-35", "E-36"])
workflow(F, "branch_true_and_pass_through", wf("Branch", [
    START, n("c", "varCondition", {"leftValue": "5", "operator": ">", "rightValue": "3"}),
    n("yes", "log", {"message": "yes"}), n("no", "log", {"message": "no"}), n("always", "log", {"message": "always"})],
    [e("start", "c"), e("c", "yes", "true"), e("c", "no", "false"), e("c", "always")]), parity=["E-24", "N-logic-03"])
workflow(F, "yes_no_handles", wf("YesNo", [
    START, n("c", "timeCondition", {"targetTime": "09:30", "operator": ">="}),
    n("y", "log", {"message": "after"}), n("nn", "log", {"message": "before"})],
    [e("start", "c"), e("c", "y", "yes"), e("c", "nn", "no")]), parity=["E-24", "N-logic-02"])
workflow(F, "errored_condition_takes_no_branch", wf("Errored", [
    START, n("c", "varCondition", {"leftValue": "{{missing}}", "operator": ">", "rightValue": "3"}),
    n("yes", "log", {"message": "yes"}), n("no", "log", {"message": "no"}), n("always", "log", {"message": "always"})],
    [e("start", "c"), e("c", "yes", "true"), e("c", "no", "false"), e("c", "always")]), parity=["E-24", "N-logic-01"])
workflow(F, "failed_node_stops_branch_only", wf("Siblings", [
    START, n("bad", "placeOrder", {"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 1, "priceType": "MARKET"}),
    n("after", "log", {"message": "after bad"}), n("other", "log", {"message": "sibling runs"})],
    [e("start", "bad"), e("bad", "after"), e("start", "other")]),
    script=[reply("placeorder", {"status": "error", "error": "RMS: margin exceeds", "code": 400})],
    parity=["N-common-16", "E-37"])
workflow(F, "and_gate_diamond", wf("Gate", [
    START, n("a", "varCondition", {"leftValue": "1", "operator": "==", "rightValue": "1"}),
    n("b", "varCondition", {"leftValue": "2", "operator": ">", "rightValue": "5"}),
    n("g", "andGate", {"inputCount": 2}), n("t", "log", {"message": "both"}), n("f", "log", {"message": "not both"})],
    [e("start", "a"), e("start", "b"), e("a", "g", "true"), e("b", "g", "false"), e("g", "t", "true"), e("g", "f", "false")]),
    parity=["N-gate-02", "N-gate-08", "E-25"])
workflow(F, "or_gate_false_input_still_delivered", wf("OrGate", [
    START, n("a", "varCondition", {"leftValue": "1", "operator": ">", "rightValue": "2"}),
    n("b", "varCondition", {"leftValue": "3", "operator": ">", "rightValue": "2"}),
    n("g", "orGate", {"inputCount": 2}), n("t", "log", {"message": "either"})],
    [e("start", "a"), e("start", "b"), e("a", "g", "true"), e("b", "g", "true"), e("g", "t", "true")]),
    parity=["E-25"])
workflow(F, "errored_input_keeps_gate_pending", wf("Pending", [
    START, n("a", "varCondition", {"leftValue": "{{nope}}", "operator": ">", "rightValue": "2"}),
    n("b", "varCondition", {"leftValue": "3", "operator": ">", "rightValue": "2"}),
    n("g", "andGate", {"inputCount": 2}), n("t", "log", {"message": "fired"})],
    [e("start", "a"), e("start", "b"), e("a", "g", "true"), e("b", "g", "true"), e("g", "t", "true")]),
    parity=["E-25", "N-gate-08"])
workflow(F, "not_gate_yes_no", wf("Not", [
    START, n("a", "varCondition", {"leftValue": "1", "operator": ">", "rightValue": "2"}),
    n("g", "notGate", {}), n("y", "log", {"message": "inverted"}), n("nn", "log", {"message": "no"})],
    [e("start", "a"), e("a", "g", "false"), e("g", "y", "yes"), e("g", "nn", "no")]), parity=["N-gate-07"])
workflow(F, "condition_reached_twice_runs_once", wf("Diamond", [
    START, n("l1", "log", {"message": "path 1"}), n("l2", "log", {"message": "path 2"}),
    n("c", "varCondition", {"leftValue": "1", "operator": "==", "rightValue": "1"}),
    n("o", "placeOrder", {"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 1, "priceType": "MARKET"})],
    [e("start", "l1"), e("start", "l2"), e("l1", "c"), e("l2", "c"), e("c", "o", "true")]),
    script=[reply("placeorder", ORDER)], parity=["E-22", "N-logic-04"])
workflow(F, "plain_node_reached_twice_runs_twice", wf("Merge", [
    START, n("l1", "log", {"message": "path 1"}), n("l2", "log", {"message": "path 2"}),
    n("o", "placeOrder", {"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 1, "priceType": "MARKET"})],
    [e("start", "l1"), e("start", "l2"), e("l1", "o"), e("l2", "o")]),
    script=[reply("placeorder", ORDER)], parity=["E-29"], note="Parity with the web (decisions.md 8): a non-branching node reached by two paths runs once per path.")
node(F, "priceAlert", "reached_again_in_run", {"symbol": "SBIN", "exchange": "NSE", "condition": "above", "price": 800},
     script=[reply("quotes", QUOTE)], parity=["N-logic-04"],
     desktop_expected=de("N-logic-04", "WEB-DEFECT (decisions.md 8): priceAlert branches but is not once-per-run, so a second path re-fetches the quote and re-fires its branch; the desktop returns at once like the other branching nodes.",
                         fix=["price_alert_branches"]))
workflow(F, "webhook_payload_injected", wf("Webhook", [
    n("t", "webhookTrigger", {}), n("o", "placeOrder", {"symbol": "{{webhook.symbol}}", "exchange": "NSE",
                                                       "action": "{{webhook.action}}", "quantity": "{{webhook.qty}}",
                                                       "priceType": "MARKET", "outputVariable": "orderResult"}),
    n("log", "log", {"message": "order {{orderResult.orderid}} {{webhook}}"})],
    [e("t", "o"), e("o", "log")]),
    webhook={"symbol": "sbin", "action": "SELL", "qty": 3}, script=[reply("placeorder", ORDER)], parity=["E-32", "E-15"])
workflow(F, "unresolved_order_field_fails_run", wf("Unresolved", [
    n("t", "webhookTrigger", {}), n("o", "placeOrder", {"symbol": "SBIN", "exchange": "NSE", "action": "BUY",
                                                       "quantity": "{{webhook.qty}}", "priceType": "MARKET"})],
    [e("t", "o")]), webhook={"symbol": "SBIN"}, parity=["N-common-03", "E-37"])
workflow(F, "no_api_key", wf("NoKey", [START, n("l", "log", {"message": "x"})], [e("start", "l")]), api_key=False,
         parity=["CL-01", "E-34"])
workflow(F, "validation_refuses_run", wf("Invalid", [START, n("o", "placeOrder", {"exchange": "NSE", "action": "BUY",
                                                                                   "quantity": 0})],
                                          [e("start", "o")]), parity=["V-140", "E-33"])
workflow(F, "group_and_unknown_pass_through", wf("Group", [
    START, n("g", "group", {"label": "G"}), n("l", "log", {"message": "inside"})],
    [e("start", "g"), e("g", "l")]), parity=["N-group-01"])

_chain = [START] + [n(f"l{i}", "log", {"message": f"step {i}"}) for i in range(1, 103)]
_edges = [e("start", "l1")] + [e(f"l{i}", f"l{i + 1}") for i in range(1, 102)]
workflow(F, "max_depth", wf("Deep", _chain, _edges), parity=["E-01", "E-38"])
_layers = [START]
_layer_edges = []
_prev = ["start"]
for k in range(1, 10):
    cur = [f"a{k}", f"b{k}"]
    _layers += [n(c, "group", {"label": c}) for c in cur]
    for p in _prev:
        for c in cur:
            _layer_edges.append(e(p, c, id_=f"e-{p}-{c}"))
    _prev = cur
workflow(F, "max_visits", wf("Wide", _layers, _layer_edges), parity=["E-01", "E-38"])

for s in _lib.SCENARIOS:
    if s["type"] == "priceAlert" and s["scenario"] == "reached_again_in_run":
        s["conditions_in"]["n1"] = True

_simple = wf("Simple", [START, n("l", "log", {"message": "ran"})], [e("start", "l")])
workflow(F, "already_running", _simple, special="already_running", parity=["E-02", "E-03"])
workflow(F, "workflow_missing", _simple, special="workflow_missing", parity=["E-33"])
workflow(F, "no_execution_record", _simple, special="no_execution_record", parity=["E-33"])
workflow(F, "no_trigger_refused_by_validator", wf("NoTrigger", [n("l", "log", {"message": "x"})], []), parity=["E-34", "V-140"])
workflow(F, "http_failure_summary", wf("HttpFail", [START, n("h", "httpRequest", {"url": "http://api.flow-golden.test/x", "method": "GET"})],
                                     [e("start", "h")]),
         env={"http": {"resolve": {"api.flow-golden.test": ["93.184.216.34"]}, "response": {"status": 500, "body": "oops", "content_type": "text/plain"}}},
         parity=["N-httpRequest-13", "E-37"],
         desktop_expected=de("N-httpRequest-13", "WEB-DEFECT: the run summary reads `httpRequest: node failed`; the desktop names the status.",
                             fix=["http_status_message"]))
