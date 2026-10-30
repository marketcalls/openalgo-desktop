//! The Flow editor's lookups over HTTP (parity R-01, R-59..R-66; web
//! `blueprints/flow.py` 1416-1770): index lot sizes from the master
//! contract, a basket's exact lot sizes, and the option-strike ladder from
//! the desktop's expiry and chain services, structure only.

use super::*;
use crate::brokers::common::symbols::SymToken;
use crate::brokers::mock::MockCall;
use crate::brokers::types::Quote;

fn row(
    symbol: &str,
    name: &str,
    exchange: &str,
    kind: &str,
    expiry: &str,
    strike: f64,
    lot: i32,
) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: symbol.into(),
        name: name.into(),
        exchange: exchange.into(),
        brexchange: exchange.into(),
        token: format!("{}:{}", exchange, symbol),
        expiry: expiry.into(),
        strike,
        lot_size: lot,
        instrument_type: kind.into(),
        tick_size: 0.05,
    }
}

/// NIFTY named in the master; BANKNIFTY only by its symbol (its `name` a
/// description); NIFTYNXT50 at another lot size; SENSEX on BFO; GOLDM on
/// MCX; FINNIFTY absent; NIFTY options on two expiries after the clock's
/// 5 Oct 2026.
fn master() -> Vec<SymToken> {
    let mut rows = vec![
        row("NIFTY", "NIFTY", "NSE_INDEX", "INDEX", "", 0.0, 1),
        row(
            "NIFTYNXT5027OCT2668000CE",
            "NIFTYNXT50",
            "NFO",
            "CE",
            "27-OCT-26",
            68000.0,
            25,
        ),
        row(
            "BANKNIFTY27OCT2655000CE",
            "BANK NIFTY INDEX OPTION",
            "NFO",
            "CE",
            "27-OCT-26",
            55000.0,
            30,
        ),
        row(
            "SENSEX29OCT2680000CE",
            "SENSEX",
            "BFO",
            "CE",
            "29-OCT-26",
            80000.0,
            20,
        ),
        row(
            "GOLDM05NOV26FUT",
            "GOLDM",
            "MCX",
            "FUT",
            "05-NOV-26",
            0.0,
            10,
        ),
        row("SBIN", "SBIN", "NSE", "EQ", "", 0.0, 1),
    ];
    for (code, master) in [("13OCT26", "13-OCT-26"), ("27OCT26", "27-OCT-26")] {
        for strike in (24000..=25000).step_by(100) {
            for side in ["CE", "PE"] {
                rows.push(row(
                    &format!("NIFTY{}{}{}", code, strike, side),
                    "NIFTY",
                    "NFO",
                    side,
                    master,
                    f64::from(strike),
                    65,
                ));
            }
        }
    }
    rows
}

struct L {
    h: H,
    cookie: String,
    csrf: String,
}

impl L {
    fn new() -> Self {
        let h = H::new();
        h.setup();
        h.ctx().symbols.load(master());
        let (cookie, csrf) = h.session(true);
        L { h, cookie, csrf }
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        self.h
            .json(with_session(get(path), &self.cookie, None))
            .await
    }

    async fn post(&self, path: &str, body: &str) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        self.h
            .json(with_session(req, &self.cookie, Some(&self.csrf)))
            .await
    }
}

/// R-01: the three lookups, each for the signed-in user only.
#[tokio::test]
async fn the_lookups_are_the_webs_and_need_the_user() {
    let table = crate::server::routes::flow::lookups::table();
    let got: Vec<(String, &str)> = table
        .iter()
        .map(|r| (r.method.to_string(), r.path))
        .collect();
    assert_eq!(
        got,
        vec![
            ("GET".to_string(), "/flow/api/index-symbols"),
            ("POST".to_string(), "/flow/api/symbol-lotsizes"),
            ("GET".to_string(), "/flow/api/option-strikes"),
        ]
    );
    assert!(table.iter().all(|r| r.access == Access::User));
    let h = H::new();
    h.setup();
    let (s, _) = h.json(get("/flow/api/index-symbols")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

/// R-59, R-60: the name first, else a symbol whose underlying really is the
/// index; an underlying without a lot size is left out.
#[tokio::test]
async fn index_lot_sizes_come_from_the_master_contract() {
    let l = L::new();
    let (s, v) = l.get("/flow/api/index-symbols").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        v,
        json!({"status": "success", "data": [
            {"value": "NIFTY", "label": "NIFTY", "exchange": "NFO", "lotSize": 65},
            {"value": "BANKNIFTY", "label": "BANKNIFTY", "exchange": "NFO", "lotSize": 30},
            {"value": "NIFTYNXT50", "label": "NIFTYNXT50", "exchange": "NFO", "lotSize": 25},
            {"value": "SENSEX", "label": "SENSEX", "exchange": "BFO", "lotSize": 20},
            {"value": "GOLDM", "label": "GOLDM", "exchange": "MCX", "lotSize": 10}
        ]})
    );
}

/// R-61, R-62: the refusals, then every pair echoed, null where unknown.
#[tokio::test]
async fn basket_lot_sizes_echo_every_pair() {
    let l = L::new();
    let path = "/flow/api/symbol-lotsizes";
    let refusals = [
        ("not json", "symbols must be a non-empty array"),
        ("{\"symbols\": []}", "symbols must be a non-empty array"),
        ("{\"symbols\": [1]}", "each symbol must be an object"),
        (
            "{\"symbols\": [{\"symbol\": \"SBIN\"}]}",
            "symbol and exchange are required",
        ),
    ];
    for (body, message) in refusals {
        let (s, v) = l.post(path, body).await;
        assert_eq!(
            (s, v),
            (
                StatusCode::BAD_REQUEST,
                json!({"status": "error", "message": message})
            ),
            "{}",
            body
        );
    }
    let many: Vec<Value> = (0..51)
        .map(|i| json!({"symbol": format!("S{}", i), "exchange": "NSE"}))
        .collect();
    let (s, v) = l.post(path, &json!({"symbols": many}).to_string()).await;
    assert_eq!(
        (s, v["message"].clone()),
        (
            StatusCode::BAD_REQUEST,
            json!("at most 50 symbols per request")
        )
    );
    let body = json!({"symbols": [
        {"symbol": " nifty13oct2624000ce ", "exchange": "nfo"},
        {"symbol": "SBIN", "exchange": "NSE"},
        {"symbol": "NOPE", "exchange": "NSE"}
    ]});
    let (s, v) = l.post(path, &body.to_string()).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        v,
        json!({"status": "success", "lotSizes": {"NFO:NIFTY13OCT2624000CE": 65, "NSE:SBIN": 1, "NSE:NOPE": null}})
    );
}

/// R-63..R-66, X-049: the real services: listed expiries from the master,
/// the ladder from the chain's structure with one underlying quote and no
/// per-strike quotes.
#[tokio::test]
async fn option_strikes_come_from_the_desktops_services_without_leg_quotes() {
    let l = L::new();
    let (s, v) = l.get("/flow/api/option-strikes").await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::BAD_REQUEST, json!("underlying is required"))
    );
    let (s, _) = l
        .get("/flow/api/option-strikes?underlying=NIFTY&optionType=XX")
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    l.h.connect_broker();
    l.h.mock.set_quote(Quote {
        symbol: "NIFTY".into(),
        exchange: "NSE_INDEX".into(),
        ltp: 24412.0,
        ..Default::default()
    });
    let (s, v) = l
        .get("/flow/api/option-strikes?underlying=nifty&optionType=PE&expiryType=next_week")
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let d = &v["data"];
    assert_eq!(d["exchange"], "NFO");
    assert_eq!(d["expiries"], json!(["13OCT26", "27OCT26"]));
    assert_eq!(d["expiry"], "27OCT26");
    assert_eq!(d["resolved"]["current_week"], "13OCT26");
    assert_eq!(d["resolved"]["current_month"], "27OCT26");
    assert_eq!(d["resolved"]["next_month"], Value::Null);
    assert_eq!(d["atm"], 24400.0);
    assert_eq!(d["underlyingLtp"], 24412.0);
    let atm = d["strikes"]
        .as_array()
        .and_then(|s| s.iter().find(|x| x["label"] == "ATM").cloned())
        .unwrap_or(Value::Null);
    assert_eq!(atm["symbol"], "NIFTY27OCT2624400PE");
    let quoted_legs = l.h.mock.calls().iter().any(|c| match c {
        MockCall::Quote(k) => k.symbol != "NIFTY",
        MockCall::MultiQuotes(_) => true,
        _ => false,
    });
    assert!(!quoted_legs, "no per-strike quotes");
}
