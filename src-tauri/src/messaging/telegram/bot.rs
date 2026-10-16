//! Command and button handlers of the Telegram bot, with the web's texts
//! (`TelegramBotService.cmd_*`, `button_callback`, `_format_*`).
//!
//! Differences from the web, deliberate:
//! * Commands run against this desktop's own services (`messaging::openalgo`);
//!   the host URL given to `/link` is stored for the pages but never
//!   contacted, and the link reply says so.
//! * The Python Strategy Host is not part of this version of the desktop
//!   (maintainer, 2026-10-12), so the web's `/stoppython` command, its
//!   strategy pickers and the "Close all + Stop strategies" choice of
//!   `/closeall` are left out.
//! * The web's two `ℹ` characters are dropped (no emojis anywhere).
//!
//! The reply texts themselves are `texts`.

use super::api::{BotApi, TgError};
use super::chart;
use super::db;
use super::texts::{
    candles_from_history, format_funds_full, format_holdings_full, format_orderbook_full,
    format_pnl_funds, format_positions_full, format_quote, format_short, format_tradebook_full,
    HELP_TEXT, RULE,
};
use crate::messaging::openalgo::{is_success, OpenAlgoClient};
use crate::messaging::ReplyClass;
use crate::state::AppState;
use chrono::Duration;
use serde_json::{json, Value};
use std::sync::Arc;

const LINK_FIRST: &str = "Please link your account first using /link";

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
    "mode",
];
/// Every button the bot sends (an entry ending in `_` is a prefix); all need
/// a linked user in a private chat.
pub const CALLBACKS: &[&str] = &[
    "cancel_action",
    "confirm_closeall",
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

/// Reply when a linked user's stored API key no longer works.
const KEY_NO_LONGER_VALID: &str =
    "Your linked API key is no longer valid. Link again with /link and your current key.";

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

    /// Record a command for the analytics, before its reply goes out: a
    /// trader who sees the reply and opens the analytics finds it counted.
    /// The web logs after replying, a race its tests do not hit; the
    /// WhatsApp bot here logs first as well.
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
        if crate::brokers::catalog::is_crypto(user.broker.as_deref().unwrap_or("")) {
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
            "start" => self.cmd_start(ctx, chat, from).await,
            "help" => {
                Self::log(ctx, from.id, "help", chat);
                self.send(chat, HELP_TEXT, true, None).await;
            }
            "link" => self.cmd_link(ctx, args, chat, from).await,
            "unlink" => self.cmd_unlink(ctx, chat, from).await,
            "status" => self.cmd_status(ctx, chat, from).await,
            "orderbook" | "tradebook" | "positions" | "holdings" | "funds" | "pnl" => {
                self.cmd_book(ctx, cmd, chat, from).await
            }
            "quote" => self.cmd_quote(ctx, args, chat, from).await,
            "chart" => self.cmd_chart(ctx, args, chat, from).await,
            "menu" => self.cmd_menu(ctx, chat, from).await,
            "closeall" => self.cmd_closeall(ctx, chat, from).await,
            "mode" => self.cmd_mode(ctx, chat, from).await,
            _ => {}
        }
    }

    /// `/start`: a welcome, saying how to link when the user is not linked.
    async fn cmd_start(&self, ctx: &Arc<AppState>, chat: i64, from: &From) {
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
        Self::log(ctx, from.id, "start", chat);
        self.send(chat, &text, true, None).await;
    }

    /// `/unlink`: remove the linked account.
    async fn cmd_unlink(&self, ctx: &Arc<AppState>, chat: i64, from: &From) {
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
        Self::log(ctx, from.id, "unlink", chat);
        self.send(chat, text, true, None).await;
    }

    /// `/menu`: the buttons of the trading menu.
    async fn cmd_menu(&self, ctx: &Arc<AppState>, chat: i64, from: &From) {
        if Self::user(ctx, from.id).is_none() {
            self.send(chat, LINK_FIRST, false, None).await;
            return;
        }
        Self::log(ctx, from.id, "menu", chat);
        self.send(
            chat,
            "*OpenAlgo Trading Menu*\nSelect an option below:",
            true,
            Some(menu_keyboard()),
        )
        .await;
    }

    /// `/closeall`: ask to confirm closing every open position.
    async fn cmd_closeall(&self, ctx: &Arc<AppState>, chat: i64, from: &From) {
        if Self::user(ctx, from.id).is_none() {
            self.send(chat, LINK_FIRST, false, None).await;
            return;
        }
        let kb = json!({"inline_keyboard": [
            [{"text": "Yes, close all", "callback_data": "confirm_closeall"}],
            [{"text": "Cancel", "callback_data": "cancel_action"}],
        ]});
        let text = format!(
            "*Close All Positions*\n{RULE}\n\nThis will close ALL open positions across all strategies.\n\nAre you sure?"
        );
        Self::log(ctx, from.id, "closeall", chat);
        self.send(chat, &text, true, Some(kb)).await;
    }

    /// `/mode`: the trading mode, with a button to switch it.
    async fn cmd_mode(&self, ctx: &Arc<AppState>, chat: i64, from: &From) {
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
        Self::log(ctx, from.id, "mode", chat);
        self.send(
            chat,
            &text,
            true,
            Some(json!({"inline_keyboard": [[{"text": label, "callback_data": data}]]})),
        )
        .await;
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
            Self::log(ctx, from.id, "link", chat);
            self.send(chat, crate::messaging::TOO_MANY_LINK_ATTEMPTS, false, None)
                .await;
            return;
        }
        let client = OpenAlgoClient::new(ctx.clone(), api_key.clone());
        let resp = client.funds().await;
        if !is_success(&resp) {
            ctx.messaging.link_throttle.fail(&who, ctx.now());
            tracing::warn!("Telegram link attempt with an API key that did not validate");
            Self::log(ctx, from.id, "link", chat);
            self.send(
                chat,
                "Failed to validate API key.\nPlease check your credentials and try again.",
                true,
                None,
            )
            .await;
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
        Self::log(ctx, from.id, "link", chat);
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
    }

    async fn cmd_status(&self, ctx: &Arc<AppState>, chat: i64, from: &From) {
        let Some(user) = Self::user(ctx, from.id) else {
            Self::log(ctx, from.id, "status", chat);
            self.send(
                chat,
                "No linked account found.\nUse /link to connect your OpenAlgo account.",
                true,
                None,
            )
            .await;
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
        Self::log(ctx, from.id, "status", chat);
        self.send(chat, &text, true, None).await;
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
        Self::log(ctx, from.id, cmd, chat);
        self.send(chat, &text, true, None).await;
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
        Self::log(ctx, from.id, "quote", chat);
        self.send(chat, &text, true, None).await;
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
        Self::log(ctx, from.id, "chart", chat);
        self.send_charts(chat, &symbol, photos).await;
    }

    /// Send the charts made: one photo, an album, or why there is none.
    async fn send_charts(&self, chat: i64, symbol: &str, mut photos: Vec<(Vec<u8>, String)>) {
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
            "confirm_closeall" => self.on_confirm_closeall(ctx, chat, mid, &from, &data).await,
            "mode_live" | "mode_analyze" => self.on_mode_button(ctx, chat, mid, &from, &data).await,
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
            other => self.on_book_button(ctx, chat, &from, other).await,
        }
    }

    /// The "Yes, close all" button: close every position and say how it
    /// went.
    async fn on_confirm_closeall(
        &self,
        ctx: &Arc<AppState>,
        chat: i64,
        mid: i64,
        from: &From,
        data: &str,
    ) {
        if Self::user(ctx, from.id).is_none() {
            self.edit(chat, mid, LINK_FIRST, false, None).await;
            return;
        }
        let Some(client) = Self::client(ctx, from.id) else {
            self.edit(chat, mid, "Failed to connect to OpenAlgo", false, None)
                .await;
            return;
        };
        self.edit(chat, mid, "Closing all positions...", false, None)
            .await;
        let resp = client.closeposition().await;
        Self::log(ctx, from.id, data, chat);
        let message = |r: &Option<Value>, d: &str| {
            r.as_ref()
                .and_then(|v| v.get("message"))
                .map(crate::messaging::format::py_str)
                .unwrap_or_else(|| d.into())
        };
        if is_success(&resp) {
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
    }

    /// The mode buttons: switch analyze mode, with a key that is still
    /// valid.
    async fn on_mode_button(
        &self,
        ctx: &Arc<AppState>,
        chat: i64,
        mid: i64,
        from: &From,
        data: &str,
    ) {
        // Being linked once is not enough to change the trading mode:
        // the linked key must still be valid now, as for every account
        // action (a key regenerated or deleted since must stop it).
        let allowed = match Self::client(ctx, from.id) {
            Some(c) => c.is_authorized().await,
            None => false,
        };
        if !allowed {
            self.edit(chat, mid, KEY_NO_LONGER_VALID, false, None).await;
            return;
        }
        let requested = data == "mode_analyze";
        let changed = crate::services::AnalyzerService::set_mode(ctx, requested).await;
        Self::log(ctx, from.id, data, chat);
        match changed {
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
    }

    /// A book, funds or P&L button: the short text for it.
    async fn on_book_button(&self, ctx: &Arc<AppState>, chat: i64, from: &From, other: &str) {
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
        Self::log(ctx, from.id, other, chat);
        self.send(chat, &text, true, None).await;
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

    /// The Python Strategy Host is not in this version: the bot neither
    /// offers nor answers the web's commands and buttons for it.
    #[test]
    fn no_python_strategy_commands_or_buttons() {
        assert!(!HELP_TEXT.contains("stoppython"));
        assert!(!HELP_TEXT.contains("Python"));
        assert_eq!(
            gate(Request::Command("stoppython"), true, 5, 5),
            Gate::Ignore
        );
        for d in ["confirm_closeall_with_strategies", "csy_all", "spy_1"] {
            assert_eq!(
                gate(Request::Callback(d), true, 5, 5),
                Gate::Ignore,
                "{}",
                d
            );
        }
        assert_eq!(
            gate(Request::Callback("confirm_closeall"), true, 5, 5),
            Gate::Allow
        );
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
