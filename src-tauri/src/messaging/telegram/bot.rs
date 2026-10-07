//! Command and button handlers of the Telegram bot, with the web's texts
//! (`TelegramBotService.cmd_*`, `button_callback`, `_format_*`).
//!
//! Differences from the web, deliberate:
//! * Commands run against this desktop's own services (`messaging::openalgo`);
//!   the host URL given to `/link` is stored for the pages but never
//!   contacted, and the link reply says so.
//! * The desktop has no Python strategy host, so `/stoppython` always
//!   reports none running.
//! * The web's two `ℹ` characters are dropped (no emojis anywhere).

use super::api::{BotApi, TgError};
use super::chart::{self, Candle};
use super::db;
use crate::messaging::format::{
    comma2, comma_int, get_f64, get_str, py_float, py_float_str, py_int, signed2, title,
};
use crate::messaging::openalgo::{is_success, OpenAlgoClient};
use crate::messaging::ReplyClass;
use crate::state::AppState;
use chrono::{Duration, FixedOffset, TimeZone};
use serde_json::{json, Value};
use std::sync::Arc;

const RULE: &str = "━━━━━━━━━━━━━━━";
const LINK_FIRST: &str = "Please link your account first using /link";
const CRYPTO_BROKERS: &[&str] = &["deltaexchange"];

/// One Telegram user as the update carries it.
#[derive(Debug, Clone, Default)]
pub struct From {
    pub id: i64,
    pub first_name: String,
    pub last_name: String,
    pub username: String,
}

impl From {
    fn parse(v: Option<&Value>) -> Option<Self> {
        let v = v?;
        Some(Self {
            id: v.get("id")?.as_i64()?,
            first_name: v
                .get("first_name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
            last_name: v
                .get("last_name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
            username: v
                .get("username")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
        })
    }
}

fn menu_keyboard() -> Value {
    json!({"inline_keyboard": [
        [{"text": "Orderbook", "callback_data": "orderbook"}, {"text": "Tradebook", "callback_data": "tradebook"}],
        [{"text": "Positions", "callback_data": "positions"}, {"text": "Holdings", "callback_data": "holdings"}],
        [{"text": "Funds", "callback_data": "funds"}, {"text": "P&L", "callback_data": "pnl"}],
        [{"text": "Refresh", "callback_data": "menu"}],
    ]})
}

/// `[+]` / `[-]` / `[=]`.
fn sign_tag(x: f64) -> &'static str {
    if x > 0.0 {
        "[+]"
    } else if x < 0.0 {
        "[-]"
    } else {
        "[=]"
    }
}

fn data(resp: &Value) -> Value {
    resp.get("data").cloned().unwrap_or(Value::Null)
}

fn list_in(v: &Value, key: Option<&str>) -> Vec<Value> {
    let v = match key {
        Some(k) => v.get(k).cloned().unwrap_or(Value::Null),
        None => v.clone(),
    };
    v.as_array().cloned().unwrap_or_default()
}

/// `int(x)` with the web's fallback to the raw value.
fn int_or_raw(v: &Value, key: &str) -> String {
    match v.get(key) {
        None => "0".into(),
        Some(x) => match py_int(x) {
            Some(i) => i.to_string(),
            None => crate::messaging::format::py_str(x),
        },
    }
}

fn quantity_nonzero(p: &Value) -> bool {
    match p.get("quantity") {
        None => false,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Null) => true,
        Some(_) => true,
    }
}

/// `_format_pnl_funds`, shared by `/pnl` and the P&L button.
pub fn format_pnl_funds(resp: &Option<Value>, cs: &str) -> String {
    if !is_success(resp) {
        return "Failed to fetch P&L".into();
    }
    let f = data(resp.as_ref().unwrap_or(&Value::Null));
    let r = get_f64(&f, "m2mrealized", 0.0);
    let u = get_f64(&f, "m2munrealized", 0.0);
    let t = r + u;
    format!(
        "*PROFIT & LOSS*\n{RULE}\n\n{} *Realized P&L*\n└ {cs}{}\n\n{} *Unrealized P&L*\n└ {cs}{}\n\n{} *Total P&L*\n└ {cs}{}",
        sign_tag(r),
        comma2(r),
        sign_tag(u),
        comma2(u),
        sign_tag(t),
        comma2(t)
    )
}

fn order_status_tag(status: &str) -> &'static str {
    match status {
        "complete" => "[OK]",
        "open" => "[OPEN]",
        "rejected" => "[REJECTED]",
        _ => "[OTHER]",
    }
}

fn action_tag(v: &Value) -> &'static str {
    if v.get("action").and_then(Value::as_str) == Some("BUY") {
        "[BUY]"
    } else {
        "[SELL]"
    }
}

fn stat_int(stats: &Value, key: &str) -> i64 {
    stats.get(key).and_then(py_int).unwrap_or(0)
}

/// `/orderbook`.
pub fn format_orderbook_full(resp: &Value, cs: &str) -> String {
    let d = data(resp);
    let orders = list_in(&d, Some("orders"));
    if orders.is_empty() {
        return format!("*ORDERBOOK*\n{RULE}\n\nNo open orders");
    }
    let mut m = format!("*ORDERBOOK*\n{RULE}\n\n");
    for o in orders.iter().take(10) {
        let status = get_str(o, "order_status", "unknown");
        let price_str = match o.get("price").map(py_float).unwrap_or(Some(0.0)) {
            Some(p) => {
                if p == 0.0 && o.get("pricetype").and_then(Value::as_str) == Some("MARKET") {
                    "Market".to_string()
                } else {
                    format!("{cs}{}", py_float_str(p))
                }
            }
            None => format!("{cs}{}", get_str(o, "price", "0")),
        };
        m += &format!(
            "{} *{}* ({})\n{} {} {} @ {}\n├ Type: {}\n├ Product: {}\n├ Status: {}\n├ Time: {}\n",
            order_status_tag(&status),
            get_str(o, "symbol", "N/A"),
            get_str(o, "exchange", "N/A"),
            action_tag(o),
            get_str(o, "action", "N/A"),
            int_or_raw(o, "quantity"),
            price_str,
            get_str(o, "pricetype", "N/A"),
            get_str(o, "product", "N/A"),
            title(&status),
            get_str(o, "timestamp", "N/A"),
        );
        if let Some(t) = o.get("trigger_price").map(py_float).unwrap_or(Some(0.0)) {
            if t > 0.0 {
                m += &format!("├ Trigger: {cs}{}\n", py_float_str(t));
            }
        }
        m += &format!("└ Order ID: `{}`\n\n", get_str(o, "orderid", "N/A"));
    }
    if orders.len() > 10 {
        m += &format!("_... and {} more orders_\n\n", orders.len() - 10);
    }
    let stats = d.get("statistics").cloned().unwrap_or(Value::Null);
    if crate::messaging::format::truthy(Some(&stats)) {
        m += &format!(
            "*Summary*\n├ Total Orders: {}\n├ Open: {}\n├ Completed: {}\n├ Rejected: {}\n├ Buy Orders: {}\n└ Sell Orders: {}",
            orders.len(),
            stat_int(&stats, "total_open_orders"),
            stat_int(&stats, "total_completed_orders"),
            stat_int(&stats, "total_rejected_orders"),
            stat_int(&stats, "total_buy_orders"),
            stat_int(&stats, "total_sell_orders"),
        );
    }
    m
}

/// `/tradebook`.
pub fn format_tradebook_full(resp: &Value, cs: &str) -> String {
    let trades = list_in(resp, Some("data"));
    if trades.is_empty() {
        return format!("*TRADEBOOK*\n{RULE}\n\nNo trades executed today");
    }
    let mut m = format!("*TRADEBOOK*\n{RULE}\n\n");
    let (mut buy, mut sell) = (0.0, 0.0);
    for t in trades.iter().take(10) {
        let value = get_f64(t, "trade_value", 0.0);
        if t.get("action").and_then(Value::as_str) == Some("BUY") {
            buy += value;
        } else {
            sell += value;
        }
        let avg = match t.get("average_price").map(py_float).unwrap_or(Some(0.0)) {
            Some(a) => format!("{cs}{}", comma2(a)),
            None => format!("{cs}{}", get_str(t, "average_price", "0")),
        };
        m += &format!(
            "{} *{}* ({})\n├ {} {} @ {}\n├ Product: {}\n├ Value: {cs}{}\n├ Time: {}\n└ Order ID: `{}`\n\n",
            action_tag(t),
            get_str(t, "symbol", "N/A"),
            get_str(t, "exchange", "N/A"),
            get_str(t, "action", "N/A"),
            int_or_raw(t, "quantity"),
            avg,
            get_str(t, "product", "N/A"),
            comma2(value),
            get_str(t, "timestamp", "N/A"),
            get_str(t, "orderid", "N/A"),
        );
    }
    if trades.len() > 10 {
        m += &format!("_... and {} more trades_\n\n", trades.len() - 10);
    }
    m += &format!(
        "*Summary*\n├ Total Trades: {}\n├ Buy Value: {cs}{}\n└ Sell Value: {cs}{}",
        trades.len(),
        comma2(buy),
        comma2(sell)
    );
    m
}

/// `/positions`.
pub fn format_positions_full(resp: &Value, cs: &str) -> String {
    let positions = list_in(resp, Some("data"));
    if positions.is_empty() {
        return format!("*POSITIONS*\n{RULE}\n\nNo open positions");
    }
    let active: Vec<&Value> = positions.iter().filter(|p| quantity_nonzero(p)).collect();
    if active.is_empty() {
        return format!("*POSITIONS*\n{RULE}\n\nNo active positions");
    }
    let mut m = format!("*POSITIONS*\n{RULE}\n\n");
    let (mut long, mut short) = (0, 0);
    for p in active.iter().take(10) {
        let q = p.get("quantity").and_then(py_int).unwrap_or(0);
        let avg = match p.get("average_price") {
            None => 0.0,
            Some(v) => py_float(v).unwrap_or(0.0),
        };
        let (kind, tag) = if q > 0 {
            long += 1;
            ("LONG", "[LONG]")
        } else {
            short += 1;
            ("SHORT", "[SHORT]")
        };
        m += &format!(
            "{} *{}* ({})\n├ Position: {}\n├ Qty: {} ({})\n",
            tag,
            get_str(p, "symbol", "N/A"),
            get_str(p, "exchange", "N/A"),
            kind,
            q.abs(),
            get_str(p, "product", "N/A"),
        );
        if avg > 0.0 {
            m += &format!("├ Avg Price: {cs}{}\n", comma2(avg));
        }
        m += "\n";
    }
    if active.len() > 10 {
        m += &format!("_... and {} more positions_\n\n", active.len() - 10);
    }
    m += &format!(
        "*Summary*\n├ Active Positions: {}\n├ Long Positions: {}\n└ Short Positions: {}",
        active.len(),
        long,
        short
    );
    m
}

/// `/holdings`.
pub fn format_holdings_full(resp: &Value, cs: &str) -> String {
    let d = data(resp);
    let holdings = list_in(&d, Some("holdings"));
    if holdings.is_empty() {
        return format!("*HOLDINGS*\n{RULE}\n\nNo holdings found");
    }
    let mut m = format!("*HOLDINGS*\n{RULE}\n\n");
    for h in holdings.iter().take(10) {
        let pnl = get_f64(h, "pnl", 0.0);
        let pct = get_f64(h, "pnlpercent", 0.0);
        let q = h
            .get("quantity")
            .map(|v| py_int(v).unwrap_or(0))
            .unwrap_or(0);
        m += &format!(
            "{} *{}* ({})\n├ Product: {}\n├ Qty: {}\n└ P&L: {cs}{} ({}%)\n\n",
            sign_tag(pnl),
            get_str(h, "symbol", "N/A"),
            get_str(h, "exchange", "N/A"),
            get_str(h, "product", "CNC"),
            q,
            comma2(pnl),
            signed2(pct),
        );
    }
    if holdings.len() > 10 {
        m += &format!("_... and {} more holdings_\n\n", holdings.len() - 10);
    }
    let stats = d.get("statistics").cloned().unwrap_or(Value::Null);
    if crate::messaging::format::truthy(Some(&stats)) {
        let tp = get_f64(&stats, "totalprofitandloss", 0.0);
        m += &format!(
            "*Portfolio Summary*\n├ Current Value: {cs}{}\n├ Investment: {cs}{}\n└ {} P&L: {cs}{} ({}%)",
            comma2(get_f64(&stats, "totalholdingvalue", 0.0)),
            comma2(get_f64(&stats, "totalinvvalue", 0.0)),
            sign_tag(tp),
            comma2(tp),
            signed2(get_f64(&stats, "totalpnlpercentage", 0.0)),
        );
    }
    m
}

/// `/funds`.
pub fn format_funds_full(resp: &Value, cs: &str) -> String {
    let f = data(resp);
    let a = get_f64(&f, "availablecash", 0.0);
    let c = get_f64(&f, "collateral", 0.0);
    let u = get_f64(&f, "utiliseddebits", 0.0);
    format!(
        "*FUNDS*\n{RULE}\n\n*Available Cash*\n└ {cs}{}\n\n*Collateral*\n└ {cs}{}\n\n*Utilized Margin*\n└ {cs}{}\n\n*Total Balance*\n└ {cs}{}",
        comma2(a),
        comma2(c),
        comma2(u),
        comma2(a + c)
    )
}

/// The short formats the menu buttons use (`_format_*`).
pub fn format_short(kind: &str, resp: &Option<Value>, cs: &str) -> String {
    let fail = |what: &str| format!("Failed to fetch {}", what);
    if !is_success(resp) {
        return fail(kind);
    }
    let r = resp.as_ref().unwrap_or(&Value::Null);
    match kind {
        "orderbook" => {
            let orders = list_in(&data(r), Some("orders"));
            if orders.is_empty() {
                return format!("*ORDERBOOK*\n{RULE}\n\nNo open orders");
            }
            let mut m = format!("*ORDERBOOK*\n{RULE}\n\n");
            for o in orders.iter().take(10) {
                let status = get_str(o, "order_status", "unknown");
                let price = match o.get("price").map(py_float).unwrap_or(Some(0.0)) {
                    Some(p) => {
                        if p == 0.0 {
                            "Market".to_string()
                        } else {
                            format!("{cs}{}", py_float_str(p))
                        }
                    }
                    None => format!("{cs}{}", get_str(o, "price", "0")),
                };
                m += &format!(
                    "{} *{}* ({})\n{} {} {} @ {}\n└ Status: {}\n\n",
                    order_status_tag(&status),
                    get_str(o, "symbol", "N/A"),
                    get_str(o, "exchange", "N/A"),
                    action_tag(o),
                    get_str(o, "action", "N/A"),
                    int_or_raw(o, "quantity"),
                    price,
                    title(&status)
                );
            }
            if orders.len() > 10 {
                m += &format!("_... and {} more orders_", orders.len() - 10);
            }
            m
        }
        "tradebook" => {
            let trades = list_in(r, Some("data"));
            if trades.is_empty() {
                return format!("*TRADEBOOK*\n{RULE}\n\nNo trades executed today");
            }
            let mut m = format!("*TRADEBOOK*\n{RULE}\n\n");
            for t in trades.iter().take(10) {
                let avg = match t.get("average_price").map(py_float).unwrap_or(Some(0.0)) {
                    Some(a) => format!("{cs}{}", comma2(a)),
                    None => format!("{cs}{}", get_str(t, "average_price", "0")),
                };
                m += &format!(
                    "{} *{}* ({})\n├ {} {} @ {}\n└ Time: {}\n\n",
                    action_tag(t),
                    get_str(t, "symbol", "N/A"),
                    get_str(t, "exchange", "N/A"),
                    get_str(t, "action", "N/A"),
                    int_or_raw(t, "quantity"),
                    avg,
                    get_str(t, "timestamp", "N/A")
                );
            }
            if trades.len() > 10 {
                m += &format!("_... and {} more trades_", trades.len() - 10);
            }
            m
        }
        "positions" => {
            let positions = list_in(r, Some("data"));
            let active: Vec<&Value> = positions.iter().filter(|p| quantity_nonzero(p)).collect();
            if active.is_empty() {
                return format!("*POSITIONS*\n{RULE}\n\nNo active positions");
            }
            let mut m = format!("*POSITIONS*\n{RULE}\n\n");
            for p in active.iter().take(10) {
                let q = p.get("quantity").and_then(py_int).unwrap_or(0);
                let (tag, kind) = if q > 0 {
                    ("[LONG]", "LONG")
                } else {
                    ("[SHORT]", "SHORT")
                };
                m += &format!(
                    "{} *{}* ({})\n├ {}\n└ Qty: {}\n\n",
                    tag,
                    get_str(p, "symbol", "N/A"),
                    get_str(p, "exchange", "N/A"),
                    kind,
                    q.abs()
                );
            }
            if active.len() > 10 {
                m += &format!("_... and {} more positions_", active.len() - 10);
            }
            m
        }
        "holdings" => {
            let holdings = list_in(&data(r), Some("holdings"));
            if holdings.is_empty() {
                return format!("*HOLDINGS*\n{RULE}\n\nNo holdings found");
            }
            let mut m = format!("*HOLDINGS*\n{RULE}\n\n");
            for h in holdings.iter().take(10) {
                let (pnl, pct) = match (
                    h.get("pnl").map(py_float).unwrap_or(Some(0.0)),
                    h.get("pnlpercent").map(py_float).unwrap_or(Some(0.0)),
                ) {
                    (Some(a), Some(b)) => (a, b),
                    _ => (0.0, 0.0),
                };
                m += &format!(
                    "{} *{}*\n└ P&L: {cs}{} ({}%)\n\n",
                    sign_tag(pnl),
                    get_str(h, "symbol", "N/A"),
                    comma2(pnl),
                    signed2(pct)
                );
            }
            if holdings.len() > 10 {
                m += &format!("_... and {} more holdings_", holdings.len() - 10);
            }
            m
        }
        "funds" => {
            let f = data(r);
            let vals = (
                f.get("availablecash").map(py_float).unwrap_or(Some(0.0)),
                f.get("collateral").map(py_float).unwrap_or(Some(0.0)),
                f.get("utiliseddebits").map(py_float).unwrap_or(Some(0.0)),
            );
            let (a, c, u) = match vals {
                (Some(a), Some(c), Some(u)) => (a, c, u),
                _ => (0.0, 0.0, 0.0),
            };
            format!(
                "*FUNDS*\n{RULE}\n\nAvailable: {cs}{}\nCollateral: {cs}{}\nUtilized: {cs}{}\nTotal: {cs}{}",
                comma2(a),
                comma2(c),
                comma2(u),
                comma2(a + c)
            )
        }
        _ => "Unknown command".into(),
    }
}

/// `/quote`.
pub fn format_quote(symbol: &str, resp: &Value, cs: &str) -> String {
    let q = data(resp);
    let ltp = get_f64(&q, "ltp", 0.0);
    let prev = get_f64(&q, "prev_close", ltp);
    let change = ltp - prev;
    let pct = if prev > 0.0 {
        change / prev * 100.0
    } else {
        0.0
    };
    let vol = q.get("volume").map(|v| py_int(v).unwrap_or(0)).unwrap_or(0);
    format!(
        "*{symbol}*\n{RULE}\n\n{} Price: {cs}{}\n├ Change: {cs}{} ({}%)\n├ Open: {cs}{}\n├ High: {cs}{}\n├ Low: {cs}{}\n├ Prev Close: {cs}{}\n└ Volume: {}",
        sign_tag(change),
        comma2(ltp),
        signed2(change),
        signed2(pct),
        comma2(get_f64(&q, "open", 0.0)),
        comma2(get_f64(&q, "high", 0.0)),
        comma2(get_f64(&q, "low", 0.0)),
        comma2(prev),
        comma_int(vol)
    )
}

pub const HELP_TEXT: &str = "
*Available Commands:*

*Account Management:*
/link `<api_key> <host_url>` - Link your OpenAlgo account
/unlink - Unlink your account
/status - Check connection status

*Trading Information:*
/orderbook - View open orders
/tradebook - View executed trades
/positions - View current positions
/holdings - View holdings
/funds - View account funds
/pnl - View P&L (realized & unrealized)
/quote `<symbol> [exchange]` - Get stock quote

*Charts:*
/chart `<symbol> [exchange] [type] [interval] [days]`
  • type: intraday or daily (default: both)
  • interval: 1m, 5m, 15m, 30m, 1h, D (default: 5m for intraday, D for daily)
  • days: number of days (default: 5 for intraday, 252 for daily)

*Remote Actions:*
/closeall - Close all open positions (with confirmation)
/stoppython - Stop running Python strategies (with confirmation)
/mode - View or toggle trading mode (Live / Analyze)

*Navigation:*
/menu - Show interactive menu
/help - Show this help message

*Examples:*
`/quote RELIANCE`
`/quote NIFTY NSE_INDEX`
`/chart RELIANCE`
`/chart RELIANCE NSE intraday 15m 10`
`/chart NIFTY NSE_INDEX daily D 100`
";

/// Turn history rows into candles in IST (the SDK's DataFrame index).
pub fn candles_from_history(resp: &Value) -> Vec<Candle> {
    let ist = FixedOffset::east_opt(5 * 3600 + 1800)
        .unwrap_or_else(|| FixedOffset::east_opt(0).expect("utc"));
    list_in(resp, Some("data"))
        .iter()
        .filter_map(|r| {
            let ts = r.get("timestamp").and_then(py_float)?;
            let time = ist.timestamp_opt(ts as i64, 0).single()?;
            Some(Candle {
                time,
                open: r.get("open").and_then(py_float)?,
                high: r.get("high").and_then(py_float)?,
                low: r.get("low").and_then(py_float)?,
                close: r.get("close").and_then(py_float)?,
                volume: r.get("volume").and_then(py_float).unwrap_or(0.0),
            })
        })
        .collect()
}

/// Commands anyone may send before linking.
pub const OPEN_COMMANDS: &[&str] = &["start", "help", "link"];
/// Commands that need a linked user in a private chat.
pub const LINKED_COMMANDS: &[&str] = &[
    "unlink",
    "status",
    "orderbook",
    "tradebook",
    "positions",
    "holdings",
    "funds",
    "pnl",
    "quote",
    "chart",
    "menu",
    "closeall",
    "stoppython",
    "mode",
];
/// Every button the bot sends (`spy_` and `csy_` are prefixes); all need a
/// linked user in a private chat.
pub const CALLBACKS: &[&str] = &[
    "cancel_action",
    "confirm_closeall",
    "confirm_closeall_with_strategies",
    "spy_",
    "csy_",
    "mode_live",
    "mode_analyze",
    "menu",
    "orderbook",
    "tradebook",
    "positions",
    "holdings",
    "funds",
    "pnl",
];

pub enum Request<'a> {
    Command(&'a str),
    Callback(&'a str),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Gate {
    Allow,
    /// Answer with this text and do nothing else.
    Deny(&'static str),
    /// Not a command or button the bot knows.
    Ignore,
}

/// The one authorization gate every command and button passes. Only
/// `/start`, `/help` and `/link` run before linking; everything else needs
/// the sender to be a linked user writing in a private chat with the bot
/// (the chat id is the sender's id), so nobody else in a group can press a
/// linked user's buttons.
pub fn gate(req: Request<'_>, linked: bool, chat_id: i64, from_id: i64) -> Gate {
    // Nothing is answered outside a private chat with the sender.
    if chat_id != from_id {
        return Gate::Ignore;
    }
    let known = match req {
        Request::Command(c) => {
            if OPEN_COMMANDS.contains(&c) {
                return Gate::Allow;
            }
            LINKED_COMMANDS.contains(&c)
        }
        Request::Callback(d) => CALLBACKS.iter().any(|n| {
            if n.ends_with('_') {
                d.starts_with(n)
            } else {
                d == *n
            }
        }),
    };
    if !known {
        return Gate::Ignore;
    }
    if !linked {
        return Gate::Deny(LINK_FIRST);
    }
    Gate::Allow
}

/// The handlers of one polling run.
pub struct Bot {
    api: BotApi,
}

impl Bot {
    pub fn new(api: BotApi) -> Self {
        Self { api }
    }

    /// Send, retrying as plain text when Telegram cannot parse the Markdown.
    async fn send(&self, chat: i64, text: &str, md: bool, markup: Option<Value>) -> Option<Value> {
        let mut r = self.api.send_message(chat, text, md, markup.clone()).await;
        if md && matches!(&r, Err(e) if e.is_parse_error()) {
            r = self.api.send_message(chat, text, false, markup).await;
        }
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!("Telegram reply failed: {}", e);
                None
            }
        }
    }

    async fn edit(&self, chat: i64, msg: i64, text: &str, md: bool, markup: Option<Value>) {
        let mut r = self
            .api
            .edit_message_text(chat, msg, text, md, markup.clone())
            .await;
        if md && matches!(&r, Err(e) if e.is_parse_error()) {
            r = self
                .api
                .edit_message_text(chat, msg, text, false, markup)
                .await;
        }
        if let Err(e) = r {
            if !matches!(&e, TgError::Api { description, .. } if description.contains("not modified"))
            {
                tracing::warn!("Telegram edit failed: {}", e);
            }
        }
    }

    fn log(ctx: &AppState, telegram_id: i64, command: &str, chat: i64) {
        let r = ctx
            .sqlite
            .conn()
            .and_then(|c| db::log_command(&c, telegram_id, command, Some(chat), ctx.now()));
        if let Err(e) = r {
            tracing::error!("Could not log a Telegram command: {}", e);
        }
    }

    fn user(ctx: &AppState, id: i64) -> Option<db::TgUser> {
        ctx.sqlite
            .conn()
            .and_then(|c| db::get_user(&c, id))
            .unwrap_or_else(|e| {
                tracing::error!("Could not read a Telegram user: {}", e);
                None
            })
    }

    fn cs(user: &db::TgUser) -> &'static str {
        if CRYPTO_BROKERS.contains(&user.broker.as_deref().unwrap_or("")) {
            "$"
        } else {
            "₹"
        }
    }

    /// The linked user's client (web `_get_sdk_client`).
    fn client(ctx: &Arc<AppState>, id: i64) -> Option<OpenAlgoClient> {
        let creds = ctx
            .sqlite
            .conn()
            .and_then(|c| db::user_credentials(&c, &ctx.security, id))
            .ok()
            .flatten()?;
        Some(OpenAlgoClient::new(ctx.clone(), creds.api_key?))
    }

    pub async fn handle_update(&mut self, ctx: &Arc<AppState>, u: &Value) {
        if let Some(q) = u.get("callback_query") {
            self.on_callback(ctx, q).await;
            return;
        }
        let Some(msg) = u.get("message") else {
            return;
        };
        let Some(text) = msg.get("text").and_then(Value::as_str) else {
            return;
        };
        let Some(chat) = msg.pointer("/chat/id").and_then(Value::as_i64) else {
            return;
        };
        let Some(from) = From::parse(msg.get("from")) else {
            return;
        };
        let Some(rest) = text.strip_prefix('/') else {
            return;
        };
        let mut parts = rest.split_whitespace();
        let cmd = parts
            .next()
            .unwrap_or("")
            .split('@')
            .next()
            .unwrap_or("")
            .to_lowercase();
        let args: Vec<String> = parts.map(String::from).collect();
        let linked = Self::user(ctx, from.id).is_some();
        // Linked users get members' caps; before linking each open command
        // is answered once an hour (link attempts have their own throttle).
        let (who, class) = if linked {
            (format!("tg:{}", from.id), ReplyClass::Member)
        } else if cmd == "link" {
            (format!("tg:{}:link", from.id), ReplyClass::Member)
        } else {
            (format!("tg:{}:{}", from.id, cmd), ReplyClass::Stranger)
        };
        let deny_key = format!("tg:{}:deny", from.id);
        let replies = &ctx.messaging.telegram_replies;
        match gate(Request::Command(&cmd), linked, chat, from.id) {
            Gate::Ignore => return,
            Gate::Deny(msg) => {
                if replies.allow(&deny_key, ReplyClass::Stranger, ctx.now()) {
                    self.send(chat, msg, false, None).await;
                }
                return;
            }
            Gate::Allow => {
                if !replies.allow(&who, class, ctx.now()) {
                    return;
                }
            }
        }
        self.on_command(ctx, &cmd, &args, chat, &from).await;
    }

    async fn on_command(
        &mut self,
        ctx: &Arc<AppState>,
        cmd: &str,
        args: &[String],
        chat: i64,
        from: &From,
    ) {
        match cmd {
            "start" => {
                let text = if Self::user(ctx, from.id).is_some() {
                    format!(
                        "Welcome back, {}! \n\nYour account is linked. Use /menu to see available options.",
                        from.first_name
                    )
                } else {
                    format!(
                        "Welcome to OpenAlgo Bot, {}! \n\nTo get started, link your OpenAlgo account:\n`/link <api_key> <host_url>`\n\nExample:\n`/link your_api_key_here http://127.0.0.1:5000`\n\nUse /help to see all available commands.",
                        from.first_name
                    )
                };
                self.send(chat, &text, true, None).await;
                Self::log(ctx, from.id, "start", chat);
            }
            "help" => {
                self.send(chat, HELP_TEXT, true, None).await;
                Self::log(ctx, from.id, "help", chat);
            }
            "link" => self.cmd_link(ctx, args, chat, from).await,
            "unlink" => {
                let ok = ctx
                    .sqlite
                    .conn()
                    .and_then(|c| db::delete_user(&c, from.id, ctx.now()))
                    .unwrap_or(false);
                let text = if ok {
                    "Account unlinked successfully.\nYour data has been removed."
                } else {
                    "No linked account found."
                };
                self.send(chat, text, true, None).await;
                Self::log(ctx, from.id, "unlink", chat);
            }
            "status" => self.cmd_status(ctx, chat, from).await,
            "orderbook" | "tradebook" | "positions" | "holdings" | "funds" | "pnl" => {
                self.cmd_book(ctx, cmd, chat, from).await
            }
            "quote" => self.cmd_quote(ctx, args, chat, from).await,
            "chart" => self.cmd_chart(ctx, args, chat, from).await,
            "menu" => {
                if Self::user(ctx, from.id).is_none() {
                    self.send(chat, LINK_FIRST, false, None).await;
                    return;
                }
                self.send(
                    chat,
                    "*OpenAlgo Trading Menu*\nSelect an option below:",
                    true,
                    Some(menu_keyboard()),
                )
                .await;
                Self::log(ctx, from.id, "menu", chat);
            }
            "closeall" => {
                if Self::user(ctx, from.id).is_none() {
                    self.send(chat, LINK_FIRST, false, None).await;
                    return;
                }
                let kb = json!({"inline_keyboard": [
                    [{"text": "Yes, close all", "callback_data": "confirm_closeall"}],
                    [{"text": "Close all + Stop strategies", "callback_data": "confirm_closeall_with_strategies"}],
                    [{"text": "Cancel", "callback_data": "cancel_action"}],
                ]});
                let text = format!(
                    "*Close All Positions*\n{RULE}\n\nThis will close ALL open positions across all strategies.\n\nChoose *Close all + Stop strategies* to also stop every running Python strategy after closing positions.\n\nAre you sure?"
                );
                self.send(chat, &text, true, Some(kb)).await;
                Self::log(ctx, from.id, "closeall", chat);
            }
            "stoppython" => {
                if Self::user(ctx, from.id).is_none() {
                    self.send(chat, LINK_FIRST, false, None).await;
                    return;
                }
                // No Python strategy host on the desktop.
                self.send(chat, "*No Python strategies running.*", true, None)
                    .await;
                Self::log(ctx, from.id, "stoppython", chat);
            }
            "mode" => {
                if Self::user(ctx, from.id).is_none() {
                    self.send(chat, LINK_FIRST, false, None).await;
                    return;
                }
                let analyze = ctx.sqlite.get_analyze_mode().unwrap_or(false);
                let (current, label, data) = if analyze {
                    ("Analyze Mode", "Switch to Live", "mode_live")
                } else {
                    ("Live Mode", "Switch to Analyze", "mode_analyze")
                };
                let text = format!(
                    "*Trading Mode*\n{RULE}\n\nCurrent: {current}\n\n• *Live Mode* — Orders execute with real broker\n• *Analyze Mode* — Sandbox mode (no real orders)\n"
                );
                self.send(
                    chat,
                    &text,
                    true,
                    Some(json!({"inline_keyboard": [[{"text": label, "callback_data": data}]]})),
                )
                .await;
                Self::log(ctx, from.id, "mode", chat);
            }
            _ => {}
        }
    }

    async fn cmd_link(&self, ctx: &Arc<AppState>, args: &[String], chat: i64, from: &From) {
        if args.len() != 2 {
            self.send(
                chat,
                "Invalid format\nUsage: `/link <api_key> <host_url>`\nExample: `/link your_api_key http://127.0.0.1:5000`",
                true,
                None,
            )
            .await;
            return;
        }
        let api_key = crate::security::Secret::new(args[0].clone());
        let host_url = args[1].trim_end_matches('/').to_string();
        // Validated in-process, exactly as a funds call would be; the host
        // given is stored for the pages but never contacted.
        // Key guesses are throttled per sender, as /api/v1 throttles them per
        // address; over the limit the key is not even tried.
        let who = format!("tg:{}", from.id);
        if !ctx.messaging.link_throttle.allowed(&who, ctx.now()) {
            tracing::warn!("Telegram link attempts throttled for a sender");
            self.send(chat, crate::messaging::TOO_MANY_LINK_ATTEMPTS, false, None)
                .await;
            Self::log(ctx, from.id, "link", chat);
            return;
        }
        let client = OpenAlgoClient::new(ctx.clone(), api_key.clone());
        let resp = client.funds().await;
        if !is_success(&resp) {
            ctx.messaging.link_throttle.fail(&who, ctx.now());
            tracing::warn!("Telegram link attempt with an API key that did not validate");
            self.send(
                chat,
                "Failed to validate API key.\nPlease check your credentials and try again.",
                true,
                None,
            )
            .await;
            Self::log(ctx, from.id, "link", chat);
            return;
        }
        ctx.messaging.link_throttle.clear(&who);
        // The key is this desktop's own (it passed the check), so the
        // account is this desktop's single user.
        let username =
            crate::messaging::account_username(ctx).unwrap_or_else(|| format!("user_{}", from.id));
        let broker = ctx
            .get_broker_session()
            .map(|b| b.broker_id)
            .unwrap_or_else(|| "default".into());
        let res = ctx.sqlite.conn().and_then(|c| {
            db::create_or_update_user(
                &c,
                &ctx.security,
                &db::Link {
                    telegram_id: from.id,
                    username: &username,
                    api_key: Some(api_key.expose()),
                    host_url: Some(&host_url),
                    first_name: &from.first_name,
                    last_name: &from.last_name,
                    telegram_username: &from.username,
                    broker: &broker,
                },
                ctx.now(),
            )
        });
        match res {
            Ok(()) => {
                tracing::info!("Telegram user linked to the OpenAlgo account");
                self.send(
                    chat,
                    "Account linked successfully!\nYou can now use all bot features.\nType /menu to see available options.\n\nCommands run on this OpenAlgo Desktop; the host address you gave is not used.",
                    true,
                    None,
                )
                .await;
            }
            Err(e) => {
                tracing::error!("Could not save the Telegram link: {}", e);
                self.send(
                    chat,
                    "Failed to link account.\nThe link could not be saved. Try again.",
                    true,
                    None,
                )
                .await;
            }
        }
        Self::log(ctx, from.id, "link", chat);
    }

    async fn cmd_status(&self, ctx: &Arc<AppState>, chat: i64, from: &From) {
        let Some(user) = Self::user(ctx, from.id) else {
            self.send(
                chat,
                "No linked account found.\nUse /link to connect your OpenAlgo account.",
                true,
                None,
            )
            .await;
            Self::log(ctx, from.id, "status", chat);
            return;
        };
        let status = match Self::client(ctx, from.id) {
            Some(c) => {
                if is_success(&c.funds().await) {
                    "Connected"
                } else {
                    "Connection Failed"
                }
            }
            None => "Client Error",
        };
        let display = user
            .telegram_username
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| Some(user.openalgo_username.clone()).filter(|s| !s.is_empty()))
            .unwrap_or_else(|| "N/A".into());
        let text = format!(
            "*Account Status*\n{RULE}\nUser: {}\nStatus: {}\nHost: {}\nLinked: {}",
            display,
            status,
            user.host_url
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "N/A".into()),
            user.created_at.clone().unwrap_or_else(|| "N/A".into()),
        );
        self.send(chat, &text, true, None).await;
        Self::log(ctx, from.id, "status", chat);
    }

    async fn cmd_book(&self, ctx: &Arc<AppState>, cmd: &str, chat: i64, from: &From) {
        let Some(user) = Self::user(ctx, from.id) else {
            self.send(chat, LINK_FIRST, false, None).await;
            return;
        };
        let cs = Self::cs(&user);
        let Some(client) = Self::client(ctx, from.id) else {
            self.send(chat, "Failed to connect to OpenAlgo", false, None)
                .await;
            return;
        };
        let (resp, what) = match cmd {
            "orderbook" => (client.orderbook().await, "orderbook"),
            "tradebook" => (client.tradebook().await, "tradebook"),
            "positions" => (client.positionbook().await, "positions"),
            "holdings" => (client.holdings().await, "holdings"),
            "funds" => (client.funds().await, "funds"),
            _ => (client.funds().await, "P&L"),
        };
        if !is_success(&resp) {
            self.send(chat, &format!("Failed to fetch {}", what), false, None)
                .await;
            return;
        }
        let r = resp.clone().unwrap_or(Value::Null);
        let text = match cmd {
            "orderbook" => format_orderbook_full(&r, cs),
            "tradebook" => format_tradebook_full(&r, cs),
            "positions" => format_positions_full(&r, cs),
            "holdings" => format_holdings_full(&r, cs),
            "funds" => format_funds_full(&r, cs),
            _ => format_pnl_funds(&resp, cs),
        };
        self.send(chat, &text, true, None).await;
        Self::log(ctx, from.id, cmd, chat);
    }

    async fn cmd_quote(&self, ctx: &Arc<AppState>, args: &[String], chat: i64, from: &From) {
        let Some(user) = Self::user(ctx, from.id) else {
            self.send(chat, LINK_FIRST, false, None).await;
            return;
        };
        if args.is_empty() {
            self.send(
                chat,
                "Usage: /quote <symbol> [exchange]\nExample: /quote RELIANCE\nExample: /quote NIFTY NSE_INDEX",
                true,
                None,
            )
            .await;
            return;
        }
        let cs = Self::cs(&user);
        let symbol = args[0].to_uppercase();
        let exchange = args
            .get(1)
            .map(|s| s.to_uppercase())
            .unwrap_or_else(|| "NSE".into());
        let Some(client) = Self::client(ctx, from.id) else {
            self.send(chat, "Failed to connect to OpenAlgo", false, None)
                .await;
            return;
        };
        let resp = client.quotes(&symbol, &exchange).await;
        if !is_success(&resp) {
            self.send(
                chat,
                &format!("Failed to fetch quote for {}", symbol),
                false,
                None,
            )
            .await;
            return;
        }
        let text = format_quote(&symbol, resp.as_ref().unwrap_or(&Value::Null), cs);
        self.send(chat, &text, true, None).await;
        Self::log(ctx, from.id, "quote", chat);
    }

    async fn chart_png(
        ctx: &Arc<AppState>,
        id: i64,
        symbol: &str,
        exchange: &str,
        interval: &str,
        days: i64,
        daily: bool,
    ) -> Option<Vec<u8>> {
        let client = Self::client(ctx, id)?;
        let now = ctx.now().with_timezone(&chrono::Local).date_naive();
        let back = if daily {
            (days as f64 * 1.5) as i64
        } else {
            days
        };
        let start = now - Duration::days(back);
        let resp = client
            .history(symbol, exchange, interval, start, now)
            .await?;
        if resp.get("status").and_then(Value::as_str) != Some("success") {
            tracing::warn!("Chart history was not available");
            return None;
        }
        let mut c = candles_from_history(&resp);
        if daily {
            let keep = days.max(0) as usize;
            if c.len() > keep {
                c.drain(..c.len() - keep);
            }
            let title = format!("{} - Daily Chart ({} Days)", symbol, days);
            let png = tokio::task::spawn_blocking(move || chart::render(&c, &title, "%d %b", 10));
            png.await.ok().flatten()
        } else {
            let title = format!("{} - {} Day Intraday ({})", symbol, days, interval);
            let png =
                tokio::task::spawn_blocking(move || chart::render(&c, &title, "%d %b %H:%M", 8));
            png.await.ok().flatten()
        }
    }

    async fn cmd_chart(&self, ctx: &Arc<AppState>, args: &[String], chat: i64, from: &From) {
        if Self::user(ctx, from.id).is_none() {
            self.send(chat, LINK_FIRST, false, None).await;
            return;
        }
        if args.is_empty() {
            self.send(
                chat,
                "Usage: /chart <symbol> [exchange] [type] [interval] [days]\nType: intraday (default), daily, or both\n\nExamples:\n/chart RELIANCE - 5m intraday chart\n/chart RELIANCE NSE intraday 15m 10\n/chart NIFTY NSE_INDEX daily D 100\n/chart RELIANCE NSE both - Both charts",
                true,
                None,
            )
            .await;
            return;
        }
        let symbol = args[0].to_uppercase();
        let exchange = args
            .get(1)
            .map(|s| s.to_uppercase())
            .unwrap_or_else(|| "NSE".into());
        let kind = args
            .get(2)
            .map(|s| s.to_lowercase())
            .unwrap_or_else(|| "intraday".into());
        let interval = args.get(3).cloned();
        let days = match args.get(4) {
            None => None,
            Some(d) => match d.trim().parse::<i64>() {
                Ok(v) => Some(v),
                Err(_) => {
                    // The web's handler raises here and its error handler answers.
                    self.send(
                        chat,
                        "An error occurred. Please try again later.",
                        false,
                        None,
                    )
                    .await;
                    return;
                }
            },
        };
        let loading = self
            .send(chat, "Generating charts... Please wait.", false, None)
            .await;
        let mut photos: Vec<(Vec<u8>, String)> = Vec::new();
        if matches!(kind.as_str(), "both" | "intraday" | "i") {
            let iv = interval.clone().unwrap_or_else(|| "5m".into());
            let d = days.filter(|d| *d != 0).unwrap_or(5);
            if let Some(png) =
                Self::chart_png(ctx, from.id, &symbol, &exchange, &iv, d, false).await
            {
                photos.push((
                    png,
                    format!("{} - {} Day Intraday Chart ({} intervals)", symbol, d, iv),
                ));
            }
        }
        if matches!(kind.as_str(), "both" | "daily" | "d") {
            let (iv, d) = if kind == "both" {
                ("D".to_string(), 252)
            } else {
                (
                    interval.clone().unwrap_or_else(|| "D".into()),
                    days.filter(|d| *d != 0).unwrap_or(252),
                )
            };
            if let Some(png) = Self::chart_png(ctx, from.id, &symbol, &exchange, &iv, d, true).await
            {
                photos.push((png, format!("{} - Daily Chart ({} days)", symbol, d)));
            }
        }
        if let Some(mid) = loading
            .as_ref()
            .and_then(|m| m.get("message_id"))
            .and_then(Value::as_i64)
        {
            let _ = self.api.delete_message(chat, mid).await;
        }
        let sent = match photos.len() {
            0 => {
                self.send(
                    chat,
                    &format!("Failed to generate charts for {}", symbol),
                    false,
                    None,
                )
                .await;
                Ok(Value::Null)
            }
            1 => {
                let (png, cap) = photos.remove(0);
                self.api.send_photo(chat, png, &cap).await
            }
            _ => self.api.send_media_group(chat, photos).await,
        };
        if let Err(e) = sent {
            tracing::warn!("Sending the chart failed: {}", e);
            self.send(
                chat,
                "Error generating charts. Please try again.",
                false,
                None,
            )
            .await;
        }
        Self::log(ctx, from.id, "chart", chat);
    }

    async fn on_callback(&mut self, ctx: &Arc<AppState>, q: &Value) {
        if let Some(id) = q.get("id").and_then(Value::as_str) {
            let _ = self.api.answer_callback_query(id).await;
        }
        let Some(from) = From::parse(q.get("from")) else {
            return;
        };
        let Some(chat) = q.pointer("/message/chat/id").and_then(Value::as_i64) else {
            return;
        };
        let mid = q
            .pointer("/message/message_id")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let data = q
            .get("data")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let linked = Self::user(ctx, from.id).is_some();
        let who = format!("tg:{}", from.id);
        let replies = &ctx.messaging.telegram_replies;
        match gate(Request::Callback(&data), linked, chat, from.id) {
            Gate::Ignore => return,
            Gate::Deny(msg) => {
                if replies.allow(
                    &format!("tg:{}:deny", from.id),
                    ReplyClass::Stranger,
                    ctx.now(),
                ) {
                    self.send(chat, msg, false, None).await;
                }
                return;
            }
            Gate::Allow => {
                if !replies.allow(&who, ReplyClass::Member, ctx.now()) {
                    return;
                }
            }
        }

        match data.as_str() {
            "cancel_action" => {
                self.edit(chat, mid, "Action cancelled.", false, None).await;
            }
            "confirm_closeall" | "confirm_closeall_with_strategies" => {
                let with = data == "confirm_closeall_with_strategies";
                if Self::user(ctx, from.id).is_none() {
                    self.edit(chat, mid, LINK_FIRST, false, None).await;
                    return;
                }
                let Some(client) = Self::client(ctx, from.id) else {
                    self.edit(chat, mid, "Failed to connect to OpenAlgo", false, None)
                        .await;
                    return;
                };
                let working = if with {
                    "Closing all positions and stopping strategies..."
                } else {
                    "Closing all positions..."
                };
                self.edit(chat, mid, working, false, None).await;
                let resp = client.closeposition().await;
                let message = |r: &Option<Value>, d: &str| {
                    r.as_ref()
                        .and_then(|v| v.get("message"))
                        .map(crate::messaging::format::py_str)
                        .unwrap_or_else(|| d.into())
                };
                if with {
                    let close = if is_success(&resp) {
                        message(&resp, "All positions closed")
                    } else {
                        let err = if resp.is_some() {
                            message(&resp, "Unknown error")
                        } else {
                            "No response".into()
                        };
                        format!("Failed to close positions: {}", err)
                    };
                    let text = format!(
                        "*Close All + Stop Strategies*\n{RULE}\n\n{}\n\nNo Python strategies were running.",
                        close
                    );
                    self.send(chat, &text, true, None).await;
                } else if is_success(&resp) {
                    let text = format!(
                        "*Positions Closed*\n{RULE}\n\n{}",
                        message(&resp, "All positions closed")
                    );
                    self.send(chat, &text, true, None).await;
                } else {
                    let err = if resp.is_some() {
                        message(&resp, "Unknown error")
                    } else {
                        "No response".into()
                    };
                    self.send(
                        chat,
                        &format!("*Failed to close positions*\n\n{}", err),
                        true,
                        None,
                    )
                    .await;
                }
                Self::log(ctx, from.id, &data, chat);
            }
            d if d.starts_with("spy_") || d.starts_with("csy_") => {
                if d == "csy_all" {
                    self.edit(chat, mid, "Stopping all running strategies...", false, None)
                        .await;
                    self.send(chat, "No Python strategies were running.", false, None)
                        .await;
                    Self::log(ctx, from.id, "confirm_stoppython_all", chat);
                } else {
                    // The selection list is always empty on the desktop.
                    self.edit(
                        chat,
                        mid,
                        "Selection expired. Please run /stoppython again.",
                        false,
                        None,
                    )
                    .await;
                }
            }
            "mode_live" | "mode_analyze" => {
                let requested = data == "mode_analyze";
                match crate::services::AnalyzerService::set_mode(ctx, requested).await {
                    Ok(s) => {
                        crate::messaging::emit(
                            ctx,
                            "app_mode_changed",
                            json!({"analyze_mode": s.analyze_mode}),
                        )
                        .await;
                        let label = if s.analyze_mode {
                            "Analyze Mode"
                        } else {
                            "Live Mode"
                        };
                        self.edit(
                            chat,
                            mid,
                            &format!("*Mode Changed*\n{RULE}\n\nNow in: {}", label),
                            true,
                            None,
                        )
                        .await;
                    }
                    Err(e) => {
                        tracing::error!("Telegram mode change failed: {}", e);
                        self.edit(
                            chat,
                            mid,
                            "Failed to change mode. Check server logs.",
                            false,
                            None,
                        )
                        .await;
                    }
                }
                Self::log(ctx, from.id, &data, chat);
            }
            "menu" => {
                let ts = crate::messaging::alerts::local_time(ctx);
                self.edit(
                    chat,
                    mid,
                    &format!(
                        "*OpenAlgo Trading Menu*\nSelect an option below:\n_Updated: {}_",
                        ts
                    ),
                    true,
                    Some(menu_keyboard()),
                )
                .await;
            }
            other => {
                let Some(user) = Self::user(ctx, from.id) else {
                    self.send(chat, LINK_FIRST, false, None).await;
                    return;
                };
                let cs = Self::cs(&user);
                let Some(client) = Self::client(ctx, from.id) else {
                    self.send(chat, "Failed to connect to OpenAlgo", false, None)
                        .await;
                    return;
                };
                let text = match other {
                    "orderbook" => format_short("orderbook", &client.orderbook().await, cs),
                    "tradebook" => format_short("tradebook", &client.tradebook().await, cs),
                    "positions" => format_short("positions", &client.positionbook().await, cs),
                    "holdings" => format_short("holdings", &client.holdings().await, cs),
                    "funds" => format_short("funds", &client.funds().await, cs),
                    "pnl" => format_pnl_funds(&client.funds().await, cs),
                    _ => "Unknown command".into(),
                };
                self.send(chat, &text, true, None).await;
                Self::log(ctx, from.id, other, chat);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_and_button_passes_the_gate() {
        for c in LINKED_COMMANDS {
            assert_eq!(
                gate(Request::Command(c), false, 5, 5),
                Gate::Deny(LINK_FIRST),
                "{}",
                c
            );
            assert_eq!(
                gate(Request::Command(c), true, -100, 5),
                Gate::Ignore,
                "{}",
                c
            );
            assert_eq!(gate(Request::Command(c), true, 5, 5), Gate::Allow, "{}", c);
        }
        for c in OPEN_COMMANDS {
            assert_eq!(gate(Request::Command(c), false, -100, 5), Gate::Ignore);
            assert_eq!(gate(Request::Command(c), false, 5, 5), Gate::Allow);
        }
        for n in CALLBACKS {
            let d = if n.ends_with('_') {
                format!("{}0", n)
            } else {
                n.to_string()
            };
            assert_eq!(
                gate(Request::Callback(&d), false, 5, 5),
                Gate::Deny(LINK_FIRST),
                "{}",
                d
            );
            assert_eq!(
                gate(Request::Callback(&d), true, -100, 5),
                Gate::Ignore,
                "{}",
                d
            );
            assert_eq!(
                gate(Request::Callback(&d), true, 5, 5),
                Gate::Allow,
                "{}",
                d
            );
        }
        assert_eq!(gate(Request::Command("nope"), true, 5, 5), Gate::Ignore);
        assert_eq!(gate(Request::Callback("nope"), true, 5, 5), Gate::Ignore);
    }

    #[test]
    fn funds_and_pnl_texts() {
        let r = json!({"status": "success", "data": {"availablecash": "1234.5", "collateral": 0,
            "utiliseddebits": 10, "m2mrealized": -5, "m2munrealized": "2.5"}});
        assert_eq!(
            format_funds_full(&r, "₹"),
            format!("*FUNDS*\n{RULE}\n\n*Available Cash*\n└ ₹1,234.50\n\n*Collateral*\n└ ₹0.00\n\n*Utilized Margin*\n└ ₹10.00\n\n*Total Balance*\n└ ₹1,234.50")
        );
        let p = format_pnl_funds(&Some(r), "$");
        assert!(p.contains("[-] *Realized P&L*\n└ $-5.00"));
        assert!(p.contains("[-] *Total P&L*\n└ $-2.50"));
        assert_eq!(format_pnl_funds(&None, "$"), "Failed to fetch P&L");
    }

    #[test]
    fn orderbook_and_quote_texts() {
        let r = json!({"status": "success", "data": {"orders": [
            {"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": "10", "price": 0,
             "pricetype": "MARKET", "product": "MIS", "order_status": "complete", "timestamp": "09:15",
             "trigger_price": 0, "orderid": "1"}],
            "statistics": {"total_open_orders": 0, "total_completed_orders": 1}}});
        let t = format_orderbook_full(&r, "₹");
        assert!(t.starts_with("*ORDERBOOK*"));
        assert!(t.contains("[OK] *SBIN* (NSE)\n[BUY] BUY 10 @ Market\n├ Type: MARKET"));
        assert!(t.contains("├ Status: Complete"));
        assert!(t.contains("└ Order ID: `1`"));
        assert!(t.contains("*Summary*\n├ Total Orders: 1"));
        let q = json!({"status": "success", "data": {"ltp": 110, "prev_close": 100, "open": 101,
            "high": 111, "low": 99, "volume": 1234567}});
        let qt = format_quote("SBIN", &q, "₹");
        assert!(qt.contains("[+] Price: ₹110.00\n├ Change: ₹+10.00 (+10.00%)"));
        assert!(qt.contains("└ Volume: 1,234,567"));
    }

    #[test]
    fn history_rows_become_ist_candles() {
        let r = json!({"status": "success", "data": [
            {"timestamp": 1758512700, "open": 1, "high": 2, "low": 0.5, "close": 1.5, "volume": 10}]});
        let c = candles_from_history(&r);
        assert_eq!(c.len(), 1);
        assert_eq!(
            c[0].time.format("%d %b %H:%M").to_string().to_uppercase(),
            "22 SEP 09:15"
        );
    }
}
