//! Samco mapping, master-contract and feed unit tests. Payload shapes are
//! built from the web code (`broker/samco/**`) and the Samco Trade API docs;
//! no account data.

use super::auth::{self, IpStatus};
use super::mapping::*;
use super::master_contract::{self, convert_date};
use super::streaming::{request_frame, unquote, SamcoFeed};
use super::*;
use crate::brokers::common::mapping::{Action, PriceType, Product, Validity};
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription, Message};
use crate::brokers::common::symbols::SymToken;
use serde_json::json;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../../tests/fixtures/brokers/samco/", $name))
    };
}

fn v(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

fn sym(symbol: &str, br: &str, ex: &str, brex: &str, token: &str, tick: f64) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: br.into(),
        name: symbol.into(),
        exchange: ex.into(),
        brexchange: brex.into(),
        token: token.into(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: "EQ".into(),
        tick_size: tick,
    }
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(vec![
        sym("SBIN", "SBIN-EQ", "NSE", "NSE", "3045_NSE", 0.05),
        sym(
            "NIFTY28OCT2624600CE",
            "NIFTY26OCT24600CE",
            "NFO",
            "NFO",
            "41015_NFO",
            0.05,
        ),
        sym(
            "CRUDEOIL20OCT26FUT",
            "CRUDEOIL26OCTFUT",
            "MCX",
            "MFO",
            "464925_MFO",
            1.0,
        ),
    ]);
    r
}

#[test]
fn status_map_matches_web() {
    for (raw, want) in [
        ("OPEN", "open"),
        ("Pending", "open"),
        ("ordered", "open"),
        ("Trigger Pending", "open"),
        ("After Market Order Req Received", "open"),
        ("Complete", "complete"),
        ("Executed", "complete"),
        ("filled", "complete"),
        ("Canceled", "cancelled"),
        ("Rejected", "rejected"),
        ("Modified", "modified"),
    ] {
        assert_eq!(map_status(raw), want, "{}", raw);
    }
}

#[test]
fn order_type_and_product_maps() {
    assert_eq!(order_type(PriceType::Market), "MKT");
    assert_eq!(order_type(PriceType::Limit), "L");
    assert_eq!(order_type(PriceType::SlM), "SL-M");
    assert_eq!(product(Product::Nrml), "NRML");
    assert_eq!(reverse_product("CNC"), "CNC");
    assert_eq!(reverse_product("BO"), "MIS");
    assert_eq!(reverse_order_type("L", Some(&json!("3"))), "MARKET");
    // Python truthiness: the string "0" is truthy, an empty string is not.
    assert_eq!(reverse_order_type("L", Some(&json!("0"))), "MARKET");
    assert_eq!(reverse_order_type("L", Some(&json!(""))), "LIMIT");
    assert_eq!(reverse_order_type("L", None), "LIMIT");
    assert_eq!(reverse_order_type("MKT", None), "MARKET");
    assert_eq!(reverse_order_type("SL-M", None), "SL-M");
    assert_eq!(oa_exchange("MFO"), "MCX");
}

#[test]
fn number_parsing_strips_commas() {
    assert_eq!(num(Some(&json!("1,550.25"))), 1550.25);
    assert_eq!(num(Some(&json!("--"))), 0.0);
    assert_eq!(int(Some(&json!("12,345"))), 12345);
    assert_eq!(int(Some(&json!(7.9))), 7);
    assert_eq!(clean_text(Some(&json!("NA"))), "");
    assert_eq!(py_float(101.0), "101.0");
    assert_eq!(py_float(101.25), "101.25");
    assert_eq!(py_g(0.5), "0.5");
    assert_eq!(py_g(3.0), "3");
}

fn resolved(symbol: &str, ex: &str, pt: &str, price: f64, trig: Option<f64>) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: ex.into(),
        side: "BUY".into(),
        quantity: 75,
        price,
        order_type: pt.into(),
        product: "NRML".into(),
        validity: String::new(),
        trigger_price: trig,
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, &master()).unwrap()
}

#[test]
fn market_order_becomes_protected_limit() {
    let o = resolved("SBIN", "NSE", "MARKET", 0.0, None);
    let r = resolve_order_type(
        o.pricetype,
        o.action,
        &o.symbol,
        o.price,
        o.trigger_price,
        Some(0.05),
        Some(812.4),
    )
    .unwrap();
    // EQ above 500: 0.5% -> 816.462 -> tick 0.05 -> 816.45.
    assert_eq!(r.order_type, "L");
    assert_eq!(r.price, "816.45");
    let body = place_body(&o, &r);
    assert_eq!(
        body,
        json!({
            "symbolName": "SBIN-EQ", "exchange": "NSE", "transactionType": "BUY",
            "orderType": "L", "quantity": "75", "disclosedQuantity": "0",
            "orderValidity": "DAY", "productType": "NRML", "afterMarketOrderFlag": "NO",
            "price": "816.45", "marketProtection": "0.5"
        })
    );
    assert!(resolve_order_type(
        PriceType::Market,
        Action::Buy,
        "SBIN",
        0.0,
        0.0,
        None,
        Some(0.0)
    )
    .unwrap_err()
    .starts_with("MARKET order failed"));
}

#[test]
fn option_market_sell_uses_option_slab() {
    let r = resolve_order_type(
        PriceType::Market,
        Action::Sell,
        "NIFTY28OCT2624600CE",
        0.0,
        0.0,
        Some(0.05),
        Some(98.1),
    )
    .unwrap();
    // CE below 100: 3% -> 95.157 -> 95.15.
    assert_eq!((r.order_type, r.price.as_str()), ("L", "95.15"));
    assert_eq!(r.mpp_percentage, Some(3.0));
}

#[test]
fn sl_m_becomes_sl_at_protected_trigger() {
    let o = resolved("SBIN", "NSE", "SL-M", 0.0, Some(800.0));
    let r = resolve_order_type(
        o.pricetype,
        o.action,
        &o.symbol,
        0.0,
        800.0,
        Some(0.05),
        None,
    )
    .unwrap();
    let body = place_body(&o, &r);
    assert_eq!(body["orderType"], "SL");
    assert_eq!(body["price"], "804.0");
    assert_eq!(body["triggerPrice"], "800.0");
    assert_eq!(body["marketProtection"], "0.5");
    assert!(resolve_order_type(PriceType::SlM, Action::Buy, "SBIN", 0.0, 0.0, None, None).is_err());
}

#[test]
fn limit_and_sl_bodies() {
    let o = resolved("SBIN", "NSE", "LIMIT", 812.5, None);
    let r = resolve_order_type(o.pricetype, o.action, "SBIN", 812.5, 0.0, None, None).unwrap();
    let b = place_body(&o, &r);
    assert_eq!(b["price"], "812.5");
    assert!(b.get("triggerPrice").is_none() && b.get("marketProtection").is_none());
    let o = resolved("SBIN", "NSE", "SL", 812.5, Some(810.0));
    let r = resolve_order_type(o.pricetype, o.action, "SBIN", 812.5, 810.0, None, None).unwrap();
    let b = place_body(&o, &r);
    assert_eq!(
        (b["orderType"].as_str(), b["triggerPrice"].as_str()),
        (Some("SL"), Some("810.0"))
    );
}

#[test]
fn modify_body_matches_web() {
    let m = ResolvedModify {
        order_id: "1".into(),
        symbol: "SBIN".into(),
        exchange: crate::brokers::common::mapping::Exchange::Nse,
        action: Action::Buy,
        product: Product::Mis,
        pricetype: PriceType::Limit,
        quantity: 20,
        price: 811.0,
        trigger_price: 0.0,
        disclosed_quantity: 0,
        instrument: sym("SBIN", "SBIN-EQ", "NSE", "NSE", "3045_NSE", 0.05),
    };
    let r = resolve_order_type(m.pricetype, m.action, "SBIN", 811.0, 0.0, None, None).unwrap();
    assert_eq!(
        modify_body(&m, &r),
        json!({"orderType":"L","quantity":"20","orderValidity":"DAY","price":"811.0"})
    );
    let m2 = ResolvedModify {
        disclosed_quantity: 5,
        ..m
    };
    assert_eq!(modify_body(&m2, &r)["disclosedQuantity"], "5");
    let _ = Validity::Day;
}

#[test]
fn order_book_is_normalised_to_openalgo() {
    let book = order_book(&v(fixture!("order_book.json")), &master());
    assert_eq!(book.len(), 4);
    let a = &book[0];
    assert_eq!((a.symbol.as_str(), a.exchange.as_str()), ("SBIN", "NSE"));
    assert_eq!(
        (a.status.as_str(), a.order_type.as_str()),
        ("open", "LIMIT")
    );
    assert_eq!((a.quantity, a.pending_quantity, a.price), (10, 10, 812.5));
    assert_eq!(a.average_price, 0.0);
    assert_eq!(a.rejection_reason, None);
    let b = &book[1];
    assert_eq!(b.symbol, "NIFTY28OCT2624600CE");
    assert_eq!(
        (b.status.as_str(), b.order_type.as_str()),
        ("complete", "MARKET")
    );
    assert_eq!((b.filled_quantity, b.average_price), (75, 101.9));
    let c = &book[2];
    assert_eq!(
        (c.symbol.as_str(), c.exchange.as_str()),
        ("CRUDEOIL20OCT26FUT", "MCX")
    );
    assert_eq!(
        (c.price, c.trigger_price, c.pending_quantity),
        (5412.0, 5400.0, 100)
    );
    assert_eq!(c.status, "open");
    let d = &book[3];
    assert_eq!(
        (d.symbol.as_str(), d.status.as_str()),
        ("UNKNOWN", "rejected")
    );
    assert_eq!(d.rejection_reason.as_deref(), Some("RMS: Margin Exceeds"));
    assert!(order_book(&json!({"status":"Failure"}), &master()).is_empty());
}

#[test]
fn trade_book_rows() {
    let t = trade_book(&v(fixture!("trade_book.json")), &master());
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].symbol, "NIFTY28OCT2624600CE");
    assert_eq!(
        (t[0].quantity, t[0].average_price, t[0].trade_value),
        (75, 101.9, 7642.5)
    );
    assert_eq!(t[0].trade_id, "5001");
}

#[test]
fn positions_are_signed_by_transaction_type() {
    let p = positions(&v(fixture!("positions_day.json")), &master());
    assert_eq!((p[0].symbol.as_str(), p[0].quantity), ("SBIN", 10));
    assert_eq!(
        (p[0].average_price, p[0].ltp, p[0].pnl),
        (1550.26, 1560.0, 97.44)
    );
    assert_eq!((p[1].quantity, p[1].average_price), (-75, 101.9));
    assert_eq!(p[1].pnl, 295.5);
}

#[test]
fn holdings_and_portfolio_stats() {
    let raw = v(fixture!("holdings.json"));
    let h = holdings(&raw, &master());
    assert_eq!(
        (h[0].symbol.as_str(), h[0].quantity, h[0].pnl),
        ("SBIN", 10, 1500.0)
    );
    // pnl / (value - pnl) = 1500 / 15000.
    assert_eq!(h[0].pnl_percentage, 10.0);
    assert_eq!(h[0].product, "CNC");
    let s = portfolio_stats(&raw);
    assert_eq!(
        (
            s.totalholdingvalue,
            s.totalinvvalue,
            s.totalprofitandloss,
            s.totalpnlpercentage
        ),
        (16500.0, 15000.0, 1500.0, 10.0)
    );
    assert_eq!(holding_pnl_percent(10.0, 0.0), 0.0);
    assert!(holdings(&json!({"status":"Failure"}), &master()).is_empty());
}

#[test]
fn funds_use_the_equity_segment() {
    let f = funds(&v(fixture!("limits.json")));
    assert_eq!(f.available_cash, 100250.46);
    assert_eq!(f.collateral, 5000.0);
    assert_eq!(f.utilised_debits, 24749.5);
    assert_eq!((f.m2m_realized, f.m2m_unrealized), (0.0, 0.0));
}

#[test]
fn margin_legs_and_response() {
    let leg = |ex: &str, price: f64| MarginLeg {
        key: QuoteKey::new(ex, "X"),
        action: Action::Sell,
        quantity: 75,
        product: Product::Nrml,
        pricetype: PriceType::Limit,
        price,
        trigger_price: 0.0,
    };
    assert_eq!(
        margin_leg(&leg("NFO", 0.0), Some("NIFTY26OCT24600CE")).unwrap(),
        json!({"exchange":"NFO","tradingSymbol":"NIFTY26OCT24600CE","qty":"75",
               "productType":"NRML","orderType":"L","transactionType":"SELL","price":"0"})
    );
    assert_eq!(
        margin_leg(&leg("NFO", 101.5), Some("B")).unwrap()["price"],
        "101.5"
    );
    assert!(margin_leg(&leg("NSE", 0.0), Some("SBIN-EQ")).is_none());
    assert!(margin_leg(&leg("NFO", 0.0), None).is_none());
    let m = parse_margin(&v(fixture!("span_margin.json"))).unwrap();
    assert_eq!(
        (m.total_margin_required, m.span_margin, m.exposure_margin),
        (152300.25, 110000.0, 42300.25)
    );
    let single =
        parse_margin(&json!({"status":"Success","marginRequired":"500","exposureMargin":"20"}))
            .unwrap();
    assert_eq!(
        (single.total_margin_required, single.span_margin),
        (500.0, 500.0)
    );
    assert_eq!(
        parse_margin(&json!({"status":"Failure","statusMessage":"Invalid scrip"})).unwrap_err(),
        "Invalid scrip"
    );
}

#[test]
fn quote_shapes() {
    let k = QuoteKey::new("NSE", "SBIN");
    let q = quote_from_details(&k, &v(fixture!("quote.json"))["quoteDetails"]);
    assert_eq!(
        (q.ltp, q.bid, q.ask, q.close, q.volume),
        (812.4, 812.35, 812.45, 800.0, 12345678)
    );
    assert_eq!(q.change, 12.4);
    let k = QuoteKey::new("NSE_INDEX", "NIFTY");
    let q = quote_from_index(&k, &v(fixture!("index_quote.json"))["indexDetails"][0]);
    assert_eq!((q.ltp, q.close, q.bid, q.oi), (24612.3, 24450.0, 0.0, 0));
}

#[test]
fn multiquote_joins_on_token_not_trading_symbol() {
    let raw = v(fixture!("multi_quote.json"));
    let entries = raw["multiQuotes"].as_array().unwrap();
    // Samco answers with its compact symbol; the token round-trips.
    let q = match_multiquote(entries, "41015_NFO", "NFO", "NFO", "NIFTY26OCT24600CE").unwrap();
    assert_eq!(text(q.get("tradingSymbol")), "NIFTY2681124600CE");
    let q = match_multiquote(entries, "", "NSE", "NSE", "SBIN").unwrap();
    assert_eq!(text(q.get("symbol")), "3045_NSE");
    assert!(match_multiquote(entries, "1_NSE", "NSE", "NSE", "NOPE").is_none());
    let k = QuoteKey::new("NFO", "NIFTY28OCT2624600CE");
    let q = quote_from_multi(&k, &entries[0]);
    assert_eq!(
        (q.ltp, q.oi, q.bid_qty, q.ask_qty),
        (98.1, 4_500_000, 750, 1500)
    );
}

#[test]
fn depth_pads_to_five_levels() {
    let raw = v(fixture!("market_depth.json"));
    let (b, a, tb, ts) = depth_levels(&raw["MarketDepthDetails"]["marketDepth"]);
    assert_eq!((b.len(), a.len()), (5, 5));
    assert_eq!((b[1].price, b[1].quantity), (812.3, 1200));
    assert_eq!((a[0].price, a[1].price), (812.45, 0.0));
    assert_eq!((tb, ts), (120000, 95000));
}

#[test]
fn history_candles() {
    let raw = v(fixture!("candles_daily.json"));
    let d = daily_candles(raw["historicalCandleData"].as_array().unwrap());
    assert_eq!(d.len(), 2);
    // 2026-09-29 00:00 UTC, sorted, duplicate date dropped (first wins).
    assert_eq!(d[0].timestamp, 1_790_640_000);
    assert_eq!((d[1].close, d[1].volume), (805.5, 10_500_000));
    let raw = v(fixture!("candles_intraday.json"));
    let i = intraday_candles(raw["intradayCandleData"].as_array().unwrap());
    // 2026-10-01 09:15 IST = 03:45 UTC.
    assert_eq!(i[0].timestamp, 1_790_826_300);
    assert_eq!(i[1].timestamp - i[0].timestamp, 300);
    assert_eq!(i[1].volume, 120_000);
    assert_eq!(parse_ist("2026-10-01 09:15"), Some(1_790_826_300));
}

#[test]
fn expiry_formats() {
    for (s, want) in [
        ("2026-10-28", "28-OCT-26"),
        ("28-10-2026", "28-OCT-26"),
        ("28/10/2026", "28-OCT-26"),
        ("28/10/26", "28-OCT-26"),
        ("28OCT2026", "28-OCT-26"),
        ("28Oct26", "28-OCT-26"),
        ("20261028", "28-OCT-26"),
        ("28-Oct-2026", "28-OCT-26"),
        ("28-OCT-26", "28-OCT-26"),
        ("", ""),
        ("weird", "WEIRD"),
    ] {
        assert_eq!(convert_date(s), want, "{}", s);
    }
}

#[test]
fn master_contract_rows() {
    let rows = master_contract::parse(fixture!("scrip_master.csv")).unwrap();
    let find = |ex: &str, token: &str| {
        rows.iter()
            .find(|r| r.exchange == ex && r.token == token)
            .unwrap_or_else(|| panic!("{} {}", ex, token))
            .clone()
    };
    let r = find("NSE", "3045_NSE");
    assert_eq!(
        (r.symbol.as_str(), r.brsymbol.as_str()),
        ("SBIN", "SBIN-EQ")
    );
    assert_eq!(find("NSE", "1234_NSE").symbol, "ABCD");
    assert_eq!(find("BSE", "500112_BSE").symbol, "SBIN");
    let f = find("NFO", "35001_NFO");
    assert_eq!(
        (f.symbol.as_str(), f.instrument_type.as_str()),
        ("BANKNIFTY28OCT26FUT", "FUT")
    );
    assert_eq!(
        (f.expiry.as_str(), f.lot_size, f.tick_size),
        ("28-OCT-26", 30, 0.2)
    );
    let o = find("NFO", "41015_NFO");
    assert_eq!(
        (o.symbol.as_str(), o.instrument_type.as_str()),
        ("NIFTY28OCT2624600CE", "CE")
    );
    assert_eq!(o.strike, 24600.0);
    assert_eq!(find("NFO", "41016_NFO").symbol, "VEDL28OCT26292.5PE");
    let m = find("MCX", "464925_MFO");
    assert_eq!(
        (m.symbol.as_str(), m.brexchange.as_str()),
        ("CRUDEOIL20OCT26FUT", "MFO")
    );
    assert_eq!(find("MCX", "470001_MFO").symbol, "CRUDEOIL16OCT265400CE");
    assert_eq!(find("CDS", "8001_CDS").symbol, "USDINR27OCT26FUT");
    assert_eq!(find("CDS", "8002_CDS").symbol, "USDINR27OCT2683.25CE");
    assert_eq!(find("BFO", "9001_BFO").symbol, "SENSEX30OCT2681000CE");
    let bf = find("BFO", "9002_BFO");
    assert_eq!(
        (bf.symbol.as_str(), bf.instrument_type.as_str()),
        ("SENSEX30OCT26FUT", "FUT")
    );
    let idx = find("NSE_INDEX", "26000_NSE");
    assert_eq!(idx.symbol, "NIFTY");
    // 13 CSV rows plus the 68 fixed indices.
    assert_eq!(rows.len(), 13 + 68);
}

#[test]
fn fixed_index_list_matches_web() {
    let idx = master_contract::index_rows();
    assert_eq!(idx.len(), 68);
    assert_eq!(idx.iter().filter(|r| r.exchange == "NSE_INDEX").count(), 39);
    assert_eq!(idx.iter().filter(|r| r.exchange == "BSE_INDEX").count(), 29);
    let n = idx.iter().find(|r| r.symbol == "NIFTY").unwrap();
    assert_eq!(
        (n.brsymbol.as_str(), n.token.as_str()),
        ("NIFTY 50", "NIFTY_50")
    );
    assert_eq!(
        (n.lot_size, n.tick_size, n.instrument_type.as_str()),
        (1, 0.05, "INDEX")
    );
    let s = idx.iter().find(|r| r.symbol == "SENSEX50").unwrap();
    assert_eq!(
        (s.brsymbol.as_str(), s.brexchange.as_str()),
        ("SNSX50", "BSE")
    );
    assert!(master_contract::parse("nope,header\n1,2\n").is_err());
}

#[test]
fn ip_status_json_matches_web_route() {
    let s = IpStatus::from_whoami(&v(fixture!("whoami.json")));
    assert_eq!(
        s.to_json(),
        json!({
            "status": "success",
            "src_ip": "203.0.113.10",
            "primary_ip": "203.0.113.10",
            "secondary_ip": "",
            "matches": true,
            "matched_as": "primary",
            "message": "Source IP matches the registered primary IP",
            "dashboard_url": "https://tradeapi.samco.in/app/login"
        })
    );
    let empty = IpStatus::from_whoami(&json!({"status":"Success"}));
    assert_eq!(empty.to_json()["matched_as"], Value::Null);
    assert_eq!(empty.to_json()["matches"], false);
    assert_eq!(
        auth::ip_status_error(IP_STATUS_NOT_LOGGED_IN),
        (401, json!({"status":"error","message":"Not logged in"}))
    );
    assert_eq!(auth::ip_status_error(IP_STATUS_NOT_CONNECTED).0, 400);
    assert_eq!(auth::ip_status_error("x").1["status"], "error");
}

#[test]
fn auth_error_messages_carry_the_fix() {
    let m = auth::error_message(
        &json!({"statusMessage":"Invalid API key","errorCode":"EOAUTH001"}),
        "d",
    );
    assert!(m.starts_with("Invalid API key. Check that the API key"));
    assert!(
        auth::error_message(&json!({"errorCode":"EOAUTH009"}), "Failed").contains("Static IPs")
    );
    assert_eq!(auth::error_message(&json!({}), "Failed"), "Failed");
    assert_eq!(
        auth::ip_check(&json!({"srcIp":"1.2.3.4","primaryIp":"1.2.3.4"})),
        Some("match")
    );
    assert_eq!(
        auth::ip_check(&json!({"srcIp":"1.2.3.4"})),
        Some("unregistered")
    );
    assert_eq!(
        auth::ip_check(&json!({"srcIp":"1.2.3.4","secondaryIp":"5.6.7.8"})),
        Some("mismatch")
    );
    assert_eq!(auth::ip_check(&json!({})), None);
}

// ---------------------------------------------------------------------------
// Feed
// ---------------------------------------------------------------------------

fn sub(symbol: &str, ex: &str, brex: &str, token: &str, mode: FeedMode) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: ex.into(),
        token: token.into(),
        brsymbol: symbol.into(),
        brexchange: brex.into(),
        mode,
        depth: 5,
    }
}

fn frame_json(m: &Message) -> Value {
    match m {
        Message::Text(t) => {
            assert!(t.ends_with('\n'), "frame must end with a newline");
            serde_json::from_str(t.trim_end()).unwrap()
        }
        _ => panic!("text frame expected"),
    }
}

fn symbols_of(m: &Message) -> (String, String, Vec<String>) {
    let j = frame_json(m);
    let r = &j["request"];
    (
        r["streaming_type"].as_str().unwrap().into(),
        r["request_type"].as_str().unwrap().into(),
        r["data"]["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["symbol"].as_str().unwrap().to_string())
            .collect(),
    )
}

fn feed() -> (SamcoFeed, Arc<Mutex<ListingIds>>) {
    let ids = Arc::new(Mutex::new(ListingIds::default()));
    (
        SamcoFeed::new("wss://stream.samco.in", "abc%3D%3D", ids.clone()),
        ids,
    )
}

#[test]
fn feed_handshake_carries_decoded_session_token() {
    let (f, _) = feed();
    let req = f.ws_request().unwrap();
    assert_eq!(req.headers()["x-session-token"], "abc==");
    assert_eq!(unquote("a%2Fb%zz"), "a/b%zz");
}

#[test]
fn subscribe_restates_the_full_set_per_stream() {
    let (mut f, ids) = feed();
    let frames = f.subscribe_frames(&[sub("SBIN", "NSE", "NSE", "3045_NSE", FeedMode::Quote)]);
    assert_eq!(frames.len(), 1);
    assert_eq!(
        symbols_of(&frames[0]),
        ("quote".into(), "subscribe".into(), vec!["3045_NSE".into()])
    );
    assert_eq!(frame_json(&frames[0])["request"]["response_format"], "json");
    // Depth subscriber: quote2 (ladder) + quote with the whole set.
    let frames = f.subscribe_frames(&[sub(
        "CRUDEOIL20OCT26FUT",
        "MCX",
        "MFO",
        "464925",
        FeedMode::Depth,
    )]);
    assert_eq!(frames.len(), 2);
    assert_eq!(
        symbols_of(&frames[0]),
        (
            "quote2".into(),
            "subscribe".into(),
            vec!["464925_MFO".into()]
        )
    );
    assert_eq!(symbols_of(&frames[1]).2, vec!["3045_NSE", "464925_MFO"]);
    // Index without a known listing id is skipped; with one it is used bare.
    assert!(f
        .subscribe_frames(&[sub("NIFTY", "NSE_INDEX", "NSE", "NIFTY_50", FeedMode::Ltp)])
        .is_empty());
    ids.lock().insert("NSE_INDEX", "NIFTY", "-23");
    let frames = f.subscribe_frames(&[sub("NIFTY", "NSE_INDEX", "NSE", "NIFTY_50", FeedMode::Ltp)]);
    assert!(symbols_of(frames.last().unwrap())
        .2
        .contains(&"-23".to_string()));
}

#[test]
fn unsubscribe_cancels_both_streams_and_clears_state() {
    let (mut f, _) = feed();
    let d = sub("SBIN", "NSE", "NSE", "3045_NSE", FeedMode::Depth);
    let q = sub("RELIANCE", "NSE", "NSE", "2885_NSE", FeedMode::Quote);
    f.subscribe_frames(&[d.clone(), q.clone()]);
    f.parse(&Message::Text(
        json!({"sym":"3045_NSE","ltp":"812.4","streaming_type":"quote"}).to_string(),
    ));
    assert_eq!(f.state_len(), 1);
    let frames = f.unsubscribe_frames(std::slice::from_ref(&d));
    assert_eq!(
        symbols_of(&frames[0]),
        (
            "quote2".into(),
            "unsubscribe".into(),
            vec!["3045_NSE".into()]
        )
    );
    assert_eq!(
        symbols_of(&frames[1]),
        (
            "quote".into(),
            "unsubscribe".into(),
            vec!["3045_NSE".into()]
        )
    );
    assert_eq!(
        symbols_of(&frames[2]),
        ("quote".into(), "subscribe".into(), vec!["2885_NSE".into()])
    );
    assert_eq!(f.state_len(), 0);
    let frames = f.unsubscribe_frames(&[q]);
    assert_eq!(frames.len(), 1);
    assert!(f.unsubscribe_frames(&[d]).is_empty());
}

#[test]
fn quote_frames_decode_and_merge_with_depth() {
    let (mut f, _) = feed();
    f.subscribe_frames(&[sub("SBIN", "NSE", "NSE", "3045_NSE", FeedMode::Depth)]);
    // Flat `quote` frame (samcoWebSocket.py:522-541, keys :587-660).
    let ev = f.parse(&Message::Text(
        json!({
            "sym":"3045_NSE","ltp":"812.40","ltq":"25","o":"805","h":"815.95","l":"801.10",
            "c":"800.00","ch":"12.40","chPer":"1.55","vol":"1,234,567","oI":"0",
            "avgPr":"809.12","bPr":"812.35","bSz":"500","aPr":"812.45","aSz":"300",
            "tBQ":"120000","tSQ":"95000","lTrdT":"03 Oct 2026, 10:15:01 AM",
            "streaming_type":"quote"
        })
        .to_string(),
    ));
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!(
        (t.symbol.as_str(), t.exchange.as_str(), t.mode),
        ("SBIN", "NSE", 3)
    );
    assert_eq!(
        (t.ltp, t.open, t.close, t.volume),
        (812.4, 805.0, 800.0, 1_234_567)
    );
    assert_eq!(
        (t.change, t.change_percent, t.last_quantity),
        (12.4, 1.55, 25)
    );
    assert_eq!(
        (t.total_buy_quantity, t.total_sell_quantity),
        (120000, 95000)
    );
    // Wrapped quote2 frame merges the ladder over the cached quote fields.
    let ev = f.parse(&Message::Text(format!(
        "{}\n",
        json!({"response":{"streaming_type":"quote2","data":{
            "symbol":"3045_NSE",
            "bidValues":[{"price":"812.35","qty":"500","no":"4"},{"price":"812.30","qty":"100","no":"1"}],
            "askValues":[{"price":"812.45","qty":"300","no":"2"}],
            "tbq":"121000","taq":"96000"}}})
    )));
    assert_eq!(ev.len(), 2);
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!(t.ltp, 812.4, "ltp survives from the quote stream");
    assert_eq!(t.total_buy_quantity, 121000);
    let FeedEvent::Depth(d) = &ev[1] else {
        panic!()
    };
    assert_eq!((d.buy.len(), d.sell.len()), (5, 5));
    assert_eq!(
        (d.buy[0].price, d.buy[0].quantity, d.buy[0].orders),
        (812.35, 500, 4)
    );
    assert_eq!(d.sell[1].price, 0.0);
    // Unknown symbols and other messages are ignored.
    assert!(f
        .parse(&Message::Text(
            json!({"sym":"1_NSE","ltp":"1","streaming_type":"quote"}).to_string()
        ))
        .is_empty());
    assert!(f.parse(&Message::Text("connected".into())).is_empty());
}

#[test]
fn ltp_subscriber_gets_ticks_only_and_change_is_derived() {
    let (mut f, _) = feed();
    f.subscribe_frames(&[sub("SBIN", "NSE", "NSE", "3045_NSE", FeedMode::Ltp)]);
    let ev = f.parse(&Message::Text(
        json!({"sym":"3045_NSE","ltp":"810","c":"800","streaming_type":"quote"}).to_string(),
    ));
    assert_eq!(ev.len(), 1);
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!((t.mode, t.change, t.change_percent), (1, 10.0, 1.25));
}

#[test]
fn mode_change_drops_depth_stream() {
    let (mut f, _) = feed();
    let d = sub("SBIN", "NSE", "NSE", "3045_NSE", FeedMode::Depth);
    f.subscribe_frames(std::slice::from_ref(&d));
    let frames = f.mode_change_frames(&d, &d.with_mode(FeedMode::Quote));
    assert_eq!(
        symbols_of(&frames[0]),
        (
            "quote2".into(),
            "unsubscribe".into(),
            vec!["3045_NSE".into()]
        )
    );
    assert_eq!(symbols_of(&frames[1]).0, "quote");
    assert_eq!(frames.len(), 2);
}

#[test]
fn request_frame_shape() {
    let m = request_frame("quote", "subscribe", &["1_NSE".into()]);
    assert_eq!(
        frame_json(&m),
        json!({"request":{"streaming_type":"quote","data":{"symbols":[{"symbol":"1_NSE"}]},
               "request_type":"subscribe","response_format":"json"}})
    );
}

#[test]
fn listing_cache_is_bounded() {
    let mut c = ListingIds::default();
    for i in 0..(LISTING_CACHE_CAP + 10) {
        c.insert("NSE_INDEX", &format!("I{}", i), "-1");
    }
    assert!(c.len() <= LISTING_CACHE_CAP);
    assert_eq!(
        c.get("NSE_INDEX", &format!("I{}", LISTING_CACHE_CAP + 9))
            .as_deref(),
        Some("-1")
    );
}

#[test]
fn errors_are_trader_facing() {
    let e = samco_error(
        StatusCode::OK,
        &json!({"status":"Failure","statusMessage":"Insufficient funds"}),
        "x",
    );
    assert_eq!(e.client_message(), "Samco: Insufficient funds");
    let e = samco_error(StatusCode::FORBIDDEN, &json!({}), "x");
    assert!(e.client_message().contains("static IP"));
    let e = samco_error(
        StatusCode::OK,
        &json!({"statusMessage":"Session Expired"}),
        "x",
    );
    assert!(e.client_message().contains("session has expired"));
    assert_eq!(url_quote("NIFTY 50&x"), "NIFTY%2050%26x");
}

#[test]
fn identity_and_capabilities() {
    let b = SamcoBroker::new(master());
    assert_eq!(b.id(), "samco");
    assert_eq!(b.login_kind(), LoginKind::ApiKeySecret);
    assert!(!b.requires_totp());
    // BF-01: order updates come from the order-book poller.
    assert!(b.capabilities().margin && b.capabilities().order_feed);
    assert_eq!(b.timeframe_map().last(), Some(&("D", "DAY")));
    assert!(b.create_feed(&AuthToken::new("")).is_err());
    assert!(b.create_feed(&AuthToken::new("tok")).is_ok());
    // No session, no poller.
    assert!(matches!(
        b.create_order_feed(&AuthToken::new("")),
        Err(crate::error::AppError::Auth(_))
    ));
    assert!(!b.order_updates_running());
}
