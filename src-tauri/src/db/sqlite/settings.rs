//! Settings management

use crate::db::sqlite::models::{AutoLogoutConfig, Settings, WebhookConfig};
use crate::error::Result;
use rusqlite::Connection;

/// Get settings
pub fn get_settings(conn: &Connection) -> Result<Settings> {
    let settings = conn.query_row(
        "SELECT id, theme, default_broker, default_exchange, default_product,
                order_confirm, sound_enabled, auto_logout_enabled,
                auto_logout_hour, auto_logout_minute, auto_logout_warnings, analyze_mode
         FROM settings WHERE id = 1",
        [],
        |row| {
            let warnings_json: String = row.get(10)?;
            let warnings: Vec<u32> = serde_json::from_str(&warnings_json).unwrap_or(vec![30, 15, 5, 1]);
            let analyze_mode: Option<i32> = row.get(11).ok();

            Ok(Settings {
                id: row.get(0)?,
                theme: row.get(1)?,
                default_broker: row.get(2)?,
                default_exchange: row.get(3)?,
                default_product: row.get(4)?,
                order_confirm: row.get::<_, i32>(5)? == 1,
                sound_enabled: row.get::<_, i32>(6)? == 1,
                auto_logout_enabled: row.get::<_, i32>(7)? == 1,
                auto_logout_hour: row.get::<_, u32>(8)?,
                auto_logout_minute: row.get::<_, u32>(9)?,
                auto_logout_warnings: warnings,
                analyze_mode: analyze_mode.map(|v| v == 1),
            })
        },
    )?;

    Ok(settings)
}

/// Get auto-logout configuration
pub fn get_auto_logout_config(conn: &Connection) -> Result<AutoLogoutConfig> {
    let config = conn.query_row(
        "SELECT auto_logout_enabled, auto_logout_hour, auto_logout_minute, auto_logout_warnings
         FROM settings WHERE id = 1",
        [],
        |row| {
            let warnings_json: String = row.get(3)?;
            let warnings: Vec<u32> = serde_json::from_str(&warnings_json).unwrap_or(vec![30, 15, 5, 1]);

            Ok(AutoLogoutConfig {
                enabled: row.get::<_, i32>(0)? == 1,
                hour: row.get::<_, u32>(1)?,
                minute: row.get::<_, u32>(2)?,
                warnings,
            })
        },
    )?;

    Ok(config)
}

/// Update auto-logout configuration
pub fn update_auto_logout_config(
    conn: &Connection,
    enabled: Option<bool>,
    hour: Option<u32>,
    minute: Option<u32>,
    warnings: Option<Vec<u32>>,
) -> Result<AutoLogoutConfig> {
    let mut updates = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(e) = enabled {
        updates.push("auto_logout_enabled = ?");
        params.push(Box::new(e as i32));
    }
    if let Some(h) = hour {
        // Validate hour (0-23)
        if h <= 23 {
            updates.push("auto_logout_hour = ?");
            params.push(Box::new(h));
        }
    }
    if let Some(m) = minute {
        // Validate minute (0-59)
        if m <= 59 {
            updates.push("auto_logout_minute = ?");
            params.push(Box::new(m));
        }
    }
    if let Some(w) = warnings {
        let warnings_json = serde_json::to_string(&w).unwrap_or("[30, 15, 5, 1]".to_string());
        updates.push("auto_logout_warnings = ?");
        params.push(Box::new(warnings_json));
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

    get_auto_logout_config(conn)
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

/// Get webhook configuration
pub fn get_webhook_config(conn: &Connection) -> Result<WebhookConfig> {
    let config = conn.query_row(
        "SELECT webhook_enabled, webhook_port, webhook_host, ngrok_url, webhook_secret
         FROM settings WHERE id = 1",
        [],
        |row| {
            Ok(WebhookConfig {
                enabled: row.get::<_, i32>(0)? == 1,
                port: row.get::<_, u16>(1)?,
                host: row.get::<_, String>(2)?,
                ngrok_url: row.get::<_, Option<String>>(3)?,
                webhook_secret: row.get::<_, Option<String>>(4)?,
            })
        },
    )?;

    Ok(config)
}

/// Update webhook configuration
pub fn update_webhook_config(
    conn: &Connection,
    enabled: Option<bool>,
    port: Option<u16>,
    host: Option<String>,
    ngrok_url: Option<String>,
    webhook_secret: Option<String>,
) -> Result<WebhookConfig> {
    let mut updates = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(e) = enabled {
        updates.push("webhook_enabled = ?");
        params.push(Box::new(e as i32));
    }
    if let Some(p) = port {
        // Validate port (1024-65535 for non-privileged)
        if p >= 1024 {
            updates.push("webhook_port = ?");
            params.push(Box::new(p));
        }
    }
    if let Some(h) = host {
        updates.push("webhook_host = ?");
        params.push(Box::new(h));
    }
    if let Some(url) = ngrok_url {
        updates.push("ngrok_url = ?");
        params.push(Box::new(url));
    }
    if let Some(secret) = webhook_secret {
        updates.push("webhook_secret = ?");
        params.push(Box::new(secret));
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

    get_webhook_config(conn)
}
