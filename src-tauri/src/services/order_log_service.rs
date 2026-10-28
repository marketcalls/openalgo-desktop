//! The API order log (live `/api/v1` calls, `logs.db` `order_logs`) for the
//! Logs page (web `blueprints/log.py`), between UTC bounds and matching an
//! optional search.

use crate::db::sqlite::monitor::{self as store, OrderLogRow};
use crate::error::Result;
use crate::state::AppState;
use std::sync::Arc;

/// Entries on one page of the Logs page.
pub const PER_PAGE: i64 = 20;

/// Page `page` (from 1) of the entries, newest first, and how many match.
pub fn page(
    ctx: &AppState,
    from: &str,
    to: &str,
    search: Option<&str>,
    page: i64,
) -> Result<(Vec<OrderLogRow>, i64)> {
    let c = ctx.logs.conn()?;
    store::order_logs_page(
        &c,
        from,
        to,
        search,
        Some(((page - 1) * PER_PAGE, PER_PAGE)),
    )
}

/// Every matching entry (the CSV export).
fn all(ctx: &AppState, from: &str, to: &str, search: Option<&str>) -> Result<Vec<OrderLogRow>> {
    let c = ctx.logs.conn()?;
    Ok(store::order_logs_page(&c, from, to, search, None)?.0)
}

/// Every matching entry turned into text by `render`, both off the async
/// workers: the rows are read, the connection returned, then rendered
/// (each entry's request and response parsed). On the async worker a
/// large log held the runtime for seconds.
pub async fn export<F>(
    ctx: &Arc<AppState>,
    from: String,
    to: String,
    search: Option<String>,
    render: F,
) -> Result<String>
where
    F: FnOnce(&[OrderLogRow]) -> String + Send + 'static,
{
    ctx.report(move |c| Ok(render(&all(c, &from, &to, search.as_deref())?)))
        .await
}
