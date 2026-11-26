//! The [`Sandbox`] calls behind `/sandbox/commissions`, `/sandbox/calculator`
//! and `/api/v1/charges` (web `blueprints/sandbox.py` charge routes and
//! `services/charges_service.py`). Validation failures come back as 400
//! [`SandboxError`]s carrying the web's sentence; storage failures as 500.

use super::report::{self, ReportRange, REPORT_TRADE_LIMIT};
use super::settings;
use crate::sandbox::charges::{default_config_values, trade_value, Market};
use crate::sandbox::core::blocking;
use crate::sandbox::{Sandbox, SandboxError, SbResult};
use rust_decimal::Decimal;
use serde_json::Value;

/// `/api/v1/charges` for an order whose price times quantity is past what
/// charges are worked out for (`charges::trade_value`). The web prices any
/// size; here such a figure would end the app, so it is refused.
const TRADE_TOO_LARGE: &str =
    "The trade value is too large to calculate charges. Check the price and the quantity.";

/// One order to price for `/api/v1/charges`, already validated.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderCharges {
    pub symbol: String,
    pub exchange: String,
    pub product: String,
    pub action: String,
    pub quantity: Decimal,
    pub price: Decimal,
}

impl Sandbox {
    /// `GET /sandbox/commissions/api/settings`: the plan and the statutory
    /// schedule of `market`.
    pub async fn charge_settings(&self, market: Market) -> SbResult<Value> {
        blocking(self.core(), move |c| {
            Ok(c.db.with_conn(|conn| settings::settings(conn, market))?)
        })
        .await
    }

    /// `POST /sandbox/commissions/api/settings`: the switch and the plan,
    /// checked whole and written in one commit. Returns the sentence and the
    /// settings now stored.
    pub async fn save_charge_settings(
        &self,
        payload: Value,
        market: Market,
    ) -> SbResult<(&'static str, Value)> {
        let values = settings::values_to_save(&payload).map_err(SandboxError::bad_request)?;
        blocking(self.core(), move |c| {
            let now = c.now_ts();
            Ok(c.db.with_tx(|tx| {
                settings::write(tx, &values, &now)?;
                settings::settings(tx, market)
            })?)
        })
        .await
        .map(|s| (settings::SAVED, s))
    }

    /// `POST /sandbox/commissions/api/reset`: the plan and the switch back
    /// to their defaults. Trades and funds are not touched.
    pub async fn reset_charge_settings(&self, market: Market) -> SbResult<Value> {
        blocking(self.core(), move |c| {
            let now = c.now_ts();
            Ok(c.db.with_tx(|tx| {
                settings::write(tx, &default_config_values(), &now)?;
                settings::settings(tx, market)
            })?)
        })
        .await
    }

    /// `POST /sandbox/calculator/api/estimate`: a round trip under the plan
    /// the payload names, or the saved one.
    pub async fn estimate_charges(&self, payload: Value, market: Market) -> SbResult<Value> {
        blocking(self.core(), move |c| {
            let r =
                c.db.with_conn(|conn| settings::estimate(conn, &payload, market))?;
            r.map_err(SandboxError::bad_request)
        })
        .await
    }

    /// The report's range from its two date fields, counted in IST days
    /// from the injected clock.
    pub fn report_range(&self, from: Option<&str>, to: Option<&str>) -> SbResult<ReportRange> {
        report::parse_report_range(from, to, self.core().now().date())
            .map_err(SandboxError::bad_request)
    }

    /// `GET /sandbox/commissions/api/report`.
    pub async fn cost_report(&self, range: ReportRange) -> SbResult<Value> {
        blocking(self.core(), move |c| {
            Ok(c.db
                .with_conn(|conn| report::cost_report(conn, c.user(), range, REPORT_TRADE_LIMIT))?)
        })
        .await
    }

    /// Every trade in the range with its charges, oldest first, for the
    /// report's CSV (no limit).
    pub async fn cost_report_trades(&self, range: ReportRange) -> SbResult<Vec<Value>> {
        blocking(self.core(), move |c| {
            let rows =
                c.db.with_conn(|conn| report::trades_in(conn, c.user(), range))?;
            Ok(rows.iter().map(report::trade_row).collect())
        })
        .await
    }

    /// What one order would be charged under the saved plan (web
    /// `charges_for_order`): refused when the symbol is not in the master
    /// or orders cannot be placed on its exchange.
    pub async fn order_charges(&self, order: OrderCharges) -> SbResult<Value> {
        let Some(meta) = self.core().symbol(&order.symbol, &order.exchange) else {
            return Err(SandboxError::bad_request(format!(
                "Symbol {} not found on {}",
                order.symbol, order.exchange
            )));
        };
        let contract_value = if meta.contract_value > Decimal::ZERO {
            meta.contract_value
        } else {
            Decimal::ONE
        };
        let Some(value) = trade_value(order.price, order.quantity.abs(), contract_value) else {
            return Err(SandboxError::bad_request(TRADE_TOO_LARGE));
        };
        blocking(self.core(), move |c| {
            let priced =
                c.db.with_conn(|conn| super::order_charges(conn, &order, value))?;
            priced.ok_or_else(|| {
                SandboxError::bad_request(format!(
                    "Orders cannot be placed on {}, so no charges apply to it",
                    order.exchange
                ))
            })
        })
        .await
    }
}
