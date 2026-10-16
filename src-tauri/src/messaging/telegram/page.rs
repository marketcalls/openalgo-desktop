//! What the `/telegram` page reads and changes in openalgo.db (web
//! `blueprints/telegram.py` with `database/telegram_db.py`): the bot
//! settings, the linked users, their notification preferences and the
//! command statistics.

use super::db::{self, BotConfig, CommandStats, ConfigUpdate, TgUser, UserFilter};
use crate::error::Result;
use crate::state::AppState;
use serde_json::Value;

/// The bot settings, the token decrypted.
pub fn bot_config(ctx: &AppState) -> Result<BotConfig> {
    let c = ctx.sqlite.conn()?;
    db::get_bot_config(&c, &ctx.security)
}

/// Change the bot settings; what `u` leaves `None` is kept.
pub fn update_config(ctx: &AppState, u: ConfigUpdate) -> Result<()> {
    let c = ctx.sqlite.conn()?;
    db::update_bot_config(&c, &ctx.security, u, ctx.now())
}

/// The linked users that pass `f`.
pub fn users(ctx: &AppState, f: &UserFilter) -> Result<Vec<TgUser>> {
    let c = ctx.sqlite.conn()?;
    db::all_users(&c, f)
}

/// The linked user with this Telegram id.
pub fn user(ctx: &AppState, telegram_id: i64) -> Result<Option<TgUser>> {
    let c = ctx.sqlite.conn()?;
    db::get_user(&c, telegram_id)
}

/// The Telegram user linked to an OpenAlgo account.
pub fn user_by_username(ctx: &AppState, username: &str) -> Result<Option<TgUser>> {
    let c = ctx.sqlite.conn()?;
    db::get_user_by_username(&c, username)
}

/// Unlink a user. `false`: nothing was unlinked.
pub fn unlink_user(ctx: &AppState, telegram_id: i64) -> Result<bool> {
    let c = ctx.sqlite.conn()?;
    db::delete_user(&c, telegram_id, ctx.now())
}

/// Bot commands of the last `days`.
pub fn command_stats(ctx: &AppState, days: i64) -> Result<CommandStats> {
    let c = ctx.sqlite.conn()?;
    db::command_stats(&c, days, ctx.now())
}

/// When the bot settings were created and last changed, as stored.
pub fn bot_config_times(ctx: &AppState) -> Result<(Option<String>, Option<String>)> {
    let c = ctx.sqlite.conn()?;
    db::bot_config_times(&c)
}

/// A linked user's notification preferences, or the defaults.
pub fn preferences(ctx: &AppState, telegram_id: i64) -> Result<Value> {
    let c = ctx.sqlite.conn()?;
    db::get_preferences(&c, telegram_id)
}

/// Save notification preferences (names and values already checked).
pub fn save_preferences(
    ctx: &AppState,
    telegram_id: i64,
    prefs: &[(&'static str, Value)],
) -> Result<()> {
    let mut c = ctx.sqlite.conn()?;
    db::update_preferences(&mut c, telegram_id, prefs, ctx.now())
}
