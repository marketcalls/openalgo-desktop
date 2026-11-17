//! Funds (web `api/funds.py`): `GET limits/rmssublimits/{userid}/`.
//!
//! * `data.cshAvl`: cash available;
//! * `data.mrgAvl.stkColVal + data.mrgAvl.mfOthColVal`: collateral from
//!   pledged stock and funds;
//! * `data.mrgUtd.mrgUtd`: margin used;
//! * `data.mrgUtd.rlPnl` / `data.mrgUtd.unRlMtm`: realised P&L and
//!   unrealised MTM.
//!
//! A value Nuvama leaves out reads as 0, as on the web; an answer without
//! its `data` object or its cash figure is unreadable, never a zero
//! balance (BR-03).

use super::client::{Call, Lane};
use super::mapping::num;
use super::NuvamaBroker;
use crate::brokers::common::book_read::{self, Book};
use crate::brokers::common::streaming::round2;
use crate::brokers::types::{AuthToken, Funds};
use crate::error::Result;
use serde_json::Value;

/// Funds from the `data` object of a limits answer.
pub(crate) fn funds_from(data: &Value) -> Funds {
    let available = data.get("mrgAvl");
    let utilised = data.get("mrgUtd");
    let at = |v: Option<&Value>, k: &str| num(v.and_then(|o| o.get(k)));
    let used = round2(at(utilised, "mrgUtd"));
    Funds {
        available_cash: round2(num(data.get("cshAvl"))),
        collateral: round2(at(available, "stkColVal") + at(available, "mfOthColVal")),
        m2m_unrealized: round2(at(utilised, "unRlMtm")),
        m2m_realized: round2(at(utilised, "rlPnl")),
        utilised_debits: used,
        used_margin: used,
        ..Default::default()
    }
}

pub async fn get_funds(b: &NuvamaBroker, auth: &AuthToken) -> Result<Funds> {
    let s = b.session(auth)?;
    let url = format!(
        "{}limits/rmssublimits/{}/",
        b.urls.eq,
        urlencoding::encode(&s.account_id)
    );
    let a = b.call(&s, Call::get(url, Lane::Account)).await?;
    if !a.ok() {
        return Err(a.refused("the funds"));
    }
    let data = book_read::object(a.body.get("data"), "nuvama", Book::Funds)?;
    // Without the cash figure the answer is not a limits answer at all.
    if data.get("cshAvl").is_none_or(Value::is_null) {
        return Err(book_read::unreadable("nuvama", Book::Funds));
    }
    Ok(funds_from(data))
}
