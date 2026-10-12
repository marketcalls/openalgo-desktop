//! LOG-07: the open Live obligations a broker switch must not leave behind
//! (`services::obligations`), counted from the stores alone.

use super::*;
use openalgo_desktop_lib::services::obligations::open_live_obligations;

#[tokio::test]
async fn nothing_open_is_empty_and_sandbox_runs_do_not_count() {
    let a = app();
    let o = open_live_obligations(&a.ctx, "zerodha").unwrap();
    assert!(o.is_empty(), "{:?}", o);
    // A Sandbox run keeps running on the next broker's quotes.
    let cfg = config("Sandbox", json!([short_call_leg()]), json!({}));
    let (row, _) = a.ctx.strategy.store.create_strategy(USER, &cfg).unwrap();
    let r = a
        .ctx
        .strategy
        .start_run(row.id, USER, "sandbox", "manual", None)
        .await;
    assert!(r.ok, "{:?}", r);
    assert!(open_live_obligations(&a.ctx, "zerodha").unwrap().is_empty());
}

#[tokio::test]
async fn a_live_run_and_its_unconfirmed_exit_are_open_obligations() {
    use openalgo_desktop_lib::brokers::mock::AfterSend;
    let a = app();
    a.ctx.reconciler.stop();
    let cfg = config("Live", json!([short_call_leg()]), json!({}));
    let (row, _) = a.ctx.strategy.store.create_strategy(USER, &cfg).unwrap();
    a.ctx
        .strategy
        .store
        .set_live_enabled(row.id, USER, true)
        .unwrap();
    a.ctx.sqlite.set_analyze_mode(false).unwrap();
    let r = a
        .ctx
        .strategy
        .start_run(row.id, USER, "live", "manual", None)
        .await;
    assert!(r.ok, "{:?}", r);
    let run = r.run_id.unwrap();
    a.ctx
        .strategy
        .apply_fill(run, 1, Some(100.0), true, FillOpts::default())
        .await;
    let o = open_live_obligations(&a.ctx, "zerodha").unwrap();
    assert_eq!(o.strategy_runs, 1, "{:?}", o);
    assert_eq!(o.uncertain_placements, 0);
    // Another broker's obligations are not this one's.
    assert_eq!(
        open_live_obligations(&a.ctx, "angel")
            .unwrap()
            .strategy_runs,
        0
    );
    // An exit whose answer was lost is an uncertain placement.
    a.mock
        .after_send
        .lock()
        .push_back((AfterSend::Dropped, "open"));
    a.ctx.strategy.process_tick(ATM_CE, "NFO", 121.0).await;
    let o = open_live_obligations(&a.ctx, "zerodha").unwrap();
    assert_eq!(o.uncertain_placements, 1, "{:?}", o);
    assert!(!o.is_empty());
}

#[tokio::test]
async fn live_scalping_stops_are_open_obligations_and_sandbox_ones_are_not() {
    use openalgo_desktop_lib::scalping::store::SlUpsert;
    let a = app();
    let stop = |mode: &str| SlUpsert {
        symbol: ATM_CE.into(),
        exchange: "NFO".into(),
        product: "NRML".into(),
        mode: mode.into(),
        side: Some("SELL".into()),
        entry_price: Some(100.0),
        quantity: Some(65),
        initial_sl: Some(120.0),
        is_active: Some(true),
        ..Default::default()
    };
    a.ctx.scalping.store.upsert_sl(&stop("analyze")).unwrap();
    assert!(open_live_obligations(&a.ctx, "zerodha").unwrap().is_empty());
    a.ctx.scalping.store.upsert_sl(&stop("live")).unwrap();
    let o = open_live_obligations(&a.ctx, "zerodha").unwrap();
    assert_eq!(o.scalping_stops, 1, "{:?}", o);
    // Exit orders stored on the stop that cannot be read back count as
    // uncertain, never as settled.
    let id = a.ctx.scalping.store.active_sl(Some("live")).unwrap()[0].id;
    a.ctx
        .scalping
        .store
        .set_exit_attempt(id, Some("{not json"))
        .unwrap();
    let o = open_live_obligations(&a.ctx, "zerodha").unwrap();
    assert_eq!(o.uncertain_placements, 1, "{:?}", o);
}
