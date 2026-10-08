//! Family unit tests. Payloads are built from the web's XTS code and the
//! XTS API shapes (`tests/fixtures/brokers/fivepaisaxts/`); no account data.

use super::auth::parse_session;
use super::binary;
use super::data::{
    depth_from, list_quotes, normalise_candles, ohlc_window, parse_ohlc, quote_from, quote_key,
};
use super::mapping::*;
use super::master_contract::{bse_index_symbol, parse_index_list, parse_segment};
use super::socketio::{self, EioPacket, SioPacket};
use super::streaming::{
    normalise_message, socket_url, token_transport_allowed, ws_base, xts_time_ms, Command,
    FeedSource, XtsFeed,
};
use super::*;
use crate::brokers::common::mapping::{Action, PriceType, Product, Validity};
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription, Message};
use chrono::NaiveDate;
use serde_json::{json, Value};

const BOOKS: &str = include_str!("../../../../tests/fixtures/brokers/fivepaisaxts/books.json");
const MARKET: &str = include_str!("../../../../tests/fixtures/brokers/fivepaisaxts/market.json");
const STREAM: &str = include_str!("../../../../tests/fixtures/brokers/fivepaisaxts/stream.json");

fn books() -> Value {
    serde_json::from_str(BOOKS).unwrap()
}
fn market() -> Value {
    serde_json::from_str(MARKET).unwrap()
}
fn stream(k: &str) -> String {
    let v: Value = serde_json::from_str(STREAM).unwrap();
    v[k].as_str().unwrap().to_string()
}

fn master() -> SymbolResolver {
    let mut rows = Vec::new();
    for (seg, body) in [
        (
            "NSECM",
            include_str!("../../../../tests/fixtures/brokers/fivepaisaxts/NSECM.txt"),
        ),
        (
            "NSEFO",
            include_str!("../../../../tests/fixtures/brokers/fivepaisaxts/NSEFO.txt"),
        ),
        (
            "BSECM",
            include_str!("../../../../tests/fixtures/brokers/fivepaisaxts/BSECM.txt"),
        ),
    ] {
        rows.extend(parse_segment(seg, body));
    }
    rows.extend(parse_index_list(1, &market()["indexlist_1"]["result"]));
    rows.extend(parse_index_list(11, &market()["indexlist_11"]["result"]));
    let r = SymbolResolver::new();
    r.load(rows);
    r
}

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

// ---------------------------------------------------------------------------
// Configs
// ---------------------------------------------------------------------------

fn all_configs() -> Vec<&'static XtsConfig> {
    vec![
        &crate::brokers::fivepaisaxts::CONFIG,
        &crate::brokers::jainamxts::CONFIG,
        &crate::brokers::compositedge::CONFIG,
        &crate::brokers::rmoney::CONFIG,
        &crate::brokers::ibulls::CONFIG,
        &crate::brokers::wisdom::CONFIG,
        &crate::brokers::iifl::CONFIG,
    ]
}

#[test]
fn every_member_config_is_well_formed() {
    for c in all_configs() {
        assert!(!c.base_url.ends_with('/'), "{}", c.id);
        assert!(c.base_url.starts_with("https://"), "{}", c.id);
        assert_eq!(c.interactive_path, "/interactive");
        assert!(c.socket_path.ends_with("/socket.io"), "{}", c.id);
        assert!(c.socket_login_path.ends_with("/auth/login"), "{}", c.id);
        assert!(c.master_segments.contains(&"NSECM"));
        assert!(c.supported_exchanges.contains(&Exchange::NseIndex));
        let b = XtsBroker::new(c, SymbolResolver::new());
        assert_eq!(b.id(), c.id);
        assert_eq!(b.login_kind(), LoginKind::ApiKeySecret);
        assert_eq!(b.timeframe_map().len(), 10);
        assert_eq!(b.capabilities().margin, c.hooks.margin_details);
    }
}

#[test]
fn member_deltas_match_the_audit_table() {
    use crate::brokers::*;
    let f = &fivepaisaxts::CONFIG;
    assert_eq!(f.base_url, "https://xtsmum.5paisa.com");
    assert_eq!(f.md_rest_path, "/apimarketdata");
    assert_eq!(f.socket_login_path, "/apibinarymarketdata/auth/login");
    assert_eq!(f.login, XtsLogin::Direct);
    assert_eq!(f.master_segments.len(), 4);
    assert_eq!(f.stream_mode_codes, [1512, 1501, 1502]);

    let j = &jainamxts::CONFIG;
    assert_eq!(j.base_url, "https://jtrade.jainam.in:5000");
    assert_eq!(j.md_rest_path, "/apibinarymarketdata");
    assert_eq!(j.socket_path, "/apibinarymarketdata/socket.io");
    assert_eq!(j.login, XtsLogin::DirectAccessToken);
    assert_eq!(j.binary_decoder, Some(BinaryDecoder::Jainam));

    let c = &compositedge::CONFIG;
    assert_eq!(c.login, XtsLogin::OAuthAccessToken);
    assert_eq!(c.master_segments.len(), 6);
    assert!(c.supported_exchanges.contains(&Exchange::Mcx));

    let r = &rmoney::CONFIG;
    assert_eq!(r.base_url, "https://xts.rmoneyindia.co.in:3000");
    assert_eq!(r.md_rest_path, "/apibinarymarketdata");
    assert_eq!(r.socket_path, "/apimarketdata/socket.io");
    assert_eq!(r.socket_login_path, "/apimarketdata/auth/login");
    assert!(!r.socket_login_source);
    assert_eq!(r.subscription_path, "/apimarketdata");
    assert_eq!(r.login, XtsLogin::OAuthSessionToken);
    assert_eq!(r.stream_mode_codes, [1501, 1501, 1502]);
    assert_eq!(r.mode_code(1), 1501);
    assert_eq!(r.hooks.funds_balance_header, Some("ALL|ALL|ALL"));
    assert!(r.hooks.margin_details && r.hooks.multiquote_oi);
    assert_eq!(r.binary_decoder, Some(BinaryDecoder::Rmoney));

    let i = &ibulls::CONFIG;
    assert_eq!(i.md_rest_path, "/apibinarymarketdata");
    assert_eq!(i.socket_path, "/apimarketdata/socket.io");
    assert_eq!(
        i.master_segments,
        &["NSECM", "NSEFO", "BSECM", "BSEFO", "MCXFO"]
    );
    assert!(!i.supported_exchanges.contains(&Exchange::Cds));

    assert_eq!(wisdom::CONFIG.base_url, "https://trade.wisdomcapital.in");
    assert_eq!(wisdom::CONFIG.name, "Wisdom Capital (XTS)");
    assert_eq!(iifl::CONFIG.base_url, "https://ttblaze.iifl.com");
    assert_eq!(iifl::CONFIG.master_segments.len(), 6);
    assert_eq!(f.mode_code(1), 1512);
    assert_eq!(f.mode_code(2), 1501);
    assert_eq!(f.mode_code(3), 1502);
}

#[test]
fn thirdparty_url_carries_key_return_url_and_state() {
    let u = thirdparty_url(
        "compositedge",
        "app key",
        "http://127.0.0.1:5000/compositedge/callback",
        "st1",
    )
    .unwrap();
    assert!(u.starts_with("https://xts.compositedge.com/interactive/thirdparty?appKey=app%20key"));
    assert!(u.contains(
        "returnURL=http%3A%2F%2F127.0.0.1%3A5000%2Fcompositedge%2Fcallback%3Fstate%3Dst1"
    ));
    let r = thirdparty_url("rmoney", "k", "http://x/rmoney/callback", "s").unwrap();
    assert!(r.starts_with("https://xts.rmoneyindia.co.in:3000/interactive/thirdparty?appKey=k"));
    assert!(thirdparty_url("iifl", "k", "r", "s").is_none());
    use crate::brokers::catalog;
    assert_eq!(catalog::auth_type("rmoney"), catalog::AuthType::OAuth);
    assert!(catalog::authorize_url("compositedge", "k", "r", "s").is_some());
    let p: std::collections::HashMap<String, String> =
        [("session".to_string(), "{}".to_string())].into();
    assert_eq!(catalog::extract_code("rmoney", &p).as_deref(), Some("{}"));
}

// ---------------------------------------------------------------------------
// Mapping
// ---------------------------------------------------------------------------

#[test]
fn exchange_maps() {
    assert_eq!(segment(Exchange::Nse), Some("NSECM"));
    assert_eq!(segment(Exchange::Mcx), Some("MCXFO"));
    assert_eq!(segment(Exchange::Cds), Some("NSECD"));
    assert_eq!(segment(Exchange::NseIndex), None);
    assert_eq!(history_segment(Exchange::NseIndex), Some("NSECM"));
    assert_eq!(history_segment(Exchange::BseIndex), Some("BSECM"));
    for (e, c) in [
        (Exchange::Nse, 1),
        (Exchange::NseIndex, 1),
        (Exchange::Nfo, 2),
        (Exchange::Cds, 3),
        (Exchange::Bse, 11),
        (Exchange::BseIndex, 11),
        (Exchange::Bfo, 12),
        (Exchange::Mcx, 51),
    ] {
        assert_eq!(segment_code(e), Some(c));
    }
    assert_eq!(segment_code(Exchange::Bcd), None);
    assert_eq!(oa_exchange("NSEFO"), "NFO");
    assert_eq!(oa_exchange("BSEFO"), "BFO");
    assert_eq!(oa_exchange("XYZ"), "XYZ");
    assert_eq!(exchange_for_code(51), Some("MCX"));
    assert_eq!(exchange_for_code(7), None);
}

#[test]
fn order_type_and_status_maps() {
    assert_eq!(order_type(PriceType::Sl), "STOPLIMIT");
    assert_eq!(order_type(PriceType::SlM), "STOPMARKET");
    assert_eq!(oa_order_type("StopLimit"), "SL");
    assert_eq!(oa_order_type("StopMarket"), "SL-M");
    assert_eq!(oa_order_type("Limit"), "LIMIT");
    assert_eq!(oa_status("Filled"), "complete");
    assert_eq!(oa_status("New"), "open");
    assert_eq!(oa_status("PartiallyFilled"), "open");
    assert_eq!(oa_status("Trigger Pending"), "trigger pending");
    assert_eq!(oa_status("Cancelled"), "cancelled");
    assert_eq!(oa_status("Rejected"), "rejected");
    assert_eq!(oa_status("PendingNew"), "pendingnew");
    assert!(is_token_error("Invalid Token"));
    assert!(is_token_error("Token expired"));
    assert!(!is_token_error("Order rejected"));
    assert_eq!(order_id(Some(&json!(1200002.0))), "1200002");
    assert_eq!(order_id(Some(&json!("1200003"))), "1200003");
    assert_eq!(order_id(Some(&json!(7))), "7");
}

fn resolved(symbols: &SymbolResolver, symbol: &str, ex: &str, pt: &str) -> ResolvedOrder {
    ResolvedOrder::resolve(
        &OrderRequest {
            symbol: symbol.into(),
            exchange: ex.into(),
            side: "BUY".into(),
            quantity: 50,
            price: 101.5,
            order_type: pt.into(),
            product: "NRML".into(),
            validity: "DAY".into(),
            trigger_price: Some(100.0),
            disclosed_quantity: None,
            amo: false,
        },
        symbols,
    )
    .unwrap()
}

#[test]
fn place_and_modify_payloads_are_typed() {
    let m = master();
    let o = resolved(&m, "NIFTY25APR2422500CE", "NFO", "SL");
    assert_eq!(o.validity, Validity::Day);
    assert_eq!(
        place_payload(&o).unwrap(),
        json!({
            "exchangeSegment": "NSEFO",
            "exchangeInstrumentID": 43210,
            "productType": "NRML",
            "orderType": "STOPLIMIT",
            "orderSide": "BUY",
            "timeInForce": "DAY",
            "disclosedQuantity": 0,
            "orderQuantity": 50,
            "limitPrice": 101.5,
            "stopPrice": 100.0,
            "orderUniqueIdentifier": "openalgo",
        })
    );
    let mut idx = o.clone();
    idx.exchange = Exchange::NseIndex;
    assert!(place_payload(&idx).is_none());
    let md = ResolvedModify::resolve(
        "1200001",
        &ModifyOrderRequest {
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            action: "BUY".into(),
            product: "MIS".into(),
            pricetype: "LIMIT".into(),
            quantity: 5,
            price: 781.0,
            trigger_price: 0.0,
            disclosed_quantity: 0,
        },
        &m,
    )
    .unwrap();
    let p = modify_payload(&md);
    assert_eq!(p["appOrderID"], 1200001);
    assert_eq!(p["modifiedOrderType"], "LIMIT");
    assert_eq!(p["modifiedProductType"], "MIS");
    assert_eq!(p["modifiedOrderQuantity"], 5);
    assert_eq!(p["modifiedLimitPrice"], 781.0);
    assert_eq!(p["modifiedTimeInForce"], "DAY");
    let x = exit_payload("NSECM", &json!("2885"), "MIS", -7);
    assert_eq!(x["orderSide"], "BUY");
    assert_eq!(x["orderQuantity"], 7);
    assert_eq!(x["orderType"], "MARKET");
    assert_eq!(x["exchangeInstrumentID"], "2885");
}

#[test]
fn order_book_uses_openalgo_symbols_and_statuses() {
    let m = master();
    let o = orders(&books()["orders"]["result"], &m);
    assert_eq!(o.len(), 6);
    assert_eq!(o[0].symbol, "RELIANCE");
    assert_eq!(o[0].exchange, "NSE");
    assert_eq!(o[0].status, "open");
    assert_eq!(o[0].order_type, "LIMIT");
    assert_eq!(o[0].order_id, "1200001");
    assert_eq!(o[0].pending_quantity, 10);
    assert_eq!(o[1].symbol, "NIFTY25APR2422500CE");
    assert_eq!(o[1].exchange, "NFO");
    assert_eq!(o[1].order_id, "1200002");
    assert_eq!(o[1].status, "complete");
    assert_eq!(o[1].average_price, 101.25);
    assert_eq!(o[1].filled_quantity, 50);
    assert_eq!(o[2].status, "trigger pending");
    assert_eq!(o[2].order_type, "SL");
    assert_eq!(o[2].trigger_price, 779.0);
    assert_eq!(o[3].order_type, "SL-M");
    assert_eq!(o[3].rejection_reason.as_deref(), Some("RMS:Margin Exceeds"));
    assert_eq!(o[3].exchange_order_id, None);
    assert_eq!(o[4].status, "open");
    assert_eq!(o[5].status, "cancelled");
    let pending: Vec<_> = o
        .iter()
        .filter(|x| {
            x.status
                .parse::<crate::brokers::common::mapping::OrderStatus>()
                .map(|s| s.is_pending())
                .unwrap_or(false)
        })
        .map(|x| x.order_id.as_str())
        .collect();
    assert_eq!(pending, ["1200001", "1200003", "1200005"]);
}

#[test]
fn trade_book() {
    let t = trades(&books()["trades"]["result"], &master());
    assert_eq!(t[0].symbol, "NIFTY25APR2422500CE");
    assert_eq!(t[0].trade_id, "T0001");
    assert_eq!(t[0].trade_value, 50.0 * 101.25);
    // Unknown token keeps the broker symbol; string prices are read.
    assert_eq!(t[1].symbol, "UNKNOWN-EQ");
    assert_eq!(t[1].average_price, 99.5);
}

#[test]
fn positions_pick_average_by_sign_and_accept_every_id_key() {
    let m = master();
    let p = positions(&books()["positions"]["result"], &m);
    assert_eq!(p.len(), 3);
    assert_eq!((p[0].symbol.as_str(), p[0].quantity), ("RELIANCE", 10));
    assert_eq!(p[0].average_price, 2501.4);
    assert_eq!(p[0].pnl, 12.5);
    assert_eq!(p[1].symbol, "NIFTY25APR2422500CE");
    assert_eq!(p[1].quantity, -50);
    assert_eq!(p[1].average_price, 101.25);
    assert_eq!(p[2].quantity, 0);
    assert_eq!(p[2].average_price, 0.0);
    assert_eq!(p[2].realized_pnl, 10.0);
    let flat = positions(&books()["positions_flat"]["result"], &m);
    assert_eq!(flat[0].symbol, "SBIN");
    assert_eq!(flat[0].average_price, 781.0);
    assert_eq!(flat[1].symbol, "RELIANCE");
    assert_eq!(position_list(&json!({"positionList": []})).len(), 0);
}

#[test]
fn holdings_by_isin() {
    let h = holdings(&books()["holdings"]["result"], &master());
    assert_eq!(h.len(), 2);
    let rel = h.iter().find(|x| x.symbol == "RELIANCE").unwrap();
    assert_eq!(rel.product, "CNC");
    assert_eq!(rel.quantity, 4);
    assert_eq!(rel.current_value, 4.0 * 2400.5);
    assert_eq!(rel.isin.as_deref(), Some("INE002A01018"));
    assert!(h.iter().any(|x| x.symbol == "INE000Z01011"));
    assert!(holdings(&json!({}), &master()).is_empty());
}

#[test]
fn funds_first_entry_or_named_header() {
    let r = &books()["balance"]["result"];
    let first = funds(r, None).unwrap();
    assert_eq!(first.available_cash, 0.0);
    let all = funds(r, Some("ALL|ALL|ALL")).unwrap();
    assert_eq!(all.available_cash, 85001.0);
    assert_eq!(all.collateral, 2500.46);
    assert_eq!(all.utilised_debits, 15000.1);
    assert_eq!(all.m2m_unrealized, -27.5);
    assert_eq!(all.m2m_realized, 0.0, "nan reads as 0");
    assert_eq!(funds(r, Some("NOPE")).unwrap(), first);
    assert!(funds(&json!({"BalanceList": []}), None).is_none());
}

#[test]
fn rmoney_margin_mapping() {
    let leg = MarginLeg {
        key: QuoteKey::new("NFO", "NIFTY25APR2422500CE"),
        action: Action::Sell,
        quantity: 50,
        product: Product::Nrml,
        pricetype: PriceType::Limit,
        price: 100.0,
        trigger_price: 0.0,
    };
    assert_eq!(
        margin_leg(&leg, Exchange::Nfo, "43210").unwrap(),
        json!({"exchange": 2, "exchangeInstrumentId": 43210, "productType": "NRML",
               "orderType": "LIMIT", "orderSide": "SELL", "quantity": 50, "price": 100.0,
               "stopPrice": 0.0, "orderSessionType": 1})
    );
    assert!(margin_leg(&leg, Exchange::NseIndex, "x").is_none());
    let r = margin_result(&books()["margin_ok"]["result"]).unwrap();
    assert_eq!(r.total_margin_required, 150.75);
    assert!(margin_result(&json!({})).is_none());
}

#[test]
fn callback_session_parsing() {
    assert_eq!(
        parse_session(r#"{"accessToken":"a1"}"#)["accessToken"],
        "a1"
    );
    // Double encoded, like the web's second json.loads.
    assert_eq!(
        parse_session(r#""{\"token\":\"t1\",\"userID\":\"U\"}""#)["token"],
        "t1"
    );
    assert_eq!(parse_session("rawtoken"), json!("rawtoken"));
}

// ---------------------------------------------------------------------------
// Market data
// ---------------------------------------------------------------------------

#[test]
fn quote_and_depth_from_1502() {
    let list = list_quotes(&market()["quotes_1502"]);
    assert_eq!(list.len(), 1);
    assert_eq!(quote_key(&list[0]), "1_2885");
    let k = QuoteKey::new("NSE", "RELIANCE");
    let q = quote_from(&list[0], 0, &k);
    assert_eq!(q.ltp, 2500.25);
    assert_eq!(q.close, 2487.6);
    assert_eq!(q.bid, 2500.1);
    assert_eq!(q.ask, 2500.4);
    assert_eq!((q.bid_qty, q.ask_qty), (120, 95));
    assert_eq!(q.volume, 4500123);
    assert_eq!(q.change, 12.65);
    let d = depth_from(&list[0], 9, &k);
    assert_eq!(d.bids.len(), 5);
    assert_eq!(d.asks.len(), 5);
    assert_eq!(d.bids[1].price, 2500.05);
    assert_eq!(d.bids[0].orders, 3);
    assert_eq!(d.asks[1], DepthLevel::default());
    assert_eq!(d.ltq, 7);
    assert_eq!(d.oi, 9);
    assert_eq!((d.total_buy_qty, d.total_sell_qty), (150000, 160000));
    assert_eq!(d.prev_close, 2487.6);
}

#[test]
fn ohlc_rows_and_timestamps() {
    let raw = market()["ohlc_minute"]["result"]["dataReponse"]
        .as_str()
        .unwrap()
        .to_string();
    let c = parse_ohlc(&raw);
    assert_eq!(c.len(), 3, "short rows are skipped");
    let n = normalise_candles(c, "60");
    assert_eq!(n.len(), 2, "duplicates removed");
    // 09:15 IST written as UTC by XTS -> 03:45 UTC.
    assert_eq!(n[0].timestamp, 1704273300 - 19800);
    assert_eq!(n[1].close, 2495.5);
    let five = normalise_candles(parse_ohlc("1704273420|1|1|1|1|1"), "300");
    assert_eq!(five[0].timestamp % 300, 0);
    let d = normalise_candles(
        parse_ohlc(
            market()["ohlc_day"]["result"]["dataReponse"]
                .as_str()
                .unwrap(),
        ),
        "D",
    );
    assert!(d.iter().all(|c| c.timestamp % 86400 == 0));
    assert_eq!(d[0].timestamp, 1704240000);
    assert!(parse_ohlc("").is_empty());
    let (a, b) = ohlc_window(
        NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(),
        NaiveDate::from_ymd_opt(2026, 1, 7).unwrap(),
    );
    assert_eq!(a, "Jan 02 2026 000000");
    assert_eq!(b, "Jan 07 2026 235959");
}

// ---------------------------------------------------------------------------
// Master contract
// ---------------------------------------------------------------------------

#[test]
fn master_cash_segments() {
    let nse = parse_segment(
        "NSECM",
        include_str!("../../../../tests/fixtures/brokers/fivepaisaxts/NSECM.txt"),
    );
    assert_eq!(nse.len(), 2, "only EQ series");
    assert_eq!(nse[0].symbol, "RELIANCE");
    assert_eq!(nse[0].brsymbol, "RELIANCE");
    assert_eq!(nse[0].exchange, "NSE");
    assert_eq!(nse[0].brexchange, "NSECM");
    assert_eq!(nse[0].token, "2885");
    assert_eq!(nse[0].instrument_type, "EQ");
    assert_eq!(nse[0].strike, 1.0);
    assert_eq!(nse[0].expiry, "");
    let bse = parse_segment(
        "BSECM",
        include_str!("../../../../tests/fixtures/brokers/fivepaisaxts/BSECM.txt"),
    );
    assert_eq!(bse.len(), 3);
    assert_eq!(bse[0].exchange, "BSE");
    assert_eq!(bse[1].exchange, "BSE_INDEX");
    assert_eq!(bse[1].symbol, "SENSEX");
    assert_eq!(bse[2].symbol, "BSEINFORMATIONTECHNOLOGY");
    assert_eq!(bse[2].name, "BSEINFORMATIONTECHNOLOGY");
    assert_eq!(bse_index_symbol(" bse   cg "), "BSECAPITALGOODS");
    assert_eq!(bse_index_symbol("S&P BSE-100"), "S&PBSE100");
}

#[test]
fn master_derivatives() {
    let fo = parse_segment(
        "NSEFO",
        include_str!("../../../../tests/fixtures/brokers/fivepaisaxts/NSEFO.txt"),
    );
    assert_eq!(fo.len(), 3);
    assert_eq!(fo[0].symbol, "BANKNIFTY24APR24FUT");
    assert_eq!(fo[0].instrument_type, "FUT");
    assert_eq!(fo[0].expiry, "24-APR-24");
    assert_eq!(fo[0].lot_size, 15);
    assert_eq!(fo[0].brsymbol, "BANKNIFTY24APRFUT");
    assert_eq!(fo[0].strike, 1.0);
    assert_eq!(fo[1].symbol, "NIFTY25APR2422500CE");
    assert_eq!(fo[1].instrument_type, "CE");
    assert_eq!(fo[1].strike, 22500.0);
    assert_eq!(fo[1].name, "NIFTY");
    assert_eq!(fo[2].symbol, "VEDL25APR24292.5PE");
    assert_eq!(fo[2].exchange, "NFO");
    let cd = parse_segment(
        "NSECD",
        include_str!("../../../../tests/fixtures/brokers/fivepaisaxts/NSECD.txt"),
    );
    assert_eq!(cd[0].symbol, "USDINR29MAY24FUT");
    assert_eq!(cd[1].symbol, "USDINR24MAY2483.25CE");
    assert_eq!(cd[1].tick_size, 0.0025);
    assert_eq!(cd[1].exchange, "CDS");
    let mcx = parse_segment(
        "MCXFO",
        include_str!("../../../../tests/fixtures/brokers/fivepaisaxts/MCXFO.txt"),
    );
    assert_eq!(mcx.len(), 1, "ContractExpiration == 1 rows are dropped");
    assert_eq!(mcx[0].symbol, "CRUDEOIL20MAY24FUT");
    assert_eq!(mcx[0].lot_size, 100);
    assert!(parse_segment("XYZ", "a|b").is_empty());
}

#[test]
fn master_index_lists() {
    let n = parse_index_list(1, &market()["indexlist_1"]["result"]);
    let syms: Vec<&str> = n.iter().map(|r| r.symbol.as_str()).collect();
    assert_eq!(
        syms,
        [
            "NIFTY",
            "BANKNIFTY",
            "INDIAVIX",
            "NIFTY100",
            "HANGSENGBEESNAV"
        ]
    );
    assert_eq!(n[0].token, "26000");
    assert_eq!(n[0].brsymbol, "NIFTY 50_26000");
    assert_eq!(n[0].exchange, "NSE_INDEX");
    assert_eq!(n[0].brexchange, "NSE_INDEX");
    assert_eq!(n[0].instrument_type, "INDEX");
    assert_eq!(n[0].lot_size, 1);
    let b = parse_index_list(11, &market()["indexlist_11"]["result"]);
    let syms: Vec<&str> = b.iter().map(|r| r.symbol.as_str()).collect();
    assert_eq!(syms, ["SENSEX", "BANKEX", "BSEINFORMATIONTECHNOLOGY"]);
    assert!(parse_index_list(1, &json!({})).is_empty());
    let m = master();
    assert_eq!(m.by_token("NSE_INDEX", "26000").unwrap().symbol, "NIFTY");
}

// ---------------------------------------------------------------------------
// Socket.IO codec
// ---------------------------------------------------------------------------

#[test]
fn engine_io_and_socket_io_packets() {
    let open = stream("open");
    match socketio::decode_eio(&open) {
        Some(EioPacket::Open(v)) => assert_eq!(v["pingInterval"], 25000),
        other => panic!("{:?}", other),
    }
    assert_eq!(socketio::decode_eio("2"), Some(EioPacket::Ping("")));
    assert_eq!(
        socketio::decode_eio("3probe"),
        Some(EioPacket::Pong("probe"))
    );
    assert_eq!(socketio::decode_eio("6"), Some(EioPacket::Noop));
    assert_eq!(socketio::decode_eio("x"), None);
    assert_eq!(socketio::decode_eio(""), None);
    assert_eq!(socketio::pong(""), "3");
    assert!(matches!(
        socketio::decode_sio("0{\"sid\":\"a\"}"),
        Some(SioPacket::Connect(_))
    ));
    match socketio::decode_sio(r#"4{"message":"Invalid token"}"#) {
        Some(SioPacket::ConnectError(v)) => {
            assert_eq!(socketio::error_message(&v), "Invalid token")
        }
        other => panic!("{:?}", other),
    }
    match socketio::decode_sio(r#"2["1501-json-full","{\"a\":1}"]"#) {
        Some(SioPacket::Event { name, args }) => {
            assert_eq!(name, "1501-json-full");
            assert_eq!(args[0], "{\"a\":1}");
        }
        other => panic!("{:?}", other),
    }
    // Namespace and ack id are skipped.
    assert!(matches!(
        socketio::decode_sio(r#"2/md,12["joined","x"]"#),
        Some(SioPacket::Event { .. })
    ));
    match socketio::decode_sio(r#"51-["xts-binary-packet",{"_placeholder":true,"num":0}]"#) {
        Some(SioPacket::BinaryEvent {
            attachments, name, ..
        }) => {
            assert_eq!(attachments, 1);
            assert_eq!(name, "xts-binary-packet");
        }
        other => panic!("{:?}", other),
    }
    assert_eq!(socketio::decode_sio("5x"), None);
    assert_eq!(socketio::decode_sio("2notjson"), None);
    assert_eq!(socketio::encode_event("e", &[json!("x")]), r#"42["e","x"]"#);
    let mut pkt = vec![4u8, 4, 0];
    pkt.extend([0u8; 20]);
    assert_eq!(socketio::strip_eio3_prefix(&pkt).len(), pkt.len() - 1);
    let raw = packet(4, 1501, 1, 2885, &[0u8; 8]);
    assert_eq!(socketio::strip_eio3_prefix(&raw), &raw[..]);
}

#[test]
fn token_only_travels_over_tls_or_loopback() {
    for cfg in [
        &crate::brokers::fivepaisaxts::CONFIG,
        &crate::brokers::jainamxts::CONFIG,
        &crate::brokers::compositedge::CONFIG,
        &crate::brokers::iifl::CONFIG,
        &crate::brokers::ibulls::CONFIG,
        &crate::brokers::wisdom::CONFIG,
        &crate::brokers::rmoney::CONFIG,
    ] {
        assert!(
            cfg.base_url.starts_with("https://"),
            "{} must be served over TLS",
            cfg.id
        );
        let sub = format!(
            "{}{}/instruments/subscription",
            cfg.base_url, cfg.subscription_path
        );
        assert!(token_transport_allowed(&sub), "{}", sub);
    }
    assert!(token_transport_allowed("http://127.0.0.1:9/x"));
    assert!(token_transport_allowed("http://localhost:9/x"));
    assert!(token_transport_allowed("http://[::1]:9/x"));
    assert!(!token_transport_allowed("http://xts.example.com/x"));
    assert!(!token_transport_allowed("http://10.0.0.5/x"));
    assert!(!token_transport_allowed("ws://127.0.0.1/x"));
    assert!(!token_transport_allowed("not a url"));
}

#[tokio::test]
async fn subscription_call_refuses_cleartext_host() {
    let err = super::streaming::subscription_call(
        &reqwest::Client::new(),
        "http://xts.example.invalid/apimarketdata/instruments/subscription",
        "t",
        true,
        1501,
        &[],
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AppError::Broker(m) if m.contains("not secure")));
}

#[test]
fn socket_url_has_query_and_eio4() {
    let c = &crate::brokers::fivepaisaxts::CONFIG;
    assert_eq!(ws_base("https://h:3000"), "wss://h:3000");
    assert_eq!(ws_base("http://127.0.0.1:9"), "ws://127.0.0.1:9");
    let u = socket_url(c, "https://xtsmum.5paisa.com", "t+k", "U1");
    assert_eq!(
        u,
        "wss://xtsmum.5paisa.com/apimarketdata/socket.io/?token=t%2Bk&userID=U1&publishFormat=JSON&broadcastMode=FULL&EIO=4&transport=websocket"
    );
    let r = socket_url(&crate::brokers::rmoney::CONFIG, "https://h", "t", "U");
    assert!(r.contains("broadcastMode=Full"));
}

// ---------------------------------------------------------------------------
// Binary decoders (offsets from jainam `ws:580-1054`, rmoney `ws:739-844`)
// ---------------------------------------------------------------------------

fn packet(kind: u16, code: u16, seg: i16, id: i32, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend(kind.to_le_bytes());
    v.extend(code.to_le_bytes());
    v.extend(seg.to_le_bytes());
    v.extend(id.to_le_bytes());
    v.extend(1i16.to_le_bytes());
    v.extend(1i16.to_le_bytes());
    v.extend((payload.len() as u16).to_le_bytes());
    v.extend(payload);
    v
}

fn put_f64(p: &mut [u8], o: usize, v: f64) {
    p[o..o + 8].copy_from_slice(&v.to_le_bytes());
}

#[test]
fn binary_header() {
    let p = packet(260, 1502, 2, 43210, &[0u8; 4]);
    let h = binary::header(&p).unwrap();
    assert_eq!(h.packet_type, 260);
    assert!(h.compressed());
    assert_eq!(h.message_code, 1502);
    assert_eq!(h.segment, 2);
    assert_eq!(h.instrument_id, 43210);
    assert_eq!(h.uncompressed_size, 4);
    assert!(binary::header(&p[..15]).is_none());
    assert!(binary::decode_rmoney(&p).is_none(), "compressed is skipped");
    assert!(binary::decode_jainam(&p).is_none());
}

#[test]
fn rmoney_binary_touchline() {
    let mut pl = vec![0u8; 230];
    pl[0..2].copy_from_slice(&1501u16.to_le_bytes());
    put_f64(&mut pl, 48, 2500.25);
    put_f64(&mut pl, 156, 2490.0);
    put_f64(&mut pl, 164, 2510.5);
    put_f64(&mut pl, 172, 2485.0);
    put_f64(&mut pl, 180, 2487.6);
    pl[188..196].copy_from_slice(&4_500_123u64.to_le_bytes());
    let v = binary::decode_rmoney(&packet(4, 1501, 1, 2885, &pl)).unwrap();
    assert_eq!(v["LastTradedPrice"], 2500.25);
    assert_eq!(v["Open"], 2490.0);
    assert_eq!(v["Close"], 2487.6);
    assert_eq!(v["TotalTradedQuantity"], 4_500_123);
    assert_eq!(v["MessageCode"], 1501);
    assert_eq!(v["ExchangeInstrumentID"], 2885);
    // 1512: LTP right after the message code.
    let mut lp = vec![0u8; 40];
    put_f64(&mut lp, 2, 781.35);
    let v = binary::decode_rmoney(&packet(4, 1512, 1, 3045, &lp)).unwrap();
    assert_eq!(v["LastTradedPrice"], 781.35);
    // Nothing plausible -> nothing.
    assert!(binary::decode_rmoney(&packet(4, 1512, 1, 3045, &[0u8; 40])).is_none());
    assert!(binary::decode_rmoney(&packet(4, 1700, 1, 3045, &lp)).is_none());
}

#[test]
fn jainam_binary_ltp_touchline_and_depth() {
    let mut lp = vec![0u8; 40];
    put_f64(&mut lp, 10, 781.35);
    let v = binary::decode_jainam(&packet(4, 1512, 1, 3045, &lp)).unwrap();
    assert_eq!(v["LastTradedPrice"], 781.35);

    let mut tl = vec![0u8; 200];
    for (o, x) in [(156, 2490.0), (164, 2510.5), (172, 2485.0), (180, 2487.6)] {
        put_f64(&mut tl, o, x);
    }
    put_f64(&mut tl, 48, 2500.25);
    put_f64(&mut tl, 10, 9.0); // outside the OHLC window, skipped
    let v = binary::decode_jainam(&packet(4, 1501, 1, 2885, &tl)).unwrap();
    assert_eq!(v["LastTradedPrice"], 2500.25);
    assert_eq!(v["High"], 2510.5);

    // 1502: LTP at 166, bids at 52 + i*22, asks scanned after 162.
    let mut dp = vec![0u8; 260];
    put_f64(&mut dp, 166, 2500.25);
    for i in 0..5 {
        let o = 52 + i * 22;
        put_f64(&mut dp, o, 2500.0 - i as f64 * 0.05);
        dp[o + 8..o + 12].copy_from_slice(&(100u32 + i as u32).to_le_bytes());
        dp[o + 14..o + 16].copy_from_slice(&(2u16).to_le_bytes());
    }
    let v = binary::decode_jainam(&packet(4, 1502, 1, 2885, &dp)).unwrap();
    assert_eq!(v["LastTradedPrice"], 2500.25);
    let bids = v["Bids"].as_array().unwrap();
    assert!(bids.len() >= 2);
    let prices: Vec<f64> = bids.iter().map(|b| b["Price"].as_f64().unwrap()).collect();
    assert!(
        prices.windows(2).all(|w| w[0] >= w[1]),
        "bids sorted descending"
    );
    assert!(v["Asks"].as_array().is_some());
    // No invented levels: an empty book stays empty.
    let mut lonely = vec![0u8; 260];
    put_f64(&mut lonely, 166, 2500.25);
    let v = binary::decode_jainam(&packet(4, 1502, 1, 2885, &lonely)).unwrap();
    for side in ["Bids", "Asks"] {
        for l in v[side].as_array().unwrap() {
            assert_ne!(l["Price"], json!(2497.75));
            assert_ne!(l["Price"], json!(2502.75));
        }
    }
    assert!(binary::decode_jainam(&packet(4, 1510, 1, 2885, &lp)).is_none());
}

#[test]
fn text_1105() {
    let v = binary::parse_1105("t:1_2885,110:2501.5,111:3,112:4500200,999:x,117:2487.6").unwrap();
    assert_eq!(v["ExchangeSegment"], 1);
    assert_eq!(v["ExchangeInstrumentID"], 2885);
    assert_eq!(v["LastTradedPrice"], 2501.5);
    assert_eq!(v["Close"], 2487.6);
    assert!(v.get("MessageCode").is_none());
    assert!(binary::parse_1105("x:1_2").is_none());
    assert!(binary::parse_1105("t:12").is_none());
}

// ---------------------------------------------------------------------------
// Feed decoding
// ---------------------------------------------------------------------------

fn feed(cfg: &'static XtsConfig) -> XtsFeed {
    let mut f = XtsFeed::new(
        cfg,
        reqwest::Client::new(),
        "https://example.invalid".into(),
        None,
        FeedSource::stored("t", "U"),
    );
    f.insert(sub("SBIN", "NSE", "3045", FeedMode::Ltp));
    f.insert(sub("RELIANCE", "NSE", "2885", FeedMode::Depth));
    f.insert(sub("NIFTY25APR2422500CE", "NFO", "43210", FeedMode::Quote));
    f.insert(sub("NIFTY", "NSE_INDEX", "26000", FeedMode::Ltp));
    f
}

fn ticks(ev: &[FeedEvent]) -> Vec<&crate::brokers::common::streaming::NormalizedTick> {
    ev.iter()
        .filter_map(|e| match e {
            FeedEvent::Tick(t) => Some(t),
            _ => None,
        })
        .collect()
}

#[test]
fn feed_decodes_json_events() {
    use crate::brokers::common::streaming::BrokerFeed;
    let mut f = feed(&crate::brokers::fivepaisaxts::CONFIG);
    let ev = f.parse(&Message::Text(stream("ltp_1512")));
    let t = ticks(&ev);
    assert_eq!(t.len(), 1);
    assert_eq!((t[0].symbol.as_str(), t[0].mode), ("SBIN", 1));
    assert_eq!(t[0].ltp, 781.35);
    assert_eq!(t[0].last_quantity, 5);
    assert_eq!(t[0].last_trade_time_ms, xts_time_ms(1388056311));

    let ev = f.parse(&Message::Text(stream("quote_1501")));
    let t = ticks(&ev);
    assert_eq!(t[0].mode, 2);
    assert_eq!(t[0].open, 2490.0);
    assert_eq!(t[0].volume, 4500123);
    assert_eq!(t[0].average_price, 2495.8);
    assert_eq!(t[0].change, 12.65);

    let ev = f.parse(&Message::Text(stream("quote_1501_root")));
    let t = ticks(&ev);
    assert_eq!(t[0].symbol, "NIFTY25APR2422500CE");
    assert_eq!(t[0].close, 100.0);
    assert_eq!(t[0].total_sell_quantity, 6000);

    let ev = f.parse(&Message::Text(stream("depth_1502")));
    assert_eq!(ev.len(), 2);
    assert_eq!(ticks(&ev)[0].mode, 3);
    match &ev[1] {
        FeedEvent::Depth(d) => {
            assert_eq!(d.symbol, "RELIANCE");
            assert_eq!(d.buy.len(), 5);
            assert_eq!(d.buy[0].price, 2500.1);
            assert_eq!(d.buy[0].quantity, 120);
            assert_eq!(d.sell[0].orders, 4);
            assert_eq!(d.total_sell_quantity, 160000);
        }
        other => panic!("{:?}", other),
    }

    let ev = f.parse(&Message::Text(stream("index_1512")));
    assert_eq!(ticks(&ev)[0].exchange, "NSE_INDEX");

    let ev = f.parse(&Message::Text(stream("text_1105")));
    let t = ticks(&ev);
    assert_eq!((t[0].symbol.as_str(), t[0].mode), ("RELIANCE", 2));
    assert_eq!(t[0].ltp, 2501.5);
    assert_eq!(t[0].total_buy_quantity, 150500);

    assert!(f.parse(&Message::Text(stream("unsubscribed"))).is_empty());
    assert!(f.parse(&Message::Text(stream("joined"))).is_empty());
    // An Engine.IO ping is answered with a pong through the manager.
    assert_eq!(
        f.parse(&Message::Text("2".into())),
        vec![
            FeedEvent::Reply(Message::Text("3".into())),
            FeedEvent::Heartbeat
        ]
    );
    assert_eq!(
        f.parse(&Message::Text(stream("connect_ack"))),
        vec![FeedEvent::AuthOk]
    );
    assert_eq!(
        f.parse(&Message::Ping(Vec::new())),
        vec![FeedEvent::Heartbeat]
    );
    // Snapshots from the subscribe answer have the full-event shape.
    let snap = market()["subscription_ok"]["result"]["listQuotes"][0].clone();
    let ev = f.parse(&Message::Text(socketio::encode_event(
        "1512-json-full",
        &[snap],
    )));
    assert_eq!(ticks(&ev)[0].ltp, 2500.25);
}

#[test]
fn feed_decodes_binary_for_members_with_a_decoder() {
    use crate::brokers::common::streaming::BrokerFeed;
    let mut lp = vec![0u8; 40];
    put_f64(&mut lp, 2, 781.35);
    let pkt = packet(4, 1512, 1, 3045, &lp);
    let mut r = feed(&crate::brokers::rmoney::CONFIG);
    // An attachment is decoded only after its placeholder event.
    assert!(r.parse(&Message::Binary(pkt.clone())).is_empty());
    assert!(r.parse(&Message::Text(stream("binary_event"))).is_empty());
    let ev = r.parse(&Message::Binary(pkt.clone()));
    assert_eq!(ticks(&ev)[0].ltp, 781.35);
    let mut f = feed(&crate::brokers::fivepaisaxts::CONFIG);
    f.parse(&Message::Text(stream("binary_event")));
    assert!(f.parse(&Message::Binary(pkt)).is_empty());
}

#[test]
fn subscribe_frames_group_by_message_code_and_unsubscribe_forgets() {
    use crate::brokers::common::streaming::BrokerFeed;
    let mut f = XtsFeed::new(
        &crate::brokers::fivepaisaxts::CONFIG,
        reqwest::Client::new(),
        "https://example.invalid".into(),
        None,
        FeedSource::default(),
    );
    let subs = [
        sub("SBIN", "NSE", "3045", FeedMode::Ltp),
        sub("RELIANCE", "NSE", "2885", FeedMode::Ltp),
        sub("NIFTY", "NSE_INDEX", "26000", FeedMode::Depth),
        sub("X", "NCDEX", "1", FeedMode::Ltp),
    ];
    // Subscriptions are REST calls, never socket frames.
    let cmds: Vec<Command> = f.commands(&subs, true);
    assert_eq!(cmds.len(), 2);
    assert!(f.subscribe_frames(&subs).is_empty());
    assert!(cmds.iter().all(|c| c.subscribe));
    assert_eq!(cmds[0].code, 1512);
    assert_eq!(
        cmds[0].instruments,
        vec![
            json!({"exchangeSegment": 1, "exchangeInstrumentID": 3045}),
            json!({"exchangeSegment": 1, "exchangeInstrumentID": 2885})
        ]
    );
    assert_eq!(cmds[1].code, 1502);
    let ev = f.parse(&Message::Text(stream("ltp_1512")));
    assert_eq!(ticks(&ev).len(), 1);
    let un = f.commands(&subs[..1], false);
    assert!(!un[0].subscribe);
    assert!(f.unsubscribe_frames(&subs[..1]).is_empty());
    assert!(f.parse(&Message::Text(stream("ltp_1512"))).is_empty());
}

#[test]
fn normalise_ignores_unknown_codes_and_converts_xts_time() {
    let s = sub("SBIN", "NSE", "3045", FeedMode::Ltp);
    assert!(normalise_message(&json!({"MessageCode": 1510, "OpenInterest": 5}), &s).is_empty());
    assert_eq!(xts_time_ms(0), 0);
    // 1388056311 s after 1980-01-01 IST = 2023-12-26 11:11:51 IST.
    assert_eq!(xts_time_ms(1_388_056_311), 1_703_569_311_000);
}

#[tokio::test]
async fn feed_speaks_engine_io() {
    use crate::brokers::common::streaming::BrokerFeed;
    let mut f = feed(&crate::brokers::fivepaisaxts::CONFIG);
    assert!(f.awaits_auth_ack());
    assert!(f.is_auth_failure(Some(400)));
    f.on_connected();
    // The server's open gets the Socket.IO connect, written by the manager.
    assert_eq!(
        f.parse(&Message::Text(stream("open"))),
        vec![FeedEvent::Reply(Message::Text("40".into()))]
    );
    assert_eq!(
        f.parse(&Message::Text(stream("connect_ack"))),
        vec![FeedEvent::AuthOk]
    );
    assert_eq!(
        f.parse(&Message::Text(stream("ping"))),
        vec![
            FeedEvent::Reply(Message::Text("3".into())),
            FeedEvent::Heartbeat
        ]
    );
    assert!(matches!(
        f.parse(&Message::Text(stream("connect_error")))[0],
        FeedEvent::AuthFailed(_)
    ));
    // With a stored feed token, prepare builds the socket address.
    f.prepare().await.unwrap();
    assert!(f
        .ws_request()
        .unwrap()
        .uri()
        .query()
        .unwrap()
        .contains("token=t&userID=U"));
    // Without a token (no market keys, nothing stored) the session is
    // refused before any connect.
    let mut none = XtsFeed::new(
        &crate::brokers::fivepaisaxts::CONFIG,
        reqwest::Client::new(),
        "https://example.invalid".into(),
        None,
        FeedSource::default(),
    );
    assert!(matches!(
        none.prepare().await,
        Err(crate::brokers::common::streaming::PrepareError::AuthFailed(
            _
        ))
    ));
    // Subscribing starts one owned REST worker; it is dropped with the feed.
    f.subscribe_frames(&[sub("SBIN", "NSE", "3045", FeedMode::Ltp)]);
    drop(f);
}
