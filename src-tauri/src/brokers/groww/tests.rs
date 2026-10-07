//! Groww mapping, protocol and adapter tests against payloads built from
//! the web code and Groww's documented shapes
//! (`src-tauri/tests/fixtures/brokers/groww/`). No real account data.

use super::auth::{checksum, choose_variant, looks_like_jwt, Variant};
use super::data::*;
use super::funds::{funds_from_payload, margin_payload, parse_margin};
use super::mapping::*;
use super::master_contract::parse_instruments;
use super::nkeys::{self, KeyPair};
use super::order_poller::{clamp_interval, diff};
use super::orders::{modify_order_body, place_order_body, segment_from_id};
use super::proto;
use super::streaming::*;
use super::*;
use crate::brokers::common::mapping::{Action, PriceType};
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription, Message};
use crate::brokers::upstox::relay::{Session, READY};
use prost::Message as _;
use serde_json::{json, Value};

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../../tests/fixtures/brokers/groww/", $name))
    };
}

fn fx(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(parse_instruments(fixture!("instrument.csv")).unwrap());
    r
}

fn core() -> GrowwCore {
    GrowwCore::new(master(), "http://127.0.0.1:9")
}

fn orders_of(v: &Value) -> Vec<GrowwOrder> {
    serde_json::from_value(v["payload"]["order_list"].clone()).unwrap()
}

// ---------------------------------------------------------------------------
// Master contract
// ---------------------------------------------------------------------------

#[test]
fn master_contract_rows_and_exchanges() {
    let rows = parse_instruments(fixture!("instrument.csv")).unwrap();
    // 16 data rows; the one with a blank trading symbol is dropped.
    assert_eq!(rows.len(), 15);
    let r = master();
    let sbin = r.by_symbol("NSE", "SBIN").unwrap();
    assert_eq!(
        (
            sbin.brexchange.as_str(),
            sbin.token.as_str(),
            sbin.instrument_type.as_str()
        ),
        ("NSE", "3045", "EQ")
    );
    assert_eq!(sbin.name, "STATE BANK OF INDIA");
    assert_eq!(sbin.expiry, "");
    assert_eq!(r.by_symbol("BSE", "RELIANCE").unwrap().token, "500325");
    // ETF reads as EQ; a quoted name with a comma keeps the columns.
    let bees = r.by_symbol("NSE", "NIFTYBEES").unwrap();
    assert_eq!(bees.instrument_type, "EQ");
    assert_eq!(bees.token, "0532");
    assert_eq!(bees.name, "NIPPON INDIA ETF NIFTY 50 BEES, GROWTH");
    // NaN lot size -> 1, blank tick -> 0.05.
    let infy = r.by_symbol("NSE", "INFY").unwrap();
    assert_eq!((infy.lot_size, infy.tick_size), (1, 0.05));
}

#[test]
fn master_contract_indices_and_renames() {
    let r = master();
    let nifty = r.by_symbol("NSE_INDEX", "NIFTY").unwrap();
    assert_eq!(nifty.instrument_type, "INDEX");
    assert_eq!(nifty.brexchange, "NSE");
    let jr = r.by_symbol("NSE_INDEX", "NIFTYNXT50").unwrap();
    assert_eq!(jr.brsymbol, "NIFTYJR");
    let sensex = r.by_symbol("BSE_INDEX", "SENSEX").unwrap();
    assert_eq!(sensex.token, "1");
    assert!(r.by_symbol("NSE_INDEX", "BANKNIFTY").is_some());
}

#[test]
fn master_contract_derivative_symbols() {
    let r = master();
    let ce = r.by_symbol("NFO", "NIFTY28OCT2524500CE").unwrap();
    assert_eq!(ce.brsymbol, "NIFTY25OCT24500CE");
    assert_eq!(ce.expiry, "28-OCT-25");
    assert_eq!((ce.strike, ce.lot_size), (24500.0, 75));
    assert_eq!(ce.name, "NIFTY");
    assert_eq!(ce.instrument_type, "CE");
    let fut = r.by_symbol("NFO", "NIFTY28OCT25FUT").unwrap();
    assert_eq!(fut.brsymbol, "NIFTY25OCTFUT");
    assert_eq!(fut.tick_size, 0.1);
    assert!(r.by_symbol("NFO", "VEDL28OCT25292.5CE").is_some());
    // Broker symbol with spaces: spaces stripped (web post-step).
    let fin = r.by_symbol("NFO", "FINNIFTY28OCT2526000PE").unwrap();
    assert_eq!(fin.brsymbol, "FINNIFTY 28 OCT 25 26000 PE");
    // Missing instrument type with a strike -> OPT, symbol not rebuilt.
    let opt = r.by_symbol("NFO", "SBIN25OCT800PE").unwrap();
    assert_eq!(opt.instrument_type, "OPT");
    // BFO keeps Groww's symbol (quirk 9.10).
    let bfo = r.by_symbol("BFO", "SENSEX25OCT82000CE").unwrap();
    assert_eq!(bfo.expiry, "30-OCT-25");
    // Option chains work off `name`.
    assert_eq!(r.expiries("NFO", "NIFTY", None), ["28-OCT-25"]);
}

#[test]
fn master_contract_rejects_unknown_header() {
    let e = parse_instruments("a,b\n1,2\n").unwrap_err();
    assert!(e.client_message().contains("unexpected format"));
    assert!(parse_instruments("").is_err());
}

// ---------------------------------------------------------------------------
// Field maps
// ---------------------------------------------------------------------------

#[test]
fn outbound_maps() {
    assert_eq!(groww_exchange("NFO"), "NSE");
    assert_eq!(groww_exchange("BFO"), "BSE");
    assert_eq!(groww_exchange("BSE_INDEX"), "BSE");
    assert_eq!(groww_segment("NFO"), "FNO");
    assert_eq!(groww_segment("NSE_INDEX"), "CASH");
    assert_eq!(order_type(PriceType::Sl), "STOP_LOSS_LIMIT");
    assert_eq!(order_type(PriceType::SlM), "STOP_LOSS_MARKET");
    assert_eq!(product(Product::Nrml), "NRML");
}

#[test]
fn inbound_maps() {
    assert_eq!(reverse_order_type("STOP_LOSS"), "SL");
    assert_eq!(reverse_order_type("STOP_LOSS_LIMIT"), "SL");
    assert_eq!(reverse_order_type("STOP_LOSS_MARKET"), "SL-M");
    assert_eq!(reverse_order_type("LIMIT"), "LIMIT");
    assert_eq!(reverse_product("INTRADAY"), "MIS");
    assert_eq!(reverse_product("MARGIN"), "NRML");
    assert_eq!(reverse_product("CNC"), "CNC");
    for (s, want) in [
        ("NEW", "open"),
        ("ACKED", "open"),
        ("APPROVED", "open"),
        ("OPEN", "open"),
        ("TRIGGER_PENDING", "trigger pending"),
        ("EXECUTED", "complete"),
        ("COMPLETED", "complete"),
        ("CANCELLED", "cancelled"),
        ("REJECTED", "rejected"),
        ("FAILED", "rejected"),
        ("MODIFICATION_REQUESTED", "open"),
    ] {
        assert_eq!(map_status(s), want, "{}", s);
    }
    assert!(is_cancellable("modification_requested"));
    assert!(!is_cancellable("EXECUTED"));
    assert_eq!(oa_exchange("NSE", "FNO"), "NFO");
    assert_eq!(oa_exchange("BSE", "FNO"), "BFO");
    // The web forced ITC to NFO because it contains a C (quirk 9.1).
    assert_eq!(oa_exchange("NSE", "CASH"), "NSE");
    assert_eq!(oa_exchange("BSE_EQ", ""), "BSE");
}

#[test]
fn derivative_fallbacks() {
    assert_eq!(
        derivative_fallback("NIFTY25051324500CE").as_deref(),
        Some("NIFTY13MAY2524500CE")
    );
    assert_eq!(
        derivative_fallback("NIFTY250513FUT").as_deref(),
        Some("NIFTY13MAY25FUT")
    );
    assert_eq!(derivative_fallback("NIFTY251313FUT"), None);
    assert_eq!(derivative_fallback("SBIN"), None);
    assert_eq!(derivative_symbol_fallback("SBIN30SEP25FUT"), "SBIN25SEPFUT");
    assert_eq!(
        derivative_symbol_fallback("SBIN30SEP25800CE"),
        "SBIN25SEP800CE"
    );
    assert_eq!(derivative_symbol_fallback("SBIN"), "SBIN");
}

#[test]
fn reference_ids_follow_web_rules() {
    assert_eq!(sanitize_reference_id("ab"), "ab000000");
    assert_eq!(sanitize_reference_id("a-b-c-d!e"), "a-b-cde0");
    assert_eq!(sanitize_reference_id(&"x".repeat(30)).len(), 20);
    let id = new_reference_id(chrono::NaiveDate::from_ymd_opt(2026, 10, 3).unwrap());
    assert!(id.starts_with("20261003-"));
    assert_eq!(id.len(), 17);
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

#[test]
fn order_book_is_normalised_to_openalgo() {
    let r = master();
    let mut raw = orders_of(&fx(fixture!("order_list_cash.json")));
    raw.extend(orders_of(&fx(fixture!("order_list_fno.json"))));
    let o = map_orders(&raw, &r);
    assert_eq!(o.len(), 6);
    assert_eq!(
        (
            o[0].symbol.as_str(),
            o[0].exchange.as_str(),
            o[0].status.as_str()
        ),
        ("SBIN", "NSE", "complete")
    );
    assert_eq!((o[0].filled_quantity, o[0].average_price), (10, 812.35));
    assert_eq!(o[0].order_type, "MARKET");
    // Not in the master: Groww symbol kept, exchange stays NSE.
    assert_eq!(
        (o[1].symbol.as_str(), o[1].exchange.as_str()),
        ("ITC", "NSE")
    );
    assert_eq!((o[1].product.as_str(), o[1].pending_quantity), ("MIS", 5));
    let rej = &o[2];
    assert_eq!(
        (
            rej.exchange.as_str(),
            rej.status.as_str(),
            rej.order_type.as_str()
        ),
        ("BSE", "rejected", "SL")
    );
    assert_eq!(rej.rejection_reason.as_deref(), Some("Insufficient funds"));
    assert_eq!((rej.quantity, rej.trigger_price), (3, 1395.0));
    assert_eq!(o[3].status, "trigger pending");
    assert_eq!(o[3].order_type, "SL-M");
    assert_eq!(
        (o[4].symbol.as_str(), o[4].exchange.as_str()),
        ("NIFTY28OCT2524500CE", "NFO")
    );
    assert_eq!(
        (
            o[5].symbol.as_str(),
            o[5].exchange.as_str(),
            o[5].product.as_str()
        ),
        ("SENSEX25OCT82000CE", "BFO", "NRML")
    );
    let s = order_stats(&raw);
    assert_eq!(
        (
            s.total_buy_orders,
            s.total_sell_orders,
            s.total_completed_orders,
            s.total_open_orders,
            s.total_rejected_orders
        ),
        (3, 3, 2, 1, 1)
    );
}

#[test]
fn trades_map_and_synthesise_without_scaling() {
    let r = master();
    let orders = orders_of(&fx(fixture!("order_list_cash.json")));
    let trades: Vec<GrowwTrade> =
        serde_json::from_value(fx(fixture!("trades.json"))["payload"]["trade_list"].clone())
            .unwrap();
    let t = map_trade(&trades[0], &orders[0], &r);
    assert_eq!((t.symbol.as_str(), t.exchange.as_str()), ("SBIN", "NSE"));
    assert_eq!((t.quantity, t.average_price), (6, 812.3));
    assert!((t.trade_value - 4873.8).abs() < 1e-9);
    assert_eq!(t.trade_id, "GMKT1001");
    let fno = orders_of(&fx(fixture!("order_list_fno.json")));
    assert!(has_fills(&fno[0]));
    assert!(!has_fills(&fno[1]));
    let syn = map_trade(&synthetic_trade(&fno[0]), &fno[0], &r);
    assert_eq!(syn.trade_id, "synthetic_GLTFO25100600001");
    assert_eq!(syn.symbol, "NIFTY28OCT2524500CE");
    // Rupees, never divided (web test_groww_tradebook_price).
    assert_eq!((syn.quantity, syn.average_price), (75, 112.4));
}

#[test]
fn positions_apply_web_derivations() {
    let r = master();
    let cash: Vec<GrowwPosition> =
        serde_json::from_value(fx(fixture!("positions_cash.json"))["payload"]["positions"].clone())
            .unwrap();
    let p = map_position(&cash[0], "CASH", &r);
    assert_eq!((p.symbol.as_str(), p.exchange.as_str()), ("SBIN", "NSE"));
    assert_eq!((p.quantity, p.buy_quantity, p.sell_quantity), (15, 15, 0));
    assert_eq!(p.average_price, 808.23);
    assert!((p.buy_value - 812.35 * 15.0).abs() < 1e-6);
    // No net quantity: buy - sell; prices are rupees as Groww sends them.
    let q = map_position(&cash[1], "CASH", &r);
    assert_eq!((q.exchange.as_str(), q.quantity), ("BSE", 0));
    assert_eq!(q.average_price, 1405.0);
    assert_eq!(q.product, "MIS");
    let fno: Vec<GrowwPosition> =
        serde_json::from_value(fx(fixture!("positions_fno.json"))["payload"]["positions"].clone())
            .unwrap();
    let f = map_position(&fno[0], "FNO", &r);
    assert_eq!(
        (f.symbol.as_str(), f.exchange.as_str(), f.quantity),
        ("NIFTY28OCT2524500CE", "NFO", 75)
    );
    // Rupees as sent: 1400 bought and 1410 sold per unit.
    assert!((q.buy_value - 1400.0 * q.buy_quantity as f64).abs() < 1e-6);
    assert!((q.sell_value - 1410.0 * q.sell_quantity as f64).abs() < 1e-6);
    assert!(says_no_positions("No positions found for user"));
    assert!(!says_no_positions("Internal error"));
}

#[test]
fn holdings_resolve_and_carry_no_prices() {
    let r = master();
    let rows: Vec<GrowwHolding> =
        serde_json::from_value(fx(fixture!("holdings.json"))["payload"]["holdings"].clone())
            .unwrap();
    let h: Vec<Holding> = rows.iter().map(|x| map_holding(x, &r)).collect();
    assert_eq!(
        (h[0].symbol.as_str(), h[0].quantity, h[0].t1_quantity),
        ("SBIN", 20, 2)
    );
    assert_eq!(h[0].isin.as_deref(), Some("INE062A01020"));
    assert_eq!((h[0].ltp, h[0].pnl), (0.0, 0.0));
    assert_eq!((h[1].quantity, h[1].average_price), (100, 245.1));
    assert_eq!(h[1].product, "CNC");
}

#[test]
fn funds_and_margin_parse() {
    let f = funds_from_payload(&fx(fixture!("funds.json"))["payload"]);
    assert_eq!(f.available_cash, 125000.5);
    assert_eq!(f.collateral, 15000.0);
    assert_eq!(f.utilised_debits, 23500.25);
    assert_eq!((f.m2m_realized, f.m2m_unrealized), (0.0, 0.0));
    let m = parse_margin(&fx(fixture!("margin.json"))["payload"]);
    assert_eq!(
        (m.total_margin_required, m.span_margin, m.exposure_margin),
        (121000.75, 98000.25, 23000.5)
    );
}

fn leg(symbol: &str, exchange: &str, price: f64) -> MarginLeg {
    MarginLeg {
        key: QuoteKey::new(exchange, symbol),
        action: Action::Buy,
        quantity: 75,
        product: Product::Nrml,
        pricetype: PriceType::Market,
        price,
        trigger_price: 0.0,
    }
}

#[test]
fn margin_payload_follows_segment_rules() {
    let c = core();
    let (seg, body) = margin_payload(
        &c,
        &[
            leg("NIFTY28OCT2524500CE", "NFO", 0.0),
            leg("SBIN", "NSE", 0.0),
            leg("SENSEX25OCT82000CE", "BFO", 150.0),
        ],
    );
    assert_eq!(seg, "FNO");
    assert_eq!(body.len(), 2);
    assert_eq!(body[0]["trading_symbol"], "NIFTY25OCT24500CE");
    assert_eq!(body[0]["exchange"], "NSE");
    assert_eq!(body[0]["order_type"], "MARKET");
    assert!(body[0].get("price").is_none());
    assert_eq!(body[1]["exchange"], "BSE");
    assert_eq!(body[1]["price"], 150.0);
    // CASH: first leg only.
    let (seg, body) = margin_payload(&c, &[leg("SBIN", "NSE", 0.0), leg("RELIANCE", "NSE", 0.0)]);
    assert_eq!((seg.as_str(), body.len()), ("CASH", 1));
}

// ---------------------------------------------------------------------------
// Order payloads
// ---------------------------------------------------------------------------

fn resolved(symbol: &str, exchange: &str, pricetype: &str, price: f64, trig: f64) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: "BUY".into(),
        quantity: 75,
        price,
        order_type: pricetype.into(),
        product: "NRML".into(),
        validity: "DAY".into(),
        trigger_price: Some(trig),
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, &master()).unwrap()
}

#[test]
fn place_body_matches_web_payload() {
    let o = resolved("NIFTY28OCT2524500CE", "NFO", "LIMIT", 110.5, 0.0);
    let b = place_order_body(&o, "20261003-1a2b3c4d");
    assert_eq!(
        b,
        json!({
            "trading_symbol": "NIFTY25OCT24500CE",
            "quantity": 75,
            "validity": "DAY",
            "exchange": "NSE",
            "segment": "FNO",
            "product": "NRML",
            "order_type": "LIMIT",
            "transaction_type": "BUY",
            "order_reference_id": "20261003-1a2b3c4d",
            "price": 110.5
        })
    );
    let m = place_order_body(&resolved("SBIN", "NSE", "MARKET", 0.0, 0.0), "x0000000");
    assert!(m.get("price").is_none() && m.get("trigger_price").is_none());
    let slm = place_order_body(&resolved("SBIN", "NSE", "SL-M", 0.0, 790.0), "x0000000");
    assert_eq!(slm["order_type"], "STOP_LOSS_MARKET");
    assert_eq!(slm["trigger_price"], 790.0);
    assert!(slm.get("price").is_none());
}

#[test]
fn modify_body_and_segments() {
    let m = ResolvedModify::resolve(
        "GLTFO1",
        &ModifyOrderRequest {
            symbol: "NIFTY28OCT2524500CE".into(),
            exchange: "NFO".into(),
            action: "BUY".into(),
            product: "NRML".into(),
            pricetype: "SL".into(),
            quantity: 150,
            price: 101.0,
            trigger_price: 100.0,
            disclosed_quantity: 0,
        },
        &master(),
    )
    .unwrap();
    assert_eq!(
        modify_order_body(&m),
        json!({"groww_order_id": "GLTFO1", "order_type": "STOP_LOSS_LIMIT", "segment": "FNO",
               "quantity": 150, "price": 101.0, "trigger_price": 100.0})
    );
    assert_eq!(segment_from_id("GLTFO123"), Some("FNO"));
    assert_eq!(segment_from_id("GMK123"), None);
}

// ---------------------------------------------------------------------------
// Market data
// ---------------------------------------------------------------------------

#[test]
fn quote_from_string_ohlc_and_aliases() {
    let k = QuoteKey::new("NSE", "SBIN");
    let q = to_quote(&k, &fx(fixture!("quote_cash.json"))["payload"]);
    assert_eq!(
        (q.ltp, q.open, q.high, q.low, q.close),
        (812.35, 809.0, 815.5, 806.25, 808.0)
    );
    assert_eq!(
        (q.bid, q.ask, q.bid_qty, q.ask_qty),
        (812.3, 812.4, 120, 80)
    );
    assert_eq!((q.volume, q.oi), (5123400, 0));
    assert_eq!((q.change, q.change_percent), (4.35, 0.54));
    let d = to_depth(&k, &fx(fixture!("quote_cash.json"))["payload"]);
    assert_eq!((d.bids.len(), d.asks.len()), (5, 5));
    assert_eq!((d.bids[2].price, d.bids[2].quantity), (812.2, 45));
    assert_eq!(d.asks[4], DepthLevel::default());
    assert_eq!((d.ltq, d.prev_close, d.total_buy_qty), (7, 808.0, 245000));
}

#[test]
fn fno_quote_falls_back_to_top_of_book() {
    let k = QuoteKey::new("NFO", "NIFTY28OCT2524500CE");
    let p = fx(fixture!("quote_fno.json"));
    let q = to_quote(&k, &p["payload"]);
    assert_eq!(
        (q.bid, q.bid_qty, q.ask, q.ask_qty),
        (112.35, 1500, 112.5, 2250)
    );
    assert_eq!((q.oi, q.volume, q.close), (4567800, 12345675, 101.2));
    let d = to_depth(&k, &p["payload"]);
    assert_eq!(d.oi, 4567800);
}

#[test]
fn ohlc_entries_and_invalid_symbols() {
    let p = fx(fixture!("ohlc.json"));
    let k = QuoteKey::new("NSE", "SBIN");
    let q = quote_from_ohlc(&k, &p["payload"]["NSE_SBIN"]);
    assert_eq!(
        (q.open, q.high, q.low, q.ltp, q.close),
        (809.0, 815.5, 806.25, 812.35, 812.35)
    );
    let r = quote_from_ohlc(&k, &p["payload"]["BSE_RELIANCE"]);
    assert_eq!(r.ltp, 1405.1);
    let n = quote_from_ohlc(&k, &p["payload"]["NSE_NIFTY"]);
    assert_eq!((n.ltp, n.open), (24890.15, 0.0));
    assert_eq!(
        invalid_symbol(r#"{"error":{"message":"Invalid trading symbol: FOO-BE in request"}}"#)
            .as_deref(),
        Some("FOO-BE")
    );
    assert_eq!(invalid_symbol("other"), None);
    let c = core();
    assert_eq!(
        exchange_symbol(&c, &QuoteKey::new("BFO", "SENSEX25OCT82000CE")),
        "BSE_SENSEX25OCT82000CE"
    );
    assert_eq!(
        exchange_symbol(&c, &QuoteKey::new("NSE_INDEX", "NIFTYNXT50")),
        "NSE_NIFTYJR"
    );
}

#[test]
fn history_processing() {
    assert_eq!(interval_minutes("4h").unwrap(), 240);
    assert!(interval_minutes("3m").is_err());
    assert_eq!(
        [1, 5, 10, 60, 240, 1440, 10080].map(chunk_days),
        [3, 7, 7, 15, 15, 100, 300]
    );
    assert_eq!(
        history_exchange_segment("BSE_INDEX").unwrap(),
        ("BSE", "CASH")
    );
    assert_eq!(history_exchange_segment("NFO").unwrap(), ("NSE", "FNO"));
    assert!(history_exchange_segment("MCX").is_err());

    let rows = fx(fixture!("history_intraday.json"));
    let raw: Vec<Candle> = rows["payload"]["candles"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(raw_candle)
        .collect();
    let c = process_candles(raw, 5);
    let ts: Vec<i64> = c.iter().map(|c| c.timestamp).collect();
    // 09:10 and 15:35 IST dropped; ms -> s; sorted and de-duplicated.
    assert_eq!(ts, [1759722300, 1759722600, 1759723200, 1759744800]);
    assert_eq!(c[2].volume, 0);
    assert!(c.iter().all(|c| c.oi == 0));

    let rows = fx(fixture!("history_daily.json"));
    let raw: Vec<Candle> = rows["payload"]["candles"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(raw_candle)
        .collect();
    let d = process_candles(raw.clone(), 1440);
    let ts: Vec<i64> = d.iter().map(|c| c.timestamp).collect();
    // Midnight UTC of the IST date (quirk 9.11).
    assert_eq!(ts, [1759708800, 1759795200, 1759881600, 1760313600]);
    let w = process_candles(raw, 10080);
    assert_eq!(w.len(), 2);
    assert_eq!(w[0].timestamp, 1759722300); // Monday 6 Oct 09:15 IST
    assert_eq!(
        (w[0].open, w[0].high, w[0].low, w[0].close),
        (800.0, 822.0, 795.0, 806.0)
    );
    assert_eq!(w[0].volume, 13_100_000);
    assert_eq!(w[1].timestamp, 1760327100);
}

// ---------------------------------------------------------------------------
// Auth helpers
// ---------------------------------------------------------------------------

#[test]
fn checksum_and_variants() {
    // sha256("secret" + "1700000000")
    let mut h = sha2::Sha256::new();
    sha2::Digest::update(&mut h, b"secret1700000000");
    assert_eq!(
        checksum("secret", "1700000000"),
        hex::encode(sha2::Digest::finalize(h))
    );
    assert!(looks_like_jwt("eyJa.b.c"));
    assert!(!looks_like_jwt("abc.def"));
    let base = BrokerCredentials {
        api_key: "KEY".into(),
        ..Default::default()
    };
    let with = |f: &dyn Fn(&mut BrokerCredentials)| {
        let mut c = base.clone();
        f(&mut c);
        choose_variant(&c)
    };
    assert!(matches!(
        with(&|c| c.totp = Some("123456".into())).unwrap(),
        Variant::Totp { .. }
    ));
    assert_eq!(
        with(&|c| c.password = Some(" tok ".into())).unwrap(),
        Variant::PastedToken("tok".into())
    );
    assert!(matches!(
        with(&|c| c.api_secret = Some("s".into())).unwrap(),
        Variant::Approval { .. }
    ));
    assert_eq!(
        with(&|c| c.api_key = "eyJh.x.y".into()).unwrap(),
        Variant::PastedToken("eyJh.x.y".into())
    );
    assert!(with(&|_| {}).is_err());
}

use sha2::Digest as _;

// ---------------------------------------------------------------------------
// nkeys, NATS and protobuf
// ---------------------------------------------------------------------------

#[test]
fn nkeys_encode_and_sign() {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    assert_eq!(nkeys::crc16(b"123456789"), 0x31C3);
    let kp = KeyPair::from_seed(&[7u8; 32]);
    let public = kp.public_key();
    assert!(public.starts_with('U'));
    assert_eq!(public.len(), 56);
    let seed = kp.seed();
    assert!(seed.starts_with("SU"));
    assert!(nkeys::decode(&seed).is_some());
    let mut broken = public.clone().into_bytes();
    broken[10] = if broken[10] == b'A' { b'B' } else { b'A' };
    assert!(nkeys::decode(std::str::from_utf8(&broken).unwrap()).is_none());
    let pk = nkeys::public_key_bytes(&public).unwrap();
    let sig_b64 = kp.sign_nonce("abc-nonce");
    let sig = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, sig_b64).unwrap();
    let vk = VerifyingKey::from_bytes(&pk).unwrap();
    let sig = Signature::from_slice(&sig).unwrap();
    assert!(vk.verify(b"abc-nonce", &sig).is_ok());
    assert!(!format!("{:?}", kp).contains(seed.as_str()));
    assert_ne!(KeyPair::generate().public_key(), public);
}

#[test]
fn nats_ops_parse_including_split_frames() {
    assert_eq!(next_op(b"PING\r\n"), Some((Op::Ping, 6)));
    assert_eq!(next_op(b"+OK\r\nPONG\r\n"), Some((Op::Ok, 5)));
    let (op, _) = next_op(b"-ERR 'Authorization Violation'\r\n").unwrap();
    assert_eq!(op, Op::Err("Authorization Violation".into()));
    let (op, _) = next_op(b"INFO {\"server_id\":\"x\",\"nonce\":\"n1\"}\r\n").unwrap();
    assert!(matches!(op, Op::Info(v) if v["nonce"] == "n1"));
    let msg = b"MSG /ld/eq/nse/price.3045 7 5\r\nhello\r\n";
    // Partial: wait for more bytes.
    assert_eq!(next_op(&msg[..20]), None);
    assert_eq!(next_op(&msg[..msg.len() - 1]), None);
    let (op, n) = next_op(msg).unwrap();
    assert_eq!(n, msg.len());
    assert_eq!(
        op,
        Op::Msg {
            subject: "/ld/eq/nse/price.3045".into(),
            sid: 7,
            payload: b"hello".to_vec()
        }
    );
    let with_reply = b"MSG subj 3 inbox.1 2\r\nab\r\n";
    assert!(matches!(
        next_op(with_reply).unwrap().0,
        Op::Msg { sid: 3, .. }
    ));
    let hmsg = b"HMSG subj 4 12 14\r\nNATS/1.0\r\n\r\nxy\r\n";
    match next_op(hmsg).unwrap().0 {
        Op::Msg { payload, sid, .. } => assert_eq!((payload, sid), (b"xy".to_vec(), 4)),
        o => panic!("{:?}", o),
    }
}

#[test]
fn connect_frame_matches_web() {
    let f = connect_frame("jwt1", Some("UKEY"), Some("SIG"));
    assert!(f.starts_with("CONNECT {") && f.ends_with("}\r\n"));
    let v: Value = serde_json::from_str(&f[8..f.len() - 2]).unwrap();
    assert_eq!(v["jwt"], "jwt1");
    assert_eq!(
        (v["nkey"].as_str(), v["sig"].as_str()),
        (Some("UKEY"), Some("SIG"))
    );
    assert_eq!(v["name"], "nats.py");
    assert_eq!(v["verbose"], false);
    let plain = connect_frame("jwt1", None, None);
    assert!(!plain.contains("nkey") && !plain.contains("sig"));
}

fn text(m: &Message) -> String {
    match m {
        Message::Text(t) => t.clone(),
        Message::Binary(b) => String::from_utf8_lossy(b).to_string(),
        other => format!("{:?}", other),
    }
}

#[test]
fn nats_session_handshake_replies_and_forwarding() {
    let kp = KeyPair::from_seed(&[9u8; 32]);
    let public = kp.public_key();
    let mut s = NatsSession::new("JWT".into(), Some(kp));
    assert!(!s.ready_on_open());
    // A subscribe before CONNECT is held back.
    assert!(s
        .on_downstream(Message::Text("SUB a 1\r\n".into()))
        .is_empty());
    let step = s.on_upstream(Message::Text("INFO {\"nonce\":\"N0\"}\r\n".into()));
    let ups: Vec<String> = step.up.iter().map(text).collect();
    assert!(ups[0].starts_with("CONNECT "));
    assert!(ups[0].contains(&public));
    assert!(ups[0].contains("\"sig\""));
    assert_eq!(ups[1], "PING\r\n");
    assert_eq!(ups[2], "SUB a 1\r\n");
    assert!(!step.ready);
    let step = s.on_upstream(Message::Text("PONG\r\nPING\r\n".into()));
    assert!(step.ready);
    assert_eq!(text(&step.up[0]), "PONG\r\n");
    assert!(matches!(step.down[0], Message::Ping(_)));
    // MSG split across two frames is forwarded once, whole.
    let step = s.on_upstream(Message::Binary(b"MSG s 1 3\r\nab".to_vec()));
    assert!(step.down.is_empty());
    let step = s.on_upstream(Message::Binary(b"c\r\n".to_vec()));
    assert_eq!(
        step.down,
        vec![Message::Binary(b"MSG s 1 3\r\nabc\r\n".to_vec())]
    );
    let step = s.on_upstream(Message::Text("-ERR 'Authorization Violation'\r\n".into()));
    assert!(step.auth_failed.is_some());
    assert_eq!(s.keepalive().map(|(d, _)| d), Some(NATS_PING_EVERY));
}

fn sub(symbol: &str, exchange: &str, token: &str, mode: FeedMode) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: exchange.into(),
        token: token.into(),
        brsymbol: symbol.into(),
        brexchange: "NSE".into(),
        mode,
        depth: 5,
    }
}

#[test]
fn subjects_follow_web_topics() {
    assert_eq!(
        subjects(&sub("SBIN", "NSE", "3045", FeedMode::Ltp)),
        ["/ld/eq/nse/price.3045"]
    );
    assert_eq!(
        subjects(&sub("X", "NFO", "35001", FeedMode::Depth)),
        ["/ld/fo/nse/price.35001", "/ld/fo/nse/book.35001"]
    );
    assert_eq!(
        subjects(&sub("SENSEX25OCT82000CE", "BFO", "825001", FeedMode::Quote)),
        ["/ld/fo/bse/price.825001"]
    );
    // NSE index: symbol as token; depth falls back to price.
    assert_eq!(
        subjects(&sub("NIFTY", "NSE_INDEX", "NIFTY", FeedMode::Depth)),
        ["/ld/eq/nse/price.NIFTY"]
    );
    assert_eq!(
        subjects(&sub("SENSEX", "BSE_INDEX", "1", FeedMode::Ltp)),
        ["/ld/eq/bse/price.1"]
    );
}

fn ltp_payload(ltp: f64) -> Vec<u8> {
    proto::LiveData {
        symbol: "SBIN".into(),
        segment: 0,
        exchange: 1,
        ltp_data: Some(proto::StocksLivePrice {
            ts_in_millis: 1759722011000.0,
            open: 809.0,
            high: 815.5,
            low: 806.25,
            close: 808.0,
            volume: 5_123_400.0,
            value: 0.0,
            ltp,
        }),
        ..Default::default()
    }
    .encode_to_vec()
}

fn depth_payload() -> Vec<u8> {
    let lvl = |o, p, q| proto::DepthLevel {
        orders: o,
        price_qty: Some(proto::PriceQty {
            price: p,
            quantity: q,
        }),
    };
    proto::LiveData {
        depth_data: Some(proto::MarketDepth {
            ts_in_millis: 1759722012000.0,
            buy: vec![lvl(3, 812.3, 120.0), lvl(0, 0.0, 0.0)],
            sell: vec![lvl(2, 812.4, 80.0)],
        }),
        ..Default::default()
    }
    .encode_to_vec()
}

fn msg_frame(sid: u64, payload: &[u8]) -> Message {
    let mut b = format!("MSG /ld/x {} {}\r\n", sid, payload.len()).into_bytes();
    b.extend_from_slice(payload);
    b.extend_from_slice(b"\r\n");
    Message::Binary(b)
}

#[test]
fn protobuf_decodes_hand_built_messages() {
    let d = proto::decode(&ltp_payload(812.35)).unwrap();
    assert_eq!(d.symbol, "SBIN");
    assert_eq!(d.exchange, 1);
    assert_eq!(d.ltp_data.unwrap().ltp, 812.35);
    let i = proto::LiveIndex {
        ts_in_millis: 1.0,
        value: 24890.15,
    };
    let bytes = proto::LiveData {
        index_data: Some(i),
        ..Default::default()
    }
    .encode_to_vec();
    assert_eq!(
        proto::decode(&bytes).unwrap().index_data.unwrap().value,
        24890.15
    );
}

#[test]
fn feed_frames_and_ticks() {
    let mut f = GrowwFeed::new(
        crate::brokers::common::http::client(),
        "tok",
        FeedEndpoints::default(),
    );
    assert!(f.awaits_auth_ack());
    assert_eq!(
        f.parse(&Message::Text(READY.into())),
        vec![FeedEvent::AuthOk]
    );
    let frames = f.subscribe_frames(&[
        sub("SBIN", "NSE", "3045", FeedMode::Quote),
        sub("NIFTY28OCT2524500CE", "NFO", "35001", FeedMode::Depth),
    ]);
    assert_eq!(
        text(&frames[0]),
        "SUB /ld/eq/nse/price.3045 1\r\nSUB /ld/fo/nse/price.35001 2\r\nSUB /ld/fo/nse/book.35001 3\r\nPING\r\n"
    );
    // Quote tick.
    let ev = f.parse(&msg_frame(1, &ltp_payload(812.35)));
    let FeedEvent::Tick(t) = &ev[0] else {
        panic!("{:?}", ev)
    };
    assert_eq!(
        (t.symbol.as_str(), t.exchange.as_str(), t.mode),
        ("SBIN", "NSE", 2)
    );
    assert_eq!(
        (t.ltp, t.open, t.close, t.volume),
        (812.35, 809.0, 808.0, 5_123_400)
    );
    assert_eq!(t.last_trade_time_ms, 1759722011000);
    assert_eq!(t.change, 4.35);
    // Quote subscriptions ignore book ticks; depth subscriptions merge.
    assert!(f.parse(&msg_frame(1, &depth_payload())).is_empty());
    let ev = f.parse(&msg_frame(3, &depth_payload()));
    assert_eq!(ev.len(), 2);
    let FeedEvent::Depth(d) = &ev[1] else {
        panic!("{:?}", ev)
    };
    assert_eq!(d.symbol, "NIFTY28OCT2524500CE");
    assert_eq!(d.buy.len(), 1); // the zero placeholder level is dropped
    assert_eq!((d.buy[0].price, d.buy[0].orders), (812.3, 3));
    let ev = f.parse(&msg_frame(2, &ltp_payload(112.4)));
    let FeedEvent::Depth(d) = &ev[1] else {
        panic!("{:?}", ev)
    };
    assert_eq!((d.ltp, d.sell[0].price), (112.4, 812.4));
    // Unknown sid, garbage payload: nothing.
    assert!(f.parse(&msg_frame(99, &ltp_payload(1.0))).is_empty());
    assert!(f.parse(&msg_frame(1, b"\xff\xff")).is_empty());
    // Unsubscribe removes every sid of the instrument.
    let un = f.unsubscribe_frames(&[sub("NIFTY28OCT2524500CE", "NFO", "35001", FeedMode::Depth)]);
    assert_eq!(text(&un[0]), "UNSUB 2\r\nUNSUB 3\r\n");
    assert_eq!(f.instrument_count(), 1);
    assert!(f.parse(&msg_frame(3, &depth_payload())).is_empty());
    // A new connection forgets per-connection sids.
    f.on_connected();
    assert_eq!(f.instrument_count(), 0);
    assert!(matches!(
        f.parse(&Message::Text(
            r#"{"openalgo_relay":"auth_failed","message":"Log in"}"#.into()
        ))[0],
        FeedEvent::AuthFailed(_)
    ));
}

#[test]
fn index_ticks_carry_ltp() {
    let mut f = GrowwFeed::new(
        crate::brokers::common::http::client(),
        "tok",
        FeedEndpoints::default(),
    );
    f.subscribe_frames(&[sub("NIFTY", "NSE_INDEX", "NIFTY", FeedMode::Depth)]);
    let bytes = proto::LiveData {
        index_data: Some(proto::LiveIndex {
            ts_in_millis: 5.0,
            value: 24890.15,
        }),
        ..Default::default()
    }
    .encode_to_vec();
    let ev = f.parse(&msg_frame(1, &bytes));
    assert_eq!(ev.len(), 1);
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!((t.ltp, t.exchange.as_str()), (24890.15, "NSE_INDEX"));
}

// ---------------------------------------------------------------------------
// Order poller
// ---------------------------------------------------------------------------

#[test]
fn poller_diff_seeds_then_reports_changes() {
    let r = master();
    let raw = orders_of(&fx(fixture!("order_list_cash.json")));
    let mut book = map_orders(&raw, &r);
    let (snap, changed) = diff(None, &book);
    assert!(changed.is_empty());
    assert_eq!(snap.len(), 4);
    book[1].status = "complete".into();
    book[1].filled_quantity = 5;
    let (_, changed) = diff(Some(&snap), &book);
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].orderid, "GMK39038RDT490CCVRP");
    assert_eq!(
        (changed[0].order_status.as_str(), changed[0].filled_quantity),
        ("complete", 5)
    );
    assert_eq!(clamp_interval(std::time::Duration::ZERO).as_secs(), 1);
    assert_eq!(
        clamp_interval(std::time::Duration::from_secs(600)).as_secs(),
        60
    );
}

// ---------------------------------------------------------------------------
// HTTP round trips against a local fake Groww (ephemeral port)
// ---------------------------------------------------------------------------

mod http_round_trip {
    use super::*;
    use axum::extract::{Path, Query};
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::sync::Arc;

    type Seen = Arc<Mutex<Vec<String>>>;

    async fn serve(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{}", addr)
    }

    fn bearer(h: &HeaderMap) -> String {
        h.get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    }

    fn fake(seen: Seen, fno_positions_fail: bool) -> Router {
        let s = seen.clone();
        let s2 = seen.clone();
        let s3 = seen.clone();
        let s4 = seen.clone();
        let s5 = seen.clone();
        let s6 = seen.clone();
        let s7 = seen.clone();
        let s8 = seen;
        Router::new()
            .route(
                "/v1/token/api/access",
                post(move |h: HeaderMap, Json(b): Json<Value>| async move {
                    s.lock().push(format!("token|{}|{}", bearer(&h), b["key_type"]));
                    let ok = match b["key_type"].as_str() {
                        Some("totp") => b["totp"] == "123456",
                        Some("approval") => {
                            let ts = b["timestamp"].as_str().unwrap_or("");
                            b["checksum"] == checksum("SECRET", ts)
                        }
                        _ => false,
                    };
                    if ok {
                        (StatusCode::OK, Json(json!({"token": "good", "tokenRefId": "r"})))
                    } else {
                        (StatusCode::BAD_REQUEST, Json(json!({"error": {"message": "bad"}})))
                    }
                }),
            )
            .route(
                "/v1/margins/detail/user",
                get(|h: HeaderMap| async move {
                    if bearer(&h) == "Bearer good" {
                        (StatusCode::OK, Json(fx(fixture!("funds.json"))))
                    } else {
                        (StatusCode::UNAUTHORIZED, Json(json!({"status": "FAILURE"})))
                    }
                }),
            )
            .route(
                "/v1/order/list",
                get(move |Query(q): Query<HashMap<String, String>>| async move {
                    s2.lock().push(format!("list|{}|{}|{}", q["segment"], q["page"], q["page_size"]));
                    if q["page"] != "0" {
                        return Json(json!({"status": "SUCCESS", "payload": {"order_list": []}}));
                    }
                    if q["segment"] == "FNO" {
                        Json(fx(fixture!("order_list_fno.json")))
                    } else {
                        Json(fx(fixture!("order_list_cash.json")))
                    }
                }),
            )
            .route(
                "/v1/order/create",
                post(move |Json(b): Json<Value>| async move {
                    s3.lock().push(format!("create|{}", b));
                    Json(json!({"status": "SUCCESS", "payload": {"groww_order_id": "GMK1", "order_status": "OPEN", "order_reference_id": b["order_reference_id"]}}))
                }),
            )
            .route(
                "/v1/order/modify",
                post(move |Json(b): Json<Value>| async move {
                    s4.lock().push(format!("modify|{}", b));
                    if b["groww_order_id"] == "BAD" {
                        return (StatusCode::BAD_REQUEST, Json(json!({"status": "FAILURE", "error": {"message": "Order not modifiable"}})));
                    }
                    (StatusCode::OK, Json(json!({"status": "SUCCESS", "payload": {"groww_order_id": b["groww_order_id"], "order_status": "MODIFICATION_REQUESTED"}})))
                }),
            )
            .route(
                "/v1/order/cancel",
                post(move |Json(b): Json<Value>| async move {
                    s5.lock().push(format!("cancel|{}|{}", b["groww_order_id"], b["segment"]));
                    Json(json!({"status": "SUCCESS", "payload": {"groww_order_id": b["groww_order_id"], "order_status": "CANCELLATION_REQUESTED"}}))
                }),
            )
            .route(
                "/v1/order/trades/{id}",
                get(move |Path(id): Path<String>, Query(q): Query<HashMap<String, String>>| async move {
                    s6.lock().push(format!("trades|{}|{}", id, q["segment"]));
                    if id == "GMK39038RDT490CCVRO" {
                        (StatusCode::OK, Json(fx(fixture!("trades.json"))))
                    } else {
                        (StatusCode::NOT_FOUND, Json(json!({"status": "FAILURE"})))
                    }
                }),
            )
            .route(
                "/v1/positions/user",
                get(move |Query(q): Query<HashMap<String, String>>| async move {
                    if q["segment"] == "FNO" {
                        if fno_positions_fail {
                            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"status": "FAILURE"})));
                        }
                        return (StatusCode::OK, Json(fx(fixture!("positions_fno.json"))));
                    }
                    (StatusCode::OK, Json(fx(fixture!("positions_cash.json"))))
                }),
            )
            .route(
                "/v1/holdings/user",
                get(|h: HeaderMap| async move {
                    if h.get("x-api-version").and_then(|v| v.to_str().ok()) != Some("1.0") {
                        return Json(json!({"status": "FAILURE"}));
                    }
                    Json(fx(fixture!("holdings.json")))
                }),
            )
            .route(
                "/v1/live-data/quote",
                get(move |Query(q): Query<HashMap<String, String>>| async move {
                    s7.lock().push(format!("quote|{}|{}|{}", q["exchange"], q["segment"], q["trading_symbol"]));
                    if q["segment"] == "FNO" {
                        Json(fx(fixture!("quote_fno.json")))
                    } else {
                        Json(fx(fixture!("quote_cash.json")))
                    }
                }),
            )
            .route(
                "/v1/live-data/ohlc",
                get(move |Query(q): Query<HashMap<String, String>>| async move {
                    let syms = q["exchange_symbols"].clone();
                    s8.lock().push(format!("ohlc|{}|{}", q["segment"], syms));
                    if syms.contains("NSE_BOGUS") {
                        return (StatusCode::BAD_REQUEST, Json(json!({"status": "FAILURE", "error": {"message": "Invalid trading symbol: BOGUS"}})));
                    }
                    let mut p = serde_json::Map::new();
                    for s in syms.split(',') {
                        p.insert(s.to_string(), json!("{open: 100.0,high: 110.0,low: 95.0,close: 105.0}"));
                    }
                    (StatusCode::OK, Json(json!({"status": "SUCCESS", "payload": p})))
                }),
            )
            .route(
                "/v1/historical/candle/range",
                get(|Query(q): Query<HashMap<String, String>>| async move {
                    assert_eq!(q["start_time"], "2025-10-06 09:15:00");
                    assert_eq!(q["end_time"], "2025-10-06 15:30:00");
                    assert_eq!(q["interval_in_minutes"], "5");
                    assert_eq!(q["trading_symbol"], "SBIN");
                    Json(fx(fixture!("history_intraday.json")))
                }),
            )
            .route(
                "/v1/margins/detail/orders",
                post(|Query(q): Query<HashMap<String, String>>, Json(b): Json<Value>| async move {
                    assert_eq!(q["segment"], "FNO");
                    assert!(b.is_array());
                    Json(fx(fixture!("margin.json")))
                }),
            )
            .fallback(|| async { StatusCode::NOT_FOUND.into_response() })
    }

    async fn broker(fno_fail: bool) -> (GrowwBroker, Seen) {
        let seen: Seen = Arc::default();
        let base = serve(fake(seen.clone(), fno_fail)).await;
        (GrowwBroker::with_base_url(master(), base), seen)
    }

    fn creds() -> BrokerCredentials {
        BrokerCredentials {
            api_key: "APIKEY".into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn auth_variants() {
        let (b, seen) = broker(false).await;
        let mut c = creds();
        c.totp = Some("123456".into());
        assert_eq!(b.authenticate(c).await.unwrap().auth_token, "good");
        let mut c = creds();
        c.api_secret = Some("SECRET".into());
        assert_eq!(b.authenticate(c).await.unwrap().auth_token, "good");
        let mut c = creds();
        c.password = Some("good".into());
        assert_eq!(b.authenticate(c).await.unwrap().auth_token, "good");
        let mut c = creds();
        c.password = Some("stale".into());
        assert_eq!(b.authenticate(c).await.unwrap_err().code(), "AUTH_ERROR");
        let mut c = creds();
        c.api_secret = Some("WRONG".into());
        let e = b.authenticate(c).await.unwrap_err();
        assert_eq!(e.code(), "AUTH_ERROR");
        assert!(!e.client_message().contains("400"));
        let seen = seen.lock().clone();
        assert_eq!(seen[0], "token|Bearer APIKEY|\"totp\"");
        assert_eq!(seen[1], "token|Bearer APIKEY|\"approval\"");
    }

    #[tokio::test]
    async fn orders_and_books() {
        let (b, seen) = broker(false).await;
        let auth = AuthToken::new("good");
        let r = b
            .place_order(
                &auth,
                &resolved("NIFTY28OCT2524500CE", "NFO", "LIMIT", 110.5, 0.0),
            )
            .await
            .unwrap();
        assert_eq!(r.order_id, "GMK1");
        let book = b.get_order_book(&auth).await.unwrap();
        assert_eq!(book.len(), 6);
        assert_eq!(book[4].symbol, "NIFTY28OCT2524500CE");
        // Cancel resolves the segment from the book.
        b.cancel_order(&auth, "GMK39038RDT490CCVRP").await.unwrap();
        b.cancel_order(&auth, "GLTFO25100600009").await.unwrap();
        let all = b.cancel_all_orders(&auth).await.unwrap();
        assert_eq!(
            all.cancelled,
            ["GMK39038RDT490CCVRP", "GMK39038RDT490CCVRR"]
        );
        let trades = b.get_trade_book(&auth).await.unwrap();
        assert_eq!(trades.len(), 3);
        assert_eq!(trades[2].trade_id, "synthetic_GLTFO25100600001");
        let seen = seen.lock().clone();
        let create = seen.iter().find(|s| s.starts_with("create|")).unwrap();
        assert!(create.contains("\"trading_symbol\":\"NIFTY25OCT24500CE\""));
        assert!(create.contains("\"segment\":\"FNO\""));
        assert!(seen.contains(&"list|CASH|0|25".to_string()));
        assert!(seen.contains(&"list|FNO|0|25".to_string()));
        assert!(seen.contains(&"cancel|\"GMK39038RDT490CCVRP\"|\"CASH\"".to_string()));
        assert!(seen.contains(&"cancel|\"GLTFO25100600009\"|\"FNO\"".to_string()));
        assert!(seen.contains(&"trades|GLTFO25100600001|FNO".to_string()));
    }

    #[tokio::test]
    async fn modify_refusal_is_an_error() {
        let (b, _) = broker(false).await;
        let auth = AuthToken::new("good");
        let req = ModifyOrderRequest {
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            action: "BUY".into(),
            product: "CNC".into(),
            pricetype: "LIMIT".into(),
            quantity: 1,
            price: 800.0,
            trigger_price: 0.0,
            disclosed_quantity: 0,
        };
        let ok = ResolvedModify::resolve("GMK9", &req, &master()).unwrap();
        assert_eq!(b.modify_order(&auth, &ok).await.unwrap().order_id, "GMK9");
        let bad = ResolvedModify::resolve("BAD", &req, &master()).unwrap();
        let e = b.modify_order(&auth, &bad).await.unwrap_err();
        assert_eq!(e.client_message(), "Order not modifiable");
    }

    #[tokio::test]
    async fn positions_holdings_funds_margin() {
        let (b, _) = broker(false).await;
        let auth = AuthToken::new("good");
        let p = b.get_positions(&auth).await.unwrap();
        assert_eq!(p.len(), 3);
        assert_eq!(
            b.get_open_position(&auth, "NIFTY28OCT2524500CE", Exchange::Nfo, Product::Nrml)
                .await
                .unwrap(),
            75
        );
        let h = b.get_holdings(&auth).await.unwrap();
        assert_eq!(h.len(), 2);
        let f = b.get_funds(&auth).await.unwrap();
        assert_eq!(f.available_cash, 125000.5);
        let m = b
            .calculate_margin(&auth, &[leg("NIFTY28OCT2524500CE", "NFO", 0.0)])
            .await
            .unwrap();
        assert_eq!(m.total_margin_required, 121000.75);
        let e = b.get_funds(&AuthToken::new("expired")).await.unwrap_err();
        assert_eq!(e.code(), "AUTH_ERROR");
    }

    #[tokio::test]
    async fn failed_fno_positions_block_open_position() {
        let (b, _) = broker(true).await;
        let auth = AuthToken::new("good");
        // The book still shows cash positions.
        assert_eq!(b.get_positions(&auth).await.unwrap().len(), 2);
        assert_eq!(
            b.get_open_position(&auth, "SBIN", Exchange::Nse, Product::Cnc)
                .await
                .unwrap(),
            15
        );
        assert!(b
            .get_open_position(&auth, "NIFTY28OCT2524500CE", Exchange::Nfo, Product::Nrml)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn quotes_depth_multiquotes_history() {
        let (b, seen) = broker(false).await;
        let auth = AuthToken::new("good");
        let q = b
            .get_quote(&auth, &QuoteKey::new("NSE", "SBIN"))
            .await
            .unwrap();
        assert_eq!(q.ltp, 812.35);
        let d = b
            .get_market_depth(&auth, &QuoteKey::new("NFO", "NIFTY28OCT2524500CE"))
            .await
            .unwrap();
        assert_eq!(d.bids.len(), 5);
        assert_eq!(d.oi, 4567800);
        let mq = b
            .get_multiquotes(
                &auth,
                &[
                    QuoteKey::new("NSE", "SBIN"),
                    QuoteKey::new("NSE", "BOGUS"),
                    QuoteKey::new("NFO", "NIFTY28OCT2524500CE"),
                ],
            )
            .await
            .unwrap();
        assert_eq!(mq.len(), 3);
        assert_eq!(mq[0].data.as_ref().unwrap().ltp, 105.0);
        assert_eq!(
            mq[1].error.as_deref(),
            Some("Invalid trading symbol in Groww")
        );
        // F&O overlay adds bid/ask and OI.
        let fo = mq[2].data.as_ref().unwrap();
        assert_eq!((fo.ltp, fo.bid, fo.oi), (105.0, 112.35, 4567800));
        let h = b
            .get_history(
                &auth,
                &HistoryRequest {
                    key: QuoteKey::new("NSE", "SBIN"),
                    interval: "5m".into(),
                    start: chrono::NaiveDate::from_ymd_opt(2025, 10, 6).unwrap(),
                    end: chrono::NaiveDate::from_ymd_opt(2025, 10, 6).unwrap(),
                },
            )
            .await
            .unwrap();
        assert_eq!(h.len(), 4);
        let seen = seen.lock().clone();
        assert!(seen.contains(&"quote|NSE|CASH|SBIN".to_string()));
        assert!(seen.contains(&"quote|NSE|FNO|NIFTY25OCT24500CE".to_string()));
        assert!(seen.contains(&"ohlc|CASH|NSE_SBIN,NSE_BOGUS".to_string()));
        assert!(seen.contains(&"ohlc|CASH|NSE_SBIN".to_string()));
        assert!(seen.contains(&"ohlc|FNO|NSE_NIFTY25OCT24500CE".to_string()));
    }

    #[tokio::test]
    async fn empty_token_is_refused_before_any_call() {
        let b = GrowwBroker::with_base_url(master(), "http://127.0.0.1:9");
        let e = b.get_order_book(&AuthToken::new("")).await.unwrap_err();
        assert_eq!(e.code(), "AUTH_ERROR");
        assert!(b.create_feed(&AuthToken::new(" ")).is_err());
    }

    #[tokio::test]
    async fn poller_publishes_changes_and_stops() {
        let (b, _) = broker(false).await;
        let auth = AuthToken::new("good");
        let mut rx = b
            .start_order_updates(&auth, std::time::Duration::from_secs(1))
            .unwrap();
        assert!(b.order_updates_running());
        // Seed poll publishes nothing.
        let r = tokio::time::timeout(std::time::Duration::from_millis(1500), rx.recv()).await;
        assert!(r.is_err());
        b.stop_order_updates();
        assert!(!b.order_updates_running());
        // The task is gone, so the channel closes.
        let end = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await;
        assert!(matches!(end, Ok(None)));
    }
}

/// Web test_groww_positions_price.py (#2173): every position price is the
/// rupees Groww sent; no paise conversion, no step at 1000.
#[test]
fn position_prices_are_the_rupees_groww_sent() {
    let r = master();
    let pos = |v: serde_json::Value| -> Position {
        let mut base = serde_json::json!({
            "trading_symbol": "SBIN", "exchange": "NSE", "segment": "CASH",
            "product": "CNC", "credit_quantity": 1, "debit_quantity": 1,
            "net_price": 433.0, "credit_price": 433.0, "debit_price": 0.0
        });
        for (k, val) in v.as_object().unwrap() {
            base[k] = val.clone();
        }
        map_position(&serde_json::from_value(base).unwrap(), "CASH", &r)
    };
    for price in [0.05, 4.33, 433.0, 999.99, 1000.0, 1001.0, 25_000.5] {
        assert_eq!(
            pos(serde_json::json!({"net_price": price})).average_price,
            price
        );
        assert_eq!(
            pos(serde_json::json!({"credit_price": price})).buy_value,
            price
        );
        assert_eq!(
            pos(serde_json::json!({"debit_price": price})).sell_value,
            price
        );
    }
    let below = pos(serde_json::json!({"net_price": 1000.0})).average_price;
    let above = pos(serde_json::json!({"net_price": 1001.0})).average_price;
    assert!((above - below - 1.0).abs() < 1e-9, "no step in the scale");
    assert_eq!(pos(serde_json::json!({"debit_price": 0.0})).sell_value, 0.0);
    let nulls = pos(serde_json::json!({"credit_price": null, "debit_price": null}));
    assert_eq!((nulls.buy_value, nulls.sell_value), (0.0, 0.0));
    assert_eq!(
        pos(serde_json::json!({"net_price": "433.0"})).average_price,
        433.0
    );
}
