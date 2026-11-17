//! The adapter's REST calls against the fake Nuvama: what goes out (paths,
//! headers, bodies) and what comes back (OpenAlgo rows, outcomes).

use super::{auth, broker, fixture};
use chrono::NaiveDate;
use openalgo_desktop_lib::brokers::common::mapping::{
    Action, Exchange, PriceType, Product, Validity,
};
use openalgo_desktop_lib::brokers::common::outcome::{classify, placing, PlaceOutcome};
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
use openalgo_desktop_lib::error::AppError;
use serde_json::json;

fn order(b: &dyn Broker, symbol: &str, exchange: Exchange, pricetype: PriceType) -> ResolvedOrder {
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
        instrument: b
            .symbols()
            .unwrap()
            .by_symbol(exchange.as_str(), symbol)
            .unwrap(),
    }
}

/// web `authenticate_broker`: loginvendor with the key and secret, then
/// logindata with the request id and no Source header; the session packs
/// the app id key the login answered with and the account.
#[tokio::test]
async fn sign_in_follows_the_sdk() {
    let (fake, b) = broker().await;
    let r = b
        .authenticate(BrokerCredentials {
            api_key: "KEY1".into(),
            api_secret: Some("SECRET1".into()),
            request_token: Some("REQ1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(r.user_id, "<USER_ID>");
    assert_eq!(
        r.auth_token,
        "AUTHVALUE1:::SOURCETOKEN1:::APPKEY3:::<USER_ID>:::EQ:::KEY1"
    );
    let vendor = fake.calls("POST", "/edelmw-login/login/accounts/loginvendor/KEY1/");
    assert_eq!(vendor.len(), 1);
    assert_eq!(vendor[0].body, json!({"pwd": "SECRET1"}));
    assert_eq!(vendor[0].header("Source").as_deref(), Some("KEY1"));
    assert!(vendor[0].header("AppIdKey").is_some());
    let data = fake.calls("POST", "/edelmw-login/login/accounts/logindata/");
    assert_eq!(data[0].body, json!({"reqId": "REQ1"}));
    assert_eq!(
        data[0].header("SourceToken").as_deref(),
        Some("SOURCETOKEN1")
    );
    assert_eq!(data[0].header("AppIdKey").as_deref(), Some("APPKEY2"));
    assert_eq!(data[0].header("Source"), None);

    // An expired link (HTTP 222) is a refused sign-in.
    fake.script(
        "POST",
        "/accounts/logindata/",
        222,
        fixture("login_expired"),
    );
    let e = b
        .authenticate(BrokerCredentials {
            api_key: "KEY1".into(),
            api_secret: Some("SECRET1".into()),
            request_token: Some("REQ2".into()),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(
        e.client_message().contains("expired"),
        "{}",
        e.client_message()
    );
    // No request id, or no secret: nothing is sent.
    let before = fake.seen.lock().len();
    assert!(b
        .authenticate(BrokerCredentials {
            api_key: "KEY1".into(),
            api_secret: Some("SECRET1".into()),
            ..Default::default()
        })
        .await
        .is_err());
    assert!(b
        .authenticate(BrokerCredentials {
            api_key: "KEY1".into(),
            request_token: Some("REQ3".into()),
            ..Default::default()
        })
        .await
        .is_err());
    assert_eq!(fake.seen.lock().len(), before);
}

/// Every authenticated call carries the four session headers.
#[tokio::test]
async fn books_carry_the_session_and_return_openalgo_rows() {
    let (fake, b) = broker().await;
    let orders = b.get_order_book(&auth()).await.unwrap();
    assert_eq!(orders.len(), 4);
    assert_eq!(orders[0].symbol, "RELIANCE");
    let seen = &fake.calls("GET", "/edelmw-eq/eq/order/book/ACC1/v1/")[0];
    for (h, v) in [
        ("Authorization", "AUTHV"),
        ("SourceToken", "SRCTOK"),
        ("AppIdKey", "APPKEY3"),
        ("Source", "KEY1"),
    ] {
        assert_eq!(seen.header(h).as_deref(), Some(v), "{h}");
    }
    assert_eq!(b.get_trade_book(&auth()).await.unwrap()[0].quantity, 10);
    let pos = b.get_positions(&auth()).await.unwrap();
    assert_eq!(
        (pos[1].symbol.as_str(), pos[1].quantity),
        ("NIFTY25NOV2626000CE", -75)
    );
    let h = b.get_holdings_with_totals(&auth()).await.unwrap();
    assert_eq!(h.holdings.len(), 1);
    assert_eq!(h.statistics().totalholdingvalue, 18120.0);
    let f = b.get_funds(&auth()).await.unwrap();
    assert_eq!((f.available_cash, f.collateral), (100000.46, 3000.0));
    assert_eq!(
        b.get_open_position(&auth(), "RELIANCE", Exchange::Nse, Product::Mis)
            .await
            .unwrap(),
        10
    );
    assert_eq!(
        b.get_open_position(&auth(), "RELIANCE", Exchange::Nse, Product::Cnc)
            .await
            .unwrap(),
        0
    );
}

/// web `test_position_read_failure` (nuvama): the empty book is HTTP 222
/// with ETRD0004 and reads flat; "Session Expired" (EGN0011) refuses the
/// smart order and asks for a new login; a 5xx is never a flat position.
#[tokio::test]
async fn position_reads_follow_the_webs_failure_spec() {
    let (fake, b) = broker().await;
    let path = "/positions/net/ACC1/";
    fake.script("GET", path, 222, fixture("positions_empty"));
    assert_eq!(
        b.get_open_position(&auth(), "RELIANCE", Exchange::Nse, Product::Mis)
            .await
            .unwrap(),
        0
    );
    assert!(b.get_positions(&auth()).await.unwrap().is_empty());
    fake.script("GET", path, 222, fixture("session_expired"));
    let e = b
        .get_open_position(&auth(), "RELIANCE", Exchange::Nse, Product::Mis)
        .await
        .unwrap_err();
    assert!(matches!(e, AppError::Auth(_)), "{e:?}");
    fake.script("GET", path, 503, fixture("positions_empty"));
    let e = b
        .get_open_position(&auth(), "RELIANCE", Exchange::Nse, Product::Mis)
        .await
        .unwrap_err();
    assert!(e
        .client_message()
        .contains("could not read your open position"));
    fake.script(
        "GET",
        path,
        200,
        json!({"data": {"pos": [{"sym": "2885_NSE", "exc": "NSE", "prdCode": "MIS"}]}}),
    );
    assert!(b
        .get_open_position(&auth(), "RELIANCE", Exchange::Nse, Product::Mis)
        .await
        .is_err());
}

/// web `apply_market_protection`: MARKET goes as LIMIT off the LTP, SL-M as
/// STOP_LIMIT off its trigger; three outcomes by Nuvama's answer.
#[tokio::test]
async fn placement_prices_and_outcomes() {
    let (fake, b) = broker().await;
    let place = "/edelmw-eq/eq/trade/placetrade/v1/ACC1/";
    let o = order(&b, "RELIANCE", Exchange::Nse, PriceType::Market);
    let r = placing(b.place_order(&auth(), &o)).await.unwrap();
    assert_eq!(r.order_id, "NXAAE00002001");
    let sent = fake.calls("POST", place);
    let body = &sent[0].body;
    assert_eq!(body["ordTyp"], "LIMIT");
    // LTP 1503.45, equity slab 0.5% buy side, on the 0.05 tick.
    assert_eq!(body["lmPrc"], "1510.95");
    assert_eq!(
        (body["trdSym"].as_str(), body["sym"].as_str()),
        (Some("INE002A01018"), Some("2885_NSE"))
    );
    assert_eq!(
        fake.calls("GET", "/edelmw-content/content/quote/scrip/2885_NSE/")
            .len(),
        1
    );

    let mut slm = order(&b, "RELIANCE", Exchange::Nse, PriceType::SlM);
    slm.trigger_price = 1600.0;
    slm.action = Action::Sell;
    placing(b.place_order(&auth(), &slm)).await.unwrap();
    let body = &fake.calls("POST", place)[1].body;
    assert_eq!(
        (body["ordTyp"].as_str(), body["lmPrc"].as_str()),
        (Some("STOP_LIMIT"), Some("1592.0"))
    );
    assert_eq!(body["trgPrc"], "1600.0");

    let limit = order(&b, "RELIANCE", Exchange::Nse, PriceType::Limit);
    fake.script("POST", place, 200, fixture("place_sliced"));
    assert_eq!(
        placing(b.place_order(&auth(), &limit))
            .await
            .unwrap()
            .order_id,
        "NXAAE00002002"
    );
    fake.script("POST", place, 222, fixture("place_refused"));
    let r = placing(b.place_order(&auth(), &limit)).await;
    assert!(
        matches!(classify(&r), PlaceOutcome::Refused { .. }),
        "{r:?}"
    );
    fake.script("POST", place, 502, json!({"error": {"errMsg": "gateway"}}));
    let r = placing(b.place_order(&auth(), &limit)).await;
    assert!(classify(&r).is_uncertain(), "{r:?}");
    fake.script("POST", place, 200, json!({"data": {}}));
    let r = placing(b.place_order(&auth(), &limit)).await;
    assert!(classify(&r).is_uncertain(), "{r:?}");
}

/// Modify and cancel read the order's book row first and send its keys.
#[tokio::test]
async fn modify_and_cancel_read_the_book_first() {
    let (fake, b) = broker().await;
    let m = ResolvedModify {
        order_id: "NXAAE00001002".into(),
        symbol: "NIFTY25NOV2626000CE".into(),
        exchange: Exchange::Nfo,
        action: Action::Buy,
        product: Product::Nrml,
        pricetype: PriceType::Limit,
        quantity: 150,
        price: 119.5,
        trigger_price: 0.0,
        disclosed_quantity: 0,
        instrument: b
            .symbols()
            .unwrap()
            .by_symbol("NFO", "NIFTY25NOV2626000CE")
            .unwrap(),
    };
    assert_eq!(
        b.modify_order(&auth(), &m).await.unwrap().order_id,
        "NXAAE00001002"
    );
    let body = &fake.calls("PUT", "/edelmw-eq/eq/trade/modifytrade/v1/ACC1/")[0].body;
    assert_eq!(
        (
            body["action"].as_str(),
            body["lmPrc"].as_str(),
            body["curQty"].as_str()
        ),
        (Some("SELL"), Some("119.5"), Some("75"))
    );
    b.cancel_order(&auth(), "NXAAE00001004").await.unwrap();
    let body = &fake.calls("PUT", "/edelmw-eq/eq/trade/canceltrade/v1/ACC1/")[0].body;
    assert_eq!(body["nstOID"], "NXAAE00001004");
    let e = b.cancel_order(&auth(), "NOPE").await.unwrap_err();
    assert!(e
        .client_message()
        .contains("not in today's Nuvama order book"));

    // Cancel all: the open and trigger-pending rows (two of four), one read.
    let reads = fake.calls("GET", "/edelmw-eq/eq/order/book/ACC1/v1/").len();
    let r = b.cancel_all_orders(&auth()).await.unwrap();
    assert_eq!(r.cancelled, ["NXAAE00001002", "NXAAE00001004"]);
    assert_eq!(
        fake.calls("GET", "/edelmw-eq/eq/order/book/ACC1/v1/").len(),
        reads + 1
    );
}

#[tokio::test]
async fn quotes_depth_history_and_master() {
    let (fake, b) = broker().await;
    let key = QuoteKey::new("NSE", "RELIANCE");
    assert_eq!(b.get_quote(&auth(), &key).await.unwrap().ltp, 1503.45);
    let d = b.get_market_depth(&auth(), &key).await.unwrap();
    assert_eq!(d.asks.len(), 3);
    let req = HistoryRequest {
        key: key.clone(),
        interval: "15m".into(),
        start: NaiveDate::from_ymd_opt(2026, 11, 16).unwrap(),
        end: NaiveDate::from_ymd_opt(2026, 11, 16).unwrap(),
    };
    let c = b.get_history(&auth(), &req).await.unwrap();
    // 09:30 and 09:45 close the 09:15 and 09:30 bars; 11-17 is outside.
    assert_eq!(
        c.iter().map(|c| c.timestamp).collect::<Vec<_>>(),
        [1_794_800_700, 1_794_801_600]
    );
    let chart = fake
        .seen
        .lock()
        .iter()
        .find(|s| s.path.contains("/charts/"))
        .cloned()
        .unwrap();
    assert_eq!(
        chart.path,
        "/edelmw-content/content/charts/v2/main/M15/NSE/EQUITY/2885_NSE"
    );
    assert_eq!(chart.body["chTyp"], "Interval");
    let e = b
        .get_history(
            &auth(),
            &HistoryRequest {
                interval: "W".into(),
                ..req.clone()
            },
        )
        .await
        .unwrap_err();
    assert!(e.client_message().contains("not available from Nuvama"));
    fake.script(
        "POST",
        "/charts/v2/main/D1/NSE/EQUITY/2885_NSE",
        222,
        fixture("chart_empty"),
    );
    assert!(b
        .get_history(
            &auth(),
            &HistoryRequest {
                interval: "D".into(),
                ..req
            }
        )
        .await
        .unwrap()
        .is_empty());
    let rows = b.download_master_contract(&auth()).await.unwrap();
    assert!(rows
        .iter()
        .any(|r| r.symbol == "NIFTY" && r.exchange == "NSE_INDEX"));
}
