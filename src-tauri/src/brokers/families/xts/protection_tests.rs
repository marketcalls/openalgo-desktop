//! Price protection for iBulls MARKET and SL-M orders (web
//! test/test_ibulls_market_protection.py, marketcalls/openalgo#2466). The
//! order paths (place, modify, close-all, which LTP is read) are driven
//! against a fake XTS in `tests/it/brokers_xts.rs`.

use super::protection::{market_limit, price, stop_limit, Priced, Pricing};
use super::XtsBroker;
use crate::brokers::common::mapping::{Action, Exchange, PriceType};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::AuthToken;

const OPTION: &str = "NIFTY25NOV2524000CE";

fn tick_of(symbol: &str) -> f64 {
    match symbol {
        "SBIN" | OPTION => 0.05,
        "PENNY" => 0.01,
        _ => 0.0,
    }
}

fn exchange_of(symbol: &str) -> Exchange {
    if symbol == OPTION {
        Exchange::Nfo
    } else {
        Exchange::Nse
    }
}

fn limited(limit: f64) -> Priced {
    Priced {
        order_type: "LIMIT",
        limit,
    }
}

fn stop(limit: f64) -> Priced {
    Priced {
        order_type: "STOPLIMIT",
        limit,
    }
}

const MARKET: Priced = Priced {
    order_type: "MARKET",
    limit: 0.0,
};

/// A whole number of ticks.
fn on_tick(price: f64, tick: f64) -> bool {
    let steps = price / tick;
    (steps - steps.round()).abs() < 1e-9
}

/// Web `test_a_market_order_goes_out_as_a_protected_limit`.
#[test]
fn a_market_order_goes_out_as_a_protected_limit() {
    for (symbol, ltp, action, limit) in [
        // Equity above 500: 0.5%. 800.5 x 1.005 = 804.5025, up to 804.55.
        ("SBIN", 800.5, Action::Buy, 804.55),
        // 800.5 x 0.995 = 796.4975, down to 796.45.
        ("SBIN", 800.5, Action::Sell, 796.45),
        // Options below 10: 5%, wider than equity.
        (OPTION, 5.0, Action::Buy, 5.25),
        (OPTION, 5.0, Action::Sell, 4.75),
        // 5% of 0.05 is less than a tick: at least one tick past the LTP.
        (OPTION, 0.05, Action::Buy, 0.1),
        // A SELL cannot go below the smallest price there is.
        (OPTION, 0.05, Action::Sell, 0.05),
    ] {
        let tick = tick_of(symbol);
        let sent = market_limit(symbol, action, ltp, tick).unwrap();
        assert_eq!(sent, limited(limit), "{symbol} {ltp} {action:?}");
        assert!(on_tick(sent.limit, tick));
        if action == Action::Buy {
            assert!(sent.limit >= ltp + tick - 1e-9);
        } else if ltp > tick {
            assert!(sent.limit <= ltp - tick + 1e-9);
        }
    }
}

/// Web `test_a_market_order_without_an_ltp_goes_out_as_market` and
/// `test_a_market_order_without_a_tick_size_goes_out_as_market_without_a_quote`
/// (the quote itself is checked against the fake XTS).
#[test]
fn a_market_order_without_an_ltp_or_a_tick_size_goes_out_as_market() {
    assert_eq!(
        market_limit("SBIN", Action::Buy, 0.0, 0.05).unwrap(),
        MARKET
    );
    assert_eq!(
        market_limit("UNLISTED", Action::Buy, 100.0, 0.0).unwrap(),
        MARKET
    );
}

/// Web `test_an_sl_m_order_goes_out_as_a_stop_limit_beyond_its_trigger`.
/// The limit comes from the trigger alone: no LTP is an argument.
#[test]
fn an_sl_m_order_goes_out_as_a_stop_limit_beyond_its_trigger() {
    for (symbol, trigger, action, limit) in [
        ("SBIN", 800.0, Action::Buy, 804.0),
        ("SBIN", 800.0, Action::Sell, 796.0),
        // Equity below 100: 2%.
        ("PENNY", 50.0, Action::Buy, 51.0),
        ("PENNY", 50.0, Action::Sell, 49.0),
        (OPTION, 5.0, Action::Sell, 4.75),
        // 5% of 0.10 is less than a tick: one tick past the trigger.
        (OPTION, 0.10, Action::Buy, 0.15),
        (OPTION, 0.10, Action::Sell, 0.05),
        // A trigger off the tick grid still gets a full tick of room:
        // snapping alone would leave 0.10 (0.04 above 0.06) and 0.10 (0.04
        // below 0.14).
        (OPTION, 0.06, Action::Buy, 0.15),
        (OPTION, 0.14, Action::Sell, 0.05),
    ] {
        let tick = tick_of(symbol);
        let sent = stop_limit(symbol, exchange_of(symbol), action, trigger, tick).unwrap();
        assert_eq!(sent, stop(limit), "{symbol} {trigger} {action:?}");
        assert!(on_tick(sent.limit, tick));
        if action == Action::Buy {
            assert!(sent.limit >= trigger + tick - 1e-9);
        } else {
            assert!(sent.limit <= trigger - tick + 1e-9);
        }
    }
}

/// Web `test_an_sl_m_sell_too_low_to_protect_is_refused` and
/// `test_an_sl_m_without_a_tick_size_is_refused`.
#[test]
fn an_sl_m_that_cannot_be_priced_is_refused() {
    let low = stop_limit(OPTION, Exchange::Nfo, Action::Sell, 0.05, 0.05).unwrap_err();
    assert_eq!(low.code(), "VALIDATION_ERROR");
    assert!(
        low.client_message().contains("too low"),
        "{}",
        low.client_message()
    );
    let no_tick = stop_limit("UNLISTED", Exchange::Nse, Action::Buy, 100.0, 0.0).unwrap_err();
    assert_eq!(no_tick.code(), "VALIDATION_ERROR");
    assert!(no_tick.client_message().contains("tick size"));
    let no_trigger = stop_limit("SBIN", Exchange::Nse, Action::Buy, 0.0, 0.05).unwrap_err();
    assert!(no_trigger
        .client_message()
        .contains("trigger price above zero"));
}

/// Web `test_a_tick_size_that_cannot_be_read_is_treated_as_missing`: the
/// master's tick is a number here, so an unreadable one is NaN.
#[test]
fn a_tick_size_that_cannot_be_read_is_treated_as_missing() {
    assert_eq!(
        market_limit("SBIN", Action::Buy, 800.5, f64::NAN).unwrap(),
        MARKET
    );
    let e = stop_limit("SBIN", Exchange::Nse, Action::Buy, 800.0, f64::NAN).unwrap_err();
    assert!(e.client_message().contains("tick size"));
}

fn ibulls_at_a_closed_port() -> XtsBroker {
    XtsBroker::with_base_url(
        &crate::brokers::ibulls::CONFIG,
        SymbolResolver::new(),
        "http://127.0.0.1:9",
    )
}

fn pricing(pricetype: PriceType, price: f64, trigger: f64) -> Pricing<'static> {
    Pricing {
        symbol: "SBIN",
        exchange: Exchange::Nse,
        action: Action::Buy,
        pricetype,
        price,
        trigger,
        tick: 0.05,
    }
}

/// Web `test_limit_and_sl_orders_keep_their_own_prices`: no quote is read
/// (the broker points at a closed port, which a quote would fall back
/// from to MARKET).
#[tokio::test]
async fn limit_and_sl_orders_keep_their_own_prices() {
    let b = ibulls_at_a_closed_port();
    let auth = AuthToken::new("interactive");
    assert_eq!(
        price(&b, &auth, &pricing(PriceType::Limit, 801.5, 0.0))
            .await
            .unwrap(),
        limited(801.5)
    );
    assert_eq!(
        price(&b, &auth, &pricing(PriceType::Sl, 801.5, 800.0))
            .await
            .unwrap(),
        stop(801.5)
    );
}

/// The other XTS members send MARKET and SL-M as they are, with no quote.
#[tokio::test]
async fn members_without_price_protection_send_market_and_sl_m_unpriced() {
    let b = XtsBroker::with_base_url(
        &crate::brokers::fivepaisaxts::CONFIG,
        SymbolResolver::new(),
        "http://127.0.0.1:9",
    );
    let auth = AuthToken::new("interactive");
    assert_eq!(
        price(&b, &auth, &pricing(PriceType::Market, 0.0, 0.0))
            .await
            .unwrap(),
        MARKET
    );
    assert_eq!(
        price(&b, &auth, &pricing(PriceType::SlM, 0.0, 800.0))
            .await
            .unwrap(),
        Priced {
            order_type: "STOPMARKET",
            limit: 0.0
        }
    );
}
