//! The option-strike lookup (web `test/test_flow_leg_contracts.py`
//! `leg_contracts_client` cases, X-041..X-051), over a fake expiry and chain
//! source as the web's test patches them; the HTTP side of all three
//! lookups is `server/tests/flow_lookups.rs`.

use crate::flow::lookups::{
    index_symbol_rows, option_strikes_from, sorted_expiry_codes, StrikeQuery, StrikeSource,
};
use crate::services::core::Reply;
use async_trait::async_trait;
use chrono::NaiveDate;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;

/// The web fixture: three listed GOLDM expiries and a three-strike chain
/// around 163000, recording what was asked.
#[derive(Default)]
struct Fixture {
    asked: Mutex<Vec<(String, String, String, i64)>>,
    expiries_asked: Mutex<Vec<(String, String)>>,
    /// What the expiry service answers instead of the three expiries.
    expiries_reply: Option<Reply>,
    /// What the chain service answers instead of the chain.
    chain_reply: Option<Reply>,
    /// A chain answer without its `expiry_date`.
    omit_expiry_date: bool,
}

#[async_trait]
impl StrikeSource for Fixture {
    async fn expiries(&self, symbol: &str, exchange: &str) -> Reply {
        self.expiries_asked
            .lock()
            .push((symbol.into(), exchange.into()));
        if let Some(r) = &self.expiries_reply {
            return r.clone();
        }
        Reply::ok(json!({"status": "success", "data": ["28-AUG-26", "25-SEP-26", "29-OCT-26"]}))
    }

    async fn chain(
        &self,
        symbol: &str,
        exchange: &str,
        expiry: &str,
        count: i64,
    ) -> Result<Value, Reply> {
        self.asked
            .lock()
            .push((symbol.into(), exchange.into(), expiry.into(), count));
        if let Some(r) = &self.chain_reply {
            return Err(r.clone());
        }
        let leg = |strike: i64, side: &str, label: &str| json!({"symbol": format!("GOLDM{}{}{}", expiry, strike, side), "label": label});
        let mut chain = json!({
            "expiry_date": expiry,
            "atm_strike": 163000.0,
            "underlying_ltp": 163012.0,
            "underlying_symbol": "GOLDM05AUG26FUT",
            "chain": [
                {"strike": 162000.0, "ce": leg(162000, "CE", "ITM1"), "pe": leg(162000, "PE", "OTM1")},
                {"strike": 163000.0, "ce": leg(163000, "CE", "ATM"), "pe": leg(163000, "PE", "ATM")},
                {"strike": 164000.0, "ce": leg(164000, "CE", "OTM1"), "pe": leg(164000, "PE", "ITM1")},
                {"ce": leg(0, "CE", "?")}
            ],
        });
        if self.omit_expiry_date {
            if let Some(c) = chain.as_object_mut() {
                c.remove("expiry_date");
            }
        }
        Ok(chain)
    }

    fn today(&self) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, 3).unwrap_or_default()
    }
}

fn query(pairs: &[(&str, &str)]) -> StrikeQuery {
    let q: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    StrikeQuery::from_params(&q)
}

async fn ask(src: &Fixture, pairs: &[(&str, &str)]) -> (u16, Value) {
    let r = option_strikes_from(src, true, &query(pairs)).await;
    (r.status, r.body)
}

#[tokio::test]
async fn a_strike_carries_the_contract_it_resolves_to_and_its_side() {
    let f = Fixture::default();
    let (s, v) = ask(&f, &[("underlying", "GOLDM"), ("optionType", "CE")]).await;
    assert_eq!(s, 200);
    let d = &v["data"];
    assert_eq!(d["strikes"][1]["symbol"], "GOLDM28AUG26163000CE");
    assert_eq!(d["strikes"][1]["label"], "ATM");
    assert_eq!(d["atm"], 163000.0);
    assert_eq!(d["strikes"][0]["label"], "ITM1");
    assert_eq!(
        d["strikes"].as_array().map(Vec::len),
        Some(3),
        "a row without a strike is skipped"
    );
    assert_eq!(d["underlyingLtp"], 163012.0);
    assert_eq!(d["underlyingSymbol"], "GOLDM05AUG26FUT");
    let (_, pe) = ask(&f, &[("underlying", "GOLDM"), ("optionType", "pe")]).await;
    assert_eq!(pe["data"]["strikes"][0]["label"], "OTM1");
    assert!(pe["data"]["strikes"][0]["symbol"]
        .as_str()
        .is_some_and(|s| s.ends_with("PE")));
    assert_eq!(pe["data"]["optionType"], "PE");
}

#[tokio::test]
async fn expiries_resolve_as_the_executor_resolves_them() {
    let f = Fixture::default();
    let (_, v) = ask(&f, &[("underlying", "GOLDM")]).await;
    assert_eq!(
        v["data"]["expiries"],
        json!(["28AUG26", "25SEP26", "29OCT26"])
    );
    assert_eq!(v["data"]["resolved"]["current_week"], "28AUG26");
    assert_eq!(v["data"]["resolved"]["next_month"], "25SEP26");
    let (_, v) = ask(
        &f,
        &[
            ("underlying", "GOLDM"),
            ("expiry", "29OCT26"),
            ("expiryType", "current_week"),
        ],
    )
    .await;
    assert_eq!(
        v["data"]["expiry"], "29OCT26",
        "an explicit expiry wins over a type"
    );
    let (_, v) = ask(&f, &[("underlying", "GOLDM"), ("expiryType", "next_month")]).await;
    assert_eq!(v["data"]["expiry"], "25SEP26");
    let (_, v) = ask(&f, &[("underlying", "GOLDM"), ("expiry", "01JAN99")]).await;
    assert_eq!(
        v["data"]["expiry"], "28AUG26",
        "an unlisted expiry falls back to the nearest"
    );
    assert!(v["data"]["strikes"]
        .as_array()
        .is_some_and(|s| !s.is_empty()));
    let (_, v) = ask(&f, &[("underlying", "GOLDM"), ("expiry", "29-oct-26")]).await;
    assert_eq!(v["data"]["expiry"], "29OCT26");
}

#[tokio::test]
async fn the_underlying_decides_the_exchange_and_the_chain_is_structure_only() {
    let f = Fixture::default();
    let (_, gold) = ask(&f, &[("underlying", "GOLDM")]).await;
    assert_eq!(gold["data"]["exchange"], "MCX");
    let (_, nifty) = ask(&f, &[("underlying", "NIFTY")]).await;
    assert_eq!(nifty["data"]["exchange"], "NFO");
    let (_, sensex) = ask(&f, &[("underlying", "SENSEX"), ("exchange", "NFO")]).await;
    assert_eq!(sensex["data"]["exchange"], "BFO");
    let (_, embedded) = ask(&f, &[("underlying", "nifty28aug26")]).await;
    assert_eq!(embedded["data"]["underlying"], "NIFTY28AUG26");
    let asked = f.asked.lock().clone();
    assert_eq!(
        asked[0],
        ("GOLDM".into(), "MCX".into(), "28AUG26".into(), 25)
    );
    assert_eq!(
        f.expiries_asked.lock().last().cloned(),
        Some(("NIFTY".into(), "NFO".into()))
    );
}

#[tokio::test]
async fn bad_queries_are_refused() {
    let f = Fixture::default();
    assert_eq!(
        ask(&f, &[("underlying", "GOLDM"), ("optionType", "XX")])
            .await
            .0,
        400
    );
    assert_eq!(
        ask(&f, &[]).await,
        (
            400,
            json!({"status": "error", "message": "underlying is required"})
        )
    );
    let r = option_strikes_from(&f, false, &query(&[("underlying", "GOLDM")])).await;
    assert_eq!(
        (r.status, r.body),
        (
            401,
            json!({"status": "error", "message": "API key not configured. Generate one at /apikey"})
        )
    );
    assert!(f.asked.lock().is_empty());
}

#[test]
fn listed_expiries_sort_as_codes_with_unreadable_ones_last() {
    let raw = [
        json!("25-SEP-26"),
        json!("28aug26"),
        json!("28-AUG-2026"),
        json!("weekly"),
        json!(" "),
        json!(5),
        json!("31-FEB-26"),
        json!("01JAN27"),
    ];
    assert_eq!(
        sorted_expiry_codes(&raw),
        vec!["28AUG26", "25SEP26", "01JAN27", "WEEKLY", "31-FEB-26"]
    );
}

/// R-63: the side's refusal sentence.
#[tokio::test]
async fn an_unknown_side_is_refused_in_the_webs_words() {
    let f = Fixture::default();
    assert_eq!(
        ask(&f, &[("underlying", "GOLDM"), ("optionType", "XX")]).await,
        (
            400,
            json!({"status": "error", "message": "optionType must be CE or PE"})
        )
    );
    // An empty side is the default CE; whitespace is not.
    assert_eq!(
        ask(&f, &[("underlying", "GOLDM"), ("optionType", "")])
            .await
            .0,
        200
    );
    assert_eq!(
        ask(&f, &[("underlying", "GOLDM"), ("optionType", " ")])
            .await
            .0,
        400
    );
}

/// R-64: no listed expiry answers 404 naming the base symbol and the
/// options exchange; a failed expiry read passes its answer through as is.
#[tokio::test]
async fn no_listed_expiry_and_a_failed_read_answer_as_the_web() {
    let empty = Fixture {
        expiries_reply: Some(Reply::ok(json!({"status": "success", "data": []}))),
        ..Default::default()
    };
    assert_eq!(
        ask(&empty, &[("underlying", "nifty28aug26")]).await,
        (
            404,
            json!({"status": "error", "message": "No option expiries found for NIFTY on NFO"})
        )
    );
    let failed = Fixture {
        expiries_reply: Some(Reply::error(503, "Broker is busy")),
        ..Default::default()
    };
    let r = option_strikes_from(&failed, true, &query(&[("underlying", "GOLDM")])).await;
    assert_eq!(
        (r.status, r.body),
        (503, Reply::error(503, "Broker is busy").body)
    );
    assert!(
        failed.asked.lock().is_empty(),
        "no chain after a failed read"
    );
}

/// R-66: a failed chain read passes its answer and status through.
#[tokio::test]
async fn a_failed_chain_read_passes_through() {
    let f = Fixture {
        chain_reply: Some(Reply::error(
            404,
            "No strikes found for GOLDM expiring 28AUG26.",
        )),
        ..Default::default()
    };
    let r = option_strikes_from(&f, true, &query(&[("underlying", "GOLDM")])).await;
    assert_eq!(
        (r.status, r.body),
        (
            404,
            json!({"status": "error", "message": "No strikes found for GOLDM expiring 28AUG26."})
        )
    );
}

/// R-59: every underlying the options nodes offer, in the editor's order:
/// the five NSE indices on NFO, the three BSE indices on BFO, then the
/// eleven MCX commodities on MCX; one with no usable lot size is left out.
#[test]
fn index_symbols_list_every_underlying_in_the_editors_order() {
    let named = |name: &str, exchange: &str, lot: i32| {
        let mut r = crate::agent::tools::testkit::row(
            &format!("{}28OCT26FUT", name),
            exchange,
            "FUT",
            "28-OCT-26",
            0.0,
            lot,
        );
        r.name = name.into();
        r
    };
    // The master lists them out of order; the answer does not.
    let mut rows = vec![
        named("MCXBULLDEX", "MCX", 19),
        named("SENSEX50", "BFO", 8),
        named("NIFTYNXT50", "NFO", 5),
        named("BANKEX", "BFO", 7),
        named("ZINC", "MCX", 18),
        named("MIDCPNIFTY", "NFO", 4),
        named("FINNIFTY", "NFO", 3),
        named("SENSEX", "BFO", 6),
        named("BANKNIFTY", "NFO", 2),
        named("NIFTY", "NFO", 1),
    ];
    for (i, name) in [
        "GOLD",
        "GOLDM",
        "SILVER",
        "SILVERM",
        "CRUDEOIL",
        "CRUDEOILM",
        "NATURALGAS",
        "NATGASMINI",
        "COPPER",
    ]
    .into_iter()
    .enumerate()
    {
        rows.push(named(name, "MCX", 9 + i as i32));
    }
    // Not offered: an index on the wrong segment, a lot size of zero.
    rows.push(named("FINNIFTY", "BFO", 99));
    rows.push(named("BANKEX", "NFO", 99));
    let listed: Vec<(String, String, i64)> = index_symbol_rows(&rows)
        .iter()
        .map(|v| {
            assert_eq!(v["value"], v["label"]);
            (
                v["value"].as_str().unwrap_or_default().to_string(),
                v["exchange"].as_str().unwrap_or_default().to_string(),
                v["lotSize"].as_i64().unwrap_or_default(),
            )
        })
        .collect();
    let want: Vec<(String, String, i64)> = [
        ("NIFTY", "NFO"),
        ("BANKNIFTY", "NFO"),
        ("FINNIFTY", "NFO"),
        ("MIDCPNIFTY", "NFO"),
        ("NIFTYNXT50", "NFO"),
        ("SENSEX", "BFO"),
        ("BANKEX", "BFO"),
        ("SENSEX50", "BFO"),
        ("GOLD", "MCX"),
        ("GOLDM", "MCX"),
        ("SILVER", "MCX"),
        ("SILVERM", "MCX"),
        ("CRUDEOIL", "MCX"),
        ("CRUDEOILM", "MCX"),
        ("NATURALGAS", "MCX"),
        ("NATGASMINI", "MCX"),
        ("COPPER", "MCX"),
        ("ZINC", "MCX"),
        ("MCXBULLDEX", "MCX"),
    ]
    .iter()
    .enumerate()
    .map(|(i, (n, e))| (n.to_string(), e.to_string(), i as i64 + 1))
    .collect();
    assert_eq!(listed, want);
    let mut zero = vec![named("NIFTY", "NFO", 0)];
    zero.push(named("GOLD", "MCX", 0));
    assert!(index_symbol_rows(&zero).is_empty());
}

/// R-63: the expiry type is read trimmed and lower-cased.
#[tokio::test]
async fn the_expiry_type_is_trimmed_and_lower_cased() {
    let f = Fixture::default();
    let (_, v) = ask(
        &f,
        &[("underlying", "GOLDM"), ("expiryType", " Next_Month ")],
    )
    .await;
    assert_eq!(v["data"]["expiry"], "25SEP26");
    assert_eq!(
        f.asked.lock().last().map(|a| a.2.clone()),
        Some("25SEP26".to_string())
    );
}

/// R-66: each strike carries its own strike value, contract and label; the
/// answer's expiry is the chain's `expiry_date`, else the code it was asked
/// for.
#[tokio::test]
async fn strikes_carry_their_values_and_the_expiry_falls_back_to_the_code() {
    let f = Fixture::default();
    let (_, v) = ask(&f, &[("underlying", "GOLDM"), ("optionType", "PE")]).await;
    assert_eq!(
        v["data"]["strikes"],
        json!([
            {"strike": 162000.0, "symbol": "GOLDM28AUG26162000PE", "label": "OTM1"},
            {"strike": 163000.0, "symbol": "GOLDM28AUG26163000PE", "label": "ATM"},
            {"strike": 164000.0, "symbol": "GOLDM28AUG26164000PE", "label": "ITM1"}
        ])
    );
    let bare = Fixture {
        omit_expiry_date: true,
        ..Default::default()
    };
    let (s, v) = ask(
        &bare,
        &[("underlying", "GOLDM"), ("expiryType", "next_month")],
    )
    .await;
    assert_eq!((s, v["data"]["expiry"].clone()), (200, json!("25SEP26")));
}
