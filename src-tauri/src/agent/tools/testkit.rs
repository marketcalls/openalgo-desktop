//! A signed-in OpenAlgo for the tool tests: an account and its real API
//! key, a mock broker session, a small instrument master and a fixed IST
//! clock, so a tool goes through the real in-process `/api/v1` (key check,
//! analyzer routing, the web's response shapes) to a broker the test
//! scripts.
//!
//! [`Kit::call`] runs a tool the way a run does: the arguments are read and
//! checked against the tool's schema first (`agent::tool_guard`), so a
//! shape the schema refuses never reaches the tool.

use crate::agent::contract::tool::{
    AgentTool, Capabilities, Surface, ToolCtx, ToolGroup, ToolOutcome,
};
use crate::agent::contract::viz::VizSink;
use crate::agent::tool_guard;
use crate::brokers::common::symbols::SymToken;
use crate::brokers::mock::MockBroker;
use crate::brokers::BrokerRegistry;
use crate::security::Secret;
use crate::services::auth_service::AuthService;
use crate::services::broker_auth_service::BrokerAuthService;
use crate::state::testing::{build, TestCtx};
use crate::state::{AppState, BrokerSession};
use chrono::{DateTime, TimeZone, Utc};
use serde_json::{Map, Value};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// 10:00 IST on Friday 9 October 2026, a trading day.
pub fn friday_morning() -> DateTime<Utc> {
    crate::agent::tools::market::IST
        .with_ymd_and_hms(2026, 10, 9, 10, 0, 0)
        .single()
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_default()
}

/// One master row.
pub fn row(
    symbol: &str,
    exchange: &str,
    kind: &str,
    expiry: &str,
    strike: f64,
    lot: i32,
) -> SymToken {
    let name = symbol
        .chars()
        .take_while(|c| c.is_ascii_alphabetic() || *c == '&')
        .collect::<String>();
    SymToken {
        symbol: symbol.into(),
        brsymbol: symbol.into(),
        name,
        exchange: exchange.into(),
        brexchange: exchange.into(),
        token: format!("{}{}", exchange, symbol),
        expiry: expiry.into(),
        strike,
        lot_size: lot,
        instrument_type: kind.into(),
        tick_size: 0.05,
    }
}

/// The instruments the tests name: shares, indices, NIFTY futures and a
/// ladder of NIFTY options on two expiries.
pub fn master_rows() -> Vec<SymToken> {
    let mut rows = vec![
        row("INFY", "NSE", "EQ", "", 0.0, 1),
        row("SBIN", "NSE", "EQ", "", 0.0, 1),
        row("RELIANCE", "NSE", "EQ", "", 0.0, 1),
        row("RELIANCE", "BSE", "EQ", "", 0.0, 1),
        row("NIFTY", "NSE_INDEX", "INDEX", "", 0.0, 1),
        row("SENSEX", "BSE_INDEX", "INDEX", "", 0.0, 1),
        row("NIFTY27OCT26FUT", "NFO", "FUT", "27-OCT-26", 0.0, 75),
        row("NIFTY24NOV26FUT", "NFO", "FUT", "24-NOV-26", 0.0, 75),
    ];
    for (expiry, master) in [("13OCT26", "13-OCT-26"), ("27OCT26", "27-OCT-26")] {
        for strike in (24000..=25000).step_by(100) {
            for right in ["CE", "PE"] {
                rows.push(row(
                    &format!("NIFTY{}{}{}", expiry, strike, right),
                    "NFO",
                    right,
                    master,
                    f64::from(strike),
                    75,
                ));
            }
        }
    }
    rows
}

/// A signed-in OpenAlgo with a mock broker.
pub struct Kit {
    pub t: TestCtx,
    pub mock: Arc<MockBroker>,
    pub key: String,
}

impl Kit {
    pub fn new() -> Kit {
        let mock = Arc::new(MockBroker::new("zerodha"));
        let t = build(
            BrokerRegistry::with(vec![mock.clone() as Arc<dyn crate::brokers::Broker>]),
            friday_morning(),
        );
        let app = t.ctx.clone();
        AuthService::setup(&app, "trader", "trader@example.com", "Secret@123")
            .unwrap_or_else(|e| panic!("setup: {:?}", e));
        let key = crate::services::apikey_service::ApiKeyService::current(&app)
            .ok()
            .flatten()
            .map(|k| k.expose().to_string())
            .unwrap_or_else(|| panic!("no API key"));
        BrokerAuthService::persist(
            &app,
            &BrokerSession {
                broker_id: "zerodha".into(),
                auth_token: "mock-access-token".into(),
                feed_token: None,
                user_id: "AB1234".into(),
                user_name: None,
                authenticated_at: app.now(),
            },
        )
        .unwrap_or_else(|e| panic!("broker session: {:?}", e));
        app.symbols.load(master_rows());
        Kit { t, mock, key }
    }

    pub fn app(&self) -> &Arc<AppState> {
        &self.t.ctx
    }

    /// Turn analyzer (sandbox) mode on or off.
    pub fn analyzer(&self, on: bool) {
        self.app()
            .sqlite
            .set_analyze_mode(on)
            .unwrap_or_else(|e| panic!("analyzer: {:?}", e));
    }

    /// A run's context on `surface`, with the account's real key.
    pub fn ctx(&self, surface: Surface) -> ToolCtx {
        ToolCtx {
            app: self.app().clone(),
            surface,
            conversation_id: Some(1),
            run_id: "run-1".into(),
            session_id: "session-1".into(),
            user_id: "trader".into(),
            api_key: Secret::new(self.key.clone()),
            caps: Capabilities::default(),
            analyzer_mode: false,
            operator_message: String::new(),
            chart_context: None,
            viz: VizSink::new(),
            cancel: CancellationToken::new(),
        }
    }

    /// Run tool `name` of `group` with `args` as a run does: read, checked
    /// against the schema, then called.
    pub async fn call_in(
        &self,
        group: &ToolGroup,
        ctx: &ToolCtx,
        name: &str,
        args: Value,
    ) -> ToolOutcome {
        let tools: Vec<Arc<dyn AgentTool>> = (group.build)(ctx);
        let Some(tool) = tools.iter().find(|t| t.spec().name == name) else {
            panic!("{} is not a tool of {}", name, group.key)
        };
        call_checked(tool.as_ref(), ctx, &args).await
    }

    /// [`Kit::call_in`] on the chat surface.
    pub async fn call(&self, group: &ToolGroup, name: &str, args: Value) -> ToolOutcome {
        let ctx = self.ctx(Surface::Chat);
        self.call_in(group, &ctx, name, args).await
    }
}

/// The tool guard, then the tool.
pub async fn call_checked(tool: &dyn AgentTool, ctx: &ToolCtx, args: &Value) -> ToolOutcome {
    let read = match tool_guard::read_arguments(args) {
        Ok(a) => a,
        Err(m) => return ToolOutcome::Retry(m),
    };
    match tool_guard::validate(&tool.spec().parameters, &read) {
        Ok(checked) => tool.call(ctx, checked).await,
        Err(refusals) => {
            ToolOutcome::Retry(tool_guard::correction(tool.spec().name, &refusals, &read))
        }
    }
}

/// The JSON inside a `<tool_result ...>` block.
pub fn body(outcome: &ToolOutcome) -> Value {
    let text = outcome.text();
    let inner = text
        .split_once('\n')
        .map(|(_, rest)| rest)
        .unwrap_or(text)
        .trim_end_matches("</tool_result>")
        .trim_end_matches('\n');
    serde_json::from_str(inner).unwrap_or_else(|e| panic!("{}: {}", e, text))
}

/// The opening tag of a result.
pub fn tag(outcome: &ToolOutcome) -> &str {
    outcome.text().lines().next().unwrap_or_default()
}

/// A map from a JSON object literal.
pub fn map(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}
