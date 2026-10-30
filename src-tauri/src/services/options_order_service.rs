//! Options orders (web `place_options_order_service.py`,
//! `options_multiorder_service.py`): resolve the contract from an offset,
//! then place through the order service.
//!
//! One request is one decision (PROD-TR-04): analyzer mode and the
//! Semi-Auto queue are read once, before any contract is resolved, and
//! every leg and chunk goes where that decision said, whatever either
//! toggle does while they are resolved and sent. The web reads both again
//! for each order it places.
//!
//! An order with no definite answer from the broker may have been placed
//! (LOG-08): a split holds back the chunks after it, and the reply carries
//! that outcome (`Reply::placement`) beside the web's wire shape, for the
//! Action Center and every other in-process caller (PROD-TR-02).
//!
//! A multi-order leg may name an absolute `strike` instead of an `offset`
//! (web `resolve_leg_symbol_by_strike`). The public `/api/v1` leg schema
//! requires `offset`, as the web's does, so only Flow's internal entry
//! (`flow::nodes::internal`) and the Action Center's replay of a request it
//! queued reach that path; the public API is not widened.

use super::batch_order_service::{
    buy_then_sell, split_chunks, too_many_orders, SplitRows, SPLIT_PAUSE,
};
use super::core::{
    analyzer_request, broker_handle, i, meta, mode_of, num, publish, s, safe_request, BrokerHandle,
    Reply,
};
use super::options_service::{
    option_exchange, parse_underlying, resolve_option, OptionPick, ResolvedOption,
};
use super::order_service::{place_admitted, route_to_pending, uncertain_legs, Route};
use crate::brokers::common::master_contract::format_strike;
use crate::events::Event;
use crate::state::AppState;
use serde_json::{json, Map, Value};

/// The order body for one resolved contract.
fn order_body(req: &Value, leg: &Value, opt: &ResolvedOption, quantity: i64) -> Value {
    let pick = |k: &str| leg.get(k).cloned().unwrap_or(Value::Null);
    json!({
        "apikey": req.get("apikey").cloned().unwrap_or(Value::Null),
        "strategy": s(req, "strategy"),
        "exchange": opt.exchange,
        "symbol": opt.symbol,
        "action": s(leg, "action").to_ascii_uppercase(),
        "pricetype": pick("pricetype"),
        "product": pick("product"),
        "price": pick("price"),
        "trigger_price": pick("trigger_price"),
        "disclosed_quantity": pick("disclosed_quantity"),
        "underlying_ltp": opt.underlying_ltp,
        "quantity": quantity,
    })
}

/// Place one leg's chunks one after another, to the side decided once
/// (`analyze`). After a chunk with no definite answer the rest are held
/// back and answered as `splitorder` answers them (LOG-08 P1-5; the web
/// keeps sending).
async fn place_split(
    ctx: &AppState,
    analyze: bool,
    req: &Value,
    leg: &Value,
    opt: &ResolvedOption,
    chunks: &[i64],
) -> SplitRows {
    let mut rows = SplitRows::new(chunks.len());
    for (k, qty) in chunks.iter().enumerate() {
        let order_num = k + 1;
        if rows.hold(order_num, *qty) {
            continue;
        }
        if k > 0 {
            tokio::time::sleep(SPLIT_PAUSE).await;
        }
        let r = place_admitted(ctx, &order_body(req, leg, opt, *qty), analyze, false).await;
        rows.record(order_num, *qty, &r);
    }
    rows
}

/// `optionsorder`.
pub async fn options_order(ctx: &AppState, req: &Value, route: Route) -> Reply {
    if let Some(r) = route_to_pending(ctx, "optionsorder", req, route) {
        return r;
    }
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    let analyze = match route.analyze(ctx) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let opt = match resolve_option(ctx, &h, &OptionPick::from_request(req)).await {
        Ok(o) => o,
        Err(r) => return r,
    };
    let quantity = i(req, "quantity");
    let splitsize = i(req, "splitsize");
    let common = |m: &mut Map<String, Value>| {
        m.insert("symbol".into(), json!(opt.symbol));
        m.insert("exchange".into(), json!(opt.exchange));
        m.insert("underlying".into(), json!(s(req, "underlying")));
        m.insert("underlying_ltp".into(), num(opt.underlying_ltp));
        m.insert("offset".into(), json!(s(req, "offset")));
        m.insert(
            "option_type".into(),
            json!(s(req, "option_type").to_ascii_uppercase()),
        );
    };
    if splitsize > 0 {
        let Some(chunks) = split_chunks(quantity, splitsize) else {
            return Reply::error(400, too_many_orders());
        };
        let split = place_split(ctx, analyze, req, req, &opt, &chunks).await;
        let successful = split
            .results
            .iter()
            .filter(|r| r["status"] == "success")
            .count() as i64;
        let total = split.results.len() as i64;
        let mut m = Map::new();
        m.insert("status".into(), json!("success"));
        common(&mut m);
        m.insert("total_quantity".into(), json!(quantity));
        m.insert("split_size".into(), json!(splitsize));
        m.insert("results".into(), json!(split.results));
        if analyze {
            m.insert("mode".into(), json!("analyze"));
        }
        let mut reply = Reply::ok(Value::Object(m));
        reply.placement = uncertain_legs(&split.uncertain);
        publish(
            ctx,
            Event::OptionsCompleted {
                meta: meta(
                    mode_of(analyze),
                    "optionsorder",
                    analyzer_request(req, "optionsorder"),
                    &reply.body,
                ),
                symbol: opt.symbol.clone(),
                action: s(req, "action").to_ascii_uppercase(),
                exchange: opt.exchange.clone(),
                pricetype: Some(s(req, "pricetype")),
                product: Some(s(req, "product")),
                successful,
                total,
            },
        );
        return reply;
    }
    let r = place_admitted(ctx, &order_body(req, req, &opt, quantity), analyze, true).await;
    if !r.is_success() {
        return r;
    }
    let mut m = Map::new();
    m.insert("status".into(), json!("success"));
    m.insert(
        "orderid".into(),
        r.body.get("orderid").cloned().unwrap_or(Value::Null),
    );
    common(&mut m);
    if let Some(mode) = r.body.get("mode") {
        m.insert("mode".into(), mode.clone());
    }
    let mut reply = Reply::new(r.status, Value::Object(m));
    reply.placement = r.placement;
    reply
}

/// `optionsmultiorder`.
pub async fn options_multi_order(ctx: &AppState, req: &Value, route: Route) -> Reply {
    if let Some(r) = route_to_pending(ctx, "optionsmultiorder", req, route) {
        return r;
    }
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    let analyze = match route.analyze(ctx) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let legs = numbered_legs(req);
    if legs.is_empty() {
        let msg = "No legs provided in the request";
        return if analyze {
            super::core::analyzer_error(ctx, "optionsmultiorder", req, msg, 400)
        } else {
            Reply::error(400, msg)
        };
    }
    let shared_ltp = shared_underlying_ltp(ctx, &h, req, &legs).await;
    let order = MultiOrder {
        ctx,
        h: &h,
        req,
        analyze,
        shared_ltp,
    };
    let mut results: Vec<(i64, Value)> = Vec::with_capacity(legs.len());
    let mut uncertain = Vec::new();
    let has_split = legs.iter().any(|l| i(l, "splitsize") > 0);
    for (n, leg) in buy_then_sell(&legs).iter().enumerate() {
        if n > 0 && has_split {
            tokio::time::sleep(SPLIT_PAUSE).await;
        }
        let (entry, unknown) = order.leg(leg).await;
        uncertain.extend(unknown);
        results.push((i(leg, "_leg"), entry));
    }
    multi_order_reply(ctx, req, results, &uncertain, shared_ltp, analyze)
}

/// The legs of a multi-order, each numbered in `_leg` (from 1) in request
/// order.
fn numbered_legs(req: &Value) -> Vec<Value> {
    req.get("legs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .map(|(n, mut l)| {
            if let Some(m) = l.as_object_mut() {
                m.insert("_leg".into(), json!(n + 1));
            }
            l
        })
        .collect()
}

/// The underlying's LTP once, shared by every leg: from the ATM call of the
/// request's expiry, else the first leg's.
async fn shared_underlying_ltp(
    ctx: &AppState,
    h: &BrokerHandle,
    req: &Value,
    legs: &[Value],
) -> Option<f64> {
    let expiry = req.get("expiry_date").and_then(Value::as_str).or_else(|| {
        legs.iter()
            .find_map(|l| l.get("expiry_date").and_then(Value::as_str))
    });
    let named = OptionPick::from_request(req);
    let atm = OptionPick::atm(named.underlying, named.exchange, expiry, "CE");
    resolve_option(ctx, h, &atm)
        .await
        .ok()
        .map(|o| o.underlying_ltp)
}

/// One `optionsmultiorder` request: what every leg shares.
struct MultiOrder<'a> {
    ctx: &'a AppState,
    h: &'a BrokerHandle,
    req: &'a Value,
    /// Where every leg goes, decided once (the sandbox when set).
    analyze: bool,
    /// The underlying's LTP, resolved once.
    shared_ltp: Option<f64>,
}

/// A leg's entry in the reply, and those of its orders that have no
/// definite answer from the broker: they may have been placed (LOG-08).
type LegOutcome = (Value, Vec<String>);

/// What a leg's reply entry repeats from the leg: its offset or its
/// strike (the other is `null`), side and action.
struct LegHead {
    leg_no: i64,
    offset: Value,
    strike: Value,
    ot: String,
    action: String,
}

impl MultiOrder<'_> {
    /// Resolve and place one leg.
    async fn leg(&self, leg: &Value) -> LegOutcome {
        let strike = leg.get("strike").filter(|v| !v.is_null()).cloned();
        let head = LegHead {
            leg_no: i(leg, "_leg"),
            offset: match &strike {
                Some(_) => leg.get("offset").cloned().unwrap_or(Value::Null),
                None => json!(s(leg, "offset")),
            },
            strike: strike.clone().unwrap_or(Value::Null),
            ot: s(leg, "option_type").to_ascii_uppercase(),
            action: s(leg, "action").to_ascii_uppercase(),
        };
        let expiry = leg
            .get("expiry_date")
            .and_then(Value::as_str)
            .or_else(|| self.req.get("expiry_date").and_then(Value::as_str));
        let resolved = match &strike {
            Some(strike) => self.by_strike(expiry, strike, &s(leg, "option_type")),
            None => {
                let offset = s(leg, "offset");
                let pick = OptionPick {
                    expiry_date: expiry,
                    offset: &offset,
                    option_type: &head.ot,
                    known_ltp: self.shared_ltp,
                    ..OptionPick::from_request(self.req)
                };
                resolve_option(self.ctx, self.h, &pick).await
            }
        };
        match resolved {
            Ok(opt) if i(leg, "splitsize") > 0 => self.place_split_leg(leg, &opt, &head).await,
            Ok(opt) => self.place_leg(leg, &opt, &head).await,
            Err(r) => (
                json!({
                    "leg": head.leg_no, "offset": head.offset, "strike": head.strike,
                    "option_type": head.ot, "action": head.action, "status": "error",
                    "message": r.message(),
                }),
                Vec::new(),
            ),
        }
    }

    /// A leg that names its own strike (web `resolve_leg_symbol_by_strike`):
    /// the strike is taken as given and only has to be listed in the
    /// master contract.
    fn by_strike(
        &self,
        expiry: Option<&str>,
        strike: &Value,
        option_type: &str,
    ) -> Result<ResolvedOption, Reply> {
        let value = match strike {
            Value::Number(n) => n.as_f64().filter(|f| f.is_finite() && *f > 0.0),
            _ => None,
        };
        let Some(value) = value else {
            return Err(Reply::error(
                400,
                format!(
                    "Invalid strike: {}. Strike must be a positive number.",
                    py_text(strike, true)
                ),
            ));
        };
        let ot = option_type.trim().to_ascii_uppercase();
        if ot != "CE" && ot != "PE" {
            return Err(Reply::error(
                400,
                format!(
                    "Invalid option_type: '{}'. Supported option types are CE and PE.",
                    option_type
                ),
            ));
        }
        let (base, embedded) = parse_underlying(&s(self.req, "underlying"));
        let Some(expiry) = embedded
            .or(expiry.map(str::to_string))
            .filter(|e| !e.is_empty())
        else {
            return Err(Reply::error(
                400,
                "No expiry date available to resolve the leg symbol.",
            ));
        };
        let opt_ex = option_exchange(&s(self.req, "exchange"));
        let symbol = format!("{}{}{}{}", base, expiry, format_strike(value), ot);
        let Some(row) = self.ctx.symbols.by_symbol(&opt_ex, &symbol) else {
            return Err(Reply::error(
                404,
                format!(
                    "Option symbol {} not found in {}. Check that strike {} is listed for {} {}, or re-download the master contract.",
                    symbol, opt_ex, py_text(strike, false), base, expiry
                ),
            ));
        };
        Ok(ResolvedOption {
            freeze_qty: self.ctx.freeze.for_option(&row.symbol, &row.exchange),
            symbol: row.symbol,
            exchange: row.exchange,
            lotsize: i64::from(row.lot_size),
            tick_size: row.tick_size,
            underlying_ltp: self.shared_ltp.unwrap_or(f64::NAN),
            strike: value,
        })
    }

    /// One leg in chunks of its `splitsize`.
    async fn place_split_leg(
        &self,
        leg: &Value,
        opt: &ResolvedOption,
        head: &LegHead,
    ) -> LegOutcome {
        let (leg_no, offset, ot, action) = (head.leg_no, &head.offset, &head.ot, &head.action);
        let strike = &head.strike;
        let qty = i(leg, "quantity");
        let size = i(leg, "splitsize");
        let Some(chunks) = split_chunks(qty, size) else {
            let entry = json!({
                "leg": leg_no, "symbol": opt.symbol, "exchange": opt.exchange, "offset": offset,
                "strike": strike, "option_type": ot, "action": action, "status": "error",
                "message": "Split orders would exceed maximum limit of 100 per leg",
            });
            return (entry, Vec::new());
        };
        let split = place_split(self.ctx, self.analyze, self.req, leg, opt, &chunks).await;
        let any_ok = split.results.iter().any(|r| r["status"] == "success");
        let uncertain = split
            .uncertain
            .iter()
            .map(|order| format!("leg {} {}", leg_no, order))
            .collect();
        let entry = json!({
            "leg": leg_no, "symbol": opt.symbol, "exchange": opt.exchange,
            "product": s(leg, "product"), "offset": offset, "strike": strike,
            "option_type": ot, "action": action,
            "status": if any_ok { "success" } else { "error" },
            "total_quantity": qty, "split_size": size, "split_results": split.results,
            "mode": if self.analyze { "analyze" } else { "live" },
        });
        (entry, uncertain)
    }

    /// One leg as one order.
    async fn place_leg(&self, leg: &Value, opt: &ResolvedOption, head: &LegHead) -> LegOutcome {
        let (leg_no, offset, ot, action) = (head.leg_no, &head.offset, &head.ot, &head.action);
        let strike = &head.strike;
        let body = order_body(self.req, leg, opt, i(leg, "quantity"));
        let r = place_admitted(self.ctx, &body, self.analyze, false).await;
        let uncertain = r
            .is_uncertain()
            .then(|| {
                format!(
                    "leg {} ({} {} {})",
                    leg_no, action, opt.symbol, opt.exchange
                )
            })
            .into_iter()
            .collect();
        if r.is_success() {
            let entry = json!({
                "leg": leg_no, "symbol": opt.symbol, "exchange": opt.exchange,
                "product": s(leg, "product"), "offset": offset, "strike": strike,
                "option_type": ot, "action": action, "status": "success",
                "orderid": r.body.get("orderid").cloned().unwrap_or(Value::Null),
                "mode": r.body.get("mode").cloned().unwrap_or(json!("live")),
            });
            return (entry, uncertain);
        }
        let msg = r.message();
        let entry = json!({
            "leg": leg_no, "symbol": opt.symbol, "exchange": opt.exchange, "offset": offset,
            "strike": strike, "option_type": ot, "action": action, "status": "error",
            "message": if msg.is_empty() { "Order placement failed".to_string() } else { msg },
        });
        (entry, uncertain)
    }
}

/// Python's `repr()` (`repr`) or `str()` of a leg value, as the web's
/// strike messages print it.
fn py_text(v: &Value, repr: bool) -> String {
    match v {
        Value::String(t) if repr => super::schema::py_repr_str(t),
        Value::String(t) => t.clone(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Null => "None".to_string(),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i.to_string(),
            None => super::schemas::py_float_repr(n.as_f64().unwrap_or(f64::NAN)),
        },
        other => other.to_string(),
    }
}

/// The `optionsmultiorder` reply, legs in request order, and its event.
/// Orders with no definite answer (`uncertain`) travel beside the web's
/// shape (P1-5).
fn multi_order_reply(
    ctx: &AppState,
    req: &Value,
    mut results: Vec<(i64, Value)>,
    uncertain: &[String],
    shared_ltp: Option<f64>,
    analyze: bool,
) -> Reply {
    results.sort_by_key(|(n, _)| *n);
    let results: Vec<Value> = results.into_iter().map(|(_, v)| v).collect();
    let successful = results.iter().filter(|r| r["status"] == "success").count() as i64;
    let total = results.len() as i64;
    let mut m = Map::new();
    m.insert("status".into(), json!("success"));
    m.insert("underlying".into(), json!(s(req, "underlying")));
    m.insert(
        "underlying_ltp".into(),
        shared_ltp.map(num).unwrap_or(Value::Null),
    );
    m.insert("results".into(), json!(results));
    if analyze {
        m.insert("mode".into(), json!("analyze"));
    }
    let mut reply = Reply::ok(Value::Object(m));
    reply.placement = uncertain_legs(uncertain);
    let request = if analyze {
        analyzer_request(req, "optionsmultiorder")
    } else {
        safe_request(req)
    };
    publish(
        ctx,
        Event::MultiOrderCompleted {
            meta: meta(mode_of(analyze), "optionsmultiorder", request, &reply.body),
            underlying: s(req, "underlying"),
            strategy: Some(s(req, "strategy")),
            exchange: s(req, "exchange"),
            successful_legs: successful,
            failed_legs: total - successful,
            total,
        },
    );
    reply
}
