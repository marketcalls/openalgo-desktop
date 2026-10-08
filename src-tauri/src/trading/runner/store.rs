//! What the runner keeps in the main SQLite database: each deployment's run
//! settings (web `services/openscript_run_config.py`, there a JSON file), the
//! schedules (web `strategies/openscript_runner_schedules.json`) and every
//! order a run placed (the per-deployment book's own record, like the strategy
//! module's order rows).
//!
//! **The unit is a deployment, not a script.** One script runs on several
//! instruments and intervals at once, each with its own position, book and
//! decision to stop, so settings, schedules and orders are keyed by the id
//! [`deployment_id`] mints. A new deployment gets a random token in its id so
//! one made where another was removed does not inherit that one's orders.
//!
//! Every write that reads before it changes runs in one immediate
//! transaction, which is this store's form of the web's write lock.

use crate::error::Result;
use crate::trading::names::{
    is_deployment_id, is_input_key, is_run_field, is_script_name, not_a_script_name,
    DEPLOYMENT_PREFIX,
};
use chrono::{DateTime, Utc};
use chrono_tz::Asia::Kolkata;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// The products a deployment may name (web `PRODUCTS`).
pub const PRODUCTS: &[&str] = &["CNC", "NRML", "MIS"];
/// How many parameters, and how long a text one may be.
pub const MAX_INPUTS: usize = 64;
pub const MAX_TEXT_LENGTH: usize = 256;
const MAX_ID_LENGTH: usize = 100;
const DIGEST_LENGTH: usize = 8;
/// Orders older than this are pruned at start-up so the table stays bounded.
pub const ORDER_RETENTION_DAYS: i64 = 180;

/// Migration `074_openscript_runner`.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS openscript_deployments (
            deployment TEXT PRIMARY KEY,
            script TEXT NOT NULL,
            symbol TEXT NOT NULL,
            exchange TEXT NOT NULL,
            interval TEXT NOT NULL,
            product TEXT NOT NULL DEFAULT '',
            user_id TEXT,
            inputs TEXT NOT NULL DEFAULT '{}',
            mode TEXT,
            updated_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS ix_openscript_deployments_script ON openscript_deployments(script);
         CREATE TABLE IF NOT EXISTS openscript_schedules (
            name TEXT PRIMARY KEY,
            start_time TEXT NOT NULL,
            stop_time TEXT,
            days TEXT NOT NULL,
            updated_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS openscript_orders (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            deployment TEXT NOT NULL,
            mode TEXT NOT NULL,
            intent_id INTEGER,
            tag TEXT NOT NULL DEFAULT '',
            symbol TEXT NOT NULL,
            exchange TEXT NOT NULL,
            action TEXT NOT NULL,
            quantity INTEGER NOT NULL,
            pricetype TEXT NOT NULL,
            product TEXT NOT NULL,
            orderid TEXT,
            status TEXT NOT NULL,
            filled_quantity INTEGER NOT NULL DEFAULT 0,
            average_price REAL,
            message TEXT,
            is_exit INTEGER NOT NULL DEFAULT 0,
            placed_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS ix_openscript_orders_deployment ON openscript_orders(deployment);
         CREATE INDEX IF NOT EXISTS ix_openscript_orders_orderid ON openscript_orders(orderid);",
    )?;
    Ok(())
}

/// A moment as the web writes a settings timestamp.
pub fn ist_text(now: DateTime<Utc>) -> String {
    now.with_timezone(&Kolkata)
        .format("%Y-%m-%d %H:%M:%S IST")
        .to_string()
}

// ------------------------------------------------------------------ ids

/// A script's name without its extension.
pub fn stem(script: &str) -> &str {
    script.strip_suffix(".oscript").unwrap_or(script)
}

/// A token for a deployment being created.
pub fn new_token() -> String {
    use rand::RngCore;
    let mut b = [0u8; 3];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}

/// The id a deployment is known by everywhere: settings key, log name and
/// the `strategy` tag on every order (web `deployment_id`). A readable id is
/// built first; one that would not fit keeps a readable head and ends in a
/// digest of the whole, so two long names never collide.
pub fn deployment_id(
    script: &str,
    symbol: &str,
    exchange: &str,
    interval: &str,
    token: &str,
) -> String {
    let base = format!("{}_{}", DEPLOYMENT_PREFIX, stem(script));
    if symbol.is_empty() {
        return base;
    }
    let mut full = base;
    for part in [symbol, exchange, interval, token] {
        if !part.is_empty() {
            full.push('_');
            full.push_str(part);
        }
    }
    if full.len() <= MAX_ID_LENGTH {
        return full;
    }
    let joined = [script, symbol, exchange, interval, token].join("|");
    let digest = hex::encode(Sha256::digest(joined.as_bytes()));
    let keep = MAX_ID_LENGTH - DIGEST_LENGTH - 1;
    let head: String = full.chars().take(keep).collect();
    format!("{}_{}", head, &digest[..DIGEST_LENGTH])
}

// ------------------------------------------------------------ deployments

/// One deployment's run settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Deployment {
    pub deployment: String,
    pub script: String,
    pub symbol: String,
    pub exchange: String,
    pub interval: String,
    pub product: String,
    pub user_id: Option<String>,
    pub inputs: Map<String, Value>,
    pub mode: Option<String>,
    pub updated_at: String,
}

impl Deployment {
    /// The settings answer the runner routes report (web `_settings_answer`).
    pub fn answer(&self, name: &str) -> Value {
        json!({
            "deployment": name,
            "file": if self.script.is_empty() { name } else { self.script.as_str() },
            "symbol": self.symbol,
            "exchange": self.exchange,
            "interval": self.interval,
            "product": self.product,
            "inputs": Value::Object(self.inputs.clone()),
            "updated_at": self.updated_at,
        })
    }
}

fn deployment_from_row(r: &rusqlite::Row) -> rusqlite::Result<Deployment> {
    let inputs: String = r.get(7)?;
    Ok(Deployment {
        deployment: r.get(0)?,
        script: r.get(1)?,
        symbol: r.get(2)?,
        exchange: r.get(3)?,
        interval: r.get(4)?,
        product: r.get(5)?,
        user_id: r.get(6)?,
        inputs: serde_json::from_str::<Value>(&inputs)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .map(|m| checked_inputs(&Value::Object(m)).unwrap_or_default())
            .unwrap_or_default(),
        mode: r
            .get::<_, Option<String>>(8)?
            .filter(|m| m == "live" || m == "sandbox"),
        updated_at: r.get(9)?,
    })
}

const DEPLOYMENT_COLUMNS: &str =
    "deployment, script, symbol, exchange, interval, product, user_id, inputs, mode, updated_at";

/// Every deployment, by id.
pub fn all_deployments(conn: &Connection) -> Result<BTreeMap<String, Deployment>> {
    let mut st = conn.prepare_cached(&format!(
        "SELECT {} FROM openscript_deployments ORDER BY deployment",
        DEPLOYMENT_COLUMNS
    ))?;
    let rows = st
        .query_map([], deployment_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(|d| (d.deployment.clone(), d))
        .collect())
}

/// Every deployment of one script.
pub fn deployments_of(conn: &Connection, script: &str) -> Result<Vec<Deployment>> {
    let mut st = conn.prepare_cached(&format!(
        "SELECT {} FROM openscript_deployments WHERE script = ?1 ORDER BY deployment",
        DEPLOYMENT_COLUMNS
    ))?;
    let rows = st
        .query_map(params![script], deployment_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn deployment_by_id(conn: &Connection, id: &str) -> Result<Option<Deployment>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {} FROM openscript_deployments WHERE deployment = ?1",
                DEPLOYMENT_COLUMNS
            ),
            params![id],
            deployment_from_row,
        )
        .optional()?)
}

/// One deployment's settings. `name` is a deployment id, or a script name
/// when that script is deployed exactly once; a script deployed twice is
/// never resolved by name, because guessing which instrument a caller meant
/// is how an order reaches the one they did not.
pub fn read(conn: &Connection, name: &str) -> Result<Option<Deployment>> {
    if is_deployment_id(name) {
        return deployment_by_id(conn, name);
    }
    if !is_script_name(name) {
        return Ok(None);
    }
    let mut theirs = deployments_of(conn, name)?;
    Ok(if theirs.len() == 1 {
        theirs.pop()
    } else {
        None
    })
}

fn listed(words: &[&str]) -> String {
    match words {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {}", rest.join(", "), last),
    }
}

/// The settings a run needs, or the sentence saying what is missing.
pub fn require(conn: &Connection, name: &str) -> Result<std::result::Result<Deployment, String>> {
    if !is_deployment_id(name) && !is_script_name(name) {
        return Ok(Err(not_a_script_name(name)));
    }
    if is_script_name(name) {
        let theirs = deployments_of(conn, name)?;
        if theirs.len() > 1 {
            let where_ = theirs
                .iter()
                .map(|d| format!("{} {} at {}", d.symbol, d.exchange, d.interval))
                .collect::<Vec<_>>()
                .join(", ");
            return Ok(Err(format!(
                "{} is deployed {} times ({}), so this cannot tell which one to start. Start it from its own row.",
                name,
                theirs.len(),
                where_
            )));
        }
    }
    let Some(found) = read(conn, name)? else {
        return Ok(Err(format!(
            "{} has no run settings saved on this server, so nothing says which instrument, exchange and interval to run it on. Save its run settings, then start it again.",
            name
        )));
    };
    let mut absent = Vec::new();
    for (value, words) in [
        (&found.symbol, "the instrument"),
        (&found.exchange, "the exchange"),
        (&found.interval, "the interval"),
    ] {
        if value.trim().is_empty() {
            absent.push(words);
        }
    }
    if !absent.is_empty() {
        return Ok(Err(format!(
            "{} is missing {} from its run settings. Save them, then start it again.",
            if found.script.is_empty() {
                name
            } else {
                &found.script
            },
            listed(&absent)
        )));
    }
    Ok(Ok(found))
}

/// The parameters a run may carry: true or false, a finite number, or text
/// up to [`MAX_TEXT_LENGTH`]. What makes one correct for a script is the
/// script's to say when the engine loads it.
pub fn checked_inputs(given: &Value) -> std::result::Result<Map<String, Value>, String> {
    let map = match given {
        Value::Null => return Ok(Map::new()),
        Value::String(s) if s.is_empty() => return Ok(Map::new()),
        Value::Object(m) => m,
        _ => return Err("The strategy parameters must be given as a set of named values.".into()),
    };
    if map.len() > MAX_INPUTS {
        return Err(format!(
            "A strategy may carry at most {} parameters.",
            MAX_INPUTS
        ));
    }
    let mut kept = Map::new();
    for (key, value) in map {
        if !is_input_key(key) {
            return Err(format!(
                "{} is not the name of a parameter a script can declare.",
                crate::trading::names::py_repr(key)
            ));
        }
        match value {
            Value::Bool(_) => {}
            Value::Number(n) => {
                if !n.as_f64().is_some_and(f64::is_finite) {
                    return Err(format!("{} was given a number that is not one.", key));
                }
            }
            Value::String(s) => {
                if s.chars().count() > MAX_TEXT_LENGTH {
                    return Err(format!("{} is longer than a parameter may be.", key));
                }
            }
            _ => {
                return Err(format!(
                    "{} was given something a parameter cannot be.",
                    key
                ))
            }
        }
        kept.insert(key.clone(), value.clone());
    }
    Ok(kept)
}

/// What a settings save carries.
#[derive(Debug, Clone, Default)]
pub struct SettingsInput {
    pub symbol: String,
    pub exchange: String,
    pub interval: String,
    pub product: String,
    pub user_id: Option<String>,
    pub inputs: Value,
    /// The deployment being edited; empty creates one.
    pub deployment: String,
}

fn an(words: &str) -> &'static str {
    if words == "exchange" {
        "an"
    } else {
        "a"
    }
}

/// Save what one script is run on (web `write_run_config`). Returns the
/// deployment id and the sentence the route answers with.
pub fn write(
    conn: &mut Connection,
    script: &str,
    input: SettingsInput,
    now: DateTime<Utc>,
) -> Result<std::result::Result<(String, String), String>> {
    if !is_script_name(script) {
        return Ok(Err(not_a_script_name(script)));
    }
    let symbol = input.symbol.trim().to_string();
    let exchange = input.exchange.trim().to_ascii_uppercase();
    let interval = input.interval.trim().to_string();
    let product = input.product.trim().to_ascii_uppercase();
    for (value, words) in [
        (&symbol, "instrument"),
        (&exchange, "exchange"),
        (&interval, "interval"),
    ] {
        if value.is_empty() {
            return Ok(Err(format!(
                "{} needs {} {} to run on",
                script,
                an(words),
                words
            )));
        }
        if !is_run_field(value) {
            return Ok(Err(format!(
                "{} is not {} {} this can start a run on",
                crate::trading::names::py_repr(value),
                an(words),
                words
            )));
        }
    }
    if !product.is_empty() && !PRODUCTS.contains(&product.as_str()) {
        return Ok(Err(format!(
            "{} is not a product this platform sends. Use one of {}.",
            crate::trading::names::py_repr(&product),
            PRODUCTS.join(", ")
        )));
    }
    let settings = match checked_inputs(&input.inputs) {
        Ok(s) => s,
        Err(e) => return Ok(Err(e)),
    };

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let stored = all_deployments(&tx)?;
    let held = if input.deployment.is_empty() {
        None
    } else {
        stored.get(&input.deployment)
    };
    let clash = stored.iter().find(|(id, d)| {
        **id != input.deployment
            && d.script == script
            && d.symbol == symbol
            && d.exchange == exchange
            && d.interval == interval
    });
    if clash.is_some() {
        return Ok(Err(format!(
            "{} is already deployed on {} {} at {}. One strategy runs once on one instrument and interval: change the instrument or the interval, or edit the deployment that is already there.",
            script, symbol, exchange, interval
        )));
    }
    let stays = held.is_some_and(|h| {
        h.script == script && h.symbol == symbol && h.exchange == exchange && h.interval == interval
    });
    let key = match (stays, held) {
        (true, Some(h)) => h.deployment.clone(),
        _ => deployment_id(script, &symbol, &exchange, &interval, &new_token()),
    };
    // An edit that moves the instrument or the interval is a new deployment
    // with an id of its own, as on the web; the one it came from is left as
    // it is (it may be running, and its book stays its own).
    let mode = if stays {
        held.and_then(|h| h.mode.clone())
    } else {
        None
    };
    tx.execute(
        "INSERT INTO openscript_deployments
            (deployment, script, symbol, exchange, interval, product, user_id, inputs, mode, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(deployment) DO UPDATE SET
            script = excluded.script, symbol = excluded.symbol, exchange = excluded.exchange,
            interval = excluded.interval, product = excluded.product, user_id = excluded.user_id,
            inputs = excluded.inputs, mode = excluded.mode, updated_at = excluded.updated_at",
        params![
            key,
            script,
            symbol,
            exchange,
            interval,
            product,
            input.user_id,
            Value::Object(settings).to_string(),
            mode,
            ist_text(now)
        ],
    )?;
    tx.commit()?;
    Ok(Ok((
        key,
        format!(
            "{} will run on {} {} at {}",
            script, symbol, exchange, interval
        ),
    )))
}

/// Remember the side a deployment's run started on, which is where its books
/// are read from once it has stopped.
pub fn record_mode(conn: &Connection, id: &str, mode: &str) -> Result<bool> {
    if mode != "live" && mode != "sandbox" {
        return Ok(false);
    }
    Ok(conn.execute(
        "UPDATE openscript_deployments SET mode = ?2 WHERE deployment = ?1",
        params![id, mode],
    )? > 0)
}

/// Remove one deployment. Returns the id removed, or the sentence why not.
pub fn delete(
    conn: &mut Connection,
    name: &str,
) -> Result<std::result::Result<(String, String), String>> {
    if !is_deployment_id(name) && !is_script_name(name) {
        return Ok(Err(not_a_script_name(name)));
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let found = if is_deployment_id(name) {
        deployment_by_id(&tx, name)?
    } else {
        let theirs = deployments_of(&tx, name)?;
        if theirs.len() > 1 {
            return Ok(Err(format!(
                "{} is deployed {} times, so this cannot tell which to remove. Remove it from its own row.",
                name,
                theirs.len()
            )));
        }
        theirs.into_iter().next()
    };
    let Some(gone) = found else {
        return Ok(Err(format!("{} has no run settings saved", name)));
    };
    tx.execute(
        "DELETE FROM openscript_deployments WHERE deployment = ?1",
        params![gone.deployment],
    )?;
    tx.commit()?;
    Ok(Ok((
        gone.deployment.clone(),
        format!(
            "The run settings for {} are gone",
            if gone.script.is_empty() {
                &gone.deployment
            } else {
                &gone.script
            }
        ),
    )))
}

// --------------------------------------------------------------- schedules

/// When a deployment starts and stops, in IST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    pub start_time: String,
    pub stop_time: Option<String>,
    pub days: Vec<String>,
}

impl Schedule {
    /// `dict(entry, file=name)`, the web's answer for a schedule.
    pub fn answer(&self, name: &str) -> Value {
        json!({
            "start_time": self.start_time,
            "stop_time": self.stop_time,
            "days": self.days,
            "file": name,
        })
    }
}

pub fn all_schedules(conn: &Connection) -> Result<BTreeMap<String, Schedule>> {
    let mut st = conn.prepare_cached(
        "SELECT name, start_time, stop_time, days FROM openscript_schedules ORDER BY name",
    )?;
    let rows = st
        .query_map([], |r| {
            let days: String = r.get(3)?;
            Ok((
                r.get::<_, String>(0)?,
                Schedule {
                    start_time: r.get(1)?,
                    stop_time: r.get(2)?,
                    days: days
                        .split(',')
                        .filter(|d| !d.is_empty())
                        .map(str::to_string)
                        .collect(),
                },
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter(|(name, _)| crate::trading::names::names_something(name))
        .collect())
}

pub fn set_schedule(conn: &Connection, name: &str, s: &Schedule, now: DateTime<Utc>) -> Result<()> {
    conn.execute(
        "INSERT INTO openscript_schedules (name, start_time, stop_time, days, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(name) DO UPDATE SET start_time = excluded.start_time,
            stop_time = excluded.stop_time, days = excluded.days, updated_at = excluded.updated_at",
        params![
            name,
            s.start_time,
            s.stop_time,
            s.days.join(","),
            ist_text(now)
        ],
    )?;
    Ok(())
}

pub fn delete_schedule(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn.execute(
        "DELETE FROM openscript_schedules WHERE name = ?1",
        params![name],
    )? > 0)
}

// ------------------------------------------------------------------ orders

/// One order a run placed, as this store keeps it.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderRow {
    pub id: i64,
    pub deployment: String,
    pub mode: String,
    pub intent_id: Option<i64>,
    pub tag: String,
    pub symbol: String,
    pub exchange: String,
    pub action: String,
    pub quantity: i64,
    pub pricetype: String,
    pub product: String,
    pub orderid: Option<String>,
    pub status: String,
    pub filled_quantity: i64,
    pub average_price: Option<f64>,
    pub message: Option<String>,
    pub is_exit: bool,
}

/// What a new row records.
#[derive(Debug, Clone, Default)]
pub struct NewOrder {
    pub deployment: String,
    pub mode: String,
    pub intent_id: Option<i64>,
    pub tag: String,
    pub symbol: String,
    pub exchange: String,
    pub action: String,
    pub quantity: i64,
    pub pricetype: String,
    pub product: String,
    pub is_exit: bool,
}

/// Engine words for an order that will not change again.
pub const TERMINAL: &[&str] = &["filled", "cancelled", "rejected", "expired"];

pub fn insert_order(conn: &Connection, o: &NewOrder, now: DateTime<Utc>) -> Result<i64> {
    let at = now.to_rfc3339();
    conn.execute(
        "INSERT INTO openscript_orders
            (deployment, mode, intent_id, tag, symbol, exchange, action, quantity, pricetype,
             product, status, is_exit, placed_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'placed', ?11, ?12, ?12)",
        params![
            o.deployment,
            o.mode,
            o.intent_id,
            o.tag,
            o.symbol,
            o.exchange,
            o.action,
            o.quantity,
            o.pricetype,
            o.product,
            o.is_exit,
            at
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Record the destination's answer to a placement.
pub fn placed(
    conn: &Connection,
    id: i64,
    orderid: Option<&str>,
    status: &str,
    message: Option<&str>,
    now: DateTime<Utc>,
) -> Result<()> {
    conn.execute(
        "UPDATE openscript_orders SET orderid = ?2, status = ?3, message = ?4, updated_at = ?5 WHERE id = ?1",
        params![id, orderid, status, message, now.to_rfc3339()],
    )?;
    Ok(())
}

/// Record what the destination now says about an order.
pub fn progressed(
    conn: &Connection,
    id: i64,
    status: &str,
    filled: i64,
    average_price: Option<f64>,
    now: DateTime<Utc>,
) -> Result<()> {
    conn.execute(
        "UPDATE openscript_orders SET status = ?2, filled_quantity = ?3,
            average_price = COALESCE(?4, average_price), updated_at = ?5 WHERE id = ?1",
        params![id, status, filled, average_price, now.to_rfc3339()],
    )?;
    Ok(())
}

fn order_from_row(r: &rusqlite::Row) -> rusqlite::Result<OrderRow> {
    Ok(OrderRow {
        id: r.get(0)?,
        deployment: r.get(1)?,
        mode: r.get(2)?,
        intent_id: r.get(3)?,
        tag: r.get(4)?,
        symbol: r.get(5)?,
        exchange: r.get(6)?,
        action: r.get(7)?,
        quantity: r.get(8)?,
        pricetype: r.get(9)?,
        product: r.get(10)?,
        orderid: r.get(11)?,
        status: r.get(12)?,
        filled_quantity: r.get(13)?,
        average_price: r.get(14)?,
        message: r.get(15)?,
        is_exit: r.get(16)?,
    })
}

const ORDER_COLUMNS: &str =
    "id, deployment, mode, intent_id, tag, symbol, exchange, action, quantity, \
pricetype, product, orderid, status, filled_quantity, average_price, message, is_exit";

/// Every order one deployment placed, oldest first.
pub fn orders_of(conn: &Connection, deployment: &str) -> Result<Vec<OrderRow>> {
    let mut st = conn.prepare_cached(&format!(
        "SELECT {} FROM openscript_orders WHERE deployment = ?1 ORDER BY id",
        ORDER_COLUMNS
    ))?;
    let rows = st
        .query_map(params![deployment], order_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The orders of one deployment that may still change.
pub fn open_orders(conn: &Connection, deployment: &str) -> Result<Vec<OrderRow>> {
    Ok(orders_of(conn, deployment)?
        .into_iter()
        .filter(|o| !TERMINAL.contains(&o.status.as_str()))
        .collect())
}

/// One contract a deployment holds, from its own fills.
#[derive(Debug, Clone, PartialEq)]
pub struct Holding {
    pub symbol: String,
    pub exchange: String,
    pub product: String,
    /// Signed: long positive, short negative.
    pub quantity: i64,
    pub average_price: Option<f64>,
    pub realized: f64,
}

/// What a deployment holds, netted from its own fills with average-cost
/// accounting (realised profit on a reduce, the average reset on a reversal).
pub fn holdings(orders: &[OrderRow]) -> Vec<Holding> {
    let mut book: BTreeMap<(String, String, String), Holding> = BTreeMap::new();
    for o in orders {
        if o.filled_quantity <= 0 {
            continue;
        }
        let side = match o.action.as_str() {
            "BUY" => 1,
            "SELL" => -1,
            _ => continue,
        };
        let key = (o.symbol.clone(), o.exchange.clone(), o.product.clone());
        let h = book.entry(key).or_insert_with(|| Holding {
            symbol: o.symbol.clone(),
            exchange: o.exchange.clone(),
            product: o.product.clone(),
            quantity: 0,
            average_price: None,
            realized: 0.0,
        });
        let qty = o.filled_quantity;
        let price = o.average_price.filter(|p| p.is_finite() && *p > 0.0);
        if h.quantity == 0 || h.quantity.signum() == side {
            let held = h.quantity.abs();
            h.average_price = match (h.average_price, price) {
                (Some(a), Some(p)) => {
                    Some((a * held as f64 + p * qty as f64) / (held + qty) as f64)
                }
                (None, Some(p)) if held == 0 => Some(p),
                _ => None,
            };
            h.quantity += side * qty;
        } else {
            let closing = qty.min(h.quantity.abs());
            if let (Some(a), Some(p)) = (h.average_price, price) {
                h.realized += (p - a) * closing as f64 * h.quantity.signum() as f64;
            }
            h.quantity += side * qty;
            if h.quantity == 0 {
                h.average_price = None;
            } else if h.quantity.signum() == side {
                h.average_price = price;
            }
        }
    }
    book.into_values().collect()
}

/// Drop order rows older than the retention.
pub fn prune_orders(conn: &Connection, now: DateTime<Utc>) -> Result<usize> {
    let cutoff = (now - chrono::Duration::days(ORDER_RETENTION_DAYS)).to_rfc3339();
    Ok(conn.execute(
        "DELETE FROM openscript_orders WHERE placed_at < ?1",
        params![cutoff],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        migrate(&c).unwrap();
        c
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 5, 4, 0, 0).unwrap()
    }

    fn input(symbol: &str, interval: &str) -> SettingsInput {
        SettingsInput {
            symbol: symbol.into(),
            exchange: "nse".into(),
            interval: interval.into(),
            product: "mis".into(),
            user_id: Some("trader".into()),
            inputs: json!({"fast": 9}),
            deployment: String::new(),
        }
    }

    #[test]
    fn ids_are_readable_and_bounded() {
        assert_eq!(deployment_id("t.oscript", "", "", "", ""), "openscript_t");
        assert_eq!(
            deployment_id("t.oscript", "SBIN", "NSE", "5m", "ab12cd"),
            "openscript_t_SBIN_NSE_5m_ab12cd"
        );
        let long = deployment_id(
            &format!("{}.oscript", "a".repeat(64)),
            "NIFTY28MAR2420800CE",
            "NFO",
            "15m",
            "ab12cd",
        );
        assert_eq!(long.len(), 100);
        assert!(is_deployment_id(&long));
        let other = deployment_id(
            &format!("{}.oscript", "a".repeat(64)),
            "NIFTY28MAR2420800PE",
            "NFO",
            "15m",
            "ab12cd",
        );
        assert_ne!(long, other);
    }

    #[test]
    fn create_edit_clash_and_remove() {
        let mut c = db();
        let (id, msg) = write(&mut c, "t.oscript", input("SBIN", "5m"), now())
            .unwrap()
            .unwrap();
        assert_eq!(msg, "t.oscript will run on SBIN NSE at 5m");
        let d = read(&c, &id).unwrap().unwrap();
        assert_eq!(d.exchange, "NSE");
        assert_eq!(d.product, "MIS");
        assert_eq!(read(&c, "t.oscript").unwrap().unwrap().deployment, id);

        // The same four parts again is one strategy twice: refused.
        let e = write(&mut c, "t.oscript", input("SBIN", "5m"), now())
            .unwrap()
            .unwrap_err();
        assert!(e.contains("already deployed on SBIN NSE at 5m"));

        // An edit keeps the id and the recorded side.
        record_mode(&c, &id, "sandbox").unwrap();
        let mut edit = input("SBIN", "5m");
        edit.deployment = id.clone();
        edit.product = "CNC".into();
        let (same, _) = write(&mut c, "t.oscript", edit, now()).unwrap().unwrap();
        assert_eq!(same, id);
        let d = read(&c, &id).unwrap().unwrap();
        assert_eq!(d.product, "CNC");
        assert_eq!(d.mode.as_deref(), Some("sandbox"));

        // A second deployment makes the script name ambiguous.
        let (id2, _) = write(&mut c, "t.oscript", input("SBIN", "1h"), now())
            .unwrap()
            .unwrap();
        assert_ne!(id2, id);
        assert!(read(&c, "t.oscript").unwrap().is_none());
        let why = require(&c, "t.oscript").unwrap().unwrap_err();
        assert!(why.contains("deployed 2 times"));
        assert!(delete(&mut c, "t.oscript")
            .unwrap()
            .unwrap_err()
            .contains("deployed 2 times"));

        delete(&mut c, &id).unwrap().unwrap();
        assert!(read(&c, &id).unwrap().is_none());
        // A deployment made where another was removed gets a new id.
        let (id3, _) = write(&mut c, "t.oscript", input("SBIN", "5m"), now())
            .unwrap()
            .unwrap();
        assert_ne!(id3, id);
    }

    #[test]
    fn settings_are_checked_before_they_are_stored() {
        let mut c = db();
        let mut i = input("", "5m");
        assert_eq!(
            write(&mut c, "t.oscript", i.clone(), now())
                .unwrap()
                .unwrap_err(),
            "t.oscript needs a instrument to run on"
        );
        i.symbol = "SBIN".into();
        i.exchange = String::new();
        assert!(write(&mut c, "t.oscript", i.clone(), now())
            .unwrap()
            .unwrap_err()
            .contains("needs an exchange"));
        i.exchange = "NSE".into();
        i.product = "BO".into();
        assert!(write(&mut c, "t.oscript", i.clone(), now())
            .unwrap()
            .unwrap_err()
            .contains("not a product"));
        i.product = String::new();
        i.inputs = json!({"x": [1]});
        assert!(write(&mut c, "t.oscript", i.clone(), now())
            .unwrap()
            .unwrap_err()
            .contains("cannot be"));
        i.inputs = json!({"1x": 1});
        assert!(write(&mut c, "t.oscript", i, now())
            .unwrap()
            .unwrap_err()
            .contains("not the name"));
        assert!(write(&mut c, "x.js", input("SBIN", "5m"), now())
            .unwrap()
            .is_err());
        assert!(all_deployments(&c).unwrap().is_empty());
    }

    #[test]
    fn schedules_round_trip() {
        let c = db();
        let s = Schedule {
            start_time: "09:20".into(),
            stop_time: Some("15:10".into()),
            days: vec!["mon".into(), "tue".into()],
        };
        set_schedule(&c, "openscript_t", &s, now()).unwrap();
        set_schedule(&c, "bad name", &s, now()).unwrap();
        let all = all_schedules(&c).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all["openscript_t"], s);
        assert!(delete_schedule(&c, "openscript_t").unwrap());
        assert!(!delete_schedule(&c, "openscript_t").unwrap());
    }

    fn filled(action: &str, qty: i64, price: f64) -> OrderRow {
        OrderRow {
            id: 0,
            deployment: "d".into(),
            mode: "sandbox".into(),
            intent_id: None,
            tag: String::new(),
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            action: action.into(),
            quantity: qty,
            pricetype: "MARKET".into(),
            product: "MIS".into(),
            orderid: Some("1".into()),
            status: "filled".into(),
            filled_quantity: qty,
            average_price: Some(price),
            message: None,
            is_exit: false,
        }
    }

    #[test]
    fn holdings_net_with_average_cost() {
        let h = holdings(&[filled("BUY", 10, 100.0), filled("BUY", 10, 110.0)]);
        assert_eq!(h[0].quantity, 20);
        assert_eq!(h[0].average_price, Some(105.0));
        let h = holdings(&[filled("BUY", 10, 100.0), filled("SELL", 15, 120.0)]);
        assert_eq!(h[0].quantity, -5);
        assert_eq!(h[0].realized, 200.0);
        assert_eq!(h[0].average_price, Some(120.0));
        let h = holdings(&[filled("BUY", 10, 100.0), filled("SELL", 10, 90.0)]);
        assert_eq!(h[0].quantity, 0);
        assert_eq!(h[0].realized, -100.0);
    }

    #[test]
    fn orders_round_trip_and_prune() {
        let c = db();
        let id = insert_order(
            &c,
            &NewOrder {
                deployment: "d".into(),
                mode: "live".into(),
                intent_id: Some(3),
                symbol: "SBIN".into(),
                exchange: "NSE".into(),
                action: "BUY".into(),
                quantity: 5,
                pricetype: "MARKET".into(),
                product: "MIS".into(),
                ..Default::default()
            },
            now(),
        )
        .unwrap();
        placed(&c, id, Some("OID1"), "working", None, now()).unwrap();
        assert_eq!(open_orders(&c, "d").unwrap().len(), 1);
        progressed(&c, id, "filled", 5, Some(101.5), now()).unwrap();
        assert!(open_orders(&c, "d").unwrap().is_empty());
        let rows = orders_of(&c, "d").unwrap();
        assert_eq!(rows[0].orderid.as_deref(), Some("OID1"));
        assert_eq!(rows[0].average_price, Some(101.5));
        assert_eq!(
            prune_orders(&c, now() + chrono::Duration::days(181)).unwrap(),
            1
        );
    }
}
