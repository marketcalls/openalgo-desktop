//! Property tests of netting through the real engine (random sequences of
//! MARKET fills at random prices, adds, reduces and reversals):
//! 1. quantity conservation: position quantity is the sum of signed fills;
//! 2. margin conservation: `used_margin` equals the margin the books hold,
//!    and a flat position holds none;
//! 3. P&L conservation: realized + unrealized at any mark equals
//!    `sum((mark - fill) * signed_qty)` (to the 8-place average rounding);
//! 4. average price: the quantity-weighted mean after a same-direction add,
//!    the fill price after a reversal;
//! 5. the web's fund identity `available + used - realized == capital`;
//! 6. two adds commute (same average and margin in either order).

use crate::sandbox_support;

use proptest::prelude::*;
use rust_decimal::Decimal;
use sandbox_support::*;

#[derive(Debug, Clone)]
struct Fill {
    buy: bool,
    qty: i64,
    /// Price in paise.
    paise: i64,
}

fn fill() -> impl Strategy<Value = Fill> {
    (any::<bool>(), 1i64..50, 1_000i64..20_000).prop_map(|(buy, qty, paise)| Fill {
        buy,
        qty,
        paise,
    })
}

fn price(p: i64) -> String {
    Decimal::new(p, 2).to_string()
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, .. ProptestConfig::default() })]

    #[test]
    fn netting_invariants_hold_through_any_fill_sequence(fills in prop::collection::vec(fill(), 1..12), mark in 1_000i64..20_000) {
        rt().block_on(async {
            let env = Env::at("2026-10-05 10:00:00");
            let capital = d("10000000");
            let mut net: i64 = 0;
            let mut cost_flow = Decimal::ZERO; // sum(fill * signed)
            for f in &fills {
                let px = price(f.paise);
                env.ltp("ZEEL", "NSE", &px);
                let before = env.sb.position_row("ZEEL", "NSE", "MIS").await.unwrap();
                let action = if f.buy { "BUY" } else { "SELL" };
                let id = place(&env, req("ZEEL", "NSE", action, f.qty, "MARKET", "MIS")).await;
                assert_eq!(env.status(&id).await, "complete");
                let signed = if f.buy { f.qty } else { -f.qty };
                let old = net;
                net += signed;
                cost_flow += d(&px) * Decimal::from(signed);

                let p = env.sb.position_row("ZEEL", "NSE", "MIS").await.unwrap().unwrap();
                // (1)
                assert_eq!(p.quantity, net);
                // (2)
                env.assert_margin_consistent().await;
                let used = env.used().await;
                if net == 0 {
                    assert_eq!(used, Decimal::ZERO);
                    assert_eq!(p.margin_blocked, Decimal::ZERO);
                } else {
                    assert_eq!(used, p.margin_blocked);
                }
                // (4)
                if old != 0 && net != 0 && old.signum() == net.signum() && net.abs() > old.abs() {
                    let prev = before.unwrap();
                    let expected = ((Decimal::from(old.abs()) * prev.average_price
                        + Decimal::from(f.qty) * d(&px))
                        / Decimal::from(net.abs()))
                        .round_dp(8);
                    assert_eq!(p.average_price, expected);
                }
                if old != 0 && net != 0 && old.signum() != net.signum() {
                    assert_eq!(p.average_price, d(&px), "a reversal restarts at the fill price");
                }
                if old == 0 {
                    assert_eq!(p.average_price, d(&px));
                }
                // (5)
                let fr = env.sb.funds_row().await.unwrap().unwrap();
                assert_eq!(fr.available_balance + fr.used_margin - fr.realized_pnl, capital);
            }
            // (3)
            let m = d(&price(mark));
            let fr = env.sb.funds_row().await.unwrap().unwrap();
            let p = env.sb.position_row("ZEEL", "NSE", "MIS").await.unwrap().unwrap();
            let unrealized = (m - p.average_price) * Decimal::from(p.quantity);
            let total = fr.realized_pnl + unrealized;
            let expected = m * Decimal::from(net) - cost_flow;
            let tol = Decimal::new(1, 4) * Decimal::from(fills.len() as i64 * 50);
            assert!((total - expected).abs() <= tol, "P&L {total} vs flow {expected}");
            assert_eq!(fr.realized_pnl, p.accumulated_realized_pnl);
            env.shutdown().await;
        });
    }

    #[test]
    fn two_adds_commute(a in 1i64..50, pa in 1_000i64..20_000, b in 1i64..50, pb in 1_000i64..20_000) {
        rt().block_on(async {
            let mut results = Vec::new();
            for order in [[(a, pa), (b, pb)], [(b, pb), (a, pa)]] {
                let env = Env::at("2026-10-05 10:00:00");
                for (q, p) in order {
                    env.ltp("ZEEL", "NSE", &price(p));
                    place(&env, req("ZEEL", "NSE", "BUY", q, "MARKET", "MIS")).await;
                }
                let p = env.sb.position_row("ZEEL", "NSE", "MIS").await.unwrap().unwrap();
                results.push((p.quantity, p.average_price, p.margin_blocked));
                env.shutdown().await;
            }
            assert_eq!(results[0], results[1]);
        });
    }
}
