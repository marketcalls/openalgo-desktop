//! BR-03, every adapter: a broker reply that cannot be read, or reads in
//! the wrong shape, is never an empty book, a flat position or a bulk
//! action with nothing to do. Outside a placement it is an error (inside
//! one it is uncertain: `broker_order_outcomes`).
//!
//! Each adapter runs against a local fake that answers every request with
//! one body:
//!
//! * an empty object, `null`, a bare string, a number, and the broker's own
//!   success marker with its list missing or of the wrong type: the
//!   decision reads (the open position a smart order sizes against, close
//!   all, cancel all) and every book (orders, trades, positions, holdings,
//!   funds, the order reconciler's tagged and exact reads) fail;
//! * the broker's empty position book with a server error (503): the
//!   position reads fail;
//! * a position row for SBIN whose quantity is missing, or is not a number:
//!   the open position and close all fail.
//!
//! Controls keep the sentinels honest: the same empty book with 200 reads
//! as flat, and the same row with a quantity of 5 reads as 5. The bodies
//! follow each broker's recorded shapes and the web's
//! `test_position_read_failure` specs. A broker that answers an empty book
//! with `data: null` (Angel, Motilal, mStock) reads that as empty, as the
//! web does, so a null list is no sentinel there.

use crate::broker_support::{fake, symbols, Fake};
use openalgo_desktop_lib::brokers::common::mapping::{Exchange, Product};
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::Broker;

/// Bodies no broker sends for a book, empty or not.
const GENERIC: &[(&str, &str)] = &[
    ("an empty object", "{}"),
    ("null", "null"),
    ("a bare string", r#""ok""#),
    ("a number", "42"),
];

/// One broker's own bodies.
struct Corpus {
    /// The success marker with the list missing or of the wrong type.
    malformed: &'static [(&'static str, &'static str)],
    /// The empty position book.
    empty: &'static str,
    /// One SBIN NSE MIS position of 5 the adapter matches.
    row: &'static str,
    /// That row without its quantity.
    no_quantity: &'static str,
    /// That row with "abc" for its quantity.
    bad_quantity: &'static str,
    /// Reads this broker answers without the book a sentinel stands in
    /// for, each with the reason.
    exempt: &'static [(&'static str, &'static str)],
}

/// What one body did that it must not.
fn misread(what: &str, op: &str, got: String) -> String {
    format!("{}: {} read as {}", what, op, got)
}

async fn open_position(b: &dyn Broker, auth: &AuthToken) -> Result<i64, String> {
    b.get_open_position(auth, "SBIN", Exchange::Nse, Product::Mis)
        .await
        .map_err(|e| e.client_message())
}

/// The decision reads of one body: the open position and close all.
async fn decision_misreads(b: &dyn Broker, auth: &AuthToken, what: &str) -> Vec<String> {
    let mut wrong = Vec::new();
    if let Ok(q) = open_position(b, auth).await {
        wrong.push(misread(what, "the open position", format!("{} (flat)", q)));
    }
    match b.close_all_positions(auth).await {
        Ok(r) if r.failed.is_empty() && r.uncertain.is_empty() => {
            wrong.push(misread(what, "close all", format!("done ({:?})", r.placed)))
        }
        _ => {}
    }
    wrong
}

/// Every read of one body; the misreads are returned.
async fn misreads(b: &dyn Broker, auth: &AuthToken, what: &str) -> Vec<String> {
    let mut wrong = decision_misreads(b, auth, what).await;
    let rows = |n: usize| format!("{} rows", n);
    if let Ok(v) = b.get_order_book(auth).await {
        wrong.push(misread(what, "the order book", rows(v.len())));
    }
    if let Ok(v) = b.get_order_book_tagged(auth).await {
        wrong.push(misread(what, "the tagged order book", rows(v.len())));
    }
    if let Ok(v) = b.get_trade_book(auth).await {
        wrong.push(misread(what, "the trade book", rows(v.len())));
    }
    if let Ok(v) = b.get_trade_book_exact(auth).await {
        wrong.push(misread(what, "the exact trade book", rows(v.len())));
    }
    if let Ok(v) = b.get_positions(auth).await {
        wrong.push(misread(what, "the position book", rows(v.len())));
    }
    if let Ok(v) = b.get_positions_exact(auth).await {
        wrong.push(misread(what, "the exact position book", rows(v.len())));
    }
    if let Ok(v) = b.get_holdings(auth).await {
        wrong.push(misread(what, "the holdings", rows(v.len())));
    }
    if let Ok(v) = b.get_funds(auth).await {
        wrong.push(misread(what, "the funds", format!("{:?}", v)));
    }
    match b.cancel_all_orders(auth).await {
        Ok(r) if r.failed.is_empty() => wrong.push(misread(
            what,
            "cancel all",
            format!("done ({:?})", r.cancelled),
        )),
        _ => {}
    }
    wrong
}

/// Every body of `c` and the generic ones; the failures are reported
/// together.
async fn check(name: &str, b: &dyn Broker, auth: &AuthToken, f: &Fake, c: &Corpus) {
    let mut wrong = Vec::new();
    for (what, body) in GENERIC.iter().chain(c.malformed) {
        f.reply(200, body);
        wrong.extend(misreads(b, auth, what).await);
    }
    wrong.retain(|w| {
        !c.exempt
            .iter()
            .any(|(op, _)| w.contains(&format!(": {} read as", op)))
    });
    f.reply(503, c.empty);
    wrong.extend(decision_misreads(b, auth, "the empty book with a server error").await);
    if let Ok(v) = b.get_positions(auth).await {
        let got = format!("{} rows", v.len());
        wrong.push(misread(
            "the empty book with a server error",
            "the position book",
            got,
        ));
    }
    for (what, body) in [
        ("a row without its quantity", c.no_quantity),
        ("a row whose quantity is not a number", c.bad_quantity),
    ] {
        f.reply(200, body);
        wrong.extend(decision_misreads(b, auth, what).await);
    }
    // Controls: the sentinels above are only wrong for being wrong.
    f.reply(200, c.empty);
    let empty = open_position(b, auth).await;
    if empty != Ok(0) {
        wrong.push(format!("control: the empty book read as {:?}", empty));
    }
    f.reply(200, c.row);
    let five = open_position(b, auth).await;
    if five != Ok(5) {
        wrong.push(format!("control: the row of 5 read as {:?}", five));
    }
    assert!(wrong.is_empty(), "{}: {:#?}", name, wrong);
}

#[tokio::test]
async fn zerodha() {
    use openalgo_desktop_lib::brokers::zerodha::ZerodhaBroker;
    let f = fake().await;
    let b = ZerodhaBroker::with_base_url(symbols(), &f.base);
    let c = Corpus {
        malformed: &[
            ("success without data", r#"{"status":"success"}"#),
            (
                "success with null data",
                r#"{"status":"success","data":null}"#,
            ),
            (
                "success with text data",
                r#"{"status":"success","data":"x"}"#,
            ),
            (
                "positions without net",
                r#"{"status":"success","data":{"day":[]}}"#,
            ),
        ],
        empty: r#"{"status":"success","data":{"net":[],"day":[]}}"#,
        row: r#"{"status":"success","data":{"net":[{"tradingsymbol":"SBIN-EQ","exchange":"NSE","product":"MIS","quantity":5}],"day":[]}}"#,
        no_quantity: r#"{"status":"success","data":{"net":[{"tradingsymbol":"SBIN-EQ","exchange":"NSE","product":"MIS"}],"day":[]}}"#,
        bad_quantity: r#"{"status":"success","data":{"net":[{"tradingsymbol":"SBIN-EQ","exchange":"NSE","product":"MIS","quantity":"abc"}],"day":[]}}"#,
        exempt: &[],
    };
    check("zerodha", &b, &AuthToken::new("kitekey:accesstok"), &f, &c).await;
}

#[tokio::test]
async fn angel() {
    use openalgo_desktop_lib::brokers::angel::AngelBroker;
    let f = fake().await;
    let b = AngelBroker::with_base_url(symbols(), &f.base);
    let c = Corpus {
        malformed: &[(
            "success with text data",
            r#"{"status":true,"message":"SUCCESS","data":"x"}"#,
        )],
        empty: r#"{"status":true,"message":"SUCCESS","errorcode":"","data":null}"#,
        row: r#"{"status":true,"message":"SUCCESS","errorcode":"","data":[{"tradingsymbol":"SBIN-EQ","symboltoken":"3045","exchange":"NSE","producttype":"INTRADAY","netqty":"5"}]}"#,
        no_quantity: r#"{"status":true,"message":"SUCCESS","errorcode":"","data":[{"tradingsymbol":"SBIN-EQ","symboltoken":"3045","exchange":"NSE","producttype":"INTRADAY"}]}"#,
        bad_quantity: r#"{"status":true,"message":"SUCCESS","errorcode":"","data":[{"tradingsymbol":"SBIN-EQ","symboltoken":"3045","exchange":"NSE","producttype":"INTRADAY","netqty":"abc"}]}"#,
        exempt: &[],
    };
    let auth = AuthToken::new("myapikey:jwt.token.sig").with_user_id("<USER_ID>");
    check("angel", &b, &auth, &f, &c).await;
}

#[tokio::test]
async fn fyers() {
    use openalgo_desktop_lib::brokers::fyers::{FyersBroker, FyersUrls};
    let f = fake().await;
    let b = FyersBroker::with_urls(
        symbols(),
        FyersUrls {
            api: f.base.clone(),
            public: format!("{}/sym_details", f.base),
            ..FyersUrls::default()
        },
    )
    .with_retry_base(std::time::Duration::from_millis(1));
    let c = Corpus {
        malformed: &[
            ("ok without the list", r#"{"s":"ok","code":200}"#),
            (
                "ok with text lists",
                r#"{"s":"ok","code":200,"orderBook":"x","netPositions":"x","tradeBook":"x","holdings":"x","fund_limit":"x"}"#,
            ),
        ],
        empty: r#"{"s":"ok","code":200,"message":"","netPositions":[]}"#,
        row: r#"{"s":"ok","code":200,"message":"","netPositions":[{"symbol":"SBIN-EQ","productType":"INTRADAY","netQty":5}]}"#,
        no_quantity: r#"{"s":"ok","code":200,"message":"","netPositions":[{"symbol":"SBIN-EQ","productType":"INTRADAY"}]}"#,
        bad_quantity: r#"{"s":"ok","code":200,"message":"","netPositions":[{"symbol":"SBIN-EQ","productType":"INTRADAY","netQty":"abc"}]}"#,
        exempt: &[],
    };
    check(
        "fyers",
        &b,
        &AuthToken::new("APPID-100:access-token"),
        &f,
        &c,
    )
    .await;
}

#[tokio::test]
async fn upstox() {
    use openalgo_desktop_lib::brokers::upstox::{UpstoxBroker, Urls};
    let f = fake().await;
    let b = UpstoxBroker::with_urls(symbols(), Urls::local(&f.base));
    let c = Corpus {
        malformed: &[
            ("success without data", r#"{"status":"success"}"#),
            (
                "success with null data",
                r#"{"status":"success","data":null}"#,
            ),
            (
                "success with text data",
                r#"{"status":"success","data":"x"}"#,
            ),
        ],
        empty: r#"{"status":"success","data":[]}"#,
        row: r#"{"status":"success","data":[{"exchange":"NSE","instrument_token":"3045","trading_symbol":"SBIN-EQ","product":"I","quantity":5}]}"#,
        no_quantity: r#"{"status":"success","data":[{"exchange":"NSE","instrument_token":"3045","trading_symbol":"SBIN-EQ","product":"I"}]}"#,
        bad_quantity: r#"{"status":"success","data":[{"exchange":"NSE","instrument_token":"3045","trading_symbol":"SBIN-EQ","product":"I","quantity":"abc"}]}"#,
        exempt: &[],
    };
    check("upstox", &b, &AuthToken::new("upstox-token"), &f, &c).await;
}

#[tokio::test]
async fn dhan() {
    use openalgo_desktop_lib::brokers::dhan::{DhanBroker, Variant};
    let f = fake().await;
    let b = DhanBroker::with_urls(
        Variant::Live,
        symbols(),
        f.base.clone(),
        format!("{}/auth", f.base),
        format!("{}/master.csv", f.base),
    )
    .with_retry_base(std::time::Duration::from_millis(1))
    .with_pacing(std::time::Duration::ZERO, std::time::Duration::ZERO);
    // Dhan's books are bare lists; an empty book is `[]`.
    let c = Corpus {
        malformed: &[("a status object", r#"{"status":"success"}"#)],
        empty: "[]",
        row: r#"[{"securityId":"3045","tradingSymbol":"SBIN","exchangeSegment":"NSE_EQ","productType":"INTRADAY","netQty":5}]"#,
        no_quantity: r#"[{"securityId":"3045","tradingSymbol":"SBIN","exchangeSegment":"NSE_EQ","productType":"INTRADAY"}]"#,
        bad_quantity: r#"[{"securityId":"3045","tradingSymbol":"SBIN","exchangeSegment":"NSE_EQ","productType":"INTRADAY","netQty":"abc"}]"#,
        exempt: &[],
    };
    let auth = AuthToken::new("1100012345:::eyJhbGciOiJIUzUxMiJ9.test.sig");
    check("dhan", &b, &auth, &f, &c).await;
}

#[tokio::test]
async fn groww() {
    use openalgo_desktop_lib::brokers::groww::GrowwBroker;
    let f = fake().await;
    let b = GrowwBroker::with_base_url(symbols(), &f.base);
    // The same body answers the CASH and the F&O position reads.
    let c = Corpus {
        malformed: &[
            ("success without payload", r#"{"status":"SUCCESS"}"#),
            (
                "success with text payload",
                r#"{"status":"SUCCESS","payload":"x"}"#,
            ),
            (
                "success without the positions",
                r#"{"status":"SUCCESS","payload":{}}"#,
            ),
        ],
        empty: r#"{"status":"SUCCESS","payload":{"positions":[]}}"#,
        row: r#"{"status":"SUCCESS","payload":{"positions":[{"trading_symbol":"SBIN","segment":"CASH","exchange":"NSE","product":"INTRADAY","quantity":5}]}}"#,
        no_quantity: r#"{"status":"SUCCESS","payload":{"positions":[{"trading_symbol":"SBIN","segment":"CASH","exchange":"NSE","product":"INTRADAY"}]}}"#,
        bad_quantity: r#"{"status":"SUCCESS","payload":{"positions":[{"trading_symbol":"SBIN","segment":"CASH","exchange":"NSE","product":"INTRADAY","quantity":"abc"}]}}"#,
        exempt: &[],
    };
    check("groww", &b, &AuthToken::new("groww-token"), &f, &c).await;
}

#[tokio::test]
async fn kotak() {
    use openalgo_desktop_lib::brokers::kotak::KotakBroker;
    let f = fake().await;
    let b = KotakBroker::with_urls(symbols(), f.base.clone(), format!("{}/5config", f.base))
        .with_retry_base(std::time::Duration::from_millis(2));
    let c = Corpus {
        malformed: &[
            ("ok without data", r#"{"stat":"Ok","stCode":200}"#),
            (
                "ok with text data",
                r#"{"stat":"Ok","stCode":200,"data":"x"}"#,
            ),
        ],
        empty: r#"{"stat":"Ok","stCode":200,"data":[]}"#,
        row: r#"{"stat":"Ok","stCode":200,"data":[{"trdSym":"SBIN-EQ","exSeg":"nse_cm","prod":"MIS","flBuyQty":"5","flSellQty":"0","cfBuyQty":"0","cfSellQty":"0"}]}"#,
        no_quantity: r#"{"stat":"Ok","stCode":200,"data":[{"trdSym":"SBIN-EQ","exSeg":"nse_cm","prod":"MIS"}]}"#,
        bad_quantity: r#"{"stat":"Ok","stCode":200,"data":[{"trdSym":"SBIN-EQ","exSeg":"nse_cm","prod":"MIS","flBuyQty":"abc","flSellQty":"0","cfBuyQty":"0","cfSellQty":"0"}]}"#,
        exempt: &[],
    };
    let auth = AuthToken::new(format!(
        "trade-tok:::trade-sid:::{}/:::acc-tok:::E43",
        f.base
    ));
    check("kotak", &b, &auth, &f, &c).await;
}

#[tokio::test]
async fn xts_family() {
    use openalgo_desktop_lib::brokers::families::xts::XtsBroker;
    use openalgo_desktop_lib::brokers::{
        compositedge, fivepaisaxts, ibulls, iifl, jainamxts, rmoney, wisdom,
    };
    let configs = [
        ("fivepaisaxts", &fivepaisaxts::CONFIG),
        ("jainamxts", &jainamxts::CONFIG),
        ("compositedge", &compositedge::CONFIG),
        ("rmoney", &rmoney::CONFIG),
        ("ibulls", &ibulls::CONFIG),
        ("wisdom", &wisdom::CONFIG),
        ("iifl", &iifl::CONFIG),
    ];
    let c = Corpus {
        malformed: &[
            (
                "success without result",
                r#"{"type":"success","code":"s-orders-0001"}"#,
            ),
            (
                "success with text result",
                r#"{"type":"success","code":"s-orders-0001","result":"x"}"#,
            ),
            (
                "positions without the list",
                r#"{"type":"success","code":"s-portfolio-0005","result":{}}"#,
            ),
        ],
        empty: r#"{"type":"success","code":"s-portfolio-0005","description":"Get Net Position successfully!","result":{"positionList":[]}}"#,
        row: r#"{"type":"success","code":"s-portfolio-0005","description":"Get Net Position successfully!","result":{"positionList":[{"TradingSymbol":"SBIN","ExchangeSegment":"NSECM","ProductType":"MIS","Quantity":"5"}]}}"#,
        no_quantity: r#"{"type":"success","code":"s-portfolio-0005","description":"Get Net Position successfully!","result":{"positionList":[{"TradingSymbol":"SBIN","ExchangeSegment":"NSECM","ProductType":"MIS"}]}}"#,
        bad_quantity: r#"{"type":"success","code":"s-portfolio-0005","description":"Get Net Position successfully!","result":{"positionList":[{"TradingSymbol":"SBIN","ExchangeSegment":"NSECM","ProductType":"MIS","Quantity":"abc"}]}}"#,
        exempt: &[],
    };
    for (name, cfg) in configs {
        let f = fake().await;
        let b = XtsBroker::with_base_url(cfg, symbols(), &f.base);
        let auth = AuthToken::new("eyJhbGciOiJIUzI1NiJ9.interactive.sig").with_user_id("<USER_ID>");
        check(name, &b, &auth, &f, &c).await;
    }
}

#[tokio::test]
async fn iiflcapital() {
    use openalgo_desktop_lib::brokers::iiflcapital::IiflCapitalBroker;
    let f = fake().await;
    let b = IiflCapitalBroker::with_base_url(symbols(), &f.base);
    let c = Corpus {
        malformed: &[
            ("ok without result", r#"{"status":"Ok"}"#),
            ("ok with text result", r#"{"status":"Ok","result":"x"}"#),
            ("a bare list", "[1]"),
        ],
        empty: r#"{"status":"Ok","message":"Success","result":[]}"#,
        row: r#"{"status":"Ok","message":"Success","result":[{"tradingSymbol":"SBIN-EQ","exchange":"NSEEQ","product":"INTRADAY","netQuantity":5}]}"#,
        no_quantity: r#"{"status":"Ok","message":"Success","result":[{"tradingSymbol":"SBIN-EQ","exchange":"NSEEQ","product":"INTRADAY"}]}"#,
        bad_quantity: r#"{"status":"Ok","message":"Success","result":[{"tradingSymbol":"SBIN-EQ","exchange":"NSEEQ","product":"INTRADAY","netQuantity":"abc"}]}"#,
        exempt: &[],
    };
    let auth = AuthToken::new("eyJhbGciOiJIUzI1NiJ9.e30.sig");
    check("iiflcapital", &b, &auth, &f, &c).await;
}

#[tokio::test]
async fn noren_family() {
    use openalgo_desktop_lib::brokers::families::noren::{NorenBroker, NorenEndpoints};
    use openalgo_desktop_lib::brokers::{flattrade, shoonya, tradesmart, zebu};
    let configs = [
        ("shoonya", &shoonya::CONFIG),
        ("flattrade", &flattrade::CONFIG),
        ("tradesmart", &tradesmart::CONFIG),
        ("zebu", &zebu::CONFIG),
    ];
    // A Noren book is a bare list; its empty book is the error envelope
    // saying "no data".
    let c = Corpus {
        malformed: &[("ok without a list", r#"{"stat":"Ok"}"#)],
        empty: r#"{"stat":"Not_Ok","emsg":"no data"}"#,
        row: r#"[{"stat":"Ok","tsym":"SBIN-EQ","exch":"NSE","prd":"I","netqty":"5"}]"#,
        no_quantity: r#"[{"stat":"Ok","tsym":"SBIN-EQ","exch":"NSE","prd":"I"}]"#,
        bad_quantity: r#"[{"stat":"Ok","tsym":"SBIN-EQ","exch":"NSE","prd":"I","netqty":"abc"}]"#,
        exempt: &[],
    };
    for (name, cfg) in configs {
        let f = fake().await;
        let b = NorenBroker::with_endpoints(
            cfg,
            symbols(),
            NorenEndpoints::rebased(cfg, &f.base, "ws://127.0.0.1:1"),
        );
        check(name, &b, &AuthToken::new("AB1234:::sess-tok"), &f, &c).await;
    }
}

#[tokio::test]
async fn firstock() {
    use openalgo_desktop_lib::brokers::firstock::FirstockBroker;
    let f = fake().await;
    let b = FirstockBroker::with_urls(symbols(), &f.base, "ws://127.0.0.1:1");
    let c = Corpus {
        malformed: &[
            ("success without data", r#"{"status":"success"}"#),
            (
                "success with text data",
                r#"{"status":"success","data":"x"}"#,
            ),
        ],
        empty: r#"{"status":"success","data":[]}"#,
        row: r#"{"status":"success","data":[{"tradingSymbol":"SBIN-EQ","exchange":"NSE","product":"I","netQuantity":"5"}]}"#,
        no_quantity: r#"{"status":"success","data":[{"tradingSymbol":"SBIN-EQ","exchange":"NSE","product":"I"}]}"#,
        bad_quantity: r#"{"status":"success","data":[{"tradingSymbol":"SBIN-EQ","exchange":"NSE","product":"I","netQuantity":"abc"}]}"#,
        exempt: &[],
    };
    check("firstock", &b, &AuthToken::new("AB1234:::jkey-1"), &f, &c).await;
}

#[tokio::test]
async fn fivepaisa() {
    use openalgo_desktop_lib::brokers::fivepaisa::{FivepaisaBroker, Session};
    let f = fake().await;
    let b = FivepaisaBroker::with_urls(symbols(), &f.base, format!("{}/master.csv", f.base));
    let auth = AuthToken::new(
        Session {
            api_key: "APPKEY".into(),
            client_code: "50001234".into(),
            access_token: "eyJhbGciOiJIUzI1NiJ9.e30.sig".into(),
        }
        .encode(),
    );
    let c = Corpus {
        malformed: &[
            (
                "success without a body",
                r#"{"head":{"statusDescription":"Success"}}"#,
            ),
            (
                "success with an empty body",
                r#"{"head":{"statusDescription":"Success"},"body":{}}"#,
            ),
            (
                "success with a text body",
                r#"{"head":{"statusDescription":"Success"},"body":"x"}"#,
            ),
        ],
        empty: r#"{"head":{"status":"0","statusDescription":"Success"},"body":{"Message":"Success","Status":0,"NetPositionDetail":[]}}"#,
        row: r#"{"head":{"status":"0","statusDescription":"Success"},"body":{"Message":"Success","Status":0,"NetPositionDetail":[{"ScripCode":3045,"Exch":"N","ExchType":"C","OrderFor":"I","NetQty":5}]}}"#,
        no_quantity: r#"{"head":{"status":"0","statusDescription":"Success"},"body":{"Message":"Success","Status":0,"NetPositionDetail":[{"ScripCode":3045,"Exch":"N","ExchType":"C","OrderFor":"I"}]}}"#,
        bad_quantity: r#"{"head":{"status":"0","statusDescription":"Success"},"body":{"Message":"Success","Status":0,"NetPositionDetail":[{"ScripCode":3045,"Exch":"N","ExchType":"C","OrderFor":"I","NetQty":"abc"}]}}"#,
        exempt: &[],
    };
    check("fivepaisa", &b, &auth, &f, &c).await;
}

#[tokio::test]
async fn nubra() {
    use openalgo_desktop_lib::brokers::nubra::NubraBroker;
    let f = fake().await;
    let b = NubraBroker::with_urls(symbols(), &f.base, "ws://127.0.0.1:1").with_fast_timings();
    let c = Corpus {
        malformed: &[
            ("a portfolio without positions", r#"{"portfolio":{}}"#),
            ("text positions", r#"{"portfolio":{"positions":"x"}}"#),
        ],
        empty: r#"{"portfolio":{"positions":[]}}"#,
        row: r#"{"portfolio":{"positions":[{"symbol":"SBIN-EQ","exchange":"NSE","refId":"3045","deliveryType":"IDAY","netQty":5}]}}"#,
        no_quantity: r#"{"portfolio":{"positions":[{"symbol":"SBIN-EQ","exchange":"NSE","refId":"3045","deliveryType":"IDAY"}]}}"#,
        bad_quantity: r#"{"portfolio":{"positions":[{"symbol":"SBIN-EQ","exchange":"NSE","refId":"3045","deliveryType":"IDAY","netQty":"abc"}]}}"#,
        exempt: &[],
    };
    check("nubra", &b, &AuthToken::new("SESS1"), &f, &c).await;
}

#[tokio::test]
async fn tradejini() {
    use openalgo_desktop_lib::brokers::tradejini::TradejiniBroker;
    let f = fake().await;
    let b = TradejiniBroker::with_urls(symbols(), &f.base, "ws://127.0.0.1:1");
    let c = Corpus {
        malformed: &[
            ("ok without data", r#"{"s":"ok"}"#),
            ("ok with text data", r#"{"s":"ok","d":"x"}"#),
        ],
        empty: r#"{"s":"ok","d":[]}"#,
        row: r#"{"s":"ok","d":[{"sym":{"exchange":"NSE","symbol":"SBIN"},"product":"intraday","netQty":5}]}"#,
        no_quantity: r#"{"s":"ok","d":[{"sym":{"exchange":"NSE","symbol":"SBIN"},"product":"intraday"}]}"#,
        bad_quantity: r#"{"s":"ok","d":[{"sym":{"exchange":"NSE","symbol":"SBIN"},"product":"intraday","netQty":"abc"}]}"#,
        exempt: &[],
    };
    let auth = AuthToken::new("APPKEY0123456789:access-xyz");
    check("tradejini", &b, &auth, &f, &c).await;
}

#[tokio::test]
async fn indmoney() {
    use openalgo_desktop_lib::brokers::indmoney::{Endpoints, IndmoneyBroker};
    let f = fake().await;
    let b = IndmoneyBroker::with_endpoints(
        symbols(),
        Endpoints {
            api: f.base.clone(),
            prices_ws: "ws://127.0.0.1:1".into(),
            orders_ws: "ws://127.0.0.1:1".into(),
        },
    );
    let c = Corpus {
        malformed: &[
            ("success without data", r#"{"status":"success"}"#),
            (
                "success with text data",
                r#"{"status":"success","data":"x"}"#,
            ),
        ],
        empty: r#"{"status":"success","data":[]}"#,
        row: r#"{"status":"success","data":[{"security_id":"3045","symbol":"SBIN-EQ","exchange":"NSE","segment":"EQUITY","net_qty":5}]}"#,
        no_quantity: r#"{"status":"success","data":[{"security_id":"3045","symbol":"SBIN-EQ","exchange":"NSE","segment":"EQUITY"}]}"#,
        bad_quantity: r#"{"status":"success","data":[{"security_id":"3045","symbol":"SBIN-EQ","exchange":"NSE","segment":"EQUITY","net_qty":"abc"}]}"#,
        exempt: &[],
    };
    check("indmoney", &b, &AuthToken::new("ind-tok-123"), &f, &c).await;
}

#[tokio::test]
async fn deltaexchange() {
    use openalgo_desktop_lib::brokers::deltaexchange::DeltaBroker;
    let f = fake().await;
    let b = DeltaBroker::with_urls(symbols(), &f.base, "ws://127.0.0.1:1");
    // Delta matches a position by its contract symbol alone.
    let c = Corpus {
        malformed: &[
            ("success without result", r#"{"success":true}"#),
            (
                "success with text result",
                r#"{"success":true,"result":"x"}"#,
            ),
        ],
        empty: r#"{"success":true,"result":[]}"#,
        row: r#"{"success":true,"result":[{"product_symbol":"SBIN-EQ","size":"5"}]}"#,
        no_quantity: r#"{"success":true,"result":[{"product_symbol":"SBIN-EQ"}]}"#,
        bad_quantity: r#"{"success":true,"result":[{"product_symbol":"SBIN-EQ","size":"abc"}]}"#,
        exempt: &[
            (
                "the holdings",
                "Delta has no demat holdings: always empty, without a broker call",
            ),
            (
                "cancel all",
                "Delta's bulk cancel answers `success: true` alone; that is its answer to the write, not a book",
            ),
        ],
    };
    check("deltaexchange", &b, &AuthToken::new("KEY:SECRET"), &f, &c).await;
}

#[tokio::test]
async fn arrow() {
    use openalgo_desktop_lib::brokers::arrow::{ArrowBroker, ArrowUrls};
    let f = fake().await;
    let b = ArrowBroker::with_base_url(symbols(), ArrowUrls::rebased(&f.base, "ws://127.0.0.1:1"));
    let c = Corpus {
        malformed: &[
            ("success without data", r#"{"status":"success"}"#),
            (
                "success with text data",
                r#"{"status":"success","data":"x"}"#,
            ),
        ],
        empty: r#"{"status":"success","data":[]}"#,
        row: r#"{"status":"success","data":[{"symbol":"SBIN-EQ","exchange":"NSE","product":"I","qty":5}]}"#,
        no_quantity: r#"{"status":"success","data":[{"symbol":"SBIN-EQ","exchange":"NSE","product":"I"}]}"#,
        bad_quantity: r#"{"status":"success","data":[{"symbol":"SBIN-EQ","exchange":"NSE","product":"I","qty":"abc"}]}"#,
        exempt: &[],
    };
    check("arrow", &b, &AuthToken::new("APP1:jwt-abc"), &f, &c).await;
}

#[tokio::test]
async fn pocketful() {
    use openalgo_desktop_lib::brokers::pocketful::PocketfulBroker;
    let f = fake().await;
    let b = PocketfulBroker::with_base_url(
        symbols(),
        &f.base,
        "ws://127.0.0.1:1",
        format!("{}/master", f.base),
    );
    let c = Corpus {
        malformed: &[
            ("success without data", r#"{"status":"success"}"#),
            (
                "success with text data",
                r#"{"status":"success","data":"x"}"#,
            ),
        ],
        empty: r#"{"status":"success","data":[]}"#,
        // fetch-position-netwise.md rows (web openalgo#2467).
        row: r#"{"status":"success","data":[{"trading_symbol":"SBIN-EQ","instrument_token":3045,"exchange":"NSE","product":"MIS","net_quantity":5}]}"#,
        no_quantity: r#"{"status":"success","data":[{"trading_symbol":"SBIN-EQ","instrument_token":3045,"exchange":"NSE","product":"MIS"}]}"#,
        bad_quantity: r#"{"status":"success","data":[{"trading_symbol":"SBIN-EQ","instrument_token":3045,"exchange":"NSE","product":"MIS","net_quantity":"abc"}]}"#,
        exempt: &[],
    };
    let auth = AuthToken::new("acc-pf-1").with_user_id("<USER_ID>");
    check("pocketful", &b, &auth, &f, &c).await;
}

#[tokio::test]
async fn paytm() {
    use openalgo_desktop_lib::brokers::paytm::PaytmBroker;
    let f = fake().await;
    let b = PaytmBroker::with_base_url(symbols(), &f.base, &f.base, "ws://127.0.0.1:1");
    let c = Corpus {
        malformed: &[
            (
                "success without data",
                r#"{"status":"success","message":"ok"}"#,
            ),
            (
                "success with text data",
                r#"{"status":"success","message":"ok","data":"x"}"#,
            ),
        ],
        empty: r#"{"status":"success","message":"","data":[]}"#,
        row: r#"{"status":"success","message":"","data":[{"security_id":"3045","exchange":"NSE","product":"I","instrument":"EQUITY","net_qty":5}]}"#,
        no_quantity: r#"{"status":"success","message":"","data":[{"security_id":"3045","exchange":"NSE","product":"I","instrument":"EQUITY"}]}"#,
        bad_quantity: r#"{"status":"success","message":"","data":[{"security_id":"3045","exchange":"NSE","product":"I","instrument":"EQUITY","net_qty":"abc"}]}"#,
        exempt: &[],
    };
    let auth = AuthToken::new("acc-1").with_feed(Some("pub-1"));
    check("paytm", &b, &auth, &f, &c).await;
}

#[tokio::test]
async fn hdfcsky() {
    use openalgo_desktop_lib::brokers::hdfcsky::HdfcSkyBroker;
    let f = fake().await;
    let b = HdfcSkyBroker::with_base_url(
        symbols(),
        &f.base,
        format!("{}/master.zip", f.base),
        "ws://127.0.0.1:1",
    );
    let c = Corpus {
        malformed: &[
            ("success without data", r#"{"status":"success"}"#),
            (
                "success with text data",
                r#"{"status":"success","data":"x"}"#,
            ),
        ],
        empty: r#"{"status":"success","data":[]}"#,
        row: r#"{"status":"success","data":[{"trading_symbol":"SBIN-EQ","exchange":"NSE","product":"MIS","net_quantity":5}]}"#,
        no_quantity: r#"{"status":"success","data":[{"trading_symbol":"SBIN-EQ","exchange":"NSE","product":"MIS"}]}"#,
        bad_quantity: r#"{"status":"success","data":[{"trading_symbol":"SBIN-EQ","exchange":"NSE","product":"MIS","net_quantity":"abc"}]}"#,
        exempt: &[],
    };
    let auth = AuthToken::new("APPKEY:eyJhbGciOiJIUzI1NiJ9.e30.signature");
    check("hdfcsky", &b, &auth, &f, &c).await;
}

#[tokio::test]
async fn hdfcsecurities() {
    use openalgo_desktop_lib::brokers::hdfcsecurities::HdfcSecuritiesBroker;
    let f = fake().await;
    let b = HdfcSecuritiesBroker::with_base_url(
        symbols(),
        &f.base,
        format!("{}/master", f.base),
        "ws://127.0.0.1:1",
    );
    let c = Corpus {
        malformed: &[
            ("success without data", r#"{"status":"success"}"#),
            (
                "success with text data",
                r#"{"status":"success","data":"x"}"#,
            ),
            ("positions without net", r#"{"status":"success","data":{}}"#),
        ],
        empty: r#"{"status":"success","data":{"net":[]}}"#,
        row: r#"{"status":"success","data":{"net":[{"security_id":"SBIN-EQ","exchange":"NSE","instrument_segment":"EQUITY","product":"INTRADAY","net_qty":5}]}}"#,
        no_quantity: r#"{"status":"success","data":{"net":[{"security_id":"SBIN-EQ","exchange":"NSE","instrument_segment":"EQUITY","product":"INTRADAY"}]}}"#,
        bad_quantity: r#"{"status":"success","data":{"net":[{"security_id":"SBIN-EQ","exchange":"NSE","instrument_segment":"EQUITY","product":"INTRADAY","net_qty":"abc"}]}}"#,
        exempt: &[],
    };
    check(
        "hdfcsecurities",
        &b,
        &AuthToken::new("APIKEY1:acc-tok"),
        &f,
        &c,
    )
    .await;
}

#[tokio::test]
async fn samco() {
    use openalgo_desktop_lib::brokers::samco::SamcoBroker;
    let f = fake().await;
    let b = SamcoBroker::with_urls(symbols(), &f.base, &f.base, "ws://127.0.0.1:1");
    let c = Corpus {
        malformed: &[
            (
                "success without the list",
                r#"{"status":"Success","statusMessage":"ok"}"#,
            ),
            (
                "success with a text list",
                r#"{"status":"Success","statusMessage":"ok","orderBookDetails":"x","positionDetails":"x","tradeBookDetails":"x","holdingDetails":"x"}"#,
            ),
        ],
        empty: r#"{"status":"Success","statusMessage":"ok","positionDetails":[]}"#,
        row: r#"{"status":"Success","statusMessage":"ok","positionDetails":[{"tradingSymbol":"SBIN-EQ","exchange":"NSE","productCode":"MIS","transactionType":"BUY","netQuantity":"5"}]}"#,
        no_quantity: r#"{"status":"Success","statusMessage":"ok","positionDetails":[{"tradingSymbol":"SBIN-EQ","exchange":"NSE","productCode":"MIS","transactionType":"BUY"}]}"#,
        bad_quantity: r#"{"status":"Success","statusMessage":"ok","positionDetails":[{"tradingSymbol":"SBIN-EQ","exchange":"NSE","productCode":"MIS","transactionType":"BUY","netQuantity":"abc"}]}"#,
        exempt: &[],
    };
    check("samco", &b, &AuthToken::new("<SESSION_TOKEN>"), &f, &c).await;
}

#[tokio::test]
async fn aliceblue() {
    use openalgo_desktop_lib::brokers::aliceblue::{AliceBlueBroker, Endpoints};
    let f = fake().await;
    let b = AliceBlueBroker::with_endpoints(
        symbols(),
        Endpoints {
            base: f.base.clone(),
            auth: f.base.clone(),
            master: format!("{}/master", f.base),
            ws: vec!["ws://127.0.0.1:1".into()],
            order_ws: "ws://127.0.0.1:1".into(),
        },
    );
    // AliceBlue answers an empty book with its EC920 envelope.
    let c = Corpus {
        malformed: &[
            ("ok without result", r#"{"status":"Ok"}"#),
            ("ok with text result", r#"{"status":"Ok","result":"x"}"#),
        ],
        empty: r#"{"status":"Not_Ok","message":"EC920"}"#,
        row: r#"{"status":"Ok","result":[{"tradingSymbol":"SBIN-EQ","exchange":"NSE","product":"INTRADAY","netQuantity":5}]}"#,
        no_quantity: r#"{"status":"Ok","result":[{"tradingSymbol":"SBIN-EQ","exchange":"NSE","product":"INTRADAY"}]}"#,
        bad_quantity: r#"{"status":"Ok","result":[{"tradingSymbol":"SBIN-EQ","exchange":"NSE","product":"INTRADAY","netQuantity":"abc"}]}"#,
        exempt: &[],
    };
    let auth = AuthToken::new("header.payload.signature").with_user_id("AB1");
    check("aliceblue", &b, &auth, &f, &c).await;
}

#[tokio::test]
async fn definedge() {
    use openalgo_desktop_lib::brokers::definedge::{DefinedgeBroker, Endpoints};
    let f = fake().await;
    let b = DefinedgeBroker::with_endpoints(
        symbols(),
        Endpoints {
            signin: format!("{}/signin", f.base),
            trade: format!("{}/dart/v1", f.base),
            data: format!("{}/sds", f.base),
            master: format!("{}/public/allmaster.zip", f.base),
            ws: "ws://127.0.0.1:9/NorenWSTRTP/".into(),
        },
    )
    .with_fast_timing();
    let c = Corpus {
        malformed: &[
            ("success without the list", r#"{"status":"SUCCESS"}"#),
            (
                "success with text lists",
                r#"{"status":"SUCCESS","orders":"x","positions":"x","trades":"x","data":"x"}"#,
            ),
        ],
        empty: r#"{"status":"SUCCESS","positions":[]}"#,
        row: r#"{"status":"SUCCESS","positions":[{"tradingsymbol":"SBIN-EQ","exchange":"NSE","product_type":"INTRADAY","net_quantity":"5"}]}"#,
        no_quantity: r#"{"status":"SUCCESS","positions":[{"tradingsymbol":"SBIN-EQ","exchange":"NSE","product_type":"INTRADAY"}]}"#,
        bad_quantity: r#"{"status":"SUCCESS","positions":[{"tradingsymbol":"SBIN-EQ","exchange":"NSE","product_type":"INTRADAY","net_quantity":"abc"}]}"#,
        exempt: &[],
    };
    let auth = AuthToken::new("sess-1:::suser-1:::API-TOKEN-1");
    check("definedge", &b, &auth, &f, &c).await;
}

#[tokio::test]
async fn mstock() {
    use openalgo_desktop_lib::brokers::mstock::MstockBroker;
    let f = fake().await;
    let b = MstockBroker::with_urls(symbols(), &f.base, &f.base, "ws://127.0.0.1:1");
    let c = Corpus {
        malformed: &[(
            "success with text data",
            r#"{"status":true,"message":"SUCCESS","data":"x"}"#,
        )],
        empty: r#"{"status":true,"message":"SUCCESS","data":null}"#,
        row: r#"{"status":true,"message":"SUCCESS","data":[{"symboltoken":"3045","exchange":"NSE","producttype":"INTRADAY","netqty":"5"}]}"#,
        no_quantity: r#"{"status":true,"message":"SUCCESS","data":[{"symboltoken":"3045","exchange":"NSE","producttype":"INTRADAY"}]}"#,
        bad_quantity: r#"{"status":true,"message":"SUCCESS","data":[{"symboltoken":"3045","exchange":"NSE","producttype":"INTRADAY","netqty":"abc"}]}"#,
        exempt: &[],
    };
    let auth = AuthToken::new("eyJhbGciOiJIUzI1NiJ9.e30.sig:::PRIVATEKEY123");
    check("mstock", &b, &auth, &f, &c).await;
}

#[tokio::test]
async fn motilal() {
    use openalgo_desktop_lib::brokers::motilal::MotilalBroker;
    let f = fake().await;
    let b = MotilalBroker::with_urls(symbols(), &f.base, "ws://127.0.0.1:1", "ws://127.0.0.1:1");
    let c = Corpus {
        malformed: &[(
            "success with text data",
            r#"{"status":"SUCCESS","data":"x"}"#,
        )],
        empty: r#"{"status":"SUCCESS","message":"","errorcode":"","data":null}"#,
        row: r#"{"status":"SUCCESS","message":"","errorcode":"","data":[{"symboltoken":3045,"exchange":"NSE","productname":"VALUEPLUS","buyquantity":5,"sellquantity":0}]}"#,
        no_quantity: r#"{"status":"SUCCESS","message":"","errorcode":"","data":[{"symboltoken":3045,"exchange":"NSE","productname":"VALUEPLUS"}]}"#,
        bad_quantity: r#"{"status":"SUCCESS","message":"","errorcode":"","data":[{"symboltoken":3045,"exchange":"NSE","productname":"VALUEPLUS","buyquantity":"abc","sellquantity":0}]}"#,
        exempt: &[],
    };
    let auth = AuthToken::new("AT1:::ACC1:::<USER_ID>:::KEY1:::SEC1");
    check("motilal", &b, &auth, &f, &c).await;
}

#[tokio::test]
async fn rupeezy() {
    use openalgo_desktop_lib::brokers::rupeezy::{RupeezyBroker, RupeezyUrls};
    let f = fake().await;
    let b =
        RupeezyBroker::with_base_url(symbols(), RupeezyUrls::rebased(&f.base, "ws://127.0.0.1:1"));
    // Vortex matches a position by its ticker and product.
    let c = Corpus {
        malformed: &[
            ("success without data", r#"{"status":"success"}"#),
            (
                "success with text data",
                r#"{"status":"success","data":"x"}"#,
            ),
            ("positions without net", r#"{"status":"success","data":{}}"#),
        ],
        empty: r#"{"status":"success","data":{"net":[]}}"#,
        row: r#"{"status":"success","data":{"net":[{"ticker":"NSE:SBIN-EQ","exchange":"NSE_EQ","product":"INTRADAY","quantity":5}]}}"#,
        no_quantity: r#"{"status":"success","data":{"net":[{"ticker":"NSE:SBIN-EQ","exchange":"NSE_EQ","product":"INTRADAY"}]}}"#,
        bad_quantity: r#"{"status":"success","data":{"net":[{"ticker":"NSE:SBIN-EQ","exchange":"NSE_EQ","product":"INTRADAY","quantity":"abc"}]}}"#,
        exempt: &[],
    };
    check("rupeezy", &b, &AuthToken::new("rupeezy-token"), &f, &c).await;
}

#[tokio::test]
async fn nuvama() {
    use openalgo_desktop_lib::brokers::nuvama::{NuvamaBroker, NuvamaUrls};
    let f = fake().await;
    let b = NuvamaBroker::with_base_url(symbols(), NuvamaUrls::rebased(&f.base, 1));
    // web `test_position_read_failure` (nuvama): a position matches by its
    // token (`sym`) and product; the empty book is the ETRD0004 error
    // object (HTTP 222 in production).
    let c = Corpus {
        malformed: &[
            ("data without its list", r#"{"data":{}}"#),
            (
                "a null list",
                r#"{"data":{"pos":null,"ord":null,"trade":null,"rmsHdg":null}}"#,
            ),
            ("text data", r#"{"data":"x"}"#),
        ],
        empty: r#"{"error":{"actCd":"54","errCd":"ETRD0004","errMsg":"Seems like there are no positions in your account currently"}}"#,
        row: r#"{"data":{"pos":[{"sym":"3045","trdSym":"INE062A01020","exc":"NSE","prdCode":"MIS","ntQty":"5"}]}}"#,
        no_quantity: r#"{"data":{"pos":[{"sym":"3045","trdSym":"INE062A01020","exc":"NSE","prdCode":"MIS"}]}}"#,
        bad_quantity: r#"{"data":{"pos":[{"sym":"3045","trdSym":"INE062A01020","exc":"NSE","prdCode":"MIS","ntQty":"abc"}]}}"#,
        exempt: &[],
    };
    check(
        "nuvama",
        &b,
        &AuthToken::new("AUTH:::SOURCE:::APPID:::ACC01:::EQ:::KEY01"),
        &f,
        &c,
    )
    .await;
}
