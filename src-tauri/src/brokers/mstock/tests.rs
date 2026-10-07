//! mStock unit tests: every mapping against payloads built from the web code
//! and the Type B documentation (no account data), master-contract parsing,
//! and binary feed decoding at the web's byte offsets.

use super::data::{
    api_exchange, chunk_days, depth_exchange_type, depth_from_packet, intraday_exchange,
    parse_candle_time, parse_candles, quote_from_row,
};
use super::mapping::*;
use super::master_contract::{build, convert_date, parse_annexure};
use super::streaming::{
    exchange_type, feed_url, parse_frame, parse_packet, MstockFeed, LTP_LEN, QUOTE_LEN, SNAP_LEN,
};
use super::*;
use crate::brokers::common::mapping::{Action, PriceType, Product, Validity};
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription, Message};
use serde_json::json;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../../tests/fixtures/brokers/mstock/", $name))
    };
}

fn fx(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

fn master() -> Vec<SymToken> {
    let rows: Vec<Value> = serde_json::from_str(fixture!("scrip_master.json")).unwrap();
    build(&rows, Some(fixture!("annexure.html")), 2026)
}

fn resolver() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(master());
    r
}

fn row<'a>(rows: &'a [SymToken], exchange: &str, token: &str) -> &'a SymToken {
    rows.iter()
        .find(|r| r.exchange == exchange && r.token == token)
        .unwrap_or_else(|| panic!("no row {} {}", exchange, token))
}

// ---------------------------------------------------------------------------
// Enum maps
// ---------------------------------------------------------------------------

#[test]
fn order_constants_match_transform_data() {
    assert_eq!(map_order_type(PriceType::Market), "MARKET");
    assert_eq!(map_order_type(PriceType::Limit), "LIMIT");
    assert_eq!(map_order_type(PriceType::Sl), "STOPLOSS_LIMIT");
    assert_eq!(map_order_type(PriceType::SlM), "STOPLOSS_MARKET");
    assert_eq!(map_variety(PriceType::Market), "NORMAL");
    assert_eq!(map_variety(PriceType::Limit), "NORMAL");
    assert_eq!(map_variety(PriceType::Sl), "STOPLOSS");
    assert_eq!(map_variety(PriceType::SlM), "STOPLOSS");
    assert_eq!(map_product_type(Product::Cnc), "DELIVERY");
    assert_eq!(map_product_type(Product::Nrml), "CARRYFORWARD");
    assert_eq!(map_product_type(Product::Mis), "INTRADAY");
    assert_eq!(reverse_map_product_type("DELIVERY"), Some("CNC"));
    assert_eq!(reverse_map_product_type("CARRYFORWARD"), Some("NRML"));
    assert_eq!(reverse_map_product_type("INTRADAY"), Some("MIS"));
    assert_eq!(reverse_map_product_type("MARGIN"), Some("MIS"));
    assert_eq!(reverse_map_product_type("BO"), None);
    for (raw, oa) in [
        ("STOPLOSS_LIMIT", "SL"),
        ("STOP_LOSS", "SL"),
        ("SL", "SL"),
        ("STOPLOSS_MARKET", "SL-M"),
        ("STOP_LOSS_MARKET", "SL-M"),
        ("SL-M", "SL-M"),
        ("LIMIT", "LIMIT"),
        ("MARKET", "MARKET"),
    ] {
        assert_eq!(order_type_to_openalgo(raw), oa, "{}", raw);
    }
}

#[test]
fn order_status_table_matches_order_data() {
    for (raw, oa) in [
        ("Traded", "complete"),
        ("TRADE CONFIRMED", "complete"),
        ("O-Pending", "open"),
        ("Pending", "open"),
        ("pending", "open"),
        ("O-Modified", "open"),
        ("o-modified", "open"),
        ("Rejected", "rejected"),
        ("rejected", "rejected"),
        ("Cancelled", "cancelled"),
        ("O-Cancelled", "cancelled"),
        ("o-cancelled", "cancelled"),
        ("Trigger Pending", "trigger pending"),
        ("AMO Received", "amo received"),
        ("", ""),
    ] {
        assert_eq!(order_status(raw), oa, "{}", raw);
    }
    for raw in ["open", "Pending", "O-Pending", "Trigger Pending"] {
        assert!(is_cancellable(raw), "{}", raw);
    }
    for raw in ["Traded", "Rejected", "O-Modified"] {
        assert!(!is_cancellable(raw), "{}", raw);
    }
}

#[test]
fn derivative_and_currency_exchanges() {
    assert_eq!(oa_exchange("NSE", "OPTIDX"), "NFO");
    assert_eq!(oa_exchange("NSE", "FUTSTK"), "NFO");
    assert_eq!(oa_exchange("BSE", "OPTIDX"), "BFO");
    assert_eq!(oa_exchange("NSE", "OPTCUR"), "CDS");
    assert_eq!(oa_exchange("BSE", "FUTCUR"), "BCD");
    assert_eq!(oa_exchange("NSE", ""), "NSE");
    assert_eq!(oa_exchange("NFO", "OPTIDX"), "NFO");
    assert_eq!(oa_exchange("MCX", "FUTCOM"), "MCX");
}

// ---------------------------------------------------------------------------
// Payloads and envelopes
// ---------------------------------------------------------------------------

fn sbin() -> SymToken {
    let rows = master();
    row(&rows, "NSE", "3045").clone()
}

#[test]
fn place_body_is_the_web_dict() {
    let o = ResolvedOrder {
        symbol: "SBIN".into(),
        exchange: Exchange::Nse,
        action: Action::Buy,
        quantity: 10,
        price: 750.5,
        trigger_price: 0.0,
        pricetype: PriceType::Limit,
        product: Product::Cnc,
        validity: Validity::Day,
        disclosed_quantity: 0,
        amo: false,
        instrument: sbin(),
    };
    assert_eq!(
        place_order_body(&o),
        json!({
            "variety": "NORMAL",
            "tradingsymbol": "SBIN-EQ",
            "symboltoken": "3045",
            "exchange": "NSE",
            "transactiontype": "BUY",
            "ordertype": "LIMIT",
            "quantity": "10",
            "producttype": "DELIVERY",
            "price": "750.5",
            "triggerprice": "0",
            "squareoff": "0",
            "stoploss": "0",
            "trailingStopLoss": "",
            "disclosedquantity": "0",
            "duration": "DAY",
            "ordertag": "",
        })
    );
}

#[test]
fn modify_and_cancel_bodies() {
    let m = ResolvedModify {
        order_id: "1181251003103".into(),
        symbol: "SBIN".into(),
        exchange: Exchange::Nse,
        action: Action::Sell,
        product: Product::Mis,
        pricetype: PriceType::SlM,
        quantity: 5,
        price: 0.0,
        trigger_price: 100.5,
        disclosed_quantity: 1,
        instrument: sbin(),
    };
    assert_eq!(
        modify_order_body(&m),
        json!({
            "variety": "STOPLOSS",
            "tradingsymbol": "SBIN-EQ",
            "symboltoken": "3045",
            "exchange": "NSE",
            "transactiontype": "SELL",
            "orderid": "1181251003103",
            "ordertype": "STOPLOSS_MARKET",
            "quantity": "5",
            "producttype": "INTRADAY",
            "duration": "DAY",
            "price": "0",
            "triggerprice": "100.5",
            "disclosedquantity": "1",
            "modqty_remng": "0",
        })
    );
    assert_eq!(
        cancel_order_body("42"),
        json!({"variety": "NORMAL", "orderid": "42"})
    );
}

#[test]
fn envelopes_status_and_order_id() {
    for v in [
        json!({"status": true}),
        json!({"status": "true"}),
        json!({"status": "TRUE"}),
        json!({"status": "True"}),
    ] {
        assert!(is_success(&v), "{}", v);
    }
    for v in [
        json!({"status": false}),
        json!({"status": "false"}),
        json!({}),
        json!({"status": 1}),
    ] {
        assert!(!is_success(&v), "{}", v);
    }
    assert_eq!(
        unwrap_list(json!([{"status": true, "data": {"orderid": "9"}}])),
        json!({"status": true, "data": {"orderid": "9"}})
    );
    assert_eq!(unwrap_list(json!([])), json!([]));
    assert_eq!(
        extract_order_id(&json!({"status": "true", "data": {"orderid": "9"}})).as_deref(),
        Some("9")
    );
    assert_eq!(
        extract_order_id(&json!({"status": true, "data": {"uniqueorderid": "u-1"}})).as_deref(),
        Some("u-1")
    );
    assert_eq!(
        extract_order_id(&json!({"status": true, "data": {"orderid": 123}})).as_deref(),
        Some("123")
    );
    assert!(extract_order_id(&json!({"status": false, "data": {"orderid": "9"}})).is_none());
    assert!(extract_order_id(&json!({"status": true, "data": null})).is_none());
    assert_eq!(num_text(0.0), "0");
    assert_eq!(num_text(101.0), "101");
    assert_eq!(num_text(101.55), "101.55");
}

#[test]
fn refusals_are_trader_messages() {
    let e = refusal(
        &json!({"status": false, "message": "RMS:Margin Exceeds"}),
        "x",
    );
    assert_eq!(e.client_message(), "mStock: RMS:Margin Exceeds");
    let e = refusal(&json!({"status": false, "message": "Invalid Token"}), "x");
    assert!(matches!(e, AppError::Auth(_)));
    let e = refusal(&json!({}), "mStock did not accept the order.");
    assert_eq!(e.client_message(), "mStock did not accept the order.");
}

#[test]
fn session_token_round_trip() {
    let s = MstockSession::parse(&AuthToken::new("jwt.abc:::pkey")).unwrap();
    assert_eq!(
        (s.jwt.as_str(), s.private_key.as_str()),
        ("jwt.abc", "pkey")
    );
    assert_eq!(s.compose(), "jwt.abc:::pkey");
    assert!(!format!("{:?}", s).contains("pkey"));
    assert!(MstockSession::parse(&AuthToken::new("bare")).is_err());
    assert!(MstockSession::parse(&AuthToken::new(":::k")).is_err());
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

#[test]
fn order_book_in_openalgo_terms() {
    let book = map_orders(&fx(fixture!("order_book.json")), &resolver());
    assert_eq!(book.len(), 5);
    let o = &book[0];
    assert_eq!((o.symbol.as_str(), o.exchange.as_str()), ("SBIN", "NSE"));
    assert_eq!(o.status, "open");
    assert_eq!(o.product, "CNC");
    assert_eq!(o.order_type, "LIMIT");
    assert_eq!(o.price, 750.5);
    assert_eq!(
        (o.quantity, o.filled_quantity, o.pending_quantity),
        (10, 0, 10)
    );
    assert_eq!(o.order_timestamp, "03-Oct-2026 09:20:11");
    // Option reported on NSE: NFO, OpenAlgo symbol, average shown.
    let o = &book[1];
    assert_eq!(
        (o.symbol.as_str(), o.exchange.as_str()),
        ("NIFTY27OCT2624500CE", "NFO")
    );
    assert_eq!(o.status, "complete");
    assert_eq!(o.product, "NRML");
    assert_eq!(o.side, "SELL");
    assert_eq!(o.price, 112.35);
    let o = &book[2];
    assert_eq!(o.status, "trigger pending");
    assert_eq!(o.order_type, "SL");
    assert_eq!(o.price, 101.0);
    assert_eq!(o.trigger_price, 100.5);
    assert_eq!(o.product, "MIS");
    assert_eq!(book[3].status, "rejected");
    assert_eq!(
        book[3].rejection_reason.as_deref(),
        Some("RMS:Margin Exceeds")
    );
    assert_eq!(book[4].status, "cancelled");
    assert!(map_orders(&json!({"status": "true", "data": null}), &resolver()).is_empty());
}

#[test]
fn trade_book_uppercase_keys() {
    let t = map_trades(&fx(fixture!("trade_book.json")), &resolver());
    assert_eq!(t.len(), 2);
    assert_eq!(
        (t[0].symbol.as_str(), t[0].exchange.as_str()),
        ("NIFTY27OCT2624500CE", "NFO")
    );
    assert_eq!(t[0].product, "NRML");
    assert_eq!(t[0].side, "SELL");
    assert_eq!(t[0].quantity, 75);
    assert_eq!(t[0].average_price, 112.35);
    assert_eq!(t[0].trade_value, 8426.25);
    assert_eq!(t[0].trade_id, "5001");
    assert_eq!(t[0].order_id, "1181251003102");
    // No token: equity name gains -EQ and resolves.
    assert_eq!(
        (t[1].symbol.as_str(), t[1].product.as_str()),
        ("INFY", "CNC")
    );
    assert_eq!(t[1].side, "BUY");
    assert_eq!(t[1].timestamp, "03-Oct-2026 10:00:00");
}

#[test]
fn positions_by_token_with_netvalue_pnl() {
    let p = map_positions(&fx(fixture!("positions.json")), &resolver());
    assert_eq!(p.len(), 3);
    assert_eq!(
        (p[0].symbol.as_str(), p[0].exchange.as_str()),
        ("SBIN", "NSE")
    );
    assert_eq!(p[0].product, "CNC");
    assert_eq!(p[0].quantity, 10);
    assert_eq!(p[0].average_price, 750.5);
    assert_eq!(p[0].pnl, -7505.0);
    assert_eq!(p[0].ltp, 0.0);
    assert_eq!(
        (p[1].symbol.as_str(), p[1].exchange.as_str(), p[1].quantity),
        ("NIFTY27OCT2624500CE", "NFO", -75)
    );
    assert_eq!(p[1].product, "NRML");
    assert_eq!(p[2].product, "MIS");
}

#[test]
fn holdings_by_broker_symbol() {
    let h = map_holdings(&fx(fixture!("holdings.json")), &resolver());
    assert_eq!(h.len(), 2);
    assert_eq!(
        (h[0].symbol.as_str(), h[0].exchange.as_str()),
        ("SBIN", "NSE")
    );
    assert_eq!(h[0].product, "CNC");
    assert_eq!(h[0].quantity, 10);
    assert_eq!(h[0].pnl, 505.0);
    assert_eq!(h[0].pnl_percentage, 7.21);
    assert_eq!(h[0].isin.as_deref(), Some("INE062A01020"));
    // exchange null -> NSE, product null -> CNC, strings parsed.
    assert_eq!(
        (h[1].symbol.as_str(), h[1].exchange.as_str()),
        ("INFY", "NSE")
    );
    assert_eq!(h[1].quantity, 2);
    assert_eq!(h[1].pnl, -20.0);
    assert_eq!(h[1].pnl_percentage, -0.67);
}

#[test]
fn funds_and_margin() {
    let v = fx(fixture!("fund_summary.json"));
    let f = funds_from_summary(&rows(&v)[0]);
    assert_eq!(f.available_cash, 100000.46);
    assert_eq!(f.collateral, 5000.0);
    assert_eq!(f.m2m_realized, 0.0);
    assert_eq!(f.m2m_unrealized, -120.25);
    assert_eq!(f.utilised_debits, 2500.5);
    let m = parse_margin(&fx(fixture!("margin.json"))).unwrap();
    assert_eq!(m.total_margin_required, 152340.75);
    assert_eq!(m.span_margin, 120000.5);
    assert_eq!(m.exposure_margin, 32340.25);
    assert!(parse_margin(&json!({"status": false, "message": "x"})).is_none());
    assert!(valid_margin_token("45001"));
    assert!(!valid_margin_token("None"));
    assert!(!valid_margin_token(""));
    assert!(!valid_margin_token("NIFTY_50"));
    let leg = MarginLeg {
        key: QuoteKey::new("NFO", "NIFTY27OCT2624500CE"),
        action: Action::Sell,
        quantity: 75,
        product: Product::Nrml,
        pricetype: PriceType::Market,
        price: 0.0,
        trigger_price: 0.0,
    };
    assert_eq!(
        margin_leg(&leg, "NIFTY26OCT24500CE", "45001"),
        json!({
            "product_type": "CARRYFORWARD",
            "transaction_type": "SELL",
            "quantity": "75",
            "price": "0",
            "exchange": "NFO",
            "symbol_name": "NIFTY26OCT24500CE",
            "token": "45001",
            "trigger_price": 0.0,
        })
    );
}

// ---------------------------------------------------------------------------
// Market data
// ---------------------------------------------------------------------------

#[test]
fn quote_rows_and_exchange_maps() {
    let v = fx(fixture!("quote.json"));
    let rows = v["data"]["fetched"].as_array().unwrap();
    let q = quote_from_row(&rows[0], &QuoteKey::new("NSE", "SBIN"));
    assert_eq!(
        (q.ltp, q.open, q.high, q.low, q.close),
        (752.4, 748.0, 755.0, 746.1, 748.0)
    );
    assert_eq!((q.bid, q.ask, q.oi, q.volume), (0.0, 0.0, 0, 0));
    assert_eq!(q.change, 4.4);
    let q = quote_from_row(&rows[1], &QuoteKey::new("NSE", "INFY"));
    assert_eq!((q.ltp, q.volume), (1490.5, 120000));
    assert_eq!(api_exchange("NSE_INDEX"), Some("NSE"));
    assert_eq!(api_exchange("BSE_INDEX"), Some("BSE"));
    assert_eq!(api_exchange("MCX_INDEX"), Some("MCX"));
    assert_eq!(api_exchange("BCD"), None);
    assert_eq!(intraday_exchange("NSE"), Some("1"));
    assert_eq!(intraday_exchange("NFO"), Some("2"));
    assert_eq!(intraday_exchange("CDS"), Some("3"));
    assert_eq!(intraday_exchange("BSE_INDEX"), Some("4"));
    assert_eq!(intraday_exchange("BFO"), Some("5"));
    assert_eq!(intraday_exchange("MCX"), Some("6"));
    assert_eq!(depth_exchange_type("CDS"), Some(13));
    assert_eq!(depth_exchange_type("MCX_INDEX"), None);
}

#[test]
fn history_chunks_and_timestamps() {
    assert_eq!(chunk_days("1m"), Some(2));
    assert_eq!(chunk_days("3m"), Some(8));
    assert_eq!(chunk_days("5m"), Some(13));
    assert_eq!(chunk_days("10m"), Some(26));
    assert_eq!(chunk_days("15m"), Some(40));
    assert_eq!(chunk_days("30m"), Some(76));
    assert_eq!(chunk_days("1h"), Some(166));
    assert_eq!(chunk_days("D"), Some(1000));
    assert_eq!(chunk_days("W"), None);
    // 2026-09-29 09:15 IST = 03:45 UTC.
    let want = 1_790_653_500;
    assert_eq!(parse_candle_time("2026-09-29T09:15:00+05:30"), Some(want));
    assert_eq!(parse_candle_time("2026-09-29T09:15:00+0530"), Some(want));
    assert_eq!(parse_candle_time("2026-09-29 09:15"), Some(want));
    assert_eq!(parse_candle_time("2026-09-29T09:15:00"), Some(want));
    // pandas reads `+05` as +05:00.
    assert_eq!(
        parse_candle_time("2026-09-29T09:15:00+05"),
        Some(want + 1800)
    );
    assert_eq!(parse_candle_time("garbage"), None);
    let c = parse_candles(&fx(fixture!("historical.json")), false);
    assert_eq!(c.len(), 2);
    assert_eq!(c[0].timestamp, want);
    assert_eq!(
        (c[0].open, c[0].close, c[0].volume, c[0].oi),
        (750.0, 751.0, 12000, 0)
    );
    let c = parse_candles(&fx(fixture!("intraday.json")), false);
    assert_eq!(c[1].close, 761.5);
    assert_eq!(c[1].volume, 4000);
    // Daily: midnight of the UTC date.
    let d = parse_candles(
        &json!({"data": {"candles": [["2026-09-29T09:15:00+05:30", 1, 2, 0.5, 1.5, 10]]}}),
        true,
    );
    assert_eq!(d[0].timestamp % 86_400, 0);
    assert_eq!(d[0].timestamp, 1_790_640_000);
}

// ---------------------------------------------------------------------------
// Master contract
// ---------------------------------------------------------------------------

#[test]
fn expiry_formats_like_convert_date() {
    for (raw, want) in [
        ("27OCT2026", "27-OCT-26"),
        ("27-OCT-2026", "27-OCT-26"),
        ("2026-10-27", "27-OCT-26"),
        ("27-Oct-26", "27-OCT-26"),
        ("27OCT26", "27-OCT-26"),
        ("27-OCT", "27-OCT-26"),
        ("27OCT", "27OCT"),
        ("", ""),
    ] {
        assert_eq!(convert_date(raw, 2026), want, "{}", raw);
    }
    assert_eq!(convert_date("99XYZ99", 2026), "99-XYZ-99");
}

#[test]
fn annexure_rows() {
    let rows = parse_annexure(fixture!("annexure.html"));
    let names: Vec<(&str, &str, &str)> = rows
        .iter()
        .map(|(s, t, _, e)| (s.as_str(), t.as_str(), e.as_str()))
        .collect();
    assert_eq!(
        names,
        [
            ("NIFTY50", "26000", "NSE"),
            ("NIFTYBANK", "26009", "NSE"),
            ("INDIAVIX", "26017", "NSE"),
            ("DUPTOKEN", "3045", "NSE"),
            ("SENSEX", "99919000", "BSE"),
            ("MIDSEL", "99919017", "BSE"),
        ]
    );
    assert_eq!(rows[0].2, "Nifty 50");
}

#[test]
fn master_rows_match_the_web_pipeline() {
    let rows = master();
    // Empty token dropped; NSE index with an existing token skipped.
    assert!(rows.iter().all(|r| !r.token.is_empty()));
    assert!(!rows.iter().any(|r| r.symbol == "DUPTOKEN"));
    let r = row(&rows, "NSE", "3045");
    assert_eq!(
        (r.symbol.as_str(), r.brsymbol.as_str(), r.name.as_str()),
        ("SBIN", "SBIN-EQ", "SBIN")
    );
    assert_eq!((r.lot_size, r.tick_size, r.strike), (1, 5.0, -1.0));
    let r = row(&rows, "NFO", "45001");
    assert_eq!(r.symbol, "NIFTY27OCT2624500CE");
    assert_eq!(r.expiry, "27-OCT-26");
    assert_eq!(
        (r.instrument_type.as_str(), r.lot_size, r.strike),
        ("CE", 75, 24500.0)
    );
    assert_eq!(r.brexchange, "NFO");
    let r = row(&rows, "NFO", "45002");
    assert_eq!(
        (r.symbol.as_str(), r.instrument_type.as_str()),
        ("NIFTY27OCT26FUT", "FUT")
    );
    let r = row(&rows, "NFO", "45003");
    assert_eq!(r.symbol, "RELIANCE27OCT261332.5PE");
    let r = row(&rows, "BFO", "845001");
    assert_eq!(r.symbol, "SENSEX08OCT2682700CE");
    let r = row(&rows, "CDS", "1201");
    assert_eq!(
        (r.symbol.as_str(), r.brexchange.as_str()),
        ("USDINR28OCT2683.25CE", "NSE")
    );
    let r = row(&rows, "BCD", "1202");
    assert_eq!(
        (r.symbol.as_str(), r.instrument_type.as_str()),
        ("USDINR28OCT26FUT", "FUT")
    );
    let r = row(&rows, "BSE_INDEX", "99919000");
    assert_eq!(
        (r.symbol.as_str(), r.brexchange.as_str()),
        ("SENSEX", "BSE")
    );
    let r = row(&rows, "BSE_INDEX", "99919017");
    assert_eq!(r.symbol, "BSEMIDCAPSELECTINDEX");
    assert_eq!(row(&rows, "BSE", "500325").symbol, "RELIANCE");
    let r = row(&rows, "NSE_INDEX", "26000");
    assert_eq!(
        (r.symbol.as_str(), r.brsymbol.as_str(), r.name.as_str()),
        ("NIFTY", "Nifty 50", "NIFTY50")
    );
    assert_eq!(
        (r.instrument_type.as_str(), r.lot_size, r.tick_size),
        ("INDEX", 1, 0.05)
    );
    assert_eq!(row(&rows, "NSE_INDEX", "26009").symbol, "BANKNIFTY");
    assert_eq!(row(&rows, "NSE_INDEX", "26017").symbol, "INDIAVIX");
    // -EQ wins the shared symbol, -BE keeps its own.
    let res = resolver();
    assert_eq!(res.by_symbol("NSE", "NHPC").unwrap().brsymbol, "NHPC-EQ");
    assert_eq!(res.by_symbol("NSE", "NHPC-BE").unwrap().token, "10001");
}

#[test]
fn master_without_annexure_keeps_bse_rows() {
    let rows: Vec<Value> = serde_json::from_str(fixture!("scrip_master.json")).unwrap();
    let out = build(&rows, None, 2026);
    assert!(out
        .iter()
        .all(|r| r.exchange != "NSE_INDEX" && r.exchange != "BSE_INDEX"));
    assert_eq!(row(&out, "BSE", "99919000").symbol, "SENSEX");
}

// ---------------------------------------------------------------------------
// Binary feed (offsets: mstockwebsocket.py:154-298)
// ---------------------------------------------------------------------------

pub fn packet(mode: u8, et: u8, token: &str, len: usize) -> Vec<u8> {
    let mut p = vec![0u8; len];
    p[0] = mode;
    p[1] = et;
    p[2..2 + token.len()].copy_from_slice(token.as_bytes());
    let put = |p: &mut Vec<u8>, o: usize, v: u64| p[o..o + 8].copy_from_slice(&v.to_le_bytes());
    put(&mut p, 27, 77); // sequence
    put(&mut p, 35, 1_790_653_500_000); // exchange timestamp
    put(&mut p, 43, 75_240); // ltp paise
    if len >= QUOTE_LEN {
        put(&mut p, 51, 25); // ltq
        put(&mut p, 59, 75_011); // avg
        put(&mut p, 67, 1_200_000); // volume
        p[75..83].copy_from_slice(&5000.0f64.to_le_bytes());
        p[83..91].copy_from_slice(&7000.0f64.to_le_bytes());
        put(&mut p, 91, 74_800);
        put(&mut p, 99, 75_500);
        put(&mut p, 107, 74_610);
        put(&mut p, 115, 74_800);
    }
    if len >= SNAP_LEN {
        put(&mut p, 123, 1_790_653_499_000);
        put(&mut p, 131, 123_456);
        put(&mut p, 139, 250);
        for i in 0..10u64 {
            let o = 147 + (i as usize) * 20;
            put(&mut p, o + 2, 100 + i); // qty
            put(&mut p, o + 10, 75_000 + i * 5); // price
            p[o + 18..o + 20].copy_from_slice(&(i as u16 + 1).to_le_bytes());
        }
        put(&mut p, 347, 82_000);
        put(&mut p, 355, 67_000);
        put(&mut p, 363, 91_000);
        put(&mut p, 371, 55_000);
    }
    p
}

#[test]
fn ltp_quote_and_snap_packets() {
    let l = parse_packet(&packet(1, 1, "3045", LTP_LEN)).unwrap();
    assert_eq!((l.mode, l.exchange_type, l.token.as_str()), (1, 1, "3045"));
    assert_eq!((l.sequence, l.ltp, l.volume), (77, 752.4, 0));
    let q = parse_packet(&packet(2, 2, "45001", QUOTE_LEN)).unwrap();
    assert_eq!(
        (q.last_traded_qty, q.avg_price, q.volume),
        (25, 750.11, 1_200_000)
    );
    assert_eq!((q.total_buy_qty, q.total_sell_qty), (5000.0, 7000.0));
    assert_eq!(
        (q.open, q.high, q.low, q.close),
        (748.0, 755.0, 746.1, 748.0)
    );
    assert!(q.bids.is_empty());
    let s = parse_packet(&packet(3, 1, "3045", SNAP_LEN)).unwrap();
    assert_eq!((s.oi, s.oi_percent), (123_456, 2.5));
    assert_eq!((s.upper_circuit, s.lower_circuit), (820.0, 670.0));
    assert_eq!((s.week_52_high, s.week_52_low), (910.0, 550.0));
    assert_eq!(s.bids.len(), 5);
    assert_eq!(s.asks.len(), 5);
    assert_eq!(
        (s.bids[0].price, s.bids[0].quantity, s.bids[0].orders),
        (750.0, 100, 1)
    );
    assert_eq!(
        (s.asks[0].price, s.asks[0].quantity, s.asks[0].orders),
        (750.25, 105, 6)
    );
    assert!(parse_packet(&[0u8; 60]).is_none());
    // A header-prefixed snap read directly.
    let mut framed = vec![1, 0, 123, 1];
    framed.extend(packet(3, 1, "3045", SNAP_LEN));
    assert_eq!(parse_packet(&framed).unwrap().token, "3045");
}

#[test]
fn header_frames_batch_and_recover() {
    // Two LTP packets behind a header (4 + 2 * 51 bytes).
    let mut f = Vec::new();
    f.extend(2u16.to_le_bytes());
    f.extend((LTP_LEN as u16).to_le_bytes());
    f.extend(packet(1, 1, "3045", LTP_LEN));
    f.extend(packet(1, 2, "45001", LTP_LEN));
    let p = parse_frame(&f);
    assert_eq!(p.len(), 2);
    assert_eq!((p[1].exchange_type, p[1].token.as_str()), (2, "45001"));
    // Header count larger than what fits is clamped.
    let mut f2 = Vec::new();
    f2.extend(9u16.to_le_bytes());
    f2.extend((QUOTE_LEN as u16).to_le_bytes());
    f2.extend(packet(2, 1, "3045", QUOTE_LEN));
    assert_eq!(parse_frame(&f2).len(), 1);
    // Impossible size: one packet filling the frame.
    let mut f3 = Vec::new();
    f3.extend(1u16.to_le_bytes());
    f3.extend(9999u16.to_le_bytes());
    f3.extend(packet(3, 1, "3045", SNAP_LEN));
    assert_eq!(parse_frame(&f3)[0].oi, 123_456);
    // Bare packets.
    assert_eq!(
        parse_frame(&packet(2, 1, "1594", QUOTE_LEN))[0].token,
        "1594"
    );
    assert!(parse_frame(&[1, 2]).is_empty());
    assert!(parse_frame(&[1, 0, 0, 0]).is_empty());
}

fn sub(symbol: &str, exchange: &str, token: &str, mode: FeedMode) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: exchange.into(),
        token: token.into(),
        brsymbol: String::new(),
        brexchange: "NSE".into(),
        mode,
        depth: 5,
    }
}

#[test]
fn feed_frames_and_events() {
    let mut feed = MstockFeed::new("wss://ws.mstock.trade", "jwt.x", "pk/1");
    let req = feed.ws_request().unwrap();
    assert_eq!(
        req.uri().to_string(),
        "wss://ws.mstock.trade/?API_KEY=pk%2F1&ACCESS_TOKEN=jwt.x"
    );
    assert_eq!(
        feed_url("wss://h", "k", "t"),
        "wss://h/?API_KEY=k&ACCESS_TOKEN=t"
    );
    assert_eq!(
        feed.on_connected(),
        vec![Message::Text("LOGIN:jwt.x".into())]
    );
    let frames = feed.subscribe_frames(&[
        sub("SBIN", "NSE", "3045", FeedMode::Quote),
        sub("NIFTY27OCT2624500CE", "NFO", "45001", FeedMode::Quote),
        sub("USDINR28OCT26FUT", "CDS", "1202", FeedMode::Depth),
    ]);
    assert_eq!(frames.len(), 2);
    let Message::Text(t) = &frames[0] else {
        panic!()
    };
    assert_eq!(
        serde_json::from_str::<Value>(t).unwrap(),
        json!({"action": 1, "params": {"mode": 2, "tokenList": [
            {"exchangeType": 1, "tokens": ["3045"]},
            {"exchangeType": 2, "tokens": ["45001"]}
        ]}})
    );
    let Message::Text(t) = &frames[1] else {
        panic!()
    };
    assert!(t.contains("\"exchangeType\":13"));
    assert_eq!(feed.registered(), 3);

    let ev = feed.parse(&Message::Binary(packet(2, 1, "3045", QUOTE_LEN)));
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!(
        (t.symbol.as_str(), t.exchange.as_str(), t.mode),
        ("SBIN", "NSE", 2)
    );
    assert_eq!((t.ltp, t.close, t.change), (752.4, 748.0, 4.4));
    assert_eq!(t.volume, 1_200_000);
    assert_eq!(ev.len(), 1);

    let ev = feed.parse(&Message::Binary(packet(3, 13, "1202", SNAP_LEN)));
    assert_eq!(ev.len(), 2);
    let FeedEvent::Depth(d) = &ev[1] else {
        panic!()
    };
    assert_eq!(d.symbol, "USDINR28OCT26FUT");
    assert_eq!((d.buy.len(), d.sell.len()), (5, 5));
    assert_eq!(d.total_sell_quantity, 7000);
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!(t.oi, 123_456);

    // Unknown token / text frames: nothing.
    assert!(feed
        .parse(&Message::Binary(packet(1, 1, "999", LTP_LEN)))
        .is_empty());
    assert!(feed.parse(&Message::Text("hello".into())).is_empty());

    let un = feed.unsubscribe_frames(&[sub("SBIN", "NSE", "3045", FeedMode::Quote)]);
    let Message::Text(t) = &un[0] else { panic!() };
    assert!(t.starts_with("{\"action\":0"));
    assert_eq!(feed.registered(), 2);
    assert!(feed
        .parse(&Message::Binary(packet(2, 1, "3045", QUOTE_LEN)))
        .is_empty());
    assert!(matches!(feed.heartbeat(), Some((d, Message::Ping(_))) if d.as_secs() == 20));
    assert_eq!(exchange_type("BSE_INDEX"), 3);
    assert_eq!(exchange_type("MCX"), 5);
    assert_eq!(exchange_type("XYZ"), 1);
}

#[test]
fn depth_from_snap_packet() {
    let p = parse_packet(&packet(3, 1, "3045", SNAP_LEN)).unwrap();
    let d = depth_from_packet(&p, &QuoteKey::new("NSE", "SBIN"));
    assert_eq!(d.bids.len(), 5);
    assert_eq!(d.asks[4].price, 750.45);
    assert_eq!(
        (d.ltp, d.ltq, d.prev_close, d.oi),
        (752.4, 25, 748.0, 123_456)
    );
    assert_eq!((d.total_buy_qty, d.total_sell_qty), (5000, 7000));
    let l = parse_packet(&packet(1, 1, "3045", LTP_LEN)).unwrap();
    let d = depth_from_packet(&l, &QuoteKey::new("NSE", "SBIN"));
    assert_eq!(d.bids, vec![DepthLevel::default(); 5]);
}

#[test]
fn broker_identity() {
    let b = MstockBroker::new(SymbolResolver::new());
    assert_eq!(b.id(), "mstock");
    assert_eq!(
        b.login_kind(),
        LoginKind::TwoStep {
            step1: &["password"],
            step2: &["totp"]
        }
    );
    assert!(b.requires_totp());
    let caps = b.capabilities();
    assert!(caps.history && caps.margin && caps.streaming && !caps.order_feed && !caps.gtt);
    assert_eq!(b.timeframe_map().len(), 8);
    assert!(b.create_feed(&AuthToken::new("jwt:::pk")).is_ok());
    assert!(b.create_feed(&AuthToken::new("jwt")).is_err());
}
