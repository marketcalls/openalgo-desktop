//! Delta Exchange mapping tests against recorded payload shapes
//! (`src-tauri/tests/fixtures/brokers/deltaexchange/`, no account data).

use super::data::{chunk_days, ist_epoch, parse_candle, to_depth, to_quote};
use super::funds::funds_from;
use super::mapping::*;
use super::master_contract::{canonical_symbol, expiry, instrument_type, parse_products};
use super::streaming::DeltaFeed;
use super::*;
use crate::brokers::common::mapping::{Action, PriceType, Validity};
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription, Message};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::str::FromStr;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!(
            "../../../tests/fixtures/brokers/deltaexchange/",
            $name
        ))
    };
}

fn result(text: &str) -> Value {
    let v: Value = serde_json::from_str(text).unwrap();
    assert_eq!(v["success"], true);
    v["result"].clone()
}

fn products() -> Vec<Value> {
    let mut all = result(fixture!("products_page1.json"))
        .as_array()
        .unwrap()
        .clone();
    all.extend(
        result(fixture!("products_page2.json"))
            .as_array()
            .unwrap()
            .clone(),
    );
    all
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load_master(parse_products(&products()));
    r
}

fn row(symbol: &str) -> SymToken {
    master().by_symbol("CRYPTO", symbol).unwrap()
}

use crate::brokers::common::symbols::SymToken;

fn resolved(symbol: &str, action: Action, pt: PriceType, price: f64, trig: f64) -> ResolvedOrder {
    ResolvedOrder {
        symbol: symbol.into(),
        exchange: Exchange::Crypto,
        action,
        quantity: 1,
        price,
        trigger_price: trig,
        pricetype: pt,
        product: Product::Nrml,
        validity: Validity::Day,
        disclosed_quantity: 0,
        amo: false,
        instrument: row(symbol),
    }
}

fn qty(v: Value) -> CryptoQuantity {
    CryptoQuantity::parse(Exchange::Crypto, &v).unwrap()
}

// ---------------------------------------------------------------------------
// Master contract
// ---------------------------------------------------------------------------

#[test]
fn master_maps_every_contract_type_to_the_crypto_symbology() {
    let m = parse_products(&products());
    // 12 products: one halted, one reduce-only, one duplicate id dropped.
    assert_eq!(m.rows.len(), 9);
    let r = master();
    let perp = r.by_symbol("CRYPTO", "BTCUSDFUT").unwrap();
    assert_eq!(
        (
            perp.brsymbol.as_str(),
            perp.token.as_str(),
            perp.instrument_type.as_str(),
            perp.expiry.as_str(),
            perp.name.as_str(),
            perp.brexchange.as_str(),
        ),
        ("BTCUSD", "27", "PERPFUT", "", "BTC", "DELTAIN")
    );
    assert_eq!(perp.tick_size, 0.5);
    assert_eq!(perp.lot_size, 1);
    let fut = r.by_symbol("CRYPTO", "BTC27NOV26FUT").unwrap();
    assert_eq!(
        (
            fut.brsymbol.as_str(),
            fut.expiry.as_str(),
            fut.instrument_type.as_str()
        ),
        ("BTCUSD27Nov2026", "27-NOV-26", "FUT")
    );
    let ce = r.by_symbol("CRYPTO", "BTC27NOV2662000CE").unwrap();
    assert_eq!(
        (ce.brsymbol.as_str(), ce.strike),
        ("C-BTC-62000-271126", 62000.0)
    );
    assert_eq!(ce.instrument_type, "CE");
    // Strike missing from the API: parsed from the symbol.
    let pe = r.by_symbol("CRYPTO", "BTC27NOV2660000PE").unwrap();
    assert_eq!(pe.strike, 60000.0);
    // Turbo call renders as CE but keeps its own type.
    let tce = r.by_symbol("CRYPTO", "ETH27NOV262500CE").unwrap();
    assert_eq!(tce.instrument_type, "TCE");
    let spot = r.by_symbol("CRYPTO", "BTCINR").unwrap();
    assert_eq!(
        (
            spot.brsymbol.as_str(),
            spot.instrument_type.as_str(),
            spot.lot_size
        ),
        ("BTC_INR", "SPOT", 1)
    );
    // Move options stay native.
    assert!(r.by_symbol("CRYPTO", "MV-BTC-62000-271126").is_some());
    assert!(r.by_brsymbol("CRYPTO", "SOLUSD").is_none());
    assert!(r.by_brsymbol("CRYPTO", "XRPUSD").is_none());
    // Contract values: the first BTCUSD row wins over the duplicate.
    assert_eq!(r.contract_value("BTCUSDFUT", "CRYPTO"), Some(0.001));
    assert_eq!(r.contract_value("ETHUSDFUT", "CRYPTO"), Some(0.01));
    assert_eq!(r.contract_value("BTCINR", "CRYPTO"), Some(1.0));
    // Option chains look up the underlying by name.
    assert_eq!(
        r.snapshot().expiries("CRYPTO", "BTC", Some("CE")),
        ["27-NOV-26"]
    );
}

#[test]
fn canonical_symbols_follow_crypto_symbol_format_doc() {
    // The worked examples of docs/prompt/crypto-symbol-format.md.
    assert_eq!(canonical_symbol("BTCUSD", "PERPFUT", ""), "BTCUSDFUT");
    assert_eq!(canonical_symbol("ETHUSD", "PERPFUT", ""), "ETHUSDFUT");
    assert_eq!(
        canonical_symbol("BTCUSD28Feb2025", "FUT", "28-FEB-25"),
        "BTC28FEB25FUT"
    );
    assert_eq!(
        canonical_symbol("C-BTC-62000-280225", "CE", "28-FEB-25"),
        "BTC28FEB2562000CE"
    );
    assert_eq!(
        canonical_symbol("P-BTC-62000-280225", "PE", "28-FEB-25"),
        "BTC28FEB2562000PE"
    );
    assert_eq!(
        canonical_symbol("C-BTC-65000-280225", "SYNCE", "28-FEB-25"),
        "BTC28FEB2565000CE"
    );
    assert_eq!(
        canonical_symbol("P-BTC-60000-280225", "TPE", "28-FEB-25"),
        "BTC28FEB2560000PE"
    );
    assert_eq!(canonical_symbol("BTC_INR", "SPOT", ""), "BTCINR");
    assert_eq!(canonical_symbol("ETH_USDT", "SPOT", ""), "ETHUSDT");
    assert_eq!(canonical_symbol("SPREAD-X", "SPREAD", ""), "SPREAD-X");
    // Quote currencies are stripped; an unknown one keeps the de-dated base.
    assert_eq!(
        canonical_symbol("ETHUSDT27Nov2026", "FUT", "27-NOV-26"),
        "ETH27NOV26FUT"
    );
    assert_eq!(
        canonical_symbol("SOLXYZ27Nov2026", "FUT", "27-NOV-26"),
        "SOLXYZ27NOV26FUT"
    );
    // An option symbol in an unexpected shape stays native.
    assert_eq!(
        canonical_symbol("C-BTC-62000", "CE", "28-FEB-25"),
        "C-BTC-62000"
    );
    assert_eq!(instrument_type("perpetual_futures"), "PERPFUT");
    assert_eq!(instrument_type("synth_put_options"), "SYNPE");
    assert_eq!(instrument_type("new_type"), "NEW_TYPE");
    assert_eq!(expiry("2025-02-28T12:00:00Z"), "28-FEB-25");
    assert_eq!(expiry(""), "");
}

// ---------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------

#[test]
fn place_payload_matches_the_web_transform() {
    let o = resolved("BTCUSDFUT", Action::Buy, PriceType::Limit, 60000.0, 0.0);
    let p = place_payload(&o, &qty(json!(1))).unwrap();
    assert_eq!(
        p,
        json!({"product_id": 27, "product_symbol": "BTCUSD", "size": 1, "side": "buy",
               "order_type": "limit_order", "time_in_force": "gtc", "limit_price": "60000.0"})
    );
    let mut sl = resolved("ETHUSDFUT", Action::Sell, PriceType::Sl, 2400.0, 2401.5);
    sl.validity = Validity::Ioc;
    let p = place_payload(&sl, &qty(json!(3))).unwrap();
    assert_eq!(p["order_type"], "limit_order");
    assert_eq!(p["stop_order_type"], "stop_loss_order");
    assert_eq!(p["stop_price"], "2401.5");
    assert_eq!(p["limit_price"], "2400.0");
    assert_eq!(p["stop_trigger_method"], "last_traded_price");
    assert_eq!(p["time_in_force"], "ioc");
    assert_eq!(p["side"], "sell");
    let slm = resolved("ETHUSDFUT", Action::Sell, PriceType::SlM, 0.0, 2400.0);
    let p = place_payload(&slm, &qty(json!(1))).unwrap();
    assert_eq!(p["order_type"], "market_order");
    assert!(p.get("limit_price").is_none());
    assert_eq!(p["stop_price"], "2400.0");
    let mkt = resolved("BTCUSDFUT", Action::Buy, PriceType::Market, 0.0, 0.0);
    let p = place_payload(&mkt, &qty(json!(2))).unwrap();
    assert_eq!(p["order_type"], "market_order");
    assert!(p.get("limit_price").is_none() && p.get("stop_price").is_none());
    // A limit order without a price sends "0" (web).
    let lim0 = resolved("BTCUSDFUT", Action::Buy, PriceType::Limit, 0.0, 0.0);
    assert_eq!(
        place_payload(&lim0, &qty(json!(1))).unwrap()["limit_price"],
        "0"
    );
}

#[test]
fn fractional_sizes_round_trip_exactly_on_spot_only() {
    let spot = resolved("BTCINR", Action::Buy, PriceType::Market, 0.0, 0.0);
    for (input, wire) in [
        (json!(0.0005), "0.0005"),
        (json!("0.0005"), "0.0005"),
        (json!(0.123456789), "0.123456789"),
        (json!("1e-4"), "0.0001"),
        (json!(2), "2.0"),
    ] {
        let p = place_payload(&spot, &qty(input.clone())).unwrap();
        // The body text carries the size exactly.
        let text = p.to_string();
        assert!(
            text.contains(&format!("\"size\":{}", wire)),
            "{} -> {}",
            input,
            text
        );
        let back = Decimal::from_str(&p["size"].to_string()).unwrap();
        assert_eq!(back.normalize(), qty(input).as_decimal(), "{}", text);
    }
    // Derivatives are whole contracts: a fraction is refused, not rounded.
    let perp = resolved("BTCUSDFUT", Action::Buy, PriceType::Market, 0.0, 0.0);
    let e = place_payload(&perp, &qty(json!(1.5))).unwrap_err();
    assert_eq!(
        e.client_message(),
        "Fractional quantity (1.5) not allowed for derivative contracts. Use whole numbers for BTCUSDFUT."
    );
    assert_eq!(
        place_payload(&perp, &qty(json!(4.0))).unwrap()["size"],
        json!(4)
    );
}

#[test]
fn modify_and_cancel_bodies_carry_the_composite_id() {
    let m = ResolvedModify {
        order_id: "27:7001".into(),
        symbol: "BTCUSDFUT".into(),
        exchange: Exchange::Crypto,
        action: Action::Buy,
        product: Product::Nrml,
        pricetype: PriceType::Limit,
        quantity: 2,
        price: 59000.0,
        trigger_price: 0.0,
        disclosed_quantity: 0,
        instrument: row("BTCUSDFUT"),
    };
    assert_eq!(
        modify_payload(&m, &qty(json!(2))).unwrap(),
        json!({"id": 7001, "product_id": 27, "size": 2, "limit_price": "59000.0"})
    );
    let slm = ResolvedModify {
        pricetype: PriceType::SlM,
        trigger_price: 58000.0,
        ..m.clone()
    };
    let p = modify_payload(&slm, &qty(json!(2))).unwrap();
    assert_eq!(
        (p["limit_price"].as_str(), p["stop_price"].as_str()),
        (Some("0"), Some("58000.0"))
    );
    // A bare id takes the product from the master.
    let bare = ResolvedModify {
        order_id: "7001".into(),
        ..m.clone()
    };
    assert_eq!(
        modify_payload(&bare, &qty(json!(1))).unwrap()["product_id"],
        27
    );
    let spot = ResolvedModify {
        order_id: "1600:7004".into(),
        instrument: row("BTCINR"),
        ..m
    };
    assert_eq!(
        modify_payload(&spot, &qty(json!("0.25"))).unwrap()["size"],
        json!(0.25)
    );
    assert_eq!(
        cancel_body("27:7001").unwrap(),
        json!({"id": 7001, "product_id": 27})
    );
    assert_eq!(cancel_body("7001").unwrap(), json!({"id": 7001}));
    assert!(cancel_body("abc").is_err());
    assert!(cancel_body("27:x").is_err());
}

#[test]
fn order_book_rows_use_openalgo_symbols_and_statuses() {
    let r = master();
    let rows: Vec<DeltaOrder> =
        serde_json::from_value(result(fixture!("orders_open.json"))).unwrap();
    let o = map_order(&rows[0], &r);
    assert_eq!(o.order_id, "27:7001");
    assert_eq!(o.symbol, "BTCUSDFUT");
    assert_eq!(
        (o.exchange.as_str(), o.product.as_str()),
        ("CRYPTO", "NRML")
    );
    assert_eq!((o.side.as_str(), o.order_type.as_str()), ("BUY", "LIMIT"));
    assert_eq!(
        (o.quantity, o.filled_quantity, o.pending_quantity),
        (10, 6, 4)
    );
    assert_eq!((o.price, o.average_price), (59500.5, 59500.0));
    assert_eq!((o.status.as_str(), o.validity.as_str()), ("open", "GTC"));
    assert_eq!(o.order_timestamp, "2026-10-03T04:15:22.123456Z");
    // Pending stop order: SL-M, open.
    let o = map_order(&rows[1], &r);
    assert_eq!(
        (
            o.symbol.as_str(),
            o.order_type.as_str(),
            o.status.as_str(),
            o.trigger_price
        ),
        ("ETHUSDFUT", "SL-M", "open", 2400.5)
    );
    let hist: Vec<DeltaOrder> =
        serde_json::from_value(result(fixture!("orders_history.json"))).unwrap();
    let o = map_order(&hist[0], &r);
    assert_eq!(
        (
            o.symbol.as_str(),
            o.order_type.as_str(),
            o.status.as_str(),
            o.validity.as_str()
        ),
        ("BTC27NOV2662000CE", "SL", "complete", "IOC")
    );
    assert_eq!(map_order(&hist[2], &r).status, "cancelled");
    // Unknown product: the broker symbol passes through, rejected maps.
    let o = map_order(&hist[4], &r);
    assert_eq!(
        (o.symbol.as_str(), o.status.as_str()),
        ("DOGEUSD", "rejected")
    );
    assert_eq!(map_status("pending"), "open");
    assert_eq!(map_status("filled"), "complete");
    assert_eq!(map_status("Weird"), "weird");
}

#[test]
fn ist_day_filter_and_dates() {
    let today = NaiveDate::from_ymd_opt(2026, 10, 3).unwrap();
    // 20:30 UTC on the 2nd is 02:00 IST on the 3rd.
    assert!(is_on_ist_day("2026-10-02T20:30:00.000000Z", today));
    assert!(!is_on_ist_day("2026-10-02T18:29:59Z", today));
    assert!(is_on_ist_day("2026-10-03T18:29:59Z", today));
    assert!(!is_on_ist_day("2026-10-03T18:30:00Z", today));
    assert!(!is_on_ist_day("garbage", today));
    assert_eq!(ist_date(1791007200), today);
    assert_eq!(ist_epoch(today, false), 1790965800);
    assert_eq!(ist_epoch(today, true), 1791052199);
}

#[test]
fn trade_book_rows() {
    let r = master();
    let fills: Vec<DeltaFill> = serde_json::from_value(result(fixture!("fills.json"))).unwrap();
    let t = map_trade(&fills[0], &r);
    assert_eq!(
        (
            t.order_id.as_str(),
            t.trade_id.as_str(),
            t.symbol.as_str(),
            t.side.as_str()
        ),
        ("27:7001", "880001", "BTCUSDFUT", "BUY")
    );
    assert_eq!(
        (t.quantity, t.average_price, t.trade_value),
        (6, 59500.0, 357000.0)
    );
    // A fractional spot fill keeps its exact value even though the shared
    // row's quantity is whole units.
    let t = map_trade(&fills[1], &r);
    assert_eq!((t.symbol.as_str(), t.quantity), ("BTCINR", 0));
    assert!((t.trade_value - 2976.0).abs() < 1e-9);
    assert_eq!(
        map_trade_exact(&fills[1], &r).quantity.to_string(),
        "0.0005"
    );
    assert_eq!(map_trade_exact(&fills[0], &r).quantity.to_string(), "6");
}

#[test]
fn positions_join_derivatives_and_whole_spot_balances() {
    let r = master();
    let margined: Vec<DeltaPosition> =
        serde_json::from_value(result(fixture!("positions_margined.json"))).unwrap();
    let wallet: Vec<WalletBalance> =
        serde_json::from_value(result(fixture!("wallet_balances.json"))).unwrap();
    let mut raw: Vec<RawPosition> = margined.iter().map(RawPosition::from).collect();
    let spot = spot_positions(&wallet);
    // BTC 0.0105 - 0.0005 blocked = 0.01 exactly; ETH 2; USD, INR, zero SOL skipped.
    assert_eq!(spot.len(), 2);
    assert_eq!(spot[0].product_symbol, "BTC_INR");
    assert_eq!(spot[0].size, Decimal::from_str("0.0100").unwrap());
    assert!(spot[0].is_spot);
    raw.extend(spot);
    let book: Vec<Position> = raw.iter().filter_map(|p| map_position(p, &r)).collect();
    assert_eq!(book.len(), 3);
    assert_eq!(
        (
            book[0].symbol.as_str(),
            book[0].product.as_str(),
            book[0].quantity
        ),
        ("BTCUSDFUT", "NRML", 6)
    );
    assert_eq!((book[0].average_price, book[0].pnl), (59500.0, 9.25));
    assert_eq!(
        (book[1].symbol.as_str(), book[1].quantity),
        ("ETHUSDFUT", -3)
    );
    assert_eq!(
        (
            book[2].symbol.as_str(),
            book[2].product.as_str(),
            book[2].quantity
        ),
        ("ETHINR", "CNC", 2)
    );
    // The fractional BTC balance is resolved for close-all.
    assert_eq!(position_row(&raw[2], &r).unwrap().symbol, "BTCINR");
    // The exact book keeps it, at its exact size.
    let exact: Vec<(String, String)> = raw
        .iter()
        .map(|p| map_position_exact(p, &r))
        .map(|e| (e.row.symbol, e.quantity.to_string()))
        .collect();
    assert_eq!(
        exact,
        [
            ("BTCUSDFUT".into(), "6".into()),
            ("ETHUSDFUT".into(), "-3".into()),
            ("BTCINR".into(), "0.01".into()),
            ("ETHINR".into(), "2".into()),
        ]
    );
}

#[test]
fn funds_sum_wallets_and_position_pnl() {
    let margined: Vec<DeltaPosition> =
        serde_json::from_value(result(fixture!("positions_margined.json"))).unwrap();
    let wallet: Vec<WalletBalance> =
        serde_json::from_value(result(fixture!("wallet_balances.json"))).unwrap();
    let f = funds_from(&wallet, &margined);
    assert_eq!(f.available_cash, 155544.27);
    assert_eq!(f.collateral, 10.0);
    assert_eq!(f.utilised_debits, 43.05);
    assert_eq!((f.m2m_realized, f.m2m_unrealized), (12.5, 1.5));
    let empty = funds_from(&[], &[]);
    assert_eq!(empty.available_cash, 0.0);
}

// ---------------------------------------------------------------------------
// Market data
// ---------------------------------------------------------------------------

#[test]
fn quote_and_depth_from_ticker_and_book() {
    let key = QuoteKey::new("CRYPTO", "BTCUSDFUT");
    let t = result(fixture!("ticker_btcusd.json"));
    let q = to_quote(&key, &t);
    assert_eq!(
        (q.ltp, q.open, q.high, q.low, q.close),
        (59876.12, 59100.0, 60500.0, 58500.5, 59000.0)
    );
    assert_eq!(
        (q.bid, q.ask, q.volume, q.oi),
        (59875.5, 59877.0, 152340, 12345)
    );
    let book = result(fixture!("l2orderbook.json"));
    let d = to_depth(&key, &t, Some(&book));
    assert_eq!(d.bids.len(), 5);
    assert_eq!(d.asks.len(), 5);
    assert_eq!((d.bids[0].price, d.bids[0].quantity), (59875.5, 800));
    assert_eq!(d.bids[3], DepthLevel::default());
    assert_eq!((d.asks[4].price, d.asks[4].quantity), (59879.0, 100));
    assert_eq!((d.total_buy_qty, d.total_sell_qty), (1700, 2000));
    assert_eq!((d.ltp, d.prev_close, d.ltq), (59876.12, 59000.0, 0));
    let empty = to_depth(&key, &t, None);
    assert!(empty.bids.iter().all(|l| *l == DepthLevel::default()));
    assert_eq!(empty.ltp, 59876.12);
}

#[test]
fn candles_in_both_shapes_and_the_web_chunk_table() {
    let arr = result(fixture!("candles_1h.json"));
    let c = parse_candle(&arr[1]).unwrap();
    assert_eq!(
        (c.timestamp, c.open, c.close, c.volume, c.oi),
        (1790998200, 59500.0, 59600.0, 1203, 15000)
    );
    assert_eq!(parse_candle(&arr[0]).unwrap().oi, 0);
    let named = result(fixture!("candles_1d.json"));
    let c = parse_candle(&named[0]).unwrap();
    assert_eq!(
        (c.timestamp, c.low, c.volume),
        (1790879400, 57800.5, 152340)
    );
    assert!(parse_candle(&json!([1, 2, 3])).is_none());
    for (res, days) in [
        ("1m", 1),
        ("3m", 7),
        ("5m", 12),
        ("15m", 30),
        ("30m", 60),
        ("1h", 90),
        ("6h", 90),
        ("1d", 0),
        ("1w", 0),
    ] {
        assert_eq!(chunk_days(res), days, "{}", res);
    }
    let keys: Vec<&str> = TIMEFRAME_MAP.iter().map(|(k, _)| *k).collect();
    assert_eq!(
        keys,
        ["1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "6h", "1d", "D", "1w", "W"]
    );
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

fn sub(symbol: &str, br: &str, mode: FeedMode) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: "CRYPTO".into(),
        token: String::new(),
        brsymbol: br.into(),
        brexchange: "DELTAIN".into(),
        mode,
        depth: 5,
    }
}

fn texts(frames: &[Message]) -> Vec<Value> {
    frames
        .iter()
        .map(|m| match m {
            Message::Text(t) => serde_json::from_str(t).unwrap(),
            other => panic!("unexpected frame {:?}", other),
        })
        .collect()
}

#[test]
fn feed_subscribe_frames_follow_the_channel_limits() {
    let mut f = DeltaFeed::new(WS_PUBLIC_URL);
    let frames = texts(&f.subscribe_frames(&[
        sub("BTCUSDFUT", "BTCUSD", FeedMode::Depth),
        sub("ETHUSDFUT", "ETHUSD", FeedMode::Quote),
        sub("BTC27NOV2662000CE", "C-BTC-62000-271126", FeedMode::Depth),
    ]));
    assert_eq!(frames.len(), 3);
    assert_eq!(
        frames[0],
        json!({"type": "subscribe", "payload": {"channels": [{"name": "ticker",
            "symbols": ["BTCUSD", "ETHUSD", "C-BTC-62000-271126"]}]}})
    );
    assert_eq!(frames[1]["payload"]["channels"][0]["name"], "ob_l2");
    assert_eq!(
        frames[1]["payload"]["channels"][0]["symbols"],
        json!(["BTCUSD"])
    );
    assert_eq!(
        frames[2]["payload"]["channels"][0]["symbols"],
        json!(["C-BTC-62000-271126"])
    );
    // Mode changes move only the book.
    let up = texts(&f.mode_change_frames(
        &sub("ETHUSDFUT", "ETHUSD", FeedMode::Quote),
        &sub("ETHUSDFUT", "ETHUSD", FeedMode::Depth),
    ));
    assert_eq!(up.len(), 1);
    assert_eq!(
        (
            up[0]["type"].as_str(),
            up[0]["payload"]["channels"][0]["name"].as_str()
        ),
        (Some("subscribe"), Some("ob_l2"))
    );
    let down = texts(&f.mode_change_frames(
        &sub("BTCUSDFUT", "BTCUSD", FeedMode::Depth),
        &sub("BTCUSDFUT", "BTCUSD", FeedMode::Ltp),
    ));
    assert_eq!(down[0]["type"], "unsubscribe");
    assert!(f
        .mode_change_frames(
            &sub("BTCUSDFUT", "BTCUSD", FeedMode::Ltp),
            &sub("BTCUSDFUT", "BTCUSD", FeedMode::Quote),
        )
        .is_empty());
    let un = texts(&f.unsubscribe_frames(&[sub("ETHUSDFUT", "ETHUSD", FeedMode::Depth)]));
    assert_eq!(un.len(), 2);
    assert_eq!(un[0]["type"], "unsubscribe");
    assert!(f
        .ws_request()
        .unwrap()
        .uri()
        .to_string()
        .starts_with("wss://public-socket.india.delta.exchange"));
    assert!(f.heartbeat().is_some());
}

#[test]
fn feed_decodes_ticker_and_book_frames_and_merges_them() {
    let mut f = DeltaFeed::new(WS_PUBLIC_URL);
    f.subscribe_frames(&[
        sub("BTCUSDFUT", "BTCUSD", FeedMode::Depth),
        sub("ETHUSDFUT", "ETHUSD", FeedMode::Ltp),
    ]);
    let lines: Vec<&str> = fixture!("ws_frames.jsonl").lines().collect();
    assert_eq!(
        f.parse(&Message::Text(lines[0].into())),
        vec![FeedEvent::Heartbeat]
    );
    let ev = f.parse(&Message::Text(lines[1].into()));
    let FeedEvent::Tick(t) = &ev[0] else {
        panic!("{:?}", ev)
    };
    assert_eq!(
        (t.symbol.as_str(), t.exchange.as_str(), t.mode),
        ("BTCUSDFUT", "CRYPTO", 3)
    );
    assert_eq!(
        (t.ltp, t.open, t.high, t.low, t.close, t.oi),
        (59876.12, 59100.0, 60500.0, 58500.5, 59870.0, 12345)
    );
    // No mark price: the frame's spot price is the LTP.
    let ev = f.parse(&Message::Text(lines[2].into()));
    let FeedEvent::Tick(t) = &ev[0] else {
        panic!("{:?}", ev)
    };
    assert_eq!(
        (t.symbol.as_str(), t.ltp, t.mode, t.close),
        ("ETHUSDFUT", 2451.2, 1, 2450.0)
    );
    let ev = f.parse(&Message::Text(lines[3].into()));
    let FeedEvent::Depth(d) = &ev[0] else {
        panic!("{:?}", ev)
    };
    assert_eq!(d.symbol, "BTCUSDFUT");
    assert_eq!(d.ltp, 59876.12);
    assert_eq!((d.buy.len(), d.sell.len()), (5, 5));
    assert_eq!((d.buy[0].price, d.buy[0].quantity), (59875.5, 800));
    assert_eq!(d.sell[2], DepthLevel::default());
    assert_eq!((d.total_buy_quantity, d.total_sell_quantity), (1775, 1300));
    // A partial ticker keeps the last LTP and OHLC; only OI moves.
    let ev = f.parse(&Message::Text(lines[4].into()));
    let FeedEvent::Tick(t) = &ev[0] else {
        panic!("{:?}", ev)
    };
    assert_eq!((t.ltp, t.open, t.oi), (59876.12, 59100.0, 12400));
    // Unsubscribed symbols and error frames produce nothing.
    assert!(f.parse(&Message::Text(lines[5].into())).is_empty());
    assert!(f.parse(&Message::Text(lines[6].into())).is_empty());
    assert!(f.parse(&Message::Binary(vec![1, 2])).is_empty());
    assert!(f.parse(&Message::Text("not json".into())).is_empty());
    // The cache is bounded by the subscriptions.
    assert_eq!(f.cached(), 2);
    f.unsubscribe_frames(&[sub("BTCUSDFUT", "BTCUSD", FeedMode::Depth)]);
    assert_eq!(f.cached(), 1);
    assert!(f.parse(&Message::Text(lines[3].into())).is_empty());
}

#[test]
fn adapter_identity() {
    let b = DeltaBroker::new(SymbolResolver::new());
    assert_eq!(b.id(), "deltaexchange");
    assert_eq!(b.login_kind(), LoginKind::ApiKeySecret);
    assert_eq!(b.supported_exchanges(), &[Exchange::Crypto]);
    assert!(b.leverage_config());
    assert_eq!(b.broker_type(), "crypto");
    assert!(!b.requires_totp());
    let c = b.capabilities();
    assert!(c.history && c.margin && c.streaming && !c.gtt && !c.multiquotes_batch);
    assert_eq!(py_str(60000.0), "60000.0");
    assert_eq!(py_str(0.5), "0.5");
    assert_eq!(py_str(59500.25), "59500.25");
}
