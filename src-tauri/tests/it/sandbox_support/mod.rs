//! Shared harness for the sandbox integration tests: an in-memory sandbox
//! with a manual IST clock, a settable quote table, a symbol master seeded
//! like the web's `test/sandbox/conftest.py` (plus F&O, MCX, CDS, NCDEX and
//! crypto instruments), and an event recorder on a real event bus.

#![allow(dead_code)]

use chrono::NaiveDate;
use openalgo_desktop_lib::clock::ManualClock;
use openalgo_desktop_lib::events::{Event, EventBus, Lane, Subscriber, Topic};
use openalgo_desktop_lib::sandbox::clock::{manual_clock_at, set_ist};
use openalgo_desktop_lib::sandbox::{
    OrderRequest, Quote, Sandbox, SandboxDeps, SandboxOptions, StaticQuoteSource, StaticSymbols,
    SymbolMeta,
};
use parking_lot::Mutex;
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

pub const USER: &str = "testuser";

pub fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

pub fn symbols() -> StaticSymbols {
    let mut s = StaticSymbols::new();
    for sym in ["ZEEL", "RELIANCE", "SBIN", "INFY", "TCS", "HDFCBANK", "ITC"] {
        s.insert(sym, "NSE", SymbolMeta::default());
        s.insert(sym, "BSE", SymbolMeta::default());
    }
    let lot = |n| SymbolMeta {
        lotsize: n,
        ..SymbolMeta::default()
    };
    s.insert("NIFTY06OCT2622400CE", "NFO", lot(65));
    s.insert("NIFTY06OCT2622400PE", "NFO", lot(65));
    s.insert("NIFTY27OCT26FUT", "NFO", lot(65));
    s.insert("CRUDEOIL19OCT26FUT", "MCX", lot(100));
    s.insert("USDINR29OCT26FUT", "CDS", lot(1));
    s.insert("GUARSEED10NOV26FUT", "NCDEX", lot(5));
    s.insert(
        "BTCUSD.P",
        "CRYPTO",
        SymbolMeta {
            lotsize: 1,
            contract_value: d("0.001"),
            expiry: None,
        },
    );
    s.insert(
        "SENSEX",
        "BSE_INDEX",
        SymbolMeta {
            expiry: NaiveDate::from_ymd_opt(2030, 1, 1),
            ..SymbolMeta::default()
        },
    );
    s
}

/// Records sandbox-related events.
#[derive(Default)]
pub struct Recorder {
    pub events: Mutex<Vec<(String, String)>>,
}

#[async_trait::async_trait]
impl Subscriber for Recorder {
    fn name(&self) -> &'static str {
        "sandbox-test-recorder"
    }
    fn topics(&self) -> Vec<Topic> {
        vec![
            Topic::OrderUpdate,
            Topic::SandboxOrderFilled,
            Topic::SandboxAutoSquareoff,
            Topic::SandboxT1Settlement,
            Topic::GttTriggered,
            Topic::GttExpired,
        ]
    }
    async fn handle(&self, event: Arc<Event>) {
        let detail = match &*event {
            Event::OrderUpdate(u) => format!("{}:{}", u.orderid, u.order_status),
            Event::Gtt { trigger_id, .. } => trigger_id.clone(),
            _ => String::new(),
        };
        self.events
            .lock()
            .push((event.topic().as_str().to_string(), detail));
    }
}

impl Recorder {
    pub fn topics(&self) -> Vec<String> {
        self.events.lock().iter().map(|(t, _)| t.clone()).collect()
    }

    pub fn count(&self, topic: &str) -> usize {
        self.events
            .lock()
            .iter()
            .filter(|(t, _)| t == topic)
            .count()
    }

    /// Order statuses announced for one order, in order.
    pub fn statuses(&self, orderid: &str) -> Vec<String> {
        self.events
            .lock()
            .iter()
            .filter(|(t, _)| t == "order.update")
            .filter_map(|(_, d)| {
                let (id, st) = d.split_once(':')?;
                (id == orderid).then(|| st.to_string())
            })
            .collect()
    }

    pub async fn wait_for(&self, topic: &str, n: usize) {
        for _ in 0..200 {
            if self.count(topic) >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!(
            "expected {n} '{topic}' events, saw {:?}",
            self.events.lock().clone()
        );
    }
}

pub struct Env {
    pub sb: Sandbox,
    pub clock: Arc<ManualClock>,
    pub quotes: Arc<StaticQuoteSource>,
    pub bus: Arc<EventBus>,
    pub rec: Arc<Recorder>,
}

impl Env {
    /// A fresh sandbox at an IST wall-clock time (`YYYY-MM-DD HH:MM:SS`).
    pub fn at(ist: &str) -> Self {
        Self::with_opts(ist, SandboxOptions::default())
    }

    pub fn with_opts(ist: &str, opts: SandboxOptions) -> Self {
        let clock = manual_clock_at(ist);
        let quotes = Arc::new(StaticQuoteSource::new());
        let bus = Arc::new(EventBus::new());
        let rec = Arc::new(Recorder::default());
        bus.subscribe(rec.clone(), Lane::Critical);
        let opts = SandboxOptions {
            user_id: USER.to_string(),
            quote_retry_delays: vec![Duration::ZERO, Duration::ZERO],
            ..opts
        };
        let sb = Sandbox::in_memory(
            SandboxDeps {
                symbols: Arc::new(symbols()),
                quotes: quotes.clone(),
                clock: clock.clone(),
                bus: Some(bus.clone()),
            },
            opts,
        )
        .unwrap();
        Self {
            sb,
            clock,
            quotes,
            bus,
            rec,
        }
    }

    pub fn set_time(&self, ist: &str) {
        set_ist(&self.clock, ist);
    }

    pub fn advance(&self, secs: i64) {
        self.clock.advance(chrono::Duration::seconds(secs));
    }

    pub fn ltp(&self, symbol: &str, exchange: &str, ltp: &str) {
        self.quotes.set_ltp(symbol, exchange, d(ltp));
    }

    pub fn quote(&self, symbol: &str, exchange: &str, q: Quote) {
        self.quotes.set(symbol, exchange, q);
    }

    pub async fn used(&self) -> Decimal {
        self.sb
            .funds_row()
            .await
            .unwrap()
            .map(|f| f.used_margin)
            .unwrap_or_default()
    }

    pub async fn available(&self) -> Decimal {
        self.sb
            .funds_row()
            .await
            .unwrap()
            .map(|f| f.available_balance)
            .unwrap_or_default()
    }

    pub async fn qty(&self, symbol: &str, exchange: &str, product: &str) -> i64 {
        self.sb
            .position_row(symbol, exchange, product)
            .await
            .unwrap()
            .map(|p| p.quantity)
            .unwrap_or(0)
    }

    pub async fn status(&self, orderid: &str) -> String {
        self.sb
            .order_row(orderid)
            .await
            .unwrap()
            .map(|o| o.order_status.as_str().to_string())
            .unwrap_or_default()
    }

    pub async fn assert_margin_consistent(&self) {
        assert_eq!(
            self.sb.margin_discrepancy().await.unwrap(),
            Decimal::ZERO,
            "used_margin must equal the margin the books hold"
        );
    }

    pub async fn shutdown(self) {
        self.sb.shutdown().await;
        self.bus.shutdown(Duration::from_secs(1)).await;
    }
}

/// Build an order request.
pub fn req(
    symbol: &str,
    exchange: &str,
    action: &str,
    qty: i64,
    pt: &str,
    product: &str,
) -> OrderRequest {
    OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        action: action.into(),
        quantity: qty,
        price: None,
        trigger_price: None,
        price_type: pt.into(),
        product: product.into(),
        strategy: "test".into(),
    }
}

pub fn limit(r: OrderRequest, price: &str) -> OrderRequest {
    OrderRequest {
        price: Some(d(price)),
        ..r
    }
}

pub fn trigger(r: OrderRequest, t: &str) -> OrderRequest {
    OrderRequest {
        trigger_price: Some(d(t)),
        ..r
    }
}

/// Place and return the order id (panics on refusal).
pub async fn place(env: &Env, r: OrderRequest) -> String {
    env.sb.place_order(r).await.unwrap().orderid
}
