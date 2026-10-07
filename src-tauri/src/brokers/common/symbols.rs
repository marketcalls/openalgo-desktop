//! The in-memory symbol master and the OpenAlgo <-> broker symbol resolver.
//!
//! `SymToken` mirrors the web's `symtoken` row column for column. The
//! resolver holds one immutable *generation* of the master: the rows plus
//! four indexes (OpenAlgo symbol, token, broker symbol, underlying). A reload
//! builds a complete new generation and swaps it in under a write lock, so
//! memory is bounded by one master (plus the outgoing one while readers
//! still hold it) and never accumulates across reloads.
//!
//! Lookups are keyed on the OpenAlgo exchange, like the web's
//! `get_br_symbol(symbol, exchange)` / `get_oa_symbol(brsymbol, exchange)`.

use parking_lot::RwLock;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;

/// One master-contract row (web `SymToken`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SymToken {
    /// OpenAlgo symbol, e.g. `NIFTY28MAR2420800CE`.
    pub symbol: String,
    /// Broker trading symbol, e.g. `NIFTY24MAR20800CE`.
    pub brsymbol: String,
    /// Underlying / instrument name, e.g. `NIFTY`.
    pub name: String,
    /// OpenAlgo exchange, e.g. `NFO`, `NSE_INDEX`.
    pub exchange: String,
    /// Broker exchange / segment code.
    pub brexchange: String,
    /// Broker token (format is broker-specific; Kite uses
    /// `instrument_token::::exchange_token`).
    pub token: String,
    /// `DD-MMM-YY` uppercase, empty when the instrument does not expire.
    pub expiry: String,
    /// Strike price; 0 (or the broker's own sentinel) for non-options.
    pub strike: f64,
    /// Lot size in OpenAlgo units.
    #[serde(rename = "lotsize")]
    pub lot_size: i32,
    /// `EQ`, `FUT`, `CE`, `PE` (broker-specific extras pass through).
    #[serde(rename = "instrumenttype")]
    pub instrument_type: String,
    pub tick_size: f64,
}

impl SymToken {
    /// The broker symbol, falling back to the OpenAlgo symbol when a broker
    /// did not supply one.
    pub fn br_symbol(&self) -> &str {
        if self.brsymbol.is_empty() {
            &self.symbol
        } else {
            &self.brsymbol
        }
    }

    /// The broker exchange, falling back to the OpenAlgo exchange.
    pub fn br_exchange(&self) -> &str {
        if self.brexchange.is_empty() {
            &self.exchange
        } else {
            &self.brexchange
        }
    }
}

/// Filters for contract search (option chains, expiry pickers).
#[derive(Debug, Clone, Default)]
pub struct ContractQuery<'a> {
    pub exchange: &'a str,
    /// Underlying (`name` column), exact match.
    pub underlying: &'a str,
    /// `DD-MMM-YY`; `None` matches every expiry.
    pub expiry: Option<&'a str>,
    /// Exact strike; `None` matches every strike.
    pub strike: Option<f64>,
    /// `FUT`, `CE`, `PE`, ...; `None` matches all.
    pub instrument_type: Option<&'a str>,
}

/// One immutable snapshot of the master.
#[derive(Debug, Default)]
pub struct SymbolGeneration {
    rows: Vec<SymToken>,
    by_symbol: HashMap<String, u32>,
    by_token: HashMap<String, u32>,
    by_brsymbol: HashMap<String, u32>,
    by_underlying: HashMap<String, Vec<u32>>,
    /// Row ids ordered by (symbol, exchange) for prefix search.
    sorted: Vec<u32>,
    /// Contract multiplier by `exchange:token` (web `contract_value`), only
    /// for venues that quote one (crypto). Empty for Indian masters.
    contract_values: HashMap<String, f64>,
    id: u64,
}

fn key(exchange: &str, value: &str) -> String {
    let mut k = String::with_capacity(exchange.len() + value.len() + 1);
    k.push_str(exchange);
    k.push(':');
    k.push_str(value);
    k
}

impl SymbolGeneration {
    /// Build a generation. Rows whose `(exchange, token)` was already seen
    /// are dropped (the web's `copy_from_dataframe` skips existing tokens);
    /// for duplicate `(exchange, symbol)` pairs the first row wins.
    pub fn build(rows: Vec<SymToken>, id: u64) -> Self {
        let mut kept: Vec<SymToken> = Vec::with_capacity(rows.len());
        let mut by_token: HashMap<String, u32> = HashMap::with_capacity(rows.len());
        for row in rows {
            let k = key(&row.exchange, &row.token);
            if by_token.contains_key(&k) {
                continue;
            }
            by_token.insert(k, kept.len() as u32);
            kept.push(row);
        }
        let mut by_symbol = HashMap::with_capacity(kept.len());
        let mut by_brsymbol = HashMap::with_capacity(kept.len());
        let mut by_underlying: HashMap<String, Vec<u32>> = HashMap::new();
        for (i, row) in kept.iter().enumerate() {
            let i = i as u32;
            by_symbol
                .entry(key(&row.exchange, &row.symbol))
                .or_insert(i);
            by_brsymbol
                .entry(key(&row.exchange, row.br_symbol()))
                .or_insert(i);
            if !row.name.is_empty() {
                by_underlying
                    .entry(key(&row.exchange, &row.name))
                    .or_default()
                    .push(i);
            }
        }
        let mut sorted: Vec<u32> = (0..kept.len() as u32).collect();
        sorted.sort_by(|a, b| {
            let (ra, rb) = (&kept[*a as usize], &kept[*b as usize]);
            ra.symbol
                .cmp(&rb.symbol)
                .then_with(|| ra.exchange.cmp(&rb.exchange))
        });
        by_underlying.shrink_to_fit();
        Self {
            rows: kept,
            by_symbol,
            by_token,
            by_brsymbol,
            by_underlying,
            sorted,
            contract_values: HashMap::new(),
            id,
        }
    }

    /// Build a generation with contract multipliers keyed by token. Values
    /// for tokens that are not in the master, and non-positive values, are
    /// dropped.
    pub fn build_with_contract_values(
        rows: Vec<SymToken>,
        contract_values: &HashMap<String, f64>,
        id: u64,
    ) -> Self {
        let mut g = Self::build(rows, id);
        if !contract_values.is_empty() {
            let mut cv = HashMap::new();
            for row in &g.rows {
                if let Some(v) = contract_values.get(&row.token) {
                    if v.is_finite() && *v > 0.0 {
                        cv.insert(key(&row.exchange, &row.token), *v);
                    }
                }
            }
            g.contract_values = cv;
        }
        g
    }

    /// Contract multiplier of the row `(exchange, token)`, when the venue
    /// quotes one.
    pub fn contract_value(&self, exchange: &str, token: &str) -> Option<f64> {
        self.contract_values.get(&key(exchange, token)).copied()
    }

    /// Every contract multiplier, keyed by token (for persisting the master).
    pub fn contract_values_by_token(&self) -> HashMap<String, f64> {
        self.contract_values
            .iter()
            .filter_map(|(k, v)| k.split_once(':').map(|(_, t)| (t.to_string(), *v)))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Monotonic generation number (0 = never loaded).
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn rows(&self) -> &[SymToken] {
        &self.rows
    }

    fn get(&self, map: &HashMap<String, u32>, exchange: &str, value: &str) -> Option<&SymToken> {
        map.get(&key(exchange, value))
            .and_then(|i| self.rows.get(*i as usize))
    }

    pub fn by_symbol(&self, exchange: &str, symbol: &str) -> Option<&SymToken> {
        self.get(&self.by_symbol, exchange, symbol)
    }

    pub fn by_token(&self, exchange: &str, token: &str) -> Option<&SymToken> {
        self.get(&self.by_token, exchange, token)
    }

    pub fn by_brsymbol(&self, exchange: &str, brsymbol: &str) -> Option<&SymToken> {
        self.get(&self.by_brsymbol, exchange, brsymbol)
    }

    /// Symbols starting with `prefix` (case-insensitive), optionally on one
    /// exchange, at most `limit`.
    pub fn search_prefix(
        &self,
        prefix: &str,
        exchange: Option<&str>,
        limit: usize,
    ) -> Vec<&SymToken> {
        let p = prefix.to_ascii_uppercase();
        let start = self
            .sorted
            .partition_point(|i| self.rows[*i as usize].symbol.as_str() < p.as_str());
        self.sorted[start..]
            .iter()
            .map(|i| &self.rows[*i as usize])
            .take_while(|r| r.symbol.starts_with(&p))
            .filter(|r| exchange.is_none_or(|e| r.exchange == e))
            .take(limit)
            .collect()
    }

    /// Contracts on one underlying, filtered by expiry, strike and type.
    pub fn contracts(&self, q: &ContractQuery<'_>) -> Vec<&SymToken> {
        let Some(ids) = self.by_underlying.get(&key(q.exchange, q.underlying)) else {
            return Vec::new();
        };
        ids.iter()
            .map(|i| &self.rows[*i as usize])
            .filter(|r| q.expiry.is_none_or(|e| r.expiry == e))
            .filter(|r| q.strike.is_none_or(|s| (r.strike - s).abs() < 1e-9))
            .filter(|r| q.instrument_type.is_none_or(|t| r.instrument_type == t))
            .collect()
    }

    /// Distinct expiries of an underlying, earliest first.
    pub fn expiries(
        &self,
        exchange: &str,
        underlying: &str,
        instrument_type: Option<&str>,
    ) -> Vec<String> {
        let q = ContractQuery {
            exchange,
            underlying,
            instrument_type,
            ..Default::default()
        };
        let mut v: Vec<(chrono::NaiveDate, String)> = self
            .contracts(&q)
            .into_iter()
            .filter(|r| !r.expiry.is_empty())
            .filter_map(|r| {
                super::master_contract::parse_oa_expiry(&r.expiry).map(|d| (d, r.expiry.clone()))
            })
            .collect();
        v.sort();
        v.dedup();
        v.into_iter().map(|(_, e)| e).collect()
    }

    /// Distinct strikes of an underlying for one expiry, ascending.
    pub fn strikes(&self, exchange: &str, underlying: &str, expiry: &str) -> Vec<f64> {
        let q = ContractQuery {
            exchange,
            underlying,
            expiry: Some(expiry),
            ..Default::default()
        };
        let mut v: Vec<f64> = self
            .contracts(&q)
            .into_iter()
            .filter(|r| r.instrument_type == "CE" || r.instrument_type == "PE")
            .map(|r| r.strike)
            .collect();
        v.sort_by(|a, b| a.total_cmp(b));
        v.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        v
    }
}

/// Shared handle to the current symbol-master generation. Cloning shares the
/// same master (the broker registry and the app context hold clones).
#[derive(Clone, Default)]
pub struct SymbolResolver {
    current: Arc<RwLock<Arc<SymbolGeneration>>>,
}

impl std::fmt::Debug for SymbolResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let g = self.snapshot();
        f.debug_struct("SymbolResolver")
            .field("generation", &g.id)
            .field("rows", &g.len())
            .finish()
    }
}

impl SymbolResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the whole master with `rows`. The previous generation is
    /// freed as soon as the last reader drops its snapshot.
    pub fn load(&self, rows: Vec<SymToken>) -> usize {
        let next_id = self.current.read().id + 1;
        let generation = Arc::new(SymbolGeneration::build(rows, next_id));
        let n = generation.len();
        *self.current.write() = generation;
        tracing::info!("Symbol master loaded: {} instruments", n);
        n
    }

    /// Replace the whole master with a download that carries contract
    /// multipliers (crypto). Same swap semantics as `load`.
    pub fn load_master(&self, master: crate::brokers::types::MasterContract) -> usize {
        let next_id = self.current.read().id + 1;
        let generation = Arc::new(SymbolGeneration::build_with_contract_values(
            master.rows,
            &master.contract_values,
            next_id,
        ));
        let n = generation.len();
        *self.current.write() = generation;
        tracing::info!("Symbol master loaded: {} instruments", n);
        n
    }

    /// Contract multiplier of an OpenAlgo symbol (web
    /// `SymToken.contract_value`); `None` when the venue quotes none.
    pub fn contract_value(&self, symbol: &str, exchange: &str) -> Option<f64> {
        let g = self.snapshot();
        let row = g.by_symbol(exchange, symbol)?;
        g.contract_value(exchange, &row.token)
    }

    /// Drop the master (broker logout).
    pub fn clear(&self) {
        let next_id = self.current.read().id + 1;
        *self.current.write() = Arc::new(SymbolGeneration {
            id: next_id,
            ..Default::default()
        });
    }

    /// The current generation, for multi-step reads that must see one
    /// consistent master.
    pub fn snapshot(&self) -> Arc<SymbolGeneration> {
        self.current.read().clone()
    }

    pub fn len(&self) -> usize {
        self.snapshot().len()
    }

    pub fn is_empty(&self) -> bool {
        self.snapshot().is_empty()
    }

    pub fn by_symbol(&self, exchange: &str, symbol: &str) -> Option<SymToken> {
        self.snapshot().by_symbol(exchange, symbol).cloned()
    }

    pub fn by_token(&self, exchange: &str, token: &str) -> Option<SymToken> {
        self.snapshot().by_token(exchange, token).cloned()
    }

    pub fn by_brsymbol(&self, exchange: &str, brsymbol: &str) -> Option<SymToken> {
        self.snapshot().by_brsymbol(exchange, brsymbol).cloned()
    }

    /// web `get_br_symbol(symbol, exchange)`.
    pub fn br_symbol(&self, symbol: &str, exchange: &str) -> Option<String> {
        self.snapshot()
            .by_symbol(exchange, symbol)
            .map(|r| r.br_symbol().to_string())
    }

    /// web `get_oa_symbol(brsymbol, exchange)`.
    pub fn oa_symbol(&self, brsymbol: &str, exchange: &str) -> Option<String> {
        self.snapshot()
            .by_brsymbol(exchange, brsymbol)
            .map(|r| r.symbol.clone())
    }

    /// OpenAlgo symbol for a broker symbol, or the broker symbol itself when
    /// the master has no row (the web logs and keeps the original).
    pub fn oa_symbol_or_raw(&self, brsymbol: &str, exchange: &str) -> String {
        match self.oa_symbol(brsymbol, exchange) {
            Some(s) => s,
            None => {
                tracing::debug!("No OpenAlgo symbol for {}:{}", exchange, brsymbol);
                brsymbol.to_string()
            }
        }
    }

    /// web `get_token(symbol, exchange)`.
    pub fn token(&self, symbol: &str, exchange: &str) -> Option<String> {
        self.snapshot()
            .by_symbol(exchange, symbol)
            .map(|r| r.token.clone())
    }

    /// web `get_brexchange(symbol, exchange)`.
    pub fn brexchange(&self, symbol: &str, exchange: &str) -> Option<String> {
        self.snapshot()
            .by_symbol(exchange, symbol)
            .map(|r| r.br_exchange().to_string())
    }

    pub fn search_prefix(
        &self,
        prefix: &str,
        exchange: Option<&str>,
        limit: usize,
    ) -> Vec<SymToken> {
        self.snapshot()
            .search_prefix(prefix, exchange, limit)
            .into_iter()
            .cloned()
            .collect()
    }

    pub fn contracts(&self, q: &ContractQuery<'_>) -> Vec<SymToken> {
        self.snapshot().contracts(q).into_iter().cloned().collect()
    }

    pub fn expiries(
        &self,
        exchange: &str,
        underlying: &str,
        instrument_type: Option<&str>,
    ) -> Vec<String> {
        self.snapshot()
            .expiries(exchange, underlying, instrument_type)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn row(symbol: &str, brsymbol: &str, exchange: &str, token: &str) -> SymToken {
        SymToken {
            symbol: symbol.into(),
            brsymbol: brsymbol.into(),
            name: symbol.into(),
            exchange: exchange.into(),
            brexchange: exchange.into(),
            token: token.into(),
            expiry: String::new(),
            strike: 0.0,
            lot_size: 1,
            instrument_type: "EQ".into(),
            tick_size: 0.05,
        }
    }

    fn opt(sym: &str, br: &str, token: &str, expiry: &str, strike: f64, t: &str) -> SymToken {
        SymToken {
            name: "NIFTY".into(),
            expiry: expiry.into(),
            strike,
            instrument_type: t.into(),
            lot_size: 75,
            ..row(sym, br, "NFO", token)
        }
    }

    fn sample() -> SymbolResolver {
        let r = SymbolResolver::new();
        r.load(vec![
            row("SBIN", "SBIN-EQ", "NSE", "3045"),
            row("SBIN", "SBIN", "BSE", "500112"),
            row("NIFTY", "NIFTY 50", "NSE_INDEX", "256265"),
            opt(
                "NIFTY27OCT2625000CE",
                "NIFTY26OCT25000CE",
                "1",
                "27-OCT-26",
                25000.0,
                "CE",
            ),
            opt(
                "NIFTY27OCT2625000PE",
                "NIFTY26OCT25000PE",
                "2",
                "27-OCT-26",
                25000.0,
                "PE",
            ),
            opt(
                "NIFTY03NOV2625100CE",
                "NIFTY26N0325100CE",
                "3",
                "03-NOV-26",
                25100.0,
                "CE",
            ),
            opt(
                "NIFTY27OCT26FUT",
                "NIFTY26OCTFUT",
                "4",
                "27-OCT-26",
                0.0,
                "FUT",
            ),
        ]);
        r
    }

    #[test]
    fn both_directions_and_tokens() {
        let r = sample();
        assert_eq!(r.br_symbol("SBIN", "NSE").as_deref(), Some("SBIN-EQ"));
        assert_eq!(r.oa_symbol("SBIN-EQ", "NSE").as_deref(), Some("SBIN"));
        assert_eq!(
            r.oa_symbol("NIFTY 50", "NSE_INDEX").as_deref(),
            Some("NIFTY")
        );
        assert_eq!(r.token("SBIN", "BSE").as_deref(), Some("500112"));
        assert_eq!(r.by_token("NSE", "3045").unwrap().symbol, "SBIN");
        assert_eq!(r.oa_symbol_or_raw("UNKNOWN-EQ", "NSE"), "UNKNOWN-EQ");
        assert!(r.oa_symbol("SBIN-EQ", "BSE").is_none());
    }

    #[test]
    fn prefix_and_contract_search() {
        let r = sample();
        let hits: Vec<String> = r
            .search_prefix("nifty27", Some("NFO"), 10)
            .into_iter()
            .map(|s| s.symbol)
            .collect();
        assert_eq!(
            hits,
            [
                "NIFTY27OCT2625000CE",
                "NIFTY27OCT2625000PE",
                "NIFTY27OCT26FUT"
            ]
        );
        assert_eq!(r.search_prefix("SB", None, 1).len(), 1);
        let ce = r.contracts(&ContractQuery {
            exchange: "NFO",
            underlying: "NIFTY",
            expiry: Some("27-OCT-26"),
            strike: Some(25000.0),
            instrument_type: Some("CE"),
        });
        assert_eq!(ce.len(), 1);
        assert_eq!(ce[0].brsymbol, "NIFTY26OCT25000CE");
        assert_eq!(r.expiries("NFO", "NIFTY", None), ["27-OCT-26", "03-NOV-26"]);
        assert_eq!(r.snapshot().strikes("NFO", "NIFTY", "27-OCT-26"), [25000.0]);
    }

    #[test]
    fn duplicate_tokens_are_skipped_first_wins() {
        let r = SymbolResolver::new();
        r.load(vec![
            row("A", "A", "NSE", "1"),
            row("B", "B", "NSE", "1"),
            row("A", "A2", "NSE", "2"),
        ]);
        assert_eq!(r.len(), 2);
        assert_eq!(r.by_symbol("NSE", "A").unwrap().token, "1");
        assert_eq!(r.by_token("NSE", "2").unwrap().brsymbol, "A2");
    }

    #[test]
    fn reload_replaces_the_whole_generation() {
        let r = sample();
        let old = r.snapshot();
        let n = r.load(vec![row("TCS", "TCS-EQ", "NSE", "11536")]);
        assert_eq!(n, 1);
        assert_eq!(r.len(), 1);
        assert!(r.by_symbol("NSE", "SBIN").is_none());
        assert!(r.snapshot().id() > old.id());
        // The old snapshot is still readable by whoever holds it ...
        assert_eq!(old.len(), 7);
        drop(old);
        // ... and is the only other owner, so it is freed when dropped.
        assert_eq!(Arc::strong_count(&r.snapshot()), 2);
        r.clear();
        assert!(r.is_empty());
    }

    #[test]
    fn reload_many_times_stays_bounded() {
        let r = SymbolResolver::new();
        for g in 0..200u32 {
            let rows = (0..100)
                .map(|i| {
                    row(
                        &format!("S{}", i),
                        &format!("S{}-EQ", i),
                        "NSE",
                        &format!("{}", g * 1000 + i),
                    )
                })
                .collect();
            r.load(rows);
        }
        let s = r.snapshot();
        assert_eq!(s.len(), 100);
        assert_eq!(s.by_symbol.len(), 100);
        assert_eq!(s.by_token.len(), 100);
        assert_eq!(s.by_brsymbol.len(), 100);
        assert_eq!(s.sorted.len(), 100);
    }

    #[test]
    fn clones_share_one_master() {
        let a = SymbolResolver::new();
        let b = a.clone();
        a.load(vec![row("SBIN", "SBIN-EQ", "NSE", "3045")]);
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn contract_values_follow_the_master_generation() {
        let r = SymbolResolver::new();
        let mut cv = HashMap::new();
        cv.insert("27".to_string(), 0.001);
        cv.insert("999".to_string(), 5.0); // not in the master: dropped
        cv.insert("3136".to_string(), 0.0); // not positive: dropped
        r.load_master(crate::brokers::types::MasterContract {
            rows: vec![
                row("BTCUSDFUT", "BTCUSD", "CRYPTO", "27"),
                row("ETHUSDFUT", "ETHUSD", "CRYPTO", "3136"),
            ],
            contract_values: cv,
        });
        assert_eq!(r.contract_value("BTCUSDFUT", "CRYPTO"), Some(0.001));
        assert_eq!(r.contract_value("ETHUSDFUT", "CRYPTO"), None);
        assert_eq!(r.contract_value("NOPE", "CRYPTO"), None);
        let by_token = r.snapshot().contract_values_by_token();
        assert_eq!(by_token.len(), 1);
        assert_eq!(by_token.get("27"), Some(&0.001));
        // A plain reload (an Indian broker) carries none.
        r.load(vec![row("SBIN", "SBIN-EQ", "NSE", "3045")]);
        assert!(r.snapshot().contract_values_by_token().is_empty());
    }
}
