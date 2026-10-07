//! Order alerts: the web's `subscribers/telegram_subscriber.py` and
//! `subscribers/whatsapp_subscriber.py` over the alert formatters of
//! `services/telegram_alert_service.py` and `services/whatsapp_alert_service.py`.
//!
//! Same topics as the web: placed, no-action, modified, cancelled, all
//! cancelled, position closed, basket, split, options, multi-order and the GTT
//! placed / modified / cancelled / triggered / expired events. Failures and
//! validation errors are not alerted, as on the web. Each channel is its own
//! bus subscriber, so a slow Telegram never holds up WhatsApp.

use super::format::{get_str, py_str, truthy};
use crate::events::{Event, Mode, OrderMeta, Subscriber, Topic};
use crate::state::AppState;
use serde_json::Value;
use std::sync::{Arc, Weak};

/// The topics both channels alert on.
pub fn alert_topics() -> Vec<Topic> {
    vec![
        Topic::OrderPlaced,
        Topic::OrderNoAction,
        Topic::OrderModified,
        Topic::OrderCancelled,
        Topic::AllOrdersCancelled,
        Topic::PositionClosed,
        Topic::BasketCompleted,
        Topic::SplitCompleted,
        Topic::OptionsCompleted,
        Topic::MultiOrderCompleted,
        Topic::GttPlaced,
        Topic::GttModified,
        Topic::GttCancelled,
        Topic::GttTriggered,
        Topic::GttExpired,
    ]
}

/// Which chat app a message is formatted for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Telegram,
    WhatsApp,
}

/// The heading template for an `api_type` (`alert_templates`), with the
/// web's `*Order Update*` default.
pub fn heading(order_type: &str) -> &'static str {
    match order_type {
        "placeorder" => "*Order Placed*",
        "placesmartorder" => "*Smart Order Placed*",
        "basketorder" => "*Basket Order Executed*",
        "splitorder" => "*Split Order Executed*",
        "optionsorder" => "*Options Order Executed*",
        "optionsmultiorder" => "*Options Multi-Order Executed*",
        "modifyorder" => "*Order Modified*",
        "cancelorder" => "*Order Cancelled*",
        "cancelallorder" => "*All Orders Cancelled*",
        "closeposition" => "*Position Closed*",
        _ => "*Order Update*",
    }
}

fn status_is_success(v: &Value) -> bool {
    v.get("status").and_then(Value::as_str) == Some("success")
}

fn results(response: &Value) -> Vec<Value> {
    response
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// `r.get('orderid', r.get('message', 'N/A'))`.
fn oid_or_message(r: &Value) -> String {
    match r.get("orderid") {
        Some(v) => py_str(v),
        None => get_str(r, "message", "N/A"),
    }
}

/// `format_order_details` of the web alert services. `timestamp` is the
/// local `%H:%M:%S` the web stamps on every alert.
pub fn format_order_details(
    channel: Channel,
    order_type: &str,
    order_data: &Value,
    response: &Value,
    analyze: bool,
    timestamp: &str,
) -> String {
    let tg = channel == Channel::Telegram;
    // Telegram wraps symbols and ids in backticks; WhatsApp prints them plain.
    let code = |s: String| if tg { format!("`{}`", s) } else { s };
    let mut d: Vec<String> = Vec::new();
    d.push(if analyze {
        "*ANALYZE MODE - No Real Order*".into()
    } else {
        "*LIVE MODE - Real Order*".into()
    });
    d.push(if tg {
        "─────────────────────".into()
    } else {
        "---------------------".into()
    });
    let ok = status_is_success(response);
    let error_line = || format!("Error: {}", get_str(response, "message", "Failed"));
    let mark = |r: &Value| {
        let good = status_is_success(r);
        match (tg, good) {
            (true, true) => "[OK]".to_string(),
            (true, false) => "[FAILED]".to_string(),
            (false, true) => "[OK]".to_string(),
            (false, false) => "[X]".to_string(),
        }
    };

    match order_type {
        "placeorder" => {
            d.push(format!("Symbol: {}", code(get_str(order_data, "symbol", "N/A"))));
            d.push(format!("Action: {}", get_str(order_data, "action", "N/A")));
            d.push(format!("Quantity: {}", get_str(order_data, "quantity", "N/A")));
            d.push(format!("Price Type: {}", get_str(order_data, "pricetype", "N/A")));
            d.push(format!("Exchange: {}", get_str(order_data, "exchange", "N/A")));
            d.push(format!("Product: {}", get_str(order_data, "product", "N/A")));
            if ok {
                d.push(format!("Order ID: {}", code(get_str(response, "orderid", "N/A"))));
            } else {
                d.push(error_line());
            }
        }
        "placesmartorder" => {
            d.push(format!("Symbol: {}", code(get_str(order_data, "symbol", "N/A"))));
            d.push(format!("Action: {}", get_str(order_data, "action", "N/A")));
            d.push(format!("Quantity: {}", get_str(order_data, "quantity", "N/A")));
            d.push(format!(
                "Position Size: {}",
                get_str(order_data, "position_size", "N/A")
            ));
            d.push(format!("Exchange: {}", get_str(order_data, "exchange", "N/A")));
            if ok {
                d.push(format!("Order ID: {}", code(get_str(response, "orderid", "N/A"))));
            }
        }
        "basketorder" => {
            if ok {
                let rs = results(response);
                let good = rs.iter().filter(|r| status_is_success(r)).count();
                d.push(format!("Total Orders: {}", rs.len()));
                d.push(format!("Successful: {}", good));
                d.push(format!("Failed: {}", rs.len() - good));
                for r in rs.iter().take(3) {
                    d.push(format!(
                        "{} {}: {}",
                        mark(r),
                        get_str(r, "symbol", "N/A"),
                        oid_or_message(r)
                    ));
                }
                if rs.len() > 3 {
                    d.push(format!("... and {} more", rs.len() - 3));
                }
            }
        }
        "splitorder" => {
            let rs = results(response);
            let good = rs.iter().filter(|r| status_is_success(r)).count();
            let failed = rs.len() - good;
            d.push(format!("Symbol: {}", code(get_str(order_data, "symbol", "N/A"))));
            d.push(format!(
                "Total Quantity: {}",
                get_str(response, "total_quantity", "N/A")
            ));
            d.push(format!("Split Size: {}", get_str(response, "split_size", "N/A")));
            d.push(format!("Total Orders: {}", rs.len()));
            d.push(format!("Successful: {}", good));
            d.push(format!("Failed: {}", failed));
            if failed > 0 && good == 0 {
                d.push("All orders rejected".into());
            } else if failed > 0 {
                d.push("Partial fill".into());
                if let Some(first) = rs.iter().find(|r| !status_is_success(r)) {
                    if truthy(first.get("message")) {
                        d.push(format!("Reason: {}", get_str(first, "message", "")));
                    }
                }
            }
        }
        "modifyorder" => {
            d.push(format!("Order ID: {}", code(get_str(order_data, "orderid", "N/A"))));
            d.push(format!("Symbol: {}", code(get_str(order_data, "symbol", "N/A"))));
            d.push(format!("New Quantity: {}", get_str(order_data, "quantity", "N/A")));
            d.push(format!("New Price: {}", get_str(order_data, "price", "N/A")));
            if ok {
                d.push("Modification Successful".into());
            } else {
                d.push(error_line());
            }
        }
        "cancelorder" => {
            d.push(format!("Order ID: {}", code(get_str(order_data, "orderid", "N/A"))));
            if ok {
                d.push("Cancellation Successful".into());
            } else {
                d.push(error_line());
            }
        }
        "cancelallorder" => {
            if ok {
                let list = |k: &str| {
                    response
                        .get(k)
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                };
                let canceled = list("canceled_orders");
                let failed = list("failed_cancellations");
                d.push(format!("Cancelled: {} orders", canceled.len()));
                d.push(format!("Failed: {} orders", failed.len()));
                if !canceled.is_empty() && canceled.len() <= 5 {
                    let ids: Vec<String> = canceled.iter().take(5).map(py_str).collect();
                    d.push(format!("Order IDs: {}", ids.join(", ")));
                }
            }
        }
        "closeposition" => {
            if ok {
                if truthy(order_data.get("symbol")) {
                    d.push(format!("Symbol: {}", code(get_str(order_data, "symbol", "N/A"))));
                    d.push(format!("Exchange: {}", get_str(order_data, "exchange", "N/A")));
                    d.push(format!("Product: {}", get_str(order_data, "product", "N/A")));
                    d.push(format!("Order ID: {}", code(get_str(response, "orderid", "N/A"))));
                } else {
                    let closed = response.get("closed_positions").cloned();
                    let failed = response.get("failed_closures").cloned();
                    if truthy(closed.as_ref()) || truthy(failed.as_ref()) {
                        let s = |v: Option<Value>| v.map(|v| py_str(&v)).unwrap_or("0".into());
                        d.push(format!("Closed: {} positions", s(closed)));
                        d.push(format!("Failed: {} positions", s(failed)));
                    } else {
                        d.push("All positions closed successfully".into());
                    }
                }
            } else {
                d.push(error_line());
            }
        }
        "optionsorder" | "optionsmultiorder" => {
            d.push(format!(
                "Underlying: {}",
                code(get_str(order_data, "underlying", "N/A"))
            ));
            if ok {
                let rs = results(response);
                let good = rs.iter().filter(|r| status_is_success(r)).count();
                let multi = order_type == "optionsmultiorder";
                let arrow = if tg { "→" } else { "->" };
                if tg && !multi && rs.is_empty() {
                    // Telegram single (non-split) options order.
                    let symbol = match response.get("symbol") {
                        Some(v) => py_str(v),
                        None => get_str(order_data, "symbol", "N/A"),
                    };
                    d.push(format!("Symbol: {}", code(symbol)));
                    d.push(format!("Action: {}", get_str(order_data, "action", "N/A")));
                    d.push(format!("Quantity: {}", get_str(order_data, "quantity", "N/A")));
                    d.push(format!("Order ID: {}", code(get_str(response, "orderid", "N/A"))));
                } else if (tg && multi) || !rs.is_empty() {
                    let total = if tg && !multi {
                        "Total Orders"
                    } else {
                        "Total Legs"
                    };
                    d.push(format!("{}: {}", total, rs.len()));
                    d.push(format!("Successful: {}", good));
                    d.push(format!("Failed: {}", rs.len() - good));
                    let shown = if tg && multi { rs.len() } else { 5 };
                    for r in rs.iter().take(shown) {
                        d.push(format!(
                            "{} {} {} {} {}",
                            mark(r),
                            code(get_str(r, "symbol", "N/A")),
                            get_str(r, "action", ""),
                            arrow,
                            oid_or_message(r)
                        ));
                    }
                }
                let show_ltp = truthy(response.get("underlying_ltp")) && (multi || !tg);
                if show_ltp {
                    d.push(format!(
                        "Underlying LTP: {}",
                        get_str(response, "underlying_ltp", "")
                    ));
                }
            } else {
                d.push(error_line());
            }
        }
        _ => {}
    }

    d.push(format!("Time: {}", timestamp));
    if truthy(order_data.get("strategy")) {
        let s = get_str(order_data, "strategy", "");
        d.insert(
            0,
            if tg {
                format!("Strategy: *{}*", s)
            } else {
                format!("Strategy: {}", s)
            },
        );
    }
    d.join("\n")
}

/// The complete alert text (`template.format(details=...)`).
pub fn order_alert_text(
    channel: Channel,
    order_type: &str,
    order_data: &Value,
    response: &Value,
    analyze: bool,
    timestamp: &str,
) -> String {
    format!(
        "{}\n{}",
        heading(order_type),
        format_order_details(channel, order_type, order_data, response, analyze, timestamp)
    )
}

/// ANALYZE or LIVE: the event's mode, or the web's `response["mode"]`.
pub fn is_analyze(meta: &OrderMeta) -> bool {
    meta.mode == Mode::Analyze
        || meta.response_data.get("mode").and_then(Value::as_str) == Some("analyze")
}

/// Local wall-clock time, as the web's `datetime.now().strftime("%H:%M:%S")`.
pub fn local_time(ctx: &AppState) -> String {
    ctx.now()
        .with_timezone(&chrono::Local)
        .format("%H:%M:%S")
        .to_string()
}

/// Telegram order alerts.
pub struct TelegramAlerts {
    ctx: Weak<AppState>,
}

impl TelegramAlerts {
    pub fn new(ctx: Weak<AppState>) -> Self {
        Self { ctx }
    }
}

#[async_trait::async_trait]
impl Subscriber for TelegramAlerts {
    fn name(&self) -> &'static str {
        "telegram"
    }
    fn topics(&self) -> Vec<Topic> {
        alert_topics()
    }
    async fn handle(&self, event: Arc<Event>) {
        let (Some(ctx), Some(meta)) = (self.ctx.upgrade(), event.meta()) else {
            return;
        };
        ctx.messaging.telegram.send_order_alert(&ctx, meta).await;
    }
}

/// WhatsApp order alerts.
pub struct WhatsAppAlerts {
    ctx: Weak<AppState>,
}

impl WhatsAppAlerts {
    pub fn new(ctx: Weak<AppState>) -> Self {
        Self { ctx }
    }
}

#[async_trait::async_trait]
impl Subscriber for WhatsAppAlerts {
    fn name(&self) -> &'static str {
        "whatsapp"
    }
    fn topics(&self) -> Vec<Topic> {
        alert_topics()
    }
    async fn handle(&self, event: Arc<Event>) {
        let (Some(ctx), Some(meta)) = (self.ctx.upgrade(), event.meta()) else {
            return;
        };
        ctx.messaging.whatsapp.send_order_alert(&ctx, meta).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn telegram_placeorder_live_and_analyze() {
        let req = json!({"symbol": "SBIN", "action": "BUY", "quantity": "10",
            "pricetype": "MARKET", "exchange": "NSE", "product": "MIS", "strategy": "Alpha"});
        let resp = json!({"status": "success", "orderid": "123"});
        let live = order_alert_text(Channel::Telegram, "placeorder", &req, &resp, false, "10:00:00");
        assert_eq!(
            live,
            "*Order Placed*\nStrategy: *Alpha*\n*LIVE MODE - Real Order*\n─────────────────────\n\
Symbol: `SBIN`\nAction: BUY\nQuantity: 10\nPrice Type: MARKET\nExchange: NSE\nProduct: MIS\n\
Order ID: `123`\nTime: 10:00:00"
        );
        let an = order_alert_text(Channel::Telegram, "placeorder", &req, &resp, true, "10:00:00");
        assert!(an.contains("*ANALYZE MODE - No Real Order*"));
    }

    #[test]
    fn whatsapp_basket_and_unknown_type() {
        let resp = json!({"status": "success", "results": [
            {"symbol": "A", "status": "success", "orderid": "1"},
            {"symbol": "B", "status": "error", "message": "rejected"}]});
        let t = order_alert_text(Channel::WhatsApp, "basketorder", &json!({}), &resp, false, "t");
        assert_eq!(
            t,
            "*Basket Order Executed*\n*LIVE MODE - Real Order*\n---------------------\n\
Total Orders: 2\nSuccessful: 1\nFailed: 1\n[OK] A: 1\n[X] B: rejected\nTime: t"
        );
        let g = order_alert_text(Channel::WhatsApp, "placegttorder", &json!({}), &json!({}), false, "t");
        assert_eq!(
            g,
            "*Order Update*\n*LIVE MODE - Real Order*\n---------------------\nTime: t"
        );
    }

    #[test]
    fn closeposition_and_cancelall() {
        let resp = json!({"status": "success", "canceled_orders": ["1", "2"], "failed_cancellations": []});
        let t = order_alert_text(Channel::Telegram, "cancelallorder", &json!({}), &resp, false, "t");
        assert!(t.contains("Cancelled: 2 orders\nFailed: 0 orders\nOrder IDs: 1, 2"));
        let c = order_alert_text(
            Channel::Telegram,
            "closeposition",
            &json!({}),
            &json!({"status": "success", "message": "All Open Positions SquaredOff"}),
            false,
            "t",
        );
        assert!(c.contains("All positions closed successfully"));
    }
}
