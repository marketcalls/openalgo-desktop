//! Nuvama mapping tests against payloads built from the web plugin's field
//! names (`src-tauri/tests/fixtures/brokers/nuvama/`; openalgo ee123273b,
//! `broker/nuvama/**`). No live capture exists here: each fixture follows
//! the keys the web adapter reads, and each test names the web function it
//! mirrors.

use super::auth::account_of;
use super::books::{self, holding_row, order_row, portfolio_totals, position_row, trade_row};
use super::client::{has_error, Answer};
use super::data::{bar_open, depth_from, first_till, next_till, quote_from, raw_bars};
use super::funds::funds_from;
use super::mapping::*;
use super::master_contract::{parse_expiry, parse_master, underlying};
use super::orders::{cancel_body, modify_body, place_body, placement_outcome};
use super::session::{initial_app_id_key, NuvamaSession};
use super::streaming::{
    depth_book, order_update, quote_tick, request, response, trade_time_ms, NuvamaFeed,
    NuvamaOrderFeed,
};
use super::tcp_relay::{split_lines, Batcher, BATCH_MAX_WAIT, BATCH_QUIET, SEND_GAP};
use super::*;
use crate::brokers::common::mapping::{Action, PriceType, Validity};
use crate::brokers::common::outcome::{classify, placing, PlaceOutcome};
use crate::brokers::common::python::py_float;
use crate::brokers::common::streaming::{
    FeedEvent, FeedMode, FeedSubscription, Message, QuoteField,
};
use chrono::{NaiveDate, NaiveDateTime};
use serde_json::{json, Value};

const CSV: &str = include_str!("../../../tests/fixtures/brokers/nuvama/master.csv");
const RESPONSES: &str = include_str!("../../../tests/fixtures/brokers/nuvama/responses.json");

fn resp(key: &str) -> Value {
    let all: Value = serde_json::from_str(RESPONSES).unwrap();
    all[key].clone()
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(parse_master(CSV).unwrap());
    r
}

fn row(symbol: &str, exchange: &str) -> SymbolData {
    master().by_symbol(exchange, symbol).unwrap()
}

fn session() -> NuvamaSession {
    NuvamaSession::parse("AUTH:::SOURCE:::APPID:::ACC01:::EQ:::KEY01").unwrap()
}

/// The SDK's app id key, put together from parts, is the web's constant
/// byte for byte (SHA-256 of `INITIAL_APP_ID_KEY` in `api/baseurl.py`).
#[test]
fn the_initial_app_id_key_is_the_sdk_constant() {
    use sha2::Digest;
    let d = sha2::Sha256::digest(initial_app_id_key().as_bytes());
    assert_eq!(
        hex::encode(d),
        "ab0b517e2bfb344783df03661997e2bf7a2bb5a2cfa06358ea40f9ac9a8ded80"
    );
}

/// web `NuvamaSession.pack` / `parse_token`: six values; a token without
/// its API key (stored before the key was packed in) is refused.
#[test]
fn the_session_packs_and_refuses_an_incomplete_token() {
    let s = session();
    assert_eq!(s.pack(), "AUTH:::SOURCE:::APPID:::ACC01:::EQ:::KEY01");
    assert_eq!(s.account_id, "ACC01");
    assert_eq!(s.stream_account_type(), "EQ");
    assert!(!format!("{:?}", s).contains("AUTH"));
    for bad in [
        "",
        "AUTH:::SOURCE:::APPID:::ACC01:::EQ",
        "AUTH:::SOURCE:::APPID:::ACC01:::EQ:::",
        ":::SOURCE:::APPID:::ACC01:::EQ:::KEY",
    ] {
        assert!(matches!(NuvamaSession::parse(bad), Err(AppError::Auth(_))));
    }
    // No account type: the stream says EQ.
    let s = NuvamaSession::parse("A:::S:::I:::ACC:::::::K").unwrap();
    assert_eq!(s.stream_account_type(), "EQ");
}

/// web `normalize_status`: in-flight states ending in "pending" are open
/// (a cancel that is pending has cancelled nothing yet).
#[test]
fn statuses_follow_the_web() {
    for (sts, want) in [
        ("Complete", "complete"),
        ("fully executed", "complete"),
        ("Traded", "complete"),
        ("open", "open"),
        ("put order req received", "open"),
        ("validation pending", "open"),
        ("open pending", "open"),
        ("cancel pending", "open"),
        ("Cancelled", "cancelled"),
        ("Rejected", "rejected"),
        ("Trigger Pending", "trigger pending"),
        ("Partially Executed", "open"),
        ("", ""),
    ] {
        assert_eq!(normalize_status(sts), want, "{sts}");
    }
}

#[test]
fn constants_map_both_ways() {
    assert_eq!(order_type(PriceType::Sl), "STOP_LIMIT");
    assert_eq!(order_type(PriceType::SlM), "STOP_MARKET");
    assert_eq!(price_type_from_nuvama("STOP_LIMIT"), "SL");
    assert_eq!(price_type_from_nuvama("sl-m"), "SL-M");
    assert_eq!(price_type_from_nuvama("LMT"), "LIMIT");
    assert_eq!(price_type_from_nuvama("ODD"), "ODD");
    assert_eq!(product_from_nuvama("I"), "MIS");
    assert_eq!(product_from_nuvama("C"), "CNC");
    assert_eq!(product_from_nuvama("M"), "NRML");
    assert_eq!(product_from_nuvama("MTF"), "MTF");
    assert_eq!(side("B"), "BUY");
    assert_eq!(side("sell"), "SELL");
    assert_eq!(nuvama_exchange("NSE_INDEX"), "NSE");
    assert_eq!(nuvama_exchange("NFO"), "NFO");
    assert_eq!(chart_exchange("BSE_INDEX"), "INDEX");
    for (ex, it, idx, want) in [
        ("NSE_INDEX", "EQ", false, "INDEX"),
        ("NSE", "EQ", false, "EQUITY"),
        ("NFO", "CE", true, "OPTIDX"),
        ("NFO", "PE", false, "OPTSTK"),
        ("NFO", "FUT", true, "FUTIDX"),
        ("BFO", "FUT", false, "FUTSTK"),
        ("CDS", "CE", false, "OPTCUR"),
        ("CDS", "FUT", false, "FUTCUR"),
        ("MCX", "PE", false, "OPTFUT"),
        ("NCDEX", "FUT", false, "FUTCOM"),
    ] {
        assert_eq!(chart_asset_type(ex, it, idx), want, "{ex} {it}");
    }
    assert_eq!(number(Some(&json!("1,503.45"))), Some(1503.45));
    assert_eq!(number(Some(&json!("--"))), None);
    assert_eq!(int(Some(&json!("75.9"))), 75);
}

/// web `process_nuvama_csv`: equity by `symbolname` with the ISIN as
/// brsymbol, indices renamed, derivatives in OpenAlgo format, duplicate
/// tokens and undated derivatives dropped.
#[test]
fn master_rows_follow_the_web() {
    let rows = parse_master(CSV).unwrap();
    let find = |s: &str, e: &str| {
        rows.iter()
            .find(|r| r.symbol == s && r.exchange == e)
            .unwrap_or_else(|| panic!("{s} {e}"))
            .clone()
    };
    assert_eq!(rows.iter().filter(|r| r.token == "2885_NSE").count(), 1);
    let rel = find("RELIANCE", "NSE");
    assert_eq!(
        (
            rel.brsymbol.as_str(),
            rel.token.as_str(),
            rel.instrument_type.as_str()
        ),
        ("INE002A01018", "2885_NSE", "EQ")
    );
    assert_eq!(rel.name, "RELIANCE INDUSTRIES LTD");
    assert_eq!(find("M&M", "NSE").token, "2031_NSE");
    let nifty = find("NIFTY", "NSE_INDEX");
    assert_eq!(
        (nifty.token.as_str(), nifty.brexchange.as_str()),
        ("-29", "NSE")
    );
    assert_eq!(nifty.tick_size, 0.05);
    assert_eq!(find("BANKNIFTY", "NSE_INDEX").token, "-21");
    assert_eq!(find("INDIAVIX", "NSE_INDEX").token, "-101");
    assert_eq!(find("SENSEX", "BSE_INDEX").tick_size, 0.01);
    assert_eq!(find("SENSEX50", "BSE_INDEX").token, "-2");
    let ce = find("NIFTY25NOV2626000CE", "NFO");
    assert_eq!(
        (
            ce.expiry.as_str(),
            ce.strike,
            ce.lot_size,
            ce.instrument_type.as_str()
        ),
        ("25-NOV-26", 26000.0, 75, "CE")
    );
    assert_eq!(ce.name, "NIFTY");
    assert_eq!(find("NIFTY25NOV2625950.5PE", "NFO").strike, 25950.5);
    let fut = find("NIFTY25NOV26FUT", "NFO");
    assert_eq!((fut.strike, fut.instrument_type.as_str()), (0.0, "FUT"));
    assert_eq!(find("CRUDEOIL18DEC26FUT", "MCX").lot_size, 100);
    let ncdex = find("TMCFGRNZM18DEC266000CE", "NCDEX");
    assert_eq!(ncdex.name, "TMCFGRNZM");
    assert_eq!(find("USDINR26NOV26FUT", "CDS").tick_size, 0.0025);
    assert!(!rows.iter().any(|r| r.brsymbol == "BADEXPIRYFUT"));
    assert_eq!(underlying("TMCFGRNZM18DEC2026"), "TMCFGRNZM");
    assert_eq!(underlying("NIFTY"), "NIFTY");
    assert_eq!(
        parse_expiry("27/nov/26"),
        NaiveDate::from_ymd_opt(2026, 11, 27)
    );
    assert_eq!(oa_index_symbol("NIFTY MID SELECT", "NSE"), "MIDCPNIFTY");
    assert_eq!(oa_index_symbol("Nifty IT", "NSE"), "NIFTYIT");
    assert_eq!(oa_index_symbol("BSE IT", "BSE"), "BSEINFORMATIONTECHNOLOGY");
}

fn answer(status: u16, body: Value) -> Answer {
    Answer { status, body }
}

/// Every book row carries OpenAlgo symbols and web statuses (web
/// `map_order_data`, `transform_order_data`).
#[test]
fn order_book_rows_are_openalgo_rows() {
    let m = master();
    let a = answer(200, resp("order_book"));
    let rows = books::rows(&a, books::NuvamaBook::Orders).unwrap();
    let o: Vec<_> = rows.iter().map(|r| order_row(r, &m)).collect();
    assert_eq!(o.len(), 4);
    assert_eq!(
        (
            o[0].symbol.as_str(),
            o[0].exchange.as_str(),
            o[0].side.as_str()
        ),
        ("RELIANCE", "NSE", "BUY")
    );
    assert_eq!(
        (o[0].quantity, o[0].filled_quantity, o[0].pending_quantity),
        (10, 10, 0)
    );
    assert_eq!(
        (o[0].status.as_str(), o[0].price, o[0].average_price),
        ("complete", 1503.45, 1503.4)
    );
    assert_eq!(o[0].order_timestamp, "17-Nov-2026 10:15:30");
    assert_eq!(o[1].symbol, "NIFTY25NOV2626000CE");
    assert_eq!(
        (
            o[1].order_type.as_str(),
            o[1].status.as_str(),
            o[1].trigger_price,
            o[1].pending_quantity
        ),
        ("SL", "trigger pending", 121.5, 75)
    );
    assert_eq!(o[2].symbol, "M&M");
    assert_eq!(o[2].status, "rejected");
    assert_eq!(
        o[2].rejection_reason.as_deref(),
        Some("RMS: margin exceeds")
    );
    assert_eq!(
        (o[3].product.as_str(), o[3].status.as_str()),
        ("MIS", "open")
    );
}

/// An empty book is Nuvama's 222 with ETRD0004; a success without its list
/// and that code on a server error are not empty (BR-03; the web reads the
/// code on any status).
#[test]
fn an_empty_book_is_only_nuvamas_empty_answer() {
    let empty = resp("positions_empty");
    assert!(
        books::rows(&answer(222, empty.clone()), books::NuvamaBook::Positions)
            .unwrap()
            .is_empty()
    );
    assert!(books::rows(&answer(503, empty), books::NuvamaBook::Positions).is_err());
    assert!(books::rows(
        &answer(200, json!({"data": {}})),
        books::NuvamaBook::Positions
    )
    .is_err());
    assert!(books::rows(
        &answer(200, json!({"data": {"pos": null}})),
        books::NuvamaBook::Positions
    )
    .is_err());
    let refused = books::rows(
        &answer(222, resp("place_refused")),
        books::NuvamaBook::Orders,
    );
    assert!(refused
        .unwrap_err()
        .client_message()
        .contains("worse than the trigger"));
}

#[test]
fn trades_positions_and_holdings_are_openalgo_rows() {
    let m = master();
    let t = trade_row(&resp("trade_book")["data"]["trade"][0], &m);
    assert_eq!(
        (
            t.symbol.as_str(),
            t.side.as_str(),
            t.quantity,
            t.average_price,
            t.trade_value
        ),
        ("RELIANCE", "BUY", 10, 1503.4, 15034.0)
    );
    assert_eq!(
        (t.trade_id.as_str(), t.timestamp.as_str()),
        ("T5001", "17-Nov-2026 10:15:31")
    );

    let pos = resp("positions")["data"]["pos"].as_array().unwrap().clone();
    let p0 = position_row(&pos[0], &m);
    assert_eq!(
        (
            p0.symbol.as_str(),
            p0.product.as_str(),
            p0.quantity,
            p0.average_price,
            p0.ltp,
            p0.pnl
        ),
        ("RELIANCE", "MIS", 10, 1503.4, 1510.25, 68.5)
    );
    let p1 = position_row(&pos[1], &m);
    // No ntPL: realised plus unrealised; a short's average is its sell side.
    assert_eq!(
        (p1.quantity, p1.average_price, p1.pnl),
        (-75, 121.25, 253.75)
    );

    let h = resp("holdings")["data"]["rmsHdg"]
        .as_array()
        .unwrap()
        .clone();
    let held: Vec<_> = h.iter().filter_map(|r| holding_row(r, &m)).collect();
    assert_eq!(held.len(), 1);
    assert_eq!(
        (
            held[0].symbol.as_str(),
            held[0].quantity,
            held[0].product.as_str(),
            held[0].average_price
        ),
        ("RELIANCE", 12, "CNC", 0.0)
    );
    assert_eq!(portfolio_totals(&h).totalholdingvalue, 18120.0);
    // web `map_portfolio_data` drops a zero quantity before the totals.
    let mut with_empty = h.clone();
    with_empty.push(json!({"sym": "2031_NSE", "exc": "NSE", "totalQty": "0", "totalVal": "500"}));
    assert_eq!(portfolio_totals(&with_empty).totalholdingvalue, 18120.0);
    // A row the master does not know keeps the broker's trading symbol.
    let other = json!({"sym": "999_NSE", "trdSym": "INE999X01011", "exc": "NSE"});
    assert_eq!(books::oa_symbol_for(&other, &m), "INE999X01011");
}

/// web `get_margin_data`.
#[test]
fn funds_follow_the_web() {
    let f = funds_from(&resp("funds")["data"]);
    assert_eq!(f.available_cash, 100000.46);
    assert_eq!(f.collateral, 3000.0);
    assert_eq!(f.m2m_unrealized, 68.5);
    assert_eq!(f.m2m_realized, -120.25);
    assert_eq!(f.utilised_debits, 15000.5);
}

/// web `_quote_from` and `get_depth`: the best level stands in for an empty
/// quote field; the book is what Nuvama sent, never padded.
#[test]
fn quote_and_depth_follow_the_web() {
    let d = &resp("quote")["data"];
    let key = QuoteKey::new("NSE", "RELIANCE");
    let q = quote_from(&key, &d["qt"], &d["mkd"]);
    assert_eq!(
        (q.ltp, q.open, q.high, q.low, q.close),
        (1503.45, 1495.0, 1510.0, 1490.1, 1498.75)
    );
    // `askPr` "" falls back to the best ask; `akSz` "0" is text, which
    // Python takes as set, so the web reports its 0.
    assert_eq!(
        (q.volume, q.bid, q.ask, q.bid_qty, q.ask_qty),
        (1234567, 1503.4, 1503.5, 120, 0)
    );
    let mut qt = d["qt"].clone();
    qt["akSz"] = json!(0);
    qt.as_object_mut().unwrap().remove("bdSz");
    let q = quote_from(&key, &qt, &d["mkd"]);
    assert_eq!((q.bid_qty, q.ask_qty), (120, 60));
    let m = depth_from(&key, &d["qt"], &d["mkd"]);
    assert_eq!((m.bids.len(), m.asks.len()), (2, 3));
    assert_eq!((m.total_buy_qty, m.total_sell_qty, m.ltq), (200, 9000, 15));
}

/// web `_series`: bars stamped with their close, moved to their open on the
/// clock grid; daily bars at the date's 00:00 UTC.
#[test]
fn chart_bars_are_stamped_with_their_open() {
    let bars = raw_bars(&resp("chart_intraday")["data"]["pltPnts"]);
    assert_eq!(bars.len(), 3, "the unreadable time is left out");
    assert_eq!(bars[2].open, 102.5);
    let t = |s: &str| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap();
    // 09:16 closes the 09:15 1m bar: 2026-11-16 09:15 IST.
    assert_eq!(bar_open(t("2026-11-16 09:16:00"), Some(1)), 1_794_800_700);
    // 10:00 closes the 09:00 1h bar; the forming bar's last tick floors too.
    assert_eq!(bar_open(t("2026-11-16 10:00:00"), Some(60)), 1_794_799_800);
    assert_eq!(bar_open(t("2026-11-16 09:37:12"), Some(15)), 1_794_801_600);
    assert_eq!(bar_open(t("2026-11-16 00:00:00"), None), 1_794_787_200);
    let d = |y, m, dd| NaiveDate::from_ymd_opt(y, m, dd).unwrap();
    let today = d(2026, 11, 17);
    assert_eq!(first_till(false, today, today), None);
    assert_eq!(
        first_till(false, d(2026, 11, 10), today),
        Some(d(2026, 11, 11))
    );
    assert_eq!(first_till(true, d(2026, 3, 1), today), None);
    assert_eq!(first_till(true, d(2024, 3, 1), today), Some(d(2025, 1, 1)));
    assert_eq!(next_till(false, d(2026, 6, 1), None), Some(d(2026, 6, 2)));
    assert_eq!(next_till(false, d(2026, 6, 1), Some(d(2026, 6, 2))), None);
    assert_eq!(
        next_till(true, d(2021, 2, 3), Some(d(2027, 1, 1))),
        Some(d(2021, 1, 1))
    );
}

fn order(symbol: &str, exchange: Exchange, pricetype: PriceType) -> ResolvedOrder {
    ResolvedOrder {
        symbol: symbol.into(),
        exchange,
        action: Action::Buy,
        quantity: 10,
        price: 1500.0,
        trigger_price: 0.0,
        pricetype,
        product: Product::Cnc,
        validity: Validity::Day,
        disclosed_quantity: 0,
        amo: false,
        instrument: row(symbol, exchange.as_str()),
    }
}

/// web `transform_data`: every number as text, the ISIN as `trdSym`, the
/// token as `sym`.
#[test]
fn the_place_body_is_the_webs() {
    let o = order("RELIANCE", Exchange::Nse, PriceType::Limit);
    let b = place_body(&o, "LIMIT", py_float(o.price));
    assert_eq!(
        b,
        json!({
            "trdSym": "INE002A01018", "exc": "NSE", "action": "BUY", "dur": "DAY",
            "ordTyp": "LIMIT", "qty": "10", "dscQty": "0", "sym": "2885_NSE",
            "mktPro": "", "lmPrc": "1500.0", "trgPrc": "0.0", "prdCode": "CNC",
            "posSqr": "N", "minQty": "0", "ordSrc": "API", "vnCode": "", "rmk": "",
            "flQty": "0"
        })
    );
    assert_eq!(py_float(1503.45), "1503.45");
    assert_eq!(py_float(0.0), "0.0");
}

/// web `auth_api.py`: `eqAccID or coAccID`, so a commodity-only account,
/// whose `eqAccID` Nuvama sends as `""`, signs in with its `coAccID`.
#[test]
fn the_login_account_follows_the_web() {
    let login = |accs: Value| json!({"lgnData": {"accTyp": "CO", "accs": accs}});
    let acc = |accs: Value| account_of(&login(accs));
    assert_eq!(
        acc(json!({"eqAccID": "", "coAccID": "C1"})),
        Some(("C1".into(), "CO".into()))
    );
    assert_eq!(
        acc(json!({"eqAccID": " ", "coAccID": "C1"})).map(|a| a.0),
        Some("C1".into())
    );
    assert_eq!(
        acc(json!({"eqAccID": "E1", "coAccID": "C1"})).map(|a| a.0),
        Some("E1".into())
    );
    assert_eq!(acc(json!({"eqAccID": "", "coAccID": ""})), None);
    assert_eq!(acc(json!({"coAccID": null})), None);
}

/// web `not data.get("error")`: an empty or zero `error` is no error.
#[test]
fn an_error_key_counts_only_when_it_holds_something() {
    for none in [
        json!({}),
        json!({"error": null}),
        json!({"error": ""}),
        json!({"error": {}}),
        json!({"error": []}),
        json!({"error": 0}),
        json!({"error": false}),
    ] {
        assert!(!has_error(&none), "{none}");
    }
    for some in [
        json!({"error": {"errMsg": "x"}}),
        json!({"error": "x"}),
        json!({"error": 1}),
        json!({"error": true}),
    ] {
        assert!(has_error(&some), "{some}");
    }
    assert!(answer(200, json!({"error": 0, "data": {}})).ok());
}

/// web `place_order_api` and `_order_id_from`: an id is an accepted order
/// (`oid`, else the first slice's when `oid` is empty); a refusal needs an
/// error that holds something; anything else on a 2xx is unknown.
#[tokio::test]
async fn a_place_answer_is_judged_like_the_web() {
    let judge = |status: u16, body: Value| async move {
        classify(&placing(async { placement_outcome(&answer(status, body)) }).await)
    };
    let accepted = |id: &str| PlaceOutcome::Accepted {
        order_id: id.into(),
    };
    assert_eq!(
        judge(200, json!({"error": 0, "data": {"oid": "N1"}})).await,
        accepted("N1")
    );
    for oid in [json!(null), json!("")] {
        assert_eq!(
            judge(200, json!({"data": {"oid": oid, "ord": [{"oid": "X"}]}})).await,
            accepted("X")
        );
    }
    assert_eq!(
        judge(200, resp("place_sliced")).await,
        accepted("NXAAE00002002")
    );
    assert!(matches!(
        judge(222, resp("place_refused")).await,
        PlaceOutcome::Refused { .. }
    ));
    for body in [
        json!({"error": {}}),
        json!({"error": ""}),
        json!({"data": {}}),
    ] {
        for status in [200, 222] {
            let o = judge(status, body.clone()).await;
            assert!(o.is_uncertain(), "{status} {body}: {o:?}");
        }
    }
}

/// web `transform_modify_order_data` and `transform_cancel_order_data`:
/// the instrument, side and quantity from the order's book row.
#[test]
fn modify_and_cancel_bodies_come_from_the_book_row() {
    let book = resp("order_book");
    let r = &book["data"]["ord"][1];
    let m = ResolvedModify {
        order_id: "NXAAE00001002".into(),
        symbol: "NIFTY25NOV2626000CE".into(),
        exchange: Exchange::Nfo,
        action: Action::Buy,
        product: Product::Nrml,
        pricetype: PriceType::Sl,
        quantity: 150,
        price: 119.0,
        trigger_price: 120.5,
        disclosed_quantity: 0,
        instrument: row("NIFTY25NOV2626000CE", "NFO"),
    };
    let b = modify_body(&m, r, "STOP_LIMIT", py_float(119.0));
    assert_eq!(b["action"], "SELL", "the side is the order's");
    assert_eq!(b["nstOID"], "NXAAE00001002");
    assert_eq!(b["curQty"], "75");
    assert_eq!(b["qty"], "150");
    assert_eq!(b["trgPrc"], "120.5");
    assert_eq!(b["sym"], "42692_NFO");
    assert_eq!(b["dtDays"], "");
    let c = cancel_body(r);
    assert_eq!(
        c,
        json!({
            "trdSym": "NIFTY25NOV2626000CE", "sym": "42692_NFO", "action": "SELL",
            "nstOID": "NXAAE00001002", "exc": "NFO", "prdCode": "NRML",
            "ordTyp": "STOP_LIMIT", "curQty": "75", "flQty": "0"
        })
    );
}

fn sub(token: &str, symbol: &str, exchange: &str, mode: FeedMode) -> FeedSubscription {
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

fn frame_json(m: &Message) -> Value {
    match m {
        Message::Text(t) => serde_json::from_str(t).unwrap(),
        other => panic!("not text: {other:?}"),
    }
}

/// web `NuvamaStreamClient`: each streaming type's whole set goes in one
/// request; an emptied type is unsubscribed; Depth adds the `quote2` set.
#[test]
fn the_feed_sends_whole_sets() {
    let mut f = NuvamaFeed::with_relay_url("ws://127.0.0.1:1/x", &session());
    assert!(f.ws_request().is_ok());
    f.on_connected();
    let rel = sub("2885_NSE", "RELIANCE", "NSE", FeedMode::Quote);
    let nifty = sub("-29", "NIFTY", "NSE_INDEX", FeedMode::Ltp);
    let frames = f.subscribe_frames(std::slice::from_ref(&rel));
    assert_eq!(frames.len(), 1);
    assert_eq!(
        frame_json(&frames[0]),
        json!({"request": {"streaming_type": "quote", "data": {"accType": "EQ",
            "symbols": [{"symbol": "2885_NSE"}]}, "formFactor": "P", "appID": "APPID",
            "response_format": "json", "request_type": "subscribe"}, "echo": {}})
    );
    let frames = f.subscribe_frames(std::slice::from_ref(&nifty));
    let v = frame_json(&frames[0]);
    assert_eq!(
        v["request"]["data"]["symbols"],
        json!([{"symbol": "-29"}, {"symbol": "2885_NSE"}])
    );
    // Quote to Depth: only the depth set changes.
    let deep = rel.with_mode(FeedMode::Depth);
    let frames = f.mode_change_frames(&rel, &deep);
    assert_eq!(frames.len(), 1);
    let v = frame_json(&frames[0]);
    assert_eq!(v["request"]["streaming_type"], "quote2");
    assert_eq!(
        v["request"]["data"]["symbols"],
        json!([{"symbol": "2885_NSE"}])
    );
    // Releasing RELIANCE empties the depth set and shrinks the quote set.
    let frames = f.unsubscribe_frames(std::slice::from_ref(&deep));
    let kinds: Vec<(String, String)> = frames
        .iter()
        .map(|m| {
            let v = frame_json(m);
            (
                v["request"]["streaming_type"].as_str().unwrap().into(),
                v["request"]["request_type"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ("quote".into(), "subscribe".into()),
            ("quote2".into(), "unsubscribe".into())
        ]
    );
    assert_eq!(
        frame_json(&frames[1])["request"]["data"]["symbols"],
        json!([])
    );
    // A reconnect starts from nothing.
    f.on_connected();
    assert_eq!(f.subscribe_frames(std::slice::from_ref(&nifty)).len(), 1);
}

fn quote_packet() -> Value {
    json!({"sym": "2885_NSE", "ltp": "1503.45", "c": "1498.75", "o": "1495.00", "h": "1510.00",
        "l": "1490.10", "vol": "1234567", "ltq": "15", "avgPrc": "1501.20", "tBQ": "50000",
        "tSQ": "62000", "hCL": "1648.60", "lCL": "1348.90", "ltt": "17/11/2026 10:15:30"})
}

/// web `_normalize`: what the packet carries, at the subscription's mode;
/// a field it leaves out is left out (the web sends 0).
#[test]
fn a_quote_packet_becomes_a_tick() {
    let s = sub("2885_NSE", "RELIANCE", "NSE", FeedMode::Depth);
    let t = quote_tick(&s, &quote_packet());
    assert_eq!(
        (t.ltp, t.close, t.open, t.volume, t.last_quantity),
        (1503.45, 1498.75, 1495.0, 1234567, 15)
    );
    assert_eq!(
        (t.total_buy_quantity, t.total_sell_quantity, t.average_price),
        (50000, 62000, 1501.2)
    );
    assert_eq!((t.change, t.change_percent), (4.7, 0.31));
    assert_eq!(t.last_trade_time_ms, 1_794_890_730_000);
    assert_eq!(t.extras.upper_circuit, Some(1648.6));
    assert!(!t.knows(QuoteField::OpenInterest), "no oI in the packet");
    let mut p = quote_packet();
    p["oI"] = json!("1200");
    p.as_object_mut().unwrap().remove("ltt");
    let t = quote_tick(&s.with_mode(FeedMode::Quote), &p);
    assert_eq!(t.oi, 1200);
    assert!(t.knows(QuoteField::OpenInterest));
    assert!(!t.knows(QuoteField::LastTradeTime));
    assert_eq!(
        trade_time_ms("21 Jul 2023, 12:44:55 PM"),
        Some(1_689_923_695_000)
    );
    assert_eq!(trade_time_ms("bad"), None);
}

/// web `_depth`: five levels with order counts, as sent (never padded); no
/// totals or price of its own.
#[test]
fn a_depth_packet_becomes_a_book() {
    let s = sub("2885_NSE", "RELIANCE", "NSE", FeedMode::Depth);
    let d = json!({"sym": "2885_NSE", "bidValues": [{"price": "1503.40", "qty": "120", "no": "3"}],
        "askValues": [{"price": "1503.50", "qty": "60", "no": "2"}, {"price": "1503.55", "qty": "40", "no": "1"}]});
    let b = depth_book(&s, &d);
    assert_eq!((b.buy.len(), b.sell.len()), (1, 2));
    assert_eq!(
        (b.buy[0].price, b.buy[0].quantity, b.buy[0].orders),
        (1503.4, 120, 3)
    );
    assert!(b.absent.contains(QuoteField::TotalBuyQuantity));
    let mut f = NuvamaFeed::with_relay_url("ws://127.0.0.1:1/x", &session());
    f.on_connected();
    f.subscribe_frames(std::slice::from_ref(&s));
    let line = json!({"response": {"streaming_type": "quote2", "data": d}}).to_string();
    assert!(matches!(
        f.parse(&Message::Text(line)).as_slice(),
        [FeedEvent::Depth(_)]
    ));
    let line = json!({"response": {"streaming_type": "quote", "data": quote_packet()}}).to_string();
    assert!(matches!(
        f.parse(&Message::Text(line)).as_slice(),
        [FeedEvent::Tick(_)]
    ));
    let unknown = json!({"response": {"streaming_type": "quote", "data": {"sym": "9_NSE"}}});
    assert!(f.parse(&Message::Text(unknown.to_string())).is_empty());
    assert_eq!(
        f.parse(&Message::Text(crate::brokers::common::relay::READY.into())),
        [FeedEvent::AuthOk]
    );
}

/// web `normalize_order_update`: order packets only; pending is what is
/// left unless the order has ended.
#[test]
fn order_packets_become_order_updates() {
    let m = master();
    let p = json!({"pTyp": "ORDER_UPDATE", "oID": "NXAAE00001001", "sym": "2885_NSE",
        "tSym": "INE002A01018", "exc": "nse", "tTyp": "B", "oTyp": "LIMIT", "pCode": "I",
        "prc": "1503.45", "trgPrc": "0", "tQty": "10", "fQty": "4", "avgPrc": "1503.40",
        "sts": "open", "rRsn": ""});
    let u = order_update(&p, &m).unwrap();
    assert_eq!(
        (
            u.orderid.as_str(),
            u.symbol.as_str(),
            u.exchange.as_str(),
            u.action.as_str()
        ),
        ("NXAAE00001001", "RELIANCE", "NSE", "BUY")
    );
    assert_eq!(
        (u.quantity, u.filled_quantity, u.pending_quantity),
        (10, 4, 6)
    );
    assert_eq!(
        (
            u.product.as_str(),
            u.pricetype.as_str(),
            u.order_status.as_str()
        ),
        ("MIS", "LIMIT", "open")
    );
    let mut r = p.clone();
    r["sts"] = json!("rejected");
    r["rRsn"] = json!(" RMS ");
    let u = order_update(&r, &m).unwrap();
    assert_eq!(
        (u.pending_quantity, u.rejection_reason.as_str()),
        (0, "RMS")
    );
    let mut t = p.clone();
    t["pTyp"] = json!("TRADE_UPDATE");
    assert!(order_update(&t, &m).is_none());

    let mut f = NuvamaOrderFeed::with_relay_url("ws://127.0.0.1:1/x", &session(), m);
    let sub = f.on_authenticated();
    assert_eq!(
        frame_json(&sub[0]),
        json!({"request": {"streaming_type": "orderFiler", "data": {"accType": "EQ",
            "userID": "ACC01", "accID": "ACC01", "responseType": ["ORDER_UPDATE", "TRADE_UPDATE"]},
            "formFactor": "P", "appID": "APPID", "response_format": "json",
            "request_type": "subscribe"}, "echo": {}})
    );
    let line = json!({"response": {"streaming_type": "orderFiler", "data": p}}).to_string();
    assert!(matches!(
        f.parse(&Message::Text(line)).as_slice(),
        [FeedEvent::OrderUpdate(_)]
    ));
}

#[test]
fn requests_and_responses_are_the_sdks() {
    let m = request("quote", json!({"symbols": []}), "APP", "unsubscribe");
    assert_eq!(frame_json(&m)["request"]["request_type"], "unsubscribe");
    assert_eq!(
        response(r#"{"response":{"streaming_type":"quote","data":{"sym":"1"}}}"#),
        Some(("quote".into(), json!({"sym": "1"})))
    );
    assert_eq!(response("{}"), None);
}

/// web `_flush_one`: the latest request per type once quiet for 0.5 s (or
/// 2 s after the first), one type at a time, 0.5 s apart; an unsubscribe
/// of a type never subscribed is not sent.
#[test]
fn the_relay_batches_like_the_web() {
    let t0 = tokio::time::Instant::now();
    let mut b = Batcher::default();
    assert_eq!(b.due(), None);
    b.put("quote", "q1".into(), true, t0);
    b.put("quote", "q2".into(), true, t0 + BATCH_QUIET / 2);
    b.put("quote2", "d1".into(), true, t0 + BATCH_QUIET / 2);
    assert_eq!(b.take(t0 + BATCH_QUIET / 2), None);
    let due = b.due().unwrap();
    assert_eq!(due, t0 + BATCH_QUIET / 2 + BATCH_QUIET);
    assert_eq!(b.take(due).as_deref(), Some("q2"));
    assert_eq!(b.take(due), None, "the next type waits its gap");
    assert_eq!(b.due(), Some(due + SEND_GAP));
    assert_eq!(b.take(due + SEND_GAP).as_deref(), Some("d1"));
    // A long burst still goes 2 s after its first change.
    let t1 = due + SEND_GAP * 4;
    let mut at = t1;
    while at < t1 + BATCH_MAX_WAIT * 2 {
        b.put("quote", "q3".into(), true, at);
        at += BATCH_QUIET / 5;
    }
    assert_eq!(b.due(), Some(t1 + BATCH_MAX_WAIT));
    // An unsubscribe of a type this socket never subscribed is dropped.
    let mut fresh = Batcher::default();
    fresh.put("quote2", "u".into(), false, t0);
    assert_eq!(fresh.take(t0 + BATCH_MAX_WAIT), None);
    assert_eq!(fresh.due(), None);
}

#[test]
fn lines_split_and_keepalive_answers_are_signs_of_life() {
    let mut buf = b"{\"a\":1}\n{}\n\n{\"b\"".to_vec();
    let (lines, alive) = split_lines(&mut buf);
    assert_eq!(lines, ["{\"a\":1}"]);
    assert!(alive);
    assert_eq!(buf, b"{\"b\"");
}

#[test]
fn the_broker_is_registered_as_the_web_describes_it() {
    let b = NuvamaBroker::new(master());
    assert_eq!((b.id(), b.name()), ("nuvama", "Nuvama"));
    assert_eq!(
        b.login_kind(),
        LoginKind::Redirect {
            param: "request_id"
        }
    );
    let caps = b.capabilities();
    assert!(caps.streaming && caps.order_feed && caps.history);
    assert!(!caps.margin && !caps.gtt && !caps.multiquotes_batch);
    let ex: Vec<&str> = b.supported_exchanges().iter().map(|e| e.as_str()).collect();
    assert_eq!(
        ex,
        [
            "NSE",
            "BSE",
            "NFO",
            "BFO",
            "CDS",
            "MCX",
            "NCDEX",
            "NSE_INDEX",
            "BSE_INDEX"
        ]
    );
    let req = |i: &str| HistoryRequest {
        key: QuoteKey::new("NSE", "RELIANCE"),
        interval: i.into(),
        start: NaiveDate::from_ymd_opt(2026, 11, 16).unwrap(),
        end: NaiveDate::from_ymd_opt(2026, 11, 16).unwrap(),
    };
    assert!(b.history_on_session_grid(&req("1h")));
    assert!(!b.history_on_session_grid(&req("D")));
    let auth = AuthToken::new("A:::S:::I:::ACC:::EQ:::K");
    assert!(b.create_feed(&auth).is_ok());
    assert!(b.create_order_feed(&auth).is_ok());
    assert!(b.create_feed(&AuthToken::new("partial")).is_err());
}
