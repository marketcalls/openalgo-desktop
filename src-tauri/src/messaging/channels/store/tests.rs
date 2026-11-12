use super::*;
use crate::db::sqlite::SqliteDb;
use chrono::TimeZone;

const SLACK_URL: &str = "https://hooks.slack.com/services/TFAKE/BFAKE/notarealslacktoken";

fn db() -> (tempfile::TempDir, SqliteDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = SqliteDb::new(&dir.path().join("openalgo.db")).unwrap();
    (dir, db)
}

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 11, 9, 15, 30).unwrap()
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// Web test/test_alerts_api.py::test_the_migration_adds_the_table_and_no_rows,
/// on a database that already holds Telegram and WhatsApp settings: the
/// migration adds the table and no rows, leaves every other value as it
/// was, and runs again without changing anything.
#[test]
fn the_migration_adds_the_table_and_no_rows_on_a_populated_database() {
    let (_dir, db) = db();
    let conn = db.conn().unwrap();
    conn.execute_batch(
        "INSERT INTO telegram_users (telegram_id, openalgo_username, notifications_enabled)
             VALUES (42, 'trader', 0);
         UPDATE bot_config SET is_active = 1, bot_username = 'mybot' WHERE id = 1;
         INSERT INTO whatsapp_users (whatsapp_jid, phone_number, openalgo_username)
             VALUES ('919@s.whatsapp.net', '919', 'trader');
         DROP TABLE alert_channels;
         DELETE FROM migrations WHERE name = '102_alert_channels';",
    )
    .unwrap();
    for _ in 0..2 {
        crate::db::sqlite::migrations::run_migrations(&conn).unwrap();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM alert_channels"), 0);
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM migrations WHERE name = '102_alert_channels'"
            ),
            1
        );
    }
    // Nothing the trader set before is touched.
    assert_eq!(
        count(
            &conn,
            "SELECT notifications_enabled FROM telegram_users WHERE telegram_id = 42"
        ),
        0
    );
    assert_eq!(
        count(&conn, "SELECT is_active FROM bot_config WHERE id = 1"),
        1
    );
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM whatsapp_users"), 1);
    // The web's columns.
    for column in [
        "channel",
        "enabled",
        "categories",
        "webhook_url",
        "updated_at",
    ] {
        assert!(
            crate::db::sqlite::migrations::column_exists(&conn, "alert_channels", column).unwrap(),
            "{}",
            column
        );
    }
}

/// Web test/test_alerts_webhooks.py::test_a_saved_webhook_is_encrypted_and_reads_back
#[test]
fn a_saved_webhook_is_sealed_and_reads_back() {
    let (_dir, db) = db();
    let sec = SecurityManager::for_tests();
    let conn = db.conn().unwrap();
    save_webhook(
        &conn,
        &sec,
        "slack",
        SLACK_URL,
        true,
        &["orders_placed"],
        now(),
    )
    .unwrap();
    let raw: String = conn
        .query_row("SELECT webhook_url FROM alert_channels", [], |r| r.get(0))
        .unwrap();
    assert!(!raw.contains("notarealslacktoken"));
    assert!(raw.starts_with("v1:"));
    assert_eq!(
        read_webhook_url(&conn, &sec, "slack")
            .unwrap()
            .unwrap()
            .expose(),
        SLACK_URL
    );
    let stored = load(&conn, "slack").unwrap().unwrap();
    assert_eq!(
        stored,
        StoredChannel {
            enabled: true,
            categories: vec!["orders_placed".into()],
            webhook_saved: true
        }
    );
}

/// The address is bound to its channel: moved to another row it does not
/// open, and a value that is not a sealed address reads as not saved.
#[test]
fn a_webhook_moved_to_another_channel_or_damaged_reads_as_not_saved() {
    let (_dir, db) = db();
    let sec = SecurityManager::for_tests();
    let conn = db.conn().unwrap();
    save_webhook(&conn, &sec, "slack", SLACK_URL, true, &[], now()).unwrap();
    conn.execute(
        "INSERT INTO alert_channels (channel, enabled, categories, webhook_url, updated_at)
         SELECT 'discord', 1, '', webhook_url, updated_at FROM alert_channels",
        [],
    )
    .unwrap();
    assert!(read_webhook_url(&conn, &sec, "discord").unwrap().is_none());
    conn.execute(
        "UPDATE alert_channels SET webhook_url = 'not-a-ciphertext' WHERE channel = 'slack'",
        [],
    )
    .unwrap();
    assert!(read_webhook_url(&conn, &sec, "slack").unwrap().is_none());
    assert!(
        load(&conn, "slack").unwrap().unwrap().webhook_saved,
        "still stored"
    );
}

/// A change of switch or groups keeps the saved address; saving an address
/// keeps the groups already chosen; forgetting it switches the channel off.
#[test]
fn settings_and_webhook_writes_keep_each_other() {
    let (_dir, db) = db();
    let sec = SecurityManager::for_tests();
    let conn = db.conn().unwrap();
    assert_eq!(load(&conn, "discord").unwrap(), None);
    save_webhook(
        &conn,
        &sec,
        "discord",
        "https://discord.com/api/webhooks/1/token",
        true,
        &["orders_placed", "gtt_orders"],
        now(),
    )
    .unwrap();
    save_channel(&conn, "discord", false, &["gtt_orders"], now()).unwrap();
    let stored = load(&conn, "discord").unwrap().unwrap();
    assert!(!stored.enabled && stored.webhook_saved);
    assert_eq!(stored.categories, ["gtt_orders"]);
    save_webhook(
        &conn,
        &sec,
        "discord",
        "https://discord.com/api/webhooks/2/token",
        true,
        &["orders_placed"],
        now(),
    )
    .unwrap();
    let stored = load(&conn, "discord").unwrap().unwrap();
    assert!(stored.enabled);
    assert_eq!(
        stored.categories,
        ["gtt_orders"],
        "the existing choice is kept"
    );
    clear_webhook(&conn, "discord", now()).unwrap();
    let stored = load(&conn, "discord").unwrap().unwrap();
    assert!(!stored.enabled && !stored.webhook_saved);
    assert!(read_webhook_url(&conn, &sec, "discord").unwrap().is_none());
}

/// KEY-01: the sealed address proves the data key, and a fresh start drops
/// it and switches its channel off; a channel without one is untouched.
#[test]
fn witness_and_drop_cover_the_saved_webhooks() {
    let (_dir, db) = db();
    let sec = SecurityManager::for_tests();
    let conn = db.conn().unwrap();
    assert!(witness(&conn).unwrap().is_none());
    save_channel(&conn, "telegram", false, &["gtt_orders"], now()).unwrap();
    save_webhook(&conn, &sec, "slack", SLACK_URL, true, &[], now()).unwrap();
    let w = witness(&conn).unwrap().unwrap();
    assert_eq!(
        sec.decrypt(&w.ciphertext, &w.nonce, &w.aad)
            .unwrap()
            .expose(),
        SLACK_URL
    );
    crate::db::sqlite::key_vault::drop_encrypted_values(&conn).unwrap();
    assert!(witness(&conn).unwrap().is_none());
    let slack = load(&conn, "slack").unwrap().unwrap();
    assert!(!slack.enabled && !slack.webhook_saved);
    let telegram = load(&conn, "telegram").unwrap().unwrap();
    assert_eq!(telegram.categories, ["gtt_orders"]);
    assert!(!telegram.enabled, "a switch the trader set is kept");
}
