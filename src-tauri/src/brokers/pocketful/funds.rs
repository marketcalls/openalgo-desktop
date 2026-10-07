//! Funds (web `api/funds.py`): `GET /api/v2/funds/view?client_id=&type=all`,
//! `data.values` as `[label, value]` pairs. Pocketful has no margin
//! calculator (web `margin_api.py` raises), so `calculate_margin` keeps the
//! trait's `Unsupported` default.

use super::{mapping, PocketfulBroker};
use crate::brokers::types::*;
use crate::error::Result;
use reqwest::Method;

pub async fn get_funds(b: &PocketfulBroker, auth: &AuthToken) -> Result<Funds> {
    let client_id = b.client_id(auth).await?;
    let v = b
        .call(
            Method::GET,
            &format!(
                "/api/v2/funds/view?client_id={}&type=all",
                urlencoding::encode(&client_id)
            ),
            auth,
            None,
        )
        .await?;
    Ok(mapping::map_funds(&v))
}
