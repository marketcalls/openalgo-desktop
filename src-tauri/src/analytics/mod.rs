//! Pure options and portfolio analytics: no database, broker, clock or
//! logging. Every input is an argument and every result a return value, so
//! each computation is testable in isolation and the services only fetch
//! and shape.
//!
//! | Module | Web source |
//! |---|---|
//! | [`black76`] | `opengreeks.black76` (via `option_greeks_service.py`) |
//! | [`chain`] | `oi_tracker_service.py` (PCR, max pain), `gex_service.py`, `iv_smile_service.py`, `gamma_density_service.py` |
//! | [`series`] | `strategy_chart_service.py` (windows, date cap, combined premium), `iv_chart_service.py` (IV series) |
//! | [`straddle`] | `straddle_chart_service.py`, `custom_straddle_service.py` |
//! | [`surface`] | `vol_surface_service.py` (strike grid) |

pub mod black76;
pub mod chain;
pub mod series;
pub mod straddle;
pub mod surface;

/// Python `round(x, n)`.
pub use black76::py_round;

/// The strike closest to `ltp`; ties go to the earlier (lower) strike, like
/// Python's `min(strikes, key=lambda s: abs(s - ltp))`. `None` when there
/// are no strikes or the price is not a number.
pub fn closest_strike(strikes: &[f64], ltp: f64) -> Option<f64> {
    if !ltp.is_finite() {
        return None;
    }
    let mut best: Option<(f64, f64)> = None;
    for s in strikes {
        let d = (s - ltp).abs();
        if best.map(|(_, bd)| d < bd).unwrap_or(true) {
            best = Some((*s, d));
        }
    }
    best.map(|(s, _)| s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closest_strike_ties_to_the_lower_strike() {
        let k = [100.0, 110.0, 120.0];
        assert_eq!(closest_strike(&k, 105.0), Some(100.0));
        assert_eq!(closest_strike(&k, 106.0), Some(110.0));
        assert_eq!(closest_strike(&k, 1e9), Some(120.0));
        assert_eq!(closest_strike(&k, f64::NAN), None);
        assert_eq!(closest_strike(&[], 1.0), None);
    }

    mod props {
        use crate::analytics::black76::{self as b76, Flag};
        use crate::analytics::chain::{max_pain, StrikeOi};
        use proptest::prelude::*;

        proptest! {
            /// Black-76 put-call parity: C - P = e^(-rT) (F - K).
            #[test]
            fn put_call_parity(
                f in 50.0f64..50_000.0,
                moneyness in 0.7f64..1.3,
                t in 0.002f64..2.0,
                r in 0.0f64..0.1,
                sigma in 0.05f64..1.5,
            ) {
                let k = f * moneyness;
                let c = b76::price(Flag::Call, f, k, t, r, sigma);
                let p = b76::price(Flag::Put, f, k, t, r, sigma);
                let rhs = (-r * t).exp() * (f - k);
                prop_assert!((c - p - rhs).abs() <= 1e-8 * f.max(1.0));
            }

            /// The solver inverts the price it is given, and a dearer option
            /// never has a lower IV.
            #[test]
            fn iv_round_trips_and_is_monotonic_in_price(
                f in 1_000.0f64..50_000.0,
                moneyness in 0.85f64..1.15,
                t in 0.01f64..1.0,
                sigma in 0.05f64..1.2,
                bump in 1.0001f64..1.2,
                call in any::<bool>(),
            ) {
                let flag = if call { Flag::Call } else { Flag::Put };
                let k = f * moneyness;
                let px = b76::price(flag, f, k, t, 0.0, sigma);
                // IV is only identifiable from the time value: a deep in-the-
                // money option near expiry is priced at its intrinsic value
                // for any small sigma, so the solver rightly floors there.
                let intrinsic = if call { (f - k).max(0.0) } else { (k - f).max(0.0) };
                prop_assume!(px - intrinsic > 1e-6 * f);
                let iv = b76::implied_volatility(px, f, k, 0.0, t, flag).unwrap();
                prop_assert!((iv - sigma).abs() < 1e-6, "iv {} sigma {}", iv, sigma);
                let hi = px * bump;
                if let Ok(iv2) = b76::implied_volatility(hi, f, k, 0.0, t, flag) {
                    prop_assert!(iv2 >= iv);
                }
            }

            /// Gamma is positive and the same for both option types.
            #[test]
            fn gamma_positive(
                f in 100.0f64..50_000.0,
                moneyness in 0.5f64..1.5,
                t in 0.001f64..2.0,
                sigma in 0.05f64..1.5,
            ) {
                let k = f * moneyness;
                let g = b76::gamma(f, k, t, 0.0, sigma);
                prop_assert!(g >= 0.0 && g.is_finite());
                let full = b76::greeks(Flag::Put, f, k, t, 0.0, sigma);
                prop_assert!((full.gamma - g).abs() <= 1e-12 * g.max(1e-12));
            }

            /// The max-pain strike's pain is the least of all strikes.
            #[test]
            fn max_pain_is_the_minimum(rows in proptest::collection::vec((1u32..60, 0u32..10_000, 0u32..10_000), 1..30)) {
                let chain: Vec<StrikeOi> = rows
                    .iter()
                    .map(|(k, c, p)| StrikeOi { strike: f64::from(*k) * 50.0, ce_oi: f64::from(*c), pe_oi: f64::from(*p) })
                    .collect();
                let (k, pain) = max_pain(&chain).unwrap();
                let best = pain.iter().find(|r| r.strike == k).unwrap().total_pain;
                prop_assert!(pain.iter().all(|r| r.total_pain >= best));
            }
        }
    }
}
