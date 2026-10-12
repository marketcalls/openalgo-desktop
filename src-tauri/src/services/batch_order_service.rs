//! Basket and split orders (web `basket_order_service.py`,
//! `split_order_service.py`).
//!
//! Both answer HTTP 200 with `status: "success"` even when some legs fail;
//! each result carries its own status. Basket legs go BUY first, then SELL
//! (margin benefit), live in batches of ten; split chunks go one after
//! another with the order-rate spacing.

use super::core::{
    analyzer_error, analyzer_request, broker_handle, i, meta, order_failed, publish, s,
    safe_request, BrokerHandle, Reply,
};
use super::order_service::{
    fractional_refusal, place_live, route_to_pending, sandbox_order, Route, FRACTIONAL_REFUSED,
};
use crate::brokers::types::QuoteKey;
use crate::events::{Event, Mode};
use crate::sandbox::Quote;
use crate::state::AppState;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;

/// Web `MAX_ORDERS` for a split.
pub const MAX_SPLIT_ORDERS: i64 = 100;
/// Live basket legs placed concurrently per batch.
pub const BASKET_BATCH: usize = 10;
/// Pause between live basket batches.
pub const BASKET_BATCH_PAUSE: Duration = Duration::from_secs(1);
/// Pause between live split chunks (1 / ORDER_RATE_LIMIT).
pub const SPLIT_PAUSE: Duration = Duration::from_millis(100);

/// BUY legs first, then SELL, each group in request order.
pub fn buy_then_sell(orders: &[Value]) -> Vec<Value> {
    let side = |o: &Value| s(o, "action").to_ascii_uppercase();
    let mut out: Vec<Value> = orders
        .iter()
        .filter(|o| side(o) == "BUY")
        .cloned()
        .collect();
    out.extend(orders.iter().filter(|o| side(o) == "SELL").cloned());
    out
}

/// Split `total` into chunks of `size`: full chunks, then the remainder.
pub fn split_chunks(total: i64, size: i64) -> Vec<i64> {
    if size <= 0 {
        return Vec::new();
    }
    let mut v = vec![size; (total / size) as usize];
    if total % size > 0 {
        v.push(total % size);
    }
    v
}

/// Quotes for many instruments in one broker call, for pricing sandbox
/// legs (web: one multiquotes call per basket).
async fn prefetch(ctx: &AppState, keys: &[(String, String)]) -> HashMap<(String, String), Quote> {
    let mut out = HashMap::new();
    let Ok(h) = broker_handle(ctx) else {
        return out;
    };
    let qk: Vec<QuoteKey> = keys
        .iter()
        .filter(|(sym, ex)| ctx.symbols.by_symbol(ex, sym).is_some())
        .map(|(sym, ex)| QuoteKey::new(ex.clone(), sym.clone()))
        .collect();
    if qk.is_empty() {
        return out;
    }
    if let Ok(rows) = h.broker.get_multiquotes(&h.auth, &qk).await {
        for r in rows {
            if let Some(q) = r.data.filter(|q| q.ltp > 0.0) {
                out.insert(
                    (r.symbol, r.exchange),
                    Quote::from_f64(q.ltp, q.bid, q.ask, q.high, q.low),
                );
            }
        }
    }
    out
}

async fn sandbox_place(
    ctx: &AppState,
    leg: &Value,
    quote: Option<Quote>,
) -> Result<String, String> {
    if fractional_refusal(leg).is_some() {
        return Err(FRACTIONAL_REFUSED.to_string());
    }
    let r = match quote {
        Some(q) => {
            ctx.sandbox
                .place_order_with_quote(sandbox_order(leg), q)
                .await
        }
        None => ctx.sandbox.place_order(sandbox_order(leg)).await,
    };
    r.map(|p| p.orderid).map_err(|e| e.message)
}

/// `basketorder`.
pub async fn basket_order(ctx: &AppState, req: &Value, route: Route) -> Reply {
    if let Some(r) = route_to_pending(ctx, "basketorder", req, route) {
        return r;
    }
    let strategy = s(req, "strategy");
    let orders = req
        .get("orders")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let total = orders.len();
    let sorted: Vec<Value> = buy_then_sell(&orders)
        .into_iter()
        .map(|mut o| {
            if let Some(m) = o.as_object_mut() {
                m.insert("strategy".into(), json!(strategy));
            }
            o
        })
        .collect();
    let analyze = match route.analyze(ctx) {
        Ok(a) => a,
        Err(r) => return r,
    };
    if analyze {
        let keys: Vec<(String, String)> = sorted
            .iter()
            .map(|o| (s(o, "symbol"), s(o, "exchange")))
            .collect();
        let quotes = prefetch(ctx, &keys).await;
        let mut results = Vec::with_capacity(sorted.len());
        for (n, leg) in sorted.iter().enumerate() {
            let key = (s(leg, "symbol"), s(leg, "exchange"));
            match sandbox_place(ctx, leg, quotes.get(&key).copied()).await {
                Ok(id) => results.push(json!({
                    "symbol": key.0, "exchange": key.1, "product": s(leg, "product"),
                    "status": "success", "orderid": id,
                    "batch_order": true, "is_last_order": n + 1 == total,
                })),
                Err(m) => results.push(json!({"symbol": key.0, "status": "error", "message": m})),
            }
        }
        let reply = Reply::ok(json!({"mode": "analyze", "status": "success", "results": results}));
        publish(
            ctx,
            basket_event(
                Mode::Analyze,
                analyzer_request(req, "basketorder"),
                &reply,
                &strategy,
                total,
            ),
        );
        return reply;
    }
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    let mut results = Vec::with_capacity(sorted.len());
    for (b, batch) in sorted.chunks(BASKET_BATCH).enumerate() {
        if b > 0 {
            tokio::time::sleep(BASKET_BATCH_PAUSE).await;
        }
        let placed =
            futures_util::future::join_all(batch.iter().map(|leg| live_leg(&h, ctx, leg))).await;
        results.extend(placed);
    }
    let reply = Reply::ok(json!({"status": "success", "results": results}));
    let n = reply.body["results"].as_array().map(Vec::len).unwrap_or(0);
    publish(
        ctx,
        basket_event(Mode::Live, safe_request(req), &reply, &strategy, n),
    );
    reply
}

async fn live_leg(h: &BrokerHandle, ctx: &AppState, leg: &Value) -> Value {
    let r = match fractional_refusal(leg) {
        Some(r) => r,
        None => place_live(h, ctx, leg).await,
    };
    if r.is_success() {
        json!({
            "symbol": s(leg, "symbol"), "exchange": s(leg, "exchange"),
            "product": s(leg, "product"), "status": "success",
            "orderid": r.body.get("orderid").cloned().unwrap_or(Value::Null),
        })
    } else {
        json!({"symbol": s(leg, "symbol"), "status": "error", "message": r.message()})
    }
}

fn successes(reply: &Reply) -> i64 {
    reply.body["results"]
        .as_array()
        .map(|a| a.iter().filter(|r| r["status"] == "success").count() as i64)
        .unwrap_or(0)
}

fn basket_event(mode: Mode, request: Value, reply: &Reply, strategy: &str, total: usize) -> Event {
    Event::BasketCompleted {
        meta: meta(mode, "basketorder", request, &reply.body),
        strategy: Some(strategy.to_string()),
        successful: successes(reply),
        total: total as i64,
    }
}

fn split_event(mode: Mode, request: Value, reply: &Reply, req: &Value) -> Event {
    let total = reply.body["results"].as_array().map(Vec::len).unwrap_or(0) as i64;
    Event::SplitCompleted {
        meta: meta(mode, "splitorder", request, &reply.body),
        symbol: Some(s(req, "symbol")),
        action: Some(s(req, "action").to_ascii_uppercase()),
        exchange: Some(s(req, "exchange")),
        pricetype: Some(s(req, "pricetype")),
        product: Some(s(req, "product")),
        successful: successes(reply),
        total,
    }
}

/// `splitorder`.
pub async fn split_order(ctx: &AppState, req: &Value, route: Route) -> Reply {
    let analyze = match route.analyze(ctx) {
        Ok(a) => a,
        Err(r) => return r,
    };
    split_order_decided(ctx, req, route, analyze).await
}

/// `splitorder` to a destination the caller decided once (`analyze`: the
/// sandbox), so every chunk goes to that side whatever the toggle does
/// while they are sent.
pub async fn split_order_decided(
    ctx: &AppState,
    req: &Value,
    route: Route,
    analyze: bool,
) -> Reply {
    if let Some(r) =
        route_to_pending(ctx, "splitorder", req, route).or_else(|| fractional_refusal(req))
    {
        return r;
    }
    let size = i(req, "splitsize");
    let total = i(req, "quantity");
    let fail = |msg: &str| {
        if analyze {
            analyzer_error(ctx, "splitorder", req, msg, 400)
        } else {
            let reply = Reply::error(400, msg);
            order_failed(
                ctx,
                "splitorder",
                req,
                &reply,
                &s(req, "symbol"),
                &s(req, "exchange"),
            );
            reply
        }
    };
    if size <= 0 {
        return fail("Split size must be greater than 0");
    }
    let chunks = split_chunks(total, size);
    if chunks.len() as i64 > MAX_SPLIT_ORDERS {
        return fail(&format!(
            "Total number of orders would exceed maximum limit of {}",
            MAX_SPLIT_ORDERS
        ));
    }
    let n_orders = chunks.len();
    let leg = |qty: i64| {
        let mut o = req.clone();
        if let Some(m) = o.as_object_mut() {
            m.insert("quantity".into(), json!(qty));
        }
        o
    };
    if analyze {
        let quote = prefetch(ctx, &[(s(req, "symbol"), s(req, "exchange"))])
            .await
            .into_values()
            .next();
        let mut results = Vec::with_capacity(n_orders);
        for (k, qty) in chunks.iter().enumerate() {
            match sandbox_place(ctx, &leg(*qty), quote).await {
                Ok(id) => results.push(json!({"order_num": k + 1, "quantity": qty, "status": "success", "orderid": id})),
                Err(m) => results.push(json!({"order_num": k + 1, "quantity": qty, "status": "error", "message": m})),
            }
        }
        let reply = Reply::ok(json!({
            "mode": "analyze", "status": "success", "total_quantity": total,
            "split_size": size, "results": results,
        }));
        publish(
            ctx,
            split_event(
                Mode::Analyze,
                analyzer_request(req, "splitorder"),
                &reply,
                req,
            ),
        );
        return reply;
    }
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    let mut results = Vec::with_capacity(n_orders);
    for (k, qty) in chunks.iter().enumerate() {
        if k > 0 {
            tokio::time::sleep(SPLIT_PAUSE).await;
        }
        let r = place_live(&h, ctx, &leg(*qty)).await;
        // The remainder chunk is numbered total_orders, as on the web.
        let order_num = k + 1;
        if r.is_success() {
            results.push(
                json!({"order_num": order_num, "quantity": qty, "status": "success",
                "orderid": r.body.get("orderid").cloned().unwrap_or(Value::Null)}),
            );
        } else {
            results.push(json!({"order_num": order_num, "quantity": qty, "status": "error", "message": r.message()}));
        }
    }
    let reply = Reply::ok(json!({
        "status": "success", "total_quantity": total, "split_size": size, "results": results,
    }));
    publish(ctx, split_event(Mode::Live, safe_request(req), &reply, req));
    reply
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_aggregation() {
        assert_eq!(split_chunks(10, 3), vec![3, 3, 3, 1]);
        assert_eq!(split_chunks(9, 3), vec![3, 3, 3]);
        assert_eq!(split_chunks(2, 5), vec![2]);
        assert!(split_chunks(5, 0).is_empty());
        assert_eq!(split_chunks(1000, 10).len(), 100);
        assert_eq!(split_chunks(1001, 10).len(), 101);
    }

    #[test]
    fn basket_orders_buy_before_sell() {
        let legs = vec![
            json!({"symbol": "A", "action": "SELL"}),
            json!({"symbol": "B", "action": "buy"}),
            json!({"symbol": "C", "action": "BUY"}),
            json!({"symbol": "D", "action": "sell"}),
        ];
        let order: Vec<String> = buy_then_sell(&legs)
            .iter()
            .map(|l| s(l, "symbol"))
            .collect();
        assert_eq!(order, ["B", "C", "A", "D"]);
    }

    #[test]
    fn success_counting() {
        let r = Reply::ok(
            json!({"results": [{"status": "success"}, {"status": "error"}, {"status": "success"}]}),
        );
        assert_eq!(successes(&r), 2);
    }
}
