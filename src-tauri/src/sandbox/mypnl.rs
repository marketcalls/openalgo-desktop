//! The My P&L page's data (web `blueprints/sandbox.py`
//! `/mypnl/api/data`): the summary, the daily history, the positions and
//! holdings with their unrealized P&L, and the latest trades.

use super::clock::display_seconds;
use super::db::{self, DailyPnlRow, HoldingRow, PositionRow, TradeRow};
use super::funds::{self, FundsRow};
use super::holdings;
use super::positions;
use super::replies::{
    MyPnlDaily, MyPnlData, MyPnlHolding, MyPnlPosition, MyPnlReply, MyPnlSummary, MyPnlTrade,
    SUCCESS,
};
use super::types::{float, money, pct, SandboxError};
use rusqlite::Connection;
use rust_decimal::Decimal;

/// The page's data for `user`.
pub(super) fn read(conn: &Connection, user: &str) -> Result<MyPnlReply, SandboxError> {
    let mut positions_rows = positions::list_user(conn, user)?;
    positions_rows.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    let holdings_rows = holdings::list_user(conn, user)?;
    let f = funds::read(conn, user)?;
    let (positions_out, pos_unrealized) = position_rows(&positions_rows);
    let (holdings_out, hold_unrealized) = holding_rows(&holdings_rows);
    let trades = latest_trades(conn, user)?;
    let daily = latest_days(conn, user)?;
    Ok(MyPnlReply {
        status: SUCCESS,
        data: MyPnlData {
            summary: summary(f.as_ref(), pos_unrealized, hold_unrealized),
            daily_pnl: daily.iter().map(daily_row).collect(),
            positions: positions_out,
            holdings: holdings_out,
            trades: trades.iter().map(trade_row).collect(),
        },
    })
}

/// The positions (an open one's P&L is unrealized) and their total
/// unrealized P&L.
fn position_rows(rows: &[PositionRow]) -> (Vec<MyPnlPosition>, Decimal) {
    let mut pos_unrealized = Decimal::ZERO;
    let out = rows
        .iter()
        .map(|p| {
            let unrealized = if p.quantity != 0 {
                p.pnl
            } else {
                Decimal::ZERO
            };
            pos_unrealized += unrealized;
            MyPnlPosition {
                symbol: p.symbol.clone(),
                exchange: p.exchange.clone(),
                product: p.product.as_str().to_string(),
                quantity: p.quantity,
                average_price: money(p.average_price),
                ltp: money(p.ltp.unwrap_or(Decimal::ZERO)),
                unrealized_pnl: money(unrealized),
                today_realized_pnl: money(p.today_realized_pnl),
                all_time_realized_pnl: money(p.accumulated_realized_pnl),
                status: if p.quantity != 0 { "Open" } else { "Closed" }.to_string(),
                updated_at: display_seconds(&p.updated_at),
            }
        })
        .collect();
    (out, pos_unrealized)
}

/// The holdings and their total unrealized P&L.
fn holding_rows(rows: &[HoldingRow]) -> (Vec<MyPnlHolding>, Decimal) {
    let mut hold_unrealized = Decimal::ZERO;
    let out = rows
        .iter()
        .map(|h| {
            hold_unrealized += h.pnl;
            MyPnlHolding {
                symbol: h.symbol.clone(),
                exchange: h.exchange.clone(),
                product: "CNC".to_string(),
                quantity: h.quantity,
                average_price: money(h.average_price),
                ltp: money(h.ltp.unwrap_or(Decimal::ZERO)),
                unrealized_pnl: money(h.pnl),
                pnl_percent: pct(h.pnl_percent),
                settlement_date: h.settlement_date.clone(),
            }
        })
        .collect();
    (out, hold_unrealized)
}

/// The latest 50 trades, newest first.
fn latest_trades(conn: &Connection, user: &str) -> rusqlite::Result<Vec<TradeRow>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM sandbox_trades WHERE user_id = ?1 ORDER BY trade_timestamp DESC, id DESC LIMIT 50",
        db::TradeRow::COLUMNS
    ))?;
    let rows = stmt.query_map(rusqlite::params![user], db::TradeRow::from_row)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
}

/// The latest 30 days of the daily P&L, newest first.
fn latest_days(conn: &Connection, user: &str) -> rusqlite::Result<Vec<DailyPnlRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT * FROM sandbox_daily_pnl WHERE user_id = ?1 ORDER BY date DESC LIMIT 30",
    )?;
    let rows = stmt.query_map(rusqlite::params![user], db::DailyPnlRow::from_row)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
}

/// Today's and all-time figures from the funds and the unrealized P&L.
fn summary(
    f: Option<&FundsRow>,
    pos_unrealized: Decimal,
    hold_unrealized: Decimal,
) -> MyPnlSummary {
    let read = |pick: fn(&FundsRow) -> Decimal| f.map(pick).unwrap_or(Decimal::ZERO);
    let today = read(|f| f.today_realized_pnl);
    let total_unrealized = pos_unrealized + hold_unrealized;
    let today_charges = read(|f| f.today_charges);
    let total_charges = read(|f| f.total_charges);
    MyPnlSummary {
        today_realized_pnl: money(today),
        all_time_realized_pnl: f.map(|f| money(f.realized_pnl)).unwrap_or(0.0),
        positions_unrealized_pnl: money(pos_unrealized),
        holdings_unrealized_pnl: money(hold_unrealized),
        total_unrealized_pnl: money(total_unrealized),
        today_total_mtm: money(today + total_unrealized),
        today_charges: money(today_charges),
        total_charges: money(total_charges),
        today_net_mtm: float(today + total_unrealized - today_charges),
        all_time_net_realized_pnl: float(read(|f| f.realized_pnl) - total_charges),
        total_pnl: f.map(|f| money(f.total_pnl)).unwrap_or(0.0),
        available_balance: f.map(|f| money(f.available_balance)).unwrap_or(0.0),
        total_capital: f.map(|f| money(f.total_capital)).unwrap_or(0.0),
    }
}

fn daily_row(d: &DailyPnlRow) -> MyPnlDaily {
    MyPnlDaily {
        date: d.date.clone(),
        realized_pnl: money(d.realized_pnl),
        positions_unrealized: money(d.positions_unrealized_pnl),
        holdings_unrealized: money(d.holdings_unrealized_pnl),
        total_unrealized: money(d.positions_unrealized_pnl + d.holdings_unrealized_pnl),
        total_mtm: money(d.total_mtm),
        charges: money(d.charges),
        net_mtm: float(d.total_mtm - d.charges),
        portfolio_value: money(d.portfolio_value),
    }
}

fn trade_row(t: &TradeRow) -> MyPnlTrade {
    MyPnlTrade {
        tradeid: t.tradeid.clone(),
        symbol: t.symbol.clone(),
        exchange: t.exchange.clone(),
        action: t.action.clone(),
        quantity: t.quantity,
        price: float(t.price),
        product: t.product.clone(),
        charges: money(t.charges.total()),
        timestamp: display_seconds(&t.trade_timestamp),
    }
}
