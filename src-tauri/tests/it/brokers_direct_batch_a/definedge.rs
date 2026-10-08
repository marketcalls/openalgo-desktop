//! definedge against a local fake broker: OTP sign-in (auto-send, wrong
//! OTP, success), orders, cancel-all, close-all, books, funds, margin,
//! quotes with the OI backfill, depth, history chunking with a 429 retry,
//! and the zipped master contract.

#![allow(unused_imports)]

use super::support::*;
use openalgo_desktop_lib::brokers::definedge::{DefinedgeBroker, Endpoints};
use openalgo_desktop_lib::brokers::families::noren::zip;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicUsize, Ordering};

const SECRET: &str = "api-secret-1";
const TOKEN: &str = "API-TOKEN-1";
const AUTH: &str = "sess-1:::suser-1:::API-TOKEN-1";

fn endpoints(base: &str) -> Endpoints {
    Endpoints {
        signin: format!("{}/signin", base),
        trade: format!("{}/dart/v1", base),
        data: format!("{}/sds", base),
        master: format!("{}/public/allmaster.zip", base),
        ws: "ws://127.0.0.1:9/NorenWSTRTP/".into(),
    }
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    let mut opt = row(
        "NIFTY28OCT2525000CE",
        "NIFTY28OCT25C25000",
        "NFO",
        "NFO",
        "43001",
        75,
        0.05,
    );
    opt.name = "NIFTY".into();
    opt.expiry = "28-OCT-25".into();
    opt.strike = 25000.0;
    opt.instrument_type = "CE".into();
    r.load(vec![
        row("SBIN", "SBIN-EQ", "NSE", "NSE", "3045", 1, 0.05),
        row("INFY", "INFY-EQ", "NSE", "NSE", "1594", 1, 0.05),
        row("NIFTY", "Nifty 50", "NSE_INDEX", "NSE", "26000", 1, 0.05),
        opt,
    ]);
    r
}

fn creds(otp: Option<&str>) -> BrokerCredentials {
    BrokerCredentials {
        api_key: TOKEN.into(),
        api_secret: Some(SECRET.into()),
        totp: otp.map(str::to_string),
        ..Default::default()
    }
}

fn hex_sha(s: &str) -> String {
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect()
}

#[derive(Default)]
struct State {
    rate_limited: AtomicUsize,
}

fn handler(state: Arc<State>) -> impl Fn(&Req) -> Response + Send + Sync + 'static {
    move |r: &Req| {
        let p = r.path.as_str();
        if p.starts_with("/signin/login/") {
            if r.header("api_secret") != SECRET || !p.ends_with(TOKEN) {
                return with_status(StatusCode::UNAUTHORIZED, json!({"error":"bad"}));
            }
            return ok(json!({"otp_token":"otp-tok-1","message":"OTP sent to registered mobile"}));
        }
        if p == "/signin/token" {
            let b = r.json();
            let otp = b["otp"].as_str().unwrap_or_default().to_string();
            let want = hex_sha(&format!("otp-tok-1{}{}", otp, SECRET));
            if b["ac"] != want.as_str() || b["otp_token"] != "otp-tok-1" {
                return ok(json!({"stat":"Not_Ok","emsg":"Checksum mismatch"}));
            }
            if otp == "000000" {
                return ok(json!({"stat":"Not_Ok","emsg":"Invalid OTP"}));
            }
            return ok(json!({
                "stat":"Ok","api_session_key":"sess-1","susertoken":"suser-1","uid":"<USER_ID>"
            }));
        }
        if p.starts_with("/dart/v1") && r.header("authorization") != "sess-1" {
            return with_status(
                StatusCode::UNAUTHORIZED,
                json!({"message":"Invalid session"}),
            );
        }
        match p {
            "/dart/v1/placeorder" => {
                let b = r.json();
                if b["tradingsymbol"] == "INFY-EQ" {
                    ok(json!({"status":"ERROR","message":"Insufficient margin"}))
                } else {
                    ok(json!({"status":"SUCCESS","order_id":"25100300000999"}))
                }
            }
            "/dart/v1/modify" => ok(json!({"status":"SUCCESS","order_id":r.json()["order_id"]})),
            "/dart/v1/cancel/25100300000103" => {
                ok(json!({"status":"ERROR","message":"Order already executed"}))
            }
            _ if p.starts_with("/dart/v1/cancel/") => ok(json!({
                "status":"SUCCESS","order_id":p.rsplit('/').next().unwrap(),"request_time":"x"
            })),
            "/dart/v1/orders" => text(crate::fixture!("definedge", "order_book.json")),
            "/dart/v1/trades" => text(crate::fixture!("definedge", "trade_book.json")),
            "/dart/v1/positions" => text(crate::fixture!("definedge", "positions.json")),
            "/dart/v1/holdings" => text(crate::fixture!("definedge", "holdings.json")),
            "/dart/v1/limits" => text(crate::fixture!("definedge", "limits.json")),
            "/dart/v1/spancalculator" => {
                ok(json!({"status":"SUCCESS","span":"98000.25","exposure":"12000"}))
            }
            "/dart/v1/quotes/NSE/3045"
            | "/dart/v1/quotes/NFO/43001"
            | "/dart/v1/quotes/NSE/26000" => text(crate::fixture!("definedge", "quote.json")),
            "/dart/v1/quotes/NSE/1594" => ok(json!({"status":"ERROR","message":"No data"})),
            "/dart/v1/securityinfo/NSE/3045" => {
                ok(json!({"status":"SUCCESS","tradingsymbol":"SBIN-EQ","lotsize":"1"}))
            }
            "/public/allmaster.zip" => bytes(zip::build(
                "allmaster.csv",
                crate::fixture!("definedge", "allmaster.csv").as_bytes(),
                true,
            )),
            _ if p.starts_with("/sds/history/NFO/43001/minute/") && r.query.is_empty() => {
                // OI backfill reads the last row's seventh column.
                text("010920251529,1,1,1,1,10,4321\n010920251530,1,1,1,1,10,5555")
            }
            _ if p.starts_with("/sds/history/NSE/3045/minute/") => {
                if state.rate_limited.fetch_add(1, Ordering::SeqCst) == 0 {
                    return with_status(StatusCode::TOO_MANY_REQUESTS, "slow down");
                }
                text(crate::fixture!("definedge", "history_minute.csv"))
            }
            _ if p.starts_with("/sds/history/NSE/3045/day/") => {
                text(crate::fixture!("definedge", "history_day.csv"))
            }
            _ => with_status(StatusCode::NOT_FOUND, json!({"status":"ERROR"})),
        }
    }
}

async fn setup() -> (Fake, DefinedgeBroker, AuthToken) {
    let fake = Fake::start(handler(Arc::new(State::default()))).await;
    let b = DefinedgeBroker::with_endpoints(master(), endpoints(&fake.base)).with_fast_timing();
    let auth = AuthToken::new(AUTH)
        .with_feed(Some("suser-1"))
        .with_user_id("<USER_ID>");
    (fake, b, auth)
}

#[tokio::test]
async fn otp_login_auto_send_wrong_otp_then_success() {
    let (fake, b, _) = setup().await;
    // No OTP requested yet: the first submit sends one and asks again.
    let e = b.authenticate(creds(Some("123456"))).await.unwrap_err();
    assert!(
        e.client_message().contains("send an OTP"),
        "{}",
        e.client_message()
    );
    assert!(b.otp_pending());
    assert_eq!(fake.calls("/login/API-TOKEN-1").len(), 1);

    let e = b.authenticate(creds(Some("000000"))).await.unwrap_err();
    assert!(
        e.client_message().contains("Invalid OTP"),
        "{}",
        e.client_message()
    );
    assert!(b.otp_pending(), "a wrong OTP keeps the token for a retry");

    let r = b.authenticate(creds(Some(" 123456 "))).await.unwrap();
    assert_eq!(r.auth_token, AUTH);
    assert_eq!(r.feed_token.as_deref(), Some("suser-1"));
    assert_eq!(r.user_id, "<USER_ID>");
    assert!(!b.otp_pending());
    let tok = fake.calls("/signin/token");
    assert_eq!(tok.len(), 2);
    assert_eq!(tok[1].json()["otp"], "123456");
}

#[tokio::test]
async fn explicit_send_otp_and_bad_credentials() {
    let (fake, b, _) = setup().await;
    let msg = b.send_otp(&creds(None)).await.unwrap();
    assert_eq!(msg, "OTP sent to registered mobile");
    assert_eq!(
        fake.calls("/login/API-TOKEN-1")[0].header("api_secret"),
        SECRET
    );
    // Pending token + empty OTP: asks for the code without another send.
    let e = b.authenticate(creds(None)).await.unwrap_err();
    assert!(e.client_message().contains("Enter the OTP"));
    assert_eq!(fake.calls("/login/API-TOKEN-1").len(), 1);

    let mut wrong = creds(None);
    wrong.api_secret = Some("nope".into());
    let e = b.send_otp(&wrong).await.unwrap_err();
    assert!(e.client_message().contains("did not send an OTP"));
    let mut missing = creds(None);
    missing.api_secret = None;
    assert!(b.authenticate(missing).await.is_err());
}

#[tokio::test]
async fn orders_place_modify_cancel() {
    let (fake, b, auth) = setup().await;
    let s = master();
    let req = OrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        side: "BUY".into(),
        quantity: 10,
        price: 0.0,
        order_type: "MARKET".into(),
        product: "MIS".into(),
        validity: "DAY".into(),
        trigger_price: None,
        disclosed_quantity: None,
        amo: false,
    };
    let o = ResolvedOrder::resolve(&req, &s).unwrap();
    let r = b.place_order(&auth, &o).await.unwrap();
    assert_eq!(r.order_id, "25100300000999");
    let sent = fake.calls("/placeorder")[0].clone();
    assert_eq!(sent.header("authorization"), "sess-1");
    assert_eq!(
        sent.json(),
        json!({"tradingsymbol":"SBIN-EQ","exchange":"NSE","quantity":10,"price":"0",
               "price_type":"MARKET","product_type":"INTRADAY","order_type":"BUY","algo_id":"99999"})
    );

    let mut bad = req.clone();
    bad.symbol = "INFY".into();
    let e = b
        .place_order(&auth, &ResolvedOrder::resolve(&bad, &s).unwrap())
        .await
        .unwrap_err();
    assert_eq!(e.client_message(), "Definedge: Insufficient margin");

    let m = ModifyOrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        product: "MIS".into(),
        pricetype: "LIMIT".into(),
        quantity: 10,
        price: 811.5,
        trigger_price: 0.0,
        disclosed_quantity: 0,
    };
    let rm = ResolvedModify::resolve("25100300000101", &m, &s).unwrap();
    assert_eq!(
        b.modify_order(&auth, &rm).await.unwrap().order_id,
        "25100300000101"
    );
    let mb = fake.calls("/modify")[0].json();
    assert_eq!(mb["price"], "811.5");
    assert_eq!(mb["validity"], "DAY");

    let c = b.cancel_order(&auth, "25100300000102").await.unwrap();
    assert_eq!(c.order_id, "25100300000102");
    assert_eq!(fake.calls("/cancel/25100300000102")[0].method, Method::GET);
    let e = b.cancel_order(&auth, "25100300000103").await.unwrap_err();
    assert_eq!(e.client_message(), "Definedge: Order already executed");
}

#[tokio::test]
async fn cancel_all_close_all_and_open_position() {
    let (fake, b, auth) = setup().await;
    let r = b.cancel_all_orders(&auth).await.unwrap();
    assert_eq!(r.cancelled, ["25100300000102"]);
    assert_eq!(r.failed, ["25100300000103"]);

    let r = b.close_all_positions(&auth).await.unwrap();
    assert_eq!(r.placed.len(), 2, "{:?}", r);
    let bodies: Vec<Value> = fake.calls("/placeorder").iter().map(|x| x.json()).collect();
    assert_eq!(bodies[0]["order_type"], "SELL");
    assert_eq!(bodies[0]["quantity"], 10);
    assert_eq!(bodies[0]["product_type"], "INTRADAY");
    assert_eq!(bodies[1]["order_type"], "BUY");
    assert_eq!(bodies[1]["quantity"], 75);
    assert_eq!(bodies[1]["tradingsymbol"], "NIFTY28OCT25C25000");
    assert_eq!(bodies[1]["product_type"], "NORMAL");

    assert_eq!(
        b.get_open_position(&auth, "NIFTY28OCT2525000CE", Exchange::Nfo, Product::Nrml)
            .await
            .unwrap(),
        -75
    );
    assert_eq!(
        b.get_open_position(&auth, "SBIN", Exchange::Nse, Product::Cnc)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn books_funds_and_margin() {
    let (_fake, b, auth) = setup().await;
    let o = b.get_order_book(&auth).await.unwrap();
    assert_eq!(o.len(), 5);
    assert_eq!(o[0].symbol, "SBIN");
    assert!(o.iter().all(|x| x.status == x.status.to_lowercase()));
    let t = b.get_trade_book(&auth).await.unwrap();
    assert_eq!(t[1].symbol, "NIFTY28OCT2525000CE");
    let p = b.get_positions(&auth).await.unwrap();
    assert_eq!(p.len(), 3);
    let h = b.get_holdings(&auth).await.unwrap();
    assert_eq!(h[0].symbol, "INFY");
    let f = b.get_funds(&auth).await.unwrap();
    assert_eq!(f.available_cash, 250000.46);
    assert_eq!(f.m2m_realized, 457.25);

    let legs = vec![MarginLeg {
        key: QuoteKey::new("NFO", "NIFTY28OCT2525000CE"),
        action: Action::Sell,
        quantity: 75,
        product: Product::Nrml,
        pricetype: PriceType::Market,
        price: 0.0,
        trigger_price: 0.0,
    }];
    let m = b.calculate_margin(&auth, &legs).await.unwrap();
    assert_eq!(m.total_margin_required, 110000.25);
    assert_eq!(m.exposure_margin, 12000.0);
    let none = vec![MarginLeg {
        key: QuoteKey::new("NFO", "UNKNOWN"),
        ..legs[0].clone()
    }];
    assert!(b.calculate_margin(&auth, &none).await.is_err());

    // Expired session (resume check uses the raw token alone).
    let e = b
        .get_funds(&AuthToken::new("stale:::x:::y"))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("session has expired"));
}

#[tokio::test]
async fn quotes_multiquotes_depth_with_oi_backfill() {
    let (fake, b, auth) = setup().await;
    let q = b
        .get_quote(&auth, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!(q.ltp, 812.35);
    assert_eq!(q.close, 805.0);
    assert_eq!(q.oi, 0);
    // Equities carry no OI: no history call.
    assert!(fake.all().iter().all(|r| !r.path.starts_with("/sds/")));

    // Indices are quoted on the cash exchange.
    let qi = b
        .get_quote(&auth, &QuoteKey::new("NSE_INDEX", "NIFTY"))
        .await
        .unwrap();
    assert_eq!(qi.ltp, 812.35);

    let qo = b
        .get_quote(&auth, &QuoteKey::new("NFO", "NIFTY28OCT2525000CE"))
        .await
        .unwrap();
    assert_eq!(qo.oi, 5555);
    let oi_calls = fake
        .all()
        .into_iter()
        .filter(|r| r.path.starts_with("/sds/history/NFO/43001/minute/"))
        .count();
    assert_eq!(oi_calls, 1);
    // Cached for a minute: a second quote does not refetch OI.
    b.get_quote(&auth, &QuoteKey::new("NFO", "NIFTY28OCT2525000CE"))
        .await
        .unwrap();
    assert_eq!(
        fake.all()
            .into_iter()
            .filter(|r| r.path.starts_with("/sds/history/NFO/43001/minute/"))
            .count(),
        1
    );

    let keys = vec![
        QuoteKey::new("NSE", "SBIN"),
        QuoteKey::new("NSE", "UNKNOWN"),
        QuoteKey::new("NSE", "INFY"),
        QuoteKey::new("NFO", "NIFTY28OCT2525000CE"),
    ];
    let m = b.get_multiquotes(&auth, &keys).await.unwrap();
    assert_eq!(m.len(), 4);
    assert_eq!(m[0].data.as_ref().unwrap().ltp, 812.35);
    assert_eq!(m[1].error.as_deref(), Some("Could not resolve token"));
    assert!(m[2].data.is_none() && m[2].error.is_some());
    assert_eq!(m[3].data.as_ref().unwrap().oi, 5555);
    assert_eq!(m[3].symbol, "NIFTY28OCT2525000CE");

    let d = b
        .get_market_depth(&auth, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!(d.bids.len(), 5);
    assert_eq!(d.total_sell_qty, 1750);

    let info = b
        .security_info(&auth, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!(info["tradingsymbol"], "SBIN-EQ");
}

#[tokio::test]
async fn history_intraday_resampled_with_retry_and_daily() {
    let (fake, b, auth) = setup().await;
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: "15m".into(),
        start: d(2025, 9, 1),
        end: d(2025, 9, 1),
    };
    let c = b.get_history(&auth, &req).await.unwrap();
    // The first call was rate limited and retried.
    let calls: Vec<Req> = fake
        .all()
        .into_iter()
        .filter(|r| r.path.starts_with("/sds/history/NSE/3045/minute/"))
        .collect();
    assert_eq!(calls.len(), 2);
    assert!(calls[0].path.ends_with("/minute/010920250000/010920252359"));
    assert_eq!(calls[0].header("authorization"), "sess-1");
    assert_eq!(c.len(), 3);
    assert_eq!(c[0].timestamp, 1756698300); // 09:15 IST
    assert_eq!(c[0].volume, 4500);
    assert_eq!(c[0].oi, 5200);
    assert_eq!(c[1].timestamp - c[0].timestamp, 900);

    // Daily: midnight timestamps, clipped to the requested window.
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: "D".into(),
        start: d(2025, 9, 2),
        end: d(2025, 9, 2),
    };
    let c = b.get_history(&auth, &req).await.unwrap();
    assert_eq!(c.len(), 1, "the API ignores `to`; the 1st is clipped away");
    assert_eq!(c[0].timestamp, 1756771200);
    assert_eq!(c[0].close, 815.0);

    // Unsupported interval: no candles, no request.
    let before = fake.all().len();
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: "3m".into(),
        start: d(2025, 9, 1),
        end: d(2025, 9, 1),
    };
    assert!(b.get_history(&auth, &req).await.unwrap().is_empty());
    assert_eq!(fake.all().len(), before);
}

#[tokio::test]
async fn master_contract_download_from_zip() {
    let (_fake, b, auth) = setup().await;
    let rows = b.download_master_contract(&auth).await.unwrap();
    assert!(rows.len() > 15);
    let nifty = rows
        .iter()
        .find(|r| r.exchange == "NSE_INDEX" && r.symbol == "NIFTY")
        .unwrap();
    assert_eq!(nifty.token, "26000");
    assert!(rows
        .iter()
        .any(|r| r.symbol == "NIFTY28OCT2525000CE" && r.expiry == "28-OCT-25"));
}

#[tokio::test]
async fn feeds_need_the_session_identity() {
    let (_fake, b, auth) = setup().await;
    let mut f = b.create_feed(&auth).unwrap();
    let c = f.on_connected();
    let Message::Text(t) = &c[0] else { panic!() };
    let v: Value = serde_json::from_str(t).unwrap();
    assert_eq!(v["susertoken"], "suser-1");
    assert_eq!(v["uid"], "<USER_ID>");
    assert!(matches!(
        Broker::create_order_feed(&b, &auth),
        Ok(OrderFeed::Socket(_))
    ));
    assert!(Broker::create_order_feed(&b, &AuthToken::new(AUTH)).is_err());
    assert!(b.create_feed(&AuthToken::new(AUTH)).is_err());
}

/// Sentinel credentials and session through every sign-in and request
/// error path (refusing broker, unreachable broker): the sentinel never
/// reaches a log line, an error's Display/Debug or a trader message.
#[tokio::test]
async fn secrets_stay_out_of_errors_and_logs() {
    let logs = capture_logs();
    let fake = refusing_fake().await;
    let key = QuoteKey::new("NSE", "SBIN");
    for base in [fake.base.clone(), closed_base()] {
        let b = DefinedgeBroker::with_endpoints(master(), endpoints(&base)).with_fast_timing();
        clean_err(b.authenticate(sentinel_creds()).await);
        let auth = AuthToken::new(format!("{s}:::{s}:::{s}", s = SENTINEL))
            .with_feed(Some(SENTINEL))
            .with_user_id(SENTINEL);
        clean_err(b.get_order_book(&auth).await);
        clean_err(b.get_positions(&auth).await);
        clean_err(b.get_funds(&auth).await);
        clean_err(b.cancel_order(&auth, "1").await);
        clean_err(b.get_quote(&auth, &key).await);
        clean_err(b.get_market_depth(&auth, &key).await);
        clean_err(b.download_master_contract(&auth).await);
        // A second sign-in verifies the OTP sent by the first.
        clean_err(b.authenticate(sentinel_creds()).await);
        clean_err(b.send_otp(&sentinel_creds()).await);
    }
    assert!(!logs.text().is_empty(), "the log capture saw nothing");
    logs.assert_clean();
}
