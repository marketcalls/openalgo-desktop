//! How the bridge brings the managers in line with what feed clients want:
//! a desired key becomes a manager reference on the main feed, or on the
//! broker's depth socket for the deep book plus a quote on the main feed,
//! and a reference nobody wants any more is released.

use super::{mode_of, BrokerBridge, DesiredKey, State};
use crate::brokers::common::streaming::{FeedMode, FeedSubscription};
use crate::feed::source::{Mode, DEFAULT_DEPTH};

/// What one desired key holds on the managers.
#[derive(Debug, Clone)]
pub(super) enum Applied {
    Main(FeedSubscription),
    /// Depth socket book plus the main feed's quote for the same instrument.
    Deep {
        depth: FeedSubscription,
        quote: FeedSubscription,
    },
}

impl Applied {
    pub(super) fn main_subs(&self) -> Vec<FeedSubscription> {
        match self {
            Applied::Main(s) => vec![s.clone()],
            Applied::Deep { quote, .. } => vec![quote.clone()],
        }
    }
}

impl BrokerBridge {
    /// Drop manager references held for `applied` entries.
    pub(super) async fn release(&self, applied: Vec<Applied>) {
        let mut main = Vec::new();
        let mut deep = Vec::new();
        for a in applied {
            match a {
                Applied::Main(s) => main.push(s),
                Applied::Deep { depth, quote } => {
                    deep.push(depth);
                    main.push(quote);
                }
            }
        }
        if !main.is_empty() {
            if let Err(e) = self.manager.unsubscribe(main).await {
                tracing::warn!("Could not release market data subscriptions: {}", e);
            }
        }
        if !deep.is_empty() {
            if let Err(e) = self.depth_manager.unsubscribe(deep).await {
                tracing::warn!("Could not release market depth subscriptions: {}", e);
            }
        }
    }

    /// How a desired key is applied (deep route when its levels match).
    fn applied_for(&self, key: &DesiredKey) -> Option<Applied> {
        let sub = self.subscription(key)?;
        let deep = *self.deep.lock();
        Some(match deep {
            Some(levels) if key.1 == Mode::Depth && key.2 == levels => Applied::Deep {
                quote: FeedSubscription {
                    mode: FeedMode::Quote,
                    depth: DEFAULT_DEPTH,
                    ..sub.clone()
                },
                depth: sub,
            },
            _ => Applied::Main(sub),
        })
    }

    fn subscription(&self, key: &DesiredKey) -> Option<FeedSubscription> {
        let (k, mode, depth) = key;
        let row = self.symbols.by_symbol(&k.exchange, &k.symbol)?;
        Some(FeedSubscription {
            symbol: k.symbol.clone(),
            exchange: k.exchange.clone(),
            token: row.token.clone(),
            brsymbol: row.br_symbol().to_string(),
            brexchange: row.br_exchange().to_string(),
            mode: mode_of(*mode),
            depth: *depth,
        })
    }

    /// Bring the manager in line with the desired set.
    pub(super) async fn reconcile(&self) {
        let (add, remove) = {
            let mut st = self.state.lock();
            let add: Vec<DesiredKey> = st
                .desired
                .iter()
                .filter(|k| !st.applied.contains_key(*k))
                .cloned()
                .collect();
            let gone: Vec<DesiredKey> = st
                .applied
                .keys()
                .filter(|k| !st.desired.contains(k))
                .cloned()
                .collect();
            let remove: Vec<Applied> = gone.iter().filter_map(|k| st.applied.remove(k)).collect();
            let State {
                desired, last_tick, ..
            } = &mut *st;
            last_tick.retain(|k, _| desired.holds(k));
            (add, remove)
        };
        self.release(remove).await;
        let mut subs = Vec::with_capacity(add.len());
        let mut keys = Vec::with_capacity(add.len());
        for k in add {
            match self.applied_for(&k) {
                Some(s) => {
                    subs.push(s);
                    keys.push(k);
                }
                None => tracing::warn!(
                    "{} on {} is not in the symbol master; not streaming it",
                    k.0.symbol,
                    k.0.exchange
                ),
            }
        }
        if subs.is_empty() {
            return;
        }
        let mut main = Vec::new();
        let mut deep = Vec::new();
        for a in &subs {
            match a {
                Applied::Main(s) => main.push(s.clone()),
                Applied::Deep { depth, quote } => {
                    deep.push(depth.clone());
                    main.push(quote.clone());
                }
            }
        }
        let mut result = self.manager.subscribe(main.clone()).await;
        if result.is_ok() && !deep.is_empty() {
            result = self.depth_manager.subscribe(deep).await;
            if result.is_err() {
                // Do not keep the quote references of a refused deep book.
                let _ = self.manager.unsubscribe(main).await;
            }
        }
        match result {
            Ok(()) => {
                let mut st = self.state.lock();
                for (k, s) in keys.into_iter().zip(subs) {
                    st.applied.insert(k, s);
                }
                // A key dropped meanwhile is released on the next pass.
                drop(st);
                self.changed.notify_one();
            }
            Err(e) => tracing::warn!("Market data subscribe failed: {}", e),
        }
    }
}
