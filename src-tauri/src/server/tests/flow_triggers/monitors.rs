//! The price monitor's poll over the app's own API, against the scripted
//! broker (parity T-033, T-034).

use super::*;
use crate::brokers::common::symbols::SymToken;
use crate::brokers::mock::MockCall;
use crate::brokers::types::Quote;
use crate::flow::triggers::price_monitor::Alert;

fn listed(symbol: &str, token: &str) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: symbol.into(),
        name: symbol.into(),
        exchange: "NSE".into(),
        brexchange: "NSE".into(),
        token: token.into(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: "EQ".into(),
        tick_size: 0.05,
    }
}

fn quote(ltp: f64) -> Quote {
    Quote {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        ltp,
        ..Default::default()
    }
}

/// An active priceAlert workflow on `symbol` whose true branch logs the
/// price that fired it.
async fn breakout(f: &F, symbol: &str) -> i64 {
    let id = f
        .create(json!({
            "name": format!("Breakout {}", symbol),
            "nodes": [
                {"id": "p", "type": "priceAlert", "position": {"x": 0, "y": 0},
                 "data": {"symbol": symbol, "exchange": "NSE", "condition": "above", "price": 800}},
                {"id": "l", "type": "log", "position": {"x": 0, "y": 100}, "data": {"message": "Bought at {{webhook.trigger_price}}"}}
            ],
            "edges": [{"id": "e", "source": "p", "target": "l", "sourceHandle": "true"}]
        }))
        .await;
    let ctx = f.h.ctx();
    crate::flow::store::workflows::set_active(&ctx.sqlite.conn().unwrap(), id, true, ctx.now())
        .unwrap();
    id
}

/// T-033, T-034: each poll asks the broker for every alert's quote through
/// the app's own API; a quote the broker refuses (INFY has none) or a price
/// of zero fires nothing and leaves the previous price unset; a real price
/// fires the run, which trusts that tick and asks for no quote of its own.
#[tokio::test]
async fn the_poll_quotes_each_alert_and_fires_only_on_a_real_price() {
    let f = F::new();
    let ctx = f.h.ctx();
    // The app's own trigger side restores the active workflows at start-up
    // on its own task; when that read comes after the save below, it holds
    // the same alert and polls the broker too. Stopped, only this side polls.
    ctx.flow.triggers().stop().await;
    ctx.symbols
        .load(vec![listed("SBIN", "3045"), listed("INFY", "1594")]);
    let (sbin, infy) = (breakout(&f, "SBIN").await, breakout(&f, "INFY").await);
    let side =
        crate::flow::triggers::TriggerSide::new(std::sync::Arc::downgrade(ctx), ctx.clock.clone());
    for (id, symbol) in [(sbin, "SBIN"), (infy, "INFY")] {
        let data = json!({"symbol": symbol, "exchange": "NSE", "condition": "above", "price": 800});
        side.alerts
            .add(Alert::from_node(id, data.as_object().unwrap(), ctx.now()).unwrap());
    }
    let quoted = |symbol: &str| {
        f.h.mock
            .calls()
            .iter()
            .filter(|c| matches!(c, MockCall::Quote(k) if k.symbol == symbol))
            .count()
    };

    f.h.mock.set_quote(quote(0.0));
    assert!(!side.check_prices(false).await);
    assert_eq!(quoted("SBIN"), 1, "one quote per alert per poll");
    assert!(quoted("INFY") >= 1, "the refused one was asked too");
    for id in [sbin, infy] {
        let a = side.alerts.get(id).unwrap();
        assert!(
            a.last.is_none() && !a.triggered,
            "a zero price or no quote is not a price"
        );
    }

    f.h.mock.set_quote(quote(812.75));
    side.check_prices(false).await;
    let polled = quoted("SBIN");
    assert_eq!(polled, 2);
    settled(&side, sbin).await;
    let logs = f.last_run_logs(sbin).await;
    assert!(
        logs.contains(&"[LOG] Bought at 812.75".to_string()),
        "{:?}",
        logs
    );
    assert_eq!(quoted("SBIN"), polled, "the run trusted the poll's tick");
    assert!(!side.alerts.get(infy).unwrap().triggered);
}
