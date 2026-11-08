//! The XTS feed against each member's web adapter (WebSocket parity,
//! audit FB-04, FB-05, FB-06 and the family appendix's item 1), from the
//! raw Socket.IO event to the normalised tick and book. The corpus cases
//! (`tests/fixtures/feed_corpus/<member>/`) take the same events on to the
//! public frame.

use super::socketio::encode_event;
use super::streaming::{FeedSource, XtsFeed};
use super::{XtsBroker, XtsConfig};
use crate::brokers::common::streaming::{
    BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth, NormalizedTick,
    QuoteField,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::{compositedge, ibulls, jainamxts, rmoney, Broker};
use serde_json::{json, Value};

fn feed(cfg: &'static XtsConfig) -> XtsFeed {
    let mut f = XtsFeed::new(
        cfg,
        reqwest::Client::new(),
        "https://example.invalid".into(),
        None,
        FeedSource::default(),
    );
    f.insert(FeedSubscription {
        symbol: "RELIANCE".into(),
        exchange: "NSE".into(),
        token: "2885".into(),
        brsymbol: "RELIANCE-EQ".into(),
        brexchange: "NSECM".into(),
        mode: FeedMode::Depth,
        depth: 5,
    });
    f
}

/// The ticks and books one Socket.IO event makes.
fn decode(
    f: &mut XtsFeed,
    name: &str,
    payload: Value,
) -> (Vec<NormalizedTick>, Vec<NormalizedDepth>) {
    let mut ticks = Vec::new();
    let mut books = Vec::new();
    for ev in f.parse(&Message::Text(encode_event(name, &[payload]))) {
        match ev {
            FeedEvent::Tick(t) => ticks.push(t),
            FeedEvent::Depth(d) => books.push(d),
            _ => {}
        }
    }
    (ticks, books)
}

fn touchline() -> Value {
    json!({
        "LastTradedPrice": 2500.25, "LastTradedQunatity": 7, "TotalBuyQuantity": 150000,
        "TotalSellQuantity": 160000, "TotalTradedQuantity": 4500123, "AverageTradedPrice": 2495.8,
        "LastTradedTime": 1478686530, "Open": 2490.0, "High": 2510.5, "Low": 2485.0, "Close": 2487.6
    })
}

fn level(price: f64, size: i64, orders: i64) -> Value {
    json!({"Price": price, "Size": size, "TotalOrders": orders})
}

fn depth_1502() -> Value {
    json!({
        "MessageCode": 1502, "ExchangeSegment": 1, "ExchangeInstrumentID": 2885,
        "Touchline": touchline(),
        "Bids": [level(2500.1, 120, 3), level(2500.05, 80, 2)],
        "Asks": [level(2500.4, 95, 4)]
    })
}

/// Appendix item 1 (both sides zeroed what a partial lacks): a 1501 that
/// carries only the last price says so, so the bridge keeps the rest of
/// the quote, and with no trade time it sends no receipt time as one.
#[test]
fn a_partial_carries_only_what_it_names() {
    let mut f = feed(&ibulls::CONFIG);
    let (t, _) = decode(
        &mut f,
        "1501-json-partial",
        json!({"MessageCode": 1501, "ExchangeSegment": 1, "ExchangeInstrumentID": 2885,
               "LastTradedPrice": 2502.0}),
    );
    assert_eq!((t[0].mode, t[0].ltp), (2, 2502.0));
    assert!(t[0].knows(QuoteField::Ltp));
    for field in [
        QuoteField::Open,
        QuoteField::Close,
        QuoteField::Volume,
        QuoteField::AveragePrice,
        QuoteField::LastQuantity,
        QuoteField::TotalBuyQuantity,
        QuoteField::LastTradeTime,
    ] {
        assert!(!t[0].knows(field), "{:?}", field);
    }
}

/// The web's XTS LTP data is `{ltp, ltt, ltq}`: a 1512 carries its last
/// quantity beyond its mode.
#[test]
fn an_ltp_event_carries_its_last_quantity() {
    let mut f = feed(&compositedge::CONFIG);
    let (t, _) = decode(
        &mut f,
        "1512-json-full",
        json!({"MessageCode": 1512, "ExchangeSegment": 1, "ExchangeInstrumentID": 2885,
               "LastTradedPrice": 2501.5, "LastTradedQunatity": 7, "LastTradedTime": 1478686530}),
    );
    assert_eq!((t[0].mode, t[0].last_quantity), (1, 7));
    assert!(t[0].knows(QuoteField::LastQuantity));
    assert!(t[0].knows(QuoteField::LastTradeTime));
}

/// The book is the levels XTS sent (the web pads each side to five zero
/// levels); a cleared side is empty. OI and the circuit limits go only
/// when the 1502 carries them.
#[test]
fn a_book_is_the_levels_sent_and_its_extras_only_when_sent() {
    let mut f = feed(&ibulls::CONFIG);
    let (t, b) = decode(&mut f, "1502-json-full", depth_1502());
    assert_eq!((b[0].buy.len(), b[0].sell.len()), (2, 1));
    assert_eq!(b[0].buy[1].price, 2500.05);
    assert!(!t[0].knows(QuoteField::OpenInterest));
    assert_eq!(t[0].extras.upper_circuit, None);

    let mut cleared = depth_1502();
    cleared["Bids"] = json!([]);
    cleared["Asks"] = json!([]);
    cleared["OpenInterest"] = json!(0);
    cleared["UpperCircuitLimit"] = json!(2751.0);
    cleared["LowerCircuitLimit"] = json!(2251.0);
    let (t, b) = decode(&mut f, "1502-json-full", cleared);
    assert!(b[0].buy.is_empty() && b[0].sell.is_empty());
    assert!(t[0].knows(QuoteField::OpenInterest));
    assert_eq!(
        (t[0].extras.upper_circuit, t[0].extras.lower_circuit),
        (Some(2751.0), Some(2251.0))
    );
}

/// FB-06: twenty levels on NSE and NFO, as every member's web registry
/// offers, five elsewhere.
#[test]
fn twenty_levels_on_nse_and_nfo() {
    let b = XtsBroker::new(&ibulls::CONFIG, SymbolResolver::new());
    assert_eq!(b.feed_depth_levels("NSE"), vec![5, 20]);
    assert_eq!(b.feed_depth_levels("NFO"), vec![5, 20]);
    for exchange in ["BSE", "BFO", "MCX", "CDS", "NSE_INDEX"] {
        assert_eq!(b.feed_depth_levels(exchange), vec![5], "{}", exchange);
    }
}

/// FB-04: RMoney's web adapter reads camelCase keys, a segment name, a book
/// in Touchline's BidInfo/AskInfo and a book with one side; the template
/// members' adapters do not.
#[test]
fn rmoney_reads_its_payload_variants() {
    let mut camel = depth_1502();
    let o = camel.as_object_mut().unwrap();
    o.remove("ExchangeSegment");
    o.remove("ExchangeInstrumentID");
    o.insert("exchangeSegment".into(), json!(1));
    o.insert("exchangeInstrumentID".into(), json!(2885));
    let mut named = depth_1502();
    named["ExchangeSegment"] = json!("NSECM");
    let mut nested = depth_1502();
    let o = nested.as_object_mut().unwrap();
    o.remove("Bids");
    o.remove("Asks");
    nested["Touchline"]["BidInfo"] = json!([level(2500.1, 120, 3)]);
    nested["Touchline"]["AskInfo"] = json!([level(2500.4, 95, 4)]);
    let mut one_sided = depth_1502();
    one_sided.as_object_mut().unwrap().remove("Asks");

    for (what, payload, buy, sell) in [
        ("camelCase", camel.clone(), 2, 1),
        ("segment name", named.clone(), 2, 1),
        ("nested book", nested, 1, 1),
        ("one side", one_sided, 2, 0),
    ] {
        let (t, b) = decode(&mut feed(&rmoney::CONFIG), "1502-json-full", payload);
        assert_eq!(t.len(), 1, "{}", what);
        assert_eq!((b[0].buy.len(), b[0].sell.len()), (buy, sell), "{}", what);
    }
    for payload in [camel, named] {
        assert!(
            decode(&mut feed(&ibulls::CONFIG), "1502-json-full", payload)
                .0
                .is_empty()
        );
    }
}

/// FB-05: Jainam's and RMoney's web clients take the code of a payload
/// without one from its event (RMoney's always); the template's adapter
/// reads a code-less payload as a touchline.
#[test]
fn jainam_and_rmoney_take_the_code_from_the_event() {
    let mut no_code = depth_1502();
    no_code.as_object_mut().unwrap().remove("MessageCode");
    for (cfg, mode, books) in [
        (&jainamxts::CONFIG, 3, 1),
        (&rmoney::CONFIG, 3, 1),
        (&ibulls::CONFIG, 2, 0),
    ] {
        let (t, b) = decode(&mut feed(cfg), "1502-json-full", no_code.clone());
        assert_eq!((t[0].mode, b.len()), (mode, books), "{}", cfg.id);
    }
    // A payload's own code: Jainam keeps it, RMoney takes the event's.
    let mut quote_code = depth_1502();
    quote_code["MessageCode"] = json!(1501);
    let (t, _) = decode(
        &mut feed(&jainamxts::CONFIG),
        "1502-json-full",
        quote_code.clone(),
    );
    assert_eq!(t[0].mode, 2);
    let (t, b) = decode(&mut feed(&rmoney::CONFIG), "1502-json-full", quote_code);
    assert_eq!((t[0].mode, b.len()), (3, 1));
}

/// FB-05: Jainam and RMoney read a 1105 text event as LTP (1512); the
/// desktop reads it as a touchline for the members whose web adapter drops
/// it.
#[test]
fn jainam_and_rmoney_read_1105_text_as_ltp() {
    let text = json!("t:1_2885,110:2503.25,111:4,112:4512345,114:2490");
    for (cfg, mode) in [
        (&jainamxts::CONFIG, 1),
        (&rmoney::CONFIG, 1),
        (&ibulls::CONFIG, 2),
    ] {
        let (t, _) = decode(&mut feed(cfg), "1105-json-partial", text.clone());
        assert_eq!((t[0].mode, t[0].ltp), (mode, 2503.25), "{}", cfg.id);
        assert!(!t[0].knows(QuoteField::LastTradeTime), "{}", cfg.id);
    }
}

/// FB-05: Jainam's web adapter publishes a 1510 open-interest event; the
/// desktop keeps its OI for the next priced message and publishes nothing
/// from a message without a trade.
#[test]
fn jainam_keeps_the_oi_of_a_1510() {
    let oi = json!({"MessageCode": 1510, "ExchangeSegment": 1, "ExchangeInstrumentID": 2885,
                    "OpenInterest": 12345675});
    let (t, _) = decode(&mut feed(&jainamxts::CONFIG), "1510-json-full", oi.clone());
    assert_eq!(t[0].oi, 12345675);
    assert!(t[0].knows(QuoteField::OpenInterest));
    assert!(!t[0].knows(QuoteField::Ltp));
    assert!(decode(&mut feed(&ibulls::CONFIG), "1510-json-full", oi)
        .0
        .is_empty());
}
