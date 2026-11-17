//! A sandbox and a strategy book written while Zerodha's master named NSE
//! series stocks by Kite's tradingsymbol (HFCL-BE, EMBASSY-RR) keep working
//! after the master that stores them under their base symbol (web
//! openalgo#2463): the held quantities move to HFCL and EMBASSY, where an
//! order can close and net them.

use super::super::{download, load_cached};
use crate::brokers::mock::MockBroker;
use crate::brokers::types::AuthToken;
use crate::brokers::{Broker, BrokerRegistry};
use crate::sandbox::{OrderRequest, Quote};
use crate::state::testing::{build, TestCtx};
use chrono::TimeZone;
use rusqlite::params;
use rust_decimal_macros::dec;
use std::sync::Arc;

const KITE_HEADER: &str = "instrument_token,exchange_token,tradingsymbol,name,last_price,\
                           expiry,strike,tick_size,lot_size,instrument_type,segment,exchange";

/// Zerodha's master as the series strip leaves it: HFCL and EMBASSY under
/// their base symbol, DUAL-BE kept because DUAL is another stock's symbol.
fn zerodha_master() -> Vec<crate::brokers::types::SymbolData> {
    let rows = [
        (1, "RELIANCE"),
        (2, "HFCL-BE"),
        (3, "EMBASSY-RR"),
        (4, "DUAL"),
        (5, "DUAL-BE"),
    ]
    .map(|(token, ts)| format!("{token},{token},{ts},{ts},0,,0,0.05,1,EQ,NSE,NSE"));
    let csv = format!("{KITE_HEADER}\n{}\n", rows.join("\n"));
    crate::brokers::zerodha::master_contract::parse_instruments(&csv).unwrap()
}

fn signed_in_to_zerodha() -> (TestCtx, Arc<dyn Broker>) {
    let zerodha = Arc::new(MockBroker::new("zerodha"));
    *zerodha.master.lock() = Some(Ok(zerodha_master()));
    let now = chrono_tz::Asia::Kolkata
        .with_ymd_and_hms(2026, 10, 5, 10, 0, 0)
        .unwrap()
        .with_timezone(&chrono::Utc);
    let zerodha: Arc<dyn Broker> = zerodha;
    let t = build(BrokerRegistry::with(vec![zerodha.clone()]), now);
    t.ctx.set_broker_session(Some(crate::state::BrokerSession {
        broker_id: "zerodha".into(),
        auth_token: crate::security::Secret::new("t"),
        feed_token: None,
        user_id: "AB1".into(),
        user_name: None,
        authenticated_at: t.ctx.now(),
    }));
    (t, zerodha)
}

/// The rows the previous build wrote, in its own column formats.
fn populate_previous_build(t: &TestCtx) {
    let user = t.ctx.sandbox.user_id().to_string();
    let at = "2026-10-01 10:00:00";
    t.ctx
        .sandbox
        .db()
        .with_tx(|tx| {
            tx.execute(
                "INSERT INTO sandbox_holdings (user_id, symbol, exchange, quantity, average_price,
                   settlement_date, created_at, updated_at)
                 VALUES (?1, 'HFCL-BE', 'NSE', 100, '250', '2026-10-02', ?2, ?2)",
                params![user, at],
            )?;
            for (symbol, quantity) in [("EMBASSY-RR", 40), ("DUAL-BE", 10)] {
                tx.execute(
                    "INSERT INTO sandbox_positions (user_id, symbol, exchange, product, quantity,
                       average_price, created_at, updated_at)
                     VALUES (?1, ?2, 'NSE', 'CNC', ?3, '380', ?4, ?4)",
                    params![user, symbol, quantity, at],
                )?;
            }
            tx.execute(
                "INSERT INTO sandbox_orders (orderid, user_id, symbol, exchange, action, quantity,
                   price, price_type, product, order_status, pending_quantity, order_timestamp,
                   update_timestamp)
                 VALUES ('20261001000001', ?1, 'HFCL-BE', 'NSE', 'BUY', 10, '240', 'LIMIT', 'CNC',
                   'open', 10, ?2, ?2)",
                params![user, at],
            )?;
            tx.execute(
                "INSERT INTO sandbox_trades (tradeid, orderid, user_id, symbol, exchange, action,
                   quantity, price, product, trade_timestamp)
                 VALUES ('T1', '20260930000001', ?1, 'HFCL-BE', 'NSE', 'BUY', 100, '250', 'CNC', ?2)",
                params![user, at],
            )?;
            tx.execute(
                "INSERT INTO sandbox_gtt (gtt_id, user_id, trigger_type, symbol, exchange,
                   created_at, updated_at)
                 VALUES ('G1', ?1, 'single', 'HFCL-BE', 'NSE', ?2, ?2)",
                params![user, at],
            )?;
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
    let book = &t.ctx.strategy.book;
    assert!(book
        .record_order_tag("K1", "u", "swing", "HFCL-BE", "NSE", "CNC")
        .unwrap());
    book.apply_fill("K1", 100.0, 250.0, "BUY").unwrap().unwrap();
}

fn sandbox_symbols(t: &TestCtx, table: &str) -> Vec<String> {
    t.ctx
        .sandbox
        .db()
        .with_conn(|c| {
            let mut stmt = c.prepare(&format!("SELECT symbol FROM {table} ORDER BY id"))?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<Vec<String>>>()
        })
        .unwrap()
}

fn sell_hfcl() -> OrderRequest {
    OrderRequest {
        symbol: "HFCL".into(),
        exchange: "NSE".into(),
        action: "SELL".into(),
        quantity: 100,
        price: None,
        trigger_price: None,
        price_type: "MARKET".into(),
        product: "CNC".into(),
        strategy: String::new(),
    }
}

/// The download moves every sandbox row and the strategy book's leg to the
/// base symbol; a CNC sell of HFCL then sells the holding, and an exit of
/// HFCL closes the leg instead of opening a short one beside it.
#[tokio::test]
async fn held_quantities_follow_the_stock_to_its_base_symbol() {
    let (t, zerodha) = signed_in_to_zerodha();
    populate_previous_build(&t);
    download(&t.ctx, &zerodha, &AuthToken::new("t"))
        .await
        .unwrap();

    assert_eq!(sandbox_symbols(&t, "sandbox_holdings"), ["HFCL"]);
    assert_eq!(
        sandbox_symbols(&t, "sandbox_positions"),
        ["EMBASSY", "DUAL-BE"]
    );
    assert_eq!(sandbox_symbols(&t, "sandbox_orders"), ["HFCL"]);
    assert_eq!(sandbox_symbols(&t, "sandbox_trades"), ["HFCL"]);
    assert_eq!(sandbox_symbols(&t, "sandbox_gtt"), ["HFCL"]);

    let placed = t
        .ctx
        .sandbox
        .place_order_with_quote(sell_hfcl(), Quote::ltp(dec!(260)))
        .await
        .unwrap();
    let (status, reason): (String, Option<String>) = t
        .ctx
        .sandbox
        .db()
        .with_conn(|c| {
            c.query_row(
                "SELECT order_status, rejection_reason FROM sandbox_orders WHERE orderid = ?1",
                [&placed.orderid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
        .unwrap();
    assert_eq!(status, "complete", "{reason:?}");

    let book = &t.ctx.strategy.book;
    assert!(book
        .record_order_tag("K2", "u", "swing", "HFCL", "NSE", "CNC")
        .unwrap());
    let leg = book
        .apply_fill("K2", 100.0, 260.0, "SELL")
        .unwrap()
        .unwrap();
    assert_eq!(leg["symbol"], "HFCL");
    assert_eq!(leg["quantity"], 0.0, "{leg}");
    assert_eq!(leg["realized_pnl"], 1000.0, "{leg}");
    let legs = book.get_strategy_legs(Some("u"), Some("swing")).unwrap();
    assert_eq!(legs.len(), 1, "{legs:?}");
}

/// The stored master's load moves too, so a row written under the old
/// symbol after the download (an order in flight across it), or a move
/// the app quit before, is made up at the next start.
#[tokio::test]
async fn loading_the_stored_master_moves_what_the_download_missed() {
    let (t, zerodha) = signed_in_to_zerodha();
    download(&t.ctx, &zerodha, &AuthToken::new("t"))
        .await
        .unwrap();
    populate_previous_build(&t);
    t.ctx.clear_symbol_cache();
    assert_eq!(load_cached(&t.ctx, "zerodha").await.unwrap(), 5);
    assert_eq!(sandbox_symbols(&t, "sandbox_holdings"), ["HFCL"]);
    let legs = t
        .ctx
        .strategy
        .book
        .get_strategy_legs(Some("u"), Some("swing"))
        .unwrap();
    assert_eq!(legs[0]["symbol"], "HFCL", "{legs:?}");
}
