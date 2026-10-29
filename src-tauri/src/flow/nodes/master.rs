//! The symbol master as Flow's nodes read it ([`SymbolMaster`] over the
//! app's loaded master contract): the lot-size lookups of the options
//! nodes (the web's `SymToken` queries), from one consistent snapshot per
//! query, never SQL in a node.

use crate::brokers::common::symbols::SymToken;
use crate::flow::contract::env::{SymbolMaster, SymbolQuery};
use crate::state::AppState;
use std::sync::{Arc, Weak};

/// What a lookup answers once the app is closing.
const CLOSING: &str = "OpenAlgo is shutting down.";

/// The app's master contract.
pub struct SymbolStoreMaster {
    app: Weak<AppState>,
}

impl SymbolStoreMaster {
    pub fn new(app: Weak<AppState>) -> Self {
        Self { app }
    }

    /// Install it for every later run.
    pub fn install(app: &Arc<AppState>) {
        let master = Arc::new(Self::new(Arc::downgrade(app)));
        app.flow.install_env(|e| e.symbols = master);
    }
}

/// Whether `row` passes every filter of `q` (the prefix aside).
fn matches(row: &SymToken, q: &SymbolQuery) -> bool {
    let same = |want: &Option<String>, have: &str| want.as_deref().is_none_or(|w| w == have);
    same(&q.symbol, &row.symbol)
        && same(&q.name, &row.name)
        && same(&q.exchange, &row.exchange)
        && same(&q.instrumenttype, &row.instrument_type)
        && q.symbol_prefix
            .as_deref()
            .is_none_or(|p| row.symbol.starts_with(p))
}

impl SymbolMaster for SymbolStoreMaster {
    fn rows(&self, q: &SymbolQuery) -> Result<Vec<SymToken>, String> {
        let app = self.app.upgrade().ok_or_else(|| CLOSING.to_string())?;
        let snap = app.symbols.snapshot();
        let limit = q.limit.unwrap_or(usize::MAX);
        // The prefix seeks the sorted symbol index (the web's half-open
        // range); an empty prefix is every row.
        let candidates: Box<dyn Iterator<Item = &SymToken>> = match q.symbol_prefix.as_deref() {
            Some(p) if !p.is_empty() => Box::new(
                snap.search_prefix(p, q.exchange.as_deref(), usize::MAX)
                    .into_iter(),
            ),
            _ => Box::new(snap.rows().iter()),
        };
        Ok(candidates
            .filter(|r| matches(r, q))
            .take(limit)
            .cloned()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tools::testkit::row;

    fn rows() -> Vec<SymToken> {
        let mut nifty = row("NIFTY25NOV2624000CE", "NFO", "CE", "25-NOV-26", 24000.0, 75);
        nifty.name = "NIFTY 25 Nov 26 24000 CE".into();
        vec![
            row(
                "NIFTYNXT5025NOV2668000CE",
                "NFO",
                "CE",
                "25-NOV-26",
                68000.0,
                25,
            ),
            nifty,
            row("SBIN", "NSE", "EQ", "", 0.0, 1),
        ]
    }

    /// parity N-common-21: the prefix query is the web's half-open range
    /// (case-sensitive, never matching a shorter symbol), and an empty
    /// prefix does not panic.
    #[test]
    fn a_query_reads_the_loaded_master() {
        let t =
            crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now());
        t.ctx.symbols.load(rows());
        let m = SymbolStoreMaster::new(Arc::downgrade(&t.ctx));
        let q = |prefix: Option<&str>, exchange: Option<&str>, limit: Option<usize>| SymbolQuery {
            symbol_prefix: prefix.map(str::to_string),
            exchange: exchange.map(str::to_string),
            limit,
            ..SymbolQuery::default()
        };
        let names = |r: Vec<SymToken>| r.into_iter().map(|t| t.symbol).collect::<Vec<_>>();
        let nifty = m.rows(&q(Some("NIFTY"), Some("NFO"), None));
        assert_eq!(
            names(nifty.unwrap_or_default()),
            ["NIFTY25NOV2624000CE", "NIFTYNXT5025NOV2668000CE"]
        );
        assert_eq!(
            m.rows(&q(Some("nifty"), None, None)).map(|r| r.len()),
            Ok(0)
        );
        assert_eq!(m.rows(&q(Some(""), None, None)).map(|r| r.len()), Ok(3));
        assert_eq!(
            m.rows(&q(None, Some("NFO"), Some(1))).map(|r| r.len()),
            Ok(1)
        );
        let by_name = SymbolQuery {
            name: Some("NIFTY 25 Nov 26 24000 CE".into()),
            ..SymbolQuery::default()
        };
        assert_eq!(m.rows(&by_name).map(|r| r[0].lot_size), Ok(75));
        drop(t);
        assert_eq!(
            m.rows(&SymbolQuery::default()).err().as_deref(),
            Some(CLOSING)
        );
    }
}
