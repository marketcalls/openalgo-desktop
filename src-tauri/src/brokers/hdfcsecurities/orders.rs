//! Orders and books (web `api/order_api.py`).
//!
//! * place  `POST   /oapi/v1/orders/regular`            -> `data.order_id`
//! * modify `PUT    /oapi/v1/orders/regular/<order_id>`
//! * cancel `DELETE /oapi/v1/orders/regular/<order_id>`
//! * books  `GET /oapi/v1/orders`, `/oapi/v1/trades`,
//!   `/oapi/v1/portfolio/cumulative-positions` (`data.net`; the documented
//!   `overall_positions` path does not exist), `/oapi/v1/portfolio/holdings`.
//!
//! Position and holding rows carry no live price, so both books are marked
//! to `/fetch-ltp` before they are mapped.

use super::data::fetch_ltp;
use super::mapping::{self, exit_action, f, i, map_product, row_exchange, s, unwrap_rows};
use super::HdfcSecuritiesBroker;
use crate::brokers::common::mapping::{Exchange, PriceType, Product};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::Value;

fn order_id_of(data: &Value, fallback: &str) -> String {
    match data.get("order_id") {
        Some(Value::String(v)) if !v.is_empty() => v.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => fallback.to_string(),
    }
}

pub async fn place_order(
    b: &HdfcSecuritiesBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    let body = mapping::place_payload(o, b.resolver());
    let data = b
        .call(Method::POST, "/oapi/v1/orders/regular", auth, Some(&body))
        .await?;
    let id = order_id_of(&data, "");
    if id.is_empty() {
        return Err(AppError::Broker(
            "HDFC Securities accepted the request but returned no order id. Check the order book before retrying."
                .into(),
        ));
    }
    Ok(OrderResponse {
        order_id: id,
        message: None,
    })
}

pub async fn modify_order(
    b: &HdfcSecuritiesBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let body = mapping::modify_payload(m);
    let data = b
        .call(
            Method::PUT,
            &format!(
                "/oapi/v1/orders/regular/{}",
                urlencoding::encode(&m.order_id)
            ),
            auth,
            Some(&body),
        )
        .await?;
    Ok(OrderResponse {
        order_id: order_id_of(&data, &m.order_id),
        message: None,
    })
}

pub async fn cancel_order(
    b: &HdfcSecuritiesBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    let data = b
        .call(
            Method::DELETE,
            &format!("/oapi/v1/orders/regular/{}", urlencoding::encode(order_id)),
            auth,
            None,
        )
        .await?;
    Ok(OrderResponse {
        order_id: order_id_of(&data, order_id),
        message: None,
    })
}

async fn raw_list(
    b: &HdfcSecuritiesBroker,
    auth: &AuthToken,
    path: &str,
    key: Option<&str>,
) -> Result<Vec<Value>> {
    let data = b.call(Method::GET, path, auth, None).await?;
    Ok(unwrap_rows(&data, key))
}

pub(crate) async fn raw_positions(
    b: &HdfcSecuritiesBroker,
    auth: &AuthToken,
) -> Result<Vec<Value>> {
    raw_list(
        b,
        auth,
        "/oapi/v1/portfolio/cumulative-positions",
        Some("net"),
    )
    .await
}

/// Add `ltp` to rows from one batched `/fetch-ltp` pass (web
/// `_enrich_with_ltp`). Rows resolve by `security_id` (the brsymbol);
/// anything unresolvable, or a failed batch, leaves `ltp` absent.
pub(crate) async fn enrich_with_ltp(
    b: &HdfcSecuritiesBroker,
    auth: &AuthToken,
    rows: &mut [Value],
    holdings: bool,
) -> Result<()> {
    let mut wanted: Vec<(usize, (String, String))> = Vec::new();
    for (n, row) in rows.iter().enumerate() {
        let sid = s(row, "security_id");
        if sid.is_empty() {
            continue;
        }
        let exchange = if holdings {
            mapping::to_oa_exchange(&s(row, "exchange"), "EQUITY")
        } else {
            row_exchange(row)
        };
        if let Some(r) = b.resolver().by_brsymbol(&exchange, &sid) {
            wanted.push((n, (r.exchange.clone(), r.token.clone())));
        }
    }
    if wanted.is_empty() {
        return Ok(());
    }
    let keys: Vec<(String, String)> = wanted.iter().map(|(_, k)| k.clone()).collect();
    let quotes = fetch_ltp(b, auth, &keys).await?;
    for (n, k) in wanted {
        if let (Some((ltp, _)), Some(obj)) = (quotes.get(&k), rows[n].as_object_mut()) {
            obj.insert("ltp".into(), Value::from(*ltp));
        }
    }
    Ok(())
}

pub async fn get_order_book(b: &HdfcSecuritiesBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    let rows = raw_list(b, auth, "/oapi/v1/orders", None).await?;
    Ok(mapping::map_orders(&rows, b.resolver()))
}

pub async fn get_trade_book(b: &HdfcSecuritiesBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let rows = raw_list(b, auth, "/oapi/v1/trades", None).await?;
    Ok(mapping::map_trades(&rows, b.resolver()))
}

/// Raw positions marked to the live price.
pub(crate) async fn priced_positions(
    b: &HdfcSecuritiesBroker,
    auth: &AuthToken,
) -> Result<Vec<Value>> {
    let mut rows = raw_positions(b, auth).await?;
    enrich_with_ltp(b, auth, &mut rows, false).await?;
    Ok(rows)
}

pub async fn get_positions(b: &HdfcSecuritiesBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    let rows = priced_positions(b, auth).await?;
    Ok(mapping::map_positions(&rows, b.resolver()))
}

pub async fn get_holdings(b: &HdfcSecuritiesBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    let mut rows = raw_list(b, auth, "/oapi/v1/portfolio/holdings", None).await?;
    enrich_with_ltp(b, auth, &mut rows, true).await?;
    Ok(mapping::map_holdings(&rows, b.resolver()))
}

pub async fn cancel_all_orders(
    b: &HdfcSecuritiesBroker,
    auth: &AuthToken,
) -> Result<CancelAllResult> {
    let mut result = CancelAllResult::default();
    for row in raw_list(b, auth, "/oapi/v1/orders", None).await? {
        if !mapping::is_cancellable(&row) {
            continue;
        }
        let id = s(&row, "order_id");
        match cancel_order(b, auth, &id).await {
            Ok(_) => result.cancelled.push(id),
            Err(e) => {
                tracing::warn!("Cancel of order {} failed: {}", id, e.code());
                result.failed.push(id)
            }
        }
    }
    Ok(result)
}

/// Net quantity for one OpenAlgo symbol / exchange / product. The row's
/// product is compared in InvestRight vocabulary, as the web does.
pub async fn get_open_position(
    b: &HdfcSecuritiesBroker,
    auth: &AuthToken,
    symbol: &str,
    exchange: Exchange,
    product: Product,
) -> Result<i64> {
    let ex = exchange.as_str();
    let want = map_product(product, ex);
    for row in raw_positions(b, auth).await? {
        let rex = row_exchange(&row);
        if rex == ex
            && mapping::oa_symbol(&row, &rex, b.resolver()) == symbol
            && s(&row, "product").eq_ignore_ascii_case(want)
        {
            return Ok(i(&row, "net_qty"));
        }
    }
    Ok(0)
}

pub async fn close_all_positions(
    b: &HdfcSecuritiesBroker,
    auth: &AuthToken,
) -> Result<CloseAllResult> {
    let symbols = b.resolver().clone();
    let mut result = CloseAllResult::default();
    for row in raw_positions(b, auth).await? {
        let net = f(&row, "net_qty") as i64;
        if net == 0 {
            continue;
        }
        let exchange = row_exchange(&row);
        let symbol = mapping::oa_symbol(&row, &exchange, &symbols);
        let label = format!("{} ({})", symbol, exchange);
        if symbol.is_empty() {
            result
                .failed
                .push(format!("{}: the symbol could not be resolved", label));
            continue;
        }
        let req = OrderRequest {
            symbol,
            exchange: exchange.clone(),
            side: exit_action(net).as_str().into(),
            quantity: i32::try_from(net.abs()).unwrap_or(i32::MAX),
            price: 0.0,
            order_type: PriceType::Market.as_str().into(),
            product: mapping::reverse_product(&s(&row, "product"))
                .unwrap_or("MIS")
                .into(),
            validity: "DAY".into(),
            trigger_price: None,
            disclosed_quantity: None,
            amo: false,
        };
        let placed = match ResolvedOrder::resolve(&req, &symbols) {
            Ok(o) => place_order(b, auth, &o).await,
            Err(e) => Err(e),
        };
        match placed {
            Ok(r) => result.placed.push(r.order_id),
            Err(e) => {
                tracing::error!("Square-off failed for {}: {}", label, e.code());
                result
                    .failed
                    .push(format!("{}: {}", label, e.client_message()))
            }
        }
    }
    Ok(result)
}
