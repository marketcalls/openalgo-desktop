//! Table-driven fill matrix: every price type x side x path (immediate at
//! placement, tick, polling fallback), with the trigger met or not and the
//! limit met or not. Expected outcomes follow the web's
//! `sandbox/order_manager.py` (placement) and `sandbox/execution_engine.py`
//! (`_process_order`, `_process_trigger_pending_order`).

use crate::sandbox_support;

use openalgo_desktop_lib::sandbox::{Quote, Tick};
use rust_decimal::Decimal;
use sandbox_support::*;

#[derive(Debug, Clone, Copy)]
enum Path {
    Tick,
    Poll,
}

struct Case {
    name: &'static str,
    action: &'static str,
    pt: &'static str,
    price: Option<&'static str>,
    trigger: Option<&'static str>,
    /// LTP when the order is placed.
    placement_ltp: &'static str,
    /// Status right after placement.
    after_place: &'static str,
    /// Average fill price after placement (if it filled).
    placed_fill: Option<&'static str>,
    /// LTPs delivered afterwards, with the status expected after each.
    steps: &'static [(&'static str, &'static str, Option<&'static str>)],
}

const CASES: &[Case] = &[
    Case {
        name: "market buy fills at ltp when the quote has no depth",
        action: "BUY",
        pt: "MARKET",
        price: None,
        trigger: None,
        placement_ltp: "100",
        after_place: "complete",
        placed_fill: Some("100"),
        steps: &[],
    },
    Case {
        name: "market sell fills at ltp when the quote has no depth",
        action: "SELL",
        pt: "MARKET",
        price: None,
        trigger: None,
        placement_ltp: "100",
        after_place: "complete",
        placed_fill: Some("100"),
        steps: &[],
    },
    Case {
        name: "marketable limit buy fills at ltp, not the limit",
        action: "BUY",
        pt: "LIMIT",
        price: Some("105"),
        trigger: None,
        placement_ltp: "100",
        after_place: "complete",
        placed_fill: Some("100"),
        steps: &[],
    },
    Case {
        name: "marketable limit sell fills at ltp, not the limit",
        action: "SELL",
        pt: "LIMIT",
        price: Some("95"),
        trigger: None,
        placement_ltp: "100",
        after_place: "complete",
        placed_fill: Some("100"),
        steps: &[],
    },
    Case {
        name: "resting limit buy fills at its limit when the price falls through",
        action: "BUY",
        pt: "LIMIT",
        price: Some("95"),
        trigger: None,
        placement_ltp: "100",
        after_place: "open",
        placed_fill: None,
        steps: &[("96", "open", None), ("95", "complete", Some("95"))],
    },
    Case {
        name: "resting limit buy fills at its limit even when the tick is lower",
        action: "BUY",
        pt: "LIMIT",
        price: Some("95"),
        trigger: None,
        placement_ltp: "100",
        after_place: "open",
        placed_fill: None,
        steps: &[("90", "complete", Some("95"))],
    },
    Case {
        name: "resting limit sell fills at its limit when the price rises through",
        action: "SELL",
        pt: "LIMIT",
        price: Some("105"),
        trigger: None,
        placement_ltp: "100",
        after_place: "open",
        placed_fill: None,
        steps: &[("104.95", "open", None), ("106", "complete", Some("105"))],
    },
    Case {
        name: "sl buy waits in trigger pending, then fills at ltp inside the limit",
        action: "BUY",
        pt: "SL",
        price: Some("106"),
        trigger: Some("105"),
        placement_ltp: "100",
        after_place: "trigger pending",
        placed_fill: None,
        steps: &[
            ("104", "trigger pending", None),
            ("105.5", "complete", Some("105.5")),
        ],
    },
    Case {
        name: "sl buy triggered beyond its limit rests open, then fills inside it",
        action: "BUY",
        pt: "SL",
        price: Some("106"),
        trigger: Some("105"),
        placement_ltp: "100",
        after_place: "trigger pending",
        placed_fill: None,
        steps: &[
            ("107", "open", None),
            ("108", "open", None),
            ("105.5", "complete", Some("105.5")),
        ],
    },
    Case {
        name: "sl buy with trigger and limit met at placement fills at ltp",
        action: "BUY",
        pt: "SL",
        price: Some("106"),
        trigger: Some("99"),
        placement_ltp: "100",
        after_place: "complete",
        placed_fill: Some("100"),
        steps: &[],
    },
    Case {
        name: "sl buy with trigger met but limit not at placement starts open",
        action: "BUY",
        pt: "SL",
        price: Some("98"),
        trigger: Some("97"),
        placement_ltp: "100",
        after_place: "open",
        placed_fill: None,
        steps: &[("97.5", "complete", Some("97.5"))],
    },
    Case {
        name: "sl sell waits, then fills at ltp inside the limit",
        action: "SELL",
        pt: "SL",
        price: Some("94"),
        trigger: Some("95"),
        placement_ltp: "100",
        after_place: "trigger pending",
        placed_fill: None,
        steps: &[
            ("96", "trigger pending", None),
            ("94.5", "complete", Some("94.5")),
        ],
    },
    Case {
        name: "sl sell triggered below its limit rests open",
        action: "SELL",
        pt: "SL",
        price: Some("94"),
        trigger: Some("95"),
        placement_ltp: "100",
        after_place: "trigger pending",
        placed_fill: None,
        steps: &[("93", "open", None), ("94", "complete", Some("94"))],
    },
    Case {
        name: "sl-m buy fills at ltp once the trigger is touched",
        action: "BUY",
        pt: "SL-M",
        price: None,
        trigger: Some("105"),
        placement_ltp: "100",
        after_place: "trigger pending",
        placed_fill: None,
        steps: &[
            ("104.95", "trigger pending", None),
            ("105", "complete", Some("105")),
        ],
    },
    Case {
        name: "sl-m buy with trigger met at placement fills at ltp",
        action: "BUY",
        pt: "SL-M",
        price: None,
        trigger: Some("99"),
        placement_ltp: "100",
        after_place: "complete",
        placed_fill: Some("100"),
        steps: &[],
    },
    Case {
        name: "sl-m sell fills at ltp once the trigger is touched",
        action: "SELL",
        pt: "SL-M",
        price: None,
        trigger: Some("95"),
        placement_ltp: "100",
        after_place: "trigger pending",
        placed_fill: None,
        steps: &[
            ("95.05", "trigger pending", None),
            ("94", "complete", Some("94")),
        ],
    },
    Case {
        name: "sl-m sell with trigger met at placement fills at ltp",
        action: "SELL",
        pt: "SL-M",
        price: None,
        trigger: Some("101"),
        placement_ltp: "100",
        after_place: "complete",
        placed_fill: Some("100"),
        steps: &[],
    },
];

async fn run_case(c: &Case, path: Path) {
    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("SBIN", "NSE", c.placement_ltp);
    let mut r = req("SBIN", "NSE", c.action, 10, c.pt, "MIS");
    if let Some(p) = c.price {
        r = limit(r, p);
    }
    if let Some(t) = c.trigger {
        r = trigger(r, t);
    }
    let id = place(&env, r).await;
    assert_eq!(
        env.status(&id).await,
        c.after_place,
        "{} ({:?}): after placement",
        c.name,
        path
    );
    if let Some(f) = c.placed_fill {
        let o = env.sb.order_row(&id).await.unwrap().unwrap();
        assert_eq!(
            o.average_price,
            Some(d(f)),
            "{}: placement fill price",
            c.name
        );
    }
    for (ltp, status, fill) in c.steps {
        match path {
            Path::Tick => {
                env.sb
                    .on_tick(Tick::new("SBIN", "NSE", d(ltp)))
                    .await
                    .unwrap();
            }
            Path::Poll => {
                env.ltp("SBIN", "NSE", ltp);
                env.sb.poll_once().await.unwrap();
            }
        }
        assert_eq!(
            &env.status(&id).await,
            status,
            "{} ({:?}) at {}",
            c.name,
            path,
            ltp
        );
        if let Some(f) = fill {
            let o = env.sb.order_row(&id).await.unwrap().unwrap();
            assert_eq!(
                o.average_price,
                Some(d(f)),
                "{} ({:?}): fill price",
                c.name,
                path
            );
            assert_eq!(o.filled_quantity, 10);
            assert_eq!(o.pending_quantity, 0);
        }
    }
    env.assert_margin_consistent().await;
    env.shutdown().await;
}

#[tokio::test]
async fn fill_matrix_tick_path() {
    for c in CASES {
        run_case(c, Path::Tick).await;
    }
}

#[tokio::test]
async fn fill_matrix_polling_path() {
    for c in CASES {
        run_case(c, Path::Poll).await;
    }
}

#[tokio::test]
async fn market_order_fills_at_the_far_side_of_the_quote() {
    let env = Env::at("2026-10-05 10:00:00");
    env.quote(
        "SBIN",
        "NSE",
        Quote {
            ltp: d("100"),
            bid: d("99.95"),
            ask: d("100.05"),
            ..Quote::default()
        },
    );
    let b = place(&env, req("SBIN", "NSE", "BUY", 1, "MARKET", "MIS")).await;
    let s = place(&env, req("SBIN", "NSE", "SELL", 1, "MARKET", "MIS")).await;
    let ob = env.sb.order_row(&b).await.unwrap().unwrap();
    let os = env.sb.order_row(&s).await.unwrap().unwrap();
    assert_eq!(ob.average_price, Some(d("100.05")), "BUY at the ask");
    assert_eq!(os.average_price, Some(d("99.95")), "SELL at the bid");
    assert_eq!(
        ob.price,
        Some(d("100")),
        "the MARKET row stores the LTP used for margin"
    );
    env.shutdown().await;
}

#[tokio::test]
async fn a_market_order_without_a_quote_rests_on_the_last_position_price_and_fills_on_the_next_tick(
) {
    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("SBIN", "NSE", "100");
    place(&env, req("SBIN", "NSE", "BUY", 1, "MARKET", "MIS")).await;
    env.quotes.remove("SBIN", "NSE");
    let id = place(&env, req("SBIN", "NSE", "BUY", 1, "MARKET", "MIS")).await;
    assert_eq!(env.status(&id).await, "open");
    env.sb
        .on_tick(Tick::new("SBIN", "NSE", d("101")))
        .await
        .unwrap();
    assert_eq!(env.status(&id).await, "complete");
    assert_eq!(env.qty("SBIN", "NSE", "MIS").await, 2);
    env.shutdown().await;
}

/// SB-05 (reporting): a close that has not filled is pending, never counted
/// as a closed position; one that filled is.
#[tokio::test]
async fn a_close_without_a_price_is_reported_pending_not_closed() {
    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("SBIN", "NSE", "100");
    place(&env, req("SBIN", "NSE", "BUY", 10, "MARKET", "MIS")).await;
    env.quotes.remove("SBIN", "NSE");
    let v = serde_json::to_value(env.sb.close_all_positions().await.unwrap()).unwrap();
    assert_eq!(v["closed_positions"], 0, "{}", v);
    assert_eq!(v["failed_closures"], 0, "{}", v);
    assert!(
        v["message"].as_str().unwrap().contains("waiting for a price"),
        "{}",
        v
    );
    assert_eq!(env.qty("SBIN", "NSE", "MIS").await, 10, "still held");
    env.shutdown().await;

    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("SBIN", "NSE", "100");
    place(&env, req("SBIN", "NSE", "BUY", 10, "MARKET", "MIS")).await;
    let v = serde_json::to_value(env.sb.close_all_positions().await.unwrap()).unwrap();
    assert_eq!(v["closed_positions"], 1, "{}", v);
    assert_eq!(v["message"], "Closed 1 positions");
    assert_eq!(env.qty("SBIN", "NSE", "MIS").await, 0);
    env.shutdown().await;
}

#[tokio::test]
async fn a_market_order_with_no_price_anywhere_is_refused() {
    let env = Env::at("2026-10-05 10:00:00");
    let e = env
        .sb
        .place_order(req("SBIN", "NSE", "BUY", 1, "MARKET", "MIS"))
        .await
        .unwrap_err();
    assert_eq!(e.http_status, 400);
    assert!(e
        .message
        .starts_with("Cannot place MARKET order for SBIN - unable to fetch current price"));
    env.shutdown().await;
}

// Port of test/sandbox/test_stale_quote_guard.py::test_both_fill_paths_consult_the_guard
#[tokio::test]
async fn test_both_fill_paths_consult_the_guard() {
    let env = Env::at("2026-10-05 10:00:00");
    let stale = Quote {
        ltp: d("1047.60"),
        high: d("1345"),
        low: d("1262"),
        ..Quote::default()
    };
    env.quote("RELIANCE", "NSE", stale);
    // Placement path: a MARKET order is deferred, not filled at the stale LTP.
    let m = place(&env, req("RELIANCE", "NSE", "BUY", 1, "MARKET", "MIS")).await;
    assert_eq!(env.status(&m).await, "open");
    // Placement path for a marketable LIMIT too.
    let l = place(
        &env,
        limit(req("RELIANCE", "NSE", "BUY", 1, "LIMIT", "MIS"), "1100"),
    )
    .await;
    assert_eq!(env.status(&l).await, "open");
    // Polling path with the same stale quote: still nothing.
    env.sb.poll_once().await.unwrap();
    assert_eq!(env.status(&m).await, "open");
    // A coherent quote fills both.
    env.quote(
        "RELIANCE",
        "NSE",
        Quote {
            ltp: d("1296"),
            high: d("1345"),
            low: d("1262"),
            ..Quote::default()
        },
    );
    env.sb.poll_once().await.unwrap();
    assert_eq!(env.status(&m).await, "complete");
    assert_eq!(
        env.sb.order_row(&m).await.unwrap().unwrap().average_price,
        Some(d("1296"))
    );
    assert_eq!(
        env.status(&l).await,
        "open",
        "a 1100 limit does not fill at 1296"
    );
    env.shutdown().await;
}

#[tokio::test]
async fn every_transition_publishes_an_order_update() {
    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("SBIN", "NSE", "100");
    let sl = place(
        &env,
        limit(
            trigger(req("SBIN", "NSE", "BUY", 1, "SL", "MIS"), "105"),
            "106",
        ),
    )
    .await;
    env.sb
        .on_tick(Tick::new("SBIN", "NSE", d("107")))
        .await
        .unwrap();
    env.sb
        .on_tick(Tick::new("SBIN", "NSE", d("105.5")))
        .await
        .unwrap();
    env.rec.wait_for("sandbox.order_filled", 1).await;
    env.rec.wait_for("order.update", 3).await;
    assert_eq!(
        env.rec.statuses(&sl),
        vec!["trigger pending", "open", "complete"]
    );
    let c = place(
        &env,
        limit(req("SBIN", "NSE", "BUY", 1, "LIMIT", "MIS"), "90"),
    )
    .await;
    env.sb.cancel_order(&c).await.unwrap();
    env.rec.wait_for("order.update", 5).await;
    assert_eq!(env.rec.statuses(&c), vec!["open", "cancelled"]);
    env.shutdown().await;
}

#[tokio::test]
async fn a_modify_after_the_decision_is_not_filled_at_the_old_terms() {
    // Port of test_gthread_sandbox_fills::test_a_modify_after_the_fill_was_decided_is_not_filled_at_the_old_terms
    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("SBIN", "NSE", "100");
    let id = place(
        &env,
        limit(req("SBIN", "NSE", "BUY", 1, "LIMIT", "MIS"), "95"),
    )
    .await;
    let decided = env.sb.order_row(&id).await.unwrap().unwrap();
    env.sb
        .modify_order(
            &id,
            openalgo_desktop_lib::sandbox::ModifyRequest {
                price: Some(d("90")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // A fill decided against the old terms (limit 95 at a 94 tick) must not land.
    let filled = openalgo_desktop_lib::sandbox::execution::decide(&decided, &Quote::ltp(d("94")));
    assert_eq!(
        filled,
        openalgo_desktop_lib::sandbox::execution::Decision::Fill(d("95"))
    );
    env.sb
        .on_tick(Tick::new("SBIN", "NSE", d("94")))
        .await
        .unwrap();
    assert_eq!(
        env.status(&id).await,
        "open",
        "the modified 90 limit does not fill at 94"
    );
    assert_eq!(decided.price, Some(Decimal::from(95)));
    env.shutdown().await;
}
