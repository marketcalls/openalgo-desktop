//! Account tools: funds, positions, holdings, the order and trade books, one
//! open position and one order's status (the web's
//! `services/agent/tools/account.py`, toolkit `account`).
//!
//! Every tool reads, so none needs approval and none is withheld when
//! trading is off. Each result states `mode` (`live` or `analyze`), read at
//! call time from the same switch the services route on, and in analyzer
//! mode says these are sandbox figures: a sandbox balance is never
//! presented as the trader's real money. Rupee figures are passed through
//! exactly as the service produced them. An empty book is an answer (a
//! count of zero and a note), not an error.

#[cfg(test)]
mod tests;

use crate::agent::contract::json::json_safe;
use crate::agent::contract::tool::{
    AgentTool, Surfaces, ToolCtx, ToolGroup, ToolOutcome, ToolSpec, MAX_RESULT_CHARS,
};
use crate::agent::redact::redact;
use crate::agent::tools::answer::{json_result, label};
use crate::agent::tools::market::{data_of, VALID_EXCHANGES};
use crate::agent::tools::service::{busy_message, failure_message, ServiceReply, SERVICE_TIMEOUT};
use crate::agent::tools::specs;
use crate::agent::tools::values::{py_str, quote, type_name};
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// Every money figure on this platform is rupees.
pub const CURRENCY: &str = "INR";

/// Most rows one book tool attempts to serialise.
pub const MAX_BOOK_ROWS: usize = 500;

/// The `strategy` an agent call carries into the API log (the web's
/// `ASSISTANT_NAME`), so it is attributable there.
pub const STRATEGY_LABEL: &str = "OpenAlgo Assistant";

/// The product codes, in the web's order.
pub const PRODUCTS: [&str; 3] = ["CNC", "NRML", "MIS"];

const ANALYZE_NOTE: &str = "Analyzer mode is on, so these are sandbox figures and not the live broker account. Say so when you report them.";

const UNEXPECTED_SHAPE_NOTE: &str = "The broker returned this book in a shape OpenAlgo does not recognise as a row list, so it is passed through unchanged. Read it as it is rather than assuming the book is empty.";

/// Which account a figure just read came from: `analyze` when analyzer mode
/// is on, else `live`, read now from the switch the services consult; the
/// run's belief is used only when the setting cannot be read.
pub fn current_mode(ctx: &ToolCtx) -> &'static str {
    match ctx.app.sqlite.get_analyze_mode() {
        Ok(true) => "analyze",
        Ok(false) => "live",
        Err(e) => {
            tracing::error!(
                "Could not read the analyzer mode for an agent tool result: {}",
                e
            );
            if ctx.analyzer_mode {
                "analyze"
            } else {
                "live"
            }
        }
    }
}

/// Whether a position quantity means "no position": true only when it
/// parses as zero. An unreadable quantity is not flat, because claiming no
/// position on a value nobody could read is the dangerous direction.
pub fn looks_flat(quantity: &Value) -> bool {
    py_str(quantity)
        .trim()
        .parse::<f64>()
        .is_ok_and(|q| q == 0.0)
}

/// The product vocabulary (the web's `order_vocab.canonical_product`):
/// INTRADAY is MIS, DELIVERY is CNC, CARRYFORWARD and NORMAL are NRML;
/// anything else upper-cased for the caller to check.
pub fn canonical_product(value: &str) -> String {
    let upper = value.trim().to_uppercase();
    match upper.as_str() {
        "INTRADAY" => "MIS".into(),
        "DELIVERY" => "CNC".into(),
        "CARRYFORWARD" | "CARRY_FORWARD" | "NORMAL" => "NRML".into(),
        _ => upper,
    }
}

pub const GROUP: ToolGroup = ToolGroup {
    key: "account",
    surfaces: Surfaces::ALL,
    requires: &[],
    order: 30,
    description: "Funds, positions, holdings, order book, trade book and order status.",
    build,
};

#[derive(Debug, Clone, Copy)]
enum Kind {
    Funds,
    Positions,
    Holdings,
    OrderBook,
    TradeBook,
    OpenPosition,
    OrderStatus,
}

struct AccountTool {
    spec: ToolSpec,
    kind: Kind,
}

fn build(_ctx: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
    [
        ("get_funds", Kind::Funds),
        ("get_positions", Kind::Positions),
        ("get_holdings", Kind::Holdings),
        ("get_orderbook", Kind::OrderBook),
        ("get_tradebook", Kind::TradeBook),
        ("get_open_position", Kind::OpenPosition),
        ("get_order_status", Kind::OrderStatus),
    ]
    .into_iter()
    .filter_map(|(name, kind)| {
        let spec = specs::web_or_log("account", name)?;
        Some(Arc::new(AccountTool { spec, kind }) as Arc<dyn AgentTool>)
    })
    .collect()
}

/// A book tool: its service, its endpoint, the key its rows sit under in
/// the service's `data` (none when `data` is the row list), the key they
/// are returned under, and what an empty book means.
struct Book {
    tool: &'static str,
    label: &'static str,
    endpoint: &'static str,
    rows_in: Option<&'static str>,
    rows_key: &'static str,
    empty_note: &'static str,
}

const POSITIONS: Book = Book {
    tool: "get_positions",
    label: "positionbook_service.get_positionbook",
    endpoint: "positionbook",
    rows_in: None,
    rows_key: "positions",
    empty_note: "There are no open positions. The position book is empty, so the operator is flat.",
};
const HOLDINGS: Book = Book {
    tool: "get_holdings",
    label: "holdings_service.get_holdings",
    endpoint: "holdings",
    rows_in: Some("holdings"),
    rows_key: "holdings",
    empty_note: "There are no holdings. The delivery portfolio is empty.",
};
const ORDERS: Book = Book {
    tool: "get_orderbook",
    label: "orderbook_service.get_orderbook",
    endpoint: "orderbook",
    rows_in: Some("orders"),
    rows_key: "orders",
    empty_note: "No orders were placed today. The order book is empty.",
};
const TRADES: Book = Book {
    tool: "get_tradebook",
    label: "tradebook_service.get_tradebook",
    endpoint: "tradebook",
    rows_in: None,
    rows_key: "trades",
    empty_note: "No trades were executed today. The trade book is empty.",
};

#[async_trait]
impl AgentTool for AccountTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx, args: Map<String, Value>) -> ToolOutcome {
        let done = match self.kind {
            Kind::Funds => get_funds(ctx).await,
            Kind::Positions => get_book(ctx, &POSITIONS).await,
            Kind::Holdings => get_book(ctx, &HOLDINGS).await,
            Kind::OrderBook => get_book(ctx, &ORDERS).await,
            Kind::TradeBook => get_book(ctx, &TRADES).await,
            Kind::OpenPosition => get_open_position(ctx, &args).await,
            Kind::OrderStatus => get_order_status(ctx, &args).await,
        };
        done.unwrap_or_else(|refusal| refusal)
    }
}

/// `{ok, mode, currency, ...fields}` (no currency on a result that carries
/// no money), and the analyzer sentence in analyzer mode.
fn envelope(ctx: &ToolCtx, currency: bool, fields: Vec<(&str, Value)>) -> Map<String, Value> {
    let mode = current_mode(ctx);
    let mut p = Map::new();
    p.insert("ok".into(), json!(true));
    p.insert("mode".into(), json!(mode));
    if currency {
        p.insert("currency".into(), json!(CURRENCY));
    }
    for (k, v) in fields {
        p.insert(k.into(), v);
    }
    if mode == "analyze" {
        p.insert("mode_note".into(), json!(ANALYZE_NOTE));
    }
    p
}

async fn get_funds(ctx: &ToolCtx) -> Result<ToolOutcome, ToolOutcome> {
    let response = ctx
        .service_read("funds_service.get_funds", "funds", Map::new())
        .await?;
    let funds = match data_of(response) {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    let empty = funds.is_empty();
    let mut result = envelope(ctx, true, vec![("funds", Value::Object(funds))]);
    if empty {
        result.insert(
            "note".into(),
            json!("The broker returned no fund figures. That is an empty answer rather than an error; say the balance could not be read rather than reporting zero."),
        );
    }
    Ok(json_result("get_funds", &Value::Object(result), &[]))
}

/// A book's rows, the fields beside them (`statistics`), and whether the
/// shape was one with a row list (the web's `_split_book`).
pub fn split_book(
    payload: &Value,
    rows_key: Option<&str>,
) -> (Vec<Value>, Map<String, Value>, bool) {
    let data = match payload {
        Value::Object(m) => m.get("data").cloned().unwrap_or(Value::Null),
        other => other.clone(),
    };
    match data {
        Value::Array(rows) => (rows, Map::new(), true),
        Value::Object(map) => {
            if let Some(Value::Array(rows)) = rows_key.and_then(|k| map.get(k)) {
                let extras = map
                    .iter()
                    .filter(|(k, _)| Some(k.as_str()) != rows_key)
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                return (rows.clone(), extras, true);
            }
            (Vec::new(), map, false)
        }
        Value::Null => (Vec::new(), Map::new(), true),
        other => {
            let mut extras = Map::new();
            extras.insert("data".into(), other);
            (Vec::new(), extras, false)
        }
    }
}

async fn get_book(ctx: &ToolCtx, book: &Book) -> Result<ToolOutcome, ToolOutcome> {
    let payload = ctx
        .service_read(book.label, book.endpoint, Map::new())
        .await?;
    let (rows, extras, recognised) = split_book(&payload, book.rows_in);
    let mode = current_mode(ctx);
    let text = book_result(mode, book, &rows, &extras, recognised);
    Ok(ToolOutcome::Ok(
        crate::agent::prompts::untrusted::wrap_tool_result(book.tool, &text, &[]),
    ))
}

/// Whether a payload, redacted, fits one result whole.
fn fits(payload: &Value) -> bool {
    serde_json::to_string(&json_safe(&redact(payload)))
        .map(|t| t.chars().count() <= MAX_RESULT_CHARS)
        .unwrap_or(false)
}

fn serialised(payload: &Value) -> String {
    crate::agent::contract::json::dumps_capped(&redact(payload), MAX_RESULT_CHARS)
}

/// A book, dropping whole rows rather than characters when it would not
/// fit: the largest prefix of rows that fits, found by measuring, with the
/// count of everything dropped stated (the web's `_book_result`).
fn book_result(
    mode: &str,
    book: &Book,
    rows: &[Value],
    extras: &Map<String, Value>,
    recognised: bool,
) -> String {
    let total = rows.len();
    let head = &rows[..total.min(MAX_BOOK_ROWS)];
    let pre_dropped = total - head.len();
    let build = |shown: &[Value], dropped: usize| -> Value {
        let mut p = Map::new();
        p.insert("ok".into(), json!(true));
        p.insert("mode".into(), json!(mode));
        p.insert("currency".into(), json!(CURRENCY));
        p.insert("count".into(), json!(total));
        p.insert(book.rows_key.into(), Value::Array(shown.to_vec()));
        if mode == "analyze" {
            p.insert("mode_note".into(), json!(ANALYZE_NOTE));
        }
        for (k, v) in extras {
            if !p.contains_key(k) {
                p.insert(k.clone(), v.clone());
            }
        }
        let omitted = dropped + pre_dropped;
        if omitted > 0 {
            p.insert("rows_omitted".into(), json!(omitted));
            p.insert(
                "note".into(),
                json!(format!(
                    "Only the first {} of {} rows are shown; {} were dropped so the result would fit. Report the count honestly, and ask about a specific symbol or order id if you need one of the rows that is missing.",
                    shown.len(),
                    total,
                    omitted
                )),
            );
        } else if !recognised {
            p.insert("note".into(), json!(UNEXPECTED_SHAPE_NOTE));
        } else if total == 0 {
            p.insert("note".into(), json!(book.empty_note));
        }
        Value::Object(p)
    };
    let whole = build(head, 0);
    if fits(&whole) || head.is_empty() {
        return serialised(&whole);
    }
    let mut best = build(&[], head.len());
    let (mut low, mut high) = (0usize, head.len() - 1);
    while low <= high {
        let middle = (low + high) / 2;
        let candidate = build(&head[..middle], head.len() - middle);
        if fits(&candidate) {
            best = candidate;
            low = middle + 1;
        } else if middle == 0 {
            break;
        } else {
            high = middle - 1;
        }
    }
    serialised(&best)
}

/// A required text argument, stripped (the web's `_clean`).
fn clean(field: &str, value: Option<&Value>, upper: bool) -> Result<String, ToolOutcome> {
    let text = match value {
        Some(Value::String(s)) => s.trim().to_string(),
        other => {
            let kind = other.map_or("NoneType", type_name);
            return Err(ToolOutcome::invalid_argument(
                field,
                &format!("it must be text, not {}.", kind),
                Some("Pass it as a plain string."),
            ));
        }
    };
    if text.is_empty() {
        return Err(ToolOutcome::invalid_argument(
            field,
            "it is empty.",
            Some("Supply a real value."),
        ));
    }
    Ok(if upper { text.to_uppercase() } else { text })
}

async fn get_open_position(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    let symbol = clean("symbol", args.get("symbol"), true)?;
    let exchange = clean("exchange", args.get("exchange"), true)?;
    let product = canonical_product(&clean("product", args.get("product"), true)?);
    if !VALID_EXCHANGES.contains(&exchange.as_str()) {
        return Err(ToolOutcome::invalid_argument(
            "exchange",
            &format!("{} is not an OpenAlgo exchange code.", quote(&exchange)),
            Some(&format!("Use one of: {}.", VALID_EXCHANGES.join(", "))),
        ));
    }
    if !PRODUCTS.contains(&product.as_str()) {
        return Err(ToolOutcome::invalid_argument(
            "product",
            &format!("{} is not an OpenAlgo product type.", quote(&product)),
            Some(&format!(
                "Use one of: {}. CNC is delivery equity, NRML is carry futures and options, MIS is intraday.",
                PRODUCTS.join(", ")
            )),
        ));
    }
    let mut request = Map::new();
    request.insert("symbol".into(), json!(symbol));
    request.insert("exchange".into(), json!(exchange));
    request.insert("product".into(), json!(product));
    request.insert("strategy".into(), json!(STRATEGY_LABEL));
    let payload = ctx
        .service_read(
            "openposition_service.get_open_position",
            "openposition",
            request,
        )
        .await?;
    let quantity = match &payload {
        Value::Object(m) => m.get("quantity").cloned().unwrap_or(Value::Null),
        other => other.clone(),
    };
    let flat = looks_flat(&quantity);
    let mut result = envelope(
        ctx,
        false,
        vec![
            ("symbol", json!(symbol)),
            ("exchange", json!(exchange)),
            ("product", json!(product)),
            ("quantity", quantity),
        ],
    );
    if flat {
        result.insert(
            "note".into(),
            json!(format!(
                "The position service reported no open quantity for {} on {} under {}. That is the answer for this exact symbol, exchange and product, not proof the operator holds nothing: check get_positions before telling the operator they are flat.",
                symbol, exchange, product
            )),
        );
    }
    Ok(json_result(
        "get_open_position",
        &Value::Object(result),
        &[label("symbol", &symbol), label("exchange", &exchange)],
    ))
}

async fn get_order_status(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    // A numeric id often arrives as a JSON number; its decimal text is the id.
    let raw = match args.get("order_id") {
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => Some(Value::String(n.to_string())),
        other => other.cloned(),
    };
    let order_id = clean("order_id", raw.as_ref(), false)?;
    let label_name = "orderstatus_service.get_order_status";
    let mut request = Map::new();
    request.insert("orderid".into(), json!(order_id));
    request.insert("strategy".into(), json!(STRATEGY_LABEL));
    let labels = [label("order_id", &order_id)];
    // Read directly rather than through service_read: an id that is not in
    // today's book answers 404, and that is an answer here, not a failure.
    let body = match ctx.api_post("orderstatus", request, SERVICE_TIMEOUT).await {
        ServiceReply::NoAnswer(_) => return Err(ToolOutcome::Retry(busy_message(label_name))),
        ServiceReply::Answered { status: 404, .. } => {
            let note = format!(
                "No order with id {} is in today's order book. That is an answer, not a failure: the id may belong to an earlier session, or the order was never accepted. Call get_orderbook to see today's orders.",
                order_id
            );
            let result = envelope(
                ctx,
                true,
                vec![
                    ("order_id", json!(order_id)),
                    ("found", json!(false)),
                    ("note", json!(note)),
                ],
            );
            return Ok(json_result(
                "get_order_status",
                &Value::Object(result),
                &labels,
            ));
        }
        ServiceReply::Answered { status, body, .. } => {
            let failed = !(200..300).contains(&status)
                || body.get("status").and_then(Value::as_str).map(str::trim) == Some("error");
            if failed {
                return Err(ToolOutcome::Retry(failure_message(
                    label_name,
                    &body,
                    Some(status),
                )));
            }
            body
        }
    };
    let order = match data_of(body) {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    let found = !order.is_empty();
    let result = envelope(
        ctx,
        true,
        vec![
            ("order_id", json!(order_id)),
            ("found", json!(found)),
            ("order", Value::Object(order)),
        ],
    );
    Ok(json_result(
        "get_order_status",
        &Value::Object(result),
        &labels,
    ))
}
