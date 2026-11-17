//! Which renames a stored master implies, and moving one store's rows.

use super::*;

fn master(rows: &[(&str, &str, &str)]) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(crate::db::sqlite::symbol::CREATE_TABLE)
        .unwrap();
    for (symbol, brsymbol, exchange) in rows {
        conn.execute(
            "INSERT INTO symtoken (symbol, brsymbol, exchange, instrumenttype) VALUES (?1, ?2, ?3, 'EQ')",
            params![symbol, brsymbol, exchange],
        )
        .unwrap();
    }
    conn
}

fn rename(from: &str, to: &str) -> SymbolRename {
    SymbolRename {
        exchange: "NSE".into(),
        from: from.into(),
        to: to.into(),
    }
}

/// Only a series suffix the master stripped is a rename: not a symbol the
/// master still lists, not another broker's own suffix (Angel's -EQ), not
/// another exchange, not a suffix kept on its row.
#[test]
fn a_rename_is_a_series_suffix_the_master_stripped() {
    let conn = master(&[
        ("HFCL", "HFCL-BE", "NSE"),
        ("EMBASSY", "EMBASSY-RR", "NSE"),
        // Twice in the master, once as a rename.
        ("PGINVIT", "PGINVIT-IV", "NSE"),
        ("PGINVIT", "PGINVIT-IV", "NSE"),
        ("RELIANCE", "RELIANCE", "NSE"),
        ("DUAL", "DUAL", "NSE"),
        ("DUAL-BE", "DUAL-BE", "NSE"),
        ("656KA30-SG", "656KA30-SG", "NSE"),
        ("SBIN", "SBIN-EQ", "NSE"),
        ("XYZ", "XYZ-BE", "BSE"),
        // The old name is still another row's symbol: never moved off it.
        ("TWIN", "TWIN-BE", "NSE"),
        ("TWIN-BE", "TWIN-BZ", "NSE"),
    ]);
    assert_eq!(
        series_renames(&conn).unwrap(),
        [
            rename("EMBASSY-RR", "EMBASSY"),
            rename("HFCL-BE", "HFCL"),
            rename("PGINVIT-IV", "PGINVIT"),
        ]
    );
}

fn store() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE legs (id INTEGER PRIMARY KEY, symbol TEXT NOT NULL,
             exchange TEXT NOT NULL, product TEXT NOT NULL, quantity INTEGER NOT NULL,
             UNIQUE (symbol, exchange, product));
         INSERT INTO legs (symbol, exchange, product, quantity) VALUES
             ('HFCL-BE', 'NSE', 'CNC', 100),
             ('HFCL-BE', 'NSE', 'MIS', 5),
             ('HFCL', 'NSE', 'MIS', 7),
             ('HFCL-BE', 'BSE', 'CNC', 3),
             ('EMBASSY-RR', 'NSE', 'CNC', 40),
             ('RELIANCE', 'NSE', 'CNC', 1);",
    )
    .unwrap();
    conn
}

fn legs(conn: &Connection) -> Vec<(String, String, String, i64)> {
    let mut stmt = conn
        .prepare("SELECT symbol, exchange, product, quantity FROM legs ORDER BY id")
        .unwrap();
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap();
    rows.collect::<rusqlite::Result<_>>().unwrap()
}

/// Rows move on the rename's exchange only; a row whose move would put two
/// positions under one key stays and is named; a second pass moves nothing.
#[test]
fn rows_move_to_the_current_symbol_and_a_clash_stays() {
    let conn = store();
    let renames = [rename("HFCL-BE", "HFCL"), rename("EMBASSY-RR", "EMBASSY")];
    let tally = rename_rows(&conn, "legs", &renames).unwrap();
    assert_eq!(tally.moved, 2, "{tally:?}");
    assert_eq!(tally.kept, ["legs: HFCL-BE -> HFCL"]);
    let row = |s: &str, e: &str, p: &str, q: i64| (s.into(), e.into(), p.into(), q);
    assert_eq!(
        legs(&conn),
        [
            row("HFCL", "NSE", "CNC", 100),
            row("HFCL-BE", "NSE", "MIS", 5),
            row("HFCL", "NSE", "MIS", 7),
            row("HFCL-BE", "BSE", "CNC", 3),
            row("EMBASSY", "NSE", "CNC", 40),
            row("RELIANCE", "NSE", "CNC", 1),
        ]
    );
    let again = rename_rows(&conn, "legs", &renames).unwrap();
    assert_eq!(again.moved, 0);
    assert_eq!(
        rename_rows(&conn, "legs", &[]).unwrap(),
        RenameTally::default()
    );
}
