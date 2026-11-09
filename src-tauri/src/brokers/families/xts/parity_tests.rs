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
    Number, NumberForm, QuoteField,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::{compositedge, ibulls, jainamxts, rmoney, Broker};
use serde_json::{json, Value};

fn feed(cfg: &'static XtsConfig) -> XtsFeed {
    held_at(cfg, FeedMode::Depth)
}

/// A feed holding RELIANCE (NSECM 2885) at `mode`.
fn held_at(cfg: &'static XtsConfig, mode: FeedMode) -> XtsFeed {
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
        mode,
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

    // A zero level in XTS's own slots is a level it sent, kept as sent
    // (lead rule on book levels); only padding beyond it is dropped.
    let mut slots = depth_1502();
    slots["Bids"] = json!([level(2500.1, 40, 2), level(0.0, 0, 0)]);
    let (_, b) = decode(&mut f, "1502-json-full", slots);
    assert_eq!(b[0].buy.len(), 2);
    assert_eq!((b[0].buy[1].price, b[0].buy[1].quantity), (0.0, 0));
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

/// R107-09: RMoney clears a side it sends empty wherever its web adapter
/// looks for one (the list in `Touchline`, `BidInfo` / `AskInfo` at the
/// root or in `Touchline`, an empty object included), so the old book does
/// not stand, and the best level XTS keeps in `Touchline.BidInfo` does not
/// stand in for a side the message cleared; a 1502 naming no side makes no
/// book, so the instrument's last book stands.
#[test]
fn rmoney_clears_a_side_sent_empty_anywhere() {
    let no_book = || {
        let mut v = depth_1502();
        let o = v.as_object_mut().unwrap();
        o.remove("Bids");
        o.remove("Asks");
        v
    };
    let mut touchline_lists = no_book();
    touchline_lists["Touchline"]["Bids"] = json!([]);
    touchline_lists["Touchline"]["Asks"] = json!([]);
    let mut root_info = no_book();
    root_info["BidInfo"] = json!([]);
    root_info["AskInfo"] = json!({});
    let mut touchline_info = no_book();
    touchline_info["Touchline"]["BidInfo"] = json!([]);
    touchline_info["Touchline"]["AskInfo"] = json!([]);
    for (what, payload) in [
        ("Touchline lists", touchline_lists),
        ("root info", root_info),
        ("Touchline info", touchline_info),
    ] {
        let (_, b) = decode(&mut feed(&rmoney::CONFIG), "1502-json-full", payload);
        assert_eq!(b.len(), 1, "{}: a cleared book is a book", what);
        assert!(b[0].buy.is_empty() && b[0].sell.is_empty(), "{}", what);
    }

    let mut beside_best = no_book();
    beside_best["Touchline"]["Bids"] = json!([]);
    beside_best["Touchline"]["Asks"] = json!([level(2500.4, 95, 4)]);
    beside_best["Touchline"]["BidInfo"] = level(2500.1, 120, 3);
    let (_, b) = decode(&mut feed(&rmoney::CONFIG), "1502-json-full", beside_best);
    assert_eq!((b[0].buy.len(), b[0].sell.len()), (0, 1));

    let (t, b) = decode(&mut feed(&rmoney::CONFIG), "1502-json-full", no_book());
    assert_eq!(t.len(), 1);
    assert!(b.is_empty(), "no side named: the last book stands");
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

/// FB-05: RMoney reads a 1105 text event as LTP (1512), as its client marks
/// it and its adapter publishes it on the LTP topic alone. Jainam's client
/// marks it 1512 too, but its adapter publishes it to every held mode in
/// that mode's dict: a touchline under Jainam's per-mode fold, as the
/// template members' 1105 is read. Jainam and RMoney parse its values with
/// `float()`, so the counts it carried are doubles for that packet.
#[test]
fn the_1105_text_event_is_read_as_each_member_publishes_it() {
    let text = json!("t:1_2885,110:2503.25,111:4,112:4512345,114:2490");
    for (cfg, mode, doubles) in [
        (&rmoney::CONFIG, 1, true),
        (&jainamxts::CONFIG, 2, true),
        (&ibulls::CONFIG, 2, false),
    ] {
        let (t, _) = decode(&mut feed(cfg), "1105-json-partial", text.clone());
        assert_eq!((t[0].mode, t[0].ltp), (mode, 2503.25), "{}", cfg.id);
        assert!(!t[0].knows(QuoteField::LastTradeTime), "{}", cfg.id);
        let double = Some(NumberForm::Double);
        assert_eq!(
            t[0].numbers.form(Number::LastQuantity) == double,
            doubles,
            "{}",
            cfg.id
        );
        assert_eq!(t[0].numbers.form(Number::Volume) == double, doubles);
        assert_eq!(
            t[0].numbers.form(Number::TotalBuyQuantity),
            None,
            "not sent"
        );
    }
}

/// RMoney subscribes LTP with 1501, and its adapter publishes a 1501 at
/// mode 1 while the symbol is held at LTP only, at mode 2 once Quote or
/// Depth is held (rmoney_adapter.py _process_json_data). The other members
/// read a 1501 as Quote.
#[test]
fn rmoney_reads_a_1501_by_what_is_held() {
    let quote = json!({"MessageCode": 1501, "ExchangeSegment": 1, "ExchangeInstrumentID": 2885,
                       "Touchline": touchline()});
    for (cfg, held, mode) in [
        (&rmoney::CONFIG, FeedMode::Ltp, 1),
        (&rmoney::CONFIG, FeedMode::Quote, 2),
        (&rmoney::CONFIG, FeedMode::Depth, 2),
        (&ibulls::CONFIG, FeedMode::Ltp, 2),
    ] {
        let (t, _) = decode(&mut held_at(cfg, held), "1501-json-full", quote.clone());
        assert_eq!(t[0].mode, mode, "{} {:?}", cfg.id, held);
        assert!(
            t[0].knows(QuoteField::Volume),
            "the quote is kept: {}",
            cfg.id
        );
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

/// Audit appendix item 6: a server that ends the namespace (`41`) or the
/// Engine.IO session (`1`) with the socket still open ends the connection.
/// The feed closes the socket, whose answered close ends the session, and
/// asks the stall watchdog to reconnect should the close go unanswered.
/// A ping is still only a pong.
#[test]
fn a_server_disconnect_ends_the_connection() {
    for frame in ["41", "1"] {
        let mut f = feed(&ibulls::CONFIG);
        assert_eq!(
            f.parse(&Message::Text(frame.into())),
            vec![FeedEvent::Reply(Message::Close(None))],
            "{frame}"
        );
        assert!(f.data_stalled(), "{frame}");
        assert!(!f.data_stalled(), "{frame}: asked once");
    }
    let mut f = feed(&ibulls::CONFIG);
    f.parse(&Message::Text("2".into()));
    assert!(!f.data_stalled());
}
