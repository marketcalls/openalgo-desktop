//! 5paisa XTS reads XTS answers as the XTS v2 docs describe them (web
//! 5749d4126, marketcalls/openalgo#2504), at the level of the family's own
//! functions: each reading is pinned for 5paisa XTS and, beside it, the
//! template every other member keeps. The same behaviour end to end, with
//! the web test each case ports, is `tests/it/broker_fivepaisaxts_docs/`.

use super::mapping::{cancellable, funds, member_status, trades};
use super::master_contract::parse_segment;
use super::{timeframes, XtsConfig, XtsHooks};
use crate::brokers::common::symbols::SymbolResolver;
use serde_json::{json, Value};

fn five() -> &'static XtsConfig {
    &crate::brokers::fivepaisaxts::CONFIG
}

/// Every member but 5paisa XTS: the template they keep.
fn others() -> [&'static XtsConfig; 6] {
    [
        &crate::brokers::jainamxts::CONFIG,
        &crate::brokers::compositedge::CONFIG,
        &crate::brokers::rmoney::CONFIG,
        &crate::brokers::ibulls::CONFIG,
        &crate::brokers::wisdom::CONFIG,
        &crate::brokers::iifl::CONFIG,
    ]
}

#[test]
fn only_fivepaisaxts_departs_from_the_template() {
    let h = &five().hooks;
    assert!(h.answers_as_documented);
    assert_eq!(h.history_window_days, 365);
    assert_eq!(h.funds_balance_header, Some("ALL|ALL|ALL"));
    for cfg in others() {
        let h = &cfg.hooks;
        assert!(!h.answers_as_documented, "{}", cfg.id);
        assert_eq!(h.history_window_days, 6, "{}", cfg.id);
    }
}

#[test]
fn statuses_and_cancel_all_by_member() {
    let five = &five().hooks;
    let template = XtsHooks::default();
    for status in ["Replaced", "PendingNew", "PendingReplace", "PendingCancel"] {
        assert_eq!(member_status(status, five), "open", "{status}");
        assert_eq!(
            member_status(status, &template),
            status.to_ascii_lowercase()
        );
    }
    assert_eq!(member_status("Trigger Pending", five), "trigger pending");
    let row = |status: &str| json!({"OrderStatus": status});
    // A trigger-pending order still works at the exchange, so cancel-all
    // cancels it for 5paisa XTS too, as the book shows it (a deviation
    // from web #2504, which dropped the status: `DOCUMENTED_CANCELLABLE`).
    for (status, five_cancels, template_cancels) in [
        ("New", true, true),
        ("Open", true, true),
        ("PartiallyFilled", true, true),
        ("Replaced", true, false),
        ("PendingNew", true, false),
        ("PendingReplace", true, false),
        ("PendingCancel", false, false),
        ("Trigger Pending", true, true),
        ("TriggerPending", true, true),
        ("Filled", false, false),
    ] {
        assert_eq!(cancellable(&row(status), five), five_cancels, "{status}");
        assert_eq!(
            cancellable(&row(status), &template),
            template_cancels,
            "{status}"
        );
    }
}

#[test]
fn trade_rows_are_fills_for_fivepaisaxts_only() {
    let rows = json!([{
        "ExchangeSegment": "NSECM",
        "AppOrderID": "1",
        "OrderQuantity": 100,
        "OrderAverageTradedPrice": 801.0,
        "LastTradedQuantity": "40",
        "LastTradedPrice": 800.5,
        "LastExecutionTransactTime": "17-11-2026 10:00:05",
        "OrderGeneratedDateTime": "17-11-2026 10:00:00",
    }]);
    let symbols = SymbolResolver::new();
    let t = &trades(&rows, &symbols, &five().hooks)[0];
    assert_eq!((t.quantity, t.average_price), (40, 800.5));
    assert_eq!(t.timestamp, "17-11-2026 10:00:05");
    let t = &trades(&rows, &symbols, &XtsHooks::default())[0];
    assert_eq!((t.quantity, t.average_price), (100, 801.0));
    assert_eq!(t.timestamp, "17-11-2026 10:00:00");
    // One fill field without the other, or an empty one: the order's.
    let partial = json!([{"OrderQuantity": 3, "OrderAverageTradedPrice": 9.5,
        "LastTradedQuantity": 1, "LastTradedPrice": ""}]);
    let t = &trades(&partial, &symbols, &five().hooks)[0];
    assert_eq!((t.quantity, t.average_price), (3, 9.5));
}

#[test]
fn funds_entry_as_one_object_is_read_by_fivepaisaxts_only() {
    let entry = json!({"limitHeader": "ALL|ALL|ALL", "limitObject": {"RMSSubLimits": {
        "netMarginAvailable": "12500.499", "collateral": 0, "UnrealizedMTM": 0,
        "RealizedMTM": 0, "marginUtilized": 0}}});
    let one: Value = json!({"BalanceList": entry});
    assert_eq!(
        funds(&one, &five().hooks).map(|f| f.available_cash),
        Some(12500.5)
    );
    let rmoney = &crate::brokers::rmoney::CONFIG.hooks;
    assert!(
        funds(&one, rmoney).is_none(),
        "rmoney's web reads a list only"
    );
}

/// The XTS v2 docs' F&O rows: an option (23 fields), a future (21), a
/// spread (21, InstrumentType 4), an option in the future's layout and one
/// with an unknown option type or an unreadable strike.
const FO_ROWS: &str = concat!(
    "NSEFO|48225|2|NIFTY|NIFTY2621725700PE|OPTIDX|NIFTY-OPTIDX|2604800048225|588|114.5|1801|0.05|65|1|-1|Nifty 50|2026-02-17T14:30:00|25700|4|NIFTY 17FEB2026 PE 25700|1|1|NIFTY2621725700PE\n",
    "NSEFO|49229|1|NIFTY|NIFTY26JANFUT|FUTIDX|NIFTY-FUTIDX|2602700049229|28369.8|23211.8|1801|0.1|65|1|-1|Nifty 50|2026-01-27T14:30:00|NIFTY 27JAN2026|2|1|NIFTY26JANFUT\n",
    "NSEFO|60000|4|NIFTY|NIFTYSPD|FUTIDX|NIFTY-FUTIDX|1|1|1|1|0.1|65|1|-1|Nifty 50|2026-01-27T14:30:00|NIFTY SPD|1|1|NIFTYSPD\n",
    "NSEFO|60001|2|NIFTY|NIFTYODD|OPTIDX|NIFTY-OPTIDX|1|1|1|1|0.05|65|1|-1|Nifty 50|2026-01-27T14:30:00|NIFTY ODD|1|1|NIFTYODD\n",
    "NSEFO|60002|2|NIFTY|NIFTYX|OPTIDX|NIFTY-OPTIDX|1|1|1|1|0.05|65|1|-1|Nifty 50|2026-01-27T14:30:00|25000|9|NIFTY X|1|1|NIFTYX\n",
    "NSEFO|60003|2|NIFTY|NIFTYY|OPTIDX|NIFTY-OPTIDX|1|1|1|1|0.05|65|1|-1|Nifty 50|2026-01-27T14:30:00|abc|3|NIFTY Y|1|1|NIFTYY\n",
);

#[test]
fn fo_master_rows_by_instrument_type_for_fivepaisaxts_only() {
    let rows = parse_segment("NSEFO", FO_ROWS, &five().hooks);
    let got: Vec<(&str, &str, &str, f64)> = rows
        .iter()
        .map(|r| {
            (
                r.token.as_str(),
                r.symbol.as_str(),
                r.instrument_type.as_str(),
                r.strike,
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            ("48225", "NIFTY17FEB2625700PE", "PE", 25700.0),
            ("49229", "NIFTY27JAN26FUT", "FUT", 0.0),
        ]
    );
    // The template keeps the web's pandas reading of the same rows.
    let template = parse_segment("NSEFO", FO_ROWS, &XtsHooks::default());
    assert_eq!(template.len(), 6);
    assert_eq!(
        (template[1].instrument_type.as_str(), template[1].strike),
        ("FUT", 1.0)
    );
    assert_eq!(template[2].token, "60000", "the spread is a contract there");
}

#[test]
fn timeframes_by_member() {
    let five = timeframes(five());
    assert_eq!(five.first(), Some(&("1m", "60")));
    assert!(five.iter().all(|(k, _)| *k != "1s"));
    for cfg in others() {
        assert_eq!(timeframes(cfg), super::TIMEFRAME_MAP, "{}", cfg.id);
        assert_eq!(timeframes(cfg)[0], ("1s", "1"));
    }
}
