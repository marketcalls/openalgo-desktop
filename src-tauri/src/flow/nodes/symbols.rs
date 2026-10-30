//! The instrument nodes: Symbol, Expiry, Option Symbol, Option Chain and
//! Synthetic Future (the web's `execute_symbol`, `execute_expiry`,
//! `execute_option_symbol`, `execute_option_chain`,
//! `execute_synthetic_future`), with the option-expiry check the web's
//! dispatcher runs before the three option nodes (`option_expiry_error`).
//! The underlying-to-exchange table is the options nodes' one
//! (`options::resolve_option_exchanges`, decision 25).
//!
//! One web defect is fixed (N-optionChain-03, lead decision 10): an explicit
//! `expiryDate` wins over an expiry embedded in the underlying, as it does
//! for Option Symbol; the web's chain service let the embedded one win. The
//! node then sends the underlying without its embedded date.

use crate::flow::contract::node::{FlowNode, NodeCtx, NodeOutcome};
use crate::flow::contract::pyfmt;
use crate::flow::contract::vars::has_unresolved;
use crate::flow::node_contracts::{is_expiry_date, parse_underlying_symbol};
use async_trait::async_trait;
use serde_json::{json, Map, Value};

/// Store and return a client reply, logging it as `{label}: {result}`.
fn reply(ctx: &NodeCtx, data: &Map<String, Value>, label: &str, result: Value) -> NodeOutcome {
    ctx.info(format!("{}: {}", label, pyfmt::str(&result)));
    ctx.store_output(data, result.clone());
    NodeOutcome::result(result)
}

/// A node refused before any call, its sentence logged at error level.
fn refuse(ctx: &NodeCtx, message: String) -> NodeOutcome {
    ctx.error(message.clone());
    NodeOutcome::error(message)
}

/// `option_expiry_error`: an option node needs a resolved underlying, and a
/// DDMMMYY expiry unless the underlying carries one (Synthetic Future always
/// needs it). Refused before any call; `None` when the node may run.
fn option_expiry_error(
    ctx: &NodeCtx,
    node_type: &str,
    data: &Map<String, Value>,
) -> Option<NodeOutcome> {
    let unresolved: Vec<(&str, String)> = ["underlying", "expiryDate"]
        .into_iter()
        .filter_map(|field| match data.get(field) {
            Some(Value::String(raw)) if raw.contains("{{") => {
                let text = ctx.vars.interpolate(raw);
                has_unresolved(&text).then_some((field, text))
            }
            _ => None,
        })
        .collect();
    if !unresolved.is_empty() {
        let detail = unresolved
            .iter()
            .map(|(k, t)| format!("{}={}", k, pyfmt::quote(t)))
            .collect::<Vec<_>>()
            .join(", ");
        let message = format!(
            "{} has unresolved variable(s) in {}. Refusing to send it with substituted values.",
            node_type, detail
        );
        ctx.error(message.clone());
        let keys: Vec<&str> = unresolved.iter().map(|(k, _)| *k).collect();
        return Some(NodeOutcome::result(
            json!({"status": "error", "message": message, "unresolved": keys}),
        ));
    }
    let underlying = ctx.get_str(data, "underlying", "").trim().to_string();
    let expiry = ctx.get_str(data, "expiryDate", "").trim().to_string();
    if underlying.is_empty() {
        return Some(refuse(
            ctx,
            format!(
                "{} needs a resolved underlying before it can request option data.",
                node_type
            ),
        ));
    }
    let (_, embedded) = parse_underlying_symbol(&underlying);
    let needs_expiry = node_type == "syntheticFuture" || !expiry.is_empty() || embedded.is_none();
    if needs_expiry && !is_expiry_date(&expiry.to_uppercase()) {
        return Some(refuse(
            ctx,
            format!(
                "{} needs a resolved expiryDate in DDMMMYY format before it can run.",
                node_type
            ),
        ));
    }
    None
}

/// Symbol: the instrument's master-contract row (lot size, tick size, ...).
pub struct SymbolNode;

#[async_trait]
impl FlowNode for SymbolNode {
    fn node_type(&self) -> &'static str {
        "symbol"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let symbol = ctx.get_str(data, "symbol", "");
        let exchange = ctx.get_str(data, "exchange", "NSE");
        ctx.info(format!(
            "Getting symbol info for: {} ({})",
            symbol, exchange
        ));
        let result = ctx.client.symbol(&symbol, &exchange).await;
        reply(ctx, data, "Symbol result", result)
    }
}

/// Expiry: the listed expiries of an F&O underlying.
pub struct ExpiryNode;

#[async_trait]
impl FlowNode for ExpiryNode {
    fn node_type(&self) -> &'static str {
        "expiry"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let symbol = ctx.get_str(data, "symbol", "NIFTY");
        let exchange = ctx.get_str(data, "exchange", "NFO");
        let mut kind = ctx
            .get_str(data, "instrumenttype", "options")
            .to_lowercase();
        if kind != "options" && kind != "futures" {
            kind = "options".into();
        }
        ctx.info(format!("Getting {} expiry dates for: {}", kind, symbol));
        let result = ctx.client.get_expiry(&symbol, &exchange, &kind).await;
        reply(ctx, data, "Expiry result", result)
    }
}

/// Option Symbol: the contract an offset (ATM, ITM2, OTM3) resolves to.
pub struct OptionSymbolNode;

#[async_trait]
impl FlowNode for OptionSymbolNode {
    fn node_type(&self) -> &'static str {
        "optionSymbol"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        if let Some(refused) = option_expiry_error(ctx, self.node_type(), data) {
            return refused;
        }
        let underlying = ctx.get_str(data, "underlying", "NIFTY");
        let exchange = ctx.get_str(data, "exchange", "NSE_INDEX");
        let expiry = ctx.get_str(data, "expiryDate", "");
        let offset = ctx.get_str(data, "offset", "ATM");
        let option_type = ctx.get_str(data, "optionType", "CE");
        ctx.info(format!(
            "Resolving option symbol: {} {} {}",
            underlying, option_type, offset
        ));
        let result = ctx
            .client
            .optionsymbol(&underlying, &exchange, &expiry, &offset, &option_type)
            .await;
        reply(ctx, data, "Option symbol result", result)
    }
}

/// Option Chain: strikes around ATM with each leg's quote.
pub struct OptionChainNode;

#[async_trait]
impl FlowNode for OptionChainNode {
    fn node_type(&self) -> &'static str {
        "optionChain"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        if let Some(refused) = option_expiry_error(ctx, self.node_type(), data) {
            return refused;
        }
        let underlying = ctx.get_str(data, "underlying", "NIFTY");
        let exchange = ctx.get_str(data, "exchange", "NSE_INDEX");
        let expiry = ctx.get_str(data, "expiryDate", "");
        let strike_count = ctx.get_int(data, "strikeCount", 10);
        ctx.info(format!(
            "Fetching option chain for: {} expiry={}",
            underlying, expiry
        ));
        // An explicit expiry wins: the chain is asked for the bare
        // underlying, so an expiry inside it cannot override the node's.
        let (base, embedded) = parse_underlying_symbol(&underlying);
        let asked = if !expiry.is_empty() && embedded.is_some() {
            base
        } else {
            underlying
        };
        let result = ctx
            .client
            .optionchain(&asked, &exchange, &expiry, strike_count)
            .await;
        ctx.info("Option chain result received");
        ctx.store_output(data, result.clone());
        NodeOutcome::result(result)
    }
}

/// Synthetic Future: the future's price implied by the ATM call and put.
pub struct SyntheticFutureNode;

#[async_trait]
impl FlowNode for SyntheticFutureNode {
    fn node_type(&self) -> &'static str {
        "syntheticFuture"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        if let Some(refused) = option_expiry_error(ctx, self.node_type(), data) {
            return refused;
        }
        let underlying = ctx.get_str(data, "underlying", "NIFTY");
        let exchange = ctx.get_str(data, "exchange", "NSE_INDEX");
        let expiry = ctx.get_str(data, "expiryDate", "");
        ctx.info(format!("Calculating synthetic future for: {}", underlying));
        let result = ctx
            .client
            .syntheticfuture(&underlying, &exchange, &expiry)
            .await;
        reply(ctx, data, "Synthetic future result", result)
    }
}
