use super::*;

/// The settings row as an earlier build left it: the web's columns, none
/// of the tunnel's own.
fn settings_before_096(host_server: Option<&str>, ngrok_allow: bool) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE settings (
            id INTEGER PRIMARY KEY,
            host_server TEXT,
            ngrok_allow INTEGER NOT NULL DEFAULT 0,
            updated_at TEXT
        );
        INSERT INTO settings (id) VALUES (1);",
    )
    .unwrap();
    conn.execute(
        "UPDATE settings SET host_server = ?1, ngrok_allow = ?2 WHERE id = 1",
        params![host_server, ngrok_allow],
    )
    .unwrap();
    conn
}

#[test]
fn the_migration_takes_the_domain_an_earlier_build_ran_ngrok_on() {
    let conn = settings_before_096(Some("https://Trader-1.ngrok-free.app/"), true);
    migrate(&conn).unwrap();
    // Run again, as a partly migrated database would: nothing changes.
    migrate(&conn).unwrap();
    let s = read(&conn).unwrap();
    assert_eq!(s.domain.as_deref(), Some("trader-1.ngrok-free.app"));
    assert!(s.enabled, "the switch keeps what the trader chose");
    assert_eq!(s.authtoken, None);
}

#[test]
fn the_migration_takes_no_domain_from_another_kind_of_address() {
    for host in [
        None,
        Some("http://127.0.0.1:5000"),
        Some("https://desk.trycloudflare.com"),
        Some("https://ngrok.app.example.com"),
        Some("not a url"),
    ] {
        let conn = settings_before_096(host, true);
        migrate(&conn).unwrap();
        assert_eq!(read(&conn).unwrap().domain, None, "{:?}", host);
    }
}

#[test]
fn the_migration_never_replaces_a_stored_domain() {
    let conn = settings_before_096(Some("https://old.ngrok.app"), false);
    migrate(&conn).unwrap();
    save(
        &conn,
        &TunnelUpdate {
            domain: Some(Some("mine.ngrok.dev".into())),
            ..Default::default()
        },
    )
    .unwrap();
    migrate(&conn).unwrap();
    assert_eq!(
        read(&conn).unwrap().domain.as_deref(),
        Some("mine.ngrok.dev")
    );
}

#[test]
fn a_save_changes_only_what_it_names() {
    let conn = settings_before_096(None, false);
    migrate(&conn).unwrap();
    save(
        &conn,
        &TunnelUpdate {
            enabled: Some(true),
            authtoken: Some(Some("v1:n:c".into())),
            domain: Some(Some("a.ngrok.app".into())),
        },
    )
    .unwrap();
    save(&conn, &TunnelUpdate::default()).unwrap();
    assert_eq!(
        read(&conn).unwrap(),
        StoredTunnel {
            enabled: true,
            authtoken: Some("v1:n:c".into()),
            domain: Some("a.ngrok.app".into()),
        }
    );
    save(
        &conn,
        &TunnelUpdate {
            authtoken: Some(None),
            domain: Some(None),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        read(&conn).unwrap(),
        StoredTunnel {
            enabled: true,
            ..Default::default()
        }
    );
}

/// The real upgrade path: a populated database from before 096 is
/// migrated by the app's own migration run.
#[test]
fn an_upgraded_database_gets_the_columns_and_the_domain() {
    let dir = tempfile::tempdir().unwrap();
    let db = crate::db::sqlite::SqliteDb::new(&dir.path().join("openalgo.db")).unwrap();
    let conn = db.conn().unwrap();
    conn.execute_batch(
        "ALTER TABLE settings DROP COLUMN ngrok_authtoken;
         ALTER TABLE settings DROP COLUMN ngrok_domain;
         DELETE FROM migrations WHERE name = '096_ngrok_tunnel';
         UPDATE settings SET host_server = 'https://desk.ngrok.app', ngrok_allow = 1,
             http_port = 5000 WHERE id = 1;",
    )
    .unwrap();
    crate::db::sqlite::migrations::run_migrations(&conn).unwrap();
    let s = read(&conn).unwrap();
    assert_eq!(s.domain.as_deref(), Some("desk.ngrok.app"));
    assert!(s.enabled);
}
