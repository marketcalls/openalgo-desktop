//! Dhan Sandbox's market data: the web's simulated feed
//! (`broker/dhan_sandbox/streaming/dhan_sandbox_adapter.py`), ported
//! exactly (maintainer, 2026-11-09). Dhan's sandbox streams nothing, so
//! the web makes prices up: every second each subscribed instrument's
//! price takes a random step from a starting price worked out from its
//! symbol, and the rest of the frame is drawn around it. They are
//! simulated prices, not market prices; the Broker page and the user guide
//! say so, and analyzer mode fills at them as the web's does.
//!
//! Where the parts live:
//! * `model.rs`: the web's price model (starting price, the step, the
//!   frame's fields).
//! * `ticker.rs`: a loopback socket that sends one tick frame a second, so
//!   the shared `WebSocketManager` runs this feed as it runs a broker's
//!   (connect, reconnect, watchdog, re-subscribe).
//! * this file: the `BrokerFeed`, which keeps the subscriptions and the
//!   prices and turns each tick frame into the second's events.
//!
//! As on the web: one subscription per instrument, at the mode asked for
//! last (the bridge serves the lower modes from it); an instrument keeps
//! its price when it is unsubscribed, so subscribing again goes on from
//! there. The seams for tests are the generator's seed and the clock.

mod model;
mod ticker;

#[cfg(test)]
mod tests;

use crate::brokers::common::streaming::{
    BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, WsRequest,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::clock::{Clock, SystemClock};
use crate::error::{AppError, Result};
use model::SpotHints;
use parking_lot::Mutex;
use rand::rngs::StdRng;
use rand::SeedableRng;
use std::collections::HashMap;
use std::sync::Arc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// Prices kept for instruments no longer subscribed; past it only the
/// subscribed ones' are kept.
const MAX_PRICES: usize = 10_000;

/// Where the tick frames come from.
enum Ticks {
    /// The loopback ticker, started at the first connect.
    Ticker(Mutex<Option<ticker::Ticker>>),
    /// A socket the test names (the corpus's fake broker).
    At(String),
}

/// One subscribed instrument (web `self.subscriptions[f"{exchange}_{symbol}"]`).
#[derive(Debug, Clone, PartialEq)]
struct Sub {
    symbol: String,
    exchange: String,
    mode: FeedMode,
}

impl Sub {
    fn key(&self) -> String {
        price_key(&self.exchange, &self.symbol)
    }
}

fn price_key(exchange: &str, symbol: &str) -> String {
    format!("{}_{}", exchange, symbol)
}

/// The simulated Dhan Sandbox feed.
pub struct DhanSandboxFeed {
    symbols: SymbolResolver,
    ticks: Ticks,
    /// In subscription order, as the web's dict iterates.
    subs: Vec<Sub>,
    prices: HashMap<String, f64>,
    spots: SpotHints,
    rng: StdRng,
    clock: Arc<dyn Clock>,
}

impl DhanSandboxFeed {
    /// The production feed: its own ticker, a generator seeded by the
    /// operating system, the system clock.
    pub(crate) fn new(symbols: SymbolResolver) -> Self {
        Self::with(
            symbols,
            Ticks::Ticker(Mutex::new(None)),
            StdRng::from_entropy(),
        )
    }

    /// A feed whose tick frames come from `url`, with a generator seeded by
    /// `seed`, so a run repeats (tests).
    pub fn seeded(url: &str, symbols: SymbolResolver, seed: u64) -> Self {
        Self::with(
            symbols,
            Ticks::At(url.to_string()),
            StdRng::seed_from_u64(seed),
        )
    }

    fn with(symbols: SymbolResolver, ticks: Ticks, rng: StdRng) -> Self {
        Self {
            symbols,
            ticks,
            subs: Vec::new(),
            prices: HashMap::new(),
            spots: SpotHints::default(),
            rng,
            clock: Arc::new(SystemClock),
        }
    }

    /// The clock the frames are stamped with (tests).
    #[cfg(test)]
    fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// web `subscribe`: the instrument at `mode`, starting from its
    /// reference price the first time it is seen.
    fn add(&mut self, s: &FeedSubscription) {
        let sub = Sub {
            symbol: s.symbol.clone(),
            exchange: s.exchange.clone(),
            mode: s.mode,
        };
        let key = sub.key();
        match self.subs.iter_mut().find(|x| x.key() == key) {
            Some(held) => held.mode = sub.mode,
            None => self.subs.push(sub),
        }
        if !self.prices.contains_key(&key) {
            if self.prices.len() >= MAX_PRICES {
                let held: Vec<String> = self.subs.iter().map(Sub::key).collect();
                self.prices.retain(|k, _| held.contains(k));
            }
            let (symbols, spots) = (&self.symbols, &mut self.spots);
            let start = model::reference_price(&s.symbol, &mut |u| spots.spot(symbols, u));
            self.prices.insert(key, start);
        }
    }

    /// One wake of the web's `_mock_streaming_loop`: every subscription's
    /// price steps once and its frame goes out.
    fn second(&mut self) -> Vec<FeedEvent> {
        let now = self.clock.now().timestamp_millis();
        let Self {
            symbols,
            subs,
            prices,
            spots,
            rng,
            ..
        } = self;
        let mut out = Vec::new();
        for sub in subs.iter() {
            let key = sub.key();
            let current = match prices.get(&key) {
                Some(p) => *p,
                None => model::reference_price(&sub.symbol, &mut |u| spots.spot(symbols, u)),
            };
            let fair = model::option_contract(&sub.symbol).map(|c| {
                let spot = spots.spot(symbols, &c.underlying).unwrap_or(c.strike);
                model::fair_premium(spot, &c)
            });
            let price = model::next_price(current, fair, rng);
            prices.insert(key, price);
            out.extend(model::events(
                &sub.symbol,
                &sub.exchange,
                sub.mode,
                price,
                now,
                rng,
            ));
        }
        out
    }
}

impl BrokerFeed for DhanSandboxFeed {
    fn broker(&self) -> &'static str {
        "dhan_sandbox"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        let url = match &self.ticks {
            Ticks::Ticker(slot) => ticker::ensure_started(slot)?,
            Ticks::At(url) => url.clone(),
        };
        url.as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("Dhan Sandbox feed address is invalid".into()))
    }

    /// The manager subscribes everything again after a connect.
    fn on_connected(&mut self) -> Vec<Message> {
        self.subs.clear();
        Vec::new()
    }

    /// Nothing goes to the ticker: the subscriptions live here.
    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        for s in subs {
            self.add(s);
        }
        Vec::new()
    }

    /// web `unsubscribe`: the instrument goes, its price stays.
    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        for s in subs {
            let key = price_key(&s.exchange, &s.symbol);
            self.subs.retain(|x| x.key() != key);
        }
        Vec::new()
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Text(t) if t.as_str() == ticker::TICK => self.second(),
            _ => Vec::new(),
        }
    }

    /// web `subscribe`: `"supported_depth": 5`.
    fn supported_depth_levels(&self) -> &'static [u8] {
        &[5]
    }
}
