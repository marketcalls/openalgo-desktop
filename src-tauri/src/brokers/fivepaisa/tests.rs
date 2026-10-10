//! 5paisa mapping, master and feed tests against payloads shaped like the
//! web adapter's and the 5paisa docs (`tests/fixtures/brokers/fivepaisa/`).

use super::auth::{access_token_body, split_api_key, totp_login_body};
use super::data::{
    candle_in_range, chunk_days, history_path, interval_code, parse_candle, to_levels, to_quote,
};
use super::funds::to_funds;
use super::mapping::*;
use super::master_contract::{index_symbol, parse_csv, row_exchange};
use super::streaming::{feed_codes, feed_url, methods, redirect_server, FivepaisaFeed};
use super::*;
use crate::brokers::common::mapping::{Action, PriceType, Validity};
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription, Message};
use crate::brokers::common::symbols::SymToken;
use chrono::NaiveDate;
use serde_json::{json, Value};

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../../tests/fixtures/brokers/fivepaisa/", $name))
    };
}

fn resp(name: &str) -> Value {
    let all: Value = serde_json::from_str(fixture!("responses.json")).unwrap();
    all[name].clone()
}

fn feed_fixture(name: &str) -> String {
    let all: Value = serde_json::from_str(fixture!("feed.json")).unwrap();
    all[name].to_string()
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(parse_csv(fixture!("ScripMaster.csv")));
    r
}

fn rows(name: &str, key: &str) -> Vec<Value> {
    body_rows(&resp(name), key)
}

const JWT: &str =
    "eyJhbGciOiJIUzI1NiJ9.eyJSZWRpcmVjdFNlcnZlciI6IkIiLCJ1bmlxdWVfbmFtZSI6IjxVU0VSX0lEPiJ9.c2ln";

#[test]
fn session_round_trips_and_redacts() {
    let s = Session {
        api_key: "APPKEY".into(),
        client_code: "50001234".into(),
        access_token: JWT.into(),
    };
    let t = AuthToken::new(s.encode());
    let back = session(&t).unwrap();
    assert_eq!(back.api_key, "APPKEY");
    assert_eq!(back.client_code, "50001234");
    assert_eq!(back.access_token.expose(), JWT);
    let dbg = format!("{:?}", back);
    assert!(!dbg.contains("APPKEY") && !dbg.contains(JWT));
    assert!(session(&AuthToken::new("just-a-token")).is_err());
    assert!(session(&AuthToken::new(":::c:::t")).is_err());
}

#[test]
fn api_key_needs_three_parts() {
    let k = split_api_key(" key:::uid:::5000 ").unwrap();
    assert_eq!(
        (
            k.api_key.as_str(),
            k.user_id.as_str(),
            k.client_code.as_str()
        ),
        ("key", "uid", "5000")
    );
    for bad in ["key", "key:::uid", "a:::b:::c:::d", "a::::::c"] {
        let e = split_api_key(bad).unwrap_err();
        assert!(e.client_message().contains("api_key:::user_id:::client_id"));
    }
}

#[test]
fn login_bodies_match_the_web() {
    assert_eq!(
        totp_login_body("K", "a@b.c", "123456", "1111"),
        json!({"head":{"Key":"K"},"body":{"Email_ID":"a@b.c","TOTP":"123456","PIN":"1111"}})
    );
    assert_eq!(
        access_token_body("K", "rt", "enc", "uid"),
        json!({"head":{"Key":"K"},"body":{"RequestToken":"rt","EncryKey":"enc","UserId":"uid"}})
    );
    assert_eq!(
        envelope("K", json!({"ClientCode":"1"})),
        json!({"head":{"key":"K"},"body":{"ClientCode":"1"}})
    );
}

#[test]
fn enum_maps() {
    assert_eq!(action_code(Action::Buy), "B");
    assert_eq!(action_code(Action::Sell), "S");
    let pairs = [
        ("NSE", "N", "C"),
        ("BSE", "B", "C"),
        ("NFO", "N", "D"),
        ("BFO", "B", "D"),
        ("CDS", "N", "U"),
        ("BCD", "B", "U"),
        ("MCX", "M", "D"),
        ("NSE_INDEX", "N", "C"),
        ("BSE_INDEX", "B", "C"),
    ];
    for (ex, e, t) in pairs {
        assert_eq!((exch_code(ex), exch_type(ex)), (e, t), "{}", ex);
        if !ex.ends_with("_INDEX") {
            assert_eq!(reverse_exchange(e, t), Some(ex));
        }
    }
    assert_eq!(reverse_exchange("X", "C"), None);
    assert_eq!(product_code(Product::Cnc), "D");
    assert_eq!(product_code(Product::Nrml), "D");
    assert_eq!(product_code(Product::Mis), "I");
    assert_eq!(reverse_product("D", "NSE"), "CNC");
    assert_eq!(reverse_product("D", "NFO"), "NRML");
    assert_eq!(reverse_product("D", "MCX"), "NRML");
    assert_eq!(reverse_product("I", "BSE"), "MIS");
}

#[test]
fn statuses_follow_the_web_table() {
    let cases = [
        ("Fully Executed", "complete"),
        ("Pending", "open"),
        ("Modified", "open"),
        ("Placed", "open"),
        ("AH Placed", "open"),
        ("AH Modified", "open"),
        ("Xmitted", "open"),
        ("Cancelled", "cancelled"),
        ("AH Cancelled", "cancelled"),
        ("Rejected By 5P", "rejected"),
        ("Rejected by Exch    ", "rejected"),
        ("Partially Rejected", "rejected"),
        ("Cancel Pending", "cancelled"),
        ("Some New State", "some new state"),
        ("", ""),
    ];
    for (raw, want) in cases {
        assert_eq!(normalize_status(raw), want, "{}", raw);
    }
    assert_eq!(book_pricetype("Y", 0.0), "MARKET");
    assert_eq!(book_pricetype("N", 0.0), "LIMIT");
    assert_eq!(book_pricetype("Y", 10.0), "SL-M");
    assert_eq!(book_pricetype("N", 10.0), "SL");
    assert_eq!(book_pricetype("", 0.0), "LIMIT");
}

#[test]
fn ms_dates() {
    assert_eq!(ms_date("/Date(1791082800000+0530)/"), "2026-10-04 08:30:00");
    assert_eq!(ms_date("/Date(1791082800000)/"), "2026-10-04 03:00:00");
    assert_eq!(ms_date("garbage"), "");
    assert_eq!(ms_date_epoch("/Date(1791082800000)/"), 1791082800000);
    assert_eq!(ms_date_epoch("/Date(1791082800000+0530)/"), 1791082800000);
    assert_eq!(ms_date_epoch(""), 0);
}

#[test]
fn order_book_rows_carry_openalgo_symbols() {
    let r = master();
    let book: Vec<Order> = rows("order_book", "OrderBookDetail")
        .iter()
        .map(|x| to_order(&r, x))
        .collect();
    assert_eq!(book.len(), 5);
    let o = &book[0];
    assert_eq!(o.order_id, "402189541");
    assert_eq!(o.exchange_order_id.as_deref(), Some("1100000012345678"));
    assert_eq!((o.symbol.as_str(), o.exchange.as_str()), ("SBIN", "NSE"));
    assert_eq!((o.side.as_str(), o.product.as_str()), ("BUY", "MIS"));
    assert_eq!(
        (o.status.as_str(), o.order_type.as_str()),
        ("open", "LIMIT")
    );
    assert_eq!((o.quantity, o.pending_quantity, o.price), (10, 10, 812.5));
    assert_eq!(o.order_timestamp, "2026-10-04 08:30:00");
    let f = &book[1];
    assert_eq!(
        (f.symbol.as_str(), f.exchange.as_str()),
        ("NIFTY27OCT26FUT", "NFO")
    );
    assert_eq!(
        (f.product.as_str(), f.status.as_str()),
        ("NRML", "complete")
    );
    assert_eq!((f.order_type.as_str(), f.filled_quantity), ("MARKET", 75));
    let sl = &book[2];
    assert_eq!((sl.product.as_str(), sl.order_type.as_str()), ("CNC", "SL"));
    assert_eq!(sl.trigger_price, 1399.0);
    let rej = &book[3];
    assert_eq!(
        (rej.status.as_str(), rej.order_type.as_str()),
        ("rejected", "SL-M")
    );
    assert_eq!(
        rej.rejection_reason.as_deref(),
        Some("Market order with Algo Id not allowed")
    );
    assert_eq!(rej.exchange_order_id, None);
    let mcx = &book[4];
    assert_eq!(
        (mcx.symbol.as_str(), mcx.exchange.as_str()),
        ("CRUDEOIL19NOV26FUT", "MCX")
    );
    assert_eq!(mcx.status, "cancelled");
}

#[test]
fn unknown_scrip_keeps_the_broker_name() {
    let r = SymbolResolver::new();
    let o = to_order(&r, &rows("order_book", "OrderBookDetail")[0]);
    assert_eq!((o.symbol.as_str(), o.exchange.as_str()), ("SBIN", "NSE"));
}

/// Web fivepaisa `_position_book_ok` and `read_position_book` (#2116):
/// a smart order reads the book only when 5 Paisa confirmed it, or when its
/// message says it is empty; anything else refuses the order.
#[test]
fn position_book_must_be_confirmed() {
    use super::orders::{positions_ok, strict_rows};
    assert!(positions_ok(&resp("positions")));
    // Rows confirm a read whatever the head says.
    assert!(positions_ok(
        &json!({"head":{"statusDescription":"Failure"},"body":{"Status":1,"NetPositionDetail":[{"NetQty":5}]}})
    ));
    let empty =
        json!({"head":{"statusDescription":"Success"},"body":{"Status":0,"NetPositionDetail":[]}});
    assert!(strict_rows(&empty).unwrap().is_empty());
    let failed =
        json!({"head":{"statusDescription":"Failure"},"body":{"Status":1,"NetPositionDetail":[]}});
    let e = strict_rows(&failed).unwrap_err();
    assert_eq!(
        e.client_message(),
        "OpenAlgo could not read your open position from 5 Paisa, so no order was sent. Check your positions and try again."
    );
    let said_empty = json!({"head":{"statusDescription":"Failure"},"body":{"Status":1,"Message":"No Data Found"}});
    assert!(strict_rows(&said_empty).unwrap().is_empty());
    // Python `int(body.get("Status", 0)) == 0`.
    let head = |body: Value| json!({"head":{"statusDescription":"Success"},"body": body});
    assert!(positions_ok(&head(json!({}))));
    assert!(positions_ok(&head(json!({"Status":"0"}))));
    assert!(!positions_ok(&head(json!({"Status":"x"}))));
    assert!(!positions_ok(&head(json!({"Status":null}))));
    assert!(!positions_ok(&head(json!({"Status":2}))));
    assert!(!positions_ok(
        &json!({"head":{"statusDescription":"Success"}})
    ));
    assert!(!positions_ok(&json!({})));
}

#[test]
fn trades_positions_holdings() {
    let r = master();
    let t: Vec<Trade> = rows("trade_book", "TradeBookDetail")
        .iter()
        .map(|x| to_trade(&r, x))
        .collect();
    assert_eq!(t[0].symbol, "NIFTY27OCT26FUT");
    assert_eq!(
        (t[0].side.as_str(), t[0].product.as_str()),
        ("SELL", "NRML")
    );
    assert_eq!(t[0].order_id, "1100000012345679");
    assert_eq!(t[0].trade_value, 1875776.25);
    assert_eq!(
        (t[1].symbol.as_str(), t[1].product.as_str()),
        ("RELIANCE", "CNC")
    );
    assert_eq!(t[1].trade_value, 4200.45);

    let p: Vec<Position> = rows("positions", "NetPositionDetail")
        .iter()
        .map(|x| to_position(&r, x))
        .collect();
    assert_eq!(
        (p[0].symbol.as_str(), p[0].quantity),
        ("NIFTY27OCT26FUT", -75)
    );
    assert_eq!(
        (p[0].average_price, p[0].product.as_str()),
        (25010.35, "NRML")
    );
    assert_eq!((p[0].realized_pnl, p[0].unrealized_pnl), (1250.5, 776.25));
    assert_eq!(
        (p[1].product.as_str(), p[1].average_price),
        ("MIS", 1400.15)
    );
    assert_eq!(p[2].quantity, 0);
    assert!(rows("positions_null", "NetPositionDetail").is_empty());

    let h: Vec<Holding> = rows("holdings", "Data")
        .iter()
        .map(|x| to_holding(&r, x))
        .collect();
    assert_eq!(
        (h[0].symbol.as_str(), h[0].exchange.as_str()),
        ("SBIN", "NSE")
    );
    assert_eq!((h[0].pnl, h[0].pnl_percentage), (2240.0, 16.0));
    assert_eq!(h[0].product, "CNC");
    assert_eq!(
        (h[1].symbol.as_str(), h[1].exchange.as_str()),
        ("RELIANCE", "BSE")
    );
    assert_eq!((h[1].pnl, h[1].pnl_percentage), (-200.0, -6.67));
}

#[test]
fn funds_match_the_web_formula() {
    let eq = rows("margin", "EquityMargin")[0].clone();
    let f = to_funds(&eq, &rows("positions", "NetPositionDetail"));
    assert_eq!(f.available_cash, 100487.24);
    assert_eq!(f.collateral, 5000.0);
    assert_eq!(f.utilised_debits, 24512.76);
    assert_eq!(f.m2m_unrealized, 781.8);
    assert_eq!(f.m2m_realized, 1230.5);
    let flat = to_funds(&eq, &[]);
    assert_eq!((flat.m2m_realized, flat.m2m_unrealized), (0.0, 0.0));
}

fn order(symbol: &str, exchange: &str, pricetype: &str, product: &str) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: "BUY".into(),
        quantity: 10,
        price: 812.0,
        order_type: pricetype.into(),
        product: product.into(),
        validity: "DAY".into(),
        trigger_price: Some(810.0),
        disclosed_quantity: Some(0),
        amo: false,
    };
    ResolvedOrder::resolve(&req, &master()).unwrap()
}

#[test]
fn place_body_matches_the_web_literal() {
    let o = order("SBIN", "NSE", "LIMIT", "MIS");
    assert_eq!(o.validity, Validity::Day);
    assert_eq!(
        place_body(&o, 812.0),
        json!({"OrderType":"B","Exchange":"N","ExchangeType":"C","ScripCode":"3045",
               "Price":812.0,"Qty":10,"StopLossPrice":810.0,"DisQty":0,"IsIntraday":true,
               "AHPlaced":"N","RemoteOrderID":"OpenAlgo"})
    );
    let f = order("NIFTY27OCT26FUT", "NFO", "LIMIT", "NRML");
    let b = place_body(&f, 25000.0);
    assert_eq!(
        (b["Exchange"].as_str(), b["ExchangeType"].as_str()),
        (Some("N"), Some("D"))
    );
    assert_eq!(b["IsIntraday"], json!(false));
    assert_eq!(f.pricetype, PriceType::Limit);
}

#[test]
fn slm_is_a_protected_stop_limit() {
    assert_eq!(
        slm_protected_price("SBIN", Action::Sell, 810.0, 0.05).unwrap(),
        805.95
    );
    assert_eq!(
        slm_protected_price("SBIN", Action::Buy, 100.0, 0.05).unwrap(),
        101.0
    );
    // At least one tick beyond the trigger even when the slab is smaller.
    assert_eq!(
        slm_protected_price("NIFTY27OCT2625000CE", Action::Buy, 1.0, 0.05).unwrap(),
        1.05
    );
    assert!(slm_protected_price("SBIN", Action::Sell, 0.05, 0.05).is_err());
    assert!(slm_protected_price("SBIN", Action::Sell, 810.0, 0.0).is_err());
}

#[test]
fn placement_acceptance_needs_status_and_order_id() {
    assert_eq!(placed_order_id(&resp("place_ok")), Ok("402189542".into()));
    assert_eq!(
        placed_order_id(&resp("place_rejected")),
        Err("Insufficient funds in your account".into())
    );
    let no_id =
        json!({"head":{"statusDescription":"Success"},"body":{"Status":0,"BrokerOrderID":0}});
    assert_eq!(
        placed_order_id(&no_id),
        Err("Order rejected by 5Paisa".into())
    );
    let string_status =
        json!({"head":{"statusDescription":"Success"},"body":{"Status":"0","BrokerOrderID":"77"}});
    assert_eq!(placed_order_id(&string_status), Ok("77".into()));
}

#[test]
fn master_contract_rows() {
    let rows = parse_csv(fixture!("ScripMaster.csv"));
    let r = master();
    let sbin = r.by_symbol("NSE", "SBIN").unwrap();
    assert_eq!(
        (sbin.brsymbol.as_str(), sbin.token.as_str(), sbin.tick_size),
        ("SBIN", "3045", 0.05)
    );
    assert_eq!(sbin.instrument_type, "EQ");
    assert_eq!(r.by_symbol("NSE", "RELIANCE").unwrap().brsymbol, "RELIANCE");
    // BE-series (trade-for-trade) cash rows are equity (web #2195).
    assert_eq!(r.by_symbol("NSE", "YESBANK").unwrap().instrument_type, "EQ");
    assert!(r.by_symbol("NSE", "SKIPPED").is_none());
    assert_eq!(r.by_symbol("BSE", "SBIN").unwrap().token, "500112");

    let nifty = r.by_symbol("NSE_INDEX", "NIFTY").unwrap();
    assert_eq!(
        (nifty.token.as_str(), nifty.brexchange.as_str()),
        ("999920000", "NSE_INDEX")
    );
    assert_eq!(nifty.name, "NIFTY");
    assert!(r.by_symbol("NSE_INDEX", "BANKNIFTY").is_some());
    assert!(r.by_symbol("NSE_INDEX", "MIDCPNIFTY").is_some());
    assert!(r.by_symbol("BSE_INDEX", "SENSEX").is_some());
    assert!(r.by_symbol("BSE_INDEX", "BANKEX").is_some());
    // The duplicate NIFTY-50 index row is dropped (first wins).
    assert!(r.by_token("NSE_INDEX", "999920043").is_none());
    // Index rows follow the others.
    assert!(rows.last().unwrap().exchange.ends_with("_INDEX"));

    let fut = r.by_symbol("NFO", "NIFTY27OCT26FUT").unwrap();
    assert_eq!(
        (
            fut.expiry.as_str(),
            fut.lot_size,
            fut.instrument_type.as_str()
        ),
        ("27-OCT-26", 75, "FUT")
    );
    assert_eq!(fut.name, "NIFTY");
    assert_eq!(fut.brsymbol, "NIFTY 27 OCT 2026");
    let ce = r.by_symbol("NFO", "NIFTY27OCT2625000CE").unwrap();
    assert_eq!((ce.strike, ce.instrument_type.as_str()), (25000.0, "CE"));
    let pe = r.by_symbol("NFO", "NIFTY27OCT2624950.5PE").unwrap();
    assert_eq!(
        (pe.token.as_str(), pe.instrument_type.as_str()),
        ("40001", "PE")
    );
    // A blank Series is not the two-space series: dropped like pandas NaN.
    assert!(r.by_token("NFO", "40002").is_none());
    assert!(r.by_symbol("BFO", "SENSEX29OCT26FUT").is_some());
    let crude = r.by_symbol("MCX", "CRUDEOIL19NOV26FUT").unwrap();
    assert_eq!((crude.lot_size, crude.tick_size), (100, 1.0));
    assert!(r.by_symbol("CDS", "USDINR27OCT26FUT").is_some());
    assert_eq!(sbin.expiry, "01-JAN-80");
}

#[test]
fn master_helpers() {
    assert_eq!(row_exchange("N", "C", 999920000), Some("NSE_INDEX"));
    assert_eq!(row_exchange("N", "C", 999900), Some("NSE"));
    assert_eq!(row_exchange("B", "C", 999901), Some("BSE_INDEX"));
    assert_eq!(row_exchange("M", "D", 1), Some("MCX"));
    assert_eq!(row_exchange("Z", "D", 1), None);
    assert_eq!(index_symbol("Nifty Fin Service"), "FINNIFTY");
    assert_eq!(index_symbol("NIFTY NEXT 50"), "NIFTYNXT50");
    assert_eq!(index_symbol("SNSX50"), "SENSEX50");
    assert_eq!(index_symbol("India VIX"), "INDIAVIX");
    assert!(parse_csv("").is_empty());
    assert!(parse_csv("Exch,ExchType\nN,C\n").is_empty());
}

#[test]
fn index_names_are_looked_up_on_the_index_exchange() {
    assert_eq!(query_exchange("NIFTY", "NSE"), "NSE_INDEX");
    assert_eq!(query_exchange("sensex", "BSE"), "BSE_INDEX");
    assert_eq!(query_exchange("NIFTYBEES", "NSE"), "NSE");
    assert_eq!(query_exchange("NIFTY", "NSE_INDEX"), "NSE_INDEX");
}

#[test]
fn quotes_and_depth() {
    let k = QuoteKey::new("NSE", "SBIN");
    let row = &rows("snapshot_sbin", "Data")[0];
    let q = to_quote(&k, row);
    assert_eq!(
        (q.ltp, q.open, q.high, q.low, q.close),
        (812.5, 808.0, 815.0, 805.0, 808.25)
    );
    assert_eq!(q.volume, 1520034);
    assert_eq!(q.change, 4.25);
    let fallback = to_quote(
        &k,
        &json!({"LastTradedPrice": 10, "PClose": 0, "PreviousClose": 0, "Close": 9.5}),
    );
    assert_eq!(fallback.close, 9.5);
    let (bids, asks, tb, ts) = to_levels(&rows("depth_sbin", "MarketDepthData"));
    assert_eq!(bids.len(), 5);
    assert_eq!(
        bids.iter().take(3).map(|l| l.price).collect::<Vec<_>>(),
        [812.45, 812.4, 812.3]
    );
    assert_eq!(asks[0].price, 812.55);
    assert_eq!((asks[0].orders, asks[2].price), (1, 0.0));
    assert_eq!((tb, ts), (470, 130));
}

#[test]
fn history_intervals_and_candles() {
    assert_eq!(
        TIMEFRAME_MAP.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
        ["1m", "5m", "10m", "15m", "30m", "1h", "D"]
    );
    assert_eq!(interval_code("1h"), Some("60m"));
    assert_eq!(interval_code("d"), Some("1d"));
    assert_eq!(interval_code("3m"), None);
    assert_eq!((chunk_days(true), chunk_days(false)), (100, 30));
    let d = |y, m, dd| NaiveDate::from_ymd_opt(y, m, dd).unwrap();
    assert_eq!(
        history_path("NSE", "3045", "1m", d(2026, 10, 1), d(2026, 10, 30)),
        "/V2/historical/N/C/3045/1m?from=2026-10-01&end=2026-10-30"
    );
    assert_eq!(
        history_path(
            "NSE_INDEX",
            "999920000",
            "1d",
            d(2026, 1, 1),
            d(2026, 4, 10)
        ),
        "/V2/historical/N/C/999920000/1d?from=2026-01-01&end=2026-04-10"
    );
    let candles = resp("history")["data"]["candles"]
        .as_array()
        .unwrap()
        .clone();
    let intraday: Vec<Candle> = candles
        .iter()
        .filter_map(|c| parse_candle(c, false, false))
        .collect();
    // All-zero and malformed rows dropped; quiet minutes kept.
    assert_eq!(intraday.len(), 3);
    assert_eq!(intraday[0].timestamp, 1790826300);
    assert_eq!(intraday[2].volume, 9000);
    let daily_rows = resp("history_daily")["data"]["candles"]
        .as_array()
        .unwrap()
        .clone();
    let daily: Vec<Candle> = daily_rows
        .iter()
        .filter_map(|c| parse_candle(c, true, false))
        .collect();
    assert_eq!(daily.len(), 2, "zero-volume flat day dropped");
    assert_eq!(daily[0].timestamp, 1790812800);
    let index: Vec<Candle> = daily_rows
        .iter()
        .filter_map(|c| parse_candle(c, true, true))
        .collect();
    assert_eq!(index.len(), 3, "indices keep zero-volume days");
}

fn sub(token: &str, brexchange: &str, mode: FeedMode) -> FeedSubscription {
    FeedSubscription {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        token: token.into(),
        brsymbol: "SBIN".into(),
        brexchange: brexchange.into(),
        mode,
        depth: 5,
    }
}

fn text_of(m: &Message) -> Value {
    match m {
        Message::Text(t) => serde_json::from_str(t).unwrap(),
        other => panic!("not text: {:?}", other),
    }
}

#[test]
fn feed_host_and_handshake() {
    assert_eq!(redirect_server(JWT), "B");
    assert_eq!(redirect_server("not.a.jwt.token"), "default");
    assert_eq!(redirect_server("x.!!!.y"), "default");
    assert_eq!(feed_url("B"), "wss://bopenfeed.5paisa.com/feeds/api/chat");
    assert_eq!(
        feed_url("default"),
        "wss://openfeed.5paisa.com/Feeds/api/chat"
    );
    let b = FivepaisaBroker::new(SymbolResolver::new());
    let auth = AuthToken::new(
        Session {
            api_key: "K".into(),
            client_code: "5000".into(),
            access_token: JWT.into(),
        }
        .encode(),
    );
    let f = b.create_feed(&auth).unwrap();
    let req = f.ws_request().unwrap();
    let uri = req.uri().to_string();
    assert!(uri.starts_with("wss://bopenfeed.5paisa.com/feeds/api/chat?Value1="));
    assert!(uri.ends_with("|5000"));
    assert!(f.heartbeat().is_some());
    assert!(b.create_feed(&AuthToken::new("bad")).is_err());
}

#[test]
fn feed_frames_per_mode() {
    assert_eq!(feed_codes("NSE"), ("N", "C"));
    assert_eq!(feed_codes("NSE_INDEX"), ("N", "C"));
    assert_eq!(feed_codes("BFO"), ("B", "D"));
    assert_eq!(feed_codes("MCX"), ("M", "D"));
    assert_eq!(feed_codes("CDS"), ("N", "U"));
    assert_eq!(methods(FeedMode::Quote), ["MarketFeedV3"]);
    assert_eq!(
        methods(FeedMode::Depth),
        ["MarketDepthService", "MarketFeedV3"]
    );

    let mut f = FivepaisaFeed::new("wss://x", JWT, "5000");
    let frames = f.subscribe_frames(&[sub("3045", "NSE", FeedMode::Depth)]);
    let v: Vec<Value> = frames.iter().map(text_of).collect();
    assert_eq!(v.len(), 2);
    assert_eq!(
        v[0],
        json!({"Method":"MarketDepthService","Operation":"Subscribe","ClientCode":"5000",
               "MarketFeedData":[{"Exch":"N","ExchType":"C","ScripCode":3045}]})
    );
    assert_eq!(v[1]["Method"], "MarketFeedV3");

    // 120 quote subscriptions -> frames of 50, 50, 20.
    let many: Vec<FeedSubscription> = (0..120)
        .map(|i| sub(&format!("{}", 1000 + i), "NFO", FeedMode::Quote))
        .collect();
    let frames = f.subscribe_frames(&many);
    let sizes: Vec<usize> = frames
        .iter()
        .map(|m| text_of(m)["MarketFeedData"].as_array().unwrap().len())
        .collect();
    assert_eq!(sizes, [50, 50, 20]);

    // Depth -> Quote drops only the depth method.
    let ch = f.mode_change_frames(
        &sub("3045", "NSE", FeedMode::Depth),
        &sub("3045", "NSE", FeedMode::Quote),
    );
    assert_eq!(ch.len(), 1);
    let c = text_of(&ch[0]);
    assert_eq!(
        (c["Method"].as_str(), c["Operation"].as_str()),
        (Some("MarketDepthService"), Some("Unsubscribe"))
    );
    // Ltp -> Quote: same method, nothing to send.
    assert!(f
        .mode_change_frames(
            &sub("3045", "NSE", FeedMode::Ltp),
            &sub("3045", "NSE", FeedMode::Quote)
        )
        .is_empty());
    let un = f.unsubscribe_frames(&[sub("3045", "NSE", FeedMode::Quote)]);
    assert_eq!(text_of(&un[0])["Operation"], "Unsubscribe");
}

#[test]
fn feed_decodes_quotes_with_snapshot_merge() {
    let mut f = FivepaisaFeed::new("wss://x", JWT, "5000");
    f.subscribe_frames(&[sub("3045", "NSE", FeedMode::Quote)]);
    let ev = f.parse(&Message::Text(feed_fixture("quote")));
    let FeedEvent::Tick(t) = &ev[0] else {
        panic!("{:?}", ev)
    };
    assert_eq!(
        (t.symbol.as_str(), t.exchange.as_str(), t.mode),
        ("SBIN", "NSE", 2)
    );
    assert_eq!(
        (t.ltp, t.open, t.high, t.low, t.close),
        (812.5, 808.0, 815.0, 805.0, 808.25)
    );
    assert_eq!(
        (t.volume, t.last_quantity, t.average_price),
        (1520034, 10, 810.2)
    );
    assert_eq!(
        (t.total_buy_quantity, t.total_sell_quantity),
        (120000, 98000)
    );
    assert_eq!(t.last_trade_time_ms, 1791082800000);
    assert_eq!(t.change, 4.25);
    // Zeroes keep the last non-zero values.
    let ev = f.parse(&Message::Text(feed_fixture("quote_zeroes")));
    let FeedEvent::Tick(t) = &ev[0] else {
        panic!("{:?}", ev)
    };
    assert_eq!((t.ltp, t.high, t.close), (812.5, 815.0, 808.25));
    assert_eq!(t.volume, 1520100);
    assert!(f
        .parse(&Message::Text(feed_fixture("unknown_token")))
        .is_empty());
    assert!(f.parse(&Message::Text("not json".into())).is_empty());
    assert_eq!(f.cached(), 1);
    f.unsubscribe_frames(&[sub("3045", "NSE", FeedMode::Quote)]);
    assert_eq!(f.cached(), 0);
}

#[test]
fn feed_ltp_mode_carries_only_the_price() {
    let mut f = FivepaisaFeed::new("wss://x", JWT, "5000");
    f.subscribe_frames(&[sub("3045", "NSE", FeedMode::Ltp)]);
    let ev = f.parse(&Message::Text(feed_fixture("quote")));
    let FeedEvent::Tick(t) = &ev[0] else {
        panic!("{:?}", ev)
    };
    assert_eq!((t.mode, t.ltp, t.volume, t.close), (1, 812.5, 0, 0.0));
    // Depth frames are ignored outside depth mode.
    assert!(f.parse(&Message::Text(feed_fixture("depth"))).is_empty());
}

#[test]
fn feed_depth_takes_ltp_from_the_quote_stream() {
    let mut f = FivepaisaFeed::new("wss://x", JWT, "5000");
    f.subscribe_frames(&[sub("3045", "NSE", FeedMode::Depth)]);
    let ev = f.parse(&Message::Text(feed_fixture("quote")));
    assert!(matches!(&ev[0], FeedEvent::Tick(t) if t.mode == 3));
    let ev = f.parse(&Message::Text(feed_fixture("depth")));
    let FeedEvent::Depth(d) = &ev[0] else {
        panic!("{:?}", ev)
    };
    assert_eq!(d.ltp, 812.5);
    assert_eq!((d.buy.len(), d.sell.len()), (5, 5));
    assert_eq!(
        (d.buy[0].price, d.buy[0].quantity, d.buy[0].orders),
        (812.45, 150, 3)
    );
    assert_eq!((d.sell[0].price, d.sell[1].price), (812.55, 0.0));
    assert_eq!(
        (d.total_buy_quantity, d.total_sell_quantity),
        (120000, 98000)
    );
}

#[test]
fn capabilities_and_identity() {
    let b = FivepaisaBroker::new(SymbolResolver::new());
    assert_eq!(b.id(), "fivepaisa");
    let c = b.capabilities();
    assert!(c.history && c.streaming && c.multiquotes_batch);
    // 13-N1: order updates come from the order-book poller.
    assert!(!c.margin && !c.gtt && c.order_feed);
    assert!(b.requires_totp());
    assert_eq!(
        b.login_kind(),
        LoginKind::DirectTotp {
            fields: &["userid", "pin", "totp"]
        }
    );
    assert!(!b.supported_exchanges().contains(&Exchange::Bcd));
}

/// Order updates are the poller, started through `Broker::create_order_feed`
/// and stopped by the logout hook.
#[tokio::test]
async fn order_feed_is_the_poller_and_logout_stops_it() {
    use crate::brokers::common::streaming::OrderFeed;
    let b = FivepaisaBroker::with_urls(
        SymbolResolver::new(),
        "http://127.0.0.1:9",
        "http://127.0.0.1:9/master",
    );
    let auth = AuthToken::new(
        Session {
            api_key: "k".into(),
            client_code: "50001234".into(),
            access_token: "t".into(),
        }
        .encode(),
    );
    // 13-N1: the capability says what the order path is (the poller).
    assert!(b.capabilities().order_feed);
    let feed = Broker::create_order_feed(&b, &auth).unwrap();
    assert!(b.order_updates_running());
    Broker::on_logout(&b).await;
    assert!(!b.order_updates_running());
    let OrderFeed::Stream(mut rx) = feed else {
        panic!("expected the poller stream")
    };
    let closed = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await;
    assert!(matches!(closed, Ok(None)));
}

/// 5paisa lists some contracts under several ScripCodes identical in every
/// other column; one (symbol, exchange) row survives, the highest ScripCode
/// (web #2195, QA audit MC-03). Index rows still keep the first.
#[test]
fn duplicate_contracts_keep_the_highest_scripcode() {
    let head = "Exch,ExchType,ScripCode,Name,Expiry,ScripType,StrikeRate,FullName,TickSize,LotSize,QtyLimit,Multiplier,SymbolRoot,BOCOAllowed,ISIN,ScripData,Series";
    let csv = format!(
        "{head}\n\
B,D,1180001,ITC 29 Oct 2026 CE 400.00,2026-10-29 14:30:00,CE,400,ITC,0.05,1600,0,1,ITC,N,,ITC_CE,XX\n\
B,D,1190002,ITC 29 Oct 2026 CE 400.00,2026-10-29 14:30:00,CE,400,ITC,0.05,1600,0,1,ITC,N,,ITC_CE,XX\n\
B,D,1170003,ITC 29 Oct 2026 CE 400.00,2026-10-29 14:30:00,CE,400,ITC,0.05,1600,0,1,ITC,N,,ITC_CE,XX\n\
N,C,11915,YESBANK,1980-01-01 00:00:00,EQ,0,YES BANK LTD,0.01,1,0,1,YESBANK,Y,,YESBANK_BE,BE\n\
N,C,999920000,NIFTY,1980-01-01 00:00:00,EQ,0,NIFTY 50,0.05,1,0,1,NIFTY 50,N,,NIFTY,EQ\n\
N,C,999920099,NIFTY,1980-01-01 00:00:00,EQ,0,NIFTY 50 AGAIN,0.05,1,0,1,NIFTY 50,N,,NIFTY,EQ\n"
    );
    let rows = parse_csv(&csv);
    let itc: Vec<&SymToken> = rows
        .iter()
        .filter(|r| r.symbol == "ITC29OCT26400CE" && r.exchange == "BFO")
        .collect();
    assert_eq!(itc.len(), 1);
    assert_eq!(itc[0].token, "1190002");
    assert_eq!(
        rows.iter()
            .find(|r| r.symbol == "YESBANK")
            .map(|r| r.instrument_type.as_str()),
        Some("EQ")
    );
    let nifty: Vec<&SymToken> = rows.iter().filter(|r| r.exchange == "NSE_INDEX").collect();
    assert_eq!(nifty.len(), 1);
    assert_eq!(nifty[0].token, "999920000");
    // Unique (symbol, exchange) across the whole master.
    let mut seen = std::collections::HashSet::new();
    assert!(rows
        .iter()
        .all(|r| seen.insert((r.symbol.clone(), r.exchange.clone()))));
}

/// A future-dated range answered with the latest candle yields nothing
/// (web #2195, QA audit HS-19).
#[test]
fn candles_outside_the_requested_chunk_are_dropped() {
    let d = |y, m, dd| NaiveDate::from_ymd_opt(y, m, dd).unwrap();
    let latest = serde_json::json!(["2026-10-08T09:15:00", 812.0, 815.0, 810.0, 813.0, 1000]);
    assert!(!candle_in_range(&latest, d(2026, 11, 1), d(2026, 11, 30)));
    assert!(candle_in_range(&latest, d(2026, 10, 1), d(2026, 10, 8)));
    assert!(candle_in_range(&latest, d(2026, 10, 8), d(2026, 10, 8)));
    assert!(!candle_in_range(&latest, d(2026, 10, 9), d(2026, 10, 9)));
    assert!(!candle_in_range(
        &serde_json::json!(["bad"]),
        d(2026, 1, 1),
        d(2027, 1, 1)
    ));
}
