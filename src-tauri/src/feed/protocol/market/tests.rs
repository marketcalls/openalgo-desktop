//! The `market_data` frame per broker: canonical keys, the broker's web
//! keys beside them (WS-05, audit C06), field presence (C03), the change
//! on every exchange where the broker's web adapter sends one (C04).

use super::market_data;
use crate::brokers::common::streaming::TimeText;
use crate::brokers::common::streaming::{
    BrokerField, BrokerValue, Number, NumberForm, QuoteExtras,
};
use crate::feed::dialect::names::BROKERS;
use crate::feed::source::{DepthBook, DepthLevel, QuoteFields};
use crate::feed::{InstrumentKey, MarketUpdate, Mode};
use serde_json::Value;

/// A full update as the bridge builds it: every canonical field known, a
/// change on every instrument.
fn update(key: InstrumentKey) -> MarketUpdate {
    let level = DepthLevel {
        price: 101.0,
        quantity: 10,
        orders: Some(2),
    };
    MarketUpdate {
        key,
        mode: Mode::Depth,
        ltp: 101.25,
        ltt: Some(1_791_000_000_000),
        timestamp: 1_791_000_000_000,
        quote: Some(QuoteFields {
            volume: Some(1000),
            last_quantity: Some(7),
            average_price: Some(100.75),
            total_buy_quantity: Some(300),
            total_sell_quantity: Some(400),
            open: Some(100.0),
            high: Some(102.0),
            low: Some(99.0),
            close: Some(100.0),
            oi: Some(5000),
            price_change: Some(1.25),
            price_change_percent: Some(1.25),
            ..Default::default()
        }),
        depth: Some(DepthBook {
            buy: vec![level; 5],
            sell: vec![level; 5],
            ..Default::default()
        }),
        exact_mode: false,
        token: None,
        broker_symbol: None,
        deep_live: None,
        deep_book: None,
    }
}

fn render(u: &MarketUpdate, mode: Mode, broker: &str) -> Value {
    let v: Value = serde_json::from_str(&market_data(u, mode, 5, broker)).unwrap();
    v["data"].clone()
}

fn data(key: InstrumentKey, mode: Mode, broker: &str) -> Value {
    render(&update(key), mode, broker)
}

/// A frame of an update of the frame's own mode (a packet of that mode).
fn data_at(key: InstrumentKey, mode: Mode, broker: &str) -> Value {
    let mut u = update(key);
    u.mode = mode;
    render(&u, mode, broker)
}

fn equity() -> InstrumentKey {
    InstrumentKey::new("SBIN", "NSE")
}

fn index() -> InstrumentKey {
    InstrumentKey::new("NIFTY", "NSE_INDEX")
}

/// The value each canonical name has in `update()`.
fn canonical_value(name: &str) -> Value {
    let q = update(equity()).quote.unwrap();
    match name {
        "last_quantity" => q.last_quantity.into(),
        "average_price" => q.average_price.into(),
        "price_change" => q.price_change.into(),
        "price_change_percent" => q.price_change_percent.into(),
        "close" => q.close.into(),
        "total_buy_quantity" => q.total_buy_quantity.into(),
        "oi" => q.oi.into(),
        other => panic!("no canonical {}", other),
    }
}

/// WS-05: every alias family carries exactly its canonical value, in Quote
/// and in Depth, on an equity as on an index, whether the canonical key
/// itself goes out or not (C04: no web adapter sends `price_change` on an
/// equity, so the change goes under the broker's own names there).
#[test]
fn each_alias_carries_its_canonical_value() {
    let families = [
        ("angel", "last_trade_quantity", "last_quantity"),
        ("upstox", "last_trade_quantity", "last_quantity"),
        ("fyers", "last_trade_quantity", "last_quantity"),
        ("samco", "last_trade_quantity", "last_quantity"),
        ("motilal", "ltq", "last_quantity"),
        ("definedge", "ltq", "last_quantity"),
        ("definedge", "change_percent", "price_change_percent"),
        ("definedge", "percent_change", "price_change_percent"),
        ("fyers", "avg_trade_price", "average_price"),
        ("fyers", "change", "price_change"),
        ("tradejini", "change", "price_change"),
        ("tradejini", "change_percent", "price_change_percent"),
        ("tradejini", "percent_change", "price_change_percent"),
        ("rupeezy", "change", "price_change"),
        ("rupeezy", "change_percent", "price_change_percent"),
        ("paytm", "change_absolute", "price_change"),
        ("fyers", "change_percent", "price_change_percent"),
        ("fyers", "percent_change", "price_change_percent"),
        ("kotak", "percent_change", "price_change_percent"),
        ("aliceblue", "change_percent", "price_change_percent"),
        ("shoonya", "percent_change", "price_change_percent"),
        ("flattrade", "percent_change", "price_change_percent"),
        ("samco", "change_percentage", "price_change_percent"),
        ("hdfcsky", "prev_close", "close"),
        ("mstock", "prev_close", "close"),
        ("kotak", "totalbuyqty", "total_buy_quantity"),
        ("aliceblue", "total_oi", "oi"),
        ("zebu", "total_traded_value", "oi"),
    ];
    for (broker, alias, canonical) in families {
        for key in [equity(), index()] {
            if key.is_index() && !canonical.starts_with("price_change") {
                continue;
            }
            for mode in [Mode::Quote, Mode::Depth] {
                let depth_only = ["totalbuyqty", "total_traded_value"].contains(&alias);
                if depth_only && mode == Mode::Quote {
                    continue;
                }
                let d = data_at(key.clone(), mode, broker);
                assert_eq!(
                    d.get(alias),
                    Some(&canonical_value(canonical)),
                    "{} {} {:?}: {}",
                    broker,
                    alias,
                    mode,
                    d
                );
            }
        }
    }
}

/// C04 (audit): the change goes with an equity under the names the
/// broker's web adapter uses for it; the canonical `price_change`, which no
/// web adapter sends on an equity, goes on an index only, and only where
/// the broker's adapter sends it there.
#[test]
fn an_instruments_change_goes_under_the_brokers_names() {
    let names = [
        ("fyers", "change"),
        ("arrow", "change"),
        ("paytm", "change_absolute"),
        ("samco", "change"),
        ("shoonya", "percent_change"),
        ("kotak", "percent_change"),
        ("pocketful", "change"),
    ];
    for (broker, name) in names {
        let d = data_at(equity(), Mode::Quote, broker);
        assert_eq!(d[name], 1.25, "{}: {}", broker, d);
        assert!(d.get("price_change").is_none(), "{}: {}", broker, d);
    }
    for broker in ["zerodha", "compositedge", "unknown"] {
        for mode in [Mode::Quote, Mode::Depth] {
            let d = data_at(equity(), mode, broker);
            for k in ["price_change", "price_change_percent", "change"] {
                assert!(d.get(k).is_none(), "{} {:?} {}: {}", broker, mode, k, d);
            }
            assert_eq!(data_at(index(), mode, broker)["price_change"], 1.25);
        }
    }
    // No change keys at all, an index's included: these adapters send none
    // (angel_adapter.py, upstox_adapter.py, dhan_adapter.py, nubra_adapter.py,
    // dhan_sandbox_adapter.py).
    for broker in ["angel", "upstox", "dhan", "nubra", "dhan_sandbox"] {
        for key in [equity(), index()] {
            for mode in [Mode::Quote, Mode::Depth] {
                let d = data_at(key.clone(), mode, broker);
                for k in ["price_change", "price_change_percent", "change"] {
                    assert!(d.get(k).is_none(), "{} {:?} {}: {}", broker, mode, k, d);
                }
            }
        }
    }
}

/// Zerodha's frames stay as recorded (no key of another broker), LTP data
/// carries no alias for a broker whose web LTP dict has none, and an alias
/// is never sent without a value.
#[test]
fn aliases_appear_only_where_the_web_sends_them() {
    let canonical: &[&str] = &[
        "symbol",
        "exchange",
        "mode",
        "ltp",
        "ltt",
        "timestamp",
        "volume",
        "last_quantity",
        "average_price",
        "total_buy_quantity",
        "total_sell_quantity",
        "open",
        "high",
        "low",
        "close",
        "price_change",
        "price_change_percent",
        "oi",
        "open_interest",
        "depth",
    ];
    for mode in [Mode::Ltp, Mode::Quote, Mode::Depth] {
        for key in [equity(), index()] {
            let d = data(key.clone(), mode, "zerodha");
            for k in d.as_object().unwrap().keys() {
                assert!(canonical.contains(&k.as_str()), "zerodha {:?} {}", mode, k);
            }
        }
    }
    for broker in ["angel", "samco", "kotak", "shoonya", "definedge"] {
        let d = data_at(equity(), Mode::Ltp, broker);
        for k in d.as_object().unwrap().keys() {
            let allowed = ["symbol", "exchange", "mode", "ltp", "ltt", "timestamp"];
            let extra_ok = ["last_trade_time", "shoonya_timestamp"];
            assert!(
                allowed.contains(&k.as_str()) || extra_ok.contains(&k.as_str()),
                "{} LTP {}: {}",
                broker,
                k,
                d
            );
        }
    }
    let mut sparse = update(equity());
    sparse.quote = Some(QuoteFields::default());
    let d = render(&sparse, Mode::Depth, "fyers");
    for k in [
        "volume",
        "close",
        "last_trade_quantity",
        "avg_trade_price",
        "change",
        "oi",
    ] {
        assert!(d.get(k).is_none(), "{}: {}", k, d);
    }
}

/// Web test/test_fyers_quote_feed.py::test_quote_carries_reference_and_existing_key_names
/// (the trade time): Fyers publishes `last_trade_time` and
/// `last_traded_time` in epoch seconds, HSM's own unit, and no `ltt`.
#[test]
fn fyers_names_the_trade_time_in_seconds() {
    for mode in [Mode::Ltp, Mode::Quote, Mode::Depth] {
        let d = data(equity(), mode, "fyers");
        assert_eq!(d["last_trade_time"], 1_791_000_000, "{:?}", mode);
        assert_eq!(d["last_traded_time"], 1_791_000_000, "{:?}", mode);
        assert!(d.get("ltt").is_none(), "{:?}", mode);
    }
}

/// The lead's per-broker switches (2026-11-09), from the web adapters and
/// the recorded Zerodha transcript: `open_interest` beside `oi` only where
/// the adapter sends both; `ltt` where it sends one (Zerodha's LTP copy of
/// a richer tick has none, `01_connect_auth_subscribe_flow.jsonl`); Angel's
/// book totals as doubles; AliceBlue's `data` without symbol, exchange or
/// mode, and with `timestamp` in Depth only.
#[test]
fn the_canonical_keys_follow_each_brokers_web_adapter() {
    for broker in [
        "zerodha",
        "sahi",
        "rupeezy",
        "arrow",
        "hdfcsky",
        "iiflcapital",
    ] {
        assert_eq!(
            data(equity(), Mode::Quote, broker)["open_interest"],
            5000,
            "{}",
            broker
        );
    }
    for broker in ["angel", "dhan", "kotak", "fyers"] {
        let d = data(equity(), Mode::Depth, broker);
        assert_eq!(d["oi"], 5000, "{}", broker);
        assert!(d.get("open_interest").is_none(), "{}: {}", broker, d);
    }
    // Upstox and Delta send OI as their adapters do: Upstox's double, as
    // a float (upstox_adapter.py `float(ff["oi"])`).
    let upstox = data(equity(), Mode::Depth, "upstox");
    assert_eq!(upstox["oi"], 5000.0, "{}", upstox);
    assert!(upstox.get("open_interest").is_none(), "{}", upstox);
    let shoonya = data(equity(), Mode::Depth, "shoonya");
    assert_eq!(shoonya["open_interest"], 5000, "Shoonya's own Depth key");

    let mut ltp = update(equity());
    ltp.mode = Mode::Ltp;
    assert_eq!(
        render(&ltp, Mode::Ltp, "zerodha")["ltt"],
        1_791_000_000_000_i64
    );
    assert!(
        data(equity(), Mode::Ltp, "zerodha").get("ltt").is_none(),
        "an LTP copy"
    );
    assert!(data(equity(), Mode::Quote, "zerodha")["ltt"].is_i64());
    for broker in ["motilal", "nubra", "shoonya", "flattrade", "deltaexchange"] {
        for mode in [Mode::Ltp, Mode::Quote, Mode::Depth] {
            assert!(
                data(equity(), mode, broker).get("ltt").is_none(),
                "{} {:?}",
                broker,
                mode
            );
        }
    }

    let angel = data(equity(), Mode::Quote, "angel");
    assert!(
        angel["total_buy_quantity"].is_f64() && angel["volume"].is_i64(),
        "{}",
        angel
    );
    assert!(data(equity(), Mode::Quote, "zerodha")["total_buy_quantity"].is_i64());

    for mode in [Mode::Ltp, Mode::Quote, Mode::Depth] {
        let d = data_at(equity(), mode, "aliceblue");
        for k in ["symbol", "exchange", "mode"] {
            assert!(d.get(k).is_none(), "{:?} {}: {}", mode, k, d);
        }
        assert_eq!(
            d.get("timestamp").is_some(),
            mode == Mode::Depth,
            "{:?}",
            mode
        );
    }
    let envelope: Value =
        serde_json::from_str(&market_data(&update(equity()), Mode::Quote, 5, "aliceblue")).unwrap();
    assert_eq!(
        (envelope["symbol"].as_str(), envelope["mode"].as_u64()),
        (Some("SBIN"), Some(2))
    );
}

/// Web openalgo#2376 (`sahi_mapping.py` `build_ltp_payload`,
/// `kotak_adapter.py`): Sahi and Kotak publish `last_trade_time` beside
/// `ltt` (the desktop in LTP frames too), and neither when the tick has no
/// trade time; Zerodha's frames have no such key.
#[test]
fn the_trade_time_alias_goes_where_ltt_goes() {
    for mode in [Mode::Ltp, Mode::Quote, Mode::Depth] {
        for broker in ["sahi", "kotak", "mstock"] {
            let d = data(equity(), mode, broker);
            assert!(d["ltt"].is_i64(), "{} {:?}: {}", broker, mode, d);
            assert_eq!(
                d.get("last_trade_time"),
                d.get("ltt"),
                "{} {:?}",
                broker,
                mode
            );
        }
        let d = data(equity(), mode, "zerodha");
        assert!(d.get("last_trade_time").is_none(), "{:?}: {}", mode, d);
        let mut no_trade = update(index());
        no_trade.ltt = None;
        let d = render(&no_trade, mode, "sahi");
        assert!(
            d.get("ltt").is_none() && d.get("last_trade_time").is_none(),
            "{}",
            d
        );
    }
}

/// `data.mode` as each broker's web adapter writes it: Zerodha's labels as
/// recorded, a number for most, none for the adapters without the key.
#[test]
fn data_mode_follows_the_broker() {
    let d = data(equity(), Mode::Depth, "zerodha");
    assert_eq!(d["mode"], "full");
    assert_eq!(data(equity(), Mode::Quote, "sahi")["mode"], "quote");
    assert_eq!(data_at(equity(), Mode::Quote, "angel")["mode"], 2);
    assert_eq!(data_at(equity(), Mode::Ltp, "dhan")["mode"], 1);
    for broker in ["upstox", "kotak", "hdfcsky", "arrow", "rupeezy", "fyers"] {
        assert!(
            data(equity(), Mode::Quote, broker).get("mode").is_none(),
            "{}",
            broker
        );
    }
    assert_eq!(data(equity(), Mode::Quote, "unknown")["mode"], 2);
}

/// C06: the optional shared fields go out under the broker's web names in
/// the modes it sends them, only when known.
#[test]
fn shared_fields_go_out_under_the_brokers_names() {
    let mut u = update(equity());
    if let Some(q) = u.quote.as_mut() {
        q.extras = QuoteExtras {
            upper_circuit: Some(111.0),
            lower_circuit: Some(91.0),
            high_52_week: Some(130.0),
            low_52_week: Some(70.0),
            bid_price: Some(101.2),
            bid_quantity: Some(40),
            ask_price: Some(101.3),
            ask_quantity: Some(50),
            previous_oi: Some(4800),
            oi_change: Some(200),
            day_close: Some(101.0),
            exchange_time_ms: Some(1_791_000_000_500),
            ..Default::default()
        };
    }
    let mut at_quote = u.clone();
    at_quote.mode = Mode::Quote;
    let angel_quote = render(&at_quote, Mode::Quote, "angel");
    assert!(
        angel_quote.get("upper_circuit").is_none(),
        "Depth only: {}",
        angel_quote
    );
    let angel = render(&u, Mode::Depth, "angel");
    assert_eq!(
        (
            angel["upper_circuit"].as_f64(),
            angel["lower_circuit"].as_f64()
        ),
        (Some(111.0), Some(91.0))
    );
    let samco = render(&u, Mode::Quote, "samco");
    assert_eq!(samco["best_bid_price"], 101.2);
    assert_eq!(samco["best_ask_quantity"], 50);
    let paytm = render(&at_quote, Mode::Quote, "paytm");
    assert_eq!(paytm["52_week_high"], 130.0);
    assert!(paytm.get("oi_change").is_none(), "Depth only");
    assert_eq!(render(&u, Mode::Depth, "paytm")["oi_change"], 200);
    let dhan = render(&u, Mode::Quote, "dhan");
    assert_eq!(dhan["day_close"], 101.0);
    let shoonya = render(&u, Mode::Ltp, "shoonya");
    assert_eq!(shoonya["shoonya_timestamp"], 1_791_000_000);
    let pocketful = render(&u, Mode::Ltp, "pocketful");
    assert_eq!(
        (pocketful["bid_price"].as_f64(), pocketful["oi"].as_i64()),
        (Some(101.2), Some(5000))
    );
    let zerodha = render(&u, Mode::Depth, "zerodha");
    assert!(zerodha.get("upper_circuit").is_none() && zerodha.get("bid_price").is_none());
    let none = render(&update(equity()), Mode::Depth, "angel");
    assert!(none.get("upper_circuit").is_none(), "not sent: not known");
}

/// C06: a broker-only field goes out under its own name from its mode up,
/// an explicit 0 included; the fixed set never names a canonical, identity
/// or mode key, nor any broker's web name.
#[test]
fn broker_fields_go_out_and_never_overwrite_a_key() {
    let mut u = update(equity());
    if let Some(q) = u.quote.as_mut() {
        q.broker_fields.set(
            BrokerField::IndicativeEquilibriumPrice,
            BrokerValue::Float(0.0),
        );
        q.broker_fields
            .set(BrokerField::CasEligible, BrokerValue::Bool(true));
    }
    let ltp = render(&u, Mode::Ltp, "upstox");
    assert_eq!(ltp["indicative_equilibrium_price"], 0.0);
    assert!(ltp.get("cas_eligible").is_none(), "Quote and up");
    assert_eq!(render(&u, Mode::Quote, "upstox")["cas_eligible"], true);

    let reserved: Vec<&str> = [
        "type",
        "symbol",
        "exchange",
        "mode",
        "broker",
        "data",
        "ltp",
        "ltt",
        "timestamp",
        "volume",
        "last_quantity",
        "average_price",
        "total_buy_quantity",
        "total_sell_quantity",
        "open",
        "high",
        "low",
        "close",
        "price_change",
        "price_change_percent",
        "oi",
        "open_interest",
        "depth",
    ]
    .to_vec();
    for f in BrokerField::ALL {
        assert!(!reserved.contains(&f.name()), "{:?}", f);
        for broker in BROKERS {
            let names = crate::feed::dialect::dialect(broker).names;
            assert!(
                names.iter().all(|n| n.key != f.name()),
                "{} {:?}",
                broker,
                f
            );
        }
    }
}

/// No broker's table names a key twice, or an identity or mode key.
#[test]
fn every_web_key_is_named_once() {
    for broker in BROKERS {
        let names = crate::feed::dialect::dialect(broker).names;
        for (i, n) in names.iter().enumerate() {
            assert!(
                ![
                    "type",
                    "symbol",
                    "exchange",
                    "mode",
                    "broker_name",
                    "data",
                    "ltp",
                    "depth"
                ]
                .contains(&n.key),
                "{} {}",
                broker,
                n.key
            );
            assert!(
                names[i + 1..].iter().all(|m| m.key != n.key),
                "{} {}",
                broker,
                n.key
            );
        }
    }
}

/// A book whose levels carry no order count sends levels without `orders`.
#[test]
fn levels_without_an_order_count_have_no_orders_key() {
    let mut u = update(equity());
    if let Some(b) = u.depth.as_mut() {
        for l in b.buy.iter_mut().chain(b.sell.iter_mut()) {
            l.orders = None;
        }
    }
    let d = render(&u, Mode::Depth, "upstox");
    let level = &d["depth"]["buy"][0];
    assert_eq!(level["price"], 101.0);
    assert!(level.get("orders").is_none(), "{}", level);
}

/// The Groww web adapter's meta keys, and the Noren and Dhan book depth.
#[test]
fn meta_keys_follow_the_frame() {
    let d = data(equity(), Mode::Quote, "groww");
    assert_eq!(d["topic"], "NSE_SBIN_QUOTE");
    assert_eq!(d["subscription_mode"], 2);
    assert_eq!(d["broker"], "groww");
    assert_eq!(data(equity(), Mode::Depth, "dhan")["depth_level"], 5);
    assert!(data_at(equity(), Mode::Quote, "dhan")
        .get("depth_level")
        .is_none());
    assert_eq!(data(equity(), Mode::Ltp, "fyers")["data_type"], "Quote");
    assert_eq!(data(equity(), Mode::Depth, "fyers")["data_type"], "Depth");
}

/// C05 (audit): a Depth frame whose update knows no OI carries none; it
/// used to send `oi` and `open_interest` as 0.
#[test]
fn a_depth_frame_never_invents_oi() {
    let mut u = update(equity());
    if let Some(q) = u.quote.as_mut() {
        q.oi = None;
    }
    let d = render(&u, Mode::Depth, "zerodha");
    assert!(
        d.get("oi").is_none() && d.get("open_interest").is_none(),
        "{}",
        d
    );
    assert_eq!(
        render(&update(equity()), Mode::Depth, "zerodha")["oi"],
        5000
    );
}

/// A book goes out level for level as the broker sent it: its fixed empty
/// slots (price 0, quantity 0, as the Zerodha golden fixtures carry five a
/// side) are neither dropped nor added to (lead 2026-11-09).
#[test]
fn a_book_goes_out_as_the_broker_sent_it_empty_slots_included() {
    let mut u = update(equity());
    let empty = DepthLevel {
        price: 0.0,
        quantity: 0,
        orders: Some(0),
    };
    if let Some(b) = u.depth.as_mut() {
        b.buy.truncate(2);
        b.buy.extend([empty; 3]);
        b.sell = vec![empty; 5];
    }
    let d = render(&u, Mode::Depth, "zerodha");
    let (buy, sell) = (&d["depth"]["buy"], &d["depth"]["sell"]);
    assert_eq!(buy.as_array().map(Vec::len), Some(5), "{}", d);
    assert_eq!(sell.as_array().map(Vec::len), Some(5), "{}", d);
    assert_eq!(
        (buy[4]["price"].as_f64(), buy[4]["quantity"].as_i64()),
        (Some(0.0), Some(0))
    );
    if let Some(b) = u.depth.as_mut() {
        b.buy.truncate(1);
        b.sell.clear();
    }
    let d = render(&u, Mode::Depth, "zerodha");
    assert_eq!(
        d["depth"]["buy"].as_array().map(Vec::len),
        Some(1),
        "never padded"
    );
    assert_eq!(
        d["depth"]["sell"].as_array().map(Vec::len),
        Some(0),
        "never padded"
    );
}

/// C03: a quote field the update does not know is left out of Quote and
/// Depth frames; a known 0 is sent.
#[test]
fn an_unknown_field_is_left_out_and_a_known_zero_sent() {
    let mut u = update(equity());
    if let Some(q) = u.quote.as_mut() {
        q.open = None;
        q.volume = Some(0);
    }
    for mode in [Mode::Quote, Mode::Depth] {
        let d = render(&u, mode, "zerodha");
        assert!(d.get("open").is_none(), "{:?}: {}", mode, d);
        assert_eq!(d["volume"], 0, "{:?}", mode);
    }
}

/// The web's Arrow, HDFC, Rupeezy, Upstox and Fyers adapters send the
/// broker's instrument token as text in every frame; others none. Paytm's
/// LTP frame carries the previous close.
#[test]
fn the_broker_token_goes_where_the_web_sends_it() {
    let mut u = update(equity());
    u.token = Some(std::sync::Arc::from("3045"));
    for broker in ["arrow", "hdfcsky", "rupeezy", "upstox", "fyers"] {
        for mode in [Mode::Ltp, Mode::Quote, Mode::Depth] {
            let d = render(&u, mode, broker);
            assert_eq!(d["token"], "3045", "{} {:?}", broker, mode);
        }
    }
    assert!(render(&u, Mode::Quote, "zerodha").get("token").is_none());
    let no_token = render(&update(equity()), Mode::Ltp, "arrow");
    assert!(no_token.get("token").is_none());
    assert_eq!(render(&u, Mode::Ltp, "paytm")["close"], 100.0);
}

/// Audit C08, lead decision 2026-11-09: a broker whose web adapter
/// publishes one dict per packet sends a lower holder the whole packet
/// (book, quote, the dict's own `mode`) under the holder's envelope mode;
/// one that publishes a dict per mode trims each frame to its holder's.
#[test]
fn a_lower_holder_gets_the_packet_as_the_brokers_web_adapter_sends_it() {
    let u = update(equity());
    let paytm = serde_json::from_str::<Value>(&market_data(&u, Mode::Ltp, 0, "paytm")).unwrap();
    assert_eq!(paytm["mode"], 1, "the holder's mode");
    let d = &paytm["data"];
    assert_eq!(d["mode"], 3, "the dict's own mode, as the web forwards it");
    assert_eq!(d["volume"], 1000);
    assert_eq!(d["depth"]["buy"].as_array().map(Vec::len), Some(5));
    let quote = render(&u, Mode::Quote, "arrow");
    assert!(quote.get("depth").is_some() && quote.get("open").is_some());

    let zerodha = render(&u, Mode::Ltp, "zerodha");
    for k in ["volume", "depth", "open"] {
        assert!(zerodha.get(k).is_none(), "{}: {}", k, zerodha);
    }
    assert!(render(&u, Mode::Quote, "zerodha").get("depth").is_none());
}

/// No key is ever sent as `null`, for any broker, mode or instrument, with
/// every field known or none: a value not known leaves its key out.
#[test]
fn no_key_is_ever_null() {
    let mut sparse = update(equity());
    sparse.quote = Some(QuoteFields::default());
    sparse.ltt = None;
    for u in [update(equity()), update(index()), sparse] {
        for broker in BROKERS {
            for mode in [Mode::Ltp, Mode::Quote, Mode::Depth] {
                let d = render(&u, mode, broker);
                let nulls: Vec<_> = d
                    .as_object()
                    .map(|o| {
                        o.iter()
                            .filter(|(_, v)| v.is_null())
                            .map(|(k, _)| k.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                assert!(nulls.is_empty(), "{} {:?}: {:?}", broker, mode, nulls);
            }
        }
    }
}

/// `data.mode` of a lower holder's frame from a one-dict broker: the dict's
/// own mode where the web adapter writes the packet's (Paytm, Dhan, the XTS
/// adapters but Jainam), the holder's where it writes each subscription's
/// (mStock, Delta Exchange, Jainam XTS, Angel, Pocketful).
#[test]
fn data_mode_is_the_packets_or_the_holders_as_each_web_adapter_writes_it() {
    let u = update(equity());
    for broker in ["paytm", "dhan", "compositedge", "rmoney"] {
        assert_eq!(render(&u, Mode::Ltp, broker)["mode"], 3, "{}", broker);
    }
    for broker in ["mstock", "deltaexchange", "jainamxts", "angel", "pocketful"] {
        let d = render(&u, Mode::Ltp, broker);
        assert_eq!(d["mode"], 1, "{}", broker);
        assert_eq!(render(&u, Mode::Quote, broker)["mode"], 2, "{}", broker);
        assert_eq!(render(&u, Mode::Depth, broker)["mode"], 3, "{}", broker);
        assert!(
            d.get("volume").is_some(),
            "still the whole packet: {}",
            broker
        );
    }
}

/// An index's order flow goes out where the broker's web adapter sends it
/// (mStock's fixed packet, under its canonical and web names alike) and
/// nowhere else; a field the packet did not carry is still left out.
#[test]
fn an_index_carries_order_flow_where_its_web_adapter_sends_it() {
    let u = update(index());
    let m = render(&u, Mode::Quote, "mstock");
    assert_eq!(m["last_quantity"], 7);
    assert_eq!(m["last_trade_quantity"], 7);
    assert_eq!(m["average_price"], 100.75);
    assert_eq!(m["total_buy_quantity"], 300);
    assert_eq!(m["total_sell_quantity"], 400);
    let z = render(&u, Mode::Quote, "zerodha");
    for k in [
        "last_quantity",
        "average_price",
        "total_buy_quantity",
        "total_sell_quantity",
    ] {
        assert!(z.get(k).is_none(), "{}: {}", k, z);
    }
    assert!(data_at(index(), Mode::Ltp, "mstock")
        .get("last_trade_quantity")
        .is_none());
    let mut sparse = update(index());
    if let Some(q) = sparse.quote.as_mut() {
        q.average_price = None;
    }
    assert!(render(&sparse, Mode::Quote, "mstock")
        .get("average_price")
        .is_none());
}

/// A key goes out as a double where the broker's web form writes one: by
/// the broker's list, for canonical keys and its own names alike (Groww's
/// LTP `volume`), or for one packet (Jainam's 1105 text event, `ltq`).
#[test]
fn doubles_go_where_the_web_writes_them() {
    let u = update(equity());
    assert!(
        render(&u, Mode::Ltp, "groww")["volume"].is_f64(),
        "Groww's LTP name"
    );
    assert!(
        render(&u, Mode::Quote, "groww")["volume"].is_f64(),
        "canonical too"
    );
    let mut text_event = update(equity());
    if let Some(q) = text_event.quote.as_mut() {
        q.numbers.set(Number::LastQuantity, NumberForm::Double);
    }
    let mut at_ltp = text_event.clone();
    at_ltp.mode = Mode::Ltp;
    assert_eq!(render(&at_ltp, Mode::Ltp, "jainamxts")["ltq"], 7.0);
    assert!(render(&at_ltp, Mode::Ltp, "jainamxts")["ltq"].is_f64());
    assert!(render(&update(equity()), Mode::Ltp, "jainamxts")["ltq"].is_i64());
    assert!(render(&text_event, Mode::Quote, "jainamxts")["last_quantity"].is_f64());
}

/// A packet can write a whole value as an integer where the broker's JSON
/// does (5paisa: 445, but 445.5), for canonical keys and the broker's names
/// alike; an unmarked value keeps its usual form.
#[test]
fn a_whole_value_goes_out_as_an_integer_where_the_packet_says() {
    let mut u = update(equity());
    if let Some(q) = u.quote.as_mut() {
        q.open = Some(445.0);
        q.high = Some(445.5);
        q.extras.bid_price = Some(101.0);
        for n in [Number::Ltp, Number::Open, Number::High, Number::BidPrice] {
            q.numbers.set(n, NumberForm::Integer);
        }
    }
    u.ltp = 445.0;
    let d = render(&u, Mode::Quote, "fivepaisa");
    assert!(d["ltp"].is_i64() && d["open"].is_i64(), "{}", d);
    assert_eq!(d["open"], 445);
    assert!(d["high"].is_f64(), "a fraction stays a double");
    assert!(d["low"].is_f64(), "unmarked: as usual");
    assert!(d["bid_price"].is_i64(), "the broker's name too: {}", d);
    assert!(render(&update(equity()), Mode::Quote, "fivepaisa")["open"].is_f64());
}

/// Fyers' 50-level frames carry the TBT book's keys; Upstox's book carries
/// the frame's timestamp; Fyers' `update_type` goes out as the adapter
/// names it.
#[test]
fn book_meta_keys_follow_the_broker() {
    let mut u = update(equity());
    if let Some(q) = u.quote.as_mut() {
        q.broker_fields
            .set(BrokerField::UpdateType, BrokerValue::Text("live"));
    }
    if let Some(b) = u.depth.as_mut() {
        b.time_ms = Some(1_791_000_000_000);
    }
    let fifty: Value = serde_json::from_str(&market_data(&u, Mode::Depth, 50, "fyers")).unwrap();
    let d = &fifty["data"];
    assert_eq!(
        (d["is_50_depth"].as_bool(), d["depth_levels"].as_u64()),
        (Some(true), Some(5)),
        "the levels the book holds"
    );
    assert_eq!(d["total_buy_qty"], 300);
    assert_eq!(d["feed_time"], 1_791_000_000, "the book packet's own time");
    assert_eq!(d["update_type"], "live");
    let five = render(&u, Mode::Depth, "fyers");
    assert!(five.get("is_50_depth").is_none() && five.get("total_buy_qty").is_none());
    let upstox = render(&u, Mode::Depth, "upstox");
    assert_eq!(upstox["depth"]["timestamp"], 1_791_000_000_000_i64);
    assert!(render(&u, Mode::Depth, "zerodha")["depth"]
        .get("timestamp")
        .is_none());
}

/// The levels a frame names are those its book was served at: Dhan's
/// `depth_level` says 5, and Fyers sends no 50-level keys, once a deep
/// subscription fell back to the main feed (audit P-10; web Dhan
/// dhan_adapter.py:930, 1042).
#[test]
fn a_fallen_back_book_names_the_levels_it_was_served_at() {
    let data = |u: &MarketUpdate, depth, broker| -> Value {
        let v: Value = serde_json::from_str(&market_data(u, Mode::Depth, depth, broker)).unwrap();
        v["data"].clone()
    };
    let levels = |u: &mut MarketUpdate, l| {
        if let Some(b) = u.depth.as_mut() {
            b.levels = l;
        }
    };
    let mut u = update(equity());
    assert_eq!(data(&u, 20, "dhan")["depth_level"], 20, "no deep socket");
    levels(&mut u, Some(20));
    assert_eq!(data(&u, 20, "dhan")["depth_level"], 20, "the deep book");
    assert_eq!(data(&u, 5, "dhan")["depth_level"], 5, "a holder at 5");
    levels(&mut u, Some(5));
    assert_eq!(data(&u, 20, "dhan")["depth_level"], 5, "fallen back");
    assert!(data(&u, 50, "fyers").get("is_50_depth").is_none());
    levels(&mut u, Some(50));
    assert_eq!(data(&u, 50, "fyers")["is_50_depth"], true);
}

/// A Depth 5 and a Depth 20 holder of one Dhan symbol while its 20 levels
/// stream from the depth socket: each gets only books at its own depth,
/// the 20 holder never the main feed's 5 levels in between (web defect:
/// the web sends every Depth publish of a symbol to every Depth client);
/// after the fallback the 20 holder gets the main feed's book, named 5.
#[test]
fn each_depth_holder_gets_only_books_at_its_own_depth() {
    use super::reaches;
    let book = |n: usize, levels: u8| DepthBook {
        buy: vec![
            DepthLevel {
                price: 101.0,
                quantity: 1,
                orders: Some(1)
            };
            n
        ],
        sell: vec![
            DepthLevel {
                price: 101.5,
                quantity: 1,
                orders: Some(1)
            };
            n
        ],
        levels: Some(levels),
        time_ms: None,
    };
    let frame = |u: &MarketUpdate, depth: u8| -> Option<Value> {
        reaches(u, Mode::Depth, depth, "dhan").then(|| {
            let v: Value =
                serde_json::from_str(&market_data(u, Mode::Depth, depth, "dhan")).unwrap();
            v["data"].clone()
        })
    };
    let side = |d: &Value| d["depth"]["buy"].as_array().map(Vec::len);
    let mut main = update(equity());
    main.exact_mode = true;
    main.depth = Some(book(5, 5));
    main.deep_live = Some(20);
    main.deep_book = Some(book(20, 20));
    let five = frame(&main, 5).expect("a 5 holder hears the main book");
    assert_eq!(
        (side(&five), five["depth_level"].as_u64()),
        (Some(5), Some(5))
    );
    assert!(frame(&main, 20).is_none(), "never the 5 levels in between");

    let mut deep = main.clone();
    deep.depth = None;
    let twenty = frame(&deep, 20).expect("the 20 holder hears its book");
    assert_eq!(
        (side(&twenty), twenty["depth_level"].as_u64()),
        (Some(20), Some(20))
    );
    assert!(frame(&deep, 5).is_none(), "the 5 holder has its own book");

    let mut tick = main.clone();
    tick.exact_mode = false;
    assert_eq!(frame(&tick, 5).as_ref().and_then(side), Some(5));
    assert_eq!(frame(&tick, 20).as_ref().and_then(side), Some(20));

    let mut fell_back = main.clone();
    fell_back.deep_live = None;
    let twenty = frame(&fell_back, 20).expect("after the fallback, the main book");
    assert_eq!(
        (side(&twenty), twenty["depth_level"].as_u64()),
        (Some(5), Some(5))
    );
}

/// Fyers' `token` is the exchange's token (the fytoken less its first ten
/// digits) on HSM frames and the ticker on TBT 50-level frames, and none on
/// an index (the web's `""` placeholder, declared by the Fyers lane).
#[test]
fn fyers_names_its_frames_by_the_token_the_web_uses() {
    let mut u = update(equity());
    u.token = Some(std::sync::Arc::from("10100000003045"));
    u.broker_symbol = Some(std::sync::Arc::from("NSE:SBIN-EQ"));
    for mode in [Mode::Ltp, Mode::Quote, Mode::Depth] {
        assert_eq!(render(&u, mode, "fyers")["token"], "3045", "{:?}", mode);
    }
    let fifty: Value = serde_json::from_str(&market_data(&u, Mode::Depth, 50, "fyers")).unwrap();
    assert_eq!(fifty["data"]["token"], "NSE:SBIN-EQ");
    assert_eq!(render(&u, Mode::Quote, "upstox")["token"], "10100000003045");
    let mut i = update(index());
    i.token = u.token.clone();
    assert!(render(&i, Mode::Quote, "fyers").get("token").is_none());
}

/// Tradejini's Quote and Depth frames carry `last_trade_time` as that
/// text; its LTP frame does not (web `_add_reference_keys`, Quote and Depth).
#[test]
fn tradejini_names_the_trade_time_as_text() {
    let mut u = update(equity());
    if let Some(q) = u.quote.as_mut() {
        q.trade_time = Some(1_791_000_000_000);
    }
    for mode in [Mode::Quote, Mode::Depth] {
        let d = render(&u, mode, "tradejini");
        assert_eq!(d["last_trade_time"], "2026-10-03 09:30:00", "{:?}", mode);
    }
    let mut ltp = u.clone();
    ltp.mode = Mode::Ltp;
    assert!(render(&ltp, Mode::Ltp, "tradejini")
        .get("last_trade_time")
        .is_none());
}

/// A trade time as Tradejini's web adapter writes it, in IST seconds, and
/// only when the broker sent one (`ltt` falls back to the receive time).
#[test]
fn a_trade_time_goes_out_as_ist_text() {
    // 1791000000 s is 2026-10-03 04:00:00 UTC, 09:30:00 IST.
    assert_eq!(
        super::time_text(1_791_000_000_250, TimeText::DateTime),
        Some(Value::from("2026-10-03 09:30:00"))
    );
    let mut u = update(equity());
    if let Some(q) = u.quote.as_mut() {
        q.trade_time = Some(1_791_000_000_000);
    }
    let fr_q = u.quote.clone().unwrap();
    let frame = super::Frame {
        update: &u,
        q: &fr_q,
        mode: Mode::Quote,
        content: Mode::Quote,
        depth: 5,
        broker: "tradejini",
    };
    let v = super::value(crate::feed::dialect::Source::TradeTimeText, &frame);
    assert_eq!(v, Some(Value::from("2026-10-03 09:30:00")));
    let none = update(equity());
    let q = none.quote.clone().unwrap();
    let frame = super::Frame {
        q: &q,
        update: &none,
        ..frame
    };
    assert_eq!(
        super::value(crate::feed::dialect::Source::TradeTimeText, &frame),
        None
    );
}

/// A trade time as the time of day (the Noren family's `last_trade_time`),
/// and in the text form a packet marks (Definedge passes `HH:MM:SS` or
/// epoch seconds through), only when the broker sent a trade time.
#[test]
fn a_trade_time_goes_out_in_the_text_form_of_its_packet() {
    use crate::feed::dialect::Source;
    let mut u = update(equity());
    if let Some(q) = u.quote.as_mut() {
        q.trade_time = Some(1_791_000_000_000);
    }
    let q = u.quote.clone().unwrap_or_default();
    let frame = super::Frame {
        update: &u,
        q: &q,
        mode: Mode::Quote,
        content: Mode::Quote,
        depth: 5,
        broker: "shoonya",
    };
    let at = |source, frame: &super::Frame<'_>| super::value(source, frame);
    assert_eq!(
        at(Source::TradeTimeOfDay, &frame),
        Some(Value::from("09:30:00"))
    );
    let mut marked = q.clone();
    marked.time_text = Some(TimeText::EpochSeconds);
    let f = super::Frame {
        q: &marked,
        ..frame
    };
    assert_eq!(
        at(Source::TradeTimeText, &f),
        Some(Value::from("1791000000"))
    );
    assert_eq!(
        at(Source::TradeTimeOfDay, &f),
        Some(Value::from("1791000000"))
    );
    marked.time_text = Some(TimeText::TimeOfDay);
    let f = super::Frame {
        q: &marked,
        ..frame
    };
    assert_eq!(at(Source::TradeTimeText, &f), Some(Value::from("09:30:00")));
    let mut unsent = q.clone();
    unsent.trade_time = None;
    let f = super::Frame {
        q: &unsent,
        ..frame
    };
    assert_eq!(at(Source::TradeTimeOfDay, &f), None);
}

/// Definedge's `last_trade_time` (Quote up) and `ltt_time` (Depth) go only
/// with a packet that marked its trade time's text form (the depth feed's
/// `ltt` clock); a packet whose trade time is the feed time sends neither.
#[test]
fn definedge_names_a_trade_time_only_from_its_clock() {
    let mut u = update(equity());
    if let Some(q) = u.quote.as_mut() {
        q.trade_time = Some(1_791_000_000_000);
    }
    for mode in [Mode::Quote, Mode::Depth] {
        let d = render(&u, mode, "definedge");
        assert!(
            d.get("last_trade_time").is_none() && d.get("ltt_time").is_none(),
            "{}",
            d
        );
    }
    if let Some(q) = u.quote.as_mut() {
        q.time_text = Some(TimeText::TimeOfDay);
    }
    let quote = render(&u, Mode::Quote, "definedge");
    assert_eq!(quote["last_trade_time"], "09:30:00");
    assert!(quote.get("ltt_time").is_none(), "Depth only");
    let depth = render(&u, Mode::Depth, "definedge");
    assert_eq!(depth["last_trade_time"], "09:30:00");
    assert_eq!(depth["ltt_time"], "09:30:00");
    assert!(data_at(equity(), Mode::Ltp, "definedge")
        .get("last_trade_time")
        .is_none());
}

/// `index_change` (lead, 2026-11-09): Firstock, AliceBlue, Samco,
/// Tradejini, Kotak and Pocketful send no canonical change on an index;
/// the broker's own names still go where it sends them (AliceBlue's
/// `change_percent`); Zerodha's index frames carry the canonical ones.
#[test]
fn the_canonical_index_change_follows_the_broker() {
    for broker in [
        "firstock",
        "aliceblue",
        "samco",
        "tradejini",
        "kotak",
        "pocketful",
    ] {
        let d = data_at(index(), Mode::Quote, broker);
        assert!(d.get("price_change").is_none(), "{}: {}", broker, d);
        assert!(d.get("price_change_percent").is_none(), "{}", broker);
    }
    assert_eq!(
        data_at(index(), Mode::Quote, "aliceblue")["change_percent"],
        1.25
    );
    assert_eq!(
        data_at(index(), Mode::Quote, "kotak")["percent_change"],
        1.25
    );
    assert_eq!(
        data_at(index(), Mode::Quote, "zerodha")["price_change"],
        1.25
    );
}
