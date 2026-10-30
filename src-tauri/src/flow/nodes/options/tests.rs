//! The options nodes against the web's tests (`test_flow_mcx_options.py`,
//! `test_flow_custom_legs.py` TestALegReachesTheBrokerAsBuilt,
//! `test_flow_default_product.py` TestOptionsAreAlwaysADerivative,
//! `test_flow_final_fix.py` custom-multi cases) and parity
//! N-optionsOrder-*, N-optionsMultiOrder-*, N-common-19..23.

use super::*;
use crate::flow::nodes::order_testing::{master_row, message, Run};
use serde_json::json;

fn pair(q: &str, o: &str) -> (String, String) {
    (q.to_string(), o.to_string())
}

/// X-127..X-133: a named underlying decides; the declared exchange is the
/// fallback; anything else is NSE.
#[test]
fn the_underlying_decides_where_it_is_quoted_and_traded() {
    for u in ["NIFTY", "BANKNIFTY", "FINNIFTY", "MIDCPNIFTY", "NIFTYNXT50"] {
        assert_eq!(
            resolve_option_exchanges(u, ""),
            pair("NSE_INDEX", "NFO"),
            "{u}"
        );
    }
    for u in ["SENSEX", "BANKEX", "SENSEX50"] {
        assert_eq!(
            resolve_option_exchanges(u, ""),
            pair("BSE_INDEX", "BFO"),
            "{u}"
        );
    }
    for u in MCX_OPTION_UNDERLYINGS {
        assert_eq!(resolve_option_exchanges(u, ""), pair("MCX", "MCX"), "{u}");
    }
    for u in ["crudeoil", "  Gold  "] {
        assert_eq!(resolve_option_exchanges(u, ""), pair("MCX", "MCX"), "{u}");
    }
    for u in ["", "NOTAPRODUCT"] {
        assert_eq!(
            resolve_option_exchanges(u, ""),
            pair("NSE_INDEX", "NFO"),
            "{u}"
        );
    }
    assert_eq!(
        resolve_option_exchanges("SENSEX", "NSE_INDEX"),
        pair("BSE_INDEX", "BFO")
    );
    assert_eq!(
        resolve_option_exchanges("CRUDEOIL", "NSE_INDEX"),
        pair("MCX", "MCX")
    );
    assert_eq!(
        resolve_option_exchanges("MENTHAOIL", "MCX"),
        pair("MCX", "MCX")
    );
    assert_eq!(
        resolve_option_exchanges("NIFTY", "NFO"),
        pair("NSE_INDEX", "NFO")
    );
}

/// WEB-DEFECT X-134 (fixed): a stock option declared on NFO (or BFO) is
/// quoted on the equity segment, where `/api/v1/optionsorder` finds the
/// stock; the web said NSE_INDEX, where SBIN is not listed. Fails on the
/// web's table.
#[test]
fn a_stock_option_is_quoted_on_the_equity_segment() {
    assert_eq!(resolve_option_exchanges("SBIN", "NFO"), pair("NSE", "NFO"));
    assert_eq!(
        resolve_option_exchanges("sbin", " nse "),
        pair("NSE", "NFO")
    );
    assert_eq!(
        resolve_option_exchanges("RELIANCE", "BFO"),
        pair("BSE", "BFO")
    );
    // What the desktop service makes of the pair: quote SBIN on NSE, trade
    // its options on NFO.
    let quote = crate::services::options_service::quote_exchange("SBIN", "NSE");
    assert_eq!(quote, "NSE");
    assert_eq!(
        crate::services::options_service::option_exchange(&quote),
        "NFO"
    );
    // The index segment, declared explicitly, stays the web's.
    assert_eq!(
        resolve_option_exchanges("SBIN", "NSE_INDEX"),
        pair("NSE_INDEX", "NFO")
    );
}

/// X-135: every exchange the desktop option-symbol service treats as
/// having no spot (its own list, which the tools service and the options
/// nodes read too) is declarable and quotes and trades on itself; an
/// exchange outside that list is not.
#[test]
fn every_no_spot_exchange_is_declarable() {
    use crate::services::options_service::{option_exchange, NO_SPOT};
    assert_eq!(NO_SPOT, crate::services::tools_service::NO_SPOT);
    assert!(!NO_SPOT.is_empty());
    for &ex in NO_SPOT {
        assert_eq!(
            resolve_option_exchanges("ANYTHING", ex),
            pair(ex, ex),
            "{ex}"
        );
        assert_eq!(option_exchange(ex), ex);
    }
    for ex in ["NSE", "BSE", "NFO", "CRYPTO"] {
        assert!(!NO_SPOT.contains(&ex));
        assert_ne!(
            resolve_option_exchanges("ANYTHING", ex),
            pair(ex, ex),
            "{ex}"
        );
    }
}

/// The `INDEX_SYMBOLS` entries the editor lists on MCX.
fn editor_mcx_symbols() -> Vec<String> {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/lib/flow/constants.ts");
    let source = std::fs::read_to_string(path).unwrap_or_default();
    let Some(start) = source.find("export const INDEX_SYMBOLS = [") else {
        return Vec::new();
    };
    let block = &source[start..];
    let block = &block[..block.find("] as const").unwrap_or(block.len())];
    block
        .lines()
        .filter(|l| l.contains("exchange: 'MCX'"))
        .filter_map(|l| {
            l.split("value: '")
                .nth(1)?
                .split('\'')
                .next()
                .map(str::to_string)
        })
        .collect()
}

/// X-140, X-141: the editor's MCX underlyings are the resolver's, and
/// each routes to MCX.
#[test]
fn the_dropdown_and_the_resolver_list_the_same_commodities() {
    let mut listed = editor_mcx_symbols();
    assert!(
        !listed.is_empty(),
        "INDEX_SYMBOLS not found in constants.ts"
    );
    for u in &listed {
        assert_eq!(resolve_option_exchanges(u, ""), pair("MCX", "MCX"), "{u}");
    }
    listed.sort();
    let mut ours: Vec<String> = MCX_OPTION_UNDERLYINGS
        .iter()
        .map(|s| s.to_string())
        .collect();
    ours.sort();
    assert_eq!(listed, ours);
}

fn lot(run: &Run, u: &str, ex: &str) -> Result<i64, String> {
    resolve_lot_size(&run.ctx, u, ex)
}

/// parity N-common-22: the master's lot size, failing closed; the
/// built-in table only for an exchange with no contracts at all.
#[test]
fn the_lot_size_comes_from_the_master_and_fails_closed() {
    let master = vec![
        master_row(
            "NIFTYNXT5025NOV2668000CE",
            "NIFTYNXT50 25 Nov 26 68000 CE",
            "NFO",
            25,
        ),
        master_row("NIFTY25NOV2624000CE", "NIFTY 25 Nov 26 24000 CE", "NFO", 75),
        master_row("BANKNIFTY25NOV2652000CE", "BANKNIFTY", "NFO", 0),
        master_row("BANKNIFTY25NOV2652100CE", "BANKNIFTY", "NFO", 30),
    ];
    let run = Run::at("2026-10-29T10:15:00", Some(master));
    // By the symbol when `name` holds a description; never NIFTYNXT50's.
    assert_eq!(lot(&run, "NIFTY", "NFO"), Ok(75));
    // By name, skipping a row without a lot size.
    assert_eq!(lot(&run, "BANKNIFTY", "NFO"), Ok(30));
    assert_eq!(
        lot(&run, "FINNIFTY", "NFO"),
        Err("FINNIFTY has no contract on NFO in the master contract. Check the symbol, or re-download the master contract.".into())
    );
    let run = Run::at("2026-10-29T10:15:00", Some(Vec::new()));
    assert_eq!(lot(&run, "SENSEX", "BFO"), Ok(20));
    assert_eq!(
        run.log(),
        [(
            "warning".to_string(),
            "Master contract has no BFO contracts loaded; using the built-in lot size 20 for SENSEX"
                .to_string()
        )]
    );
    assert_eq!(
        lot(&run, "TATAMOTORS", "NFO"),
        Err("The master contract has no NFO contracts loaded and no built-in lot size for TATAMOTORS. Download the master contract first.".into())
    );
    let run = Run::at("2026-10-29T10:15:00", None);
    assert_eq!(
        lot(&run, "NIFTY", "NFO"),
        Err(
            "Could not read the lot size for NIFTY on NFO. Refusing to size an order on a guess."
                .into()
        )
    );
}

/// parity N-common-23: what an expiry lookup that cannot be used logs.
#[tokio::test]
async fn an_unusable_expiry_reply_is_logged_and_resolves_nothing() {
    let cases = [
        (
            json!({"status": "error", "error": "Invalid openalgo apikey", "code": 403}),
            "Failed to fetch expiry: {'code': 403, 'error': 'Invalid openalgo apikey', 'status': 'error'}",
        ),
        (json!({"status": "success", "data": []}), "No expiry dates found for NIFTY on NFO"),
        (json!({"status": "success", "data": 5}), "Error resolving expiry: 'int' object is not iterable"),
        (
            json!({"status": "success", "data": ["soon", "later"]}),
            "No expiry matches current_week for NIFTY on NFO among 2 listed contract(s)",
        ),
    ];
    for (reply, want) in cases {
        let run = Run::new();
        run.reply("expiry", reply);
        assert_eq!(
            resolve_expiry_date(&run.ctx, "NIFTY", "NFO", "current_week").await,
            None
        );
        assert_eq!(run.log(), [("error".to_string(), want.to_string())]);
    }
}

/// X-040, decisions.md 1: the month a monthly type names is the IST one.
/// 31 October 2026, 20:00 UTC is 1 November in IST.
#[tokio::test]
async fn a_monthly_expiry_is_counted_in_ist() {
    let run = Run::at("2026-11-01T01:30:00", Some(Vec::new()));
    run.reply(
        "expiry",
        json!({"status": "success", "data": ["27-OCT-26", "24-NOV-26", "29-DEC-26"]}),
    );
    let got = resolve_expiry_date(&run.ctx, "NIFTY", "NFO", "current_month").await;
    assert_eq!(got.as_deref(), Some("24NOV26"));
}

fn expiries() -> Value {
    json!({"status": "success", "data": ["28-AUG-26", "25-SEP-26", "30-OCT-26"]})
}

/// A custom multi-leg run in August 2026 with NIFTY's lot at 75
/// (TestALegReachesTheBrokerAsBuilt).
async fn place_legs(legs: Value) -> (NodeOutcome, Run) {
    let master = vec![master_row("NIFTY28AUG2624000CE", "NIFTY", "NFO", 75)];
    let run = Run::at("2026-08-20T10:00:00", Some(master));
    run.reply("expiry", expiries()).reply(
        "optionsmultiorder",
        json!({"status": "success", "results": []}),
    );
    let data = json!({"strategy": "custom", "underlying": "NIFTY", "quantity": 1,
        "expiryType": "current_week", "priceType": "MARKET", "product": "NRML", "action": "SELL",
        "legs": legs});
    let o = run.node("optionsMultiOrder", data).await;
    (o, run)
}

fn sent(run: &Run) -> Value {
    run.client
        .calls_to("optionsmultiorder")
        .first()
        .map(|c| Value::Object(c.payload.clone()))
        .unwrap_or(Value::Null)
}

/// X-026..X-030: what the editor builds is what is placed: strike, expiry,
/// side and size, exactly one strike selector per leg.
#[tokio::test]
async fn a_leg_reaches_the_service_as_built() {
    let (o, run) = place_legs(json!([
        {"strikeMode": "STRIKE", "strike": 24500, "expiry": "28OCT25", "optionType": "CE", "action": "SELL", "quantity": 1},
        {"strikeMode": "STRIKE", "strike": 24500, "expiry": "25NOV25", "optionType": "CE", "action": "BUY", "quantity": 2}
    ]))
    .await;
    assert_eq!(
        o.value().map(|v| v["status"].clone()),
        Some(json!("success"))
    );
    let legs = sent(&run)["legs"].clone();
    assert_eq!(
        (
            legs[0]["expiry_date"].clone(),
            legs[1]["expiry_date"].clone()
        ),
        (json!("28OCT25"), json!("25NOV25"))
    );
    assert_eq!(
        (legs[0]["strike"].clone(), legs[1]["strike"].clone()),
        (json!(24500.0), json!(24500.0))
    );
    assert_eq!(
        (legs[0]["action"].clone(), legs[1]["action"].clone()),
        (json!("SELL"), json!("BUY"))
    );
    assert_eq!(legs[1]["quantity"], 150);
    let (_, run) = place_legs(json!([{"offset": "OTM2", "expiryType": "next_month", "optionType": "PE", "action": "BUY", "quantity": 1}])).await;
    assert_eq!(sent(&run)["legs"][0]["expiry_date"], "25SEP26");
    let (_, run) =
        place_legs(json!([{"offset": "ATM", "optionType": "PE", "action": "SELL", "quantity": 1}]))
            .await;
    assert!(sent(&run)["legs"][0].get("expiry_date").is_none());
    assert_eq!(sent(&run)["expiry_date"], "28AUG26");
    let (_, run) = place_legs(json!([
        {"strikeMode": "STRIKE", "strike": 24500, "optionType": "CE", "action": "BUY", "quantity": 1},
        {"strikeMode": "OFFSET", "offset": "OTM2", "optionType": "PE", "action": "BUY", "quantity": 1}
    ]))
    .await;
    let legs = sent(&run)["legs"].clone();
    assert!(legs[0].get("offset").is_none() && legs[1].get("strike").is_none());
    let (_, run) =
        place_legs(json!([{"offset": "ATM", "optionType": "CE", "action": "BUY", "quantity": 3}]))
            .await;
    assert_eq!(sent(&run)["legs"][0]["quantity"], 225);
}

/// X-031: a leg that cannot be used stops the basket before anything is
/// sent.
#[tokio::test]
async fn a_bad_leg_stops_the_basket_before_anything_fills() {
    let (o, run) = place_legs(json!([
        {"offset": "ATM", "optionType": "CE", "action": "BUY", "quantity": 1},
        {"offset": "ATM", "expiry": "not-a-date", "optionType": "PE", "action": "BUY", "quantity": 1}
    ]))
    .await;
    assert!(o.is_error());
    assert!(run.calls().is_empty());
    assert_eq!(
        message(&o),
        "Options multi-order has invalid runtime order data: leg 2: expiry must be in DDMMMYY format such as 28OCT25, got 'NOT-A-DATE'"
    );
}

/// The final_fix runs: lot size 10, one expiry, 27 August 2026.
fn final_fix_run(runtime: Value) -> Run {
    let master = vec![master_row("NIFTY27AUG2624000CE", "NIFTY", "NFO", 10)];
    let run = Run::at("2026-08-20T10:00:00", Some(master));
    run.reply(
        "expiry",
        json!({"status": "success", "data": ["27-AUG-26"]}),
    )
    .reply(
        "optionsmultiorder",
        json!({"status": "success", "results": []}),
    );
    run.ctx.vars.set("runtime", runtime);
    run
}

fn custom_multi(legs: Value) -> Value {
    json!({"underlying": "NIFTY", "expiryType": "current_week", "strategy": "custom", "quantity": 1,
        "action": "SELL", "product": " mis ", "priceType": " sl ", "price": 110, "triggerPrice": 105,
        "legs": legs})
}

/// X-086: legs supplied whole from a variable keep their structure; each
/// leg's own value wins, the node's fills the rest; the wire shape.
#[tokio::test]
async fn raw_legs_resolve_with_common_fields_and_overrides() {
    let run = final_fix_run(json!([
        {"offset": " atm ", "optionType": " ce ", "action": " buy ", "quantity": 2,
         "pricetype": " sl ", "expiryDate": "{{missing.legacy_expiry}}"},
        {"offset": "otm2", "optionType": "pe", "action": "sell", "quantity": 3,
         "product": " nrml ", "priceType": " limit ", "price": 120, "triggerPrice": 0}
    ]));
    let o = run
        .node("optionsMultiOrder", custom_multi(json!("{{runtime}}")))
        .await;
    assert!(!o.is_error(), "{:?}", o);
    let methods: Vec<&str> = run.calls().iter().map(|c| c.method).collect();
    assert_eq!(methods, ["get_expiry", "options_multi_order"]);
    let p = sent(&run);
    assert_eq!(p["expiry_date"], "27AUG26");
    assert_eq!(
        p["legs"],
        json!([
            {"offset": "ATM", "option_type": "CE", "action": "BUY", "quantity": 20, "pricetype": "SL",
             "product": "MIS", "price": 110.0, "trigger_price": 105.0, "splitsize": 0},
            {"offset": "OTM2", "option_type": "PE", "action": "SELL", "quantity": 30, "pricetype": "LIMIT",
             "product": "NRML", "price": 120.0, "trigger_price": 0.0, "splitsize": 0}
        ])
    );
}

/// X-087: a whole-field list is not stringified by the walk's unresolved
/// check (nested references resolve leg by leg).
#[tokio::test]
async fn a_raw_list_passes_the_unresolved_order_field_check() {
    for field in ["legs", "orders"] {
        let run = Run::new();
        run.ctx.vars.set(
            "runtime",
            json!([{"quantity": "{{qty}}", "expiryDate": "{{missing.legacy_expiry}}"}]),
        );
        run.ctx.vars.set("qty", json!(2));
        let node = if field == "legs" {
            "optionsMultiOrder"
        } else {
            "basketOrder"
        };
        let errors = run
            .walk(node, json!({field: "{{runtime}}", "strategy": "custom"}))
            .await;
        let text = errors
            .first()
            .map(|e| e["message"].to_string())
            .unwrap_or_default();
        assert!(!text.contains("has unresolved variable"), "{field}: {text}");
    }
}

/// X-088: the legacy `orderLegs` from a variable still runs.
#[tokio::test]
async fn legacy_order_legs_resolve() {
    let run = final_fix_run(
        json!([{"offset": "ATM", "optionType": "CE", "action": "BUY",
        "quantity": 1, "product": "MIS", "pricetype": "MARKET"}]),
    );
    let mut data = custom_multi(Value::Null);
    if let Some(m) = data.as_object_mut() {
        m.remove("legs");
        m.insert("orderLegs".into(), json!("{{runtime}}"));
        m.insert("priceType".into(), json!("MARKET"));
    }
    let o = run.node("optionsMultiOrder", data).await;
    assert!(!o.is_error(), "{:?}", o);
    assert_eq!(sent(&run)["legs"][0]["pricetype"], "MARKET");
}

/// X-089, X-090: an unusable leg container or leg field fails before any
/// call, naming the field.
#[tokio::test]
async fn an_unusable_leg_fails_before_any_call() {
    for runtime in [
        json!([]),
        json!({}),
        json!(["not-an-object"]),
        json!("{{still.missing}}"),
    ] {
        let run = final_fix_run(runtime.clone());
        let o = run
            .node("optionsMultiOrder", custom_multi(json!("{{runtime}}")))
            .await;
        assert!(o.is_error(), "{runtime}");
        assert!(run.calls().is_empty(), "{runtime}");
    }
    let cases = [
        (json!({"offset": "BAD"}), Value::Null, "offset"),
        (json!({"optionType": "XX"}), Value::Null, "optiontype"),
        (json!({"action": "HOLD"}), Value::Null, "action"),
        (
            json!({"quantity": "{{runtime}}"}),
            json!("not-a-number"),
            "quantity",
        ),
        (json!({"product": "BAD"}), Value::Null, "product"),
        (json!({"pricetype": "BAD"}), Value::Null, "pricetype"),
        (json!({"price": "{{runtime}}"}), json!("nan"), "price"),
        (json!({"triggerPrice": "{{runtime}}"}), json!(0), "trigger"),
    ];
    for (update, runtime, field) in cases {
        let mut leg = json!({"offset": "ATM", "optionType": "CE", "action": "BUY", "quantity": 1,
            "product": "MIS", "pricetype": "SL", "price": 110, "triggerPrice": 105});
        if let (Some(m), Value::Object(u)) = (leg.as_object_mut(), update) {
            m.extend(u);
        }
        let run = final_fix_run(runtime);
        let o = run
            .node("optionsMultiOrder", custom_multi(json!([leg])))
            .await;
        assert!(
            message(&o).to_lowercase().contains(field),
            "{field}: {}",
            message(&o)
        );
        assert!(run.calls().is_empty(), "{field}");
    }
}

/// X-076, X-077: an option is a derivative: NRML when the node names no
/// product, whatever its underlying's exchange says.
#[tokio::test]
async fn options_default_to_nrml() {
    let run = Run::new();
    run.reply(
        "expiry",
        json!({"status": "success", "data": ["04-NOV-26"]}),
    )
    .reply("optionsorder", json!({"status": "success"}));
    let data =
        json!({"underlying": "NIFTY", "exchange": "NSE_INDEX", "expiryType": "current_week"});
    run.node("optionsOrder", data).await;
    assert_eq!(
        run.client.calls_to("optionsorder")[0].payload["product"],
        "NRML"
    );
    let run = Run::new();
    run.reply(
        "expiry",
        json!({"status": "success", "data": ["04-NOV-26"]}),
    )
    .reply("optionsmultiorder", json!({"status": "success"}));
    let data = json!({"underlying": "NIFTY", "strategy": "straddle", "expiryType": "current_week"});
    run.node("optionsMultiOrder", data).await;
    let legs = sent(&run)["legs"].clone();
    assert_eq!(
        (legs[0]["product"].clone(), legs[1]["product"].clone()),
        (json!("NRML"), json!("NRML"))
    );
    assert_eq!(
        legs[0]["action"], "SELL",
        "a multi-leg node's default action is SELL"
    );
}

/// X-136, N-optionsOrder-07: both options nodes trade a commodity on MCX,
/// size it from the MCX contract, and read `current_week` on a
/// monthly-only product as the nearest expiry.
#[tokio::test]
async fn both_options_nodes_trade_commodities_on_mcx() {
    let monthly = json!({"status": "success", "data": ["17-NOV-26", "16-DEC-26"]});
    let run = Run::new();
    run.reply("expiry", monthly.clone())
        .reply("optionsorder", json!({"status": "success"}));
    let data =
        json!({"underlying": "CRUDEOIL", "exchange": "NSE_INDEX", "expiryType": "current_week"});
    run.node("optionsOrder", data).await;
    let calls = run.calls();
    assert_eq!(
        calls[0].payload["exchange"], "MCX",
        "the expiry is read on MCX"
    );
    let p = &calls[1].payload;
    assert_eq!(
        (&p["exchange"], &p["quantity"], &p["expiry_date"]),
        (&json!("MCX"), &json!(100), &json!("17NOV26"))
    );
    let run = Run::new();
    run.reply("expiry", monthly)
        .reply("optionsmultiorder", json!({"status": "success"}));
    let data =
        json!({"underlying": "CRUDEOIL", "strategy": "straddle", "expiryType": "current_week"});
    run.node("optionsMultiOrder", data).await;
    assert_eq!(sent(&run)["exchange"], "MCX");
    assert_eq!(sent(&run)["legs"][0]["quantity"], 100);
}

/// parity N-optionsMultiOrder-01..11: the run log of a generated strategy,
/// line for line.
#[tokio::test]
async fn a_generated_strategy_logs_as_the_web() {
    let run = Run::new();
    run.reply(
        "expiry",
        json!({"status": "success", "data": ["04-NOV-26", "11-NOV-26"]}),
    )
    .reply(
        "optionsmultiorder",
        json!({"status": "success", "results": []}),
    );
    let data = json!({"underlying": "NIFTY", "strategy": "straddle", "action": "SELL",
        "quantity": 2, "expiryType": "current_week", "outputVariable": "multi"});
    run.node("optionsMultiOrder", data).await;
    assert_eq!(
        run.messages(),
        [
            "Options multi-order node_data keys: ['action', 'expiryType', 'outputVariable', 'quantity', 'strategy', 'underlying']",
            "Strategy: straddle, Action: SELL, Quantity: 2 lots, Product: NRML",
            "Lot size for NIFTY: 75 -> 2 lot(s) = 150",
            "Resolved expiry: current_week -> 04NOV26",
            "Placing options multi-order: NIFTY straddle with 2 legs",
            "  Leg 1: ATM CE SELL qty=150",
            "  Leg 2: ATM PE SELL qty=150",
            "Options multi-order result: {'results': [], 'status': 'success'}",
            "Stored result in variable: multi",
        ]
    );
}

/// N-common-19: a declared BSE_INDEX routes to BFO; each exchange without a
/// spot named on its own quotes and trades on itself.
#[test]
fn every_declared_exchange_routes_as_the_web() {
    for (declared, want) in [
        ("BSE_INDEX", ("BSE_INDEX", "BFO")),
        ("bse_index ", ("BSE_INDEX", "BFO")),
        ("NSE_INDEX", ("NSE_INDEX", "NFO")),
        ("MCX", ("MCX", "MCX")),
        ("CDS", ("CDS", "CDS")),
        ("BCD", ("BCD", "BCD")),
        ("NCDEX", ("NCDEX", "NCDEX")),
        ("NCO", ("NCO", "NCO")),
        ("LSE", ("NSE_INDEX", "NFO")),
    ] {
        assert_eq!(
            resolve_option_exchanges("ANYTHING", declared),
            pair(want.0, want.1),
            "{declared}"
        );
    }
}

/// N-common-22: every entry of the built-in table, used only when the
/// exchange has no contracts loaded.
#[test]
fn the_built_in_lot_sizes_are_the_webs() {
    for (underlying, exchange, lot) in [
        ("NIFTY", "NFO", 65),
        ("BANKNIFTY", "NFO", 30),
        ("FINNIFTY", "NFO", 60),
        ("MIDCPNIFTY", "NFO", 120),
        ("NIFTYNXT50", "NFO", 25),
        ("SENSEX", "BFO", 20),
        ("BANKEX", "BFO", 30),
        ("SENSEX50", "BFO", 25),
    ] {
        let run = Run::at("2026-10-29T10:15:00", Some(Vec::new()));
        assert_eq!(
            resolve_lot_size(&run.ctx, underlying, exchange),
            Ok(lot),
            "{underlying}"
        );
    }
}

/// N-optionsOrder-01: a node with no fields sends the executor's defaults:
/// NIFTY, current_week, ATM, CE, BUY, MARKET, NRML, one lot, no split, no
/// prices.
#[tokio::test]
async fn a_bare_options_order_sends_the_executor_defaults() {
    let run = Run::new();
    run.reply(
        "expiry",
        json!({"status": "success", "data": ["04-NOV-26", "11-NOV-26"]}),
    )
    .reply("optionsorder", json!({"status": "success"}));
    let o = run.node("optionsOrder", json!({})).await;
    assert!(!o.is_error(), "{:?}", o);
    let calls = run.calls();
    assert_eq!(
        Value::Object(calls[0].payload.clone()),
        json!({"symbol": "NIFTY", "exchange": "NFO", "instrumenttype": "options"})
    );
    assert_eq!(
        Value::Object(calls[1].payload.clone()),
        json!({"strategy": crate::flow::nodes::order_testing::WORKFLOW, "underlying": "NIFTY",
               "exchange": "NSE_INDEX", "action": "BUY", "quantity": 75, "expiry_date": "04NOV26",
               "offset": "ATM", "option_type": "CE", "pricetype": "MARKET", "product": "NRML",
               "price": 0.0, "trigger_price": 0.0, "splitsize": 0})
    );
}

/// N-optionsMultiOrder-02: a generated strategy with no underlying or
/// expiry type trades NIFTY's current_week; one with no strategy at all is
/// custom and needs legs.
#[tokio::test]
async fn a_bare_multi_order_trades_nifty_this_week() {
    let run = Run::new();
    run.reply(
        "expiry",
        json!({"status": "success", "data": ["04-NOV-26", "11-NOV-26"]}),
    )
    .reply("optionsmultiorder", json!({"status": "success"}));
    run.node("optionsMultiOrder", json!({"strategy": "straddle"}))
        .await;
    let calls = run.calls();
    assert_eq!(calls[0].payload["symbol"], "NIFTY");
    let p = sent(&run);
    assert_eq!(
        (&p["underlying"], &p["exchange"], &p["expiry_date"]),
        (&json!("NIFTY"), &json!("NSE_INDEX"), &json!("04NOV26"))
    );
    assert_eq!(
        (&p["legs"][0]["action"], &p["legs"][0]["quantity"]),
        (&json!("SELL"), &json!(75))
    );
    let run = Run::new();
    let o = run.node("optionsMultiOrder", json!({})).await;
    assert_eq!(
        message(&o),
        "Options multi-order has invalid runtime order data: legs is required"
    );
}

/// N-optionsMultiOrder-05, N-common-15: a leg's required fields, its
/// expiry type read in any case, and an optional expiry that is null,
/// blank or padded.
#[tokio::test]
async fn a_legs_required_and_optional_fields_read_as_the_web() {
    let leg = |extra: Value| {
        let mut l = json!({"optionType": "CE", "action": "BUY", "quantity": 1});
        if let (Some(m), Value::Object(e)) = (l.as_object_mut(), extra) {
            for (k, v) in e {
                if v == json!("$drop") {
                    m.remove(&k);
                } else {
                    m.insert(k, v);
                }
            }
        }
        l
    };
    for (l, want) in [
        (leg(json!({})), "leg 1: offset is required"),
        (
            leg(json!({"offset": "ATM", "action": "$drop"})),
            "leg 1: action is required",
        ),
        (
            leg(json!({"offset": "ATM", "optionType": "$drop"})),
            "leg 1: optionType is required",
        ),
        (
            leg(json!({"offset": "ATM", "quantity": "$drop"})),
            "leg 1: quantity is required",
        ),
        (
            leg(json!({"strikeMode": "STRIKE"})),
            "leg 1: strike is required",
        ),
    ] {
        let (o, run) = place_legs(json!([l.clone()])).await;
        assert_eq!(
            message(&o),
            format!("Options multi-order has invalid runtime order data: {want}"),
            "{l}"
        );
        assert!(run.calls().is_empty());
    }
    let (o, run) = place_legs(json!([
        leg(json!({"offset": "ATM", "expiryType": " Next_Month "})),
        leg(json!({"offset": "ATM", "expiry": null})),
        leg(json!({"offset": "ATM", "expiry": "   "})),
        leg(json!({"offset": "ATM", "expiry": "  25sep26 "}))
    ]))
    .await;
    assert!(!o.is_error(), "{:?}", o);
    let legs = sent(&run)["legs"].clone();
    assert_eq!(
        legs[0]["expiry_date"], "25SEP26",
        "expiry type read lower-cased"
    );
    assert!(
        legs[1].get("expiry_date").is_none(),
        "a null expiry inherits"
    );
    assert!(
        legs[2].get("expiry_date").is_none(),
        "a blank expiry inherits"
    );
    assert_eq!(legs[3]["expiry_date"], "25SEP26", "an expiry is trimmed");
}

/// N-optionsMultiOrder-08: the generated-legs defences, unreachable through
/// the node (its field checks refuse first) and kept as the web keeps them:
/// SL/SL-M have no trigger to carry, and a LIMIT without a price is
/// refused. The run logs either with `Options multi-order failed: `, the
/// branch the unresolvable leg-expiry golden pins.
#[tokio::test]
async fn the_generated_legs_defences_say_the_web_words() {
    let run = Run::new();
    let node = |price_type: &str, price: f64| MultiNode {
        underlying: "NIFTY".into(),
        expiry: ("current_week".into(), String::new()),
        strategy_type: "straddle".into(),
        action: "SELL".into(),
        quantity_lots: 1,
        product: "NRML".into(),
        strangle_width: "OTM2".into(),
        price_type: price_type.into(),
        price,
        trigger_price: 0.0,
    };
    let sized = Sized {
        quote_exchange: "NSE_INDEX".into(),
        fo_exchange: "NFO".into(),
        lot_size: 75,
        expiry_date: "04NOV26".into(),
    };
    for pt in ["SL", "SL-M"] {
        let got = OptionsMultiOrderNode::legs(&run.ctx, &node(pt, 100.0), None, &sized).await;
        assert_eq!(
            got,
            Err(format!(
                "{pt} is not available for a generated straddle strategy: its legs carry no trigger price. Use MARKET or LIMIT, or build the legs individually as custom legs where each can set its own trigger price."
            ))
        );
    }
    let got = OptionsMultiOrderNode::legs(&run.ctx, &node("LIMIT", 0.0), None, &sized).await;
    assert_eq!(
        got,
        Err("LIMIT order needs a positive price, got 0.0".into())
    );
    let got = OptionsMultiOrderNode::legs(&run.ctx, &node("LIMIT", 120.5), None, &sized).await;
    assert_eq!(got.map(|l| l.len()), Ok(2));
    assert!(run.calls().is_empty());
}
