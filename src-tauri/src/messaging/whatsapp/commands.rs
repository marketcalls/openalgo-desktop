//! WhatsApp slash commands from the paired owner (web `_dispatch_command`
//! and `_cmd_*`). Only the owner's own messages outside groups reach here
//! (`command_allowed`). The account is the desktop's single user; calls run
//! in-process with the stored API key, never over HTTP.

use crate::messaging::format::py_str;
use crate::messaging::openalgo::OpenAlgoClient;
use crate::services::apikey_service::ApiKeyService;
use crate::state::AppState;
use serde_json::Value;
use std::sync::Arc;

pub const HELP: &str = "OpenAlgo WhatsApp Bot\n/status - connection + paired status\n/orderbook - today's orders\n/tradebook - today's trades\n/positions - open positions\n/holdings - holdings\n/funds - account funds\n/pnl - net P&L\n/quote <symbol> [exchange] - last traded price\n/closeall - square off all positions\n/mode - live or analyze mode";

/// Every command the bot answers.
pub const COMMANDS: &[&str] = &[
    "start", "help", "menu", "status", "orderbook", "tradebook", "positions", "holdings", "funds",
    "pnl", "quote", "closeall", "mode",
];

/// Web `_format_dict`.
pub fn format_dict(v: &Value) -> String {
    match v {
        Value::Object(m) => m
            .iter()
            .map(|(k, v)| format!("{}: {}", k, format_dict(v)))
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Array(a) => {
            if a.is_empty() {
                return "(empty)".into();
            }
            a.iter()
                .take(10)
                .map(|x| format!("- {}", format_dict(x)))
                .collect::<Vec<_>>()
                .join("\n")
        }
        other => py_str(other),
    }
}

/// Web `_send_truncated` text.
pub fn truncated(title: &str, payload: &Value) -> String {
    let mut body = format_dict(payload);
    if body.chars().count() > 3500 {
        body = body.chars().take(3500).collect::<String>() + "\n...(truncated)";
    }
    format!("*{}*\n{}", title, body)
}

/// The owner's client (web `_sdk_client_for_owner`), or the reply to send.
fn owner_client(ctx: &Arc<AppState>) -> Result<OpenAlgoClient, String> {
    let cfg = super::WhatsAppService::config(ctx);
    if cfg.owner_username.as_deref().unwrap_or("").is_empty() {
        return Err("No owner recorded for this paired device. Re-pair from the /whatsapp page while logged in to OpenAlgo.".into());
    }
    match ApiKeyService::current(ctx) {
        Ok(Some(k)) => Ok(OpenAlgoClient::new(ctx.clone(), k)),
        Ok(None) => Err(
            "No API key on file for the owner. Generate one at /apikey on the web UI, then try again."
                .into(),
        ),
        Err(e) => {
            tracing::error!("Could not load the owner's API key: {}", e);
            Err("Could not load OpenAlgo API key for the owner.".into())
        }
    }
}

/// The reply to a command (None for nothing to say).
pub async fn reply_for(ctx: &Arc<AppState>, cmd: &str, args: &[String]) -> String {
    match cmd {
        "start" | "help" | "menu" => HELP.into(),
        "status" => {
            let cfg = super::WhatsAppService::config(ctx);
            let mut lines = vec![
                format!("Bot connected: {}", if cfg.is_active { "yes" } else { "no" }),
                format!("Device paired: {}", if cfg.is_paired { "yes" } else { "no" }),
            ];
            if let Some(p) = cfg.own_phone.filter(|p| !p.is_empty()) {
                lines.push(format!("Paired number: +{}", p));
            }
            if let Some(o) = cfg.owner_username.filter(|o| !o.is_empty()) {
                lines.push(format!("Owner: {}", o));
            }
            lines.join("\n")
        }
        "mode" => {
            let mode = match ctx.sqlite.get_analyze_mode() {
                Ok(true) => "analyze",
                Ok(false) => "live",
                Err(_) => "unknown",
            };
            format!("Trading mode: {}", mode)
        }
        "quote" if args.is_empty() => "Usage: /quote <symbol> [exchange]".into(),
        _ => {
            let client = match owner_client(ctx) {
                Ok(c) => c,
                Err(m) => return m,
            };
            let (title, resp, what) = match cmd {
                "orderbook" => ("Orderbook".to_string(), client.orderbook().await, "orderbook"),
                "tradebook" => ("Tradebook".to_string(), client.tradebook().await, "tradebook"),
                "positions" => ("Positions".to_string(), client.positionbook().await, "positions"),
                "holdings" => ("Holdings".to_string(), client.holdings().await, "holdings"),
                "funds" => ("Funds".to_string(), client.funds().await, "funds"),
                "pnl" => ("P&L".to_string(), client.positionbook().await, "P&L"),
                "quote" => {
                    let s = args[0].to_uppercase();
                    let e = args.get(1).map(|e| e.to_uppercase()).unwrap_or_else(|| "NSE".into());
                    (format!("Quote {} {}", s, e), client.quotes(&s, &e).await, "quote")
                }
                "closeall" => {
                    return match client.closeposition().await {
                        Some(r) => format!("Close-all response:\n{}", format_dict(&r)),
                        None => "Close-all failed: no response".into(),
                    }
                }
                _ => return "Unknown command. Send /help for the list.".into(),
            };
            match resp {
                Some(r) => truncated(&title, &r),
                None => format!("Failed to fetch {}: no response", what),
            }
        }
    }
}

/// Parse and answer one command (web `_dispatch_command`).
pub async fn dispatch(ctx: &Arc<AppState>, chat: &str, sender: &str, text: &str) {
    let mut parts = text.split_whitespace();
    let cmd = parts
        .next()
        .unwrap_or("")
        .trim_start_matches('/')
        .to_lowercase();
    let args: Vec<String> = parts.map(String::from).collect();
    let svc = &ctx.messaging.whatsapp;
    if !COMMANDS.contains(&cmd.as_str()) {
        svc.send(ctx, &[chat.to_string()], "Unknown command. Send /help for the list.")
            .await;
        return;
    }
    let _ = ctx.sqlite.conn().and_then(|c| {
        super::db::log_command(&c, sender, &cmd, &args, ctx.now())
    });
    let reply = reply_for(ctx, &cmd, &args).await;
    svc.send(ctx, &[chat.to_string()], &reply).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn python_dict_formatting() {
        let v = json!({"status": "success", "data": {"availablecash": "100.00", "list": [], "n": null}});
        assert_eq!(
            format_dict(&v),
            // Keys print in the map's (sorted) order, not the web's
            // insertion order.
            "data: availablecash: 100.00\nlist: (empty)\nn: None\nstatus: success"
        );
        let l = json!([{"a": 1}, 2]);
        assert_eq!(format_dict(&l), "- a: 1\n- 2");
        let long = json!({"x": "y".repeat(4000)});
        let t = truncated("Funds", &long);
        assert!(t.starts_with("*Funds*\nx: yyy"));
        assert!(t.ends_with("\n...(truncated)"));
    }
}
