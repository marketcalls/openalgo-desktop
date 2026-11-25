//! Web `test/test_groww_order_adapter.py` (openalgo#2474, f0a50b038): the
//! order push decoder, the update a push makes with and without the
//! order's detail, the NATS handshake, a message split across frames, the
//! socket-token request and the order feed the broker builds. Payloads
//! are written field by field as the web test's `_order_payload` writes
//! them; the master stands in for the web test's patched symbol lookup.

use super::*;
use crate::brokers::common::http;
use crate::brokers::common::streaming::OrderFeed;
use crate::brokers::common::symbols::SymToken;
use crate::brokers::groww::nkeys::KeyPair;
use crate::brokers::groww::streaming::GrowwFeed;
use crate::brokers::groww::GrowwBroker;
use crate::brokers::Broker;
use parking_lot::Mutex;
use std::sync::Arc;

// OrderDetailUpdateDto field numbers (web `_ORDER_FIELDS`).
const QTY: u64 = 1;
const PRICE: u64 = 2;
const FILLED: u64 = 4;
const REMAINING: u64 = 5;
const AVG_FILL: u64 = 6;
const ID: u64 = 7;
const EXCHANGE_ORDER_ID: u64 = 8;
const STATUS: u64 = 9;
const EXCHANGE: u64 = 11;
const SEGMENT: u64 = 12;
const PRODUCT: u64 = 13;
const ORDER_TYPE: u64 = 14;
const SIDE: u64 = 15;
const REMARK: u64 = 16;
const CONTRACT: u64 = 22;

// Enum wire values (StockOrdersSocketResponse.proto).
const TRIGGER_PENDING: u64 = 2;
const REJECTED: u64 = 4;
const EXECUTED: u64 = 6;
const BSE: u64 = 0;
const NSE: u64 = 1;
const CASH: u64 = 0;
const FNO: u64 = 1;
const NRML: u64 = 4;
const LIMIT: u64 = 1;
const SELL: u64 = 1;

fn varint(mut v: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

fn number_field(number: u64, v: u64) -> Vec<u8> {
    let mut out = varint(number << 3);
    out.extend(varint(v));
    out
}

fn bytes_field(number: u64, v: &[u8]) -> Vec<u8> {
    let mut out = varint((number << 3) | 2);
    out.extend(varint(v.len() as u64));
    out.extend_from_slice(v);
    out
}

/// One field of a push as the web test writes it.
enum F {
    Int(u64, i64),
    /// An enum, left off the wire at 0 as proto3 leaves it.
    Enum(u64, u64),
    Text(u64, &'static str),
}

/// `OrderDetailsBroadCastDto`: a stage message (field 1) and the update
/// (field 2), as the web test's `_order_payload` builds it.
fn payload(fields: &[F]) -> Vec<u8> {
    let mut inner = Vec::new();
    for f in fields {
        match *f {
            F::Int(n, v) => inner.extend(number_field(n, v as u64)),
            F::Enum(_, 0) => {}
            F::Enum(n, v) => inner.extend(number_field(n, v)),
            F::Text(n, t) => inner.extend(bytes_field(n, t.as_bytes())),
        }
    }
    let mut out = bytes_field(1, &bytes_field(2, b"ORDER_PLACED"));
    out.extend(bytes_field(2, &inner));
    out
}

/// The web test's `FNO_PUSH`.
fn fno_push() -> Vec<F> {
    vec![
        F::Int(QTY, 75),
        F::Int(PRICE, 130),
        F::Int(FILLED, 75),
        F::Int(REMAINING, 0),
        F::Int(AVG_FILL, 110),
        F::Text(ID, "GMKFO250214150557M2HR6EJF2HSE"),
        F::Text(EXCHANGE_ORDER_ID, "1400000179694433"),
        F::Enum(STATUS, EXECUTED),
        F::Enum(EXCHANGE, NSE),
        F::Enum(SEGMENT, FNO),
        F::Enum(PRODUCT, NRML),
        F::Enum(ORDER_TYPE, LIMIT),
        F::Enum(SIDE, SELL),
        F::Text(CONTRACT, "NIFTY2522025400CE"),
    ]
}

fn row(symbol: &str, brsymbol: &str, exchange: &str) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: brsymbol.into(),
        name: symbol.into(),
        exchange: exchange.into(),
        brexchange: exchange.into(),
        token: "1".into(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: "EQ".into(),
        tick_size: 0.05,
    }
}

/// The master the cases resolve against: IDEA, and the F&O contract the
/// web test pushes, whose OpenAlgo symbol differs from Groww's.
fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(vec![
        row("IDEA", "IDEA", "NSE"),
        row("NIFTY20FEB2525400CE", "NIFTY2522025400CE", "NFO"),
    ]);
    r
}

fn decode(fields: &[F]) -> OrderPush {
    order_push::decode(&payload(fields)).expect("an order push")
}

fn update(fields: &[F]) -> OrderUpdate {
    update_from_push(&decode(fields), &master())
}

type Seen = Arc<Mutex<Vec<String>>>;

/// A server answering every request with `status` and `body`, keeping each
/// request's head (request line and headers), lowercased.
async fn server(status: u16, body: &'static str) -> (String, Seen) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let seen: Seen = Arc::default();
    let log = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            let mut head = Vec::new();
            let mut buf = [0u8; 4096];
            while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                match s.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => head.extend_from_slice(&buf[..n]),
                }
            }
            log.lock()
                .push(String::from_utf8_lossy(&head).to_ascii_lowercase());
            let reply = format!(
                "HTTP/1.1 {} X\r\ncontent-type: application/json\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{}",
                status,
                body.len(),
                body
            );
            let _ = s.write_all(reply.as_bytes()).await;
        }
    });
    (url, seen)
}

fn order_feed(socket_token_url: String) -> GrowwOrderFeed {
    GrowwOrderFeed::new(
        http::client(),
        "LOGINTOKEN",
        FeedEndpoints {
            socket_token_url,
            ws_url: "ws://127.0.0.1:9/".into(),
        },
        master(),
    )
}

fn replies(events: &[FeedEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            FeedEvent::Reply(Message::Text(t)) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

/// Web test_groww_order_adapter.py::test_decoder_reads_every_field_and_defaults_zero_enums
#[test]
fn the_decoder_reads_every_field_and_defaults_zero_enums() {
    let push = decode(&[
        F::Int(QTY, 3),
        F::Int(FILLED, 3),
        F::Text(ID, "GMK1"),
        F::Enum(STATUS, EXECUTED),
        F::Enum(EXCHANGE, BSE),
        F::Enum(SEGMENT, CASH),
        F::Text(CONTRACT, "INE221H01019"),
    ]);
    assert_eq!(push.qty, 3);
    assert_eq!(push.groww_order_id, "GMK1");
    assert_eq!(push.order_status, "EXECUTED");
    // Zero enums are absent on the wire: BSE, CASH, CNC, MARKET, BUY.
    assert_eq!(
        (
            push.exchange.as_str(),
            push.segment.as_str(),
            push.product.as_str()
        ),
        ("BSE", "CASH", "CNC")
    );
    assert_eq!(
        (push.order_type.as_str(), push.transaction_type.as_str()),
        ("MARKET", "BUY")
    );
    assert_eq!(push.contract_id, "INE221H01019");
}

/// The web decoder's edges: an enum past the names is kept as its number,
/// a negative int64 is its two's complement, and a message the reader
/// cannot walk, or one without an update, is none.
#[test]
fn a_push_the_reader_cannot_walk_is_no_update() {
    let push = decode(&[F::Text(ID, "G1"), F::Enum(STATUS, 42), F::Int(QTY, -1)]);
    assert_eq!((push.order_status.as_str(), push.qty), ("42", -1));
    // Wire type 3 (a group start) is not one the web reads.
    let mut groups = payload(&[F::Text(ID, "G1")]);
    groups.extend([(30 << 3) | 3]);
    assert_eq!(order_push::decode(&groups), None);
    // A varint cut off at the end of the message.
    assert_eq!(order_push::decode(&[0x10, 0x80]), None);
    // Only the stage, no update (field 2).
    assert_eq!(
        order_push::decode(&bytes_field(1, &bytes_field(2, b"ORDER_PLACED"))),
        None
    );
}

/// Web test_groww_order_adapter.py::test_order_detail_supplies_symbol_and_prices
#[tokio::test]
async fn the_order_detail_supplies_symbol_and_prices() {
    let (url, seen) = server(
        200,
        r#"{"status":"SUCCESS","payload":{"groww_order_id":"GMK250502123553ZXM5BKVXX9LM","trading_symbol":"IDEA","exchange":"NSE","segment":"CASH","transaction_type":"BUY","order_type":"SL_M","product":"CNC","price":0,"trigger_price":14.5,"average_fill_price":14.55}}"#,
    )
    .await;
    let pushed = update(&[
        F::Int(QTY, 3),
        F::Int(FILLED, 3),
        F::Text(ID, "GMK250502123553ZXM5BKVXX9LM"),
        F::Enum(STATUS, EXECUTED),
        F::Enum(EXCHANGE, NSE),
        F::Text(CONTRACT, "INE669E01016"),
    ]);
    let core = GrowwCore::new(master(), &url);
    let done = complete(&core, &AuthToken::new("tok"), pushed).await;
    let heads = seen.lock().clone();
    assert_eq!(heads.len(), 1, "{:?}", heads);
    assert!(
        heads[0]
            .starts_with("get /v1/order/detail/gmk250502123553zxm5bkvxx9lm?segment=cash http/1.1"),
        "{}",
        heads[0]
    );
    assert_eq!(
        done,
        OrderUpdate {
            orderid: "GMK250502123553ZXM5BKVXX9LM".into(),
            order_status: "complete".into(),
            quantity: 3,
            filled_quantity: 3,
            pending_quantity: 0,
            rejection_reason: String::new(),
            symbol: "IDEA".into(),
            exchange: "NSE".into(),
            action: "BUY".into(),
            price: 0.0,
            trigger_price: 14.5,
            pricetype: "SL-M".into(),
            product: "CNC".into(),
            average_price: 14.55,
        }
    );
}

/// A detail's F&O contract maps through the master to its OpenAlgo symbol
/// on NFO, as the order book maps it.
#[test]
fn a_detail_of_an_fno_order_names_its_openalgo_contract() {
    let detail: GrowwOrder = serde_json::from_value(serde_json::json!({
        "trading_symbol": "NIFTY2522025400CE", "exchange": "NSE", "segment": "FNO",
        "transaction_type": "SELL", "order_type": "LIMIT", "product": "NRML",
        "price": 130.5, "trigger_price": 0, "average_fill_price": 110.25
    }))
    .unwrap();
    let done = with_detail(update(&fno_push()), &detail, &master());
    assert_eq!(
        (done.symbol.as_str(), done.exchange.as_str()),
        ("NIFTY20FEB2525400CE", "NFO")
    );
    assert_eq!((done.price, done.average_price), (130.5, 110.25));
    assert_eq!((done.quantity, done.filled_quantity), (75, 75));
}

/// Web test_groww_order_adapter.py::test_push_fields_are_used_when_the_detail_read_fails
#[tokio::test]
async fn the_push_fields_stay_when_the_detail_read_fails() {
    let failed = r#"{"status":"FAILURE","error":{"code":"GA004","message":"Order not found"}}"#;
    let (refusing, _) = server(400, failed).await;
    let (empty, _) = server(200, r#"{"status":"SUCCESS","payload":{}}"#).await;
    for base in ["http://127.0.0.1:9".to_string(), refusing, empty] {
        let core = GrowwCore::new(master(), &base);
        let fields = complete(&core, &AuthToken::new("tok"), update(&fno_push())).await;
        assert_eq!(fields.exchange, "NFO", "{}", base);
        assert_eq!(fields.symbol, "NIFTY20FEB2525400CE");
        assert_eq!(fields.action, "SELL");
        assert_eq!(fields.pricetype, "LIMIT");
        assert_eq!(fields.product, "NRML");
        // The push's integer prices have no documented unit, so none are
        // reported: the event's defaults.
        assert_eq!(
            (fields.price, fields.trigger_price, fields.average_price),
            (0.0, 0.0, 0.0)
        );
        assert_eq!(fields.order_status, "complete");
    }
}

/// Web test_groww_order_adapter.py::test_rejection_and_trigger_pending
#[test]
fn a_rejection_carries_its_remark_and_a_trigger_wait_does_not() {
    let rejected = update(&[
        F::Text(ID, "G1"),
        F::Enum(STATUS, REJECTED),
        F::Text(REMARK, "Insufficient margin"),
    ]);
    let pending = update(&[
        F::Text(ID, "G2"),
        F::Enum(STATUS, TRIGGER_PENDING),
        F::Text(REMARK, "Order placed"),
    ]);
    assert_eq!(rejected.order_status, "rejected");
    assert_eq!(rejected.rejection_reason, "Insufficient margin");
    assert_eq!(pending.order_status, "trigger pending");
    assert_eq!(pending.rejection_reason, "");
}

/// Web test_groww_order_adapter.py::test_handshake_answers_first_info_only_and_subscribes_both_subjects
#[test]
fn the_handshake_answers_the_first_info_only_and_subscribes_both_subjects() {
    let key = KeyPair::from_seed(&[7u8; 32]);
    let (public, sig) = (key.public_key(), key.sign_nonce("abc"));
    let mut feed = order_feed("http://127.0.0.1:9/token".into());
    feed.set_minted("jwt", "sub-1", Some(key));
    feed.on_connected();

    let info = Message::Text("INFO {\"server_id\":\"x\",\"nonce\":\"abc\"}\r\n".into());
    let sent = replies(&feed.parse(&info));
    assert_eq!(sent.len(), 1, "one frame: {:?}", sent);
    let lines: Vec<&str> = sent[0].split("\r\n").collect();
    assert_eq!(lines.len(), 5, "{:?}", lines);
    let connect: Value = serde_json::from_str(&lines[0]["CONNECT ".len()..]).unwrap();
    assert_eq!(connect["jwt"], "jwt");
    assert_eq!(connect["nkey"], public.as_str());
    assert_eq!(connect["sig"], sig.as_str());
    assert_eq!(lines[1], "SUB stocks/order/updates.apex.sub-1 1");
    assert_eq!(lines[2], "SUB stocks_fo/order/updates.apex.sub-1 2");
    assert_eq!((lines[3], lines[4]), ("PING", ""));

    // The INFO after CONNECT.
    assert!(replies(&feed.parse(&info)).is_empty());
    assert_eq!(
        replies(&feed.parse(&Message::Text("PING\r\n".into()))),
        vec!["PONG\r\n".to_string()]
    );
    // A new connection handshakes again.
    feed.on_connected();
    assert_eq!(replies(&feed.parse(&info)).len(), 1);
}

/// Web test_groww_order_adapter.py::test_msg_split_across_frames_is_reassembled
#[test]
fn a_message_split_across_frames_is_reassembled() {
    let mut feed = order_feed("http://127.0.0.1:9/token".into());
    let body = payload(&fno_push());
    let mut frame = format!(
        "MSG stocks_fo/order/updates.apex.sub-1 2 {}\r\n",
        body.len()
    )
    .into_bytes();
    frame.extend_from_slice(&body);
    frame.extend_from_slice(b"\r\n");
    assert!(feed
        .parse(&Message::Binary(frame[..30].to_vec()))
        .is_empty());
    let mut rest = frame[30..].to_vec();
    rest.extend_from_slice(b"PONG\r\n");
    let events = feed.parse(&Message::Binary(rest));
    let ids: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            FeedEvent::OrderUpdate(u) => Some(u.orderid.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(ids, vec!["GMKFO250214150557M2HR6EJF2HSE"]);
    assert_eq!(events.last(), Some(&FeedEvent::Heartbeat));
}

/// Web test_groww_order_adapter.py::test_socket_token_refusal_stands_the_adapter_down:
/// a 401 or 403 is an expired login and stops the order feed; any other
/// failure, or an answer without the subscription id its subjects need, is
/// retried. The login token never reaches the socket.
#[tokio::test]
async fn a_refused_socket_token_stops_the_order_feed() {
    for status in [401, 403] {
        let (url, _) = server(status, "{}").await;
        match order_feed(url).prepare().await {
            Err(PrepareError::AuthFailed(m)) => assert_eq!(m, ORDERS_LOGIN_EXPIRED),
            other => panic!("{}: {:?}", status, other.err()),
        }
    }
    for (status, body) in [(500, "{}"), (200, r#"{"token":"SOCKETJWT"}"#)] {
        let (url, _) = server(status, body).await;
        assert_eq!(
            order_feed(url).prepare().await,
            Err(PrepareError::Unavailable),
            "{} {}",
            status,
            body
        );
    }
    let (url, _) = server(200, r#"{"token":"SOCKETJWT","subscriptionId":"sub-1"}"#).await;
    let mut feed = order_feed(url);
    assert_eq!(feed.prepare().await, Ok(()));
    let req = feed.ws_request().unwrap();
    let h = req.headers();
    assert_eq!(h["authorization"], "Bearer SOCKETJWT");
    assert_eq!(h["x-subscription-id"], "sub-1");
    assert_eq!(h["sec-websocket-protocol"], "nats");
    assert!(!format!("{:?}", h).contains("LOGINTOKEN"));
}

/// Web test_groww_order_adapter.py::test_socket_token_request_sends_the_sdk_headers:
/// the order adapter asks as growwapi 1.5.0 does; the market feed still
/// asks as the web's market data client (0.0.8).
#[tokio::test]
async fn the_socket_token_request_sends_the_sdk_headers() {
    let answer = r#"{"token":"jwt","subscriptionId":"sub-1"}"#;
    let (url, seen) = server(200, answer).await;
    let mut feed = order_feed(format!("{}/token", url));
    assert_eq!(feed.prepare().await, Ok(()));
    let (market_url, market_seen) = server(200, answer).await;
    let mut market = GrowwFeed::new(
        http::client(),
        "tok",
        FeedEndpoints {
            socket_token_url: format!("{}/token", market_url),
            ws_url: "ws://127.0.0.1:9/".into(),
        },
    );
    assert!(market.prepare().await.is_ok());

    let head = seen.lock()[0].clone();
    for line in [
        "post /token http/1.1",
        "authorization: bearer logintoken",
        "x-client-id: growwapi",
        "x-client-platform: growwapi-python-client",
        "x-client-platform-version: 1.5.0",
        "x-api-version: 1.0",
    ] {
        assert!(head.contains(line), "{} in {}", line, head);
    }
    let market_head = market_seen.lock()[0].clone();
    assert!(
        market_head.contains("x-client-platform-version: 0.0.8"),
        "{}",
        market_head
    );
}

/// Web test_groww_order_adapter.py::test_factory_without_session_returns_none
/// and ::test_factory_with_session_builds_the_adapter: the broker's order
/// feed is the order socket (no longer the order-book poller), and needs a
/// session.
#[test]
fn the_order_feed_is_a_socket_that_needs_a_session() {
    let b = GrowwBroker::with_base_url(master(), "http://127.0.0.1:9");
    assert!(b.create_order_feed(&AuthToken::new(" ")).is_err());
    match b.create_order_feed(&AuthToken::new("tok")) {
        Ok(OrderFeed::Socket(feed)) => {
            assert_eq!(feed.broker(), "groww");
            // No socket token yet: nothing to connect with.
            assert!(feed.ws_request().is_err());
        }
        other => panic!("expected the order socket: {:?}", other),
    }
    assert!(b.capabilities().order_feed);
}
