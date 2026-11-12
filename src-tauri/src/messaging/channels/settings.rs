//! Each channel's switch and event routing, as the /alerts page sets them
//! (web `services/alerts/settings.py`).
//!
//! Read on every alert, for every channel, so reads are cached
//! (`crate::cache::TtlCache`, the web's `LockedTTLCache(16, 300 s)`) and a
//! write clears its entry once it has committed. A channel nobody has
//! changed uses its defaults: on for Telegram and WhatsApp with every
//! group, exactly as they alerted before this page existed, and off for a
//! webhook channel until its webhook is saved.
//!
//! **Writes are serialised per channel and read the stored row.** A change
//! reads what is saved, applies one field, and writes the whole row back.
//! Two changes to one channel at once (the switch and a group, from two
//! tabs) would otherwise each write back what they read, and the later one
//! would undo the earlier. And a change never starts from the defaults
//! because the row could not be read: that would quietly switch a channel
//! the trader turned off back on.
//!
//! The functions here are synchronous and may wait up to [`WRITE_WAIT`] for
//! a channel's lock; async callers run them on the blocking pool.

use super::{categories, store, webhook, ChannelName};
use crate::cache::TtlCache;
use crate::error::AppError;
use crate::state::AppState;
use parking_lot::Mutex;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

/// How long a change waits for another change to the same channel.
pub const WRITE_WAIT: Duration = Duration::from_secs(10);

/// Settings change only when the trader saves them, and a save clears its
/// entry, so the time limit only bounds how long a write made outside this
/// module can go unseen.
const CACHE_TTL: Duration = Duration::from_secs(300);
const CACHE_SIZE: usize = 16;

/// A channel's switch and the event groups it receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelSettings {
    pub enabled: bool,
    /// Group ids, in alphabetical order (as the page lists them).
    pub categories: BTreeSet<&'static str>,
}

impl ChannelSettings {
    /// Whether an alert in `category` goes to this channel.
    pub fn receives(&self, category: &str) -> bool {
        self.enabled && self.categories.contains(category)
    }
}

/// Why a change was not made. The sentences are for the trader.
#[derive(Debug)]
pub enum SettingsError {
    /// A group that does not exist.
    UnknownCategory(String),
    /// A webhook channel switched on with no usable webhook.
    WebhookRequired(String),
    /// Another change to this channel did not finish in time.
    Busy,
    /// The stored settings could not be read or written; nothing changed.
    Storage(AppError),
}

impl From<AppError> for SettingsError {
    fn from(e: AppError) -> Self {
        SettingsError::Storage(e)
    }
}

/// The cache and one writer lock per channel.
pub(crate) struct SettingsState {
    cache: TtlCache<ChannelName, ChannelSettings>,
    writers: [Mutex<()>; 4],
    #[cfg(test)]
    pub(crate) hooks: Hooks,
}

impl Default for SettingsState {
    fn default() -> Self {
        Self {
            cache: TtlCache::new(CACHE_SIZE, CACHE_TTL),
            writers: Default::default(),
            #[cfg(test)]
            hooks: Hooks::default(),
        }
    }
}

impl SettingsState {
    fn hold(&self, channel: ChannelName) -> Result<parking_lot::MutexGuard<'_, ()>, SettingsError> {
        self.hold_for(channel, WRITE_WAIT)
    }

    /// The channel's writer lock, waiting at most `wait` for it.
    fn hold_for(
        &self,
        channel: ChannelName,
        wait: Duration,
    ) -> Result<parking_lot::MutexGuard<'_, ()>, SettingsError> {
        let index = match channel {
            ChannelName::Telegram => 0,
            ChannelName::WhatsApp => 1,
            ChannelName::Slack => 2,
            ChannelName::Discord => 3,
        };
        self.writers[index]
            .try_lock_for(wait)
            .ok_or(SettingsError::Busy)
    }
}

fn state(ctx: &AppState) -> &SettingsState {
    &ctx.messaging.channels.settings
}

/// What a channel does before anybody changes it.
pub fn defaults(channel: ChannelName) -> ChannelSettings {
    ChannelSettings {
        enabled: channel.enabled_by_default(),
        categories: categories::all_ids().collect(),
    }
}

/// Settings from a stored row, or the defaults when there is none. A group
/// that no longer exists is ignored rather than an error.
fn from_stored(channel: ChannelName, stored: Option<store::StoredChannel>) -> ChannelSettings {
    match stored {
        None => defaults(channel),
        Some(row) => ChannelSettings {
            enabled: row.enabled,
            categories: categories::all_ids()
                .filter(|id| row.categories.iter().any(|c| c == id))
                .collect(),
        },
    }
}

fn read_stored(ctx: &AppState, channel: ChannelName) -> crate::error::Result<ChannelSettings> {
    let conn = ctx.sqlite.conn()?;
    Ok(from_stored(channel, store::load(&conn, channel.name())?))
}

/// A channel's current settings. A database that cannot be read gives the
/// defaults, so a broken settings table never silences Telegram or
/// WhatsApp, which were always on.
pub fn settings_for(ctx: &AppState, channel: ChannelName) -> ChannelSettings {
    let cache = &state(ctx).cache;
    let (cached, generation) = cache.get_with_generation(&channel, Instant::now());
    if let Some(settings) = cached {
        return settings;
    }
    match read_stored(ctx, channel) {
        Ok(settings) => {
            #[cfg(test)]
            Hooks::run(&state(ctx).hooks.after_read);
            cache.fill(channel, settings.clone(), generation, Instant::now());
            settings
        }
        Err(e) => {
            tracing::error!(
                "Could not read the {} alert settings; using the defaults: {}",
                channel.label(),
                e
            );
            defaults(channel)
        }
    }
}

/// Whether an alert in `category` goes to `channel` now.
pub fn receives(ctx: &AppState, channel: ChannelName, category: &str) -> bool {
    settings_for(ctx, channel).receives(category)
}

/// A change to a channel's switch, its groups, or both. `None` keeps a
/// field as it is.
#[derive(Debug, Clone, Default)]
pub struct Change {
    pub enabled: Option<bool>,
    pub categories: Option<Vec<String>>,
}

/// Change a channel's switch, its groups, or both, and answer the settings
/// now in force.
pub fn update(
    ctx: &AppState,
    channel: ChannelName,
    change: Change,
) -> Result<ChannelSettings, SettingsError> {
    if let Some(wanted) = &change.categories {
        let unknown: BTreeSet<&str> = wanted
            .iter()
            .map(String::as_str)
            .filter(|c| !categories::is_known(c))
            .collect();
        if !unknown.is_empty() {
            let names: Vec<&str> = unknown.into_iter().collect();
            return Err(SettingsError::UnknownCategory(format!(
                "Unknown event group: {}",
                names.join(", ")
            )));
        }
    }
    let state = state(ctx);
    let _writer = state.hold(channel)?;
    // Checked under the lock forget_webhook takes, so a webhook removed at
    // the same moment cannot leave the channel on without one.
    if change.enabled == Some(true) {
        if let Some(kind) = channel.webhook() {
            if !webhook::status(ctx, kind)?.configured {
                let label = channel.label();
                return Err(SettingsError::WebhookRequired(format!(
                    "Save a {} webhook before turning {} on.",
                    label, label
                )));
            }
        }
    }
    // The row itself, not the cached read: a read that fails is an error
    // here instead of the defaults.
    let current = read_stored(ctx, channel)?;
    let updated = ChannelSettings {
        enabled: change.enabled.unwrap_or(current.enabled),
        categories: match change.categories {
            None => current.categories,
            Some(wanted) => categories::all_ids()
                .filter(|id| wanted.iter().any(|w| w == id))
                .collect(),
        },
    };
    #[cfg(test)]
    Hooks::run(&state.hooks.before_save);
    let in_page_order: Vec<&str> = categories::all_ids()
        .filter(|id| updated.categories.contains(id))
        .collect();
    let conn = ctx.sqlite.conn()?;
    store::save_channel(
        &conn,
        channel.name(),
        updated.enabled,
        &in_page_order,
        ctx.now(),
    )?;
    state.cache.invalidate(&channel);
    Ok(updated)
}

/// Store a validated webhook URL and switch the channel on. Saving a
/// webhook is what switching a webhook channel on means to a trader, so it
/// is not left as a second step to forget.
pub fn save_webhook(
    ctx: &AppState,
    url: &webhook::WebhookUrl,
) -> Result<ChannelSettings, SettingsError> {
    let channel = url.kind().channel();
    let state = state(ctx);
    {
        let _writer = state.hold(channel)?;
        let conn = ctx.sqlite.conn()?;
        let all: Vec<&str> = categories::all_ids().collect();
        store::save_webhook(
            &conn,
            &ctx.security,
            channel.name(),
            url.expose(),
            true,
            &all,
            ctx.now(),
        )?;
        state.cache.invalidate(&channel);
    }
    Ok(settings_for(ctx, channel))
}

/// Forget a webhook URL. The channel is switched off with it.
pub fn forget_webhook(
    ctx: &AppState,
    kind: webhook::WebhookKind,
) -> Result<ChannelSettings, SettingsError> {
    let channel = kind.channel();
    let state = state(ctx);
    {
        let _writer = state.hold(channel)?;
        #[cfg(test)]
        Hooks::run(&state.hooks.before_forget);
        let conn = ctx.sqlite.conn()?;
        store::clear_webhook(&conn, channel.name(), ctx.now())?;
        state.cache.invalidate(&channel);
    }
    Ok(settings_for(ctx, channel))
}

/// Forget every cached setting: logout, a password change or reset, and
/// every other moment the app's caches are cleared.
pub fn clear_cache(ctx: &AppState) {
    state(ctx).cache.clear();
}

/// Points where a test can hold one call while another runs, to stage the
/// races the web's tests stage by patching.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct Hooks {
    /// After `settings_for` read the row, before it fills the cache.
    pub(crate) after_read: Mutex<Option<std::sync::Arc<dyn Fn() + Send + Sync>>>,
    /// In `update`, after its checks and its read, before it saves.
    pub(crate) before_save: Mutex<Option<std::sync::Arc<dyn Fn() + Send + Sync>>>,
    /// In `forget_webhook`, under the channel's lock, before it clears.
    pub(crate) before_forget: Mutex<Option<std::sync::Arc<dyn Fn() + Send + Sync>>>,
}

#[cfg(test)]
impl Hooks {
    fn run(hook: &Mutex<Option<std::sync::Arc<dyn Fn() + Send + Sync>>>) {
        let hook = hook.lock().clone();
        if let Some(hook) = hook {
            hook();
        }
    }
}

#[cfg(test)]
mod tests;
