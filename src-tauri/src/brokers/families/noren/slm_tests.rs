//! Web test/test_slm_trigger_pricing_2390.py for Shoonya and Flattrade
//! (marketcalls/openalgo#2495, issue #2390): both refuse SL-MKT from API
//! sessions, so an SL-M goes out as SL-LMT, and its limit is priced off
//! its own trigger, never the LTP. Off the LTP a SELL stop at 980 with the
//! LTP at 1000 went out at about 995, above its trigger. Firstock's half
//! of the web file is in `firstock::slm_tests`; the requests the members
//! send (and do not send) are in `tests/it/brokers_noren.rs`.
//!
//! The web's quote stand-in answers LTP 1000 with tick 0.05 and its master
//! contract tick 0.05, as `quote()` and `TICK` do here.

use super::mapping::{mpp_place, MppOrder, MppQuote};
use super::NorenConfig;
use crate::brokers::common::mapping::{Action, PriceType};
use crate::brokers::{flattrade, shoonya, tradesmart, zebu};
use crate::error::AppError;

const LTP: f64 = 1000.0;
const TICK: f64 = 0.05;
const OPTION: &str = "NIFTY28NOV2526000CE";
const NO_TRIGGER: &str = "A stop-loss market (SL-M) order needs a trigger price. Set a trigger price above zero and place the order again.";

/// The two members the web changed.
fn members() -> [&'static NorenConfig; 2] {
    [shoonya::config(), flattrade::config()]
}

/// The web's `_Quotes`: LTP 1000, with or without the exchange tick.
fn quote(tick: Option<f64>) -> Option<MppQuote> {
    Some(MppQuote { ltp: LTP, tick })
}

fn slm(symbol: &str, action: Action, trigger: f64) -> MppOrder<'_> {
    MppOrder {
        symbol,
        action,
        pricetype: PriceType::SlM,
        price: 0.0,
        trigger,
    }
}

/// `(prctyp, prc)` a member sends for an SL-M on SBIN, quoted at LTP 1000
/// with tick 0.05 and a master tick of 0.05.
fn sbin(cfg: &NorenConfig, action: Action, trigger: f64) -> (&'static str, String) {
    mpp_place(
        cfg.mpp,
        &slm("SBIN", action, trigger),
        quote(Some(TICK)),
        TICK,
    )
    .unwrap()
}

/// Web `test_sell_stop_limit_lands_below_the_trigger`: 980 less the 0.5%
/// slab, floored to the tick (it was about 995, off the LTP).
#[test]
fn sell_stop_limit_lands_below_the_trigger() {
    for cfg in members() {
        assert_eq!(
            sbin(cfg, Action::Sell, 980.0),
            ("SL-LMT", "975.1".into()),
            "{}",
            cfg.id
        );
    }
}

/// Web `test_buy_stop_limit_lands_above_the_trigger`: 1020 plus the 0.5%
/// slab, ceiled to the tick (it was about 1005, off the LTP).
#[test]
fn buy_stop_limit_lands_above_the_trigger() {
    for cfg in members() {
        assert_eq!(
            sbin(cfg, Action::Buy, 1020.0),
            ("SL-LMT", "1025.1".into()),
            "{}",
            cfg.id
        );
    }
}

/// Web `test_sl_m_without_a_trigger_is_refused`: refused with the web's
/// sentence, which `/api/v1` answers with 400. The web's `""`, `None` and
/// `"abc"` never reach an adapter here: the request schema refuses them
/// ("Not a valid number.", "Field may not be null.") and reads a missing
/// trigger as 0, so the adapter's cases are numbers.
#[test]
fn sl_m_without_a_trigger_is_refused() {
    for cfg in members() {
        for trigger in [0.0, -5.0, f64::NAN, f64::INFINITY] {
            let e = mpp_place(
                cfg.mpp,
                &slm("SBIN", Action::Sell, trigger),
                quote(Some(TICK)),
                TICK,
            )
            .unwrap_err();
            assert!(
                matches!(&e, AppError::Validation(m) if m == NO_TRIGGER),
                "{} {}: {:?}",
                cfg.id,
                trigger,
                e
            );
        }
    }
}

/// Web `test_limit_is_at_least_one_tick_past_the_trigger`: the 5% option
/// slab of a 0.50 trigger is 0.025, under one tick.
#[test]
fn limit_is_at_least_one_tick_past_the_trigger() {
    for cfg in members() {
        for (action, limit) in [(Action::Sell, "0.45"), (Action::Buy, "0.55")] {
            assert_eq!(
                mpp_place(cfg.mpp, &slm(OPTION, action, 0.5), quote(Some(TICK)), TICK).unwrap(),
                ("SL-LMT", limit.into()),
                "{} {:?}",
                cfg.id,
                action
            );
        }
    }
}

/// Web `test_sell_trigger_one_tick_above_zero_keeps_the_trigger`: no
/// price lies a tick below it, so the trigger is the limit (not a refusal).
#[test]
fn sell_trigger_one_tick_above_zero_keeps_the_trigger() {
    for cfg in members() {
        assert_eq!(
            mpp_place(
                cfg.mpp,
                &slm(OPTION, Action::Sell, 0.05),
                quote(Some(TICK)),
                TICK
            )
            .unwrap(),
            ("SL-LMT", "0.05".into()),
            "{}",
            cfg.id
        );
    }
}

/// Web `test_no_tick_size_anywhere_uses_the_trigger`: neither the quote
/// nor the master has a tick, so the trigger is the limit (not a refusal).
#[test]
fn no_tick_size_anywhere_uses_the_trigger() {
    for cfg in members() {
        assert_eq!(
            mpp_place(cfg.mpp, &slm("SBIN", Action::Sell, 980.0), quote(None), 0.0).unwrap(),
            ("SL-LMT", "980".into()),
            "{}",
            cfg.id
        );
    }
}

/// Web `test_no_auth_token_prices_off_the_trigger_with_the_master_tick`:
/// the web reads no quote without a session; here the quote that could
/// not be read is `None`, and the master's tick prices the limit. The
/// option case is the desktop's: the fallback before #2390 rounded a BUY
/// at 0.50 back onto its own trigger.
#[test]
fn no_quote_prices_off_the_trigger_with_the_master_tick() {
    for cfg in members() {
        let place = |o: &MppOrder<'_>| mpp_place(cfg.mpp, o, None, TICK).unwrap();
        assert_eq!(
            place(&slm("SBIN", Action::Sell, 980.0)),
            ("SL-LMT", "975.1".into()),
            "{}",
            cfg.id
        );
        assert_eq!(
            place(&slm(OPTION, Action::Buy, 0.5)),
            ("SL-LMT", "0.55".into()),
            "{}",
            cfg.id
        );
    }
}

/// The web reads the tick from the quote first and the master only
/// without one: at 1021 a BUY lands on 1026.2 with the quote's 0.1 and on
/// 1026.15 with the master's 0.05.
#[test]
fn the_quote_tick_comes_before_the_master_tick() {
    for cfg in members() {
        let o = slm("SBIN", Action::Buy, 1021.0);
        assert_eq!(
            mpp_place(cfg.mpp, &o, quote(Some(0.1)), TICK).unwrap(),
            ("SL-LMT", "1026.2".into()),
            "{}",
            cfg.id
        );
        assert_eq!(
            mpp_place(cfg.mpp, &o, quote(None), TICK).unwrap(),
            ("SL-LMT", "1026.15".into()),
            "{}",
            cfg.id
        );
    }
}

/// Web `test_market_orders_still_price_off_the_ltp`: LTP 1000 plus the
/// 0.5% slab.
#[test]
fn market_orders_still_price_off_the_ltp() {
    for cfg in members() {
        let market = MppOrder {
            pricetype: PriceType::Market,
            ..slm("SBIN", Action::Buy, 0.0)
        };
        assert_eq!(
            mpp_place(cfg.mpp, &market, quote(Some(TICK)), TICK).unwrap(),
            ("LMT", "1005".into()),
            "{}",
            cfg.id
        );
    }
}

/// The web changed Shoonya and Flattrade only. Tradesmart already prices
/// SL-M off the trigger by its own rule (the slab rounded to the nearest
/// tick, the LTP without a trigger: web `tradesmart/_apply_mpp`) and Zebu
/// sends SL-M as SL-MKT; both stay as they were.
#[test]
fn tradesmart_and_zebu_keep_their_own_stop_rules() {
    let ts = tradesmart::config().mpp;
    assert_eq!(
        mpp_place(ts, &slm(OPTION, Action::Buy, 0.5), quote(Some(TICK)), TICK).unwrap(),
        ("SL-LMT", "0.5".into())
    );
    assert_eq!(
        mpp_place(ts, &slm("SBIN", Action::Buy, 0.0), quote(Some(TICK)), TICK).unwrap(),
        ("SL-LMT", "1005".into())
    );
    assert_eq!(
        mpp_place(
            zebu::config().mpp,
            &slm("SBIN", Action::Sell, 980.0),
            quote(Some(TICK)),
            TICK
        )
        .unwrap(),
        ("SL-MKT", "0".into())
    );
}
