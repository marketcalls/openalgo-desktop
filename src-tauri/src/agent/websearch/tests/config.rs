//! The configuration, the keys and the daily counter (the web's
//! `test_agent_websearch_settings.py`, and D-019).

use super::ctx;
use crate::agent::websearch::settings::{self, count_one, prepare, usage_from, Config, WriteError};
use crate::agent::websearch::{provider_spec, today, KEY_USAGE};
use serde_json::{json, Map, Value};

fn body(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn ciphertext(t: &crate::state::testing::TestCtx, name: &str) -> String {
    t.ctx
        .sqlite
        .conn()
        .unwrap()
        .query_row(
            "SELECT ciphertext FROM ag_secret WHERE name = ?1",
            [name],
            |r| r.get(0),
        )
        .unwrap()
}

#[tokio::test]
async fn the_configuration_describes_every_key_and_shows_none() {
    let t = ctx();
    settings::set_key(&t.ctx, "tavily", Some(&json!("tvly-test-key-0123456789"))).unwrap();
    let c = settings::config(&t.ctx).unwrap();
    assert_eq!(c["provider"], "duckduckgo");
    assert_eq!(c["perplexity_model"], "openai/gpt-5.6-luna");
    assert_eq!(
        (c["max_calls_per_turn"].clone(), c["daily_cap"].clone()),
        (json!(5), json!(200))
    );
    let day = today(t.ctx.now());
    assert_eq!(
        c["usage"],
        json!({"date": day, "count": 0, "remaining": 200})
    );
    let ids: Vec<&str> = c["providers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["duckduckgo", "tavily", "perplexity"]);
    let tavily = &c["providers"][1];
    assert_eq!(
        (tavily["has_api_key"].clone(), tavily["ready"].clone()),
        (json!(true), json!(true))
    );
    assert!(tavily["api_key_fingerprint"]
        .as_str()
        .unwrap()
        .starts_with("...6789 sha256:"));
    let perplexity = &c["providers"][2];
    assert_eq!(
        (
            perplexity["has_api_key"].clone(),
            perplexity["ready"].clone()
        ),
        (json!(false), json!(false))
    );
    assert_eq!(
        c["providers"][0]["ready"],
        json!(true),
        "keyless is always ready"
    );
    let keys: Vec<&String> = tavily.as_object().unwrap().keys().collect();
    assert_eq!(keys.len(), 10, "{:?}", keys);
    assert!(!c.to_string().contains("tvly-test-key"));
}

#[test]
fn the_provider_vocabulary_is_closed_and_case_insensitive() {
    for ok in ["duckduckgo", " Tavily ", "PERPLEXITY"] {
        assert!(provider_spec(ok).is_ok(), "{}", ok);
    }
    for bad in ["bing", "", "duckduckgo2"] {
        assert_eq!(
            provider_spec(bad).unwrap_err(),
            "provider must be one of: duckduckgo, tavily, perplexity"
        );
    }
    assert_eq!(
        prepare(&body(json!({"provider": null}))).unwrap_err(),
        "provider must be one of: duckduckgo, tavily, perplexity"
    );
}

#[test]
fn a_tunable_out_of_range_is_refused_not_clamped() {
    for (field, value, msg) in [
        (
            "max_calls_per_turn",
            json!(-1),
            "max_calls_per_turn must be between 0 and 50",
        ),
        (
            "max_calls_per_turn",
            json!(51),
            "max_calls_per_turn must be between 0 and 50",
        ),
        (
            "daily_cap",
            json!(10001),
            "daily_cap must be between 0 and 10000",
        ),
        (
            "daily_cap",
            json!("five"),
            "daily_cap must be a whole number",
        ),
        ("daily_cap", json!(true), "daily_cap must be a whole number"),
        ("daily_cap", json!(7.5), "daily_cap must be a whole number"),
    ] {
        let mut m = Map::new();
        m.insert(field.to_string(), value);
        assert_eq!(prepare(&m).unwrap_err(), msg);
    }
    for (value, stored) in [
        (json!(0), "0"),
        (json!(50), "50"),
        (json!("7"), "7"),
        (json!(7), "7"),
    ] {
        let rows = prepare(&body(json!({"max_calls_per_turn": value}))).unwrap();
        assert_eq!(
            rows,
            vec![("websearch_max_calls_per_turn", stored.to_string())]
        );
    }
}

#[test]
fn a_blank_perplexity_model_restores_the_default_and_a_model_is_a_plain_id() {
    assert_eq!(
        prepare(&body(json!({"perplexity_model": "  "}))).unwrap(),
        vec![(
            "websearch_perplexity_model",
            "openai/gpt-5.6-luna".to_string()
        )]
    );
    for bad in ["a b", "a\tb", "modèle"] {
        assert_eq!(
            prepare(&body(json!({"perplexity_model": bad}))).unwrap_err(),
            "perplexity_model must be a plain model id with no spaces"
        );
    }
    assert_eq!(
        prepare(&body(json!({"perplexity_model": "x".repeat(200)}))).unwrap_err(),
        "perplexity_model must be at most 120 characters"
    );
}

#[test]
fn an_unknown_key_is_refused_and_a_key_may_not_travel_in_the_settings() {
    assert_eq!(
        prepare(&body(json!({"providr": "tavily"}))).unwrap_err(),
        "Unknown web search setting(s): providr"
    );
    assert_eq!(
        prepare(&body(json!({"api_key": "tvly-x", "zeta": 1}))).unwrap_err(),
        "Unknown web search setting(s): api_key, zeta"
    );
    assert_eq!(prepare(&Map::new()).unwrap_err(), "Nothing to update");
}

#[tokio::test]
async fn an_update_is_all_or_nothing() {
    let t = ctx();
    let e = settings::update(
        &t.ctx,
        &body(json!({"provider": "tavily", "daily_cap": 99999})),
    )
    .unwrap_err();
    assert_eq!(
        e,
        WriteError::Invalid("daily_cap must be between 0 and 10000".into())
    );
    assert_eq!(
        Config::read(&t.ctx.sqlite.conn().unwrap()).provider,
        "duckduckgo"
    );
    settings::update(
        &t.ctx,
        &body(json!({"provider": " Tavily ", "daily_cap": 3})),
    )
    .unwrap();
    let c = Config::read(&t.ctx.sqlite.conn().unwrap());
    assert_eq!((c.provider, c.daily_cap), ("tavily", 3));
}

#[test]
fn stored_values_are_read_leniently() {
    let rows = |pairs: &[(&str, &str)]| -> std::collections::HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };
    let c = Config::from_rows(&rows(&[
        ("websearch_provider", "bing"),
        ("websearch_max_calls_per_turn", "500"),
        ("websearch_daily_cap", "lots"),
    ]));
    assert_eq!(
        (c.provider, c.max_calls_per_turn, c.daily_cap),
        ("duckduckgo", 50, 200)
    );
}

#[tokio::test]
async fn keys_are_checked_without_being_quoted() {
    let t = ctx();
    let refuse = |p: &str, v: Value| settings::set_key(&t.ctx, p, Some(&v)).unwrap_err();
    assert_eq!(
        refuse("duckduckgo", json!("k")),
        WriteError::Invalid("DuckDuckGo needs no API key".into())
    );
    for blank in [json!(""), json!("   "), json!(null), json!(42)] {
        assert_eq!(
            refuse("tavily", blank),
            WriteError::Invalid("An api_key is required for Tavily".into())
        );
    }
    assert_eq!(
        refuse("tavily", json!("x".repeat(501))),
        WriteError::Invalid("api_key must be at most 500 characters".into())
    );
    let e = refuse("perplexity", json!("pplx-secret part"));
    assert_eq!(
        e,
        WriteError::Invalid("api_key must not contain spaces or control characters".into())
    );
    assert!(!format!("{:?}", e).contains("pplx-secret"));
    assert_eq!(
        settings::set_key(&t.ctx, "tavily", None).unwrap_err(),
        WriteError::Invalid("An api_key is required for Tavily".into())
    );
}

#[tokio::test]
async fn an_unchanged_key_is_not_rewritten_and_clearing_is_idempotent() {
    let t = ctx();
    settings::set_key(&t.ctx, "tavily", Some(&json!("  tvly-same-0123456789 "))).unwrap();
    let first = ciphertext(&t, "websearch:tavily");
    settings::set_key(&t.ctx, "tavily", Some(&json!("tvly-same-0123456789"))).unwrap();
    assert_eq!(ciphertext(&t, "websearch:tavily"), first);
    assert_eq!(
        settings::stored_key(&t.ctx, "tavily").unwrap().expose(),
        "tvly-same-0123456789"
    );
    settings::clear_key(&t.ctx, "tavily").unwrap();
    settings::clear_key(&t.ctx, "tavily").unwrap();
    assert!(settings::stored_key(&t.ctx, "tavily").is_none());
}

#[test]
fn todays_count_reads_zero_for_anything_but_today() {
    let day = "2026-10-27";
    assert_eq!(
        usage_from(Some(r#"{"date": "2026-10-27", "count": 7}"#), day),
        7
    );
    assert_eq!(
        usage_from(Some(r#"{"date":"2026-10-26","count":7}"#), day),
        0
    );
    assert_eq!(usage_from(Some("not json"), day), 0);
    assert_eq!(usage_from(Some("[1, 2]"), day), 0);
    assert_eq!(
        usage_from(Some(r#"{"date": "2026-10-27", "count": -3}"#), day),
        0
    );
    assert_eq!(usage_from(None, day), 0);
}

#[tokio::test]
async fn the_counter_counts_and_resets_on_a_new_ist_day() {
    let t = ctx();
    let conn = t.ctx.sqlite.conn().unwrap();
    assert_eq!(count_one(&conn, "2026-10-27").unwrap(), 1);
    assert_eq!(count_one(&conn, "2026-10-27").unwrap(), 2);
    assert_eq!(
        count_one(&conn, "2026-10-28").unwrap(),
        1,
        "a new day starts again"
    );
    conn.execute(
        "UPDATE ag_setting SET value = 'garbage' WHERE key = ?1",
        [KEY_USAGE],
    )
    .unwrap();
    assert_eq!(
        count_one(&conn, "2026-10-28").unwrap(),
        1,
        "a broken counter starts again"
    );
    let raw: String = conn
        .query_row(
            "SELECT value FROM ag_setting WHERE key = ?1",
            [KEY_USAGE],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(usage_from(Some(&raw), "2026-10-28"), 1);
    // The IST date: 20:00 UTC is already tomorrow in India.
    let late = chrono::DateTime::parse_from_rfc3339("2026-10-27T20:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert_eq!(today(late), "2026-10-28");
}

/// D-019: the web read the counter, added one and wrote it back, so two
/// searches at once could lose a count. Eight threads, 25 searches each,
/// every one counted.
#[tokio::test]
async fn concurrent_searches_lose_no_count() {
    let t = ctx();
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let app = t.ctx.clone();
            std::thread::spawn(move || {
                for _ in 0..25 {
                    let conn = app.sqlite.conn().unwrap();
                    count_one(&conn, "2026-10-27").unwrap();
                }
            })
        })
        .collect();
    for th in threads {
        th.join().unwrap();
    }
    assert_eq!(
        settings::usage(&t.ctx.sqlite.conn().unwrap(), "2026-10-27"),
        200
    );
}
