//! The two order calls Flow makes that the public `/api/v1` schemas do not
//! carry, sent to the services through an internal entry instead, so the
//! public API stays the web's (decisions.md 4):
//!
//! - a multi-leg order with a leg that names an absolute `strike` (the
//!   leg schema requires `offset`; the web reaches its strike path only
//!   because its Flow calls the service in process), parity
//!   N-optionsMultiOrder-12;
//! - a Close Positions node naming one position (`/api/v1/closeposition`
//!   takes only a strategy and closes everything; the web's live branch
//!   ignored the filter too, WEB-DEFECT CL-09).
//!
//! Each is checked as the public route checks its nearest endpoint (the
//! multi-order leg schema with the strike set aside, the smart-order
//! schema), with the run's API key and broker session, then handed to the
//! service with `Route::API`, so analyzer mode, Semi-Auto and the events
//! apply as for any API call. The call runs as a task the app owns and is
//! answered within the client's timeout; a call cut off may have placed
//! orders and says so, as `/api/v1` calls do.

use crate::flow::contract::client::app::{CALL_TIMEOUT, FLOW_CALLER};
use crate::flow::contract::client::ApiCall;
use crate::mcp::dispatch::{Raw, Transport};
use crate::security::Secret;
use crate::server::api_v1::{check, Auth, Style};
use crate::services::core::Reply;
use crate::services::{options_order_service, order_service, schemas, Route};
use crate::state::AppState;
use axum::response::Response;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// Which internal entry a call takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Internal {
    /// `optionsmultiorder` with at least one leg naming a `strike`.
    StrikeLegs,
    /// `closeposition` naming a `symbol`.
    ScopedClose,
}

/// What stands in for a strike leg's offset while the leg schema checks
/// the rest of the leg.
const OFFSET_STAND_IN: &str = "ATM";

/// The internal entry `call` needs, if any.
pub fn route(call: &ApiCall) -> Option<Internal> {
    match call.endpoint {
        "optionsmultiorder" => call
            .payload
            .get("legs")
            .and_then(Value::as_array)
            .is_some_and(|legs| legs.iter().any(has_strike))
            .then_some(Internal::StrikeLegs),
        "closeposition" => call
            .payload
            .get("symbol")
            .is_some_and(|s| !s.is_null())
            .then_some(Internal::ScopedClose),
        _ => None,
    }
}

fn has_strike(leg: &Value) -> bool {
    leg.get("strike").is_some_and(|s| !s.is_null())
}

/// Send `payload` through `which`, as a task the app owns, answered within
/// [`CALL_TIMEOUT`].
pub async fn run(
    app: &Arc<AppState>,
    api_key: &str,
    which: Internal,
    payload: Map<String, Value>,
) -> Result<Raw, Transport> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let owner = app.clone();
    let key = Secret::new(api_key);
    app.spawn(async move {
        use futures_util::FutureExt;
        // From here the request is in the services' hands: whatever goes
        // wrong may come after an order reached the broker.
        let out = std::panic::AssertUnwindSafe(serve(&owner, &key, which, payload))
            .catch_unwind()
            .await;
        if out.is_err() {
            tracing::error!("A Flow order service failed while serving a call");
        }
        let _ = tx.send(out.ok());
    });
    match tokio::time::timeout(CALL_TIMEOUT, rx).await {
        Ok(Ok(Some(raw))) => Ok(raw),
        Ok(_) => Err(Transport::NoReply),
        Err(_) => Err(Transport::Timeout),
    }
}

async fn serve(app: &AppState, key: &Secret, which: Internal, payload: Map<String, Value>) -> Raw {
    let reply = match which {
        Internal::StrikeLegs => strike_legs(app, key, payload).await,
        Internal::ScopedClose => scoped_close(app, key, payload).await,
    };
    match reply {
        Ok(r) => Raw {
            status: r.status,
            text: r.body.to_string(),
            outcome_unknown: r.is_uncertain(),
        },
        Err(early) => read_early(*early).await,
    }
}

/// The refusal the checks answered, as the route would have sent it.
async fn read_early(resp: Response) -> Raw {
    let status = resp.status().as_u16();
    match axum::body::to_bytes(resp.into_body(), 1 << 20).await {
        Ok(b) => Raw::new(status, String::from_utf8_lossy(&b).into_owned()),
        Err(e) => {
            tracing::error!("Flow could not read a refusal: {}", e);
            Raw::new(status, "")
        }
    }
}

type Checked = Result<Reply, crate::server::envelope::EarlyReply>;

/// A multi-leg order with strike legs: each strike leg is checked by the
/// leg schema with a stand-in offset, then sent with its strike and no
/// offset.
async fn strike_legs(app: &AppState, key: &Secret, mut payload: Map<String, Value>) -> Checked {
    payload.insert("apikey".into(), json!(key.expose()));
    let mut strikes: Vec<Option<Value>> = Vec::new();
    if let Some(legs) = payload.get_mut("legs").and_then(Value::as_array_mut) {
        for leg in legs.iter_mut() {
            let strike = match leg.as_object_mut() {
                Some(m) => match m.remove("strike") {
                    Some(s) if !s.is_null() => {
                        m.entry("offset").or_insert(json!(OFFSET_STAND_IN));
                        Some(s)
                    }
                    _ => None,
                },
                None => None,
            };
            strikes.push(strike);
        }
    }
    let schema = schemas::options_multi_order();
    let style = Style::Envelope("Validation error");
    let mut loaded = check(app, FLOW_CALLER, &payload, schema, style, Auth::Broker).await?;
    if let Some(legs) = loaded.get_mut("legs").and_then(Value::as_array_mut) {
        for (leg, strike) in legs.iter_mut().zip(strikes) {
            if let (Some(m), Some(strike)) = (leg.as_object_mut(), strike) {
                m.remove("offset");
                m.insert("strike".into(), strike);
            }
        }
    }
    Ok(options_order_service::options_multi_order(app, &loaded, Route::API).await)
}

/// A close of one position: checked as the smart order that closes it.
async fn scoped_close(app: &AppState, key: &Secret, payload: Map<String, Value>) -> Checked {
    let field = |k: &str| payload.get(k).cloned().unwrap_or(Value::Null);
    let mut body = Map::new();
    body.insert("apikey".into(), json!(key.expose()));
    for k in ["strategy", "symbol", "exchange", "product"] {
        body.insert(k.into(), field(k));
    }
    for (k, v) in [
        ("action", json!("BUY")),
        ("quantity", json!(0)),
        ("position_size", json!(0)),
        ("pricetype", json!("MARKET")),
    ] {
        body.insert(k.into(), v);
    }
    let schema = schemas::smart_order();
    let style = Style::PyOrder("closeposition");
    let loaded = check(app, FLOW_CALLER, &body, schema, style, Auth::Broker).await?;
    Ok(order_service::close_one_position(app, &loaded, Route::API).await)
}

#[cfg(test)]
mod tests;
