//! What each order tool means (the web's per-tool `plan()` closures): its
//! arguments checked, the guard's reads made and its verdict, and the call
//! to send. The pipeline owns the order of the steps; this owns the
//! meaning of each tool.
//!
//! Analyzer routing and Semi-Auto are the platform's, as for every
//! `/api/v1` client: with analyzer mode on the order goes to the sandbox
//! and the reply says so; in Semi-Auto a new order (place, smart order,
//! close one position) is queued in the Action Center and the reply says
//! so, while modify, cancel, cancel all and close all are refused there
//! with the platform's sentence. No tool passes a destination of its own.

use super::pipeline::{Sent, DISPATCH_TIMEOUT, PREPARE_TIMEOUT};
use super::reads::{self, Lookup};
use super::smart;
use super::validate::{self, arg, Check};
use super::AGENT_STRATEGY;
use crate::agent::contract::tool::{ToolCtx, ToolOutcome};
use crate::agent::safety::guard;
use crate::agent::safety::risk::{modify_args, ExistingOrder, Verdict};
use crate::agent::tools::order_vocab::split_exchange_prefix;
use crate::services::order_service::SmartDecision;
use serde_json::{json, Map, Value};

/// A tool's checked intention, ready to send.
pub(super) struct Plan {
    pub verdict: Verdict,
    pub dispatch: Dispatch,
    /// The service the web names in its messages.
    pub label: &'static str,
    /// Fields added to a successful result (the order as sent).
    pub extra: Map<String, Value>,
}

/// The call a plan sends.
pub(super) enum Dispatch {
    Post {
        endpoint: &'static str,
        payload: Map<String, Value>,
    },
    /// Square one position off: read the held quantity, then a smart order
    /// to a target of zero (the platform's own square-off).
    SquareOff {
        symbol: String,
        exchange: String,
        product: String,
    },
}

/// Why there is no plan, so nothing is sent.
pub(super) enum Unplanned {
    /// A fixable argument: the model reads the correction.
    Retry(String),
    /// The platform failed before anything was sent (logged, not shown).
    Internal(String),
    /// What the check needs could not be confirmed (the order book, the
    /// position held), so it was refused rather than checked against the
    /// model's word: `stage` names what, `message` is the trader's sentence.
    Unverified {
        stage: &'static str,
        message: String,
    },
    /// The position is already where the smart order asks: nothing to
    /// place, as the platform's own smart order would answer.
    NothingToPlace {
        message: String,
        extra: Map<String, Value>,
    },
}

impl From<ToolOutcome> for Unplanned {
    fn from(o: ToolOutcome) -> Unplanned {
        Unplanned::Retry(o.text().to_string())
    }
}

fn internal(e: crate::error::AppError) -> Unplanned {
    Unplanned::Internal(format!("{}: {}", "AppError", e))
}

fn session(ctx: &ToolCtx) -> String {
    guard::session_key(&ctx.session_id, ctx.conversation_id)
}

fn extra(fields: &[(&str, Value)]) -> Map<String, Value> {
    fields
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

/// The last traded price and the funds, read together within
/// [`PREPARE_TIMEOUT`]; a read that does not arrive is unknown.
async fn price_and_funds(
    ctx: &ToolCtx,
    order: &Map<String, Value>,
) -> Result<(Value, Value), Unplanned> {
    let symbol = arg(order, "symbol").as_str().unwrap_or_default();
    let exchange = arg(order, "exchange").as_str().unwrap_or_default();
    let both = async {
        tokio::join!(
            reads::last_traded_price(ctx, symbol, exchange),
            reads::available_funds(ctx)
        )
    };
    match tokio::time::timeout(PREPARE_TIMEOUT, both).await {
        Ok((ltp, funds)) => Ok((ltp.unwrap_or(Value::Null), funds.unwrap_or(Value::Null))),
        Err(_) => Err(Unplanned::Internal(format!(
            "Timeout: the reads before the risk check took longer than {} seconds",
            PREPARE_TIMEOUT.as_secs()
        ))),
    }
}

/// The guard's view of a new order of `quantity` units.
fn guard_args(
    order: &Map<String, Value>,
    quantity: i64,
    ltp: Value,
    funds: Value,
) -> Map<String, Value> {
    extra(&[
        ("symbol", arg(order, "symbol").clone()),
        ("exchange", arg(order, "exchange").clone()),
        ("action", arg(order, "action").clone()),
        ("quantity", json!(quantity)),
        ("product", arg(order, "product").clone()),
        ("price_type", arg(order, "pricetype").clone()),
        ("price", arg(order, "price").clone()),
        ("trigger_price", arg(order, "trigger_price").clone()),
        ("ltp", ltp),
        ("available_funds", funds),
    ])
}

pub(super) async fn place_order(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<Plan, Unplanned> {
    let order = validate::build_order(args, false)?;
    let (ltp, funds) = price_and_funds(ctx, &order).await?;
    let quantity = arg(&order, "quantity").as_i64().unwrap_or(0);
    let verdict = guard::check_order(
        &ctx.app,
        &session(ctx),
        guard_args(&order, quantity, ltp, funds),
        reads::analyzer_mode(ctx),
    )
    .await
    .map_err(internal)?;
    Ok(Plan {
        verdict,
        extra: extra(&[("order", Value::Object(order.clone()))]),
        dispatch: Dispatch::Post {
            endpoint: "placeorder",
            payload: order,
        },
        label: "place_order_service.place_order",
    })
}

/// A smart order's arguments checked: the order, its target position and
/// its quantity. Both at zero would flatten whatever is held, which is
/// refused as the web refuses it.
pub(super) fn smart_order(args: &Map<String, Value>) -> Check<(Map<String, Value>, i64, i64)> {
    let target = validate::whole_number("position_size", arg(args, "position_size"), None)?;
    let mut fields = args.clone();
    if arg(args, "quantity").is_null() {
        fields.insert("quantity".into(), json!(0));
    }
    let mut order = validate::build_order(&fields, true)?;
    order.insert("position_size".into(), json!(target));
    let quantity = arg(&order, "quantity").as_i64().unwrap_or(0);
    if target.abs().max(quantity) <= 0 {
        return Err(ToolOutcome::invalid_argument(
            "position_size",
            "a smart order with both quantity and position_size at zero flattens whatever the account happens to be holding, which is not something to ask for by accident.",
            Some("Use the close_position tool to flatten a position, or give a non-zero position_size for the position you want to end up with."),
        ));
    }
    Ok((order, target, quantity))
}

fn text(order: &Map<String, Value>, key: &str) -> String {
    arg(order, key).as_str().unwrap_or_default().to_string()
}

/// The guard checks the order the smart order will send (security review,
/// 2026-10-27): the difference between the position held and the target,
/// with its own side, decided by the platform's smart-order table. The
/// web checked the larger of the target and the quantity, half a reversal.
pub(super) async fn place_smart_order(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<Plan, Unplanned> {
    let (order, target, quantity) = smart_order(args)?;
    let (symbol, exchange, product) = (
        text(&order, "symbol"),
        text(&order, "exchange"),
        text(&order, "product"),
    );
    let Some(held) = reads::open_position_quantity(ctx, &symbol, &exchange, &product).await else {
        return Err(Unplanned::Unverified {
            stage: "position",
            message: smart::position_unreadable(&symbol, &exchange, &product),
        });
    };
    let (side, size) = match smart::decide(held, target, quantity, &text(&order, "action")) {
        SmartDecision::Place { action, quantity } => (action, quantity),
        SmartDecision::NoAction(message) => {
            return Err(Unplanned::NothingToPlace {
                message: message.to_string(),
                extra: extra(&[
                    ("order", Value::Object(order)),
                    ("held_quantity", json!(held)),
                ]),
            })
        }
    };
    let mut sent = order.clone();
    sent.insert("action".into(), json!(side));
    let (ltp, funds) = price_and_funds(ctx, &sent).await?;
    let verdict = guard::check_order(
        &ctx.app,
        &session(ctx),
        guard_args(&sent, size, ltp, funds),
        reads::analyzer_mode(ctx),
    )
    .await
    .map_err(internal)?;
    Ok(Plan {
        verdict,
        extra: extra(&[
            ("order", Value::Object(order.clone())),
            ("risk_checked_quantity", json!(size)),
            ("held_quantity", json!(held)),
            (
                "sends",
                smart::describe(held, target, quantity, &text(&order, "action")),
            ),
        ]),
        dispatch: Dispatch::Post {
            endpoint: "placesmartorder",
            payload: order,
        },
        label: "place_smart_order_service.place_smart_order",
    })
}

/// The order a modification changes, from the order book; refused when it
/// is not there or the book cannot be read (security review, 2026-10-27).
async fn existing_order(ctx: &ToolCtx, identifier: &str) -> Result<ExistingOrder, Unplanned> {
    let message = match reads::order_record(ctx, identifier).await {
        Lookup::Found(order) => return Ok(order),
        Lookup::NotFound => format!(
            "Order {} is not in the order book, so the change could not be checked against the order it would modify, and nothing was sent. Read the order id from the order book and ask again with it exactly as shown.",
            identifier
        ),
        Lookup::Unreadable => "The order book could not be read, so the change could not be checked against the order it would modify, and nothing was sent. Tell the operator the order book is not available right now, and ask again once it is.".to_string(),
    };
    Err(Unplanned::Unverified {
        stage: "order",
        message,
    })
}

/// D-007: the modified order meets the symbol, size, price and notional
/// rules against the order it changes, whose last traded price is read.
pub(super) async fn modify_order(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<Plan, Unplanned> {
    let identifier = validate::order_id(arg(args, "order_id"))?;
    let mut order = validate::build_order(args, false)?;
    order.insert("orderid".into(), json!(identifier));
    let existing = existing_order(ctx, &identifier).await?;
    let ltp = tokio::time::timeout(
        PREPARE_TIMEOUT,
        reads::last_traded_price(ctx, &existing.symbol, &existing.exchange),
    )
    .await
    .map_err(|_| {
        Unplanned::Internal("Timeout: the quote before the risk check did not arrive".into())
    })?;
    let details = extra(&[
        ("orderid", json!(identifier)),
        ("symbol", json!(existing.symbol)),
        ("exchange", json!(existing.exchange)),
        ("quantity", arg(&order, "quantity").clone()),
    ]);
    let verdict = guard::check_modify(
        &ctx.app,
        modify_args(&order, ltp.as_ref()),
        existing,
        reads::analyzer_mode(ctx),
        details,
    )
    .await
    .map_err(internal)?;
    Ok(Plan {
        verdict,
        extra: extra(&[("order", Value::Object(order.clone()))]),
        dispatch: Dispatch::Post {
            endpoint: "modifyorder",
            payload: order,
        },
        label: "modify_order_service.modify_order",
    })
}

pub(super) async fn cancel_order(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<Plan, Unplanned> {
    let identifier = validate::order_id(arg(args, "order_id"))?;
    let verdict = guard::check_destructive(
        &ctx.app,
        "cancel_order",
        reads::analyzer_mode(ctx),
        extra(&[("orderid", json!(identifier))]),
    )
    .await
    .map_err(internal)?;
    Ok(Plan {
        verdict,
        extra: extra(&[("order_id", json!(identifier))]),
        dispatch: Dispatch::Post {
            endpoint: "cancelorder",
            payload: extra(&[
                ("strategy", json!(AGENT_STRATEGY)),
                ("orderid", json!(identifier)),
            ]),
        },
        label: "cancel_order_service.cancel_order",
    })
}

pub(super) async fn cancel_all_orders(
    ctx: &ToolCtx,
    _args: &Map<String, Value>,
) -> Result<Plan, Unplanned> {
    let verdict = guard::check_destructive(
        &ctx.app,
        "cancel_all_orders",
        reads::analyzer_mode(ctx),
        Map::new(),
    )
    .await
    .map_err(internal)?;
    Ok(Plan {
        verdict,
        extra: Map::new(),
        dispatch: Dispatch::Post {
            endpoint: "cancelallorder",
            payload: extra(&[("strategy", json!(AGENT_STRATEGY))]),
        },
        label: "cancel_all_order_service.cancel_all_orders",
    })
}

pub(super) async fn close_position(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<Plan, Unplanned> {
    let text = |k: &str| {
        (!arg(args, k).is_null()).then(|| crate::agent::safety::risk::num::py_str(arg(args, k)))
    };
    let (bare, venue) =
        split_exchange_prefix(text("symbol").as_deref(), text("exchange").as_deref())?;
    let symbol = validate::required_text("symbol", &json!(bare))?.to_uppercase();
    let exchange = validate::exchange(&json!(venue))?;
    let product = validate::product(arg(args, "product"))?;
    let details = extra(&[
        ("symbol", json!(symbol)),
        ("exchange", json!(exchange)),
        ("product", json!(product)),
    ]);
    let verdict = guard::check_destructive(
        &ctx.app,
        "close_position",
        reads::analyzer_mode(ctx),
        details.clone(),
    )
    .await
    .map_err(internal)?;
    Ok(Plan {
        verdict,
        extra: details,
        dispatch: Dispatch::SquareOff {
            symbol,
            exchange,
            product,
        },
        label: "place_smart_order_service.place_smart_order",
    })
}

pub(super) async fn close_all_positions(
    ctx: &ToolCtx,
    _args: &Map<String, Value>,
) -> Result<Plan, Unplanned> {
    let verdict = guard::check_destructive(
        &ctx.app,
        "close_all_positions",
        reads::analyzer_mode(ctx),
        Map::new(),
    )
    .await
    .map_err(internal)?;
    Ok(Plan {
        verdict,
        extra: Map::new(),
        dispatch: Dispatch::Post {
            endpoint: "closeposition",
            payload: extra(&[("strategy", json!(AGENT_STRATEGY))]),
        },
        label: "close_position_service.close_position",
    })
}

/// Send a plan's call. A square-off whose position reads zero sends
/// nothing; one whose position cannot be read is sent, and the platform
/// reads the position itself.
pub(super) async fn dispatch(ctx: &ToolCtx, d: &Dispatch) -> Sent {
    match d {
        Dispatch::Post { endpoint, payload } => Sent::of(
            ctx.api_post(endpoint, payload.clone(), DISPATCH_TIMEOUT)
                .await,
        ),
        Dispatch::SquareOff {
            symbol,
            exchange,
            product,
        } => {
            let held = reads::open_position_quantity(ctx, symbol, exchange, product).await;
            if held == Some(0) {
                return Sent::NothingToClose {
                    symbol: symbol.clone(),
                    exchange: exchange.clone(),
                    product: product.clone(),
                };
            }
            let order = extra(&[
                ("strategy", json!(AGENT_STRATEGY)),
                ("symbol", json!(symbol)),
                ("exchange", json!(exchange)),
                (
                    "action",
                    json!(if held.unwrap_or(0) > 0 { "SELL" } else { "BUY" }),
                ),
                ("quantity", json!(0)),
                ("position_size", json!(0)),
                ("product", json!(product)),
                ("pricetype", json!("MARKET")),
                ("price", json!(0.0)),
                ("trigger_price", json!(0.0)),
                ("disclosed_quantity", json!(0)),
            ]);
            Sent::of(
                ctx.api_post("placesmartorder", order, DISPATCH_TIMEOUT)
                    .await,
            )
        }
    }
}
