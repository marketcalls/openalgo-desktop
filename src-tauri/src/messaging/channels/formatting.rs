//! What an order alert says, before any channel decides how it looks (web
//! `services/alerts/formatting.py`).
//!
//! [`build_message`] turns an order event's request and response into a
//! title, a mode banner and plain `Label: value` lines. Each channel then
//! renders that one structure in its own markup: WhatsApp and Slack bold
//! with `*`, Discord with `**`, and the webhook channels escape the values
//! first. A new order type, or a new field on an existing one, reaches every
//! channel that uses this module.
//!
//! This is the formatter WhatsApp alerts used, unchanged except for GTT
//! orders, which it described as a bare "Order Update" with no fields.
//! WhatsApp's other messages are pinned byte for byte by the web's golden
//! corpus (`tests/fixtures/web/alerts/whatsapp_alert_golden.json`). Telegram
//! keeps its own formatter (`messaging::alerts`): it marks symbols as code
//! and draws a different separator, and converging the two would change
//! messages Telegram users already receive.

use crate::messaging::format::{get_str, py_str, truthy};
use serde_json::{Map, Value};

pub const LIVE_BANNER: &str = "LIVE MODE - Real Order";
pub const ANALYZE_BANNER: &str = "ANALYZE MODE - No Real Order";

/// Drawn under the banner. Plain hyphens so it reads the same on every
/// channel and in every font.
pub const SEPARATOR: &str = "---------------------";

/// How many child orders a basket alert lists before summarising the rest.
const BASKET_LISTED: usize = 3;
/// How many legs an options alert lists.
const OPTIONS_LISTED: usize = 5;

/// One alert, channel-neutral.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertMessage {
    /// The headline, such as "Order Placed".
    pub title: &'static str,
    /// The body, one `Label: value` line each, ending with the time.
    pub lines: Vec<String>,
    /// Live or analyze mode. `None` for the short message sent when the
    /// event could not be read.
    pub banner: Option<&'static str>,
    /// The strategy that sent the order, shown above the banner.
    pub strategy: Option<String>,
}

/// The title for each order type, `Order Update` for any other.
pub fn title(api_type: &str) -> &'static str {
    match api_type {
        "placeorder" => "Order Placed",
        "placesmartorder" => "Smart Order Placed",
        "basketorder" => "Basket Order Executed",
        "splitorder" => "Split Order Executed",
        "optionsorder" => "Options Order Executed",
        "optionsmultiorder" => "Options Multi-Order Executed",
        "modifyorder" => "Order Modified",
        "cancelorder" => "Order Cancelled",
        "cancelallorder" => "All Orders Cancelled",
        "closeposition" => "Position Closed",
        "placegttorder" => "GTT Order Placed",
        "modifygttorder" => "GTT Order Modified",
        "cancelgttorder" => "GTT Order Cancelled",
        "gtttriggered" => "GTT Triggered",
        "gttexpired" => "GTT Expired",
        _ => "Order Update",
    }
}

/// An event part the web's formatter could not read: Python raised on it,
/// so the alert falls back to the short message.
struct Unreadable;

type Lines = Result<Vec<String>, Unreadable>;

/// Describe one order event as an alert. Never fails: an event that cannot
/// be read becomes a short message naming the order type and status, so a
/// malformed response costs the detail and never the alert.
///
/// `analyze` is the event's mode (`alerts::is_analyze`); `time` is the
/// local `%H:%M:%S` the alert is raised at.
pub fn build_message(
    api_type: &str,
    request: &Value,
    response: &Value,
    analyze: bool,
    time: &str,
) -> AlertMessage {
    let title = title(api_type);
    let read = if request.is_object() && response.is_object() {
        detail_lines(api_type, &Event { request, response })
    } else {
        Err(Unreadable)
    };
    match read {
        Ok(mut lines) => {
            lines.push(format!("Time: {}", time));
            AlertMessage {
                title,
                lines,
                banner: Some(if analyze { ANALYZE_BANNER } else { LIVE_BANNER }),
                strategy: truthy(request.get("strategy")).then(|| get_str(request, "strategy", "")),
            }
        }
        Err(Unreadable) => {
            tracing::warn!("Could not read a {} event for its alert", api_type);
            AlertMessage {
                title,
                lines: vec![
                    format!("Order Type: {}", api_type),
                    format!("Status: {}", get_str(response, "status", "unknown")),
                ],
                banner: None,
                strategy: None,
            }
        }
    }
}

/// What every part of an alert reads.
struct Event<'a> {
    request: &'a Value,
    response: &'a Value,
}

impl Event<'_> {
    fn req(&self, key: &str) -> String {
        get_str(self.request, key, "N/A")
    }

    fn resp(&self, key: &str) -> String {
        get_str(self.response, key, "N/A")
    }

    fn succeeded(&self) -> bool {
        is_success(self.response)
    }

    fn error_line(&self) -> String {
        format!("Error: {}", get_str(self.response, "message", "Failed"))
    }

    /// `success_text`, or the error line.
    fn outcome(&self, success_text: &str) -> String {
        if self.succeeded() {
            success_text.to_string()
        } else {
            self.error_line()
        }
    }

    /// `response.get("results", [])`: every child order, each an object.
    fn results(&self) -> Result<Vec<&Map<String, Value>>, Unreadable> {
        match self.response.get("results") {
            None => Ok(Vec::new()),
            Some(Value::Array(list)) => list
                .iter()
                .map(|r| r.as_object().ok_or(Unreadable))
                .collect(),
            Some(_) => Err(Unreadable),
        }
    }
}

fn is_success(v: &Value) -> bool {
    v.get("status").and_then(Value::as_str) == Some("success")
}

/// The lines between the banner and the time, for one order type.
fn detail_lines(api_type: &str, e: &Event<'_>) -> Lines {
    match api_type {
        "placeorder" => Ok(place_order(e)),
        "placesmartorder" => Ok(smart_order(e)),
        "basketorder" => basket_order(e),
        "splitorder" => split_order(e),
        "modifyorder" => Ok(vec![
            format!("Order ID: {}", e.req("orderid")),
            format!("Symbol: {}", e.req("symbol")),
            format!("New Quantity: {}", e.req("quantity")),
            format!("New Price: {}", e.req("price")),
            e.outcome("Modification Successful"),
        ]),
        "cancelorder" => Ok(vec![
            format!("Order ID: {}", e.req("orderid")),
            e.outcome("Cancellation Successful"),
        ]),
        "cancelallorder" => Ok(cancel_all(e)),
        "closeposition" => Ok(close_position(e)),
        "optionsorder" | "optionsmultiorder" => options_order(e),
        "placegttorder" | "modifygttorder" | "cancelgttorder" | "gtttriggered" | "gttexpired" => {
            gtt(api_type, e)
        }
        _ => Ok(Vec::new()),
    }
}

fn place_order(e: &Event<'_>) -> Vec<String> {
    let mut lines = vec![
        format!("Symbol: {}", e.req("symbol")),
        format!("Action: {}", e.req("action")),
        format!("Quantity: {}", e.req("quantity")),
        format!("Price Type: {}", e.req("pricetype")),
        format!("Exchange: {}", e.req("exchange")),
        format!("Product: {}", e.req("product")),
    ];
    if e.succeeded() {
        lines.push(format!("Order ID: {}", e.resp("orderid")));
    } else {
        lines.push(e.error_line());
    }
    lines
}

fn smart_order(e: &Event<'_>) -> Vec<String> {
    let mut lines = vec![
        format!("Symbol: {}", e.req("symbol")),
        format!("Action: {}", e.req("action")),
        format!("Quantity: {}", e.req("quantity")),
        format!("Position Size: {}", e.req("position_size")),
        format!("Exchange: {}", e.req("exchange")),
    ];
    if e.succeeded() {
        lines.push(format!("Order ID: {}", e.resp("orderid")));
    }
    lines
}

/// How many child orders there were, and how many went through.
fn tally(total_label: &str, results: &[&Map<String, Value>]) -> Vec<String> {
    let good = successes(results);
    vec![
        format!("{}: {}", total_label, results.len()),
        format!("Successful: {}", good),
        format!("Failed: {}", results.len() - good),
    ]
}

fn successes(results: &[&Map<String, Value>]) -> usize {
    results
        .iter()
        .filter(|r| r.get("status").and_then(Value::as_str) == Some("success"))
        .count()
}

fn mark(result: &Map<String, Value>) -> &'static str {
    if result.get("status").and_then(Value::as_str) == Some("success") {
        "OK"
    } else {
        "X"
    }
}

/// `r.get('orderid', r.get('message', 'N/A'))`.
fn order_id_or_message(result: &Map<String, Value>) -> String {
    match result.get("orderid") {
        Some(v) => py_str(v),
        None => result.get("message").map_or_else(|| "N/A".into(), py_str),
    }
}

fn field(result: &Map<String, Value>, key: &str, default: &str) -> String {
    result.get(key).map_or_else(|| default.to_string(), py_str)
}

fn basket_order(e: &Event<'_>) -> Lines {
    if !e.succeeded() {
        return Ok(Vec::new());
    }
    let results = e.results()?;
    let mut lines = tally("Total Orders", &results);
    for r in results.iter().take(BASKET_LISTED) {
        lines.push(format!(
            "[{}] {}: {}",
            mark(r),
            field(r, "symbol", "N/A"),
            order_id_or_message(r)
        ));
    }
    if results.len() > BASKET_LISTED {
        lines.push(format!("... and {} more", results.len() - BASKET_LISTED));
    }
    Ok(lines)
}

fn split_order(e: &Event<'_>) -> Lines {
    let results = e.results()?;
    let good = successes(&results);
    let failed = results.len() - good;
    let mut lines = vec![
        format!("Symbol: {}", e.req("symbol")),
        format!("Total Quantity: {}", e.resp("total_quantity")),
        format!("Split Size: {}", e.resp("split_size")),
    ];
    lines.extend(tally("Total Orders", &results));
    if failed > 0 && good == 0 {
        lines.push("All orders rejected".into());
    } else if failed > 0 {
        lines.push("Partial fill".into());
        let first_fail = results
            .iter()
            .find(|r| r.get("status").and_then(Value::as_str) != Some("success"));
        if let Some(r) = first_fail.filter(|r| truthy(r.get("message"))) {
            lines.push(format!("Reason: {}", field(r, "message", "")));
        }
    }
    Ok(lines)
}

/// Every order cancelled at once. The web joins the ids as text and loses
/// the whole message to the short form when a broker lists them as
/// numbers; here a number prints as it is.
fn cancel_all(e: &Event<'_>) -> Vec<String> {
    if !e.succeeded() {
        return Vec::new();
    }
    let list = |key: &str| {
        e.response
            .get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let cancelled = list("canceled_orders");
    let failed = list("failed_cancellations");
    let mut lines = vec![
        format!("Cancelled: {} orders", cancelled.len()),
        format!("Failed: {} orders", failed.len()),
    ];
    if !cancelled.is_empty() && cancelled.len() <= 5 {
        let ids: Vec<String> = cancelled.iter().map(py_str).collect();
        lines.push(format!("Order IDs: {}", ids.join(", ")));
    }
    lines
}

/// One position closed, or a count of every position closed.
fn close_position(e: &Event<'_>) -> Vec<String> {
    if !e.succeeded() {
        return vec![e.error_line()];
    }
    if truthy(e.request.get("symbol")) {
        return vec![
            format!("Symbol: {}", e.req("symbol")),
            format!("Exchange: {}", e.req("exchange")),
            format!("Product: {}", e.req("product")),
            format!("Order ID: {}", e.resp("orderid")),
        ];
    }
    let closed = e.response.get("closed_positions");
    let failed = e.response.get("failed_closures");
    if truthy(closed) || truthy(failed) {
        let count = |v: Option<&Value>| v.map_or_else(|| "0".into(), py_str);
        return vec![
            format!("Closed: {} positions", count(closed)),
            format!("Failed: {} positions", count(failed)),
        ];
    }
    vec!["All positions closed successfully".into()]
}

/// The underlying, each leg's outcome, and the underlying's price.
fn options_order(e: &Event<'_>) -> Lines {
    let mut lines = vec![format!("Underlying: {}", e.req("underlying"))];
    if !e.succeeded() {
        lines.push(e.error_line());
        return Ok(lines);
    }
    let results = e.results()?;
    if !results.is_empty() {
        lines.extend(tally("Total Legs", &results));
        for r in results.iter().take(OPTIONS_LISTED) {
            lines.push(format!(
                "[{}] {} {} -> {}",
                mark(r),
                field(r, "symbol", "N/A"),
                field(r, "action", ""),
                order_id_or_message(r)
            ));
        }
    }
    if truthy(e.response.get("underlying_ltp")) {
        lines.push(format!("Underlying LTP: {}", e.resp("underlying_ltp")));
    }
    Ok(lines)
}

/// A GTT placed, modified or cancelled by the trader, or fired or lapsed
/// on its own.
fn gtt(api_type: &str, e: &Event<'_>) -> Lines {
    match api_type {
        "cancelgttorder" => Ok(vec![
            format!("Trigger ID: {}", e.req("trigger_id")),
            e.outcome("Cancellation Successful"),
        ]),
        "gtttriggered" | "gttexpired" => {
            let mut lines = vec![
                format!("Symbol: {}", e.req("symbol")),
                format!("Exchange: {}", e.req("exchange")),
                format!("Trigger ID: {}", e.req("trigger_id")),
            ];
            if api_type == "gtttriggered" {
                lines.push(format!("Trigger Prices: {}", trigger_prices(e.request)?));
                let order_id = e.response.get("orderid").filter(|v| truthy(Some(v)));
                lines.push(format!(
                    "Order ID: {}",
                    order_id.map_or_else(|| "N/A".into(), py_str)
                ));
            } else if truthy(e.response.get("message")) {
                lines.push(get_str(e.response, "message", ""));
            }
            Ok(lines)
        }
        _ => Ok(gtt_order(api_type, e)),
    }
}

/// `", ".join(str(p) for p in request.get("trigger_prices") or [])`.
fn trigger_prices(request: &Value) -> Result<String, Unreadable> {
    let prices = request.get("trigger_prices");
    if !truthy(prices) {
        return Ok("N/A".into());
    }
    let list = prices.and_then(Value::as_array).ok_or(Unreadable)?;
    Ok(list.iter().map(py_str).collect::<Vec<_>>().join(", "))
}

/// A GTT placed or modified: the order it will send.
fn gtt_order(api_type: &str, e: &Event<'_>) -> Vec<String> {
    let modify = api_type == "modifygttorder";
    let mut lines = Vec::new();
    if modify {
        lines.push(format!("Trigger ID: {}", e.req("trigger_id")));
    }
    lines.extend([
        format!("Symbol: {}", e.req("symbol")),
        format!("Action: {}", e.req("action")),
        format!("Quantity: {}", e.req("quantity")),
        format!("Exchange: {}", e.req("exchange")),
        format!("Trigger Type: {}", e.req("trigger_type")),
    ]);
    // A SINGLE GTT sets one of the two; an OCO sets both.
    if truthy(e.request.get("triggerprice_sl")) {
        lines.push(format!("Stop Loss Trigger: {}", e.req("triggerprice_sl")));
    }
    if truthy(e.request.get("triggerprice_tg")) {
        lines.push(format!("Target Trigger: {}", e.req("triggerprice_tg")));
    }
    if modify {
        lines.push(e.outcome("Modification Successful"));
    } else if e.succeeded() {
        lines.push(format!("Trigger ID: {}", e.resp("trigger_id")));
    } else {
        lines.push(e.error_line());
    }
    lines
}

#[cfg(test)]
mod tests;
