//! Settings management

use crate::db::sqlite::models::Settings;
use crate::error::Result;
use rusqlite::Connection;

/// Get settings
pub fn get_settings(conn: &Connection) -> Result<Settings> {
    let settings = conn.query_row(
        "SELECT id, theme, default_broker, default_exchange, default_product, order_confirm, sound_enabled
         FROM settings WHERE id = 1",
        [],
        |row| {
            Ok(Settings {
                id: row.get(0)?,
                theme: row.get(1)?,
                default_broker: row.get(2)?,
                default_exchange: row.get(3)?,
                default_product: row.get(4)?,
                order_confirm: row.get::<_, i32>(5)? == 1,
                sound_enabled: row.get::<_, i32>(6)? == 1,
            })
        },
    )?;

    Ok(settings)
}

/// Update settings
pub fn update_settings(
    conn: &Connection,
    theme: Option<String>,
    default_broker: Option<String>,
    default_exchange: Option<String>,
    default_product: Option<String>,
    order_confirm: Option<bool>,
    sound_enabled: Option<bool>,
) -> Result<Settings> {
    let mut updates = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(t) = theme {
        updates.push("theme = ?");
        params.push(Box::new(t));
    }
    if let Some(b) = default_broker {
        updates.push("default_broker = ?");
        params.push(Box::new(b));
    }
    if let Some(e) = default_exchange {
        updates.push("default_exchange = ?");
        params.push(Box::new(e));
    }
    if let Some(p) = default_product {
        updates.push("default_product = ?");
        params.push(Box::new(p));
    }
    if let Some(c) = order_confirm {
        updates.push("order_confirm = ?");
        params.push(Box::new(c as i32));
    }
    if let Some(s) = sound_enabled {
        updates.push("sound_enabled = ?");
        params.push(Box::new(s as i32));
    }

    if !updates.is_empty() {
        updates.push("updated_at = datetime('now')");

        let sql = format!(
            "UPDATE settings SET {} WHERE id = 1",
            updates.join(", ")
        );

        let params_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        conn.execute(&sql, params_refs.as_slice())?;
    }

    get_settings(conn)
}
