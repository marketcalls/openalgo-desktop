//! Option-chain tools (web `oi_tracker_service.py`, `gex_service.py`,
//! `iv_smile_service.py`, `gamma_density_service.py`,
//! `oi_profile_service.py`). Each reads one option chain (a single batched
//! quote call) and hands the numbers to [`crate::analytics::chain`].

use super::core::{broker_handle, float, num, BrokerHandle, Reply};
use super::options_service::{
    expiry_datetime, greeks_underlying_exchange, option_chain_answer, parse_option_symbol,
    synthetic_future_price, time_to_expiry, today_ist, ChainAnswer, GreeksInput,
};
use super::tools_service::{
    chain_options_exchange, fan_out, history_gate, history_rows, nearest_future, quote,
};
use crate::analytics::chain::{self, DensityInput, StrikeOi};
use crate::analytics::py_round;
use crate::analytics::series::{cap_last_n_dates, trading_window};
use crate::state::AppState;
use chrono::Duration;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::time::Duration as StdDuration;

/// How long one OI Profile request may spend on previous-day OI (web
/// `GTHREAD_OI_CHANGE_BUDGET_SECONDS`).
pub const OI_CHANGE_BUDGET: StdDuration = StdDuration::from_secs(60);

fn leg<'a>(row: &'a Value, side: &str) -> Option<&'a Map<String, Value>> {
    row.get(side).and_then(Value::as_object)
}

fn lf(leg: Option<&Map<String, Value>>, k: &str) -> f64 {
    leg.and_then(|m| m.get(k))
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
}

fn ls(leg: Option<&Map<String, Value>>, k: &str) -> Option<String> {
    leg.and_then(|m| m.get(k))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn opt_float(v: Option<f64>) -> Value {
    v.map(float).unwrap_or(Value::Null)
}

/// Python's `round(x, 2) if computed else 0`: a computed value prints as a
/// float, an untouched one as the integer 0.
fn computed(x: f64, is_float: bool) -> Value {
    if is_float {
        float(x)
    } else {
        json!(0)
    }
}

/// The chain the tool reads: the `/api/v1/optionchain` body, with the legs
/// the broker could not supply.
async fn fetch_chain(
    ctx: &AppState,
    underlying: &str,
    exchange: &str,
    expiry: &str,
    strike_count: i64,
) -> Result<ChainAnswer, Reply> {
    option_chain_answer(
        ctx,
        underlying,
        exchange,
        expiry,
        Some(strike_count),
        false,
        None,
    )
    .await
}

/// An open-interest figure is worked out only when every leg's open
/// interest is a reading. A leg the broker answered without one (no
/// quote, or OI not supplied) would otherwise read as zero and pass for a
/// quiet strike (TOOLS-04, TOOLS-13). The web totals such legs as zero.
fn require_oi(a: &ChainAnswer, underlying: &str, expiry: &str) -> Result<(), Reply> {
    let unknown = a.oi_unknown();
    if unknown.is_empty() {
        return Ok(());
    }
    let total = rows(&a.body)
        .iter()
        .map(|r| usize::from(leg(r, "ce").is_some()) + usize::from(leg(r, "pe").is_some()))
        .sum::<usize>();
    tracing::warn!(
        "Open interest missing for {} of {} options of {} {}; OI figures refused",
        unknown.len(),
        total,
        underlying,
        expiry
    );
    Err(Reply::error(
        503,
        format!(
            "Open interest did not come back from your broker for {} of {} options of {} {}, so the OI figures are not shown: a missing value would read as zero. Try again shortly.",
            unknown.len(),
            total,
            underlying.to_ascii_uppercase(),
            expiry.to_ascii_uppercase()
        ),
    ))
}

fn rows(body: &Value) -> Vec<Value> {
    body.get("chain")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn field(body: &Value, k: &str) -> Value {
    body.get(k).cloned().unwrap_or(Value::Null)
}

/// Web `_get_nearest_futures_price`: the LTP of the matching (else nearest)
/// future, `null` when there is none or the quote fails.
async fn futures_price(
    ctx: &AppState,
    h: &BrokerHandle,
    underlying: &str,
    exchange: &str,
    expiry: &str,
) -> Value {
    let Some((sym, ex)) = nearest_future(ctx, underlying, exchange, expiry) else {
        tracing::warn!(
            "No futures contracts found for {} on {}",
            underlying,
            exchange
        );
        return Value::Null;
    };
    match quote(ctx, h, &sym, &ex).await {
        Ok(q) => num(q.ltp),
        Err(r) => {
            tracing::warn!("Futures quote for {}:{} failed: {}", ex, sym, r.message());
            Value::Null
        }
    }
}

struct OiSnapshot {
    body: Value,
    chain: Vec<StrikeOi>,
    strikes: Vec<Value>,
    lot_size: Option<i64>,
    total_ce_oi: f64,
    total_pe_oi: f64,
    pcr_oi: Option<f64>,
    pcr_volume: Option<f64>,
    futures_price: Value,
}

async fn oi_snapshot(
    ctx: &AppState,
    underlying: &str,
    exchange: &str,
    expiry: &str,
) -> Result<OiSnapshot, Reply> {
    let h = broker_handle(ctx)?;
    // 23 strikes each side: 94 symbols, inside the smallest broker
    // multiquote OI bucket (web comment, oi_tracker_service.py).
    let answer = fetch_chain(ctx, underlying, exchange, expiry, 23).await?;
    require_oi(&answer, underlying, expiry)?;
    let body = answer.body;
    let (mut ce_oi, mut pe_oi, mut ce_vol, mut pe_vol) = (0.0, 0.0, 0.0, 0.0);
    let mut lot_size = None;
    let mut chain = Vec::new();
    let mut strikes = Vec::new();
    for item in rows(&body) {
        let (ce, pe) = (leg(&item, "ce"), leg(&item, "pe"));
        for l in [ce, pe].into_iter().flatten() {
            let lot = l.get("lotsize").and_then(Value::as_i64).unwrap_or(0);
            if lot_size.is_none() && lot != 0 {
                lot_size = Some(lot);
            }
        }
        ce_oi += lf(ce, "oi");
        pe_oi += lf(pe, "oi");
        ce_vol += lf(ce, "volume");
        pe_vol += lf(pe, "volume");
        chain.push(StrikeOi {
            strike: item.get("strike").and_then(Value::as_f64).unwrap_or(0.0),
            ce_oi: lf(ce, "oi"),
            pe_oi: lf(pe, "oi"),
        });
        strikes.push(field(&item, "strike"));
    }
    let futures_price = futures_price(ctx, &h, underlying, exchange, expiry).await;
    Ok(OiSnapshot {
        body,
        chain,
        strikes,
        lot_size,
        total_ce_oi: ce_oi,
        total_pe_oi: pe_oi,
        pcr_oi: chain::pcr(pe_oi, ce_oi),
        pcr_volume: chain::pcr(pe_vol, ce_vol),
        futures_price,
    })
}

fn pcr_json(p: Option<f64>) -> Value {
    p.map(float).unwrap_or(json!(0))
}

/// `POST /oitracker/api/oi-data`.
pub async fn oi_data(ctx: &AppState, underlying: &str, exchange: &str, expiry: &str) -> Reply {
    let s = match oi_snapshot(ctx, underlying, exchange, expiry).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let oi_chain: Vec<Value> = s
        .chain
        .iter()
        .zip(&s.strikes)
        .map(|(c, k)| json!({"strike": k, "ce_oi": num(c.ce_oi), "pe_oi": num(c.pe_oi)}))
        .collect();
    Reply::ok(json!({
        "status": "success",
        "underlying": s.body.get("underlying").cloned().unwrap_or_else(|| json!(underlying)),
        "spot_price": field(&s.body, "underlying_ltp"),
        "futures_price": s.futures_price,
        "lot_size": s.lot_size.unwrap_or(1),
        "pcr_oi": pcr_json(s.pcr_oi),
        "pcr_volume": pcr_json(s.pcr_volume),
        "total_ce_oi": num(s.total_ce_oi),
        "total_pe_oi": num(s.total_pe_oi),
        "atm_strike": field(&s.body, "atm_strike"),
        "expiry_date": expiry,
        "chain": oi_chain,
    }))
}

/// `POST /oitracker/api/maxpain`.
pub async fn max_pain(ctx: &AppState, underlying: &str, exchange: &str, expiry: &str) -> Reply {
    let s = match oi_snapshot(ctx, underlying, exchange, expiry).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    if s.chain.is_empty() {
        return Reply::error(404, "No OI data available");
    }
    // Every strike at zero open interest has no max pain: the lowest
    // strike would win the tie and read as a finding (TOOLS-13).
    if s.chain.iter().all(|c| c.ce_oi <= 0.0 && c.pe_oi <= 0.0) {
        return Reply::error(
            404,
            "Open interest is zero at every strike of this expiry, so there is no max pain to show yet.",
        );
    }
    let Some((strike, pain)) = chain::max_pain(&s.chain) else {
        return Reply::error(404, "No valid strike data available");
    };
    Reply::ok(json!({
        "status": "success",
        "underlying": s.body.get("underlying").cloned().unwrap_or_else(|| json!(underlying)),
        "spot_price": field(&s.body, "underlying_ltp"),
        "futures_price": s.futures_price,
        "atm_strike": field(&s.body, "atm_strike"),
        "max_pain_strike": float(strike),
        "lot_size": s.lot_size.unwrap_or(1),
        "pcr_oi": pcr_json(s.pcr_oi),
        "pcr_volume": pcr_json(s.pcr_volume),
        "expiry_date": expiry,
        "pain_data": pain,
    }))
}

/// Greeks of one chain leg against `spot` (web `calculate_greeks` with the
/// chain LTP), `None` when the service does not answer with success.
fn leg_greeks(
    ctx: &AppState,
    symbol: &str,
    options_exchange: &str,
    spot: f64,
    ltp: f64,
) -> Option<Value> {
    let c = parse_option_symbol(symbol).ok()?;
    let r = GreeksInput::new(symbol, options_exchange, &c, spot, ltp).calculate(ctx.now());
    (r.is_success() && r.body.get("status").and_then(Value::as_str) == Some("success"))
        .then_some(r.body)
}

fn spot_of(body: &Value) -> Option<f64> {
    body.get("underlying_ltp")
        .and_then(Value::as_f64)
        .filter(|s| *s > 0.0)
}

/// `POST /gex/api/gex-data`.
pub async fn gex(ctx: &AppState, underlying: &str, exchange: &str, expiry: &str) -> Reply {
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    let body = match fetch_chain(ctx, underlying, exchange, expiry, 45).await {
        Ok(a) => match require_oi(&a, underlying, expiry) {
            Ok(()) => a.body,
            Err(r) => return r,
        },
        Err(r) => return r,
    };
    let Some(spot) = spot_of(&body) else {
        return Reply::error(500, "Could not determine spot price");
    };
    let opt_ex = chain_options_exchange(exchange);
    let mut lot_size = None;
    let mut out = Vec::new();
    let mut gex_rows = Vec::new();
    let mut any_float = (false, false, false);
    for item in rows(&body) {
        let k = item.get("strike").and_then(Value::as_f64).unwrap_or(0.0);
        let mut side = |l: Option<&Map<String, Value>>| -> (f64, f64, f64, bool) {
            let Some(sym) = ls(l, "symbol") else {
                return (0.0, 0.0, 1.0, false);
            };
            let oi = lf(l, "oi");
            let ltp = lf(l, "ltp");
            let lot = l
                .and_then(|m| m.get("lotsize"))
                .and_then(Value::as_i64)
                .filter(|x| *x != 0)
                .unwrap_or(1);
            if lot_size.is_none() {
                lot_size = Some(lot);
            }
            if ltp > 0.0 && oi > 0.0 {
                if let Some(g) = leg_greeks(ctx, &sym, &opt_ex, spot, ltp) {
                    let gamma = g
                        .get("greeks")
                        .and_then(|x| x.get("gamma"))
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0);
                    // The theoretical (deep ITM) answer carries an integer 0.
                    let is_float = g["greeks"]["gamma"].is_f64();
                    return (oi, gamma, lot as f64, is_float);
                }
            }
            (oi, 0.0, lot as f64, false)
        };
        let (ce_oi, ce_g, ce_lot, ce_f) = side(leg(&item, "ce"));
        let (pe_oi, pe_g, pe_lot, pe_f) = side(leg(&item, "pe"));
        let r = chain::gex_row(k, (ce_oi, ce_g, ce_lot), (pe_oi, pe_g, pe_lot));
        any_float = (
            any_float.0 || ce_f,
            any_float.1 || pe_f,
            any_float.2 || ce_f || pe_f,
        );
        out.push(json!({
            "strike": field(&item, "strike"),
            "ce_oi": num(ce_oi),
            "pe_oi": num(pe_oi),
            "ce_gamma": computed(r.ce_gamma, ce_f),
            "pe_gamma": computed(r.pe_gamma, pe_f),
            "ce_gex": computed(r.ce_gex, ce_f),
            "pe_gex": computed(r.pe_gex, pe_f),
            "net_gex": computed(r.net_gex, ce_f || pe_f),
        }));
        gex_rows.push(r);
    }
    let futures_price = futures_price(ctx, &h, underlying, exchange, expiry).await;
    let (t_ce_oi, t_pe_oi, t_ce, t_pe, t_net) = chain::gex_totals(&gex_rows);
    Reply::ok(json!({
        "status": "success",
        "underlying": body.get("underlying").cloned().unwrap_or_else(|| json!(underlying)),
        "spot_price": field(&body, "underlying_ltp"),
        "futures_price": futures_price,
        "lot_size": lot_size.unwrap_or(1),
        "atm_strike": field(&body, "atm_strike"),
        "expiry_date": expiry,
        "pcr_oi": pcr_json(chain::pcr(t_pe_oi, t_ce_oi)),
        "total_ce_oi": num(t_ce_oi),
        "total_pe_oi": num(t_pe_oi),
        "total_ce_gex": computed(t_ce, any_float.0),
        "total_pe_gex": computed(t_pe, any_float.1),
        "total_net_gex": computed(t_net, any_float.2),
        "chain": out,
    }))
}

/// `POST /ivsmile/api/iv-smile-data`.
pub async fn iv_smile(ctx: &AppState, underlying: &str, exchange: &str, expiry: &str) -> Reply {
    let body = match fetch_chain(ctx, underlying, exchange, expiry, 25).await {
        Ok(a) => a.body,
        Err(r) => return r,
    };
    let Some(spot) = spot_of(&body) else {
        return Reply::error(500, "Could not determine spot price");
    };
    let opt_ex = chain_options_exchange(exchange);
    let atm = body.get("atm_strike").and_then(Value::as_f64);
    let iv_of = |l: Option<&Map<String, Value>>| -> Option<f64> {
        let sym = ls(l, "symbol")?;
        let ltp = lf(l, "ltp");
        if ltp <= 0.0 {
            return None;
        }
        let g = leg_greeks(ctx, &sym, &opt_ex, spot, ltp)?;
        g.get("implied_volatility")
            .and_then(Value::as_f64)
            .filter(|v| *v > 0.0)
            .map(|v| py_round(v, 2))
    };
    let mut smile = Vec::new();
    let mut out = Vec::new();
    let (mut atm_ce, mut atm_pe) = (None, None);
    for item in rows(&body) {
        let k = item.get("strike").and_then(Value::as_f64).unwrap_or(0.0);
        let ce = iv_of(leg(&item, "ce"));
        let pe = iv_of(leg(&item, "pe"));
        if Some(k) == atm {
            atm_ce = ce;
            atm_pe = pe;
        }
        smile.push((k, ce, pe));
        out.push(json!({"strike": field(&item, "strike"), "ce_iv": opt_float(ce), "pe_iv": opt_float(pe)}));
    }
    let skew = atm.and_then(|a| chain::skew(&smile, a));
    Reply::ok(json!({
        "status": "success",
        "underlying": body.get("underlying").cloned().unwrap_or_else(|| json!(underlying)),
        "spot_price": field(&body, "underlying_ltp"),
        "atm_strike": field(&body, "atm_strike"),
        "atm_iv": opt_float(chain::atm_iv(atm_ce, atm_pe)),
        "skew": opt_float(skew),
        "expiry_date": expiry,
        "chain": out,
    }))
}

/// `POST /gammadensity/api/gamma-data`.
pub async fn gamma_density(
    ctx: &AppState,
    underlying: &str,
    exchange: &str,
    expiry: &str,
) -> Reply {
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    let body = match fetch_chain(ctx, underlying, exchange, expiry, 23).await {
        Ok(a) => match require_oi(&a, underlying, expiry) {
            Ok(()) => a.body,
            Err(r) => return r,
        },
        Err(r) => return r,
    };
    let items = rows(&body);
    let Some(spot) = spot_of(&body).filter(|_| !items.is_empty()) else {
        return Reply::error(404, "Spot price or option chain unavailable");
    };
    let (t_years, dte_days) = expiry_datetime(expiry, exchange, None)
        .and_then(|e| time_to_expiry(e, ctx.now()))
        .map(|(d, y)| (y, d))
        .unwrap_or((0.0, 0.0));
    let rate_pct = 0.0;
    let forward = chain_forward(ctx, &h, &body, underlying, exchange, expiry).await;
    let f = forward.unwrap_or(spot);
    let atm = body.get("atm_strike").and_then(Value::as_f64);
    let g = chain::gamma_density(
        &density_inputs(&items),
        spot,
        f,
        atm,
        t_years,
        rate_pct / 100.0,
    );
    if g.atm_iv_source == chain::AtmIvSource::Assumed {
        tracing::warn!(
            "No invertible IV for {} {}; using fallback IV {}",
            underlying,
            expiry,
            chain::FALLBACK_IV
        );
    }
    let priced = DensityPricing {
        spot,
        forward,
        dte_days,
    };
    Reply::ok(gamma_density_body(
        &body, underlying, exchange, expiry, &priced, &g,
    ))
}

/// The synthetic future of the chain's underlying and expiry, which
/// prices the options; `None` when it cannot be read (spot is used).
async fn chain_forward(
    ctx: &AppState,
    h: &BrokerHandle,
    body: &Value,
    underlying: &str,
    exchange: &str,
    expiry: &str,
) -> Option<f64> {
    let base = body
        .get("underlying")
        .and_then(Value::as_str)
        .unwrap_or(underlying)
        .to_string();
    let ux = greeks_underlying_exchange(&base, &exchange.to_ascii_uppercase());
    synthetic_future_price(ctx, h, &base, &ux, &expiry.to_ascii_uppercase())
        .await
        .ok()
        .map(|(_, f, _)| py_round(f, 2))
}

/// Strike, OI and LTP of both legs of each chain row.
fn density_inputs(items: &[Value]) -> Vec<DensityInput> {
    items
        .iter()
        .map(|item| {
            let (ce, pe) = (leg(item, "ce"), leg(item, "pe"));
            DensityInput {
                strike: item.get("strike").and_then(Value::as_f64).unwrap_or(0.0),
                ce_oi: lf(ce, "oi"),
                pe_oi: lf(pe, "oi"),
                ce_ltp: lf(ce, "ltp"),
                pe_ltp: lf(pe, "ltp"),
            }
        })
        .collect()
}

/// What the gamma density was priced against.
struct DensityPricing {
    spot: f64,
    /// The synthetic future, when it could be read.
    forward: Option<f64>,
    dte_days: f64,
}

/// The web's gamma density answer, with the provenance fields (TOOLS-15).
fn gamma_density_body(
    body: &Value,
    underlying: &str,
    exchange: &str,
    expiry: &str,
    p: &DensityPricing,
    g: &chain::GammaDensity,
) -> Value {
    let f = p.forward.unwrap_or(p.spot);
    let rows_json: Vec<Value> = g
        .rows
        .iter()
        .map(|r| {
            json!({
                "strike": float(r.strike),
                "ce_oi": num(r.ce_oi),
                "pe_oi": num(r.pe_oi),
                "iv": opt_float(r.iv),
                "density_intraday": float(r.density_intraday),
                "density_expiry": float(r.density_expiry),
            })
        })
        .collect();
    let b = &g.intraday_band;
    json!({
        "status": "success",
        "underlying": body.get("underlying").cloned().unwrap_or_else(|| json!(underlying)),
        "exchange": exchange,
        "expiry_date": expiry,
        "spot_price": num(py_round(p.spot, 2)),
        "forward_price": if p.forward.is_some() { float(f) } else { num(py_round(f, 2)) },
        "atm_strike": field(body, "atm_strike"),
        "atm_iv": float(py_round(g.atm_iv * 100.0, 2)),
        // Provenance the web does not report (TOOLS-15): where the ATM IV
        // came from, how many legs priced with it instead of their own IV,
        // and what the options were priced against.
        "atm_iv_source": g.atm_iv_source,
        "estimated_iv_legs": g.estimated_iv_legs,
        "pricing_reference": if p.forward.is_some() { "forward" } else { "spot" },
        "dte_days": float(py_round(p.dte_days, 2)),
        "interest_rate": 0,
        "peak_intraday_strike": opt_float(g.peak_intraday_strike),
        "peak_expiry_strike": opt_float(g.peak_expiry_strike),
        "sigma_move": b.sigma_move,
        "one_sigma_low": b.one_sigma_low,
        "one_sigma_high": b.one_sigma_high,
        "two_sigma_low": b.two_sigma_low,
        "two_sigma_high": b.two_sigma_high,
        "intraday_band": b,
        "expiry_band": g.expiry_band,
        "chain": rows_json,
    })
}

/// Previous-day OI per option, and what could not be had.
#[derive(Debug, Default)]
pub struct PreviousOi {
    /// Symbol -> the previous trading day's OI, a reading (zero included).
    pub values: HashMap<String, f64>,
    /// History did not answer, or had no previous trading day in it.
    pub failed: usize,
    /// Not reached within the time budget.
    pub omitted: usize,
}

/// Previous-day OI per option (web `_fetch_daily_oi_changes`): the
/// second-last daily candle's OI over the last 14 days, fetched through the
/// shared gate. A failed or too-short history, or one with no open
/// interest in it ([`carries_oi`]), is counted as failed, never read as a
/// previous OI of zero (TOOLS-14: the web does, and the whole current OI
/// then shows as one day's build-up); symbols not reached within `budget`
/// are counted as omitted.
pub async fn previous_day_oi(
    ctx: &AppState,
    symbols: Vec<String>,
    options_exchange: &str,
    budget: StdDuration,
) -> PreviousOi {
    let today = today_ist(ctx);
    let start = today - Duration::days(14);
    let deadline = tokio::time::Instant::now() + budget;
    let got = fan_out(history_gate(), symbols, |sym| async move {
        if tokio::time::Instant::now() >= deadline {
            return (sym, None);
        }
        let prev = match history_rows(ctx, &sym, options_exchange, "D", start, today).await {
            Ok(rows) if rows.len() >= 2 && carries_oi(&rows) => {
                rows[rows.len() - 2].get("oi").and_then(Value::as_f64)
            }
            Ok(_) => None,
            Err(r) => {
                tracing::debug!("Previous-day OI for {} not read: {}", sym, r.message());
                None
            }
        };
        (sym, Some(prev))
    })
    .await;
    let mut out = PreviousOi::default();
    for (sym, p) in got {
        match p {
            Some(Some(v)) => {
                out.values.insert(sym, v);
            }
            Some(None) => out.failed += 1,
            None => out.omitted += 1,
        }
    }
    out
}

/// Whether a contract's daily history has open interest in it. A candle
/// has no way to leave OI out, so a broker whose history carries none
/// (Pocketful's chart candles, for BFO and MCX options and whenever its
/// NFO endpoint falls back) sends 0 on every bar. Only contracts with open
/// interest now are asked about, so a history that does carry OI shows
/// some on a bar: one that is zero on every bar is no reading, and a
/// zero beside other readings is one (a contract that had none yesterday).
fn carries_oi(rows: &[Value]) -> bool {
    rows.iter().any(|r| {
        r.get("oi")
            .and_then(Value::as_f64)
            .is_some_and(|oi| oi != 0.0)
    })
}

/// `POST /oiprofile/api/profile-data`.
pub async fn oi_profile(
    ctx: &AppState,
    underlying: &str,
    exchange: &str,
    expiry: &str,
    interval: &str,
    days: i64,
) -> Reply {
    let opt_ex = chain_options_exchange(exchange);
    let body = match fetch_chain(ctx, underlying, exchange, expiry, 20).await {
        Ok(a) => match require_oi(&a, underlying, expiry) {
            Ok(()) => a.body,
            Err(r) => return r,
        },
        Err(r) => return r,
    };
    let mut lot_size = None;
    let mut chain_rows = Vec::new();
    let mut wanted: Vec<String> = Vec::new();
    for item in rows(&body) {
        let mut side = |l: Option<&Map<String, Value>>| -> (f64, Option<String>) {
            let oi = lf(l, "oi");
            let sym = ls(l, "symbol");
            if let Some(lot) = l.and_then(|m| m.get("lotsize")).and_then(Value::as_i64) {
                if lot_size.is_none() && lot != 0 {
                    lot_size = Some(lot);
                }
            }
            if let Some(s) = sym.as_ref().filter(|_| oi > 0.0) {
                wanted.push(s.clone());
            }
            (oi, sym)
        };
        let (ce_oi, ce_sym) = side(leg(&item, "ce"));
        let (pe_oi, pe_sym) = side(leg(&item, "pe"));
        chain_rows.push((field(&item, "strike"), ce_oi, pe_oi, ce_sym, pe_sym));
    }

    let mut candles: Vec<Value> = Vec::new();
    let fut = nearest_future(ctx, underlying, &opt_ex, expiry);
    if let Some((sym, ex)) = &fut {
        let (start, end) = trading_window(today_ist(ctx), days);
        if let Ok(rows) = history_rows(ctx, sym, ex, interval, start, end).await {
            candles = rows
                .into_iter()
                .map(|mut c| {
                    if let Some(m) = c.as_object_mut() {
                        if !m.contains_key("time") {
                            if let Some(t) = m.get("timestamp").and_then(Value::as_f64) {
                                let t = if t > 1e12 {
                                    (t / 1000.0) as i64
                                } else {
                                    t as i64
                                };
                                m.insert("time".into(), json!(t));
                            }
                        }
                    }
                    c
                })
                .collect();
            candles = cap_last_n_dates(candles, days, |c| {
                c.get("time").and_then(Value::as_i64).unwrap_or(0)
            });
        }
    }

    wanted.sort();
    wanted.dedup();
    let wanted_set: std::collections::HashSet<String> = wanted.iter().cloned().collect();
    let prev = previous_day_oi(ctx, wanted, &opt_ex, OI_CHANGE_BUDGET).await;

    let oi_chain: Vec<Value> = chain_rows
        .into_iter()
        .map(|(k, ce_oi, pe_oi, ce_sym, pe_sym)| {
            let change = |oi: f64, s: &Option<String>| oi_change(oi, s, &wanted_set, &prev);
            json!({
                "strike": k,
                "ce_oi": num(ce_oi),
                "pe_oi": num(pe_oi),
                "ce_oi_change": change(ce_oi, &ce_sym),
                "pe_oi_change": change(pe_oi, &pe_sym),
            })
        })
        .collect();
    let mut out = json!({
        "status": "success",
        "underlying": body.get("underlying").cloned().unwrap_or_else(|| json!(underlying)),
        "spot_price": field(&body, "underlying_ltp"),
        "atm_strike": field(&body, "atm_strike"),
        "lot_size": lot_size.unwrap_or(1),
        "expiry_date": expiry,
        "futures_symbol": fut.map(|(s, _)| json!(s)).unwrap_or(Value::Null),
        "interval": interval,
        "candles": candles,
        "oi_chain": oi_chain,
    });
    note_partial_oi_change(&mut out, &prev, wanted_set.len());
    Reply::ok(out)
}

/// A contract's OI change since the previous day. One whose previous OI
/// was asked for and not had has no change (null), not a change of its
/// whole OI (TOOLS-14); one with no OI now is not looked up and keeps the
/// web's 0.
fn oi_change(
    oi: f64,
    symbol: &Option<String>,
    wanted: &std::collections::HashSet<String>,
    prev: &PreviousOi,
) -> Value {
    match symbol.as_ref() {
        Some(s) if wanted.contains(s) => match prev.values.get(s) {
            Some(p) => float(oi - p),
            None => Value::Null,
        },
        _ => json!(0),
    }
}

/// When some previous-day OI is missing, say for how many contracts and
/// why: failed histories apart from those past the time budget.
fn note_partial_oi_change(out: &mut Value, prev: &PreviousOi, requested: usize) {
    let loaded = prev.values.len();
    if loaded >= requested {
        return;
    }
    tracing::warn!(
        "OI Profile previous-day OI: {} of {} contracts ({} failed, {} past the time budget)",
        loaded,
        requested,
        prev.failed,
        prev.omitted
    );
    let mut why = Vec::new();
    if prev.failed > 0 {
        why.push(format!(
            "the broker's history did not give the previous day for {}",
            prev.failed
        ));
    }
    if prev.omitted > 0 {
        why.push(format!("loading took too long for {}", prev.omitted));
    }
    if let Some(m) = out.as_object_mut() {
        m.insert("message".into(), json!(format!(
            "The daily OI change is shown for {} of {} option contracts: {}. The others show no change rather than a guess. Refresh in a minute to try again.",
            loaded,
            requested,
            why.join(", and ")
        )));
        m.insert("oi_change_loaded".into(), json!(loaded));
        m.insert("oi_change_requested".into(), json!(requested));
        m.insert("oi_change_failed".into(), json!(prev.failed));
        m.insert("oi_change_omitted".into(), json!(prev.omitted));
    }
}
