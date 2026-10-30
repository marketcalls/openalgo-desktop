//! The price monitor's poll over the app's own API, against the scripted
//! broker (parity T-033, T-034).

use super::*;
use crate::brokers::mock::MockCall;
use crate::brokers::types::Quote;
use crate::flow::triggers::price_monitor::Alert;

fn quote(ltp: f64) -> Quote {
    Quote {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        ltp,
        ..Default::default()
    }
}

/// T-033, T-034: each poll asks the broker for the alert's quote through
/// the app's own API; a failed quote or a price of zero fires nothing and
/// leaves the previous price unset; a real price fires the run, which
/// trusts that tick.
#[tokio::test]
async fn the_poll_quotes_each_alert_and_fires_only_on_a_real_price() {
    let f = F::new();
    let id = f
        .create(json!({
            "name": "Breakout",
            "nodes": [
                {"id": "p", "type": "priceAlert", "position": {"x": 0, "y": 0},
                 "data": {"symbol": "SBIN", "exchange": "NSE", "condition": "above", "price": 800}},
                {"id": "l", "type": "log", "position": {"x": 0, "y": 100}, "data": {"message": "Bought at {{webhook.trigger_price}}"}}
            ],
            "edges": [{"id": "e", "source": "p", "target": "l", "sourceHandle": "true"}]
        }))
        .await;
    let ctx = f.h.ctx();
    ctx.symbols
        .load(vec![crate::brokers::common::symbols::SymToken {
            symbol: "SBIN".into(),
            brsymbol: "SBIN".into(),
            name: "STATE BANK OF INDIA".into(),
            exchange: "NSE".into(),
            brexchange: "NSE".into(),
            token: "3045".into(),
            expiry: String::new(),
            strike: 0.0,
            lot_size: 1,
            instrument_type: "EQ".into(),
            tick_size: 0.05,
        }]);
    crate::flow::store::workflows::set_active(&ctx.sqlite.conn().unwrap(), id, true, ctx.now())
        .unwrap();
    let side =
        crate::flow::triggers::TriggerSide::new(std::sync::Arc::downgrade(ctx), ctx.clock.clone());
    let data = json!({"symbol": "SBIN", "exchange": "NSE", "condition": "above", "price": 800});
    side.alerts
        .add(Alert::from_node(id, data.as_object().unwrap(), ctx.now()).unwrap());
    let quoted = || {
        f.h.mock
            .calls()
            .iter()
            .filter(|c| matches!(c, MockCall::Quote(k) if k.symbol == "SBIN"))
            .count()
    };

    assert!(!side.check_prices(false).await);
    assert_eq!(quoted(), 1, "one quote per alert per poll");
    let a = side.alerts.get(id).unwrap();
    assert!(a.last.is_none() && !a.triggered, "no quote: nothing seen");

    f.h.mock.set_quote(quote(0.0));
    side.check_prices(false).await;
    assert_eq!(quoted(), 2);
    let a = side.alerts.get(id).unwrap();
    assert!(
        a.last.is_none() && !a.triggered,
        "a zero price is not a price"
    );

    f.h.mock.set_quote(quote(812.75));
    side.check_prices(false).await;
    settled(&side, id).await;
    let logs = f.last_run_logs(id).await;
    assert!(
        logs.contains(&"[LOG] Bought at 812.75".to_string()),
        "{:?}",
        logs
    );
    assert_eq!(quoted(), 3, "the run trusted the poll's tick");
}
