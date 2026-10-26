//! Options analytics (web `option_symbol_service.py`,
//! `option_chain_service.py`, `synthetic_future_service.py`,
//! `option_greeks_service.py`). Responses are flat (no `data` wrapper).

use super::core::{broker_handle, float, num, BrokerHandle, Reply};
use super::market_data_service::fetch_quote;
use crate::analytics::black76::{self as bs, Flag};
use crate::brokers::common::master_contract::{format_strike, parse_oa_expiry};
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::{Quote, QuoteKey};
use crate::state::AppState;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;
use serde_json::{json, Map, Value};

mod chain_answer;
mod greeks;
pub use chain_answer::{option_chain_answer, ChainAnswer};
pub use greeks::*;

const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];

pub fn today_ist(ctx: &AppState) -> NaiveDate {
    ctx.now().with_timezone(&Kolkata).date_naive()
}

/// `NIFTY27OCT26FUT` -> (`NIFTY`, Some(`27OCT26`)); anything else is a bare
/// underlying.
pub fn parse_underlying(underlying: &str) -> (String, Option<String>) {
    let up = underlying.to_ascii_uppercase();
    let alpha: String = up.chars().take_while(|c| c.is_ascii_uppercase()).collect();
    let rest = &up[alpha.len()..];
    let b = rest.as_bytes();
    let dated = b.len() >= 7
        && b[0..2].iter().all(u8::is_ascii_digit)
        && b[2..5].iter().all(u8::is_ascii_uppercase)
        && b[5..7].iter().all(u8::is_ascii_digit)
        && (b.len() == 7 || &rest[7..] == "FUT");
    if !alpha.is_empty() && dated {
        (alpha, Some(rest[..7].to_string()))
    } else {
        (up, None)
    }
}

/// `DDMMMYY` -> date (month in any case): exactly seven ASCII bytes, two
/// digits, a month name and two digits, naming a real calendar day.
/// Anything else, multibyte text included, is `None`; never a panic.
pub fn parse_compact_expiry(e: &str) -> Option<NaiveDate> {
    let b = e.as_bytes();
    if b.len() != 7
        || !b[..2].iter().all(u8::is_ascii_digit)
        || !b[2..5].iter().all(u8::is_ascii_alphabetic)
        || !b[5..].iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    // All seven bytes are ASCII, so these slices fall on char boundaries.
    let day: u32 = e[0..2].parse().ok()?;
    let mon = MONTHS
        .iter()
        .position(|m| m.eq_ignore_ascii_case(&e[2..5]))? as u32
        + 1;
    let yy: i32 = e[5..7].parse().ok()?;
    NaiveDate::from_ymd_opt(2000 + yy, mon, day)
}

/// An expiry a caller sent: upper-case `DDMMMYY` that names a real day.
/// Every API, page and MCP boundary that takes an expiry reads it through
/// [`Expiry::parse`] before any master or broker lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expiry {
    compact: String,
    date: NaiveDate,
}

impl Expiry {
    /// Trimmed and upper-cased; `None` unless it is a valid `DDMMMYY`.
    pub fn parse(raw: &str) -> Option<Self> {
        let compact = raw.trim().to_ascii_uppercase();
        let date = parse_compact_expiry(&compact)?;
        Some(Self { compact, date })
    }

    /// `DDMMMYY`, upper case.
    pub fn compact(&self) -> &str {
        &self.compact
    }

    /// The master's `DD-MMM-YY`.
    pub fn master(&self) -> String {
        format!(
            "{}-{}-{}",
            &self.compact[..2],
            &self.compact[2..5],
            &self.compact[5..]
        )
    }

    pub fn date(&self) -> NaiveDate {
        self.date
    }
}

/// `DDMMMYY` -> the master's `DD-MMM-YY` (web: by position). Text that is
/// not seven ASCII characters comes back upper-cased and unchanged, so it
/// matches no contract instead of splitting a character.
pub fn master_expiry(expiry: &str) -> String {
    match Expiry::parse(expiry) {
        Some(e) => e.master(),
        None => expiry.to_ascii_uppercase(),
    }
}

/// Expiry cut-off time (IST) per exchange.
pub fn cutoff(exchange: &str) -> NaiveTime {
    let (h, m) = match exchange.to_ascii_uppercase().as_str() {
        "MCX" => (23, 30),
        "CDS" => (12, 30),
        _ => (15, 30),
    };
    NaiveTime::from_hms_opt(h, m, 0).unwrap_or_default()
}

/// The time on its expiry day until which a contract is still listed live
/// in the option tools: the venue's expiry cut-off (IST). Delta Exchange
/// settles crypto contracts at 12:00 UTC (17:30 IST, the product master's
/// `settlement_time`); BSE currency options expire at 12:30 like CDS.
pub fn listing_cutoff(exchange: &str) -> NaiveTime {
    match exchange.to_ascii_uppercase().as_str() {
        "CRYPTO" => NaiveTime::from_hms_opt(17, 30, 0).unwrap_or_default(),
        "BCD" => cutoff("CDS"),
        other => cutoff(other),
    }
}

/// Whether a master expiry (`DD-MMM-YY`) is still live at `now`: before the
/// venue's cut-off on its expiry day, so a contract expiring today stays
/// listed until then. An unreadable date is never live.
pub fn listed_live(expiry: &str, exchange: &str, now: DateTime<Utc>) -> bool {
    let Some(d) = parse_oa_expiry(expiry) else {
        return false;
    };
    Kolkata
        .from_local_datetime(&NaiveDateTime::new(d, listing_cutoff(exchange)))
        .single()
        .is_some_and(|t| t.with_timezone(&Utc) > now)
}

/// Expiry instant: the date at the exchange cut-off, IST.
pub fn expiry_datetime(
    expiry: &str,
    exchange: &str,
    time: Option<NaiveTime>,
) -> Option<DateTime<Utc>> {
    let d = parse_compact_expiry(expiry)?;
    let naive = NaiveDateTime::new(d, time.unwrap_or_else(|| cutoff(exchange)));
    Kolkata
        .from_local_datetime(&naive)
        .single()
        .map(|t| t.with_timezone(&Utc))
}

/// `(days, years)` to expiry; `None` once expired.
pub fn time_to_expiry(expiry: DateTime<Utc>, now: DateTime<Utc>) -> Option<(f64, f64)> {
    if expiry < now {
        return None;
    }
    let secs = (expiry - now).num_microseconds().unwrap_or(0) as f64 / 1e6;
    let days = secs / 86400.0;
    let years = days / 365.0;
    if years < 0.0001 {
        Some((0.0365, 0.0001))
    } else {
        Some((days, years))
    }
}

const NSE_INDICES: &[&str] = &[
    "NIFTY",
    "BANKNIFTY",
    "FINNIFTY",
    "MIDCPNIFTY",
    "NIFTYNXT50",
    "INDIAVIX",
];
const BSE_INDICES: &[&str] = &["SENSEX", "BANKEX", "SENSEX50"];
const NO_SPOT: &[&str] = &["MCX", "CDS", "BCD", "NCDEX", "NCO"];

/// Where an underlying is quoted.
pub fn quote_exchange(base: &str, exchange: &str) -> String {
    let ex = exchange.to_ascii_uppercase();
    if ex == "NFO" || ex == "BFO" {
        if NSE_INDICES.contains(&base) {
            return "NSE_INDEX".into();
        }
        if BSE_INDICES.contains(&base) {
            return "BSE_INDEX".into();
        }
        return if ex == "NFO" {
            "NSE".into()
        } else {
            "BSE".into()
        };
    }
    exchange.to_string()
}

/// Web `get_option_exchange`.
pub fn option_exchange(quote_exchange: &str) -> String {
    match quote_exchange.to_ascii_uppercase().as_str() {
        "NSE" | "NSE_INDEX" => "NFO".into(),
        "BSE" | "BSE_INDEX" => "BFO".into(),
        "MCX" => "MCX".into(),
        "CDS" => "CDS".into(),
        "NCO" => "NCO".into(),
        "BCD" => "BCD".into(),
        "NCDEX" => "NCDEX".into(),
        "CRYPTO" => "CRYPTO".into(),
        _ => "NFO".into(),
    }
}

/// The near-month unexpired future of `base` on a no-spot exchange.
fn near_future(rows: &[SymToken], base: &str, exchange: &str, today: NaiveDate) -> Option<String> {
    rows.iter()
        .filter(|r| r.exchange == exchange && r.instrument_type == "FUT" && !r.expiry.is_empty())
        .filter(|r| {
            r.symbol
                .strip_prefix(base)
                .and_then(|rest| rest.strip_suffix("FUT"))
                .map(|d| d.len() == 7 && parse_compact_expiry(d).is_some())
                .unwrap_or(false)
        })
        .filter_map(|r| parse_oa_expiry(&r.expiry).map(|d| (d, r.symbol.clone())))
        .filter(|(d, _)| *d >= today)
        .min()
        .map(|(_, s)| s)
}

/// The near-month unexpired future of `base` on `exchange` (web
/// `resolve_underlying_quote` for exchanges without a spot).
pub fn near_future_symbol(ctx: &AppState, base: &str, exchange: &str) -> Option<String> {
    let snap = ctx.symbols.snapshot();
    near_future(snap.rows(), base, exchange, today_ist(ctx))
}

/// `{base}{expiry}` options of one type on the options exchange, ascending
/// strikes (web `get_available_strikes`).
pub fn available_strikes(
    rows: &[SymToken],
    base: &str,
    expiry: &str,
    option_type: &str,
    exchange: &str,
) -> Vec<f64> {
    // The web converts DDMMMYY to the DB's DD-MMM-YY by position.
    let db_expiry = master_expiry(expiry);
    let prefix = format!("{}{}", base, expiry.to_ascii_uppercase());
    let mut v: Vec<f64> = rows
        .iter()
        .filter(|r| {
            r.exchange == exchange
                && r.instrument_type == option_type
                && r.expiry == db_expiry
                && r.symbol.starts_with(&prefix)
                && r.symbol.ends_with(option_type)
        })
        .map(|r| r.strike)
        .collect();
    v.sort_by(|a, b| a.total_cmp(b));
    v.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    v
}

/// Closest strike to `ltp` (ties to the lower strike).
pub fn atm_index(strikes: &[f64], ltp: f64) -> Option<usize> {
    if !ltp.is_finite() {
        return None;
    }
    let mut best: Option<(usize, f64)> = None;
    for (i, s) in strikes.iter().enumerate() {
        let d = (s - ltp).abs();
        if best.map(|(_, bd)| d < bd).unwrap_or(true) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

/// Offset target index (`None` = out of range); `Err` = Python's int() error.
fn offset_index(offset: &str, atm: usize, call: bool, len: usize) -> Result<Option<usize>, String> {
    let up = offset.to_ascii_uppercase();
    if up == "ATM" {
        return Ok(Some(atm));
    }
    let (kind, n) = if let Some(n) = up.strip_prefix("ITM") {
        ("ITM", n)
    } else if let Some(n) = up.strip_prefix("OTM") {
        ("OTM", n)
    } else {
        return Ok(None);
    };
    let n: i64 = n
        .trim()
        .parse()
        .map_err(|_| format!("invalid literal for int() with base 10: '{}'", &up[3..]))?;
    let up_side = (kind == "ITM") != call; // PE ITM and CE OTM move up
    let idx = if up_side {
        atm as i64 + n
    } else {
        atm as i64 - n
    };
    if idx < 0 || idx >= len as i64 {
        Ok(None)
    } else {
        Ok(Some(idx as usize))
    }
}

/// A resolved option contract.
#[derive(Debug, Clone)]
pub struct ResolvedOption {
    pub symbol: String,
    pub exchange: String,
    pub lotsize: i64,
    pub tick_size: f64,
    pub freeze_qty: i64,
    pub underlying_ltp: f64,
    pub strike: f64,
}

impl ResolvedOption {
    pub fn reply(&self) -> Reply {
        Reply::ok(json!({
            "status": "success",
            "symbol": self.symbol,
            "exchange": self.exchange,
            "lotsize": self.lotsize,
            "tick_size": float(self.tick_size),
            "freeze_qty": self.freeze_qty,
            "underlying_ltp": num(self.underlying_ltp),
        }))
    }
}

/// Web `get_available_strikes` came back empty (404).
fn no_strikes(base: &str, expiry: &str) -> Reply {
    Reply::error(
        404,
        format!(
            "No strikes found for {} expiring {}. Please check expiry date or update master contract.",
            base, expiry
        ),
    )
}

/// Where the underlying is quoted and the expiry to use.
struct Underlying {
    base: String,
    expiry: String,
    quote_symbol: String,
    quote_exchange: String,
}

fn resolve_underlying(
    ctx: &AppState,
    underlying: &str,
    exchange: &str,
    expiry_date: Option<&str>,
    embedded_wins: bool,
) -> Result<Underlying, Reply> {
    let (base, embedded) = parse_underlying(underlying);
    let expiry = if embedded_wins {
        embedded.clone().or_else(|| expiry_date.map(str::to_string))
    } else {
        expiry_date
            .filter(|e| !e.is_empty())
            .map(str::to_string)
            .or_else(|| embedded.clone())
    };
    let Some(expiry) = expiry.filter(|e| !e.is_empty()) else {
        return Err(Reply::error(
            400,
            if embedded_wins {
                "Expiry date is required.".to_string()
            } else {
                "Expiry date required. Provide via expiry_date parameter or embed in underlying (e.g., NIFTY28OCT25FUT).".to_string()
            },
        ));
    };
    // An expiry that names no real DDMMMYY day matches no contract. Answer
    // as the web does once its strike lookup comes back empty, but before
    // any master or broker lookup (the web fetches the LTP first).
    if parse_compact_expiry(&expiry).is_none() {
        return Err(no_strikes(&base, &expiry));
    }
    let qx = quote_exchange(&base, exchange);
    let ex_up = exchange.to_ascii_uppercase();
    let snap = ctx.symbols.snapshot();
    let quote_symbol = if ex_up == "CRYPTO" {
        let perp = format!("{}USDFUT", base);
        if snap.by_symbol(&ex_up, &perp).is_none() {
            return Err(Reply::error(
                404,
                format!("No perpetual futures found for {} on {}", base, exchange),
            ));
        }
        perp
    } else if embedded.is_some() {
        if !embedded_wins && (ex_up == "MCX" || ex_up == "CDS") {
            underlying.to_ascii_uppercase()
        } else {
            base.clone()
        }
    } else if NO_SPOT.contains(&ex_up.as_str()) {
        match near_future(snap.rows(), &base, &ex_up, today_ist(ctx)) {
            Some(s) => s,
            None if embedded_wins => {
                return Err(Reply::error(404, format!(
                    "No unexpired futures found for {} on {}. {} options are priced against the near-month future, which this product does not currently have.",
                    base, ex_up, ex_up
                )))
            }
            None => {
                return Err(Reply::error(404, format!(
                    "No unexpired futures contract for {} on {}, so the ATM reference price cannot be determined. Check the symbol, or re-download the master contract.",
                    base, ex_up
                )))
            }
        }
    } else {
        underlying.to_string()
    };
    Ok(Underlying {
        base,
        expiry,
        quote_symbol,
        quote_exchange: qx,
    })
}

async fn underlying_ltp(
    ctx: &AppState,
    h: &BrokerHandle,
    u: &Underlying,
    sep: &str,
) -> Result<Quote, Reply> {
    match fetch_quote(ctx, h, &u.quote_symbol, &u.quote_exchange).await {
        Ok(q) => {
            if !q.ltp.is_finite() {
                return Err(Reply::error(
                    500,
                    format!("Could not determine a usable LTP for {}.", u.quote_symbol),
                ));
            }
            Ok(q)
        }
        Err(r) => {
            let msg = r.message();
            Err(Reply::error(
                r.status,
                format!(
                    "Failed to fetch LTP for {}{} {}",
                    u.quote_symbol,
                    sep,
                    if msg.is_empty() {
                        "Unknown error".to_string()
                    } else {
                        msg
                    }
                ),
            ))
        }
    }
}

/// What [`resolve_option`] resolves: the web `get_option_symbol`
/// arguments.
#[derive(Debug, Clone, Copy, Default)]
pub struct OptionPick<'a> {
    pub underlying: &'a str,
    pub exchange: &'a str,
    pub expiry_date: Option<&'a str>,
    /// The strike interval; `None` picks among the listed strikes.
    pub strike_int: Option<f64>,
    /// `ATM`, `ITM<n>` or `OTM<n>`.
    pub offset: &'a str,
    /// `CE` or `PE`, in any case.
    pub option_type: &'a str,
    /// The underlying's LTP when the caller already has it.
    pub known_ltp: Option<f64>,
}

impl<'a> OptionPick<'a> {
    /// The contract a request names (`underlying`, `exchange`,
    /// `expiry_date`, `strike_int`, `offset`, `option_type`).
    pub fn from_request(req: &'a Value) -> Self {
        let s = |k: &str| req.get(k).and_then(Value::as_str).unwrap_or_default();
        Self {
            underlying: s("underlying"),
            exchange: s("exchange"),
            expiry_date: req.get("expiry_date").and_then(Value::as_str),
            strike_int: req.get("strike_int").and_then(Value::as_f64),
            offset: s("offset"),
            option_type: s("option_type"),
            known_ltp: None,
        }
    }

    /// The at-the-money `option_type` of `expiry_date`.
    pub fn atm(
        underlying: &'a str,
        exchange: &'a str,
        expiry_date: Option<&'a str>,
        option_type: &'a str,
    ) -> Self {
        Self {
            underlying,
            exchange,
            expiry_date,
            offset: "ATM",
            option_type,
            ..Self::default()
        }
    }
}

/// The option type in capitals, once it and the strike interval are valid.
fn checked_option_type(p: &OptionPick<'_>) -> Result<String, Reply> {
    let ot = p.option_type.trim().to_ascii_uppercase();
    if ot != "CE" && ot != "PE" {
        return Err(Reply::error(
            400,
            format!(
                "Invalid option_type: '{}'. Supported option types are CE and PE.",
                p.option_type
            ),
        ));
    }
    if let Some(si) = p.strike_int {
        if !si.is_finite() || si <= 0.0 {
            return Err(Reply::error(
                400,
                format!(
                    "Invalid strike_int: {}. Strike interval must be a positive number.",
                    si
                ),
            ));
        }
    }
    Ok(ot)
}

/// The listed strike `offset` steps from the one nearest `ltp`.
fn listed_strike(
    rows: &[SymToken],
    u: &Underlying,
    ot: &str,
    opt_ex: &str,
    offset: &str,
    ltp: f64,
) -> Result<f64, Reply> {
    let strikes = available_strikes(rows, &u.base, &u.expiry, ot, opt_ex);
    if strikes.is_empty() {
        return Err(no_strikes(&u.base, &u.expiry));
    }
    let Some(atm) = atm_index(&strikes, ltp) else {
        return Err(Reply::error(
            500,
            "Failed to determine ATM strike from available strikes.",
        ));
    };
    match offset_index(offset, atm, ot == "CE", strikes.len()) {
        Ok(Some(i)) => Ok(strikes[i]),
        Ok(None) => Err(Reply::error(
            400,
            format!(
                "Offset {} is out of range for available strikes. Please use a smaller offset.",
                offset
            ),
        )),
        Err(m) => Err(Reply::error(400, m)),
    }
}

/// The strike `offset` intervals of `si` from the multiple of `si` nearest
/// `ltp`.
fn interval_strike(si: f64, ltp: f64, offset: &str, ot: &str) -> Result<f64, Reply> {
    let atm = (ltp / si).round_ties_even() * si;
    let up = offset.to_ascii_uppercase();
    if up == "ATM" {
        return Ok(atm);
    }
    let (kind, n) = if let Some(n) = up.strip_prefix("ITM") {
        ("ITM", n)
    } else if let Some(n) = up.strip_prefix("OTM") {
        ("OTM", n)
    } else {
        return Err(Reply::error(400, format!("Invalid offset: {}", up)));
    };
    let n: f64 = n.parse::<i64>().map_err(|_| {
        Reply::error(
            400,
            format!("invalid literal for int() with base 10: '{}'", n),
        )
    })? as f64;
    let up_side = (kind == "ITM") != (ot == "CE");
    Ok(if up_side { atm + n * si } else { atm - n * si })
}

/// Web `get_option_symbol`, reused by optionsorder, optionsmultiorder and
/// syntheticfuture.
pub async fn resolve_option(
    ctx: &AppState,
    h: &BrokerHandle,
    p: &OptionPick<'_>,
) -> Result<ResolvedOption, Reply> {
    let ot = checked_option_type(p)?;
    let u = resolve_underlying(ctx, p.underlying, p.exchange, p.expiry_date, false)?;
    let ltp = match p.known_ltp {
        Some(l) => l,
        None => underlying_ltp(ctx, h, &u, ".").await?.ltp,
    };
    let opt_ex = option_exchange(&u.quote_exchange);
    let snap = ctx.symbols.snapshot();
    let strike = match p.strike_int {
        None => listed_strike(snap.rows(), &u, &ot, &opt_ex, p.offset, ltp)?,
        Some(si) => interval_strike(si, ltp, p.offset, &ot)?,
    };
    let symbol = format!("{}{}{}{}", u.base, u.expiry, format_strike(strike), ot);
    let Some(row) = snap.by_symbol(&opt_ex, &symbol) else {
        return Err(Reply::error(404, format!(
            "Option symbol {} not found in {}. Symbol may not exist or master contract needs update.",
            symbol, opt_ex
        )));
    };
    Ok(ResolvedOption {
        symbol: row.symbol.clone(),
        exchange: row.exchange.clone(),
        lotsize: i64::from(row.lot_size),
        tick_size: row.tick_size,
        freeze_qty: ctx.freeze.for_option(&row.symbol, &row.exchange),
        underlying_ltp: ltp,
        strike,
    })
}

/// `optionsymbol`.
pub async fn option_symbol(ctx: &AppState, req: &Value) -> Reply {
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    match resolve_option(ctx, &h, &OptionPick::from_request(req)).await {
        Ok(r) => r.reply(),
        Err(r) => r,
    }
}

// ------------------------------------------------------------------ synthetic

/// Synthetic future: ATM strike + call - put.
pub async fn synthetic_future_price(
    ctx: &AppState,
    h: &BrokerHandle,
    underlying: &str,
    exchange: &str,
    expiry: &str,
) -> Result<(f64, f64, f64), Reply> {
    let ce = OptionPick::atm(underlying, exchange, Some(expiry), "CE");
    let ce = resolve_option(ctx, h, &ce).await?;
    let pe = OptionPick {
        known_ltp: Some(ce.underlying_ltp),
        ..OptionPick::atm(underlying, exchange, Some(expiry), "PE")
    };
    let pe = resolve_option(ctx, h, &pe).await?;
    let keys = [
        QuoteKey::new(ce.exchange.clone(), ce.symbol.clone()),
        QuoteKey::new(pe.exchange.clone(), pe.symbol.clone()),
    ];
    let rows = h
        .broker
        .get_multiquotes(&h.auth, &keys)
        .await
        .map_err(|e| Reply::broker_failure(&e, 500))?;
    let ltp_of = |sym: &str| {
        rows.iter()
            .find(|r| r.symbol == sym)
            .and_then(|r| r.data.as_ref())
            .map(|q| q.ltp)
            .filter(|l| *l != 0.0)
    };
    let Some(c) = ltp_of(&ce.symbol) else {
        return Err(Reply::error(
            500,
            format!("Could not fetch LTP for Call option: {}", ce.symbol),
        ));
    };
    let Some(p) = ltp_of(&pe.symbol) else {
        return Err(Reply::error(
            500,
            format!("Could not fetch LTP for Put option: {}", pe.symbol),
        ));
    };
    Ok((ce.strike, ce.strike + c - p, ce.underlying_ltp))
}

/// `syntheticfuture`.
pub async fn synthetic_future(
    ctx: &AppState,
    underlying: &str,
    exchange: &str,
    expiry: &str,
) -> Reply {
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    match synthetic_future_price(ctx, &h, underlying, exchange, expiry).await {
        Ok((atm, price, ltp)) => Reply::ok(json!({
            "atm_strike": float(atm),
            "expiry": expiry,
            "status": "success",
            "synthetic_future_price": bs::py_round(price, 2),
            "underlying": underlying,
            "underlying_ltp": num(ltp),
        })),
        Err(r) => r,
    }
}

// ------------------------------------------------------------------ chain

/// One strike of a chain: the strike, the CE leg and the PE leg.
type ChainRow = (f64, Option<Map<String, Value>>, Option<Map<String, Value>>);

fn label(idx: usize, atm: usize, call: bool) -> String {
    if idx == atm {
        return "ATM".into();
    }
    let (itm, n) = if idx < atm {
        (call, atm - idx)
    } else {
        (!call, idx - atm)
    };
    format!("{}{}", if itm { "ITM" } else { "OTM" }, n)
}

fn leg_json(row: &SymToken, label: &str, q: Option<&Quote>) -> Map<String, Value> {
    let z = Quote::default();
    let q = q.unwrap_or(&z);
    let v = json!({
        "symbol": row.symbol, "label": label,
        "ltp": num(q.ltp), "bid": num(q.bid), "ask": num(q.ask),
        "bid_qty": q.bid_qty, "ask_qty": q.ask_qty,
        "open": num(q.open), "high": num(q.high), "low": num(q.low),
        "prev_close": num(q.close), "volume": q.volume, "oi": q.oi,
        "lotsize": row.lot_size, "tick_size": float(row.tick_size),
    });
    v.as_object().cloned().unwrap_or_default()
}

/// Attach IV and Greeks to a chain leg (web `calculate_chain_greeks`).
fn add_leg_greeks(leg: &mut Map<String, Value>, call: bool, f: f64, k: f64, t: f64, r: f64) {
    let price = leg.get("ltp").and_then(Value::as_f64).unwrap_or(0.0);
    if price <= 0.0 || k <= 0.0 {
        return;
    }
    let intrinsic = if call {
        (f - k).max(0.0)
    } else {
        (k - f).max(0.0)
    };
    let tv = price - intrinsic;
    let flag = if call { Flag::Call } else { Flag::Put };
    let no_tv = tv <= 0.0 || (intrinsic > 0.0 && tv < 0.01);
    let iv = if no_tv {
        None
    } else {
        bs::implied_volatility(price, f, k, r, t, flag)
            .ok()
            .filter(|x| x.is_finite() && *x > 0.0)
    };
    match iv {
        Some(iv) => {
            let g = bs::greeks(flag, f, k, t, r, iv);
            leg.insert(
                "implied_volatility".into(),
                json!(bs::py_round(iv * 100.0, 2)),
            );
            leg.insert("delta".into(), json!(bs::py_round(g.delta, 4)));
            leg.insert("gamma".into(), json!(bs::py_round(g.gamma, 6)));
            leg.insert("theta".into(), json!(bs::py_round(g.theta, 4)));
            leg.insert("vega".into(), json!(bs::py_round(g.vega, 4)));
        }
        None => {
            let delta = if intrinsic > 0.0 {
                if call {
                    1.0
                } else {
                    -1.0
                }
            } else {
                0.0
            };
            leg.insert("implied_volatility".into(), json!(0));
            leg.insert("delta".into(), json!(delta));
            leg.insert("gamma".into(), json!(0));
            leg.insert("theta".into(), json!(0));
            leg.insert("vega".into(), json!(0));
        }
    }
}

/// `optionchain`.
pub async fn option_chain(
    ctx: &AppState,
    underlying: &str,
    exchange: &str,
    expiry_date: &str,
    strike_count: Option<i64>,
    with_greeks: bool,
    interest_rate: Option<f64>,
) -> Reply {
    match option_chain_answer(
        ctx,
        underlying,
        exchange,
        expiry_date,
        strike_count,
        with_greeks,
        interest_rate,
    )
    .await
    {
        Ok(a) => Reply::ok(a.body),
        Err(r) => r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn underlying_parsing() {
        assert_eq!(
            parse_underlying("NIFTY27OCT26FUT"),
            ("NIFTY".into(), Some("27OCT26".into()))
        );
        assert_eq!(parse_underlying("nifty"), ("NIFTY".into(), None));
        assert_eq!(
            parse_underlying("NIFTY06OCT2622400CE"),
            ("NIFTY06OCT2622400CE".into(), None)
        );
    }

    #[test]
    fn offsets_move_the_right_way() {
        let strikes: Vec<f64> = (0..11).map(|i| 22150.0 + 50.0 * i as f64).collect();
        let atm = atm_index(&strikes, 22421.95).unwrap();
        assert_eq!(strikes[atm], 22400.0);
        let pick = |o: &str, call: bool| {
            offset_index(o, atm, call, strikes.len())
                .unwrap()
                .map(|i| strikes[i])
        };
        assert_eq!(pick("OTM2", false), Some(22300.0));
        assert_eq!(pick("ITM2", false), Some(22500.0));
        assert_eq!(pick("OTM3", true), Some(22550.0));
        assert_eq!(pick("ITM1", true), Some(22350.0));
        assert_eq!(pick("OTM9", true), None);
        assert_eq!(pick("BAD", true), None);
        assert!(offset_index("ITMX", atm, true, 11).is_err());
        // Ties go to the lower strike.
        assert_eq!(atm_index(&[100.0, 110.0], 105.0), Some(0));
    }

    #[test]
    fn labels() {
        assert_eq!(label(5, 5, true), "ATM");
        assert_eq!(label(3, 5, true), "ITM2");
        assert_eq!(label(3, 5, false), "OTM2");
        assert_eq!(label(7, 5, true), "OTM2");
        assert_eq!(label(7, 5, false), "ITM2");
    }

    #[test]
    fn expiry_and_time() {
        let e = expiry_datetime("06OCT26", "NFO", None).unwrap();
        assert_eq!(e.timestamp(), 1791280800);
        let now = DateTime::<Utc>::from_timestamp(1791000272, 500_000_000).unwrap();
        let (days, _) = time_to_expiry(e, now).unwrap();
        assert_eq!(bs::py_round(days, 4), 3.2468);
        assert!(time_to_expiry(now, e).is_none());
        assert_eq!(
            expiry_datetime("27OCT26", "NFO", None).unwrap().timestamp(),
            1793095200
        );
    }

    #[test]
    fn option_symbol_parsing() {
        let c = parse_option_symbol("NIFTY06OCT2622400CE").unwrap();
        assert_eq!(
            (
                c.base.as_str(),
                c.expiry.as_str(),
                c.strike,
                c.option_type.as_str()
            ),
            ("NIFTY", "06OCT26", 22400.0, "CE")
        );
        assert_eq!(
            parse_option_symbol("VEDL25APR24292.5PE").unwrap().strike,
            292.5
        );
        assert!(parse_option_symbol("NIFTY27OCT26FUT").is_err());
    }

    /// Web test/test_option_expiry_timezone_contract.py::
    /// test_parse_option_symbols_with_punctuation_and_digits_in_underlying
    #[test]
    fn option_symbols_with_punctuation_and_digits_in_underlying() {
        let cases = [
            ("M&M27OCT262800CE", "M&M", (2026, 10, 27), 2800.0, "CE"),
            ("GVT&D27OCT264200PE", "GVT&D", (2026, 10, 27), 4200.0, "PE"),
            (
                "NAM-INDIA27OCT261040CE",
                "NAM-INDIA",
                (2026, 10, 27),
                1040.0,
                "CE",
            ),
            (
                "BAJAJ-AUTO27OCT2610000CE",
                "BAJAJ-AUTO",
                (2026, 10, 27),
                10000.0,
                "CE",
            ),
            (
                "360ONE27OCT261040CE",
                "360ONE",
                (2026, 10, 27),
                1040.0,
                "CE",
            ),
            (
                "NIFTY28NOV2424000CE",
                "NIFTY",
                (2024, 11, 28),
                24000.0,
                "CE",
            ),
            (
                "SENSEX28NOV24100000PE",
                "SENSEX",
                (2024, 11, 28),
                100000.0,
                "PE",
            ),
            ("USDINR28NOV2483.50CE", "USDINR", (2024, 11, 28), 83.5, "CE"),
        ];
        for (symbol, underlying, (y, m, d), strike, option_type) in cases {
            let c = parse_option_symbol(symbol).unwrap_or_else(|e| panic!("{symbol}: {e}"));
            assert_eq!(c.base, underlying, "{symbol}");
            assert_eq!(
                parse_compact_expiry(&c.expiry),
                NaiveDate::from_ymd_opt(y, m, d),
                "{symbol}"
            );
            assert_eq!(c.strike, strike, "{symbol}");
            assert_eq!(c.option_type, option_type, "{symbol}");
        }
    }

    /// Web test/test_option_expiry_timezone_contract.py::
    /// test_parse_option_symbol_rejects_trailing_newline, plus other text
    /// after the type, which the anchored pattern also refuses.
    #[test]
    fn option_symbol_rejects_trailing_text() {
        for symbol in [
            "NIFTY11AUG2624600CE\n",
            "NIFTY11AUG2624600CEX",
            "NIFTY11AUG2624600PE ",
        ] {
            let err = parse_option_symbol(symbol).unwrap_err();
            assert!(
                err.contains("Invalid option symbol format"),
                "{symbol:?}: {err}"
            );
        }
    }

    #[test]
    fn greeks_reply_shape() {
        let c = parse_option_symbol("NIFTY06OCT2622400CE").unwrap();
        let now = DateTime::<Utc>::from_timestamp(1791000272, 500_000_000).unwrap();
        let r = GreeksInput::new("NIFTY06OCT2622400CE", "NFO", &c, 22453.35, 156.95).calculate(now);
        assert!(r.is_success());
        assert_eq!(r.body["implied_volatility"], json!(15.23));
        assert_eq!(r.body["interest_rate"], json!(0));
        assert_eq!(r.body["expiry_date"], json!("06-Oct-2026"));
        assert_eq!(r.body["days_to_expiry"], json!(3.2468));
        assert_eq!(r.body["greeks"]["delta"], json!(0.5686));
        let deep = GreeksInput::new("X", "NFO", &c, 23000.0, 590.0).calculate(now);
        assert_eq!(deep.body["implied_volatility"], json!(0));
        assert_eq!(deep.body["greeks"]["delta"], json!(1.0));
    }

    /// TOOLS-06: an expiry is checked byte by byte before any slice, so
    /// multibyte text is refused instead of splitting a character.
    #[test]
    fn expiry_parsing_refuses_what_is_not_a_real_ddmmmyy_day() {
        for bad in [
            "1\u{e9}CT26",
            "30\u{d4}CT26",
            "\u{ff13}0OCT26",
            "30OCT2\u{e9}",
            "",
            "30OC",
            "30OCT2026",
            "30-OCT-26",
            "31FEB26",
            "29FEB27",
            "00OCT26",
            "32OCT26",
            "30XYZ26",
            "3OOCT26",
        ] {
            assert_eq!(parse_compact_expiry(bad), None, "{:?}", bad);
            assert_eq!(Expiry::parse(bad), None, "{:?}", bad);
            assert!(
                available_strikes(&[], "NIFTY", bad, "CE", "NFO").is_empty(),
                "{:?}",
                bad
            );
            // `listed_live` reads the master's DD-MMM-YY, which "30-OCT-26" is.
            if bad != "30-OCT-26" {
                assert!(!listed_live(bad, "NFO", Utc::now()), "{:?}", bad);
            }
        }
        assert_eq!(master_expiry("1\u{e9}ct26"), "1\u{e9}CT26");
        let e = Expiry::parse(" 29feb28 ").unwrap();
        assert_eq!(e.compact(), "29FEB28");
        assert_eq!(e.master(), "29-FEB-28");
        assert_eq!(e.date(), NaiveDate::from_ymd_opt(2028, 2, 29).unwrap());
        assert_eq!(master_expiry("30oct26"), "30-OCT-26");
        assert_eq!(
            parse_compact_expiry("27Oct26"),
            NaiveDate::from_ymd_opt(2026, 10, 27)
        );
    }

    mod expiry_props {
        use super::super::*;
        use proptest::prelude::*;

        proptest! {
            /// No text, however malformed, panics an expiry helper; what
            /// parses is seven ASCII bytes naming a real day, and it
            /// round-trips through the master's `DD-MMM-YY`.
            #[test]
            fn expiry_helpers_never_panic(
                s in prop_oneof![
                    any::<String>(),
                    "[0-9\u{e9}\u{ff13}]{2}[A-Za-z\u{e9}\u{d4}]{3}[0-9\u{e9}\n]{2}",
                ]
            ) {
                let parsed = parse_compact_expiry(&s);
                let _ = master_expiry(&s);
                let _ = available_strikes(&[], "NIFTY", &s, "CE", "NFO");
                let _ = expiry_datetime(&s, "NFO", None);
                let _ = listed_live(&s, "MCX", Utc::now());
                if let Some(d) = parsed {
                    prop_assert!(s.len() == 7 && s.is_ascii());
                    let e = Expiry::parse(&s).unwrap();
                    prop_assert_eq!(e.date(), d);
                    prop_assert_eq!(parse_oa_expiry(&e.master()), Some(d));
                }
            }

            /// Every real day in DDMMMYY form parses to itself.
            #[test]
            fn every_real_day_parses(days in 0i64..36_500) {
                let d = NaiveDate::from_ymd_opt(2000, 1, 1).unwrap()
                    + chrono::Duration::days(days);
                let compact = crate::brokers::common::master_contract::format_expiry(d)
                    .replace('-', "");
                prop_assert_eq!(parse_compact_expiry(&compact), Some(d));
                prop_assert_eq!(Expiry::parse(&compact).map(|e| e.date()), Some(d));
            }
        }
    }
}
