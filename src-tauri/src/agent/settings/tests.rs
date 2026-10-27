//! The settings contract, against the fixture recorded from the web
//! (`scripts/agent/capture_settings.py`): every key, default and bound, what
//! each sample write stores or the exact sentence that refuses it, and how
//! a stored row reads.

use super::parse::{default_of, parse, render, serialise};
use super::schema::{field_of, FIELDS, VOICE_KEYS};
use super::*;
use serde_json::{json, Value};
use std::path::Path;

const FIXTURE: &str = include_str!("../../../tests/fixtures/agent/settings.json");

fn fixture() -> Value {
    serde_json::from_str(FIXTURE).unwrap()
}

fn data_dir() -> &'static Path {
    Path::new("/data")
}

#[test]
fn every_key_default_and_bound_is_the_webs() {
    let f = fixture();
    let keys = f["keys"].as_array().unwrap();
    assert_eq!(keys.len(), 26);
    assert_eq!(FIELDS.len(), keys.len());
    for (ours, web) in FIELDS.iter().zip(keys) {
        assert_eq!(ours.key, web["key"], "order");
        let kind = format!("{:?}", ours.kind).to_lowercase();
        assert_eq!(kind, web["kind"].as_str().unwrap(), "{}", ours.key);
        let default = render(&default_of(ours, data_dir()));
        if ours.key == schema::KILL_SWITCH_FILE {
            let file = data_dir().join("agent_kill_switch");
            assert_eq!(default, json!(file.to_string_lossy()));
        } else {
            assert_eq!(default, web["default"], "{}", ours.key);
        }
        assert_eq!(
            ours.min.map(|m| m.to_string()),
            web["min"]
                .as_i64()
                .map(|m| m.to_string())
                .or_else(|| web["min"].as_str().map(str::to_string)),
            "{}",
            ours.key
        );
        assert_eq!(
            ours.max.map(|m| m.to_string()),
            web["max"]
                .as_i64()
                .map(|m| m.to_string())
                .or_else(|| web["max"].as_str().map(str::to_string)),
            "{}",
            ours.key
        );
        if let Some(choices) = web["choices"].as_array() {
            let mut ours: Vec<&str> = ours.choices.to_vec();
            ours.sort_unstable();
            let web: Vec<&str> = choices.iter().map(|c| c.as_str().unwrap()).collect();
            assert_eq!(ours, web);
        }
    }
    let voice: Vec<&str> = f["voice_keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(VOICE_KEYS, voice.as_slice());
}

#[test]
fn a_write_stores_what_the_web_stores_or_refuses_in_its_words() {
    let f = fixture();
    for case in f["strict"].as_array().unwrap() {
        let key = case["key"].as_str().unwrap();
        let field = field_of(key).unwrap();
        let default = default_of(field, data_dir());
        let got = parse(field, &case["raw"], &default, true).map(|v| serialise(&v));
        match (case.get("stored"), case.get("error")) {
            (Some(stored), _) => assert_eq!(
                got.as_deref().ok(),
                stored.as_str(),
                "{} {}",
                key,
                case["raw"]
            ),
            (_, Some(error)) => assert_eq!(
                got.err().as_deref(),
                error.as_str(),
                "{} {}",
                key,
                case["raw"]
            ),
            _ => panic!("bad case"),
        }
    }
}

#[test]
fn a_stored_row_reads_as_the_web_reads_it() {
    let f = fixture();
    for case in f["lenient"].as_array().unwrap() {
        let key = case["key"].as_str().unwrap();
        let field = field_of(key).unwrap();
        let default = default_of(field, data_dir());
        let got = parse(field, &case["stored"], &default, false).unwrap();
        assert_eq!(render(&got), case["reads"], "{} {}", key, case["stored"]);
    }
}

fn ctx() -> crate::state::testing::TestCtx {
    crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now())
}

fn body(v: Value) -> serde_json::Map<String, Value> {
    v.as_object().unwrap().clone()
}

#[tokio::test]
async fn get_all_shows_typed_values_and_a_missing_override_as_null() {
    let t = ctx();
    let all = Snapshot::read(&t.ctx).to_json();
    let keys: Vec<&String> = all.as_object().unwrap().keys().collect();
    let web: Vec<&str> = FIELDS.iter().map(|f| f.key).collect();
    assert_eq!(keys, web, "the web's order");
    assert_eq!(all["system_prompt"], Value::Null);
    assert_eq!(all["max_order_value"], json!("500000.00"));
    assert_eq!(all["allowed_products"], json!(["CNC", "MIS", "NRML"]));
    assert_eq!(all["trading_enabled"], json!(false));
    assert_eq!(all["mcp_enabled"], json!(true));
    let defaults = defaults_json(&t.ctx.data_dir);
    assert_eq!(defaults["system_prompt"], json!(""));
    assert_eq!(
        defaults["kill_switch_file"],
        json!(t.ctx.data_dir.join("agent_kill_switch").to_string_lossy())
    );
}

#[tokio::test]
async fn an_update_is_all_or_nothing_and_answers_fresh() {
    let t = ctx();
    let refused = update(
        &t.ctx,
        &body(json!({"trading_enabled": true, "max_order_quantity": "ten"})),
    );
    assert_eq!(
        refused,
        Err(UpdateError::Invalid(
            "max_order_quantity must be a whole number, got 'ten'".into()
        ))
    );
    assert!(
        !Snapshot::read(&t.ctx).flag("trading_enabled"),
        "nothing written"
    );
    let unknown = update(
        &t.ctx,
        &body(json!({"zeta": 1, "alpha": 2, "trading_enabled": true})),
    );
    assert_eq!(
        unknown,
        Err(UpdateError::Invalid(
            "Unknown agent setting(s): alpha, zeta".into()
        ))
    );
    let done = update(
        &t.ctx,
        &body(json!({"trading_enabled": "on", "allowed_exchanges": "nse, nfo", "system_prompt": "  Be brief.  "})),
    )
    .unwrap();
    assert_eq!(done["trading_enabled"], json!(true));
    assert_eq!(done["allowed_exchanges"], json!(["NFO", "NSE"]));
    assert_eq!(done["system_prompt"], json!("Be brief."));
    let conn = t.ctx.sqlite.conn().unwrap();
    let stored: String = conn
        .query_row(
            "SELECT value FROM ag_setting WHERE key = 'allowed_exchanges'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, r#"["NFO", "NSE"]"#, "Python's json.dumps");
    assert!(store::delete(&conn, "trading_enabled").unwrap());
    assert!(
        !Snapshot::read(&t.ctx).flag("trading_enabled"),
        "the default again"
    );
}

#[tokio::test]
async fn an_unchanged_write_takes_no_write() {
    let t = ctx();
    let conn = t.ctx.sqlite.conn().unwrap();
    store::write_all(&conn, &[("kill_switch", "true".into())]).unwrap();
    let before: i64 = conn
        .query_row("SELECT total_changes()", [], |r| r.get(0))
        .unwrap();
    store::write_all(&conn, &[("kill_switch", "true".into())]).unwrap();
    let after: i64 = conn
        .query_row("SELECT total_changes()", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after);
}

/// D-014: an override longer than the prompt can hold is refused where it
/// is saved, with a sentence, instead of being dropped at the next message.
#[tokio::test]
async fn a_system_prompt_too_long_to_fit_is_refused_at_save() {
    let t = ctx();
    let long = "a".repeat(schema::MAX_SYSTEM_PROMPT_CHARS + 1);
    assert_eq!(
        update(&t.ctx, &body(json!({ "system_prompt": long }))),
        Err(UpdateError::Invalid(parse::SYSTEM_PROMPT_TOO_LONG.into()))
    );
    let fits = format!("  {}  ", "a".repeat(schema::MAX_SYSTEM_PROMPT_CHARS));
    assert!(update(&t.ctx, &body(json!({ "system_prompt": fits }))).is_ok());
    for word in ["40000", "40,000", "budget", "token"] {
        assert!(!parse::SYSTEM_PROMPT_TOO_LONG.contains(word), "{}", word);
    }
}

#[tokio::test]
async fn the_answers_carry_the_webs_envelope_and_statuses() {
    let t = ctx();
    let got = settings_answer(&t.ctx);
    assert_eq!(got["status"], "success");
    assert!(got["data"].is_object() && got["defaults"].is_object());
    let empty = update_answer(&t.ctx, &serde_json::Map::new()).unwrap_err();
    assert_eq!(
        (empty.status, empty.message.as_str()),
        (400, "Nothing to update")
    );
    let bad = update_answer(&t.ctx, &body(json!({"kill_switch": "maybe"}))).unwrap_err();
    assert_eq!(bad.status, 400);
    assert_eq!(bad.message, "kill_switch must be a boolean, got 'maybe'");
    let ok = update_answer(&t.ctx, &body(json!({"kill_switch": true}))).unwrap();
    assert_eq!(ok["data"]["kill_switch"], json!(true));
}

#[tokio::test]
async fn a_run_reads_the_switches_and_voice_trades_only_under_the_master() {
    let t = ctx();
    let s = policy::run_settings(&Snapshot::read(&t.ctx));
    assert!(!s.trading_enabled && !s.voice_trading_enabled);
    assert!(s.mcp_enabled, "external data ships on");
    assert_eq!(s.voice_agent_name.as_deref(), Some("Vega"));
    update(&t.ctx, &body(json!({"voice_trading_enabled": true}))).unwrap();
    assert!(!policy::run_settings(&Snapshot::read(&t.ctx)).voice_trading_enabled);
    update(
        &t.ctx,
        &body(json!({"trading_enabled": true, "system_prompt": "x"})),
    )
    .unwrap();
    let s = policy::run_settings(&Snapshot::read(&t.ctx));
    assert!(s.trading_enabled && s.voice_trading_enabled);
    assert_eq!(s.system_prompt_override.as_deref(), Some("x"));
    let installed = t
        .ctx
        .agent
        .policy()
        .settings(&t.ctx, crate::agent::contract::tool::Surface::Chat);
    assert!(
        installed.trading_enabled,
        "the app runs with this policy installed"
    );
}

#[tokio::test]
async fn the_kill_switch_panel_sets_both_halves_and_reads_either() {
    let t = ctx();
    let path = t.ctx.data_dir.join("agent_kill_switch");
    let off = kill_switch::state(&t.ctx);
    assert!(!off.engaged && off.path == path);
    let on = kill_switch::set(&t.ctx, true).unwrap();
    assert!(on.engaged && on.by_setting && on.by_file && path.exists());
    let released = kill_switch::set(&t.ctx, false).unwrap();
    assert!(!released.engaged && !path.exists());
    // The file alone engages it (a trader with no working screen).
    std::fs::write(&path, "").unwrap();
    let by_file = kill_switch::state(&t.ctx);
    assert!(by_file.engaged && by_file.by_file && !by_file.by_setting);
    std::fs::remove_file(&path).unwrap();
    // A relative setting is inside the data folder.
    update(&t.ctx, &body(json!({"kill_switch_file": "stops/halt"}))).unwrap();
    assert_eq!(
        kill_switch::state(&t.ctx).path,
        t.ctx.data_dir.join("stops/halt")
    );
    // An absolute path is kept as it is.
    let elsewhere = t.dir.path().join("elsewhere").join("halt");
    assert_eq!(
        kill_switch::resolve(&elsewhere.to_string_lossy(), &t.ctx.data_dir),
        elsewhere
    );
}

/// Migration 098 on a database already in use: the three tables appear,
/// the voice rows are seeded only where none exists, nothing else changes,
/// and a second run changes nothing.
#[test]
fn the_migration_seeds_voice_rows_on_a_populated_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = crate::db::sqlite::SqliteDb::new(&dir.path().join("openalgo.db")).unwrap();
    let conn = db.conn().unwrap();
    conn.execute_batch(
        "DROP TABLE ag_provider_model; DROP TABLE ag_secret; DROP TABLE ag_setting;
         DELETE FROM migrations WHERE name = '098_agent_config';
         INSERT INTO users (username, email, password_hash) VALUES ('trader', 'e', 'h');
         INSERT INTO ag_conversation (user_id, title, created_at, updated_at)
         VALUES ('trader', 'Kept', 't', 't');
         CREATE TABLE ag_setting (key TEXT PRIMARY KEY, value TEXT);
         INSERT INTO ag_setting (key, value) VALUES ('voice_speaker', 'cedar'), ('voice_enabled', NULL);",
    )
    .unwrap();
    crate::db::sqlite::migrations::run_migrations(&conn).unwrap();
    let value = |k: &str| -> Option<String> {
        conn.query_row("SELECT value FROM ag_setting WHERE key = ?1", [k], |r| {
            r.get(0)
        })
        .unwrap()
    };
    assert_eq!(
        value("voice_speaker").as_deref(),
        Some("cedar"),
        "the trader's row stays"
    );
    assert_eq!(value("voice_enabled"), None, "an emptied row stays emptied");
    assert_eq!(value("voice_model").as_deref(), Some("gpt-live-1"));
    assert_eq!(value("voice_agent_name").as_deref(), Some("Vega"));
    assert_eq!(value("voice_trading_enabled").as_deref(), Some("false"));
    assert_eq!(value("voice_idle_timeout_seconds").as_deref(), Some("180"));
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM ag_setting", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 8);
    let kept: String = conn
        .query_row("SELECT title FROM ag_conversation", [], |r| r.get(0))
        .unwrap();
    assert_eq!(kept, "Kept");
    for t in ["ag_provider_model", "ag_secret"] {
        let n: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {}", t), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "{}", t);
    }
    crate::agent::models::migration::migrate(&conn).unwrap();
    let again: i64 = conn
        .query_row("SELECT COUNT(*) FROM ag_setting", [], |r| r.get(0))
        .unwrap();
    assert_eq!(again, 8);
}
