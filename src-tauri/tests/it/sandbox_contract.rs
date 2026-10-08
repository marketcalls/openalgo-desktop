//! Serializer contract: every analyze-mode web fixture under
//! `tests/fixtures/web/rest/**` that the sandbox engine answers is compared
//! with the engine's reply for an equivalent scenario: the same keys at every
//! level, the same JSON types (float vs integer vs string vs null), the same
//! status strings and, for business errors, the same message and HTTP code.
//! Request-schema errors (marshmallow messages) belong to the API layer and
//! are not covered here.

use crate::sandbox_support;

use openalgo_desktop_lib::sandbox::{
    GttRequest, ModifyRequest, SandboxError, SmartOrderRequest, Tick,
};
use sandbox_support::*;
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;

fn fixture(rel: &str) -> (u16, Value) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/web/rest")
        .join(rel);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let v: Value = serde_json::from_str(&text).unwrap();
    let status = v["response"]["status_code"].as_u64().unwrap() as u16;
    let body = v["response"]["body"].clone();
    assert_eq!(
        body["mode"], "analyze",
        "{rel} is not an analyze-mode fixture"
    );
    (status, body)
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "integer",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Keys, types and nesting must match. Arrays: every element of ours is
/// compared with the fixture's first element.
fn assert_shape(path: &str, fx: &Value, ours: &Value) {
    match (fx, ours) {
        (Value::Object(a), Value::Object(b)) => {
            let ka: std::collections::BTreeSet<_> = a.keys().collect();
            let kb: std::collections::BTreeSet<_> = b.keys().collect();
            assert_eq!(ka, kb, "{path}: keys differ");
            for (k, va) in a {
                assert_shape(&format!("{path}.{k}"), va, &b[k]);
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            if let Some(first) = a.first() {
                assert!(
                    !b.is_empty(),
                    "{path}: the scenario must produce rows to compare"
                );
                for (i, item) in b.iter().enumerate() {
                    assert_shape(&format!("{path}[{i}]"), first, item);
                }
            }
        }
        // A nullable column may hold a value in our scenario.
        (Value::Null, Value::String(_)) => {}
        _ => assert_eq!(
            kind(fx),
            kind(ours),
            "{path}: type differs ({fx} vs {ours})"
        ),
    }
}

fn to_value<T: Serialize>(t: &T) -> Value {
    serde_json::to_value(t).unwrap()
}

fn check_ok<T: Serialize>(rel: &str, ours: &T) {
    let (status, fx) = fixture(rel);
    assert_eq!(status, 200, "{rel}");
    let v = to_value(ours);
    assert_shape(rel, &fx, &v);
    assert_eq!(v["status"], fx["status"], "{rel}: status string");
    assert_eq!(v.get("mode"), fx.get("mode"), "{rel}: mode");
}

fn check_err(rel: &str, e: &SandboxError, exact_message: bool) {
    let (status, fx) = fixture(rel);
    assert_eq!(e.http_status, status, "{rel}: HTTP status ({})", e.message);
    let v = to_value(&e.body());
    assert_shape(rel, &fx, &v);
    if exact_message {
        assert_eq!(v["message"], fx["message"], "{rel}: message");
    }
}

/// A populated session mirroring the fixture recording.
async fn populated() -> Env {
    let env = Env::at("2026-10-05 09:42:28");
    env.ltp("RELIANCE", "NSE", "1167.7");
    env.ltp("SBIN", "NSE", "954.1");
    env.ltp("NIFTY06OCT2622400CE", "NFO", "156.95");
    env.ltp("NIFTY27OCT26FUT", "NFO", "22450");
    env.ltp("CRUDEOIL19OCT26FUT", "MCX", "9008");
    env
}

#[tokio::test]
async fn placeorder_fixtures() {
    let env = populated().await;
    for (rel, r) in [
        (
            "placeorder/market_buy_mis_reliance.json",
            req("RELIANCE", "NSE", "BUY", 5, "MARKET", "MIS"),
        ),
        (
            "placeorder/market_buy_cnc_sbin.json",
            req("SBIN", "NSE", "BUY", 2, "MARKET", "CNC"),
        ),
        (
            "placeorder/market_sell_mis_sbin.json",
            req("SBIN", "NSE", "SELL", 3, "MARKET", "MIS"),
        ),
        (
            "placeorder/lowercase_action_buy.json",
            req("SBIN", "NSE", "buy", 1, "MARKET", "MIS"),
        ),
        (
            "placeorder/market_buy_mis_nifty_option.json",
            req("NIFTY06OCT2622400CE", "NFO", "BUY", 65, "MARKET", "MIS"),
        ),
        (
            "placeorder/market_buy_nrml_nifty_option.json",
            req("NIFTY06OCT2622400CE", "NFO", "BUY", 65, "MARKET", "NRML"),
        ),
        (
            "placeorder/market_buy_nrml_nifty_future.json",
            req("NIFTY27OCT26FUT", "NFO", "BUY", 65, "MARKET", "NRML"),
        ),
        (
            "placeorder/market_buy_nrml_crudeoil_future_mcx.json",
            req("CRUDEOIL19OCT26FUT", "MCX", "BUY", 100, "MARKET", "NRML"),
        ),
        (
            "placeorder/limit_buy_cnc_sbin_far_below.json",
            limit(req("SBIN", "NSE", "BUY", 2, "LIMIT", "CNC"), "763.3"),
        ),
        (
            "placeorder/limit_buy_mis_reliance_far_below.json",
            limit(req("RELIANCE", "NSE", "BUY", 1, "LIMIT", "MIS"), "934.2"),
        ),
        (
            "placeorder/limit_sell_mis_reliance_far_above.json",
            limit(req("RELIANCE", "NSE", "SELL", 1, "LIMIT", "MIS"), "1401.2"),
        ),
        (
            "placeorder/sl_buy_mis_reliance.json",
            limit(
                trigger(req("RELIANCE", "NSE", "BUY", 1, "SL", "MIS"), "1226.1"),
                "1237.8",
            ),
        ),
        (
            "placeorder/slm_sell_mis_sbin.json",
            trigger(req("SBIN", "NSE", "SELL", 1, "SL-M", "MIS"), "906.4"),
        ),
    ] {
        let placed = env.sb.place_order(r).await.unwrap();
        assert_eq!(placed.orderid.len(), 14, "{rel}: 14-digit order id");
        check_ok(rel, &placed);
    }
    for (rel, r) in [
        (
            "placeorder/error_limit_price_zero.json",
            req("SBIN", "NSE", "BUY", 1, "LIMIT", "MIS"),
        ),
        (
            "placeorder/error_option_qty_not_lot_multiple.json",
            req("NIFTY06OCT2622400CE", "NFO", "BUY", 10, "MARKET", "NRML"),
        ),
        (
            "placeorder/error_sl_without_trigger.json",
            limit(req("SBIN", "NSE", "BUY", 1, "SL", "MIS"), "900"),
        ),
        (
            "placeorder/error_unknown_symbol.json",
            req("NOTASYMBOL", "NSE", "BUY", 1, "MARKET", "MIS"),
        ),
    ] {
        let e = env.sb.place_order(r).await.unwrap_err();
        check_err(rel, &e, true);
    }
    env.shutdown().await;
}

#[tokio::test]
async fn orderstatus_modify_cancel_fixtures() {
    let env = populated().await;
    let market = env
        .sb
        .place_order(req("RELIANCE", "NSE", "BUY", 5, "MARKET", "MIS"))
        .await
        .unwrap()
        .orderid;
    let lim = env
        .sb
        .place_order(limit(req("SBIN", "NSE", "BUY", 2, "LIMIT", "CNC"), "763.3"))
        .await
        .unwrap()
        .orderid;
    let lim2 = env
        .sb
        .place_order(limit(
            req("RELIANCE", "NSE", "BUY", 1, "LIMIT", "MIS"),
            "934.2",
        ))
        .await
        .unwrap()
        .orderid;
    let lim_sell = env
        .sb
        .place_order(limit(
            req("RELIANCE", "NSE", "SELL", 1, "LIMIT", "MIS"),
            "1401.2",
        ))
        .await
        .unwrap()
        .orderid;
    let sl = env
        .sb
        .place_order(limit(
            trigger(req("RELIANCE", "NSE", "BUY", 1, "SL", "MIS"), "1226.1"),
            "1237.8",
        ))
        .await
        .unwrap()
        .orderid;
    let slm = env
        .sb
        .place_order(trigger(
            req("SBIN", "NSE", "SELL", 1, "SL-M", "MIS"),
            "906.4",
        ))
        .await
        .unwrap()
        .orderid;
    let opt = env
        .sb
        .place_order(req(
            "NIFTY06OCT2622400CE",
            "NFO",
            "BUY",
            65,
            "MARKET",
            "NRML",
        ))
        .await
        .unwrap()
        .orderid;

    for (rel, id, st) in [
        (
            "orderstatus/market_buy_mis_reliance.json",
            &market,
            "complete",
        ),
        (
            "orderstatus/limit_buy_cnc_sbin_far_below.json",
            &lim,
            "open",
        ),
        (
            "orderstatus/sl_buy_mis_reliance.json",
            &sl,
            "trigger pending",
        ),
        (
            "orderstatus/slm_sell_mis_sbin.json",
            &slm,
            "trigger pending",
        ),
        (
            "orderstatus/market_buy_nrml_nifty_option.json",
            &opt,
            "complete",
        ),
    ] {
        let s = env.sb.order_status(id).await.unwrap();
        check_ok(rel, &s);
        let (_, fx) = fixture(rel);
        assert_eq!(s.data.order_status, st);
        assert_eq!(fx["data"]["order_status"], st, "fixture agrees on {rel}");
        assert_eq!(
            s.data.price_type,
            fx["data"]["price_type"].as_str().unwrap()
        );
    }
    for rel in [
        "orderstatus/unknown_orderid.json",
        "orderstatus/unknown_orderid_session2.json",
    ] {
        let (_, fx) = fixture(rel);
        let id = fx["message"]
            .as_str()
            .unwrap()
            .split(' ')
            .nth(1)
            .unwrap()
            .to_string();
        let e = env.sb.order_status(&id).await.unwrap_err();
        check_err(rel, &e, true);
    }

    // modify
    let m = env
        .sb
        .modify_order(
            &lim2,
            ModifyRequest {
                quantity: Some(2),
                price: Some(d("957.5")),
                trigger_price: None,
            },
        )
        .await
        .unwrap();
    check_ok("modifyorder/open_limit_price_and_qty.json", &m);
    assert_eq!(m.message, "Order modified successfully");
    let after = env.sb.order_status(&lim2).await.unwrap();
    check_ok("orderstatus/after_modify.json", &after);
    assert_eq!((after.data.quantity, after.data.price), (2, 957.5));
    let m = env
        .sb
        .modify_order(
            &sl,
            ModifyRequest {
                quantity: Some(1),
                price: Some(d("1261.1")),
                trigger_price: Some(d("1249.4")),
            },
        )
        .await
        .unwrap();
    check_ok("modifyorder/sl_order_trigger.json", &m);
    let e = env
        .sb
        .modify_order(
            &market,
            ModifyRequest {
                quantity: Some(5),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    check_err("modifyorder/error_completed_order.json", &e, true);
    let e = env
        .sb
        .modify_order("99999999999999", ModifyRequest::default())
        .await
        .unwrap_err();
    check_err("modifyorder/error_unknown_orderid.json", &e, true);

    // cancel
    let c = env.sb.cancel_order(&lim_sell).await.unwrap();
    check_ok("cancelorder/open_limit.json", &c);
    assert_eq!(c.message, "Order cancelled successfully");
    let after = env.sb.order_status(&lim_sell).await.unwrap();
    check_ok("orderstatus/after_cancel.json", &after);
    check_ok(
        "cancelorder/trigger_pending_sl.json",
        &env.sb.cancel_order(&sl).await.unwrap(),
    );
    check_ok(
        "cancelorder/trigger_pending_slm.json",
        &env.sb.cancel_order(&slm).await.unwrap(),
    );
    let e = env.sb.cancel_order(&lim_sell).await.unwrap_err();
    check_err("cancelorder/error_already_cancelled.json", &e, true);
    let e = env
        .sb
        .modify_order(
            &lim_sell,
            ModifyRequest {
                quantity: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    check_err("modifyorder/error_cancelled_order.json", &e, true);
    let e = env.sb.cancel_order(&market).await.unwrap_err();
    check_err("cancelorder/error_completed_order.json", &e, true);
    let e = env.sb.cancel_order("99999999999999").await.unwrap_err();
    check_err("cancelorder/error_unknown_orderid.json", &e, true);
    env.shutdown().await;
}

#[tokio::test]
async fn books_and_funds_fixtures() {
    let env = populated().await;
    // Empty books.
    check_ok("orderbook/default.json", &env.sb.orderbook().await.unwrap());
    check_ok("tradebook/default.json", &env.sb.tradebook().await.unwrap());
    check_ok(
        "positionbook/default.json",
        &env.sb.positionbook().await.unwrap(),
    );
    check_ok("holdings/default.json", &env.sb.holdings().await.unwrap());
    check_ok(
        "gttorderbook/default.json",
        &env.sb.gtt_orderbook(Some("active")).await.unwrap(),
    );
    check_ok(
        "pnl/symbols_after_cleanup.json",
        &env.sb.pnl_symbols().await.unwrap(),
    );
    let f = env.sb.funds().await.unwrap();
    for rel in [
        "funds/apikey_in_body.json",
        "funds/apikey_in_header_and_body.json",
    ] {
        check_ok(rel, &f);
        let (_, fx) = fixture(rel);
        assert_eq!(
            to_value(&f.data)["availablecash"],
            fx["data"]["availablecash"],
            "1 crore start"
        );
    }
    check_ok(
        "closeposition/no_open_positions.json",
        &env.sb.close_all_positions().await.unwrap(),
    );
    let none = env.sb.cancel_all_orders().await.unwrap();
    check_ok("cancelallorder/nothing_open.json", &none);
    assert_eq!(
        to_value(&none),
        fixture("cancelallorder/nothing_open.json").1
    );

    // Populate.
    for r in [
        req("RELIANCE", "NSE", "BUY", 5, "MARKET", "MIS"),
        req("SBIN", "NSE", "BUY", 2, "MARKET", "CNC"),
        req("SBIN", "NSE", "BUY", 8, "MARKET", "MIS"),
        req("NIFTY06OCT2622400CE", "NFO", "BUY", 65, "MARKET", "MIS"),
    ] {
        env.sb.place_order(r).await.unwrap();
    }
    let resting = env
        .sb
        .place_order(limit(req("SBIN", "NSE", "BUY", 1, "LIMIT", "CNC"), "763.3"))
        .await
        .unwrap();
    let rejected = env
        .sb
        .place_order(req("RELIANCE", "NSE", "SELL", 10, "MARKET", "CNC"))
        .await
        .unwrap_err();
    assert_eq!(rejected.http_status, 400);
    assert!(rejected.orderid.is_some());

    let ob = env.sb.orderbook().await.unwrap();
    check_ok("orderbook/populated.json", &ob);
    check_ok(
        "tradebook/populated.json",
        &env.sb.tradebook().await.unwrap(),
    );
    check_ok(
        "positionbook/populated.json",
        &env.sb.positionbook().await.unwrap(),
    );
    check_ok(
        "pnl/symbols_populated.json",
        &env.sb.pnl_symbols().await.unwrap(),
    );
    for rel in [
        "funds/after_activity.json",
        "funds/after_cleanup.json",
        "funds/final.json",
    ] {
        check_ok(rel, &env.sb.funds().await.unwrap());
    }
    for (rel, sym, exch, product, qty) in [
        (
            "openposition/reliance_mis_long.json",
            "RELIANCE",
            "NSE",
            "MIS",
            5,
        ),
        (
            "openposition/no_position_symbol.json",
            "INFY",
            "NSE",
            "CNC",
            0,
        ),
        (
            "openposition/from_positionbook_or_default.json",
            "TCS",
            "NSE",
            "MIS",
            0,
        ),
    ] {
        let o = env.sb.open_position(sym, exch, product).await.unwrap();
        check_ok(rel, &o);
        assert_eq!(o.quantity, qty);
    }

    // Holdings after T+1.
    env.set_time("2026-10-06 00:00:05");
    env.sb.t1_settlement().await.unwrap();
    env.set_time("2026-10-06 09:30:00");
    let h = env.sb.holdings().await.unwrap();
    assert_eq!(h.data.holdings.len(), 1);
    let (_, fx) = fixture("holdings/after_cnc_buys.json");
    // The recorded fixture has no rows (it was taken before T+1); compare the
    // envelope and statistics, then the row keys against the web's
    // `get_holdings` row.
    assert_shape("holdings/after_cnc_buys.json", &fx, &to_value(&h));
    let row = to_value(&h.data.holdings[0]);
    let keys: std::collections::BTreeSet<&str> = row
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    assert_eq!(
        keys,
        [
            "symbol",
            "exchange",
            "product",
            "quantity",
            "average_price",
            "ltp",
            "pnl",
            "pnlpercent",
            "current_value",
            "settlement_date"
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(kind(&row["quantity"]), "integer");
    assert_eq!(kind(&row["pnlpercent"]), "float");

    // Cancel-all with resting orders, close-all with positions.
    env.set_time("2026-10-06 10:00:00");
    env.sb
        .place_order(limit(req("SBIN", "NSE", "BUY", 1, "LIMIT", "CNC"), "763.3"))
        .await
        .unwrap();
    let all = env.sb.cancel_all_orders().await.unwrap();
    check_ok("cancelallorder/with_open_orders.json", &all);
    assert!(
        !all.canceled_orders.contains(&resting.orderid),
        "only the current session's orders"
    );
    env.sb
        .place_order(req("RELIANCE", "NSE", "BUY", 5, "MARKET", "MIS"))
        .await
        .unwrap();
    let closed = env.sb.close_all_positions().await.unwrap();
    check_ok("closeposition/with_open_positions.json", &closed);
    env.shutdown().await;
}

#[tokio::test]
async fn smart_order_fixtures() {
    let env = populated().await;
    env.ltp("INFY", "NSE", "1035");
    let smart = |action: &str, qty: i64, size: i64| SmartOrderRequest {
        symbol: "INFY".into(),
        exchange: "NSE".into(),
        product: "MIS".into(),
        action: action.into(),
        quantity: qty,
        position_size: size,
        strategy: "fixtures".into(),
        ..Default::default()
    };
    check_ok(
        "placesmartorder/open_from_flat_to_10.json",
        &env.sb
            .place_smart_order(smart("BUY", 10, 10))
            .await
            .unwrap(),
    );
    check_ok(
        "openposition/infy_after_smart_open.json",
        &env.sb.open_position("INFY", "NSE", "MIS").await.unwrap(),
    );
    check_ok(
        "placesmartorder/raise_10_to_15.json",
        &env.sb.place_smart_order(smart("BUY", 5, 15)).await.unwrap(),
    );
    check_ok(
        "placesmartorder/reduce_15_to_5.json",
        &env.sb
            .place_smart_order(smart("SELL", 10, 5))
            .await
            .unwrap(),
    );
    let no = env.sb.place_smart_order(smart("BUY", 0, 5)).await.unwrap();
    check_ok("placesmartorder/no_action_already_at_5.json", &no);
    assert_eq!(
        to_value(&no),
        fixture("placesmartorder/no_action_already_at_5.json").1
    );
    check_ok(
        "placesmartorder/to_zero.json",
        &env.sb.place_smart_order(smart("SELL", 0, 0)).await.unwrap(),
    );
    check_ok(
        "placesmartorder/flat_to_short_minus_3.json",
        &env.sb
            .place_smart_order(smart("SELL", 3, -3))
            .await
            .unwrap(),
    );
    let matched = env
        .sb
        .place_smart_order(smart("SELL", 3, -3))
        .await
        .unwrap();
    assert_eq!(
        to_value(&matched),
        fixture("placesmartorder/no_action_qty_nonzero_position_matches.json").1
    );
    let o = env.sb.open_position("INFY", "NSE", "MIS").await.unwrap();
    check_ok("openposition/infy_after_smart_short.json", &o);
    assert_eq!(o.quantity, -3);
    let mut bad = smart("BUY", 1, 1);
    bad.symbol = "NOTASYMBOL".into();
    let e = env.sb.place_smart_order(bad).await.unwrap_err();
    check_err("placesmartorder/error_unknown_symbol.json", &e, true);
    env.shutdown().await;
}

#[tokio::test]
async fn gtt_fixtures() {
    let env = populated().await;
    let single = GttRequest {
        trigger_type: "SINGLE".into(),
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        product: "CNC".into(),
        quantity: 1,
        pricetype: "LIMIT".into(),
        price: Some(d("858.7")),
        triggerprice_sl: Some(d("858.7")),
        strategy: Some("fixtures".into()),
        ..Default::default()
    };
    let s = env.sb.place_gtt(single.clone()).await.unwrap();
    check_ok("placegttorder/single_buy_cnc_trigger_below.json", &s);
    let oco = GttRequest {
        trigger_type: "OCO".into(),
        action: "SELL".into(),
        price: Some(d("954.1")),
        triggerprice_sl: Some(d("858.7")),
        stoploss: Some(d("849.1")),
        triggerprice_tg: Some(d("1049.5")),
        target: Some(d("1059.1")),
        ..single.clone()
    };
    let o = env.sb.place_gtt(oco.clone()).await.unwrap();
    check_ok("placegttorder/oco_sell_cnc.json", &o);
    let book = env.sb.gtt_orderbook(Some("active")).await.unwrap();
    check_ok("gttorderbook/after_place.json", &book);
    check_ok("gttorderbook/status_active.json", &book);
    let (_, fx) = fixture("gttorderbook/after_place.json");
    let ours = to_value(&book);
    // Same margins and trigger prices as the recording (SBIN at 954.1).
    let find = |v: &Value, ty: &str| {
        v["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["trigger_type"] == ty)
            .unwrap()
            .clone()
    };
    for ty in ["single", "two-leg"] {
        assert_eq!(
            find(&ours, ty)["margin_blocked"],
            find(&fx, ty)["margin_blocked"],
            "{ty} margin"
        );
        assert_eq!(
            find(&ours, ty)["trigger_prices"],
            find(&fx, ty)["trigger_prices"],
            "{ty} triggers"
        );
        assert_eq!(find(&ours, ty)["legs"], find(&fx, ty)["legs"], "{ty} legs");
        assert_eq!(find(&ours, ty)["last_price"], find(&fx, ty)["last_price"]);
    }

    let mut m = single.clone();
    m.quantity = 2;
    m.price = Some(d("839.6"));
    m.triggerprice_sl = Some(d("839.6"));
    check_ok(
        "modifygttorder/single_change_trigger_and_qty.json",
        &env.sb.modify_gtt(&s.trigger_id, m).await.unwrap(),
    );
    let mut mo = oco.clone();
    mo.triggerprice_tg = Some(d("1097.2"));
    mo.target = Some(d("1106.8"));
    check_ok(
        "modifygttorder/oco_change_target.json",
        &env.sb.modify_gtt(&o.trigger_id, mo).await.unwrap(),
    );
    let book = env.sb.gtt_orderbook(Some("active")).await.unwrap();
    check_ok("gttorderbook/after_modify.json", &book);
    let ours = to_value(&book);
    let (_, fx) = fixture("gttorderbook/after_modify.json");
    for ty in ["single", "two-leg"] {
        assert_eq!(
            find(&ours, ty)["margin_blocked"],
            find(&fx, ty)["margin_blocked"],
            "{ty} margin after modify"
        );
    }

    check_ok(
        "cancelgttorder/single.json",
        &env.sb.cancel_gtt(&s.trigger_id).await.unwrap(),
    );
    let e = env.sb.cancel_gtt(&s.trigger_id).await.unwrap_err();
    check_err("cancelgttorder/error_already_cancelled.json", &e, false);
    assert_eq!(
        e.message,
        format!("No active GTT with trigger_id '{}'", s.trigger_id)
    );
    let e = env.sb.cancel_gtt("GTT-000000-deadbeef").await.unwrap_err();
    check_err("cancelgttorder/error_unknown_trigger_id.json", &e, true);
    check_ok(
        "gttorderbook/status_all.json",
        &env.sb.gtt_orderbook(None).await.unwrap(),
    );
    check_ok(
        "gttorderbook/final.json",
        &env.sb.gtt_orderbook(Some("active")).await.unwrap(),
    );

    // The web's OCO cancel failed (its reconciliation had released the
    // reservation); ours succeeds, and a failed release still has the web's
    // error shape with the trigger id.
    let ok = env.sb.cancel_gtt(&o.trigger_id).await.unwrap();
    assert_eq!(ok.trigger_id, o.trigger_id);
    let synthetic = SandboxError::internal(
        "Could not release 1106.80 margin (x). The GTT is unchanged - retry the cancel.",
    )
    .with_trigger_id(o.trigger_id.clone());
    check_err("cancelgttorder/oco.json", &synthetic, false);
    assert_eq!(env.used().await, d("0"));

    let mut unknown = single.clone();
    unknown.symbol = "NOTASYMBOL".into();
    unknown.price = Some(d("900"));
    unknown.triggerprice_sl = Some(d("900"));
    check_err(
        "placegttorder/error_unknown_symbol.json",
        &env.sb.place_gtt(unknown).await.unwrap_err(),
        true,
    );
    let _ = env.sb.on_tick(Tick::new("SBIN", "NSE", d("954.1"))).await;
    env.shutdown().await;
}
