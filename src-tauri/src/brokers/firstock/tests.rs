//! Firstock mapping, master and feed tests against recorded payloads
//! (`src-tauri/tests/fixtures/brokers/firstock/`).

use super::data::{chunk_days, parse_candle, to_depth, to_quote};
use super::mapping::*;
use super::master_contract::{carried_index_rows, index_symbol, parse_file, parse_index_list};
use super::streaming::FirstockFeed;
use super::*;
use crate::brokers::common::mapping::{Action, PriceType};
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription, Message};
use serde_json::{json, Value};

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../../tests/fixtures/brokers/firstock/", $name))
    };
}

fn resp(name: &str) -> Value {
    let all: Value = serde_json::from_str(fixture!("responses.json")).unwrap();
    all[name].clone()
}

fn master() -> SymbolResolver {
    let mut rows = parse_file("NSE", fixture!("NSE.csv"));
    rows.extend(parse_file("BSE", fixture!("BSE.csv")));
    rows.extend(parse_file("NFO", fixture!("NFO.csv")));
    rows.extend(parse_index_list(&resp("index_list")));
    let r = SymbolResolver::new();
    r.load(rows);
    r
}

fn list(name: &str) -> Vec<Value> {
    resp(name)["data"].as_array().cloned().unwrap()
}

#[test]
fn master_csvs_and_index_list() {
    let r = master();
    let sbin = r.by_symbol("NSE", "SBIN").unwrap();
    assert_eq!((sbin.brsymbol.as_str(), sbin.tick_size), ("SBIN-EQ", 0.05));
    assert_eq!(r.by_symbol("NSE", "YESBANK").unwrap().instrument_type, "BE");
    let nifty = r.by_symbol("NSE_INDEX", "NIFTY").unwrap();
    assert_eq!(
        (nifty.brexchange.as_str(), nifty.token.as_str()),
        ("NSE", "26000")
    );
    assert!(r.by_symbol("NSE_INDEX", "FINNIFTY").is_some());
    assert!(r.by_symbol("NSE_INDEX", "BANKNIFTY").is_some());
    assert_eq!(
        r.by_symbol("BSE_INDEX", "SENSEX").unwrap().brexchange,
        "BSE"
    );
    assert!(r
        .by_symbol("BSE_INDEX", "BSEINFORMATIONTECHNOLOGY")
        .is_some());
    let fut = r.by_symbol("NFO", "NIFTY27OCT26FUT").unwrap();
    assert_eq!((fut.expiry.as_str(), fut.lot_size), ("27-OCT-26", 75));
    assert!(r.by_symbol("NFO", "NIFTY27OCT2625000CE").is_some());
    assert_eq!(
        index_symbol("NIFTY MIDCAP 150", "", "NSE"),
        "NIFTYMIDCAP150"
    );
    assert_eq!(index_symbol("Some New Index", "", "NSE"), "SOMENEWINDEX");
}

/// Web #2198 `get_existing_index_rows`: only INDEX rows are carried, and
/// one the fresh rows already hold under the same exchange and token is
/// not repeated.
#[test]
fn carried_index_rows_skip_what_the_fresh_files_hold() {
    let mut current = parse_file("NSE", fixture!("NSE.csv"));
    current.extend(parse_index_list(&resp("index_list")));
    let fresh = parse_file("BSE", fixture!("BSE.csv"));
    let kept = carried_index_rows(&current, &fresh);
    assert!(kept.iter().all(|r| r.instrument_type == "INDEX"));
    assert!(!kept
        .iter()
        .any(|r| r.exchange == "NSE" || r.symbol == "SBIN"));
    // SENSEX (BSE_INDEX, token 1) is in the fresh BSE file.
    assert!(!kept
        .iter()
        .any(|r| r.exchange == "BSE_INDEX" && r.token == "1"));
    let tokens: Vec<&str> = kept.iter().map(|r| r.token.as_str()).collect();
    assert!(tokens.contains(&"26009"));
    assert!(tokens.contains(&"26037"));
    // NIFTY is stored twice (file and index list): carried once, first wins.
    assert_eq!(tokens.iter().filter(|t| **t == "26000").count(), 1);
    assert_eq!(tokens.len(), 3);
    assert!(carried_index_rows(&[], &fresh).is_empty());
}

#[test]
fn credentials_and_session() {
    assert_eq!(user_from_vendor("AB1234_API"), "AB1234");
    assert_eq!(user_from_vendor("AB1234"), "AB1234");
    let b = login_body("AB1234", "pw", "123456", "AB1234_API", "key");
    assert_eq!(b["password"], sha256_hex(&["pw"]));
    assert_eq!(b["TOTP"], "123456");
    assert_eq!(b["vendorCode"], "AB1234_API");
    let s = session(&AuthToken::new("AB1234:::jk")).unwrap();
    assert_eq!((s.uid.as_str(), s.jkey.as_str()), ("AB1234", "jk"));
    assert!(session(&AuthToken::new("jk")).is_err());
    assert_eq!(
        error_message(&resp("cancel_failed")),
        "Order not found to cancel"
    );
    assert!(matches!(
        firstock_error(&json!({"status":"failed","error":{"message":"Invalid jKey"}})),
        AppError::Auth(_)
    ));
}

#[test]
fn books_are_openalgo_shaped() {
    let r = master();
    let orders: Vec<Order> = list("order_book")
        .iter()
        .map(|o| map_order(o, &r))
        .collect();
    assert_eq!(
        (
            orders[0].symbol.as_str(),
            orders[0].product.as_str(),
            orders[0].order_type.as_str(),
            orders[0].status.as_str()
        ),
        ("SBIN", "CNC", "LIMIT", "open")
    );
    assert_eq!(orders[1].symbol, "NIFTY27OCT26FUT");
    assert_eq!(
        (orders[1].status.as_str(), orders[1].order_type.as_str()),
        ("trigger_pending", "SL-M")
    );
    assert_eq!(
        orders[2].rejection_reason.as_deref(),
        Some("RMS:Margin Exceeds")
    );
    assert_eq!(
        (orders[2].order_type.as_str(), orders[2].product.as_str()),
        ("LIMIT", "MIS")
    );
    assert_eq!(orders[3].status, "complete");
    let trades: Vec<Trade> = list("trade_book")
        .iter()
        .map(|t| map_trade(t, &r))
        .collect();
    assert_eq!(
        (trades[0].symbol.as_str(), trades[0].side.as_str()),
        ("SBIN", "SELL")
    );
    assert_eq!(trades[0].trade_value, 4059.75);
    assert_eq!(trades[0].trade_id, "77001");
    let pos: Vec<Position> = list("positions")
        .iter()
        .map(|p| map_position(p, &r))
        .collect();
    assert_eq!((pos[0].quantity, pos[0].product.as_str()), (-75, "NRML"));
    assert_eq!((pos[0].pnl, pos[0].unrealized_pnl), (150.0, 1500.0));
    let h = map_holdings(&list("holdings"), &r);
    assert_eq!(h.len(), 1);
    assert_eq!(
        (h[0].symbol.as_str(), h[0].exchange.as_str(), h[0].quantity),
        ("SBIN", "NSE", 0)
    );
    let f = funds_from(&resp("limit")["data"]);
    assert_eq!(
        (f.available_cash, f.collateral, f.utilised_debits),
        (85000.0, 2500.0, 20000.0)
    );
    assert_eq!((f.m2m_realized, f.m2m_unrealized), (0.0, 0.0));
    assert_eq!(map_status("CANCELED"), "cancelled");
    assert_eq!(map_status("PENDING"), "open");
}

fn order(pt: &str) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        side: "BUY".into(),
        quantity: 10,
        price: 0.0,
        order_type: pt.into(),
        product: "MIS".into(),
        validity: "DAY".into(),
        trigger_price: None,
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, &master()).unwrap()
}

#[test]
fn order_payloads_and_mpp() {
    let b = place_body(&order("MARKET"), Some(812.4));
    assert_eq!(
        b,
        json!({"exchange":"NSE","tradingSymbol":"SBIN-EQ","quantity":"10","price":"816.45",
               "triggerPrice":"0","product":"I","transactionType":"B","priceType":"LMT",
               "retention":"DAY","mkt_protection":"0","remarks":"Place Order"})
    );
    // No quote: sent as MKT, server-side protection applies.
    assert_eq!(place_body(&order("MARKET"), None)["priceType"], "MKT");
    assert_eq!(
        mpp(
            "SBIN",
            Some(Action::Sell),
            PriceType::SlM,
            0.0,
            Some(1000.0),
            0.05,
            false
        ),
        ("SL-LMT", "995".into(), "0")
    );
    // Modify without a quote defers to the server with mkt_protection 1.
    assert_eq!(
        mpp(
            "SBIN",
            Some(Action::Buy),
            PriceType::Market,
            0.0,
            None,
            0.05,
            true
        ),
        ("MKT", "0".into(), "1")
    );
    assert_eq!(
        mpp(
            "SBIN",
            Some(Action::Buy),
            PriceType::Limit,
            811.0,
            None,
            0.05,
            true
        ),
        ("LMT", "811".into(), "0")
    );
    let basket = basket_body(vec![
        json!({"tradingSymbol":"A"}),
        json!({"tradingSymbol":"B"}),
    ])
    .unwrap();
    assert_eq!(basket["BasketList_Params"], json!([{"tradingSymbol":"B"}]));
}

#[test]
fn quotes_depth_and_candles() {
    let k = QuoteKey::new("NSE", "SBIN");
    let q = to_quote(&k, &resp("quote")["data"]);
    assert_eq!(
        (q.ltp, q.close, q.bid, q.ask, q.bid_qty),
        (812.4, 808.0, 812.35, 812.45, 100)
    );
    let d = to_depth(&k, &resp("quote")["data"]);
    assert_eq!(
        (d.bids[1].price, d.asks[1].quantity, d.asks[4].price),
        (812.3, 210, 0.0)
    );
    assert_eq!((d.total_buy_qty, d.total_sell_qty), (345678, 456789));
    let m = list("history_minute");
    assert_eq!(parse_candle(&m[0], false).unwrap().timestamp, 1790826360);
    assert_eq!(parse_candle(&m[1], false).unwrap().timestamp, 1790826300);
    // Daily bars land on 09:15 of their date.
    let day = parse_candle(&list("history_day")[0], true).unwrap();
    assert_eq!(day.timestamp, 1790812800 + 33300);
    assert_eq!(
        (chunk_days("1m"), chunk_days("4h"), chunk_days("D")),
        (1, 15, 30)
    );
}

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

#[test]
fn feed_url_frames_and_ticks() {
    use crate::brokers::common::streaming::BrokerFeed;
    let mut f = FirstockFeed::new(WS_URL, "AB1234", "jk/1");
    let req = f.ws_request().unwrap();
    assert_eq!(
        req.uri().to_string(),
        "wss://socket.firstock.in/V2/ws?userId=AB1234&jKey=jk%2F1&source=developer-api"
    );
    assert!(f.on_connected().is_empty());
    assert!(!f.awaits_auth_ack());
    let frames = f.subscribe_frames(&[
        sub("SBIN", "NSE", "3045", FeedMode::Quote),
        sub("NIFTY27OCT26FUT", "NFO", "54321", FeedMode::Depth),
    ]);
    match &frames[0] {
        Message::Text(t) => assert_eq!(
            serde_json::from_str::<Value>(t).unwrap(),
            json!({"action":"subscribe","tokens":"NSE:3045|NFO:54321"})
        ),
        _ => panic!(),
    }
    let ev = f.parse_text(&resp("feed_v1").to_string());
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!(
        (t.symbol.as_str(), t.ltp, t.close, t.volume),
        ("SBIN", 812.4, 808.0, 1234567)
    );
    assert_eq!(t.change, 4.4);
    let ev = f.parse_text(&resp("feed_v2").to_string());
    assert_eq!(ev.len(), 2);
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!((t.ltp, t.open, t.oi, t.volume), (25080.0, 0.0, 9876525, 0));
    let FeedEvent::Depth(d) = &ev[1] else {
        panic!()
    };
    assert_eq!(
        (d.buy[0].price, d.sell[0].quantity, d.sell[1].price),
        (25079.9, 150, 0.0)
    );
    assert_eq!(d.buy.len(), 5);
    assert!(matches!(
        f.parse_text(&resp("feed_failed").to_string())[0],
        FeedEvent::AuthFailed(_)
    ));
    assert_eq!(f.cached(), 2);
    let u = f.unsubscribe_frames(&[sub("SBIN", "NSE", "3045", FeedMode::Quote)]);
    assert_eq!(u.len(), 1);
    assert_eq!(f.cached(), 1);
    assert!(f.parse_text(&resp("feed_v1").to_string()).is_empty());
    assert!(matches!(f.heartbeat().unwrap().1, Message::Ping(_)));
}

/// A run of subscriptions is one frame without repeated instruments; mode
/// changes need no wire frame (web test_firstock_batch_subscriptions.py).
#[test]
fn subscribe_run_is_one_deduplicated_frame() {
    let mut f = FirstockFeed::new("wss://x", "u", "k");
    let frames = f.subscribe_frames(&[
        sub("SBIN", "NSE", "3045", FeedMode::Ltp),
        sub("TCS", "NSE", "11536", FeedMode::Quote),
        sub("SBIN", "NSE", "3045", FeedMode::Ltp),
        sub("NIFTY27OCT26FUT", "NFO", "54321", FeedMode::Depth),
    ]);
    assert_eq!(frames.len(), 1);
    match &frames[0] {
        Message::Text(t) => assert_eq!(
            serde_json::from_str::<Value>(t).unwrap(),
            json!({"action":"subscribe","tokens":"NSE:3045|NSE:11536|NFO:54321"})
        ),
        _ => panic!(),
    }
    assert!(f
        .mode_change_frames(
            &sub("SBIN", "NSE", "3045", FeedMode::Ltp),
            &sub("SBIN", "NSE", "3045", FeedMode::Depth)
        )
        .is_empty());
    assert!(f.subscribe_frames(&[]).is_empty());
}

/// Unsubscribing one instrument of a batch retires only that instrument;
/// the others keep streaming (web test_firstock_batch_subscriptions.py
/// test_unsubscribe_retires_batched_tracking_only_after_last_token). A new
/// connection starts without the old snapshots
/// (test_connection_close_clears_batch_tracking_before_reconnect).
#[test]
fn unsubscribe_retires_only_its_instrument_from_a_batch() {
    use crate::brokers::common::streaming::BrokerFeed;
    let mut f = FirstockFeed::new(WS_URL, "AB1234", "jk");
    let frames = f.subscribe_frames(&[
        sub("SBIN", "NSE", "3045", FeedMode::Quote),
        sub("NIFTY27OCT26FUT", "NFO", "54321", FeedMode::Depth),
    ]);
    assert_eq!(frames.len(), 1);
    assert!(!f.parse_text(&resp("feed_v1").to_string()).is_empty());
    assert!(!f.parse_text(&resp("feed_v2").to_string()).is_empty());
    let u = f.unsubscribe_frames(&[sub("NIFTY27OCT26FUT", "NFO", "54321", FeedMode::Depth)]);
    match &u[..] {
        [Message::Text(t)] => assert_eq!(
            serde_json::from_str::<Value>(t).unwrap(),
            json!({"action":"unsubscribe","tokens":"NFO:54321"})
        ),
        other => panic!("{other:?}"),
    }
    // SBIN, from the same batch, still streams; the future no longer does.
    assert!(!f.parse_text(&resp("feed_v1").to_string()).is_empty());
    assert!(f.parse_text(&resp("feed_v2").to_string()).is_empty());
    assert_eq!(f.cached(), 1);
    assert!(f.on_connected().is_empty());
    assert_eq!(f.cached(), 0);
}
