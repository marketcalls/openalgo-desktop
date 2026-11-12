//! Each channel's switch and routing (web `test/test_alerts_routing.py`
//! "Who receives it" and the settings cases of `test/test_alerts_api.py`).

use super::*;
use crate::messaging::channels::testkit::{gate, url, with_provider, SLACK_URL};
use crate::messaging::channels::webhook::WebhookKind;

fn change(enabled: Option<bool>, categories: Option<&[&str]>) -> Change {
    Change {
        enabled,
        categories: categories.map(|c| c.iter().map(|s| s.to_string()).collect()),
    }
}

fn stored(ctx: &AppState, channel: ChannelName) -> Option<store::StoredChannel> {
    store::load(&ctx.sqlite.conn().unwrap(), channel.name()).unwrap()
}

fn move_table(ctx: &AppState, from: &str, to: &str) {
    ctx.sqlite
        .conn()
        .unwrap()
        .execute_batch(&format!("ALTER TABLE {} RENAME TO {}", from, to))
        .unwrap();
}

/// Web test/test_alerts_routing.py::test_with_nothing_saved_telegram_and_whatsapp_receive_every_group
/// and ::test_with_nothing_saved_slack_and_discord_receive_nothing
#[tokio::test]
async fn with_nothing_saved_the_bots_receive_everything_and_webhooks_nothing() {
    let (t, _) = with_provider();
    for channel in [ChannelName::Telegram, ChannelName::WhatsApp] {
        let current = settings_for(&t.ctx, channel);
        assert!(current.enabled);
        assert_eq!(
            current.categories,
            categories::all_ids().collect::<BTreeSet<_>>()
        );
    }
    for channel in [ChannelName::Slack, ChannelName::Discord] {
        assert!(!settings_for(&t.ctx, channel).enabled);
    }
}

/// Web test/test_alerts_routing.py::test_a_channel_switched_off_is_not_handed_the_alert
/// and ::test_a_group_routed_away_from_a_channel_is_not_handed_to_it
#[tokio::test]
async fn a_channel_switched_off_or_routed_away_does_not_receive() {
    let (t, _) = with_provider();
    update(&t.ctx, ChannelName::WhatsApp, change(Some(false), None)).unwrap();
    assert!(!receives(&t.ctx, ChannelName::WhatsApp, "orders_placed"));
    update(
        &t.ctx,
        ChannelName::Telegram,
        change(None, Some(&["gtt_orders"])),
    )
    .unwrap();
    assert!(!receives(&t.ctx, ChannelName::Telegram, "orders_placed"));
    assert!(receives(&t.ctx, ChannelName::Telegram, "gtt_orders"));
}

/// Web test/test_alerts_routing.py::test_an_unknown_group_is_refused
#[tokio::test]
async fn an_unknown_group_is_refused() {
    let (t, _) = with_provider();
    let refused = update(
        &t.ctx,
        ChannelName::Slack,
        change(None, Some(&["orders_placed", "bogus", "also"])),
    );
    match refused {
        Err(SettingsError::UnknownCategory(m)) => {
            assert_eq!(m, "Unknown event group: also, bogus")
        }
        other => panic!("{:?}", other),
    }
    assert_eq!(stored(&t.ctx, ChannelName::Slack), None);
}

/// Web test/test_alerts_routing.py::test_a_change_is_seen_on_the_next_alert
#[tokio::test]
async fn a_change_is_seen_on_the_next_alert() {
    let (t, _) = with_provider();
    assert!(settings_for(&t.ctx, ChannelName::Telegram).enabled);
    update(&t.ctx, ChannelName::Telegram, change(Some(false), None)).unwrap();
    assert!(!settings_for(&t.ctx, ChannelName::Telegram).enabled);
}

/// Web test/test_alerts_routing.py::test_a_settings_table_that_cannot_be_read_does_not_silence_telegram
#[tokio::test]
async fn a_settings_table_that_cannot_be_read_does_not_silence_telegram() {
    let (t, _) = with_provider();
    move_table(&t.ctx, "alert_channels", "alert_channels_away");
    assert!(settings_for(&t.ctx, ChannelName::Telegram).enabled);
    assert!(!settings_for(&t.ctx, ChannelName::Slack).enabled);
    move_table(&t.ctx, "alert_channels_away", "alert_channels");
}

/// Web test/test_alerts_api.py::test_a_change_never_starts_from_the_defaults_when_the_row_cannot_be_read
#[tokio::test]
async fn a_change_never_starts_from_the_defaults_when_the_row_cannot_be_read() {
    let (t, _) = with_provider();
    update(&t.ctx, ChannelName::Telegram, change(Some(false), None)).unwrap();
    clear_cache(&t.ctx);
    move_table(&t.ctx, "alert_channels", "alert_channels_away");
    let failed = update(
        &t.ctx,
        ChannelName::Telegram,
        change(None, Some(&["gtt_orders"])),
    );
    move_table(&t.ctx, "alert_channels_away", "alert_channels");
    assert!(matches!(failed, Err(SettingsError::Storage(_))));
    let row = stored(&t.ctx, ChannelName::Telegram).unwrap();
    assert!(!row.enabled);
    assert_ne!(row.categories, ["gtt_orders"]);
}

/// Web test/test_alerts_api.py::test_slack_cannot_be_switched_on_without_a_webhook
#[tokio::test]
async fn slack_cannot_be_switched_on_without_a_webhook() {
    let (t, _) = with_provider();
    match update(&t.ctx, ChannelName::Slack, change(Some(true), None)) {
        Err(SettingsError::WebhookRequired(m)) => {
            assert_eq!(m, "Save a Slack webhook before turning Slack on.")
        }
        other => panic!("{:?}", other),
    }
    assert!(!settings_for(&t.ctx, ChannelName::Slack).enabled);
}

/// Web test/test_alerts_webhooks.py::test_saving_a_webhook_turns_the_channel_on_and_forgetting_it_turns_it_off
#[tokio::test]
async fn saving_a_webhook_switches_on_and_forgetting_it_switches_off() {
    let (t, _) = with_provider();
    let discord = url(
        WebhookKind::Discord,
        crate::messaging::channels::testkit::DISCORD_URL,
    );
    assert!(save_webhook(&t.ctx, &discord).unwrap().enabled);
    assert!(
        webhook::status(&t.ctx, WebhookKind::Discord)
            .unwrap()
            .configured
    );
    assert!(
        !forget_webhook(&t.ctx, WebhookKind::Discord)
            .unwrap()
            .enabled
    );
    assert!(webhook::saved_url(&t.ctx, WebhookKind::Discord)
        .unwrap()
        .is_none());
    assert!(
        !webhook::status(&t.ctx, WebhookKind::Discord)
            .unwrap()
            .configured
    );
}

/// Web test/test_alerts_api.py::test_two_changes_to_one_channel_at_once_both_land:
/// a second change arrives while the first is inside its save. Without the
/// per-channel lock the second read the row before the first wrote it, and
/// whichever wrote last undid the other.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_changes_to_one_channel_at_once_both_land() {
    let (t, _) = with_provider();
    let g = gate();
    *t.ctx.messaging.channels.settings.hooks.before_save.lock() = Some(g.hook.clone());
    let ctx = t.ctx.clone();
    let switch = std::thread::spawn(move || {
        update(&ctx, ChannelName::Telegram, change(Some(false), None)).unwrap();
    });
    g.entered
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let ctx = t.ctx.clone();
    let route = std::thread::spawn(move || {
        update(
            &ctx,
            ChannelName::Telegram,
            change(None, Some(&["gtt_orders"])),
        )
        .unwrap();
    });
    // Long enough for an unserialised second change to finish.
    std::thread::sleep(std::time::Duration::from_millis(300));
    g.release.send(()).unwrap();
    switch.join().unwrap();
    route.join().unwrap();
    let row = stored(&t.ctx, ChannelName::Telegram).unwrap();
    assert!(!row.enabled);
    assert_eq!(row.categories, ["gtt_orders"]);
}

/// Web test/test_alerts_api.py::test_a_webhook_removed_while_slack_is_switched_on_never_leaves_it_on:
/// the webhook is removed after the switch-on checked for one and before
/// it saved.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_webhook_removed_while_slack_is_switched_on_never_leaves_it_on() {
    let (t, _) = with_provider();
    save_webhook(&t.ctx, &url(WebhookKind::Slack, SLACK_URL)).unwrap();
    update(&t.ctx, ChannelName::Slack, change(Some(false), None)).unwrap();
    let g = gate();
    *t.ctx.messaging.channels.settings.hooks.before_save.lock() = Some(g.hook.clone());
    let ctx = t.ctx.clone();
    let switch_on = std::thread::spawn(move || {
        let _ = update(&ctx, ChannelName::Slack, change(Some(true), None));
    });
    g.entered
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let ctx = t.ctx.clone();
    let remove = std::thread::spawn(move || {
        forget_webhook(&ctx, WebhookKind::Slack).unwrap();
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    g.release.send(()).unwrap();
    switch_on.join().unwrap();
    remove.join().unwrap();
    let row = stored(&t.ctx, ChannelName::Slack).unwrap();
    assert!(!(row.enabled && !row.webhook_saved), "{:?}", row);
}

/// A read in flight across a write is answered to its own caller but never
/// cached (the cache's generation rule, `docs/port/no-keychain.md`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_overtaken_by_a_write_is_not_cached() {
    let (t, _) = with_provider();
    let g = gate();
    *t.ctx.messaging.channels.settings.hooks.after_read.lock() = Some(g.hook.clone());
    let ctx = t.ctx.clone();
    let reader = std::thread::spawn(move || settings_for(&ctx, ChannelName::Telegram));
    g.entered
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    update(&t.ctx, ChannelName::Telegram, change(Some(false), None)).unwrap();
    g.release.send(()).unwrap();
    assert!(reader.join().unwrap().enabled, "it read before the write");
    assert!(!settings_for(&t.ctx, ChannelName::Telegram).enabled);
}

/// Web test/test_alerts_api.py::test_a_change_waiting_too_long_for_another_is_refused:
/// a change that cannot take the channel's lock in time is refused as busy.
#[tokio::test]
async fn a_change_waiting_too_long_is_refused_as_busy() {
    let (t, _) = with_provider();
    let state = &t.ctx.messaging.channels.settings;
    let held = state
        .hold_for(ChannelName::Discord, Duration::from_millis(10))
        .unwrap();
    assert!(matches!(
        state.hold_for(ChannelName::Discord, Duration::from_millis(50)),
        Err(SettingsError::Busy)
    ));
    assert!(
        state
            .hold_for(ChannelName::Slack, Duration::from_millis(10))
            .is_ok(),
        "one lock per channel"
    );
    drop(held);
}

/// Logout and the other moments every cache is cleared clear this one too.
#[tokio::test]
async fn clearing_the_app_caches_clears_the_settings() {
    let (t, _) = with_provider();
    assert!(settings_for(&t.ctx, ChannelName::Telegram).enabled);
    store::save_channel(
        &t.ctx.sqlite.conn().unwrap(),
        "telegram",
        false,
        &[],
        t.ctx.now(),
    )
    .unwrap();
    assert!(
        settings_for(&t.ctx, ChannelName::Telegram).enabled,
        "cached"
    );
    crate::services::auth_service::AuthService::clear_caches(&t.ctx);
    assert!(!settings_for(&t.ctx, ChannelName::Telegram).enabled);
}
