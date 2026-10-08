//! Noren family mapping tests against recorded payloads
//! (`src-tauri/tests/fixtures/brokers/<member>/`), built from the web code
//! and the Noren API documentation; no account data.

use super::auth::{exchange_request, sha256_hex, split_api_key};
use super::data::{parse_candle, quote_matches, repair, sort_dedupe_last, to_depth, to_quote};
use super::funds::{basket_body, funds_from};
use super::mapping::*;
use super::master_contract::{expiry, nse_index_symbol, parse_file};
use super::streaming::NorenFeed;
use super::*;
use crate::brokers::common::mapping::{Action, Exchange, PriceType, Product};
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription, Message};
use crate::brokers::{flattrade, shoonya, tradesmart, zebu};
use serde_json::{json, Value};

macro_rules! fixture {
    ($b:literal, $name:literal) => {
        include_str!(concat!(
            "../../../../tests/fixtures/brokers/",
            $b,
            "/",
            $name
        ))
    };
}

fn shoonya_master() -> SymbolResolver {
    let cfg = shoonya::config();
    let mut rows = Vec::new();
    for (ex, text) in [
        ("NSE", fixture!("shoonya", "NSE_symbols.txt")),
        ("BSE", fixture!("shoonya", "BSE_symbols.txt")),
        ("NFO", fixture!("shoonya", "NFO_symbols.txt")),
        ("CDS", fixture!("shoonya", "CDS_symbols.txt")),
        ("MCX", fixture!("shoonya", "MCX_symbols.txt")),
        ("BFO", fixture!("shoonya", "BFO_symbols.txt")),
    ] {
        rows.extend(parse_file(cfg, ex, text));
    }
    let r = SymbolResolver::new();
    r.load(rows);
    r
}

fn json(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

fn rows(s: &str) -> Vec<Value> {
    json(s).as_array().cloned().unwrap()
}

// ---------------------------------------------------------------------------
// Member configuration
// ---------------------------------------------------------------------------

#[test]
fn member_configs_carry_the_web_hosts_and_login_variants() {
    let s = shoonya::config();
    assert_eq!(s.rest_url, "https://api.shoonya.com/NorenWClientAPI");
    assert_eq!(s.ws_url, "wss://api.shoonya.com/NorenWSAPI/");
    assert_eq!(s.dialect, Dialect::BearerJData);
    assert_eq!(s.chart_dialect, Some(Dialect::JKeyForm));
    assert!(
        matches!(s.login, Login::GenAcsTok { authorize_url } if authorize_url == "https://api.shoonya.com/OAuthlogin/authorize/oauth")
    );
    assert_eq!(s.master_files.len(), 6);
    assert!(s
        .master_files
        .iter()
        .all(|f| f.zipped
            && f.url == format!("https://api.shoonya.com/{}_symbols.txt.zip", f.exchange)));

    let z = zebu::config();
    assert_eq!(z.rest_url, "https://go.mynt.in/NorenWClientAPI");
    assert_eq!(z.ws_url, "wss://go.mynt.in/NorenWSAPI/");
    assert!(!z.exchanges.contains(&Exchange::BseIndex));
    assert_eq!(z.margin, MarginApi::Unsupported);
    assert_eq!(z.mpp, MppScope::MarketOnly);
    assert!(z
        .master_files
        .iter()
        .all(|f| f.url.starts_with("https://go.mynt.in/")));

    let t = tradesmart::config();
    assert_eq!(
        t.rest_url,
        "https://v2api.tradesmartonline.in/NorenWClientAPIv2"
    );
    assert_eq!(t.ws_url, "wss://v2api.tradesmartonline.in/NorenWSAPI/");
    assert_eq!(t.place_remarks, Some("openalgo"));
    assert!(!t.send_mkt_protection);
    assert!(!t.timeframes.iter().any(|(k, _)| *k == "4h"));
    assert_eq!(t.margin, MarginApi::PerLeg);
    assert!(!t.order_feed_subscribe);

    let f = flattrade::config();
    assert_eq!(f.rest_url, "https://piconnect.flattrade.in/PiConnectAPI");
    assert_eq!(f.ws_url, "wss://piconnect.flattrade.in/PiConnectWSAPI/");
    assert_eq!(f.dialect, Dialect::JKeyForm);
    assert!(
        matches!(f.login, Login::ApiToken { token_url, .. } if token_url == "https://authapi.flattrade.in/trade/apitoken")
    );
    assert_eq!(f.master_files.len(), 8);
    assert!(f.master_files.iter().all(|m| !m.zipped
        && m.url
            .starts_with("https://flattrade.s3.ap-south-1.amazonaws.com/scripmaster/")));
    assert_eq!(
        f.master_files
            .iter()
            .filter(|m| m.exchange == "NFO")
            .count(),
        2
    );
    assert!(f.persistent_socket);
    assert_eq!(
        f.rate.order,
        Some(Window {
            per_second: 9,
            per_minute: 38
        })
    );
    assert_eq!(
        f.rate.data,
        Some(Window {
            per_second: 9,
            per_minute: 110
        })
    );
}

#[test]
fn endpoints_and_authorize_urls() {
    let e = NorenEndpoints::from_config(shoonya::config());
    assert_eq!(e.token, "https://api.shoonya.com/NorenWClientAPI/GenAcsTok");
    let r = NorenEndpoints::rebased(shoonya::config(), "http://127.0.0.1:9", "ws://127.0.0.1:8");
    assert_eq!(r.rest, "http://127.0.0.1:9/NorenWClientAPI");
    assert_eq!(r.ws, "ws://127.0.0.1:8/NorenWSAPI/");
    assert_eq!(r.master_urls[0], "http://127.0.0.1:9/NSE_symbols.txt.zip");
    let f = NorenEndpoints::rebased(flattrade::config(), "http://h", "ws://w");
    assert_eq!(f.token, "http://h/trade/apitoken");

    assert_eq!(
        authorize_url(shoonya::config(), "FA123:::FA123_U", "st"),
        "https://api.shoonya.com/OAuthlogin/authorize/oauth?client_id=FA123_U&state=st"
    );
    assert_eq!(
        authorize_url(flattrade::config(), "FT1:::abc", "s2"),
        "https://auth.flattrade.in/?app_key=abc&state=s2"
    );
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

#[test]
fn checksums_follow_each_login_variant() {
    assert_eq!(
        sha256_hex(&["abc", "def", "ghi"]),
        "19cc02f26df43cc571bc9ed7b0c4d29224a3ec229529221725ef76d021c8326f"
    );
    let (body, ct) = exchange_request(shoonya::config().login, "abc", "def", "ghi");
    assert_eq!(ct, "text/plain");
    assert!(body.starts_with("jData="));
    let j = json(&body["jData=".len()..]);
    assert_eq!(j["code"], "ghi");
    assert_eq!(j["checksum"], sha256_hex(&["abc", "def", "ghi"]));
    // Flattrade orders the hash api_key + code + secret.
    let (body, ct) = exchange_request(flattrade::config().login, "abc", "def", "ghi");
    assert_eq!(ct, "application/json");
    let j = json(&body);
    assert_eq!(j["api_key"], "abc");
    assert_eq!(j["request_code"], "ghi");
    assert_eq!(j["api_secret"], sha256_hex(&["abc", "ghi", "def"]));
}

#[test]
fn api_key_split_and_login_hooks() {
    assert_eq!(
        split_api_key("Z56004:::Z56004_U", None),
        ("Z56004".into(), "Z56004_U".into())
    );
    assert_eq!(
        split_api_key("KEY", Some("U1")),
        ("U1".into(), "KEY".into())
    );
    assert_eq!(
        hooks::login_access_token(&json!({"stat":"Ok","access_token":"t1"})),
        Some(("t1".into(), None))
    );
    assert_eq!(
        hooks::login_token(&json!({"token":"t2"})),
        Some(("t2".into(), None))
    );
    assert_eq!(
        hooks::login_any(&json!({"susertoken":"t3","actid":"TS1"})),
        Some(("t3".into(), Some("TS1".into())))
    );
    assert_eq!(hooks::login_any(&json!({"stat":"Ok"})), None);
}

// ---------------------------------------------------------------------------
// Master contract
// ---------------------------------------------------------------------------

#[test]
fn shoonya_master_symbols_expiry_and_ticks() {
    let r = shoonya_master();
    let sbin = r.by_symbol("NSE", "SBIN").unwrap();
    assert_eq!(sbin.brsymbol, "SBIN-EQ");
    assert_eq!(sbin.tick_size, 0.05);
    assert_eq!(sbin.instrument_type, "EQ");
    assert_eq!(sbin.strike, -1.0);
    assert_eq!(r.by_symbol("NSE", "YESBANK").unwrap().instrument_type, "EQ");
    assert_eq!(r.by_symbol("NSE", "M&M").unwrap().brsymbol, "M&M-EQ");
    let nifty = r.by_symbol("NSE_INDEX", "NIFTY").unwrap();
    assert_eq!(nifty.brsymbol, "Nifty 50");
    assert_eq!(nifty.brexchange, "NSE_INDEX");
    assert_eq!(nifty.token, "26000");
    for s in ["BANKNIFTY", "FINNIFTY", "INDIAVIX", "NIFTYIT"] {
        assert!(r.by_symbol("NSE_INDEX", s).is_some(), "{}", s);
    }
    // Placeholder row with blank symbol columns is dropped.
    assert!(r.by_token("NSE", "99999").is_none());
    // BSE: all EQ, manual SENSEX/BANKEX.
    assert_eq!(r.by_symbol("BSE", "SBIN").unwrap().instrument_type, "EQ");
    assert_eq!(r.by_symbol("BSE_INDEX", "SENSEX").unwrap().token, "1");
    assert_eq!(r.by_symbol("BSE_INDEX", "BANKEX").unwrap().token, "12");
    // NFO.
    let fut = r.by_symbol("NFO", "NIFTY27OCT26FUT").unwrap();
    assert_eq!(fut.brsymbol, "NIFTY27OCT26F");
    assert_eq!(fut.expiry, "27-OCT-26");
    assert_eq!(fut.lot_size, 75);
    assert_eq!(fut.tick_size, 0.1);
    let ce = r.by_symbol("NFO", "NIFTY27OCT2625000CE").unwrap();
    assert_eq!((ce.strike, ce.instrument_type.as_str()), (25000.0, "CE"));
    assert!(r.by_symbol("NFO", "VEDL27OCT26292.5CE").is_some());
    assert_eq!(r.expiries("NFO", "NIFTY", None), ["27-OCT-26"]);
    // CDS: token <= 100 dropped, OPTCUR -> CE.
    assert!(r.by_token("CDS", "1").is_none());
    assert!(r.by_symbol("CDS", "USDINR27OCT26FUT").is_some());
    assert_eq!(
        r.by_symbol("CDS", "USDINR27OCT2683.25CE")
            .unwrap()
            .tick_size,
        0.0025
    );
    // MCX: OPTFUT -> CE.
    assert!(r.by_symbol("MCX", "CRUDEOIL19OCT26FUT").is_some());
    assert_eq!(
        r.by_symbol("MCX", "CRUDEOIL15OCT268650CE")
            .unwrap()
            .lot_size,
        100
    );
    // BFO: underlying and type from the trading symbol.
    let bfo = r.by_symbol("BFO", "SENSEX29OCT2682000CE").unwrap();
    assert_eq!(bfo.name, "SENSEX");
    assert!(r.by_symbol("BFO", "SENSEX29OCT26FUT").is_some());
}

#[test]
fn flattrade_master_csv_layout() {
    let cfg = flattrade::config();
    let nse = parse_file(cfg, "NSE", fixture!("flattrade", "NSE_Equity.csv"));
    assert_eq!(nse.len(), 4, "blank trading symbol dropped");
    let sbin = nse.iter().find(|r| r.symbol == "SBIN").unwrap();
    assert_eq!((sbin.tick_size, sbin.lot_size), (0.05, 1));
    assert!(nse
        .iter()
        .any(|r| r.exchange == "NSE_INDEX" && r.symbol == "NIFTY"));
    assert!(nse.iter().any(|r| r.symbol == "MIDCPNIFTY"));
    let bse = parse_file(cfg, "BSE", fixture!("flattrade", "BSE_Equity.csv"));
    let sensex = bse
        .iter()
        .find(|r| r.exchange == "BSE_INDEX" && r.symbol == "SENSEX")
        .unwrap();
    assert_eq!(
        (sensex.brexchange.as_str(), sensex.instrument_type.as_str()),
        ("BSE", "INDEX")
    );
    assert!(bse.iter().any(|r| r.symbol == "SENSEX50"));
    let nfo = parse_file(
        cfg,
        "NFO",
        fixture!("flattrade", "Nfo_Index_Derivatives.csv"),
    );
    assert_eq!(nfo[0].symbol, "NIFTY27OCT26FUT");
    assert_eq!(nfo[0].expiry, "27-OCT-26");
    assert_eq!(nfo[1].symbol, "NIFTY27OCT2625000CE");
    let cds = parse_file(
        cfg,
        "CDS",
        "Exchange,Token,Lotsize,Symbol,Tradingsymbol,Instrument,Expiry,Strike,Optiontype\nCDS,5,1,USDINR,USDINR27OCT26F,FUTCUR,27-OCT-2026,0,XX\n",
    );
    assert_eq!(cds[0].tick_size, 0.0025);
}

#[test]
fn zebu_master_exact_index_names_raw_ticks() {
    let rows = parse_file(zebu::config(), "NSE", fixture!("zebu", "NSE_symbols.txt"));
    let nifty = rows.iter().find(|r| r.token == "26000").unwrap();
    assert_eq!(
        (nifty.symbol.as_str(), nifty.brexchange.as_str()),
        ("NIFTY", "NSE")
    );
    assert_eq!(
        rows.iter().find(|r| r.token == "26009").unwrap().symbol,
        "BANKNIFTY"
    );
    // Not in the exact table: keeps the broker name.
    assert_eq!(
        rows.iter().find(|r| r.token == "26014").unwrap().symbol,
        "NIFTY IT"
    );
    assert_eq!(
        rows.iter().find(|r| r.token == "3045").unwrap().tick_size,
        0.05
    );
    let bse = parse_file(
        zebu::config(),
        "BSE",
        fixture!("shoonya", "BSE_symbols.txt"),
    );
    assert!(bse.iter().all(|r| r.exchange == "BSE"));
}

#[test]
fn expiry_and_index_helpers() {
    assert_eq!(expiry("27-OCT-2026"), "27-OCT-26");
    assert_eq!(expiry("05-Jan-2027"), "05-JAN-27");
    assert_eq!(expiry(""), "");
    assert_eq!(expiry("garbage"), "");
    assert_eq!(
        nse_index_symbol(IndexNaming::StripAndOverride, "Nifty Fin Service"),
        "FINNIFTY"
    );
    assert_eq!(
        nse_index_symbol(IndexNaming::StripAndOverride, "NIFTY MIDCAP SELECT"),
        "MIDCPNIFTY"
    );
    assert_eq!(
        nse_index_symbol(IndexNaming::ExactName, "NIFTY FIN SERVICE"),
        "FINNIFTY"
    );
    assert_eq!(
        nse_index_symbol(IndexNaming::ExactName, "Nifty 50"),
        "Nifty 50"
    );
}

// ---------------------------------------------------------------------------
// Enum maps and statuses
// ---------------------------------------------------------------------------

#[test]
fn enum_maps() {
    assert_eq!(noren_exchange("NSE_INDEX"), "NSE");
    assert_eq!(noren_exchange("BSE_INDEX"), "BSE");
    assert_eq!(noren_exchange("NFO"), "NFO");
    assert_eq!(product_code(Product::Cnc), "C");
    assert_eq!(product_code(Product::Nrml), "M");
    assert_eq!(product_code(Product::Mis), "I");
    assert_eq!(reverse_product("M"), Some("NRML"));
    assert_eq!(reverse_product("H"), None);
    assert_eq!(book_product("NSE", "C"), "CNC");
    assert_eq!(book_product("NFO", "C"), "C");
    assert_eq!(book_product("MCX", "M"), "NRML");
    assert_eq!(book_product("NSE", "M"), "M");
    assert_eq!(book_product("BFO", "I"), "MIS");
    assert_eq!(pricetype_code(PriceType::Sl), "SL-LMT");
    assert_eq!(pricetype_code(PriceType::SlM), "SL-MKT");
    assert_eq!(book_pricetype("SL-MKT"), "SL-M");
    assert_eq!(book_pricetype("SLLMT"), "SL");
    assert_eq!(book_pricetype("MKT"), "MARKET");
    assert_eq!(escape_tsym("M&M-EQ"), "M%26M-EQ");
}

#[test]
fn rest_and_push_statuses() {
    assert_eq!(normalize_status("COMPLETE"), "complete");
    assert_eq!(normalize_status("TRIGGER_PENDING"), "open");
    assert_eq!(normalize_status("Open"), "open");
    assert_eq!(normalize_status("REJECT"), "rejected");
    assert_eq!(normalize_status("CANCELED"), "cancelled");
    assert_eq!(normalize_status("AFTER MARKET ORDER REQ RECEIVED"), "open");
    assert_eq!(normalize_status("SOMETHING_ELSE"), "something else");
    assert_eq!(push_status("TRIGGER_PENDING", ""), "trigger pending");
    assert_eq!(push_status("Executed", ""), "complete");
    assert_eq!(push_status("", "Rejected"), "rejected");
    assert_eq!(push_status("", "Canceled"), "cancelled");
    assert_eq!(push_status("", "Fill"), "open");
    assert_eq!(push_status("weird", "Fill"), "weird");
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

#[test]
fn shoonya_order_book_is_openalgo_shaped() {
    let r = shoonya_master();
    let cfg = shoonya::config();
    let book: Vec<Order> = rows(fixture!("shoonya", "order_book.json"))
        .iter()
        .map(|o| map_order(cfg, o, &r))
        .collect();
    assert_eq!(book.len(), 5);
    let o = &book[0];
    assert_eq!(
        (
            o.symbol.as_str(),
            o.side.as_str(),
            o.product.as_str(),
            o.order_type.as_str(),
            o.status.as_str()
        ),
        ("SBIN", "BUY", "CNC", "LIMIT", "open")
    );
    assert_eq!(o.price, 812.5);
    assert_eq!(o.exchange_order_id.as_deref(), Some("1100000012345678"));
    let sl = &book[1];
    assert_eq!(sl.symbol, "NIFTY27OCT2625000PE");
    assert_eq!(
        (
            sl.order_type.as_str(),
            sl.product.as_str(),
            sl.status.as_str()
        ),
        ("SL-M", "NRML", "open")
    );
    assert_eq!(sl.trigger_price, 118.0);
    let m = &book[2];
    assert_eq!(
        (m.status.as_str(), m.filled_quantity, m.pending_quantity),
        ("complete", 5, 0)
    );
    assert_eq!(m.price, 0.0, "shoonya keeps prc as sent");
    assert_eq!(
        book[3].rejection_reason.as_deref(),
        Some("RMS:Margin Exceeds")
    );
    assert_eq!(book[3].symbol, "YESBANK");
    assert_eq!(
        book[4].symbol, "UNKNOWN-EQ",
        "unknown instrument keeps the broker symbol"
    );
    assert_eq!(book[4].status, "cancelled");
}

#[test]
fn flattrade_order_book_price_fallbacks() {
    let r = shoonya_master();
    let cfg = flattrade::config();
    let book: Vec<Order> = rows(fixture!("flattrade", "order_book.json"))
        .iter()
        .map(|o| map_order(cfg, o, &r))
        .collect();
    assert_eq!(book[0].price, 812.45, "rprc for a zero-priced MARKET");
    assert_eq!((book[0].filled_quantity, book[0].pending_quantity), (4, 6));
    assert_eq!(book[1].price, 25010.55, "avgprc when instname is present");
    assert_eq!(book[1].average_price, 25010.55);
}

#[test]
fn trade_book_per_member() {
    let r = shoonya_master();
    let t = rows(fixture!("shoonya", "trade_book.json"));
    let s: Vec<Trade> = t
        .iter()
        .map(|x| map_trade(shoonya::config(), x, &r))
        .collect();
    assert_eq!(s[0].symbol, "RELIANCE");
    assert_eq!(s[0].timestamp, "09:22:00");
    assert_eq!(s[0].trade_id, "55001");
    assert_eq!(s[0].trade_value, 7091.0);
    assert_eq!(s[1].symbol, "NIFTY27OCT26FUT");
    assert_eq!(
        (s[1].side.as_str(), s[1].product.as_str()),
        ("SELL", "NRML")
    );
    let f: Vec<Trade> = t
        .iter()
        .map(|x| map_trade(flattrade::config(), x, &r))
        .collect();
    assert_eq!(f[1].timestamp, "10:01:02 03-10-2026");
    assert_eq!(f[1].average_price, 25123.46);
    assert_eq!(f[1].trade_value, 1884259.5);
}

#[test]
fn positions_and_pnl_rules() {
    let r = shoonya_master();
    let p = rows(fixture!("shoonya", "positions.json"));
    let s: Vec<Position> = p
        .iter()
        .map(|x| map_position(shoonya::config(), x, &r))
        .collect();
    assert_eq!(s[0].symbol, "RELIANCE");
    assert_eq!((s[0].quantity, s[0].product.as_str()), (5, "MIS"));
    assert_eq!(s[0].pnl, 34.0);
    // Short future without urmtom: (avg - lp) * |qty| + rpnl.
    assert_eq!(s[1].quantity, -75);
    assert_eq!(s[1].pnl, 20.0 * 75.0 + 150.0);
    // Closed row: average from the day buy average, pnl = rpnl.
    assert_eq!((s[2].average_price, s[2].pnl), (810.0, -25.5));
    let f: Vec<Position> = p
        .iter()
        .map(|x| map_position(flattrade::config(), x, &r))
        .collect();
    assert_eq!(f[1].unrealized_pnl, (25080.0 - 25100.0) * -75.0);
    assert_eq!(f[1].pnl, 1650.0);
    assert_eq!(f[2].average_price, 0.0);
}

#[test]
fn holdings_quantity_formulas() {
    let r = shoonya_master();
    let h = rows(fixture!("shoonya", "holdings.json"));
    let s = map_holdings(shoonya::config(), &h, &r);
    assert_eq!(s.len(), 2, "Not_Ok rows and non-NSE legs dropped");
    assert_eq!(s[0].symbol, "SBIN");
    assert_eq!(s[0].quantity, 10);
    assert_eq!(s[0].isin.as_deref(), Some("INE062A01020"));
    assert_eq!(
        (s[0].average_price, s[0].ltp, s[0].pnl),
        (780.0, 780.0, 0.0)
    );
    // btst 2 + max(npoad 3, dp 0) - used 1.
    assert_eq!(s[1].quantity, 4);
    assert_eq!(s[1].symbol, "M&M");
    assert_eq!(map_holdings(flattrade::config(), &h, &r)[1].quantity, 3);
    assert_eq!(map_holdings(zebu::config(), &h, &r)[0].quantity, 10);
    assert_eq!(hooks::holding_qty_npoadt1(&h[0]), 10);
}

#[test]
fn funds_formulas() {
    let l = json(fixture!("shoonya", "limits.json"));
    let s = funds_from(shoonya::config(), &l, &[]);
    assert_eq!(s.available_cash, 85000.0);
    assert_eq!(s.collateral, 2500.0);
    assert_eq!(s.utilised_debits, 20000.0);
    assert_eq!((s.m2m_realized, s.m2m_unrealized), (150.0, 34.0));
    let p = rows(fixture!("shoonya", "positions.json"));
    let f = funds_from(flattrade::config(), &l, &p);
    assert_eq!(f.collateral, 9999.0, "flattrade prefers collateral");
    assert_eq!(f.m2m_realized, 124.5);
    assert_eq!(f.m2m_unrealized, 34.0 + 1500.0);
}

// ---------------------------------------------------------------------------
// Order payloads and MPP
// ---------------------------------------------------------------------------

fn resolved(symbol: &str, exchange: &str, pricetype: &str, price: f64) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: "BUY".into(),
        quantity: 10,
        price,
        order_type: pricetype.into(),
        product: "CNC".into(),
        validity: "DAY".into(),
        trigger_price: Some(0.0),
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, &shoonya_master()).unwrap()
}

#[test]
fn place_payload_per_member() {
    let o = resolved("M&M", "NSE", "LIMIT", 2900.5);
    let s = place_jdata(shoonya::config(), "U1", &o, "LMT", "2900.5");
    assert_eq!(
        s,
        json!({"uid":"U1","actid":"U1","exch":"NSE","tsym":"M%26M-EQ","qty":"10","prc":"2900.5",
               "trgprc":"0","dscqty":"0","prd":"C","trantype":"B","prctyp":"LMT",
               "mkt_protection":"0","ret":"DAY","ordersource":"API"})
    );
    let t = place_jdata(tradesmart::config(), "U1", &o, "LMT", "2900.5");
    assert!(t.get("mkt_protection").is_none());
    assert_eq!(t["remarks"], "openalgo");
}

#[test]
fn modify_payload_trigger_only_for_stops() {
    let sym = shoonya_master();
    let m = |pt: &str| {
        ResolvedModify::resolve(
            "26100300000001",
            &ModifyOrderRequest {
                symbol: "SBIN".into(),
                exchange: "NSE".into(),
                action: "BUY".into(),
                product: "CNC".into(),
                pricetype: pt.into(),
                quantity: 5,
                price: 810.0,
                trigger_price: 805.0,
                disclosed_quantity: 0,
            },
            &sym,
        )
        .unwrap()
    };
    let lim = modify_jdata(shoonya::config(), "U1", &m("LIMIT"));
    assert!(lim.get("trgprc").is_none());
    assert_eq!(lim["norenordno"], "26100300000001");
    assert_eq!(lim["tsym"], "SBIN-EQ");
    assert!(lim.get("actid").is_none());
    assert_eq!(
        modify_jdata(shoonya::config(), "U1", &m("SL"))["trgprc"],
        "805"
    );
    assert_eq!(modify_jdata(zebu::config(), "U1", &m("MARKET"))["prc"], "0");
    assert_eq!(
        modify_jdata(shoonya::config(), "U1", &m("MARKET"))["prc"],
        "810"
    );
}

fn mo(pricetype: PriceType, price: f64, trigger: f64) -> MppOrder<'static> {
    MppOrder {
        symbol: "SBIN",
        action: Action::Buy,
        pricetype,
        price,
        trigger,
    }
}

#[test]
fn mpp_scopes() {
    let q = Some(MppQuote {
        ltp: 812.4,
        tick: Some(0.05),
    });
    // 812.4 * 1.005 = 816.462 -> 816.45 on a 0.05 tick.
    let m = mo(PriceType::Market, 0.0, 0.0);
    assert_eq!(
        mpp_place(MppScope::MarketAndStop, &m, q, 0.05),
        ("LMT", "816.45".into())
    );
    assert_eq!(
        mpp_place(MppScope::MarketAndStop, &m, None, 0.05),
        ("MKT", "0".into())
    );
    let slm = mo(PriceType::SlM, 0.0, 800.0);
    assert_eq!(
        mpp_place(MppScope::MarketAndStop, &slm, q, 0.05).0,
        "SL-LMT"
    );
    // No quote: SL-LMT priced off the trigger, buffered by the master tick.
    assert_eq!(
        mpp_place(MppScope::MarketAndStop, &slm, None, 0.05),
        ("SL-LMT", "804".into())
    );
    assert_eq!(
        mpp_place(MppScope::MarketAndStop, &slm, None, 0.0),
        ("SL-LMT", "800".into())
    );
    // Zebu: MARKET only.
    assert_eq!(
        mpp_place(MppScope::MarketOnly, &slm, q, 0.05),
        ("SL-MKT", "0".into())
    );
    assert_eq!(mpp_place(MppScope::MarketOnly, &m, q, 0.05).0, "LMT");
    // Tradesmart: always converts, SL-M off the trigger.
    assert_eq!(
        mpp_place(MppScope::AlwaysConvert, &slm, q, 0.05),
        ("SL-LMT", "804".into())
    );
    assert_eq!(
        mpp_place(MppScope::AlwaysConvert, &m, None, 0.05),
        ("LMT", "0".into())
    );
    let lim = mo(PriceType::Limit, 811.0, 0.0);
    assert_eq!(
        mpp_place(MppScope::AlwaysConvert, &lim, q, 0.05),
        ("LMT", "811".into())
    );
    // Margin legs always convert.
    assert_eq!(mpp_margin(&m, None), ("LMT", "0".into()));
    assert_eq!(mpp_margin(&slm, None), ("SL-LMT", "800".into()));
    assert_eq!(mpp_margin(&m, q).1, "816.45");
}

#[test]
fn basket_margin_body_first_leg_flat() {
    let legs = vec![
        json!({"exch":"NFO","tsym":"A"}),
        json!({"exch":"NFO","tsym":"B"}),
    ];
    let b = basket_body("U1", legs).unwrap();
    assert_eq!(b["tsym"], "A");
    assert_eq!(b["uid"], "U1");
    assert_eq!(b["actid"], "U1");
    assert_eq!(b["basketlists"], json!([{"exch":"NFO","tsym":"B"}]));
    assert!(basket_body("U1", vec![]).is_none());
    assert_eq!(
        hooks::margin_used_trade(&json!({"marginusedtrade":"10","marginused":"20"})),
        10.0
    );
    assert_eq!(hooks::margin_used_trade(&json!({"marginused":"20"})), 20.0);
    assert_eq!(hooks::order_margin(&json!({"marginused":"7"})), 7.0);
    assert_eq!(
        hooks::cancel_message(&json!({"emsg":"x"})).as_deref(),
        Some("x")
    );
    assert_eq!(
        hooks::cancel_emsg(&json!({"emsg":"y","message":"z"})).as_deref(),
        Some("y")
    );
}

// ---------------------------------------------------------------------------
// Quotes and history
// ---------------------------------------------------------------------------

#[test]
fn quote_and_depth() {
    let v = json(fixture!("shoonya", "quote.json"));
    let key = QuoteKey::new("NSE", "SBIN");
    let q = to_quote(&key, &v);
    assert_eq!(
        (q.ltp, q.close, q.bid, q.ask, q.volume),
        (812.4, 808.0, 812.35, 812.45, 1234567)
    );
    assert_eq!((q.bid_qty, q.ask_qty), (100, 200));
    assert_eq!(q.change, 4.4);
    let d = to_depth(&key, &v, false);
    assert_eq!(d.bids.len(), 5);
    assert_eq!(d.asks[4].price, 812.65);
    assert_eq!(d.bids[0].orders, 3);
    assert_eq!(d.total_buy_qty, 600);
    assert_eq!(d.total_sell_qty, 1100);
    assert_eq!(d.oi, 0, "shoonya reports no depth OI");
    assert_eq!(to_depth(&key, &v, true).oi, 7);
    assert!(quote_matches(&v, "NSE", "3045"));
    assert!(!quote_matches(&v, "NSE", "26000"));
    assert!(!quote_matches(&v, "BSE", "3045"));
    assert!(quote_matches(&json!({"stat":"Ok"}), "NSE", "1"));
}

#[test]
fn candles_parse_repair_and_dedupe() {
    let raw = rows(fixture!("shoonya", "tpseries.json"));
    let mut c: Vec<Candle> = raw.iter().filter_map(parse_candle).collect();
    assert_eq!(c.len(), 3, "all-zero row skipped");
    c = sort_dedupe_last(c);
    assert_eq!(c[0].timestamp, 1790826300);
    c.iter_mut().for_each(repair);
    let bad = c.iter().find(|x| x.timestamp == 1790848200).unwrap();
    assert_eq!((bad.low, bad.high, bad.volume), (811.0, 813.0, 0));
    let eod = rows(fixture!("shoonya", "eod.json"));
    let d: Vec<Candle> = eod.iter().filter_map(parse_candle).collect();
    assert_eq!(d[0].timestamp, 1790726400);
    // No ssboe: DD-MON-YYYY at UTC midnight.
    assert_eq!(d[1].timestamp, 1790812800);
    assert_eq!(d[1].volume, 8000000);
    // IST wall clock when only `time` is present.
    let t = parse_candle(
        &json!({"time":"01-10-2026 09:15:00","into":"1","inth":"1","intl":"1","intc":"1"}),
    )
    .unwrap();
    assert_eq!(t.timestamp, 1790826300);
    let dup = sort_dedupe_last(vec![
        Candle {
            timestamp: 1,
            close: 1.0,
            ..Default::default()
        },
        Candle {
            timestamp: 1,
            close: 2.0,
            ..Default::default()
        },
    ]);
    assert_eq!((dup.len(), dup[0].close), (1, 2.0));
    assert_eq!(shoonya_history_window("1m"), 5 * 86400);
    assert_eq!(shoonya_history_window("D"), 730 * 86400);
}

// ---------------------------------------------------------------------------
// Feed
// ---------------------------------------------------------------------------

fn sub(symbol: &str, exchange: &str, token: &str, mode: FeedMode) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: exchange.into(),
        token: token.into(),
        brsymbol: String::new(),
        brexchange: exchange.into(),
        mode,
        depth: 5,
    }
}

fn frame(name: &str) -> String {
    json(fixture!("shoonya", "feed.json"))[name].to_string()
}

fn text_of(m: &Message) -> Value {
    match m {
        Message::Text(t) => json(t),
        _ => panic!("text frame expected"),
    }
}

#[test]
fn feed_handshake_subscribe_and_heartbeat() {
    use crate::brokers::common::streaming::BrokerFeed;
    let mut f = NorenFeed::new(
        shoonya::config(),
        "wss://x/NorenWSAPI/",
        "U1",
        "tok",
        shoonya_master(),
    );
    assert!(f.awaits_auth_ack());
    let c = f.on_connected();
    assert_eq!(
        text_of(&c[0]),
        json!({"t":"a","uid":"U1","actid":"U1","source":"API","accesstoken":"tok"})
    );
    // The order subscription goes once per connection, after the login
    // ack, whether or not anything is subscribed.
    let after_ack = f.on_authenticated();
    assert_eq!(text_of(&after_ack[0]), json!({"t":"o","actid":"U1"}));
    let frames = f.subscribe_frames(&[
        sub("SBIN", "NSE", "3045", FeedMode::Quote),
        sub("NIFTY", "NSE_INDEX", "26000", FeedMode::Ltp),
        sub("NIFTY27OCT26FUT", "NFO", "54321", FeedMode::Depth),
    ]);
    assert_eq!(
        text_of(&frames[0]),
        json!({"t":"t","k":"NSE|3045#NSE|26000"})
    );
    assert_eq!(text_of(&frames[1]), json!({"t":"d","k":"NFO|54321"}));
    let again = f.subscribe_frames(&[sub("RELIANCE", "NSE", "2885", FeedMode::Ltp)]);
    assert_eq!(again.len(), 1);
    let many: Vec<FeedSubscription> = (0..250)
        .map(|n| sub(&format!("S{}", n), "NSE", &n.to_string(), FeedMode::Ltp))
        .collect();
    assert_eq!(f.subscribe_frames(&many).len(), 3, "100 scrips per frame");
    let (period, hb) = f.heartbeat().unwrap();
    assert_eq!(period.as_secs(), 30);
    assert_eq!(text_of(&hb), json!({"t":"h"}));
    // Tradesmart pushes order updates without asking.
    let mut t = NorenFeed::new(
        tradesmart::config(),
        "wss://x/",
        "U1",
        "tok",
        shoonya_master(),
    );
    t.on_connected();
    assert!(t.on_authenticated().is_empty());
    let tf = t.subscribe_frames(&[sub("SBIN", "NSE", "3045", FeedMode::Ltp)]);
    assert_eq!(tf.len(), 1);
    assert_eq!(text_of(&tf[0])["t"], "t");
}

#[test]
fn feed_parses_acks_ticks_depth_and_orders() {
    use crate::brokers::common::streaming::BrokerFeed;
    let mut f = NorenFeed::new(shoonya::config(), "wss://x/", "U1", "tok", shoonya_master());
    f.on_connected();
    assert_eq!(f.parse_text(&frame("ack_ok")), vec![FeedEvent::AuthOk]);
    assert!(matches!(
        f.parse_text(&frame("ack_bad"))[0],
        FeedEvent::AuthFailed(_)
    ));
    assert_eq!(
        f.parse_text(&frame("heartbeat_ack")),
        vec![FeedEvent::Heartbeat]
    );
    f.subscribe_frames(&[
        sub("SBIN", "NSE", "3045", FeedMode::Quote),
        sub("NIFTY", "NSE_INDEX", "26000", FeedMode::Ltp),
        sub("NIFTY27OCT26FUT", "NFO", "54321", FeedMode::Depth),
    ]);
    let ev = f.parse_text(&frame("touchline_snapshot"));
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!(
        (t.symbol.as_str(), t.exchange.as_str(), t.mode),
        ("SBIN", "NSE", 2)
    );
    assert_eq!(
        (t.ltp, t.open, t.close, t.volume),
        (812.4, 809.0, 808.0, 1234567)
    );
    assert_eq!(t.last_trade_time_ms, 1790837699000);
    assert_eq!(t.change, 4.4);
    // Update overlays the snapshot; a zero open does not wipe it.
    let ev = f.parse_text(&frame("touchline_update"));
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!(
        (t.ltp, t.open, t.high, t.volume),
        (812.9, 809.0, 815.0, 1234600)
    );
    // Index under its OpenAlgo exchange.
    let ev = f.parse_text(&frame("index_tick"));
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!(
        (t.symbol.as_str(), t.exchange.as_str(), t.mode),
        ("NIFTY", "NSE_INDEX", 1)
    );
    // Depth.
    let ev = f.parse_text(&frame("depth_snapshot"));
    assert_eq!(ev.len(), 2);
    let FeedEvent::Depth(d) = &ev[1] else {
        panic!()
    };
    assert_eq!(d.buy[1].quantity, 150);
    assert_eq!(d.sell[4].price, 25080.5);
    assert_eq!(d.total_sell_quantity, 450);
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!(t.oi, 9876525);
    let ev = f.parse_text(&frame("depth_update"));
    let FeedEvent::Depth(d) = &ev[1] else {
        panic!()
    };
    assert_eq!(
        (d.buy[0].price, d.buy[0].quantity, d.buy[1].quantity),
        (25080.9, 150, 150)
    );
    // Unknown scrip ignored.
    assert!(f.parse_text(&frame("unsubscribed")).is_empty());
    assert!(f.parse_text("not json").is_empty());
    // Order updates.
    let ev = f.parse_text(&frame("order_update"));
    let FeedEvent::OrderUpdate(u) = &ev[0] else {
        panic!()
    };
    assert_eq!(u.symbol, "NIFTY27OCT2625000PE");
    assert_eq!(
        (
            u.order_status.as_str(),
            u.pricetype.as_str(),
            u.product.as_str()
        ),
        ("trigger pending", "SL-M", "NRML")
    );
    assert_eq!(
        (u.action.as_str(), u.quantity, u.pending_quantity),
        ("SELL", 75, 75)
    );
    let ev = f.parse_text(&frame("order_rejected"));
    let FeedEvent::OrderUpdate(u) = &ev[0] else {
        panic!()
    };
    assert_eq!(u.orderid, "26100300000004");
    assert_eq!(
        (
            u.order_status.as_str(),
            u.product.as_str(),
            u.symbol.as_str()
        ),
        ("rejected", "CNC", "YESBANK")
    );
    assert_eq!(u.rejection_reason, "RMS:Margin Exceeds");
    // Unsubscribe drops the routing entry and the cached snapshot.
    assert_eq!(f.cached(), 3);
    let u = f.unsubscribe_frames(&[
        sub("SBIN", "NSE", "3045", FeedMode::Quote),
        sub("NIFTY27OCT26FUT", "NFO", "54321", FeedMode::Depth),
    ]);
    assert_eq!(text_of(&u[0]), json!({"t":"u","k":"NSE|3045"}));
    assert_eq!(text_of(&u[1]), json!({"t":"ud","k":"NFO|54321"}));
    assert_eq!(f.cached(), 1);
    assert!(f.parse_text(&frame("touchline_update")).is_empty());
    // Reconnect clears the cache.
    f.parse_text(&frame("index_tick"));
    f.on_connected();
    assert_eq!(f.cached(), 0);
}
