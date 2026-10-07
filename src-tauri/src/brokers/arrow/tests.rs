//! Arrow mapping tests against payloads built from the web plugin
//! (`src-tauri/tests/fixtures/brokers/arrow/`).

use super::data::{arrow_interval, index_candidates, parse_candles};
use super::funds::{parse_basket_margin, parse_order_margins, total_charges};
use super::mapping::*;
use super::master_contract::{classify_index_symbol, index_rows, norm, parse_instruments};
use super::orders::{modify_order_body, place_order_body};
use super::streaming::{decode_packet, ArrowFeed, ArrowOrderFeed};
use super::*;
use crate::brokers::common::mapping::{Action, PriceType, Validity};
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription, Message};
use serde_json::{json, Value};
use std::collections::HashSet;

const CSV: &str = include_str!("../../../tests/fixtures/brokers/arrow/instruments.csv");
const RESPONSES: &str = include_str!("../../../tests/fixtures/brokers/arrow/responses.json");

fn resp(key: &str) -> Value {
    let all: Value = serde_json::from_str(RESPONSES).unwrap();
    all[key].clone()
}

fn data(key: &str) -> Value {
    resp(key)["data"].clone()
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(parse_instruments(CSV).unwrap());
    r
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

#[test]
fn checksum_is_colon_separated_app_secret_token() {
    // sha256("app:secret:tok")
    let expected = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(b"app:secret:tok"))
    };
    assert_eq!(checksum("app", "secret", "tok"), expected);
    assert_ne!(
        checksum("app", "secret", "tok"),
        checksum("app", "tok", "secret")
    );
    assert_eq!(checksum("a", "b", "c").len(), 64);
}

#[test]
fn identity_and_capabilities() {
    let b = ArrowBroker::new(SymbolResolver::new());
    assert_eq!(b.id(), "arrow");
    assert_eq!(
        b.login_kind(),
        LoginKind::Redirect {
            param: "request-token"
        }
    );
    let c = b.capabilities();
    assert!(c.history && c.multiquotes_batch && c.margin && c.streaming && c.order_feed);
    assert!(!c.gtt);
    assert_eq!(c.depth_levels, &[5]);
    assert!(b.supported_exchanges().contains(&Exchange::Bcd));
    assert!(!b.supported_exchanges().contains(&Exchange::Nco));
    let keys: Vec<&str> = b.timeframe_map().iter().map(|(k, _)| *k).collect();
    assert_eq!(
        keys,
        ["1m", "3m", "5m", "10m", "15m", "30m", "1h", "2h", "3h", "4h", "D", "W", "M"]
    );
    // A token without the appID half is an expired session.
    assert!(b.create_feed(&AuthToken::new("jwtonly")).is_err());
    assert!(b.create_feed(&AuthToken::new("APP:jwt")).is_ok());
    assert!(b.create_order_feed(&AuthToken::new("APP:jwt")).is_ok());
}

// ---------------------------------------------------------------------------
// Vocabularies
// ---------------------------------------------------------------------------

#[test]
fn enum_maps_match_transform_data() {
    assert_eq!(order_type(PriceType::Market), "MKT");
    assert_eq!(order_type(PriceType::Limit), "LMT");
    assert_eq!(order_type(PriceType::Sl), "SL-LMT");
    assert_eq!(order_type(PriceType::SlM), "SL-MKT");
    for (a, b) in [
        ("MKT", "MARKET"),
        ("LMT", "LIMIT"),
        ("SL-LMT", "SL"),
        ("SL-MKT", "SL-M"),
    ] {
        assert_eq!(price_type_from_arrow(a), b);
    }
    assert_eq!(price_type_from_arrow("SL-M"), "SL-M");
    assert_eq!(product_code(Product::Cnc), "C");
    assert_eq!(product_code(Product::Nrml), "M");
    assert_eq!(product_code(Product::Mis), "I");
    assert_eq!(product_from_arrow("C"), "CNC");
    assert_eq!(product_from_arrow("M"), "NRML");
    assert_eq!(product_from_arrow("I"), "MIS");
    assert_eq!(product_from_arrow("X"), "X");
    assert_eq!(side_code(Action::Buy), "B");
    assert_eq!(side_from_arrow("S"), "SELL");
}

#[test]
fn statuses_are_lowercase_openalgo() {
    assert_eq!(map_status("COMPLETE"), "complete");
    assert_eq!(map_status("OPEN"), "open");
    assert_eq!(map_status("PENDING"), "open");
    assert_eq!(map_status("AFTER_MARKET_ORDER_REQ_RECEIVED"), "open");
    assert_eq!(map_status("TRIGGER_PENDING"), "trigger pending");
    assert_eq!(map_status("CANCELLED"), "cancelled");
    assert_eq!(map_status("REJECTED"), "rejected");
    assert_eq!(map_status("PARTIALLY_FILLED"), "partially_filled");
    assert!(
        is_cancellable("OPEN") && is_cancellable("PENDING") && is_cancellable("TRIGGER_PENDING")
    );
    assert!(!is_cancellable("COMPLETE"));
}

#[test]
fn exchange_codes_for_quotes_and_history() {
    assert_eq!(quote_exchange("NSE"), "NSE");
    assert_eq!(quote_exchange("BFO"), "BFO");
    assert_eq!(quote_exchange("MCX"), "MCXFO");
    assert_eq!(quote_exchange("NSE_INDEX"), "INDEX");
    assert_eq!(quote_exchange("BSE_INDEX"), "INDEX");
    assert!(quote_unsupported("CDS") && quote_unsupported("BCD") && quote_unsupported("NCO"));
    assert!(!quote_unsupported("MCX"));
    assert_eq!(history_exchange("NFO"), "nfo");
    assert_eq!(history_exchange("NSE_INDEX"), "nse");
    assert_eq!(history_exchange("BSE_INDEX"), "bse");
    assert_eq!(history_exchange("MCX"), "mcx");
    assert_eq!(arrow_interval("1h").unwrap(), "hour");
    assert_eq!(arrow_interval("D").unwrap(), "day");
    assert!(arrow_interval("2m")
        .unwrap_err()
        .client_message()
        .contains("not supported by Arrow"));
}

// ---------------------------------------------------------------------------
// Master contract
// ---------------------------------------------------------------------------

#[test]
fn master_contract_symbols_and_expiry() {
    let rows = parse_instruments(CSV).unwrap();
    // 20 data rows, one on an unknown segment is dropped.
    assert_eq!(rows.len(), 19);
    let r = master();
    let sbin = r.by_symbol("NSE", "SBIN").unwrap();
    assert_eq!(sbin.brsymbol, "SBIN-EQ");
    assert_eq!(sbin.brexchange, "NSECM");
    assert_eq!(sbin.token, "3045");
    assert_eq!(sbin.strike, 0.0);
    assert!(sbin.strike.is_sign_positive());
    assert_eq!(sbin.instrument_type, "EQ");
    assert_eq!(sbin.name, "STATE BANK OF INDIA");
    // Quoted name with a comma keeps the columns aligned.
    assert_eq!(r.by_symbol("NSE", "INFY").unwrap().name, "INFOSYS, LTD");
    // Only the -EQ series suffix is stripped.
    assert!(r.by_symbol("NSE", "749AP39-SG").is_some());
    assert!(r.by_symbol("BSE", "SBIN").is_some());

    let fut = r.by_symbol("NFO", "NIFTY27OCT26FUT").unwrap();
    assert_eq!(fut.brsymbol, "NIFTY27OCT26F");
    assert_eq!(fut.expiry, "27-OCT-26");
    assert_eq!(fut.instrument_type, "FUT");
    assert_eq!(fut.lot_size, 65);
    assert_eq!(fut.name, "NIFTY");
    let pe = r.by_symbol("NFO", "NIFTY06OCT2622400PE").unwrap();
    assert_eq!(pe.strike, 22400.0);
    assert_eq!(pe.expiry, "06-OCT-26");
    assert_eq!(pe.instrument_type, "PE");
    assert!(r.by_symbol("NFO", "VEDL27OCT26292.5CE").is_some());
    // Expiry on a derivative exchange without an option type is a future.
    assert_eq!(
        r.by_symbol("NFO", "RELIANCE27OCT26FUT")
            .unwrap()
            .instrument_type,
        "FUT"
    );
    assert!(r.by_symbol("BFO", "SENSEX30OCT26FUT").is_some());
    let crude = r.by_symbol("MCX", "CRUDEOIL19OCT26FUT").unwrap();
    assert_eq!(crude.brexchange, "MCXFO");
    assert_eq!(crude.lot_size, 100);
}

#[test]
fn master_contract_currency_scaling() {
    let r = master();
    let usd = r.by_symbol("CDS", "USDINR27OCT26FUT").unwrap();
    assert_eq!(usd.tick_size, 0.0025);
    assert_eq!(usd.strike, 0.0);
    let opt = r.by_symbol("CDS", "USDINR27OCT2683.5CE").unwrap();
    assert_eq!(opt.strike, 83.5);
    assert_eq!(opt.tick_size, 0.0025);
}

#[test]
fn master_contract_indices_are_renamed() {
    let r = master();
    let nifty = r.by_symbol("NSE_INDEX", "NIFTY").unwrap();
    assert_eq!(nifty.brsymbol, "Nifty 50");
    assert_eq!(nifty.instrument_type, "EQ");
    assert_eq!(nifty.brexchange, "NSEIDX");
    assert!(r.by_symbol("NSE_INDEX", "BANKNIFTY").is_some());
    assert!(r.by_symbol("NSE_INDEX", "INDIAVIX").is_some());
    assert!(r.by_symbol("NSE_INDEX", "NIFTYIT").is_some());
    assert!(r.by_symbol("BSE_INDEX", "SENSEX").is_some());
    assert!(r.by_symbol("BSE_INDEX", "BSEAUTO").is_some());
    assert!(r.by_symbol("MCX_INDEX", "MCXCRUDEX").is_some());

    assert_eq!(norm(" Nifty  Bank "), "NIFTYBANK");
    assert_eq!(
        classify_index_symbol("Nifty Fin Service", Some("NSE_INDEX")),
        ("FINNIFTY".to_string(), "NSE_INDEX")
    );
    // Unknown names keep their cleaned form.
    assert_eq!(
        classify_index_symbol("Nifty Something", Some("NSE_INDEX")).0,
        "NIFTYSOMETHING"
    );
    // Without an exchange: BSE codes go to BSE_INDEX, the rest to NSE.
    assert_eq!(
        classify_index_symbol("SNXT50", None),
        ("BSESENSEXNEXT50".to_string(), "BSE_INDEX")
    );
    assert_eq!(
        classify_index_symbol("Nifty 50", None),
        ("NIFTY".to_string(), "NSE_INDEX")
    );
}

#[test]
fn index_list_rows_merge_by_token() {
    let existing: HashSet<String> = ["26000".to_string()].into_iter().collect();
    let rows = index_rows(&data("index_list"), &existing);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].symbol, "NIFTYMIDCAP50");
    assert_eq!(rows[0].exchange, "NSE_INDEX");
    assert_eq!(rows[0].brexchange, "INDEX");
    assert_eq!(rows[0].brsymbol, "Nifty Midcap 50");
    assert_eq!(rows[1].symbol, "BSESENSEXNEXT50");
    assert_eq!(rows[1].token, "14");
    assert!(index_rows(&json!({"x": 1}), &existing).is_empty());
}

#[test]
fn master_contract_rejects_unknown_header() {
    let e = parse_instruments("a,b\n1,2\n").unwrap_err();
    assert!(e.client_message().contains("unexpected format"));
    assert!(parse_instruments("").is_err());
}

// ---------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------

fn resolved(symbol: &str, exchange: &str, pricetype: PriceType, price: f64) -> ResolvedOrder {
    let r = master();
    let instrument = r.by_symbol(exchange, symbol).unwrap();
    ResolvedOrder {
        symbol: symbol.into(),
        exchange: exchange.parse().unwrap(),
        action: Action::Buy,
        quantity: 65,
        price,
        trigger_price: 25110.0,
        pricetype,
        product: Product::Nrml,
        validity: Validity::Day,
        disclosed_quantity: 0,
        amo: false,
        instrument,
    }
}

#[test]
fn place_body_market_sets_mpp() {
    let b = place_order_body(&resolved("SBIN", "NSE", PriceType::Market, 0.0));
    assert_eq!(
        b,
        json!({
            "exchange": "NSE",
            "symbol": "SBIN-EQ",
            "quantity": "65",
            "transactionType": "B",
            "order": "MKT",
            "product": "M",
            "price": "0",
            "validity": "DAY",
            "disclosedQty": "0",
            "remarks": "openalgo",
            "mpp": true
        })
    );
}

#[test]
fn place_body_stop_carries_trigger_price() {
    let b = place_order_body(&resolved("NIFTY27OCT26FUT", "NFO", PriceType::Sl, 25100.5));
    assert_eq!(b["symbol"], "NIFTY27OCT26F");
    assert_eq!(b["order"], "SL-LMT");
    assert_eq!(b["price"], "25100.5");
    assert_eq!(b["triggerPrice"], "25110");
    assert!(b.get("mpp").is_none());
    let l = place_order_body(&resolved("SBIN", "NSE", PriceType::Limit, 800.0));
    assert!(l.get("triggerPrice").is_none());
}

#[test]
fn modify_body_has_no_side() {
    let r = master();
    let m = ResolvedModify {
        order_id: "26100300003".into(),
        symbol: "SBIN".into(),
        exchange: Exchange::Nse,
        action: Action::Sell,
        product: Product::Cnc,
        pricetype: PriceType::SlM,
        quantity: 5,
        price: 0.0,
        trigger_price: 790.5,
        disclosed_quantity: 1,
        instrument: r.by_symbol("NSE", "SBIN").unwrap(),
    };
    let b = modify_order_body(&m);
    assert_eq!(
        b,
        json!({
            "exchange": "NSE",
            "symbol": "SBIN-EQ",
            "quantity": "5",
            "order": "SL-MKT",
            "product": "C",
            "price": "0",
            "validity": "DAY",
            "disclosedQty": "1",
            "triggerPrice": "790.5"
        })
    );
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

#[test]
fn order_book_is_normalised_to_openalgo() {
    let r = master();
    let orders = map_orders(rows(data("orders"), "order"), &r);
    assert_eq!(orders.len(), 7);
    let o = &orders[0];
    assert_eq!(o.order_id, "26100300001");
    assert_eq!(o.symbol, "SBIN");
    assert_eq!(o.side, "BUY");
    assert_eq!(o.status, "complete");
    assert_eq!(o.order_type, "MARKET");
    assert_eq!(o.product, "MIS");
    assert_eq!(o.average_price, 812.45);
    assert_eq!(o.exchange_order_id.as_deref(), Some("1100000012345"));
    assert_eq!(o.order_timestamp, "2026-10-03T09:20:11");
    // `id` stands in for a missing orderNo; requestTime for orderTime.
    let sl = &orders[1];
    assert_eq!(sl.order_id, "26100300002");
    assert_eq!(sl.symbol, "NIFTY27OCT26FUT");
    assert_eq!(sl.status, "trigger pending");
    assert_eq!(sl.order_type, "SL");
    assert_eq!(sl.product, "NRML");
    assert_eq!(sl.trigger_price, 25110.0);
    assert_eq!(sl.order_timestamp, "2026-10-03T09:21:00");
    let open = &orders[2];
    assert_eq!(
        (
            open.status.as_str(),
            open.filled_quantity,
            open.pending_quantity
        ),
        ("open", 2, 3)
    );
    assert_eq!(open.product, "CNC");
    // Numbers as numbers parse too.
    assert_eq!(orders[3].status, "open");
    assert_eq!(orders[3].order_type, "SL-M");
    assert_eq!(orders[3].symbol, "NIFTY06OCT2622400PE");
    assert_eq!(orders[3].price, 12.5);
    assert_eq!(orders[4].status, "cancelled");
    let rej = &orders[5];
    assert_eq!(rej.status, "rejected");
    assert_eq!(
        rej.rejection_reason.as_deref(),
        Some("RMS: Insufficient funds")
    );
    // Unknown instruments keep the broker symbol rather than vanish.
    assert_eq!(orders[6].symbol, "UNKNOWN-EQ");
    assert_eq!(orders[6].status, "open");
}

#[test]
fn trade_book_uses_fill_fields_with_fallbacks() {
    let r = master();
    let t = map_trades(rows(data("trades"), "trade"), &r);
    assert_eq!(t.len(), 2);
    assert_eq!(t[0].symbol, "SBIN");
    assert_eq!(t[0].trade_id, "5001");
    assert_eq!(t[0].quantity, 10);
    assert_eq!(t[0].average_price, 812.45);
    assert!((t[0].trade_value - 8124.5).abs() < 1e-9);
    assert_eq!(t[0].product, "MIS");
    assert_eq!(t[0].timestamp, "2026-10-03T09:20:12");
    assert_eq!(t[1].order_id, "26100300010");
    assert_eq!(t[1].symbol, "NIFTY06OCT2622400PE");
    assert_eq!(t[1].side, "SELL");
    assert_eq!(t[1].quantity, 65);
    assert_eq!(t[1].average_price, 14.2);
    assert_eq!(t[1].timestamp, "2026-10-03T10:00:00");
}

#[test]
fn positions_and_holdings() {
    let r = master();
    let p = map_positions(rows(data("positions"), "position"), &r);
    assert_eq!(p.len(), 3);
    assert_eq!(p[0].symbol, "SBIN");
    assert_eq!(p[0].product, "MIS");
    assert_eq!(p[0].quantity, 10);
    assert_eq!(p[0].average_price, 812.45);
    assert_eq!(p[0].pnl, 26.46);
    assert_eq!(p[1].symbol, "NIFTY27OCT26FUT");
    assert_eq!(p[1].quantity, -65);
    assert_eq!(p[1].product, "NRML");
    assert_eq!(p[2].pnl, 120.5);

    let h = map_holdings(rows(data("holdings"), "holding"), &r);
    assert_eq!(h.len(), 2);
    assert_eq!(h[0].symbol, "SBIN");
    assert_eq!(h[0].exchange, "NSE");
    assert_eq!(h[0].product, "CNC");
    assert_eq!(h[0].isin.as_deref(), Some("INE062A01020"));
    assert_eq!(h[0].quantity, 20);
    assert_eq!(h[0].pnl, 2302.0);
    assert_eq!(h[0].pnl_percentage, 16.44);
    assert_eq!(h[1].symbol, "");
    assert_eq!(h[1].pnl_percentage, 0.0);
    let stats = PortfolioStats::from_holdings(&h);
    assert_eq!(stats.totalinvvalue, 14000.0);
}

#[test]
fn bad_rows_are_skipped_not_fatal() {
    let v = json!([{"symbol": "SBIN-EQ", "qty": "1"}, "nonsense", {"symbol": {"x": 1}}]);
    let p: Vec<ArrowPosition> = rows(v, "position");
    assert_eq!(p.len(), 2);
    assert!(rows::<ArrowPosition>(Value::Null, "position").is_empty());
}

// ---------------------------------------------------------------------------
// Funds and margin
// ---------------------------------------------------------------------------

#[test]
fn funds_from_user_limits() {
    let f = funds_from_limits(&data("limits"));
    assert_eq!(f.available_cash, 200000.25);
    assert_eq!(f.collateral, 15000.5);
    assert_eq!(f.m2m_unrealized, 1250.75);
    assert_eq!(f.m2m_realized, -300.0);
    assert_eq!(f.utilised_debits, 50000.25);
    assert_eq!(funds_from_limits(&Value::Null), Funds::default());
}

#[test]
fn margin_bodies_and_parsing() {
    let r = master();
    let legs = vec![
        MarginLeg {
            key: QuoteKey::new("NFO", "NIFTY06OCT2622400PE"),
            action: Action::Sell,
            quantity: 65,
            product: Product::Nrml,
            pricetype: PriceType::Market,
            price: 0.0,
            trigger_price: 0.0,
        },
        MarginLeg {
            key: QuoteKey::new("NSE", "SBIN"),
            action: Action::Buy,
            quantity: 1,
            product: Product::Mis,
            pricetype: PriceType::Sl,
            price: 800.5,
            trigger_price: 801.0,
        },
        MarginLeg {
            key: QuoteKey::new("NSE", "NOPE"),
            action: Action::Buy,
            quantity: 1,
            product: Product::Cnc,
            pricetype: PriceType::Limit,
            price: 1.0,
            trigger_price: 0.0,
        },
    ];
    let b = margin_bodies(&legs, &r);
    assert_eq!(b.len(), 2);
    assert_eq!(
        b[0],
        json!({"exchange": "NFO", "symbol": "NIFTY06OCT2622400P", "quantity": "65", "product": "M",
               "price": "0", "transactionType": "S", "order": "MKT"})
    );
    assert_eq!(b[1]["order"], "LMT");
    assert_eq!(b[1]["product"], "I");
    assert_eq!(b[1]["price"], "800.5");

    let one = parse_order_margins(&[data("margin_order")]);
    assert_eq!(one.total_margin_required, 152340.55);
    assert_eq!((one.span_margin, one.exposure_margin), (0.0, 0.0));
    let basket = parse_basket_margin(&data("margin_basket"));
    assert_eq!(basket.total_margin_required, 206512.75);
    assert_eq!(
        total_charges(data("margin_basket")["orders"].as_array().unwrap()),
        41.5
    );
}

// ---------------------------------------------------------------------------
// Quotes and history
// ---------------------------------------------------------------------------

#[test]
fn quote_and_depth_are_descaled() {
    let k = QuoteKey::new("NSE", "SBIN");
    let q = format_quote(&k, &data("quote_full"));
    assert_eq!(q.ltp, 815.1);
    assert_eq!(q.open, 809.0);
    assert_eq!(q.close, 810.0);
    assert_eq!(q.bid, 815.05);
    assert_eq!(q.ask, 815.15);
    assert_eq!((q.bid_qty, q.ask_qty), (100, 80));
    assert_eq!(q.volume, 1234567);
    assert_eq!(q.change, 5.1);
    assert_eq!(q.change_percent, 0.63);
    let d = to_depth(&k, &data("quote_full"));
    assert_eq!(d.bids.len(), 5);
    assert_eq!(d.asks.len(), 5);
    assert_eq!(d.bids[1].price, 815.0);
    assert_eq!(d.bids[1].orders, 5);
    assert_eq!(d.asks[1], DepthLevel::default());
    assert_eq!(d.prev_close, 810.0);
    assert_eq!(
        (d.ltq, d.total_buy_qty, d.total_sell_qty),
        (25, 50000, 42000)
    );
    // Empty book: bid/ask zero.
    let q = format_quote(&k, &json!({"ltp": 100}));
    assert_eq!((q.ltp, q.bid, q.ask), (1.0, 0.0, 0.0));
}

#[test]
fn index_quote_candidates_in_web_order() {
    assert_eq!(
        index_candidates("NIFTY", "Nifty 50"),
        ["NIFTY", "NIFTY 50", "Nifty 50"]
    );
    assert_eq!(index_candidates("SENSEX", "SENSEX"), ["SENSEX"]);
}

#[test]
fn candles_epoch_scaling_and_daily_shift() {
    let rows = resp("history_minute");
    let c = parse_candles(rows.as_array().unwrap(), false, true);
    assert_eq!(c.len(), 3);
    // 2026-10-01 09:16 IST.
    assert_eq!(c[0].timestamp, 1_790_826_360);
    assert_eq!(c[0].open, 811.0);
    assert_eq!(c[0].close, 812.5);
    assert_eq!(c[0].volume, 900);
    assert_eq!(c[0].oi, 12000);
    let no_oi = parse_candles(rows.as_array().unwrap(), false, false);
    assert_eq!(no_oi[0].oi, 0);
    let day = parse_candles(resp("history_day").as_array().unwrap(), true, false);
    // IST midnight + 5:30 lands on 00:00 UTC of the session date.
    assert_eq!(day[0].timestamp, 1_790_726_400);
    assert_eq!(day[1].close, 815.1);
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

fn sub(symbol: &str, exchange: &str, token: &str, mode: FeedMode) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: exchange.into(),
        token: token.into(),
        brsymbol: symbol.into(),
        brexchange: exchange.into(),
        mode,
        depth: 5,
    }
}

fn texts(frames: Vec<Message>) -> Vec<Value> {
    frames
        .into_iter()
        .map(|m| match m {
            Message::Text(t) => serde_json::from_str(&t).unwrap(),
            other => panic!("unexpected frame {:?}", other),
        })
        .collect()
}

#[test]
fn subscribe_frames_key_the_array_by_mode() {
    let mut f = ArrowFeed::new("wss://ds.arrow.trade", "APP", "jwt.x.y");
    let req = f.ws_request().unwrap();
    assert_eq!(
        req.uri().to_string(),
        "wss://ds.arrow.trade/?appID=APP&token=jwt.x.y"
    );
    let frames = texts(f.subscribe_frames(&[
        sub("NIFTY", "NSE_INDEX", "26000", FeedMode::Ltp),
        sub("SBIN", "NSE", "3045", FeedMode::Quote),
        sub("INFY", "NSE", "1594", FeedMode::Quote),
        sub("NIFTY27OCT26FUT", "NFO", "35001", FeedMode::Depth),
        sub("BAD", "NSE", "x", FeedMode::Ltp),
    ]));
    assert_eq!(
        frames,
        [
            json!({"code": "sub", "mode": "ltpc", "ltpc": [26000]}),
            json!({"code": "sub", "mode": "quote", "quote": [3045, 1594]}),
            json!({"code": "sub", "mode": "full", "full": [35001]}),
        ]
    );
    // Unsubscribe mirrors the subscribed mode.
    let un = texts(f.unsubscribe_frames(&[sub("SBIN", "NSE", "3045", FeedMode::Ltp)]));
    assert_eq!(
        un,
        [json!({"code": "unsub", "mode": "quote", "quote": [3045]})]
    );
    let hb = f.heartbeat().unwrap();
    assert_eq!(hb.0, std::time::Duration::from_secs(3));
    assert_eq!(hb.1, Message::Text("PONG".into()));
}

#[test]
fn subscribe_batches_of_one_hundred() {
    let mut f = ArrowFeed::new("wss://x", "A", "B");
    let subs: Vec<FeedSubscription> = (0..250)
        .map(|i| sub(&format!("S{}", i), "NSE", &i.to_string(), FeedMode::Quote))
        .collect();
    let frames = texts(f.subscribe_frames(&subs));
    let sizes: Vec<usize> = frames
        .iter()
        .map(|v| v["quote"].as_array().unwrap().len())
        .collect();
    assert_eq!(sizes, [100, 100, 50]);
}

fn put_u32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_be_bytes());
}

fn put_u64(b: &mut [u8], o: usize, v: u64) {
    b[o..o + 8].copy_from_slice(&v.to_be_bytes());
}

/// A quote-layout packet of `len` bytes (93 quote, 241/249 full).
fn quote_packet(len: usize, token: u32) -> Vec<u8> {
    let mut b = vec![0u8; len];
    put_u32(&mut b, 0, token);
    put_u32(&mut b, 4, 81510); // ltp 815.10
    b[8] = 1; // change flag (ignored)
    put_u32(&mut b, 9, 510); // net change (ignored)
    put_u32(&mut b, 13, 25); // ltq
    put_u32(&mut b, 17, 81234); // avg
    put_u64(&mut b, 21, 50000); // tbq
    put_u64(&mut b, 29, 42000); // tsq
    put_u32(&mut b, 37, 80900); // open
    put_u32(&mut b, 41, 81800); // high
    put_u32(&mut b, 45, 81000); // close (before low)
    put_u32(&mut b, 49, 80650); // low
    put_u64(&mut b, 53, 1_234_567); // volume
    put_u32(&mut b, 61, 1_759_290_360); // ltt
    put_u32(&mut b, 65, 1_759_290_361); // feed time
    put_u64(&mut b, 69, 1_500_000); // oi
    put_u64(&mut b, 77, 1_600_000); // oi day high
    put_u64(&mut b, 85, 1_400_000); // oi day low
    if len >= 241 {
        put_u32(&mut b, 93, 73000); // lower limit
        put_u32(&mut b, 97, 89000); // upper limit
        let off = if len >= 249 { 109 } else { 101 };
        for i in 0..10usize {
            let o = off + i * 14;
            put_u64(&mut b, o, 100 + i as u64);
            put_u32(&mut b, o + 8, 81500 + i as u32 * 5);
            b[o + 12..o + 14].copy_from_slice(&(i as u16 + 1).to_be_bytes());
        }
    }
    b
}

#[test]
fn decodes_ltp_and_ltpc_packets() {
    let mut ltp = vec![0u8; 13];
    put_u32(&mut ltp, 0, 26000);
    put_u32(&mut ltp, 4, 2_501_235);
    let p = decode_packet(&ltp).unwrap();
    assert_eq!((p.token, p.ltp, p.close), (26000, 25012.35, None));
    let mut ltpc = vec![0u8; 17];
    put_u32(&mut ltpc, 0, 26000);
    put_u32(&mut ltpc, 4, 2_501_235);
    put_u32(&mut ltpc, 13, 2_495_000);
    let p = decode_packet(&ltpc).unwrap();
    assert_eq!(p.close, Some(24950.0));
    assert!(!p.has_quote);
    assert!(decode_packet(&[0u8; 12]).is_none());
}

#[test]
fn decodes_quote_packet_offsets() {
    let p = decode_packet(&quote_packet(93, 3045)).unwrap();
    assert_eq!(p.token, 3045);
    assert_eq!(p.ltp, 815.1);
    assert_eq!(p.ltq, 25);
    assert_eq!(p.average_price, 812.34);
    assert_eq!(
        (p.total_buy_quantity, p.total_sell_quantity),
        (50000, 42000)
    );
    assert_eq!((p.open, p.high, p.low), (809.0, 818.0, 806.5));
    assert_eq!(p.close, Some(810.0));
    assert_eq!(p.volume, 1_234_567);
    assert_eq!(p.ltt, 1_759_290_360);
    assert_eq!(p.feed_time, 1_759_290_361);
    assert_eq!(p.oi, 1_500_000);
    assert!(p.depth.is_none());
}

#[test]
fn decodes_full_packets_both_layouts() {
    for len in [249usize, 241] {
        let p = decode_packet(&quote_packet(len, 35001)).unwrap();
        assert_eq!((p.lower_limit, p.upper_limit), (730.0, 890.0), "{}", len);
        let (buy, sell) = p.depth.unwrap();
        assert_eq!(buy.len(), 5);
        assert_eq!(sell.len(), 5);
        assert_eq!(
            buy[0],
            DepthLevel {
                price: 815.0,
                quantity: 100,
                orders: 1
            }
        );
        assert_eq!(
            sell[0],
            DepthLevel {
                price: 815.25,
                quantity: 105,
                orders: 6
            }
        );
        assert_eq!(sell[4].orders, 10);
    }
}

#[test]
fn feed_emits_ticks_under_the_subscription() {
    let mut f = ArrowFeed::new("wss://x", "A", "B");
    f.subscribe_frames(&[
        sub("NIFTY27OCT26FUT", "NFO", "35001", FeedMode::Depth),
        sub("NIFTY", "NSE_INDEX", "26000", FeedMode::Ltp),
    ]);
    let ev = f.parse(&Message::Binary(quote_packet(249, 35001)));
    assert_eq!(ev.len(), 2);
    let FeedEvent::Tick(t) = &ev[0] else {
        panic!("tick expected")
    };
    assert_eq!(t.symbol, "NIFTY27OCT26FUT");
    assert_eq!(t.exchange, "NFO");
    assert_eq!(t.mode, 3);
    assert_eq!(t.close, 810.0);
    assert_eq!(t.change, 5.1);
    assert_eq!(t.oi, 1_500_000);
    assert_eq!(t.timestamp_ms, 1_759_290_361_000);
    assert_eq!(t.last_trade_time_ms, 1_759_290_360_000);
    let FeedEvent::Depth(d) = &ev[1] else {
        panic!("depth expected")
    };
    assert_eq!(d.buy[0].price, 815.0);
    assert_eq!(d.total_buy_quantity, 50000);

    let mut ltpc = vec![0u8; 17];
    put_u32(&mut ltpc, 0, 26000);
    put_u32(&mut ltpc, 4, 2_501_235);
    put_u32(&mut ltpc, 13, 2_495_000);
    let ev = f.parse(&Message::Binary(ltpc));
    let FeedEvent::Tick(t) = &ev[0] else {
        panic!("tick expected")
    };
    assert_eq!(
        (t.exchange.as_str(), t.mode, t.ltp),
        ("NSE_INDEX", 1, 25012.35)
    );
    assert_eq!(t.change, 62.35);
    // Unsubscribed tokens are ignored.
    assert!(f.parse(&Message::Binary(quote_packet(93, 999))).is_empty());
}

#[test]
fn feed_text_errors() {
    let mut f = ArrowFeed::new("wss://x", "A", "B");
    let ev = f.parse(&Message::Text(
        r#"{"status":"error","message":"invalid token"}"#.into(),
    ));
    assert!(matches!(ev.as_slice(), [FeedEvent::AuthFailed(_)]));
    assert!(f
        .parse(&Message::Text(
            r#"{"status":"error","message":"bad mode"}"#.into()
        ))
        .is_empty());
    assert!(f.parse(&Message::Text("ok".into())).is_empty());
}

#[test]
fn order_updates_are_normalised() {
    let f = ArrowOrderFeed::new("wss://order-updates.arrow.trade", "APP", "jwt", master());
    let u = f.parse_text(&resp("order_update").to_string()).unwrap();
    assert_eq!(u.orderid, "26100300003");
    assert_eq!(u.symbol, "INFY");
    assert_eq!(u.action, "BUY");
    assert_eq!(u.order_status, "complete");
    assert_eq!(
        (u.quantity, u.filled_quantity, u.pending_quantity),
        (5, 5, 0)
    );
    assert_eq!(u.pricetype, "LIMIT");
    assert_eq!(u.product, "CNC");
    assert_eq!(u.average_price, 1499.85);
    assert_eq!(u.rejection_reason, "");
    let r = f
        .parse_text(&resp("order_update_rejected").to_string())
        .unwrap();
    assert_eq!(r.order_status, "rejected");
    assert_eq!(r.symbol, "NIFTY27OCT26FUT");
    assert_eq!(r.pricetype, "SL-M");
    assert_eq!(r.product, "NRML");
    // leavesQuantity absent: quantity - filled.
    assert_eq!(r.pending_quantity, 65);
    assert_eq!(r.rejection_reason, "RMS: Insufficient funds");
    assert!(f.parse_text("PING").is_none());
    assert!(f.parse_text(r#"{"updateType":"ORDER_UPDATE"}"#).is_none());
    assert!(f
        .parse_text(r#"{"id":"1","updateType":"TRADE_UPDATE"}"#)
        .is_none());
    let mut feed = ArrowOrderFeed::new("wss://x", "A", "B", master());
    assert!(feed.subscribe_frames(&[]).is_empty());
    assert_eq!(feed.heartbeat().unwrap().1, Message::Text("PONG".into()));
    assert!(matches!(
        feed.parse(&Message::Text(resp("order_update").to_string()))
            .as_slice(),
        [FeedEvent::OrderUpdate(_)]
    ));
}

// ---------------------------------------------------------------------------
// Secrets stay out of logged errors
// ---------------------------------------------------------------------------

const SENTINEL: &str = "SENTINEL-7f3a9c";

/// A loopback port with nothing listening (bound, then released).
fn closed_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

#[tokio::test]
async fn transport_errors_lose_their_url_before_logging() {
    let url = format!(
        "http://127.0.0.1:{}/oapi/v1/orders?api_key={s}&token={s}",
        closed_port(),
        s = SENTINEL
    );
    let raw = crate::brokers::common::http::client()
        .get(&url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .unwrap_err();
    // Precondition: the unredacted error does carry the secret.
    assert!(format!("{} {:?}", raw, raw).contains(SENTINEL));
    let e = super::redact(crate::error::AppError::from(raw));
    let shown = format!("{} {:?} {} {}", e, e, e.code(), e.client_message());
    assert!(!shown.contains(SENTINEL), "{}", shown);
    // Non-transport errors pass through untouched.
    let other = super::redact(crate::error::AppError::Broker("kept".into()));
    assert_eq!(other.client_message(), "kept");
}
