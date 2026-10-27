//! Live orders still unconfirmed from before this version recorded the
//! account an order went through (migration 094; NEW-TR-01).
//!
//! Such a row may belong to another broker's session than the one connected
//! now, so no book read here can say it was not placed: it is never
//! released or held as absent, never counted as unresolved, and no book is
//! read for it alone more than once per session (`reconcile`): the book of
//! each new session is read once, and a book that finds it adopts it.
//! Otherwise the trader settles it: it is listed with the held orders on
//! the Health page, and once they have checked their broker their mark sets
//! it aside, after one last look at the connected broker's book (the
//! reconciler's own re-check of the books comes first, as for any held
//! order). It is told in the deployment's `elsewhere` meanwhile.

use super::reconcile::unconfirmed_of;
use super::{store, Runner};
use crate::services::live_owner::LiveOwner;
use crate::services::order_reconciler::{BookSnapshot, Found, HeldOrder};
use crate::strategy::dispatch::RunMode;

/// What the row reads once the trader has checked it at their broker.
const CHECKED: &str = "You checked this order at your broker; OpenAlgo does not follow it";

/// Whether `o` is a Live row whose account was never recorded.
pub(super) fn is_unowned_live(o: &store::OrderRow) -> bool {
    o.mode == "live" && o.broker.is_empty()
}

impl Runner {
    /// The unowned unconfirmed rows, as held orders for the trader to check.
    pub(super) fn unowned_held(&self) -> Vec<HeldOrder> {
        let rows = match self
            .db
            .conn()
            .and_then(|c| store::unconfirmed_rows(&c, "live"))
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!("Unconfirmed OpenScript orders could not be read: {}", e);
                return Vec::new();
            }
        };
        rows.into_iter()
            .filter(is_unowned_live)
            .map(|o| {
                let placed_at = chrono::DateTime::parse_from_rfc3339(&o.placed_at)
                    .ok()
                    .map(|t| t.with_timezone(&chrono::Utc));
                HeldOrder {
                    owner: String::new(),
                    id: o.id.to_string(),
                    message: format!(
                        "OpenScript {}: {} {} {} may have been placed at your broker before this version recorded which account it went through. Check your broker's order book, then mark it here.",
                        o.deployment, o.action, o.quantity, o.symbol
                    ),
                    symbol: o.symbol,
                    exchange: o.exchange,
                    action: o.action,
                    quantity: o.quantity,
                    since: placed_at.unwrap_or_else(|| self.clock.now()),
                    product: Some(o.product),
                    client_tag: o.client_tag,
                    placed_at,
                }
            })
            .collect()
    }

    /// Whether the book of `session` should still be read for unowned
    /// rows: once per session, so a sign-in gives each a chance to be found.
    pub(super) fn unowned_need_a_look(&self, session: Option<&LiveOwner>) -> bool {
        session.is_some() && self.unowned_look.lock().as_ref() != session
    }

    /// A book of `session` was read for the unowned rows.
    pub(super) fn unowned_looked(&self, session: Option<&LiveOwner>) {
        *self.unowned_look.lock() = session.cloned();
    }

    /// The trader checked an unowned unconfirmed row at their broker. One
    /// last look at the connected broker's order book first: found there, it
    /// is adopted and followed, and the trader told. Otherwise it is set
    /// aside as checked. False when it is not such a row (any more). A book
    /// that cannot be read changes nothing.
    pub(super) async fn dismiss_unowned(&self, id: &str) -> Result<bool, String> {
        let Ok(row_id) = id.parse::<i64>() else {
            return Ok(false);
        };
        let unreadable = |e: String| {
            tracing::error!("OpenScript order {} could not be checked: {}", id, e);
            "The order could not be checked just now. Nothing was changed. Try again.".to_string()
        };
        let row = self
            .db
            .conn()
            .and_then(|c| store::order_by_id(&c, row_id))
            .map_err(|e| unreadable(e.to_string()))?;
        let Some(o) = row.filter(|o| is_unowned_live(o) && o.status == store::UNCONFIRMED) else {
            return Ok(false);
        };
        let services = self.services();
        if let Some(session) = services.live_owner() {
            let rows = services
                .order_book_rows(RunMode::Live)
                .await
                .map_err(unreadable)?;
            // Which of the book's orders are the runner's, read on the
            // blocking pool (PROD-REL-02); unknown, all of them.
            let ids: Vec<String> = rows.iter().map(|r| r.order_id.clone()).collect();
            let every = ids.iter().cloned().collect();
            let owned = self
                .blocking(move |me| me.owned_among(&ids))
                .await
                .unwrap_or(every);
            let book =
                BookSnapshot::new(rows, self.clock.now()).with_session(Some(session.clone()));
            if let Found::Order(b) =
                book.find(&unconfirmed_of(&o), &|oid: &str| owned.contains(oid))
            {
                self.bind_unconfirmed(Some(&session), &o, b);
                return Ok(true);
            }
        }
        let done = self
            .db
            .conn()
            .and_then(|c| store::release_unconfirmed(&c, o.id, CHECKED, self.clock.now()))
            .map_err(|e| unreadable(e.to_string()))?;
        if done {
            self.tell(
                &o,
                &format!(
                    "{} {} {} from before this version recorded the account: {}.",
                    o.action, o.quantity, o.symbol, CHECKED
                ),
            );
        }
        Ok(done)
    }
}
