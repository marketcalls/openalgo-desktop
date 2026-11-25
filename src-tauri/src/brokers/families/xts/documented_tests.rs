//! 5paisa XTS's departures from the template (web 5749d4126,
//! marketcalls/openalgo#2484 and #2504), at the level of the family's own
//! functions: each reading is pinned for 5paisa XTS and, beside it, the
//! template every other member keeps. The same behaviour end to end, with
//! the web test each case ports, is `tests/it/broker_fivepaisaxts_docs.rs`.

use super::mapping::{cancellable, funds, member_status, trades};
use super::master_contract::parse_segment;
use super::session::{market_data_user_id, MarketSessions, SharedSession};
use super::streaming::{FeedSource, XtsFeed};
use super::{timeframes, XtsConfig, XtsHooks};
use crate::brokers::common::streaming::BrokerFeed;
use crate::brokers::common::symbols::SymbolResolver;
use crate::security::Secret;
use base64::Engine;
use serde_json::{json, Value};
use std::sync::Arc;

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

/// A market-data token whose `userID` claim is `claim`, as XTS issues it.
fn jwt(claim: &str) -> String {
    let part = |s: String| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
    format!(
        "{}.{}.sig",
        part("{\"alg\":\"HS256\"}".into()),
        part(format!("{{\"userID\":\"{claim}\"}}"))
    )
}

#[test]
fn only_fivepaisaxts_departs_from_the_template() {
    let h = &five().hooks;
    assert!(h.answers_as_documented && h.feed_on_stored_session);
    assert_eq!(h.history_window_days, 365);
    assert_eq!(h.funds_balance_header, Some("ALL|ALL|ALL"));
    for cfg in others() {
        let h = &cfg.hooks;
        assert!(!h.answers_as_documented, "{}", cfg.id);
        assert!(!h.feed_on_stored_session, "{}", cfg.id);
        assert_eq!(h.history_window_days, 6, "{}", cfg.id);
    }
}

/// Web `test_market_data_user_id_is_the_claim_before_the_underscore`.
#[test]
fn market_data_user_id_is_the_claim_before_the_underscore() {
    assert_eq!(
        market_data_user_id(&jwt("1048131_856F2F2AF32542B762129")).as_deref(),
        Some("1048131")
    );
    assert_eq!(
        market_data_user_id(&jwt("1048131")).as_deref(),
        Some("1048131")
    );
    assert_eq!(market_data_user_id("not-a-jwt"), None);
    assert_eq!(market_data_user_id(&jwt("_APP")), None);
}

fn shared(token: &str) -> SharedSession {
    SharedSession {
        token: Secret::new(token),
        user_id: "1048131".into(),
    }
}

/// The renewed session follows the cache rules: kept when nothing cleared
/// the slot since its generation was read, never when a sign-in, resume or
/// sign-out did (the renewal belongs to the session that ended).
#[test]
fn a_renewal_is_kept_only_when_nothing_cleared_the_slot() {
    let s = MarketSessions::default();
    let generation = s.generation();
    assert!(s.renewed(shared("t1"), generation));
    assert_eq!(
        s.current().map(|x| x.token.expose().to_string()).as_deref(),
        Some("t1")
    );
    let generation = s.generation();
    s.clear();
    assert!(!s.renewed(shared("stale"), generation));
    assert!(s.current().is_none());
    assert!(s.renewed(shared("t2"), s.generation()));
    assert_eq!(s.current().map(|x| x.user_id).as_deref(), Some("1048131"));
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
    for (status, five_cancels, template_cancels) in [
        ("New", true, true),
        ("Open", true, true),
        ("PartiallyFilled", true, true),
        ("Replaced", true, false),
        ("PendingNew", true, false),
        ("PendingReplace", true, false),
        ("PendingCancel", false, false),
        ("Trigger Pending", false, true),
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

/// The address the feed opens after `prepare`.
async fn address(cfg: &'static XtsConfig, source: FeedSource) -> String {
    let mut feed = XtsFeed::new(
        cfg,
        crate::brokers::common::http::client(),
        "https://xts.invalid".into(),
        None,
        source,
    );
    assert!(feed.prepare().await.is_ok(), "{}", cfg.id);
    feed.ws_request()
        .map(|r| r.uri().to_string())
        .unwrap_or_default()
}

/// 5paisa XTS's feed connects with the session quotes renewed before the
/// one stored at sign-in, and takes the user id from the token's claim,
/// else the one stored with it; it logs in for neither.
#[tokio::test]
async fn fivepaisaxts_feed_prefers_the_renewed_session() {
    let stored = FeedSource::stored("tok", "U1");
    let url = address(five(), stored.clone()).await;
    assert!(url.contains("token=tok&userID=U1&"), "{url}");

    let slot = Arc::new(MarketSessions::default());
    assert!(slot.renewed(shared(&jwt("1048131_APP")), slot.generation()));
    let source = FeedSource {
        shared: Some(slot.clone()),
        ..stored
    };
    let url = address(five(), source.clone()).await;
    assert!(
        url.contains(&format!("token={}&", jwt("1048131_APP"))),
        "{url}"
    );
    assert!(url.contains("userID=1048131&"), "{url}");

    // A template member keeps its stored token whatever was renewed.
    let url = address(&crate::brokers::iifl::CONFIG, source).await;
    assert!(url.contains("token=tok&userID=U1&"), "{url}");
}
