//! Where each broker's web adapter shapes the feed differently: the names
//! of the contested quote fields (WS-05) and the depth a Depth subscribe
//! is acknowledged with.
//!
//! The desktop renders one schema for every broker: the names the recorded
//! Zerodha transcript carries (`last_quantity`, `average_price`,
//! `price_change`, `price_change_percent`). The web forwards each broker
//! adapter's own dict, and several adapters name these fields differently,
//! so an SDK script written against the web with, say, Angel reads
//! `data["last_trade_quantity"]`. For those brokers the desktop adds the web
//! adapter's name next to the canonical one, with the same value: no client
//! loses a key and none meets a KeyError. Zerodha gets no alias, so its
//! frames stay exactly as recorded.
//!
//! Deliberate deviation from the web: the web sends only the adapter's
//! names (and some adapters switch names between Quote and Depth); the
//! desktop sends the canonical names always and the aliases in both modes.
//! An alias exists only where its canonical key does: the change fields
//! are sent for indices only, as in the recorded transcript, because most
//! desktop feeds do not compute a change for equities. Table read from the
//! web adapters (`broker/<name>/streaming/*_adapter.py`) at `76fb805`.

use super::source::{Mode, DEFAULT_DEPTH};
use serde::Serialize;

/// How a broker's web adapter fills `actual_depth`, which the web's proxy
/// puts in a subscribe acknowledgment (`websocket_proxy/server.py`:
/// `response.get("actual_depth", depth_level)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DepthAck {
    /// No `actual_depth` (Zerodha, Fyers, Upstox, Kotak and the others in
    /// [`depth_ack`]): the depth asked for, in every mode, whatever is
    /// streamed.
    Echo,
    /// The level it falls back to in Depth mode, the depth asked for
    /// otherwise (Angel, Dhan, Definedge, the XTS family, Motilal, Nubra,
    /// Pocketful, Samco, Tradejini; the default for a broker not listed).
    Fallback,
    /// The level it falls back to in Depth mode, `null` otherwise: Arrow
    /// and both HDFC adapters send `actual_depth=None` outside Depth mode.
    FallbackElseNull,
    /// Always 5, in every mode (IIFL Capital).
    Five,
}

fn depth_ack(broker: &str) -> DepthAck {
    match broker {
        "zerodha" | "fyers" | "upstox" | "kotak" | "groww" | "aliceblue" | "shoonya"
        | "flattrade" | "zebu" | "tradesmart" | "firstock" | "fivepaisa" | "indmoney"
        | "mstock" | "paytm" | "deltaexchange" => DepthAck::Echo,
        "arrow" | "hdfcsecurities" | "hdfcsky" => DepthAck::FallbackElseNull,
        "iiflcapital" => DepthAck::Five,
        _ => DepthAck::Fallback,
    }
}

/// A subscribe's book depth: the levels streamed, and the depth its
/// acknowledgment names (`None`: JSON `null`), exactly as the broker's web
/// adapter answers (maintainer decision 2026-10-16: like the web). `None`
/// only when the broker streams no level at all.
///
/// Depth mode streams the largest level the broker serves up to the one
/// asked for, else its smallest (the web's `get_fallback_depth_level`
/// defaults to 5); other modes need no book. Because an echoing broker
/// names the depth asked for (20 on Zerodha, which streams 5), a client
/// reads the depth it gets from the frames, not from the acknowledgment.
pub(crate) fn subscribe_depth(
    broker: &str,
    mode: Mode,
    requested: i64,
    supported: &[u8],
) -> Option<(u8, Option<i64>)> {
    let rule = depth_ack(broker);
    if mode != Mode::Depth {
        let named = match rule {
            DepthAck::Echo | DepthAck::Fallback => Some(requested),
            DepthAck::FallbackElseNull => None,
            DepthAck::Five => Some(5),
        };
        return Some((DEFAULT_DEPTH, named));
    }
    let streamed = supported
        .iter()
        .copied()
        .filter(|d| i64::from(*d) <= requested)
        .max()
        .or_else(|| supported.iter().copied().min())?;
    let named = match rule {
        DepthAck::Echo => requested,
        DepthAck::Five => 5,
        DepthAck::Fallback | DepthAck::FallbackElseNull => i64::from(streamed),
    };
    Some((streamed, Some(named)))
}

/// The alias keys a broker's Quote and Depth data carry, each the same
/// value as its canonical key. Serialized after the canonical keys.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub(crate) struct Aliases {
    /// `last_quantity` (Angel, Fyers, Upstox, 5paisa, mstock, Paytm,
    /// Pocketful, Samco).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_trade_quantity: Option<i64>,
    /// `last_quantity` (Definedge, Motilal).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ltq: Option<i64>,
    /// `average_price` (Fyers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_trade_price: Option<f64>,
    /// `price_change` (Fyers, Arrow, both HDFC, Tradejini, Samco).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<f64>,
    /// `price_change` (Paytm).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_absolute: Option<f64>,
    /// `price_change_percent` (Fyers, AliceBlue, Arrow, both HDFC,
    /// Definedge, Paytm, Tradejini).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_percent: Option<f64>,
    /// `price_change_percent` (Shoonya, Flattrade, Zebu, TradeSmart).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent_change: Option<f64>,
    /// `price_change_percent` (Samco).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_percentage: Option<f64>,
}

/// Which web name a broker uses for each contested field (`None`: the
/// canonical name, or the web adapter does not send the field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Dialect {
    last_quantity: Option<LastQuantity>,
    avg_trade_price: bool,
    change: Option<Change>,
    change_percent: Option<ChangePercent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LastQuantity {
    LastTradeQuantity,
    Ltq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    Change,
    ChangeAbsolute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChangePercent {
    ChangePercent,
    PercentChange,
    ChangePercentage,
}

const CANONICAL: Dialect = Dialect {
    last_quantity: None,
    avg_trade_price: false,
    change: None,
    change_percent: None,
};

fn dialect(broker: &str) -> Dialect {
    use Change as C;
    use ChangePercent as P;
    use LastQuantity as Q;
    match broker {
        "fyers" => Dialect {
            last_quantity: Some(Q::LastTradeQuantity),
            avg_trade_price: true,
            change: Some(C::Change),
            change_percent: Some(P::ChangePercent),
        },
        "angel" | "upstox" | "fivepaisa" | "mstock" | "pocketful" => Dialect {
            last_quantity: Some(Q::LastTradeQuantity),
            ..CANONICAL
        },
        "paytm" => Dialect {
            last_quantity: Some(Q::LastTradeQuantity),
            change: Some(C::ChangeAbsolute),
            change_percent: Some(P::ChangePercent),
            ..CANONICAL
        },
        "samco" => Dialect {
            last_quantity: Some(Q::LastTradeQuantity),
            change: Some(C::Change),
            change_percent: Some(P::ChangePercentage),
            ..CANONICAL
        },
        "definedge" => Dialect {
            last_quantity: Some(Q::Ltq),
            change_percent: Some(P::ChangePercent),
            ..CANONICAL
        },
        "motilal" => Dialect {
            last_quantity: Some(Q::Ltq),
            ..CANONICAL
        },
        "arrow" | "hdfcsecurities" | "hdfcsky" | "tradejini" => Dialect {
            change: Some(C::Change),
            change_percent: Some(P::ChangePercent),
            ..CANONICAL
        },
        "aliceblue" => Dialect {
            change_percent: Some(P::ChangePercent),
            ..CANONICAL
        },
        "shoonya" | "flattrade" | "zebu" | "tradesmart" => Dialect {
            change_percent: Some(P::PercentChange),
            ..CANONICAL
        },
        _ => CANONICAL,
    }
}

/// The canonical values a Quote or Depth frame carries.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Canonical {
    pub last_quantity: Option<i64>,
    pub average_price: Option<f64>,
    pub price_change: Option<f64>,
    pub price_change_percent: Option<f64>,
}

/// The alias keys for `broker`, each carrying its canonical value.
pub(crate) fn aliases(broker: &str, c: Canonical) -> Aliases {
    let d = dialect(broker);
    let mut a = Aliases::default();
    match d.last_quantity {
        Some(LastQuantity::LastTradeQuantity) => a.last_trade_quantity = c.last_quantity,
        Some(LastQuantity::Ltq) => a.ltq = c.last_quantity,
        None => {}
    }
    if d.avg_trade_price {
        a.avg_trade_price = c.average_price;
    }
    match d.change {
        Some(Change::Change) => a.change = c.price_change,
        Some(Change::ChangeAbsolute) => a.change_absolute = c.price_change,
        None => {}
    }
    match d.change_percent {
        Some(ChangePercent::ChangePercent) => a.change_percent = c.price_change_percent,
        Some(ChangePercent::PercentChange) => a.percent_change = c.price_change_percent,
        Some(ChangePercent::ChangePercentage) => a.change_percentage = c.price_change_percent,
        None => {}
    }
    a
}

#[cfg(test)]
mod tests {
    use super::subscribe_depth;
    use crate::feed::protocol::market_data;
    use crate::feed::source::{DepthBook, DepthLevel, QuoteFields};
    use crate::feed::{InstrumentKey, MarketUpdate, Mode};
    use serde_json::Value;

    const ALIAS_KEYS: &[&str] = &[
        "last_trade_quantity",
        "ltq",
        "avg_trade_price",
        "change",
        "change_absolute",
        "change_percent",
        "percent_change",
        "change_percentage",
    ];

    /// A full update as the bridge builds it: the change fields on an
    /// index only (`bridge::quote_fields`).
    fn update(key: InstrumentKey) -> MarketUpdate {
        let change = key.is_index().then_some(1.25);
        let level = DepthLevel {
            price: 101.0,
            quantity: 10,
            orders: 2,
        };
        MarketUpdate {
            key,
            mode: Mode::Depth,
            ltp: 101.25,
            ltt: Some(1_791_000_000_000),
            timestamp: 1_791_000_000_000,
            quote: Some(QuoteFields {
                volume: 1000,
                last_quantity: 7,
                average_price: 100.75,
                total_buy_quantity: 300,
                total_sell_quantity: 400,
                open: Some(100.0),
                high: Some(102.0),
                low: Some(99.0),
                close: Some(100.0),
                oi: None,
                price_change: change,
                price_change_percent: change,
            }),
            depth: Some(DepthBook {
                buy: vec![level; 5],
                sell: vec![level; 5],
            }),
            exact_mode: false,
        }
    }

    fn data(key: InstrumentKey, mode: Mode, broker: &str) -> Value {
        let frame = market_data(&update(key), mode, 5, broker);
        let v: Value = serde_json::from_str(&frame).unwrap();
        v["data"].clone()
    }

    fn equity() -> InstrumentKey {
        InstrumentKey::new("SBIN", "NSE")
    }

    fn index() -> InstrumentKey {
        InstrumentKey::new("NIFTY", "NSE_INDEX")
    }

    /// WS-05: every alias family carries exactly its canonical key's value,
    /// in Quote and in Depth. Last quantity and average price are on an
    /// equity; the change fields on an index, the only place the canonical
    /// ones are sent.
    #[test]
    fn each_alias_carries_its_canonical_value() {
        let families = [
            ("angel", "last_trade_quantity", "last_quantity"),
            ("upstox", "last_trade_quantity", "last_quantity"),
            ("fyers", "last_trade_quantity", "last_quantity"),
            ("samco", "last_trade_quantity", "last_quantity"),
            ("motilal", "ltq", "last_quantity"),
            ("definedge", "ltq", "last_quantity"),
            ("fyers", "avg_trade_price", "average_price"),
            ("fyers", "change", "price_change"),
            ("tradejini", "change", "price_change"),
            ("paytm", "change_absolute", "price_change"),
            ("fyers", "change_percent", "price_change_percent"),
            ("aliceblue", "change_percent", "price_change_percent"),
            ("shoonya", "percent_change", "price_change_percent"),
            ("flattrade", "percent_change", "price_change_percent"),
            ("samco", "change_percentage", "price_change_percent"),
        ];
        for (broker, alias, canonical) in families {
            let key = if canonical.starts_with("price_change") {
                index()
            } else {
                equity()
            };
            for mode in [Mode::Quote, Mode::Depth] {
                let d = data(key.clone(), mode, broker);
                assert!(d.get(canonical).is_some(), "{} {:?}: {}", broker, mode, d);
                assert_eq!(
                    d.get(alias),
                    d.get(canonical),
                    "{} {} {:?}: {}",
                    broker,
                    alias,
                    mode,
                    d
                );
            }
        }
    }

    /// Zerodha's frames stay exactly as recorded, LTP data carries no alias
    /// for anyone, and an alias is never sent without its canonical key
    /// (equities carry no change fields).
    #[test]
    fn aliases_appear_only_beside_their_canonical_key() {
        for mode in [Mode::Ltp, Mode::Quote, Mode::Depth] {
            for key in [equity(), index()] {
                let d = data(key.clone(), mode, "zerodha");
                for alias in ALIAS_KEYS {
                    assert!(
                        d.get(alias).is_none(),
                        "zerodha {:?} {}: {}",
                        mode,
                        alias,
                        d
                    );
                }
            }
        }
        for broker in ["angel", "fyers", "samco", "paytm", "shoonya"] {
            let d = data(equity(), Mode::Ltp, broker);
            for alias in ALIAS_KEYS {
                assert!(d.get(alias).is_none(), "{} LTP {}: {}", broker, alias, d);
            }
        }
        let d = data(equity(), Mode::Quote, "fyers");
        assert!(d.get("price_change").is_none());
        for alias in ["change", "change_percent"] {
            assert!(d.get(alias).is_none(), "fyers equity {}: {}", alias, d);
        }
    }

    /// Maintainer decision 2026-10-16, like the web: Zerodha, whose web
    /// adapter has no `actual_depth`, names the depth asked for in every mode
    /// and streams what it serves.
    #[test]
    fn an_echoing_broker_names_the_depth_asked_for() {
        let depth = |requested, supported: &[u8]| {
            subscribe_depth("zerodha", Mode::Depth, requested, supported)
        };
        assert_eq!(depth(20, &[5]), Some((5, Some(20))));
        assert_eq!(depth(50, &[5]), Some((5, Some(50))));
        assert_eq!(depth(5, &[5]), Some((5, Some(5))));
        assert_eq!(depth(3, &[5]), Some((5, Some(3))));
        assert_eq!(
            subscribe_depth("fyers", Mode::Depth, 50, &[5, 50]),
            Some((50, Some(50)))
        );
        assert_eq!(
            subscribe_depth("zerodha", Mode::Ltp, 20, &[5]),
            Some((5, Some(20)))
        );
    }

    /// Angel's and Dhan's web adapters name the level they fall back to
    /// (the highest served up to the one asked for, else 5).
    #[test]
    fn a_falling_back_broker_names_the_level_streamed() {
        assert_eq!(
            subscribe_depth("angel", Mode::Depth, 20, &[5]),
            Some((5, Some(5)))
        );
        assert_eq!(
            subscribe_depth("dhan", Mode::Depth, 50, &[5, 20]),
            Some((20, Some(20)))
        );
        assert_eq!(
            subscribe_depth("angel", Mode::Depth, 3, &[5]),
            Some((5, Some(5)))
        );
        assert_eq!(
            subscribe_depth("angel", Mode::Quote, 20, &[5]),
            Some((5, Some(20)))
        );
        assert_eq!(subscribe_depth("angel", Mode::Depth, 5, &[]), None);
    }

    /// Arrow and both HDFC adapters send `actual_depth=None` outside Depth
    /// mode, so the web answers `"depth": null` there; IIFL Capital always
    /// names 5.
    #[test]
    fn arrow_and_hdfc_name_no_depth_outside_depth_mode_and_iifl_capital_five() {
        for broker in ["arrow", "hdfcsecurities", "hdfcsky"] {
            for mode in [Mode::Ltp, Mode::Quote] {
                assert_eq!(subscribe_depth(broker, mode, 5, &[5]), Some((5, None)));
            }
            assert_eq!(
                subscribe_depth(broker, Mode::Depth, 20, &[5]),
                Some((5, Some(5)))
            );
        }
        for mode in [Mode::Ltp, Mode::Depth] {
            assert_eq!(
                subscribe_depth("iiflcapital", mode, 20, &[5]),
                Some((5, Some(5)))
            );
        }
    }
}
