//! The capital the P&L Tracker's percentage is read against (docs-49, a
//! deliberate deviation). The web divides the day's MTM by a fixed
//! 1,00,000 whatever the account holds, so its percentage is right only for
//! an account of exactly that size. Here the base is the capital the rest of
//! the app shows for the mode the books are read in:
//!
//! - analyzer mode: the sandbox's capital (the sandbox funds'
//!   `total_capital`, which is the starting capital set on the Sandbox page,
//!   1 crore by default), the money every sandbox margin and P&L counts
//!   against;
//! - live: the account's margin from the broker's funds, the Dashboard's
//!   Available Balance plus Utilised (`account_service::live_funds`).
//!
//! When it cannot be read or is not positive the tracker sends `null` and
//! the page shows no percentage, never one against a made-up base.

use super::account_service::utilised;
use super::core::{analyze_mode, broker_handle};
use crate::state::AppState;
use rust_decimal::prelude::ToPrimitive;
use serde_json::{json, Value};

/// What the base is, for the page's label.
const SANDBOX: &str = "sandbox";
const ACCOUNT: &str = "account";

/// The capital and its kind, or `None` when it is not known.
async fn capital_base(ctx: &AppState) -> Option<(f64, &'static str)> {
    let analyze = analyze_mode(ctx).ok()?;
    let (amount, basis) = if analyze {
        (sandbox_capital(ctx).await?, SANDBOX)
    } else {
        (account_margin(ctx).await?, ACCOUNT)
    };
    (amount.is_finite() && amount > 0.0).then_some((amount, basis))
}

async fn sandbox_capital(ctx: &AppState) -> Option<f64> {
    let capital = match ctx.sandbox.funds_row().await {
        Ok(Some(row)) => row.total_capital,
        // No sandbox order yet, so no funds row: it opens at the starting
        // capital.
        Ok(None) => ctx.sandbox.config().await.ok()?.starting_capital,
        Err(e) => {
            tracing::warn!("P&L tracker: sandbox funds unreadable ({})", e.message);
            return None;
        }
    };
    capital.to_f64()
}

async fn account_margin(ctx: &AppState) -> Option<f64> {
    let h = broker_handle(ctx).ok()?;
    match h.broker.get_funds(&h.auth).await {
        Ok(f) => Some(f.available_cash + utilised(&f)),
        Err(e) => {
            tracing::warn!("P&L tracker: funds unavailable ({})", e.code());
            None
        }
    }
}

/// Add `capital` and `capital_basis` (`"sandbox"` or `"account"`, both
/// `null` when unknown) to the tracker's `data`.
pub(crate) async fn attach(ctx: &AppState, body: &mut Value) {
    let base = capital_base(ctx).await;
    if let Some(data) = body.get_mut("data").and_then(Value::as_object_mut) {
        data.insert("capital".into(), json!(base.map(|b| b.0)));
        data.insert("capital_basis".into(), json!(base.map(|b| b.1)));
    }
}
