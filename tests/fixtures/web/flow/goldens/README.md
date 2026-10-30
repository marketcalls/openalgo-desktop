# Flow behaviour goldens

What OpenAlgo web's own Flow executor (`services/flow_executor_service.py` at
web commit `96e5927e5`) did, node by node and workflow by workflow, recorded
against a scripted, recording stand-in for the services `FlowOpenAlgoClient`
calls. The desktop replays every file through its node registry and walk
(`src-tauri/src/flow/goldens/`, tests `goldens_<family>`) and
must do the same: the same client calls with the same payloads, the same
result, variables, condition results, run log, branch and failures.

## Families

One folder per lane, one file per scenario, named `<nodeType>__<scenario>.json`
(`workflow__<name>.json` for whole workflows):

| Folder | Node types |
|---|---|
| `orders/` | placeOrder, smartOrder, optionsOrder, optionsMultiOrder, modifyOrder, cancelOrder, cancelAllOrders, closePositions, basketOrder, splitOrder |
| `data/` | getQuote, getDepth, getOrderStatus, openPosition, history, strategyPnl, priorPeriodOhlc, barOffset, indicator, symbol, optionSymbol, expiry, intervals, multiQuotes, optionChain, syntheticFuture, calendar, holidays, timings, orderBook, tradeBook, positionBook, holdings, funds, margin |
| `logic/` | positionCheck, fundCheck, priceCondition, varCondition, timeWindow, timeCondition, variable, mathExpression, delay, waitUntil, log, telegramAlert, whatsappAlert, httpRequest |
| `triggers/` | priceAlert, webhookTrigger, orderUpdateTrigger, subscribeLtp, subscribeQuote, subscribeDepth, unsubscribe |
| `core/` | start, group, andGate, orGate, notGate, an unknown type, and whole workflows that pin the walk (branches, gates, once-per-run, depth and visit limits, the run's result) |
| `workflows/` | every JSON workflow in the web's `docs/prompt/flow-import-format.md` and the flow-builder skill, run end to end |

The node examples of the import-format doc are node goldens in their
family, named `<type>__doc_example_line<N>.json`.

## How a node golden runs

The node is `n1`, alone in `graph` with probe children (`type: "goldenProbe"`)
on every handle it can take: `true`/`false` (or `yes`/`no`) and `pass` (no
handle) for a condition or gate, `next` for any other node. The walk starts
at `start` (`n1`). A gate's inputs are `in1`, `in2`, ... whose stored results
are `conditions_in`. The web ran the same graph through its
`execute_node_chain`, so the dispatcher's own work (unresolved order fields,
the gate wait, "Stopping this branch") is part of every golden.

## Fields

| Field | Meaning |
|---|---|
| `format` | 1 |
| `web_commit` | the web commit recorded |
| `kind` | `node` or `workflow` |
| `family`, `type`, `scenario` | as the file name |
| `note` | what the scenario shows, where it is not obvious |
| `parity` | the parity.md IDs the golden is evidence for |
| `clock` | the frozen clock, IST wall time (`YYYY-MM-DDTHH:MM:SS`, decisions.md 1) |
| `analyzer` | whether the scripted replies are analyzer-mode replies (the executor never branches on it; the services decide) |
| `node` | node goldens: `{type, data}` |
| `workflow_name` | node goldens: the run's workflow name (the default order tag) |
| `vars_in` | variables stored before the node runs (`webhook` is the trigger payload) |
| `conditions_in` | condition results stored before the node runs (gate inputs, once-per-run) |
| `graph`, `start` | node goldens: the graph walked and where |
| `workflow`, `webhook`, `api_key` | workflow goldens: the stored workflow, the trigger payload, whether the run has an API key |
| `special` | workflow goldens the harness cannot run without the app: `already_running`, `workflow_missing`, `no_execution_record` |
| `run_context` | `{"trigger": "price_alert"}`: the run was started by the price monitor (a trusted flag, never a payload field; N-priceAlert-04) |
| `env` | what the node reads besides the client: `master_contract` rows (lot sizes; `"unreadable"` = the read fails), `strategy_book` legs (or `{"unavailable": msg}`), `feed` (`username`, `broker`, `connect_error`, `subscribe_error`, `ticks` delivered after subscribing, `unsubscribe_error`, `unsubscribe_raise`, `verify_raise`), `http` (`resolve`: host to the addresses the destination check sees; `response`: what the stub answers; `record_headers`), `calendar_holidays` (holiday entries per year; `"unavailable"` or `"raise"`) |
| `script` | the client's replies in call order, `{endpoint, reply}`, each reply in the shape the web client hands a node (`_handle_response`: `{"status": "success", ...}` or `{"status": "error", "error": msg, "code": http}`). Scripted as the desktop's `FakeFlowClient` answers: queued per endpoint, the last one repeats, an endpoint with no script answers `{"status": "error", "error": "No reply was scripted for this call", "code": 500}`. A reply `{"$raise": msg}` is a service that raised (web evidence only: the desktop client never raises, parity CL-26) |
| `alert_reply` | what every Telegram/WhatsApp send answers (`null`: `{"status": "success", "data": {"message": "Alert queued successfully"}}`) |
| `expected` | what the web did (below) |
| `desktop_expected` | where decisions.md fixes a WEB-DEFECT: the values the desktop must produce instead (below) |
| `source` | workflow goldens from a document: `file:line` |

`expected`:

| Key | Meaning |
|---|---|
| `calls` | every client call in order: `{method, endpoint, payload}`; `method` is the web client's method, `endpoint` the desktop `/api/v1` name, `payload` what the web client sent its service, without the API key |
| `outputs` | node goldens: the dict the node returned (`null` for none) |
| `outputs_by` | `node`, or `walk` when the desktop's walk builds that dict (unresolved order fields, gates); a `walk` result is checked through `logs` and `error` |
| `vars_out` | every variable after the run |
| `conditions_out` | condition results after the run |
| `logs` | the run log, `[level, message]` (the entry time is not recorded) |
| `next` | node goldens: the probe children the walk reached, sorted |
| `error` | node goldens: `{"errors": [{node, type, message}], "abort": text}` (the run's failures, and the text when the whole run stopped: the web raised), `null` when neither |
| `result`, `record` | workflow goldens: what the run answered (without `execution_id` and `logs`) and the execution record's `{status, error}` |
| `alerts` | Telegram/WhatsApp sends: `{channel, message, to}` |
| `feed` | subscribe/unsubscribe/wait operations the stream nodes made |
| `subscriptions` | the workflow's recorded subscriptions after the run, `[symbol, exchange, mode]` |
| `http_requests` | what the HTTP stub received: method, path, query, the headers the node set (plus content-type), body |
| `sleeps` | seconds each Delay / Wait Until slept (the clock does not move) |
| `server_log` | lines the web wrote to its server log from other modules (indicator service, calendar); evidence only, never compared |

## desktop_expected

A web defect that decisions.md says to fix keeps the web's output in
`expected` and carries the fixed one here, so the file shows the difference
and the replay checks the fix:

- `parity`, `why`: the item and the reason.
- `fix`: the names of the source fixes the recorder applied to the web's own
  code to compute the values (each is a one-line change of the web's function,
  listed in the recorder's `fixes.py`), so a fixed value is the web's behaviour
  with only that defect removed.
- any `expected` key: its desktop value, replacing the web's.
- `script`, `script_extra`: the replies the desktop run is scripted with
  instead of / besides `script`.

Values may use wildcards where the desktop's own words replace the web's
(the lane chooses them): `{"$any": ...}` matches anything, `{"$prefix": p}` a
text starting with `p`.

## Replay

`src-tauri/src/flow/goldens/` (one test per family, `flow::goldens::goldens_<family>`;
run with `--nocapture` to see the per-family lines) scripts core's
`FakeFlowClient` from `script` (plus `desktop_expected.script` /
`script_extra`), builds the node services of `contract::env` from `env`
(`env.rs`: the master contract rows, the strategy book, a feed that answers
from `env.feed`, an HTTP stub that answers `env.http.response` and records
each request with whether it went to a vetted address, the holiday table),
runs the graph through the registry and core's `Walk` (a workflow golden
also through the run's start and end as core's executor does them), and
compares every recorded key. It prints one line per family: total, replayed,
matched, not built (a node type no family registers yet), needs a hook, web
evidence only. A registered type's mismatch fails the test.

A workflow golden goes through the run's own gate first: the strict
validator (`ValidateOptions::COMPLETE`), whose refusal answers before any
record, context or log exists, as the executor does. Hooks the harness does
not have yet, so those goldens are reported, not run: `app` (the store:
already-running, missing workflow, no execution record).

## How values are compared

- Numbers keep their kind (an integer is not a float; Python prints `10` and
  `10.0` differently); floats within `1e-6 + 1e-9 * |want|` (the 1.0.4
  indicator tolerance).
- A `null` payload field equals an absent one. The desktop's
  `cancelorder`, `cancelallorder`, `closeposition` and `orderstatus` carry
  the client's default `strategy`, which the web's services never received
  (CL-07, CL-08, CL-09, CL-23); it is ignored where the web sent none.
- Log lines (decisions.md 19): a Python dict or list written in a line is
  compared as a value with dict keys in any order; everything else byte for
  byte, including each value's spelling (`'a'`, `True`, `None`, `100.0`).
- A list right after `keys: ` in a log line lists a dict's keys
  (`Options multi-order node_data keys: [...]`, in stored order on the web,
  key order on the desktop): the same keys in any order, each spelled the
  same. The node goldens also write node data keys sorted.

## Recording

`scripts/flow_goldens/` holds the recorder (Python, run with the web
checkout's own interpreter; nothing of it ships). It reads an export of the
web at the recorded commit, never the web checkout: it replaces the service
modules the web client imports with recording fakes, so the web's real
client, executor, validator and indicator service run; points every database
at a temporary SQLite file; refuses every socket except its local HTTP stubs
on ephemeral ports (one plain, one TLS with a throwaway certificate for the
doc's `https://` examples); freezes the clock per scenario in IST; and writes
one file per scenario. The scenarios are `scenarios_<family>.py`; the decided
web-defect fixes are `fixes.py`.

    git -C <web> archive 96e5927e5 services database utils blueprints restx_api subscribers sandbox broker/__init__.py docs/prompt .claude/skills/flow-builder | tar -x -C scripts/flow_goldens/web
    <web>/.venv/bin/python -I scripts/flow_goldens/record.py tests/fixtures/web/flow/goldens
    python3 scripts/flow_goldens/sentences.py tests/fixtures/web/flow/goldens scripts/flow_goldens/web <parity.md>

Recording twice gives byte-identical files. `sentences.py` lists every
run-time sentence of the executor that no golden shows; at this recording
four remain, none reachable on the web: the generated-strategy trigger-price
refusal (after an earlier check refuses SL/SL-M), `Custom strategy selected
but no legs provided` (the custom path never calls the generator), `No
trigger node found` (the strict validator refuses such a workflow first) and
the gthread pool's `The workflow could not be started. Try again shortly.`
(the desktop has no pools, decisions.md 2).
