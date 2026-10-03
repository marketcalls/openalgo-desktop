//! The Fyers HSM feed through the real feed manager against a local fake
//! HSM server: no `Authorization` header on the handshake, the binary auth
//! frame carries the JWT's `hsm_key`, subscriptions wait for the `"K"` ack,
//! topics are `sf|nse_cm|<token>`, and a snapshot frame becomes a tick.

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::streaming::{
    FeedEvent, FeedMode, FeedSubscription, Message,
};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::fyers::master_contract::parse_all;
use openalgo_desktop_lib::brokers::fyers::streaming::{subscribe_frame, HSM_ABSENT};
use openalgo_desktop_lib::brokers::fyers::{FyersBroker, FyersUrls};
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use openalgo_desktop_lib::websocket::{FeedConfig, WebSocketManager};
use std::collections::HashMap;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

fn master() -> SymbolResolver {
    let mut files: HashMap<&str, String> = HashMap::new();
    for (k, v) in [
        (
            "NSE_CM",
            include_str!("../fixtures/brokers/fyers/NSE_CM.csv"),
        ),
        (
            "BSE_CM",
            include_str!("../fixtures/brokers/fyers/BSE_CM.csv"),
        ),
        (
            "NSE_FO",
            include_str!("../fixtures/brokers/fyers/NSE_FO.csv"),
        ),
        (
            "BSE_FO",
            include_str!("../fixtures/brokers/fyers/BSE_FO.csv"),
        ),
        (
            "NSE_CD",
            include_str!("../fixtures/brokers/fyers/NSE_CD_sym_master.json"),
        ),
        (
            "MCX_COM",
            include_str!("../fixtures/brokers/fyers/MCX_COM_sym_master.json"),
        ),
    ] {
        files.insert(k, v.to_string());
    }
    let r = SymbolResolver::new();
    r.load(parse_all(&files, &HashMap::new()).unwrap());
    r
}

fn auth() -> AuthToken {
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(br#"{"hsm_key":"hsmkey123","exp":4102444800}"#);
    AuthToken::new(format!("APPID-100:eyJhbGciOiJIUzI1NiJ9.{}.c2ln", payload))
}

/// Type-6 frame with one sf snapshot (`_parse_snapshot_data`).
fn snapshot_frame(topic: &str, ltp_raw: i32) -> Vec<u8> {
    let mut v = vec![0u8, 0, 6, 0, 0, 0, 0, 0, 1, 83];
    v.extend(1u16.to_le_bytes());
    v.push(topic.len() as u8);
    v.extend(topic.as_bytes());
    let mut fields = vec![HSM_ABSENT; 21];
    fields[0] = ltp_raw;
    fields[20] = 9_506_500;
    v.push(fields.len() as u8);
    for f in fields {
        v.extend(f.to_be_bytes());
    }
    v.extend([0, 0]);
    v.extend(100u16.to_be_bytes());
    v.push(2);
    for s in ["NSE", "3045", "SBIN-EQ"] {
        v.push(s.len() as u8);
        v.extend(s.as_bytes());
    }
    v
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hsm_feed_authenticates_subscribes_and_ticks() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let (seen_tx, seen_rx) = oneshot::channel::<(bool, Vec<u8>, Vec<u8>)>();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut had_auth_header = false;
        let mut ws = tokio_tungstenite::accept_hdr_async(tcp, |req: &Request, resp: Response| {
            had_auth_header = req.headers().contains_key("authorization");
            Ok(resp)
        })
        .await
        .unwrap();
        let auth = match ws.next().await {
            Some(Ok(Message::Binary(b))) => b,
            other => panic!("expected the auth frame, got {:?}", other),
        };
        // Accept the session: field 1 is "K".
        ws.send(Message::Binary(vec![0, 6, 1, 1, 1, 0, 1, b'K']))
            .await
            .unwrap();
        let sub = loop {
            match ws.next().await {
                Some(Ok(Message::Binary(b))) => break b,
                Some(Ok(_)) => continue,
                other => panic!("expected the subscribe frame, got {:?}", other),
            }
        };
        ws.send(Message::Binary(snapshot_frame("sf|nse_cm|3045", 9_541_000)))
            .await
            .unwrap();
        let _ = seen_tx.send((had_auth_header, auth, sub));
        // Hold the socket until the client goes away.
        while let Some(Ok(_)) = ws.next().await {}
    });

    let broker = FyersBroker::with_urls(
        master(),
        FyersUrls {
            hsm: url,
            ..FyersUrls::default()
        },
    );
    let manager = WebSocketManager::with_config(FeedConfig {
        connect_timeout: Duration::from_secs(5),
        ..FeedConfig::default()
    });
    let mut ticks = manager.subscribe_ticks();
    manager
        .subscribe(vec![FeedSubscription {
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            token: "10100000003045".into(),
            brsymbol: "NSE:SBIN-EQ".into(),
            brexchange: "NSE".into(),
            mode: FeedMode::Quote,
            depth: 5,
        }])
        .await
        .unwrap();
    manager
        .connect(broker.create_feed(&auth()).unwrap())
        .await
        .unwrap();

    let tick = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(ev) = ticks.recv().await {
                if let FeedEvent::Tick(t) = &*ev {
                    return t.clone();
                }
            }
        }
    })
    .await
    .expect("a tick from the fake HSM server");
    assert_eq!(
        (tick.symbol.as_str(), tick.exchange.as_str()),
        ("SBIN", "NSE")
    );
    assert_eq!(tick.ltp, 954.1);
    assert_eq!(tick.close, 950.65);

    let (had_auth_header, auth_frame, sub_frame) =
        tokio::time::timeout(Duration::from_secs(5), seen_rx)
            .await
            .unwrap()
            .unwrap();
    assert!(
        !had_auth_header,
        "HSM handshake must not carry Authorization"
    );
    assert_eq!(auth_frame[2], 1, "request type 1 is authentication");
    assert!(auth_frame
        .windows(b"hsmkey123".len())
        .any(|w| w == b"hsmkey123"));
    assert_eq!(sub_frame, subscribe_frame(&["sf|nse_cm|3045".into()], 11));

    manager.disconnect().await.unwrap();
    server.abort();
}
